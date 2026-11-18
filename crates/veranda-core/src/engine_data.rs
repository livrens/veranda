//! Data folders of the database engine that is not active (left in `data/` after switching
//! between MariaDB and MySQL): size, optional archive into `backups/`, and removal.

use crate::Veranda;
use crate::config::DbEngine;
use crate::migrate::{engine_by_id, engine_name};
use crate::packages::Registry;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};

const ENGINES: [DbEngine; 2] = [DbEngine::MariaDb, DbEngine::MySql];

#[derive(Debug, Clone, Serialize)]
pub struct EngineData {
    /// `mariadb` or `mysql`; also the word typed to confirm removal.
    pub engine: String,
    pub name: String,
    pub bytes: u64,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoveReport {
    pub engine: String,
    pub freed: u64,
    pub backup: Option<String>,
}

/// The inactive engines that still have a data folder.
pub fn list(app: &Veranda) -> Vec<EngineData> {
    ENGINES
        .into_iter()
        .filter(|e| *e != app.config.db.engine)
        .filter(|e| {
            std::fs::symlink_metadata(app.layout.data().join(e.id())).is_ok_and(|m| m.is_dir())
        })
        .map(|e| EngineData {
            engine: e.id().into(),
            name: engine_name(e).into(),
            bytes: crate::slim::dir_size(&app.layout.data().join(e.id())),
            running: is_running(app, e),
        })
        .collect()
}

/// True while a server of `engine` may be using its data: the temporary import server always
/// runs the inactive engine, and the main server counts when its program belongs to `engine`
/// or cannot be identified.
pub fn is_running(app: &Veranda, engine: DbEngine) -> bool {
    let layout = &app.layout;
    if crate::process::running_pid(layout, crate::migrate::MIGRATE_INSTANCE).is_some() {
        return true;
    }
    let Some(pid) = crate::process::running_pid(layout, "db") else {
        return false;
    };
    let (Some(exe), Ok(reg)) = (crate::process::pid_exe(pid), Registry::load(layout)) else {
        return true;
    };
    let kind = crate::services::db::kind(engine);
    reg.items
        .iter()
        .any(|i| i.kind == kind && exe.starts_with(&i.path))
}

/// Canonical data folder of `engine`; refused unless it is a real folder (not a link) directly
/// inside Veranda's `data/` and not the active server's data folder.
fn checked_dir(app: &Veranda, engine: DbEngine) -> Result<PathBuf> {
    let base = app
        .layout
        .data()
        .canonicalize()
        .context("Veranda data folder not found")?;
    let dir = app.layout.data().join(engine.id());
    let meta = std::fs::symlink_metadata(&dir)
        .with_context(|| format!("no {} data folder", engine_name(engine)))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        bail!("refusing: {} is a link or not a folder", dir.display());
    }
    let real = dir.canonicalize()?;
    if real.parent() != Some(base.as_path()) {
        bail!(
            "refusing: {} is outside the Veranda data folder",
            real.display()
        );
    }
    // a custom `db.data_dir` may point into this folder
    let active = app.config.db_data_dir(&app.layout);
    if active.starts_with(&dir) || active.canonicalize().is_ok_and(|a| a.starts_with(&real)) {
        bail!("refusing: {} holds the active database", real.display());
    }
    Ok(real)
}

