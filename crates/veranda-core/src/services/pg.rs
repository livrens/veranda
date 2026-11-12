//! PostgreSQL service: official binaries repackaged by the zonky project (Apache-2.0), one
//! data directory per Veranda root, superuser `postgres` with a generated password, loopback only.

use super::{Ctx, STOP_GRACE, ServiceId, ServiceStatus, render};
use crate::manifest::Kind;
use crate::packages::Installed;
use crate::paths::Layout;
use crate::{fsutil, process};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

const PID: &str = "pg";
const CONF_TPL: &str = include_str!("../templates/postgresql.conf.tpl");
const HBA_TPL: &str = include_str!("../templates/pg_hba.conf.tpl");

pub struct PgCtx {
    pub pkg: Installed,
    pub port: u16,
    pub data_dir: PathBuf,
    pub layout: Layout,
}

pub fn ctx(ctx: &Ctx) -> Option<PgCtx> {
    let cfg = &ctx.app.config;
    let pkg = ctx.reg.active(Kind::Postgres, &cfg.pg.version)?.clone();
    Some(PgCtx {
        pkg,
        port: cfg.pg.port,
        data_dir: cfg.pg_data_dir(&ctx.app.layout),
        layout: ctx.app.layout.clone(),
    })
}

pub fn conf_dir(layout: &Layout) -> PathBuf {
    layout.etc().join("postgres")
}

pub fn password_file(layout: &Layout) -> PathBuf {
    layout.secrets().join("pg-root.txt")
}

