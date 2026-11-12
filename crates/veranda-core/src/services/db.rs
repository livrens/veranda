//! MariaDB / MySQL server lifecycle (initialisation, root password, start, stop).

use super::{Ctx, STOP_GRACE, ServiceId, ServiceStatus, render};
use crate::config::DbEngine;
use crate::db::DbCtx;
use crate::manifest::Kind;
use crate::paths::Layout;
use crate::process;
use anyhow::{Context, Result, bail};

const PID: &str = "db";
const MY_CNF: &str = include_str!("../templates/my.cnf.tpl");

pub fn kind(engine: DbEngine) -> Kind {
    match engine {
        DbEngine::MariaDb => Kind::MariaDb,
        DbEngine::MySql => Kind::MySql,
    }
}

pub fn conf_path(layout: &Layout, engine: DbEngine) -> std::path::PathBuf {
    layout.etc().join(engine.id()).join("my.cnf")
}

/// Unix socket path. `sun_path` is limited to ~107 bytes, so very long roots fall back to the
/// system temp directory with a name that includes the port to stay unique.
pub fn socket_path(layout: &Layout, db: &DbCtx) -> std::path::PathBuf {
    let preferred = layout.run().join(format!("{}.sock", db.engine.id()));
    if preferred.as_os_str().len() <= 100 {
        preferred
    } else {
        std::env::temp_dir().join(format!("veranda-{}-{}.sock", db.engine.id(), db.port))
    }
}

/// Marker written once the data directory is initialised *and* the root password is set.
fn ready_marker(db: &DbCtx) -> std::path::PathBuf {
    db.data_dir.join(".veranda-ready")
}

pub fn prepare(ctx: &Ctx) -> Result<()> {
    let Some(db) = DbCtx::from_app(ctx.app, ctx.reg) else {
        return Ok(());
    };
    let layout = &ctx.app.layout;
    let conf = conf_path(layout, db.engine);
    std::fs::create_dir_all(conf.parent().unwrap())?;
    std::fs::create_dir_all(&db.data_dir)?;
    let windows = ctx.app.platform().is_windows();
    let socket = socket_path(layout, &db);
    write_conf(layout, &db, windows, &conf, &socket, db.engine.id())
}

/// Render my.cnf for `db` into `conf`. `instance` names the log and pid files: the engine id for
/// the regular server, something else for a temporary one (the migration source).
pub fn write_conf(
    layout: &Layout,
    db: &DbCtx,
    windows: bool,
    conf: &std::path::Path,
    socket: &std::path::Path,
    instance: &str,
) -> Result<()> {
    let (client_socket, server_socket) = if windows {
        (String::new(), String::new())
    } else {
        (
            format!("socket = \"{}\"", Layout::slashes(socket)),
            format!("socket = \"{}\"", Layout::slashes(socket)),
        )
    };
    let extra_security = match db.engine {
        DbEngine::MariaDb => "".to_string(),
        // no X plugin, no binary log, a 32 MB redo log instead of MySQL's 100 MB default
        DbEngine::MySql => "mysqlx = OFF\nskip-log-bin\ninnodb_redo_log_capacity = 32M".to_string(),
    };
    // Redo-log sizing differs per engine: MariaDB still uses innodb_log_file_size, while MySQL 8.3+
    // (e.g. 9.x) rejects it as an unknown variable and uses innodb_redo_log_capacity (set above).
    let engine_tuning = match db.engine {
        DbEngine::MariaDb => "innodb_log_file_size = 64M".to_string(),
        DbEngine::MySql => "".to_string(),
    };
    let text = render(
        MY_CNF,
        &[
            ("PORT", db.port.to_string()),
            ("ENGINE_TUNING", engine_tuning),
            ("CLIENT_SOCKET", client_socket),
            ("BASEDIR", Layout::slashes(&db.pkg.path)),
            ("DATADIR", Layout::slashes(&db.data_dir)),
            ("TMP_DIR", Layout::slashes(&layout.tmp())),
            ("SERVER_SOCKET", server_socket),
            (
                "PID_FILE",
                format!(
                    "pid-file = \"{}/{}.internal.pid\"",
                    Layout::slashes(&layout.run()),
                    instance
                ),
            ),
            ("LOG_DIR", Layout::slashes(&layout.logs())),
            ("ENGINE", instance.to_string()),
            ("EXTRA_SECURITY", extra_security),
            (
                "FLUSH_METHOD",
                if windows {
                    "unbuffered".into()
                } else {
                    "fsync".into()
                },
            ),
        ],
    );
    crate::fsutil::write_if_changed(conf, &text)?;
    Ok(())
}

