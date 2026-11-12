//! Small filesystem helpers: atomic writes, private permissions, safe names.

use anyhow::{Context, Result};
use std::path::{Component, Path, PathBuf};

/// Write `data` to `path` atomically (temp file + rename) so readers never see partial files.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut tmp, data)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Write text only if it changed; returns true when written.
pub fn write_if_changed(path: &Path, data: &str) -> Result<bool> {
    if let Ok(existing) = std::fs::read_to_string(path)
        && existing == data
    {
        return Ok(false);
    }
    write_atomic(path, data.as_bytes())?;
    Ok(true)
}

/// Write a secret file readable only by the current user.
pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    write_atomic(path, data)?;
    make_private_file(path)
}

#[cfg(unix)]
pub fn make_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(unix)]
pub fn make_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(unix)]
pub fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path)?.permissions();
    perm.set_mode(perm.mode() | 0o755);
    std::fs::set_permissions(path, perm)?;
    Ok(())
}

#[cfg(windows)]
pub fn make_private_file(path: &Path) -> Result<()> {
    // Restrict the ACL to the current user via icacls (built into Windows).
    let out = crate::process::new_cmd("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{}:(R,W)", whoami_user()))
        .output();
    if let Ok(o) = out
        && !o.status.success()
    {
        tracing::warn!(
            "icacls failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&o.stderr)
        );
    }
    Ok(())
}

#[cfg(windows)]
pub fn make_private_dir(path: &Path) -> Result<()> {
    let out = crate::process::new_cmd("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{}:(OI)(CI)F", whoami_user()))
        .output();
    if let Ok(o) = out
        && !o.status.success()
    {
        tracing::warn!(
            "icacls failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&o.stderr)
        );
    }
    Ok(())
}

#[cfg(windows)]
pub fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn whoami_user() -> String {
    std::env::var("USERNAME").unwrap_or_else(|_| "%USERNAME%".into())
}

/// A folder / project / database name that is safe to use in paths, hostnames and SQL identifiers.
pub fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Lower-case and replace anything unsafe with `-`; used for auto host names.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for c in name.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// True if `path` is lexically inside `base` (no `..` escapes). Does not touch the filesystem.
pub fn is_within(base: &Path, path: &Path) -> bool {
    let mut depth: i64 = 0;
    for c in path.strip_prefix(base).unwrap_or(path).components() {
        match c {
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// Remove a directory tree, tolerating a missing directory.
pub fn remove_dir_all_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// Find the single directory inside `dir` (archives usually unpack into one top folder).
pub fn single_subdir(dir: &Path) -> Option<PathBuf> {
    let mut entries = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .collect::<Vec<_>>();
    if entries.len() == 1 && entries[0].path().is_dir() {
        Some(entries.remove(0).path())
    } else {
        None
    }
}

/// Timestamp usable in file names: `20261031-141500`.
pub fn timestamp() -> String {
    let now = time::OffsetDateTime::now_utc();
    let fmt = time::macros::format_description!("[year][month][day]-[hour][minute][second]");
    now.format(&fmt).unwrap_or_else(|_| "now".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_names() {
        assert!(is_safe_name("my-app_1"));
        assert!(!is_safe_name("My App"));
        assert!(!is_safe_name("-x"));
        assert!(!is_safe_name("../x"));
        assert_eq!(slugify("My Cool App!"), "my-cool-app");
        assert_eq!(slugify("Laravel_12"), "laravel-12");
    }

    #[test]
    fn within() {
        let base = Path::new("/a/b");
        assert!(is_within(base, Path::new("/a/b/c/d")));
        assert!(!is_within(base, Path::new("/a/b/../../etc")));
        assert!(is_within(base, Path::new("/a/b/c/../d")));
    }
}