pub fn password(layout: &Layout) -> Result<Option<String>> {
    match std::fs::read_to_string(password_file(layout)) {
        Ok(s) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn ensure_password(layout: &Layout) -> Result<String> {
    if let Some(pw) = password(layout)? {
        return Ok(pw);
    }
    let pw = crate::db::random_token(24);
    fsutil::write_private(&password_file(layout), pw.as_bytes())?;
    Ok(pw)
}

/// Unix socket directory (`sun_path` is limited to ~107 bytes, so long roots use the temp dir).
fn socket_dir(layout: &Layout, port: u16) -> PathBuf {
    let preferred = layout.run();
    if preferred.as_os_str().len() <= 80 {
        preferred
    } else {
        std::env::temp_dir().join(format!("veranda-pg-{port}"))
    }
}

fn bin(pg: &PgCtx, name: &str) -> PathBuf {
    pg.pkg
        .path
        .join("bin")
        .join(crate::Platform::current().exe(name))
}

/// Write postgresql.conf and pg_hba.conf. Cheap; done before every start.
pub fn prepare(ctx: &Ctx) -> Result<()> {
    let Some(pg) = self::ctx(ctx) else {
        return Ok(());
    };
    let layout = &ctx.app.layout;
    let dir = conf_dir(layout);
    std::fs::create_dir_all(&dir)?;
    let sock = if ctx.app.platform().is_windows() {
        String::new()
    } else {
        let d = socket_dir(layout, pg.port);
        std::fs::create_dir_all(&d)?;
        Layout::slashes(&d)
    };
    let conf = render(
        CONF_TPL,
        &[
            ("PORT", pg.port.to_string()),
            ("SOCKET_DIR", sock),
            ("LOG_DIR", Layout::slashes(&layout.logs())),
            (
                "DSM",
                if ctx.app.platform().is_windows() {
                    "windows".to_string()
                } else {
                    "posix".to_string()
                },
            ),
        ],
    );
    fsutil::write_if_changed(&dir.join("postgresql.conf"), &conf)?;
    fsutil::write_if_changed(&dir.join("pg_hba.conf"), HBA_TPL)?;
    Ok(())
}

pub fn status(ctx: &Ctx) -> ServiceStatus {
    let pg = self::ctx(ctx);
    let pid = process::running_pid(&ctx.app.layout, PID);
    ServiceStatus {
        id: ServiceId::Pg,
        name: "PostgreSQL".into(),
        enabled: true,
        installed: pg.is_some(),
        version: pg
            .as_ref()
            .map(|p| p.pkg.version.clone())
            .unwrap_or_default(),
        running: pid.is_some(),
        pids: pid.into_iter().collect(),
        ports: vec![ctx.app.config.pg.port],
        message: if pg.is_none() {
            "not installed".into()
        } else {
            String::new()
        },
    }
}

fn initialize(pg: &PgCtx) -> Result<()> {
    let pw = ensure_password(&pg.layout)?;
    let pwfile = pg.layout.tmp().join("pg-init-pw.txt");
    fsutil::write_private(&pwfile, format!("{pw}\n").as_bytes())?;
    std::fs::create_dir_all(&pg.data_dir)?;
    fsutil::make_private_dir(&pg.data_dir)?;
    let initdb = bin(pg, "initdb");
    if !initdb.is_file() {
        bail!("initdb not found in {}", pg.pkg.path.display());
    }
    let mut cmd = crate::process::new_cmd(&initdb);
    cmd.arg("-D")
        .arg(&pg.data_dir)
        .args(["-U", "postgres", "-E", "UTF8", "--no-locale"])
        .arg(format!("--pwfile={}", pwfile.display()))
        .args(["--auth-host=scram-sha-256", "--auth-local=scram-sha-256"])
        .current_dir(&pg.pkg.path);
    let res = process::run_capture(cmd).context("initdb");
    let _ = std::fs::remove_file(&pwfile);
    res.map(|_| ())
}

pub fn start(ctx: &Ctx) -> Result<()> {
    let pg = self::ctx(ctx).ok_or_else(|| super::not_installed(Kind::Postgres))?;
    let layout = &ctx.app.layout;
    prepare(ctx)?;
    if process::running_pid(layout, PID).is_some() {
        return Ok(());
    }
    process::clear_pid(layout, PID);
    super::ensure_port_free(pg.port, "PostgreSQL")?;
    if !pg.data_dir.join("PG_VERSION").is_file() {
        initialize(&pg).context("initialising the PostgreSQL data directory")?;
    }
    let server = bin(&pg, "postgres");
    if !server.is_file() {
        bail!("postgres binary not found: {}", server.display());
    }
    let dir = conf_dir(layout);
    let mut cmd = process::command(&server, &ctx.path_entries());
    cmd.arg("-D")
        .arg(&pg.data_dir)
        .arg("-c")
        .arg(format!(
            "config_file={}",
            dir.join("postgresql.conf").display()
        ))
        .arg("-c")
        .arg(format!("hba_file={}", dir.join("pg_hba.conf").display()))
        .current_dir(&pg.pkg.path);
    process::spawn_detached(layout, PID, cmd)?;
    if !process::wait_for_port(pg.port, std::time::Duration::from_secs(60)) {
        bail!(
            "PostgreSQL did not start listening on port {}. See logs/pg.log",
            pg.port
        );
    }
    Ok(())
}

pub fn stop(ctx: &Ctx) -> Result<()> {
    let layout = &ctx.app.layout;
    if let (Some(pg), Some(pid)) = (self::ctx(ctx), process::running_pid(layout, PID)) {
        // pg_ctl does a clean fast shutdown; fall back to the signal path below.
        let pg_ctl = bin(&pg, "pg_ctl");
        if pg_ctl.is_file() {
            let mut cmd = crate::process::new_cmd(pg_ctl);
            cmd.arg("stop")
                .arg("-D")
                .arg(&pg.data_dir)
                .args(["-m", "fast", "-t", "10", "-s"]);
            let _ = process::run_capture(cmd);
            let start = std::time::Instant::now();
            while start.elapsed() < STOP_GRACE && process::is_pid_alive(pid) {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    }
    process::stop_named(layout, PID, std::time::Duration::from_secs(12))
}