/// Pack `dir` into `backups/<engine>-data-<time>.tar.gz`; links are stored, never followed.
fn archive(app: &Veranda, engine: DbEngine, dir: &Path) -> Result<PathBuf> {
    let backups = app.layout.backups();
    std::fs::create_dir_all(&backups)?;
    let dest = backups.join(format!(
        "{}-data-{}.tar.gz",
        engine.id(),
        crate::fsutil::timestamp()
    ));
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let res: Result<()> = (|| {
        let out = std::fs::File::create(&part)?;
        crate::fsutil::make_private_file(&part)?;
        let enc = flate2::write::GzEncoder::new(
            std::io::BufWriter::new(out),
            flate2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(enc);
        tar.follow_symlinks(false);
        tar.append_dir_all(engine.id(), dir)?;
        tar.into_inner()?
            .finish()?
            .into_inner()
            .map_err(|e| e.into_error())?
            .sync_all()?;
        std::fs::rename(&part, &dest)?;
        Ok(())
    })();
    if let Err(e) = res {
        let _ = std::fs::remove_file(&part);
        return Err(e.context("archiving the data folder; nothing was deleted"));
    }
    Ok(dest)
}

/// Delete the data folder of an inactive, stopped engine. `confirm` must be the engine id;
/// with `backup` the folder is archived first and kept when archiving fails.
pub fn remove(app: &Veranda, engine: &str, confirm: &str, backup: bool) -> Result<RemoveReport> {
    let engine = engine_by_id(engine).context("unknown database engine")?;
    let name = engine_name(engine);
    if engine == app.config.db.engine {
        bail!("{name} is the active database engine; its data cannot be deleted here");
    }
    if !confirm.trim().eq_ignore_ascii_case(engine.id()) {
        bail!("type `{}` to confirm", engine.id());
    }
    if is_running(app, engine) {
        bail!("{name} is running; stop it first");
    }
    let dir = checked_dir(app, engine)?;
    let freed = crate::slim::dir_size(&dir);
    let backup = if backup {
        Some(archive(app, engine, &dir)?)
    } else {
        None
    };
    // archiving can take minutes; check again before deleting
    if is_running(app, engine) {
        bail!("{name} was started meanwhile; nothing was deleted");
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    tracing::info!("deleted the {name} data folder ({freed} bytes)");
    Ok(RemoveReport {
        engine: engine.id().into(),
        freed,
        backup: backup.map(|p| p.display().to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (tempfile::TempDir, Veranda) {
        let root = tempfile::tempdir().unwrap();
        let layout = crate::Layout::new(root.path());
        layout.ensure().unwrap();
        let mut config = crate::Config::default();
        config.db.engine = DbEngine::MySql;
        (root, Veranda { layout, config })
    }

    fn fill(app: &Veranda, engine: &str) -> PathBuf {
        let dir = app.layout.data().join(engine);
        std::fs::create_dir_all(dir.join("mysql")).unwrap();
        std::fs::write(dir.join("mysql/user.ibd"), vec![7u8; 4000]).unwrap();
        std::fs::write(dir.join("ibdata1"), vec![1u8; 1000]).unwrap();
        dir
    }

    #[test]
    fn lists_only_the_inactive_engine() {
        let (_root, app) = app();
        fill(&app, "mariadb");
        fill(&app, "mysql");
        let v = list(&app);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].engine, "mariadb");
        assert_eq!(v[0].bytes, 5000);
        assert!(!v[0].running);
    }

    #[test]
    fn refuses_active_engine_and_wrong_confirmation() {
        let (_root, app) = app();
        let maria = fill(&app, "mariadb");
        let mysql = fill(&app, "mysql");
        assert!(remove(&app, "mysql", "mysql", false).is_err());
        assert!(remove(&app, "mariadb", "MySQL", false).is_err());
        assert!(remove(&app, "../mariadb", "../mariadb", false).is_err());
        assert!(maria.is_dir() && mysql.is_dir());
    }

    #[test]
    fn refuses_folder_holding_custom_active_data_dir() {
        let (_root, mut app) = app();
        let maria = fill(&app, "mariadb");
        app.config.db.data_dir = maria.join("mysql").display().to_string();
        assert!(remove(&app, "mariadb", "mariadb", false).is_err());
        assert!(maria.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_folder() {
        let (_root, app) = app();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), app.layout.data().join("mariadb")).unwrap();
        assert!(list(&app).is_empty());
        assert!(remove(&app, "mariadb", "mariadb", false).is_err());
        assert!(outside.path().join("keep").is_file());
    }

    #[test]
    fn backs_up_then_deletes() {
        let (_root, app) = app();
        let maria = fill(&app, "mariadb");
        fill(&app, "mysql");
        let rep = remove(&app, "mariadb", " MariaDB ", true).unwrap();
        assert_eq!(rep.freed, 5000);
        assert!(!maria.exists());
        assert!(app.layout.data().join("mysql").is_dir());
        let out = tempfile::tempdir().unwrap();
        crate::archive::extract(
            Path::new(rep.backup.as_deref().unwrap()),
            crate::manifest::ArchiveType::TarGz,
            out.path(),
            0,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(out.path().join("mariadb/mysql/user.ibd"))
                .unwrap()
                .len(),
            4000
        );
    }
}
