//! Periodic database dumps into `backups/`, keeping the newest N.

use crate::Veranda;
use crate::db::DbCtx;
use crate::packages::Registry;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

pub fn run(app: &Veranda) -> Result<PathBuf> {
    let reg = Registry::load(&app.layout)?;
    let db = DbCtx::from_app(app, &reg).context("database is not installed")?;
    if !crate::process::port_accepting(db.port) {
        anyhow::bail!("database is not running");
    }
    let dest = app.layout.backups().join(format!(
        "{}-{}.sql.gz",
        db.engine.id(),
        crate::fsutil::timestamp()
    ));
    crate::db::dump_all(&db, &dest)?;
    // dumps made by older versions were plain text; shrink them once
    let _ = crate::slim::compress_legacy_backups(&app.layout);
    rotate(app)?;
    Ok(dest)
}

/// Delete older dumps beyond `backup.keep`.
/// `name.sql` or `name.sql.gz`.
pub fn is_dump(p: &std::path::Path) -> bool {
    let n = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    p.is_file() && (n.ends_with(".sql") || n.ends_with(".sql.gz"))
}

pub fn rotate(app: &Veranda) -> Result<()> {
    let mut files: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(app.layout.backups())?
        .filter_map(|e| e.ok())
        .filter(|e| is_dump(&e.path()))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, p) in files.into_iter().skip(app.config.backup.keep as usize) {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

pub fn last_backup(app: &Veranda) -> Option<SystemTime> {
    std::fs::read_dir(app.layout.backups())
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| is_dump(&e.path()))
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
}

/// True when the configured interval has elapsed since the last dump.
pub fn is_due(app: &Veranda) -> bool {
    if !app.config.backup.enabled {
        return false;
    }
    let interval = Duration::from_secs(u64::from(app.config.backup.interval_hours) * 3600);
    match last_backup(app) {
        Some(t) => t.elapsed().map(|e| e >= interval).unwrap_or(true),
        None => true,
    }
}