pub fn status(ctx: &Ctx) -> ServiceStatus {
    let db = DbCtx::from_app(ctx.app, ctx.reg);
    let pid = process::running_pid(&ctx.app.layout, PID);
    let name = match ctx.app.config.db.engine {
        DbEngine::MariaDb => "MariaDB",
        DbEngine::MySql => "MySQL",
    };
    let running = pid.is_some()
        || (db.is_some() && process::port_accepting(ctx.app.config.db.port) && pid.is_some());
    ServiceStatus {
        id: ServiceId::Db,
        name: name.into(),
        enabled: true,
        installed: db.is_some(),
        version: db
            .as_ref()
            .map(|d| d.pkg.version.clone())
            .unwrap_or_default(),
        running,
        pids: pid.into_iter().collect(),
        ports: vec![ctx.app.config.db.port],
        message: if db.is_none() {
            "not installed".into()
        } else {
            String::new()
        },
    }
}

/// Generic Linux tarballs of MySQL/MariaDB link `libaio.so.1`. Debian/Ubuntu 24.04+ ship it as
/// `libaio.so.1t64`, which the loader does not match, so the server dies with exit 127.
/// The binaries carry `RUNPATH=$ORIGIN/../lib/private`; dropping a symlink there fixes it
/// without touching the system. Missing entirely => a clear install hint.
#[cfg(target_os = "linux")]
pub fn ensure_runtime_libs(pkg_dir: &std::path::Path) -> Result<()> {
    const DIRS: [&str; 6] = [
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
        "/lib/x86_64-linux-gnu",
        "/lib64",
    ];
    let find = |name: &str| {
        DIRS.iter()
            .map(|d| std::path::Path::new(d).join(name))
            .find(|p| p.exists())
    };
    let private = pkg_dir.join("lib").join("private");
    let shim = private.join("libaio.so.1");
    if shim.exists() || find("libaio.so.1").is_some() {
        return Ok(());
    }
    match find("libaio.so.1t64") {
        Some(sys) => {
            std::fs::create_dir_all(&private)?;
            std::os::unix::fs::symlink(&sys, &shim)
                .with_context(|| format!("linking {} -> {}", shim.display(), sys.display()))?;
            Ok(())
        }
        None => bail!(
            "the database server needs the libaio library. Install it and try again: \
             `sudo apt install libaio1t64` (Debian/Ubuntu), `sudo dnf install libaio` (Fedora), \
             `sudo pacman -S libaio` (Arch)"
        ),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn ensure_runtime_libs(_pkg_dir: &std::path::Path) -> Result<()> {
    Ok(())
}

pub fn start(ctx: &Ctx) -> Result<()> {
    let db = DbCtx::from_app(ctx.app, ctx.reg)
        .ok_or_else(|| super::not_installed(kind(ctx.app.config.db.engine)))?;
    let layout = &ctx.app.layout;
    prepare(ctx)?;
    if process::running_pid(layout, PID).is_some() {
        return Ok(());
    }
    process::clear_pid(layout, PID);
    super::ensure_port_free(db.port, "the database")?;
    ensure_runtime_libs(&db.pkg.path)?;
    let needs_setup = !ready_marker(&db).exists();
    if !db.is_initialized() {
        crate::db::initialize(&db).context("initialising the database directory")?;
    }
    let server = db.server_binary();
    if !server.is_file() {
        bail!("database server binary not found: {}", server.display());
    }
    let mut cmd = process::command(&server, &ctx.path_entries());
    cmd.arg(format!(
        "--defaults-file={}",
        conf_path(layout, db.engine).display()
    ))
    .current_dir(&db.pkg.path);
    if ctx.app.platform().is_windows() {
        cmd.arg("--console");
    }
    process::spawn_detached(layout, PID, cmd)?;
    if !process::wait_for_port(db.port, std::time::Duration::from_secs(60)) {
        bail!(
            "database did not start listening on port {}. See logs/db.log and logs/{}.log",
            db.port,
            db.engine.id()
        );
    }
    // The server opens its port before start-up is complete and may still abort afterwards
    // (for example on a rejected my.cnf variable). Wait until SQL actually answers, and if the
    // process dies meanwhile surface the tail of its error log instead of a bare "can't connect".
    wait_until_sql_ready(layout, &db, std::time::Duration::from_secs(45))?;
    if needs_setup {
        // A start that failed half-way (after init, before the password) is retried here:
        // `set_root_password` first tries an empty password, then the stored one.
        let pw = crate::db::ensure_root_password_file(layout)?;
        crate::db::set_root_password(&db, None, &pw)
            .context("setting the initial root password")?;
        std::fs::write(ready_marker(&db), b"ok\n")?;
    }
    Ok(())
}

fn error_log_tail(layout: &Layout, db: &DbCtx, lines: usize) -> String {
    let path = layout.logs().join(format!("{}.log", db.engine.id()));
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let all: Vec<&str> = text.lines().collect();
            let start = all.len().saturating_sub(lines);
            all[start..].join("\n")
        }
        Err(_) => format!("(log not readable: {})", path.display()),
    }
}

