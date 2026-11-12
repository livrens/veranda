//! Safe extraction of zip / tar.gz / tar.xz archives.
//!
//! Every entry path is validated to stay inside the destination (zip-slip),
//! absolute paths and `..` are rejected, symlinks pointing outside are skipped,
//! and an optional number of leading components is stripped.

use crate::fsutil;
use crate::manifest::ArchiveType;
use anyhow::{Context, Result, bail};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub fn extract(archive: &Path, archive_type: ArchiveType, dest: &Path, strip: u32) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    match archive_type {
        ArchiveType::Zip => extract_zip(archive, dest, strip),
        ArchiveType::TarGz => {
            let f = std::fs::File::open(archive)?;
            let dec = flate2::read::GzDecoder::new(std::io::BufReader::new(f));
            extract_tar(dec, dest, strip)
        }
        ArchiveType::TarXz => {
            let f = std::fs::File::open(archive)?;
            let dec = liblzma::read::XzDecoder::new_multi_decoder(std::io::BufReader::new(f));
            extract_tar(dec, dest, strip)
        }
        ArchiveType::File => bail!("not an archive"),
    }
}

/// Strip `n` leading components; returns None when nothing remains or the path is unsafe.
fn sanitize(rel: &Path, strip: u32) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut skipped = 0;
    for c in rel.components() {
        match c {
            Component::Normal(p) => {
                if skipped < strip {
                    skipped += 1;
                    continue;
                }
                out.push(p);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn extract_zip(archive: &Path, dest: &Path, strip: u32) -> Result<()> {
    let f = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(f))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let Some(enclosed) = entry.enclosed_name() else {
            tracing::warn!("skipping unsafe zip entry {:?}", entry.name().ok());
            continue;
        };
        let Some(rel) = sanitize(&enclosed, strip) else {
            continue;
        };
        let target = dest.join(&rel);
        if !fsutil::is_within(dest, &target) {
            bail!("zip entry escapes destination: {}", rel.display());
        }
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&target)
            .with_context(|| format!("creating {}", target.display()))?;
        std::io::copy(&mut entry, &mut out)?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode & 0o777));
        }
    }
    Ok(())
}

fn extract_tar<R: Read>(reader: R, dest: &Path, strip: u32) -> Result<()> {
    let mut ar = tar::Archive::new(reader);
    ar.set_preserve_permissions(true);
    ar.set_overwrite(true);
    for entry in ar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let Some(rel) = sanitize(&path, strip) else {
            continue;
        };
        let target = dest.join(&rel);
        if !fsutil::is_within(dest, &target) {
            bail!("tar entry escapes destination: {}", rel.display());
        }
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            // Only allow links that stay inside the destination.
            let Some(link) = entry.link_name()?.map(|l| l.into_owned()) else {
                continue;
            };
            let resolved = target.parent().unwrap_or(dest).join(&link);
            if link.is_absolute() || !fsutil::is_within(dest, &resolved) {
                tracing::warn!(
                    "skipping link escaping destination: {} -> {}",
                    rel.display(),
                    link.display()
                );
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let _ = std::fs::remove_file(&target);
            #[cfg(unix)]
            {
                if kind.is_symlink() {
                    std::os::unix::fs::symlink(&link, &target)?;
                } else {
                    let _ = std::fs::hard_link(&resolved, &target);
                }
            }
            #[cfg(windows)]
            {
                // Links are rare in the archives we use; copy the target if it exists.
                if resolved.is_file() {
                    let _ = std::fs::copy(&resolved, &target);
                }
            }
            continue;
        }
        if kind.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if !(kind.is_file() || kind.is_gnu_sparse() || kind.is_contiguous()) {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry
            .unpack(&target)
            .with_context(|| format!("unpacking {}", rel.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_paths() {
        assert_eq!(
            sanitize(Path::new("a/b/c"), 1).unwrap(),
            PathBuf::from("b/c")
        );
        assert!(sanitize(Path::new("a"), 1).is_none());
        assert!(sanitize(Path::new("../x"), 0).is_none());
        assert!(sanitize(Path::new("/etc/passwd"), 0).is_none());
        assert_eq!(
            sanitize(Path::new("./a/./b"), 0).unwrap(),
            PathBuf::from("a/b")
        );
    }

    #[test]
    fn zip_roundtrip_with_strip() {
        let dir = tempfile::tempdir().unwrap();
        let zpath = dir.path().join("t.zip");
        {
            let f = std::fs::File::create(&zpath).unwrap();
            let mut w = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            w.add_directory("top/", opts).unwrap();
            w.start_file("top/hello.txt", opts).unwrap();
            std::io::Write::write_all(&mut w, b"hi").unwrap();
            w.start_file("top/sub/x.txt", opts).unwrap();
            std::io::Write::write_all(&mut w, b"x").unwrap();
            w.finish().unwrap();
        }
        let out = dir.path().join("out");
        extract(&zpath, ArchiveType::Zip, &out, 1).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("hello.txt")).unwrap(),
            "hi"
        );
        assert_eq!(std::fs::read_to_string(out.join("sub/x.txt")).unwrap(), "x");
    }

    #[test]
    fn tar_gz_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let tpath = dir.path().join("t.tar.gz");
        {
            let f = std::fs::File::create(&tpath).unwrap();
            let enc = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
            let mut b = tar::Builder::new(enc);
            let mut h = tar::Header::new_gnu();
            h.set_size(3);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, "pkg/bin/php", &b"abc"[..]).unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        let out = dir.path().join("out");
        extract(&tpath, ArchiveType::TarGz, &out, 1).unwrap();
        assert_eq!(std::fs::read_to_string(out.join("bin/php")).unwrap(), "abc");
    }
}
