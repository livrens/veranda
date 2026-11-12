//! Keep the installation small without taking anything away that the stack uses.
//!
//! * [`prune_package`]: after a package is extracted, drop the parts a local dev server never
//!   runs: debug builds of the server, debug plugin copies, static link libraries, test suites,
//!   benchmarks, man pages and C headers. Storage engines, plugins and client tools all stay.
//! * [`free_space`]: the one-click "Free space" action: prune every installed package the same
//!   way, gzip old plain-text SQL backups, delete leftover downloads and trim oversized logs.
//! * [`usage`]: how much each part of the root takes, for the Settings page.

use crate::Veranda;
use crate::manifest::Kind;
use crate::packages::Registry;
use crate::paths::Layout;
use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Service logs longer than this are cut back to their last part when a service starts.
pub const LOG_MAX_BYTES: u64 = 20 * 1024 * 1024;
const LOG_KEEP_BYTES: u64 = 2 * 1024 * 1024;

/// Relative paths (directories or files) that are safe to remove from a freshly extracted
/// package. Globs are not supported; `lib/*.a` is handled separately.
fn prunable(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::MySql => &[
            "bin/mysqld-debug",
            "bin/mysqld-debug.exe",
            "lib/plugin/debug",
            "lib/debug",
            "man",
            "include",
            "docs",
        ],
        Kind::MariaDb => &[
            "mariadb-test",
            "mysql-test",
            "sql-bench",
            "bin/mariadb-test",
            "bin/mariadb-client-test",
            "bin/mysqltest",
            "bin/mysql_client_test",
            "bin/mariadb-test-embedded",
            "man",
            "include",
        ],
        Kind::Postgres => &["include", "doc", "share/man"],
        _ => &[],
    }
}

fn dir_size(p: &Path) -> u64 {
    if let Ok(meta) = std::fs::symlink_metadata(p) {
        if meta.is_file() {
            return meta.len();
        }
        if meta.is_dir() {
            return std::fs::read_dir(p)
                .map(|rd| rd.filter_map(|e| e.ok()).map(|e| dir_size(&e.path())).sum())
                .unwrap_or(0);
        }
    }
    0
}

fn remove_path(p: &Path) -> u64 {
    let size = dir_size(p);
    let ok = match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p).is_ok(),
        Ok(_) => std::fs::remove_file(p).is_ok(),
        Err(_) => false,
    };
    if ok { size } else { 0 }
}

/// Remove the prunable parts of one extracted package. Returns bytes freed.
pub fn prune_package(kind: Kind, dir: &Path) -> u64 {
    let mut freed = 0;
    for rel in prunable(kind) {
        let p = dir.join(rel);
        if p.exists() {
            freed += remove_path(&p);
        }
    }
    // static link libraries are only for compiling programs against the server
    if matches!(kind, Kind::MySql | Kind::MariaDb | Kind::Postgres)
        && let Ok(rd) = std::fs::read_dir(dir.join("lib"))
    {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "a") && p.is_file() {
                freed += remove_path(&p);
            }
        }
    }
    if freed > 0 {
        tracing::info!(
            "{kind}: pruned {} MB of debug/test/dev files",
            freed / 1_048_576
        );
    }
    freed
}

/// Cut a log back to its last [`LOG_KEEP_BYTES`] when it is larger than [`LOG_MAX_BYTES`].
pub fn trim_log(path: &Path) -> Result<u64> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(0);
    };
    if meta.len() <= LOG_MAX_BYTES {
        return Ok(0);
    }
    use std::io::{Read, Seek, SeekFrom};
    let mut tail = Vec::with_capacity(LOG_KEEP_BYTES as usize);
    {
        // closed again before the file is replaced: Windows refuses to rename over an open file
        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::End(-(LOG_KEEP_BYTES as i64)))?;
        f.read_to_end(&mut tail)?;
    }
    // start at a line boundary
    let start = tail
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let mut out = format!(
        "[trimmed by Veranda: {} MB before this line]\n",
        meta.len() / 1_048_576
    )
    .into_bytes();
    out.extend_from_slice(&tail[start..]);
    crate::fsutil::write_atomic(path, &out)?;
    Ok(meta.len() - out.len() as u64)
}

/// gzip every plain `.sql` backup in `backups/` (keeping the modification time), so old
/// dumps take a fraction of the space. Returns bytes freed.
pub fn compress_legacy_backups(layout: &Layout) -> Result<u64> {
    let mut freed = 0;
    let Ok(rd) = std::fs::read_dir(layout.backups()) else {
        return Ok(0);
    };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        if !p.is_file() || p.extension().is_none_or(|x| x != "sql") {
            continue;
        }
        let gz = PathBuf::from(format!("{}.gz", p.display()));
        if gz.exists() {
            continue;
        }
        let meta = std::fs::metadata(&p)?;
        let res: Result<()> = (|| {
            let mut input = std::io::BufReader::new(std::fs::File::open(&p)?);
            let out = std::fs::File::create(&gz)?;
            let mut enc = flate2::write::GzEncoder::new(
                std::io::BufWriter::new(out),
                flate2::Compression::default(),
            );
            std::io::copy(&mut input, &mut enc)?;
            enc.finish()?
                .into_inner()
                .map_err(|e| e.into_error())?
                .sync_all()?;
            if let Ok(m) = meta.modified() {
                let _ = std::fs::File::options()
                    .write(true)
                    .open(&gz)
                    .and_then(|f| f.set_modified(m));
            }
            crate::fsutil::make_private_file(&gz)?;
            Ok(())
        })();
        match res {
            Ok(()) => {
                let after = std::fs::metadata(&gz).map(|m| m.len()).unwrap_or(0);
                std::fs::remove_file(&p)?;
                freed += meta.len().saturating_sub(after);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&gz);
                tracing::warn!("compressing {}: {e}", p.display());
            }
        }
    }
    Ok(freed)
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct CleanReport {
    pub freed: u64,
    pub actions: Vec<String>,
}

