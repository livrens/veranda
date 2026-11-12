//! SFTP over an SSH connection, plus the local-side listing used by the dual-pane browser.

use crate::client::Connection;
use anyhow::{Context, Result, bail};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType, OpenFlags};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: String,
    /// `dir`, `file`, `link`, `other`.
    pub kind: String,
    pub size: u64,
    /// Unix time (seconds), 0 when unknown.
    pub mtime: u64,
    /// Permission bits (`0o644`), 0 when unknown.
    pub mode: u32,
    pub owner: String,
    pub hidden: bool,
}

pub struct Sftp {
    session: SftpSession,
    _conn: Arc<Connection>,
}

const CHUNK: usize = 128 * 1024;

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Normalise `..` and `.` in a remote (POSIX) path.
pub fn normalize(path: &str) -> String {
    let abs = path.starts_with('/');
    let mut parts: Vec<&str> = vec![];
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            x => parts.push(x),
        }
    }
    let body = parts.join("/");
    if abs { format!("/{body}") } else { body }
}

impl Sftp {
    pub async fn open(conn: Arc<Connection>) -> Result<Self> {
        let ch = conn
            .open_session()
            .await
            .context("opening a session channel")?;
        ch.request_subsystem(true, "sftp")
            .await
            .context("the server has no SFTP subsystem")?;
        let session = SftpSession::new(ch.into_stream())
            .await
            .context("starting SFTP")?;
        session.set_timeout(60);
        Ok(Self {
            session,
            _conn: conn,
        })
    }

    pub async fn home(&self) -> Result<String> {
        Ok(self.session.canonicalize(".").await?)
    }

    pub async fn canonicalize(&self, path: &str) -> Result<String> {
        Ok(self.session.canonicalize(path).await?)
    }

    pub async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let dir = if path.is_empty() {
            self.home().await?
        } else {
            path.to_string()
        };
        let rd = self.session.read_dir(&dir).await?;
        let mut out = vec![];
        for e in rd {
            let name = e.file_name();
            // a hostile server must not be able to smuggle path separators into local paths
            if name == "."
                || name == ".."
                || name.contains('/')
                || name.contains('\\')
                || name.contains('\0')
            {
                continue;
            }
            let m = e.metadata();
            let mut kind = match e.file_type() {
                FileType::Dir => "dir",
                FileType::File => "file",
                FileType::Symlink => "link",
                FileType::Other => "other",
            };
            let full = join(&dir, &name);
            if kind == "link"
                && let Ok(target) = self.session.metadata(&full).await
                && target.is_dir()
            {
                kind = "dir";
            }
            out.push(Entry {
                hidden: name.starts_with('.'),
                name,
                path: full,
                kind: kind.into(),
                size: m.size.unwrap_or(0),
                mtime: m.mtime.unwrap_or(0) as u64,
                mode: m.permissions.unwrap_or(0) & 0o7777,
                owner: m.user.clone().unwrap_or_default(),
            });
        }
        out.sort_by(|a, b| {
            (a.kind != "dir")
                .cmp(&(b.kind != "dir"))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }

    pub async fn mkdir(&self, path: &str) -> Result<()> {
        Ok(self.session.create_dir(path).await?)
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<()> {
        Ok(self.session.rename(from, to).await?)
    }

    pub async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        let mut m = self.session.metadata(path).await?;
        let kind_bits = m.permissions.unwrap_or(0) & !0o7777;
        m.permissions = Some(kind_bits | (mode & 0o7777));
        self.session.set_metadata(path, m).await?;
        Ok(())
    }

    /// Delete a file, or a directory with everything in it.
    pub fn remove<'a>(&'a self, path: &'a str) -> crate::client::BoxFut<'a, Result<()>> {
        Box::pin(async move {
            let m = self.session.symlink_metadata(path).await?;
            if m.is_dir() {
                for e in self.session.read_dir(path).await? {
                    let name = e.file_name();
                    if name == "." || name == ".." {
                        continue;
                    }
                    self.remove(&join(path, &name)).await?;
                }
                self.session.remove_dir(path).await?;
            } else {
                self.session.remove_file(path).await?;
            }
            Ok(())
        })
    }

    pub async fn read_text(&self, path: &str, max: u64) -> Result<String> {
        let m = self.session.metadata(path).await?;
        if m.len() > max {
            bail!("file is larger than {} KB", max / 1024);
        }
        let data = self.session.read(path).await?;
        Ok(String::from_utf8_lossy(&data).into_owned())
    }

    pub async fn write_text(&self, path: &str, text: &str) -> Result<()> {
        Ok(self.session.write(path, text.as_bytes()).await?)
    }

    pub async fn size(&self, path: &str) -> Result<u64> {
        Ok(self.session.metadata(path).await?.len())
    }

    /// Download one file; `progress(done, total)` is called per chunk.
    pub async fn download(
        &self,
        remote: &str,
        local: &Path,
        cancel: &AtomicBool,
        progress: &(dyn Fn(u64, u64) + Sync),
    ) -> Result<()> {
        let total = self.size(remote).await?;
        let mut src = self.session.open(remote).await?;
        if let Some(p) = local.parent() {
            tokio::fs::create_dir_all(p).await?;
        }
        let mut dst = tokio::fs::File::create(local)
            .await
            .with_context(|| format!("creating {}", local.display()))?;
        let mut buf = vec![0u8; CHUNK];
        let mut done = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                drop(dst);
                let _ = tokio::fs::remove_file(local).await;
                bail!("transfer cancelled");
            }
            let n = src.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).await?;
            done += n as u64;
            progress(done, total);
        }
        dst.flush().await?;
        Ok(())
    }

    pub async fn upload(
        &self,
        local: &Path,
        remote: &str,
        cancel: &AtomicBool,
        progress: &(dyn Fn(u64, u64) + Sync),
    ) -> Result<()> {
        let mut src = tokio::fs::File::open(local)
            .await
            .with_context(|| format!("opening {}", local.display()))?;
        let total = src.metadata().await.map(|m| m.len()).unwrap_or(0);
        let mut dst = self
            .session
            .open_with_flags(
                remote,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
            )
            .await?;
        let mut buf = vec![0u8; CHUNK];
        let mut done = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = dst.shutdown().await;
                let _ = self.session.remove_file(remote).await;
                bail!("transfer cancelled");
            }
            let n = src.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).await?;
            done += n as u64;
            progress(done, total);
        }
        dst.shutdown().await?;
        Ok(())
    }

    pub async fn close(&self) {
        let _ = self.session.close().await;
    }
}