/// Poll `SELECT 1` (with an empty or the stored root password) until the server answers.
/// Fails early with the error-log tail when the server process is gone.
pub(crate) fn wait_until_sql_ready(
    layout: &Layout,
    db: &DbCtx,
    timeout: std::time::Duration,
) -> Result<()> {
    let started = std::time::Instant::now();
    let stored = crate::db::root_password(layout).ok().flatten();
    // "Access denied" is an answer: the SQL layer is up, only the credentials differ (a password
    // changed outside Veranda, or the stored one not applied yet). Start-up must not fail on
    // that; the password step and the Database page say so in their own words.
    let answered = |msg: &str| {
        msg.contains("Access denied")
            || msg.contains("ERROR 1045")
            || msg.contains("ERROR 1130")
            || msg.contains("ERROR 2061")
    };
    let mut last = String::new();
    loop {
        let mut ok = false;
        for pw in std::iter::once(None).chain(stored.as_deref().map(Some)) {
            match db.sql("SELECT 1", pw) {
                Ok(_) => {
                    ok = true;
                    break;
                }
                Err(e) => {
                    last = format!("{e:#}");
                    if answered(&last) {
                        tracing::warn!("{} answers SQL but rejects root: {last}", db.engine.id());
                        ok = true;
                        break;
                    }
                }
            }
        }
        if ok {
            return Ok(());
        }
        if process::running_pid(layout, PID).is_none() {
            bail!(
                "the database server exited during start-up. Last lines of logs/{}.log:\n{}",
                db.engine.id(),
                error_log_tail(layout, db, 12)
            );
        }
        if started.elapsed() >= timeout {
            bail!(
                "the database server is not answering SQL after {}s. Client said: {}. Last lines of logs/{}.log:\n{}",
                timeout.as_secs(),
                last.lines().last().unwrap_or("").trim(),
                db.engine.id(),
                error_log_tail(layout, db, 12)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

pub fn stop(ctx: &Ctx) -> Result<()> {
    let layout = &ctx.app.layout;
    if let (Some(db), Some(pid)) = (
        DbCtx::from_app(ctx.app, ctx.reg),
        process::running_pid(layout, PID),
    ) {
        // Clean shutdown flushes InnoDB; fall back to a signal if the password is unknown.
        if let Ok(Some(pw)) = crate::db::root_password(layout) {
            let _ = crate::db::shutdown(&db, &pw);
            let start = std::time::Instant::now();
            while start.elapsed() < STOP_GRACE && process::is_pid_alive(pid) {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    }
    process::stop_named(layout, PID, std::time::Duration::from_secs(12))
}