fn mb(n: u64) -> String {
    if n >= 1 << 30 {
        format!("{:.2} GB", n as f64 / (1u64 << 30) as f64)
    } else if n >= 1 << 20 {
        format!("{} MB", n / 1_048_576)
    } else {
        format!("{} KB", n.div_ceil(1024))
    }
}

/// Everything that can be freed without losing data or features.
pub fn free_space(app: &Veranda, progress: &dyn Fn(String)) -> Result<CleanReport> {
    let mut rep = CleanReport::default();
    let layout = &app.layout;

    progress("trimming installed packages…".into());
    if let Ok(reg) = Registry::load(layout) {
        for item in &reg.items {
            if !item.path.starts_with(layout.bin()) {
                continue;
            }
            let n = prune_package(item.kind, &item.path);
            if n > 0 {
                rep.freed += n;
                rep.actions.push(format!(
                    "{} {}: {} of debug/test files",
                    item.kind,
                    item.version,
                    mb(n)
                ));
            }
        }
    }

    progress("compressing old backups…".into());
    let n = compress_legacy_backups(layout)?;
    if n > 0 {
        rep.freed += n;
        rep.actions.push(format!("backups compressed: {}", mb(n)));
    }

    progress("removing leftovers…".into());
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(layout.downloads()) {
        for e in rd.filter_map(|e| e.ok()) {
            n += remove_path(&e.path());
        }
    }
    let leftover = layout.tmp().join("move-databases.sql");
    if leftover.exists() {
        n += remove_path(&leftover);
    }
    // abandoned install stagings
    if let Ok(rd) = std::fs::read_dir(layout.bin()) {
        for kind_dir in rd.filter_map(|e| e.ok()) {
            if let Ok(inner) = std::fs::read_dir(kind_dir.path()) {
                for e in inner.filter_map(|e| e.ok()) {
                    if e.file_name().to_string_lossy().starts_with(".staging-") {
                        n += remove_path(&e.path());
                    }
                }
            }
        }
    }
    if n > 0 {
        rep.freed += n;
        rep.actions
            .push(format!("downloads and leftovers: {}", mb(n)));
    }

    progress("trimming logs…".into());
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(layout.logs()) {
        for e in rd.filter_map(|e| e.ok()) {
            if e.path().extension().is_some_and(|x| x == "log") {
                n += trim_log(&e.path()).unwrap_or(0);
            }
        }
    }
    if n > 0 {
        rep.freed += n;
        rep.actions.push(format!("logs trimmed: {}", mb(n)));
    }
    if rep.actions.is_empty() {
        rep.actions.push("nothing to free".into());
    }
    Ok(rep)
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageEntry {
    pub id: String,
    pub bytes: u64,
}

/// Size of each part of the root: packages, data (databases), backups, logs, www, other.
pub fn usage(layout: &Layout) -> Vec<UsageEntry> {
    let parts: [(&str, PathBuf); 6] = [
        ("packages", layout.bin()),
        ("data", layout.data()),
        ("backups", layout.backups()),
        ("logs", layout.logs()),
        ("www", layout.www()),
        ("tmp", layout.tmp()),
    ];
    parts
        .into_iter()
        .map(|(id, p)| UsageEntry {
            id: id.into(),
            bytes: dir_size(&p),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_and_compress_and_trim() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("mysql");
        for rel in [
            "bin/mysqld",
            "bin/mysqld-debug",
            "lib/plugin/debug/x.so",
            "lib/libmysqlclient.a",
            "lib/libmysqlclient.so",
            "lib/plugin/ha_rocksdb.so",
            "man/m1",
        ] {
            let p = pkg.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, vec![1u8; 1000]).unwrap();
        }
        let freed = prune_package(Kind::MySql, &pkg);
        assert_eq!(freed, 4000);
        assert!(pkg.join("bin/mysqld").is_file());
        assert!(pkg.join("lib/libmysqlclient.so").is_file());
        assert!(pkg.join("lib/plugin/ha_rocksdb.so").is_file());
        assert!(!pkg.join("bin/mysqld-debug").exists());
        assert!(!pkg.join("lib/plugin/debug").exists());
        assert!(!pkg.join("lib/libmysqlclient.a").exists());
        assert_eq!(prune_package(Kind::Php, &pkg), 0);

        let layout = Layout::new(dir.path().join("root"));
        layout.ensure().unwrap();
        let sql = layout.backups().join("mariadb-1.sql");
        std::fs::write(&sql, "CREATE DATABASE a;\n".repeat(2000)).unwrap();
        let freed = compress_legacy_backups(&layout).unwrap();
        assert!(freed > 30_000, "{freed}");
        assert!(!sql.exists());
        let gz = layout.backups().join("mariadb-1.sql.gz");
        assert!(gz.is_file());
        let mut dec = flate2::read::GzDecoder::new(std::fs::File::open(&gz).unwrap());
        let mut s = String::new();
        std::io::Read::read_to_string(&mut dec, &mut s).unwrap();
        assert!(s.starts_with("CREATE DATABASE a;"));

        let log = layout.logs().join("php.log");
        std::fs::write(&log, "x\n".repeat((LOG_MAX_BYTES as usize) / 2 + 10)).unwrap();
        let freed = trim_log(&log).unwrap();
        assert!(freed > 0);
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.starts_with("[trimmed by Veranda"));
        assert!(text.len() as u64 <= LOG_KEEP_BYTES + 100);
        assert_eq!(trim_log(&log).unwrap(), 0);
        let u = usage(&layout);
        assert_eq!(u.len(), 6);
    }
}