/* ---------------- local side ---------------- */

pub fn local_home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Drives on Windows, `/` elsewhere: the roots the "up" button cannot leave.
pub fn local_roots() -> Vec<String> {
    #[cfg(windows)]
    {
        let mut v = vec![];
        for letter in b'A'..=b'Z' {
            let p = format!("{}:\\", letter as char);
            if Path::new(&p).exists() {
                v.push(p);
            }
        }
        v
    }
    #[cfg(not(windows))]
    {
        vec!["/".to_string()]
    }
}

pub fn local_list(path: &Path) -> Result<Vec<Entry>> {
    let rd = std::fs::read_dir(path).with_context(|| format!("reading {}", path.display()))?;
    let mut out = vec![];
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let full = e.path();
        let meta = match std::fs::metadata(&full).or_else(|_| e.metadata()) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_link = e.file_type().map(|t| t.is_symlink()).unwrap_or(false);
        let kind = if meta.is_dir() {
            "dir"
        } else if is_link {
            "link"
        } else if meta.is_file() {
            "file"
        } else {
            "other"
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o7777
        };
        #[cfg(not(unix))]
        let mode = 0;
        #[cfg(windows)]
        let hidden = {
            use std::os::windows::fs::MetadataExt;
            meta.file_attributes() & 0x2 != 0 || name.starts_with('.')
        };
        #[cfg(not(windows))]
        let hidden = name.starts_with('.');
        out.push(Entry {
            name,
            path: full.to_string_lossy().into_owned(),
            kind: kind.into(),
            size: meta.len(),
            mtime,
            mode,
            owner: String::new(),
            hidden,
        });
    }
    out.sort_by(|a, b| {
        (a.kind != "dir")
            .cmp(&(b.kind != "dir"))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_paths() {
        assert_eq!(normalize("/home/a/../b/./c"), "/home/b/c");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize("/a/.."), "/");
        assert_eq!(normalize("a/b/../c"), "a/c");
    }

    #[test]
    fn local_listing_sorts_dirs_first() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "x").unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join(".hidden"), "").unwrap();
        let l = local_list(dir.path()).unwrap();
        assert_eq!(l[0].name, "a");
        assert_eq!(l[0].kind, "dir");
        assert!(l.iter().any(|e| e.name == ".hidden" && e.hidden));
        assert_eq!(l.iter().find(|e| e.name == "b.txt").unwrap().size, 1);
    }
}
