//! Move the databases of an old MySQL 5.6 data folder into the active MariaDB. MariaDB is fully
//! backed up first. The folder is only read: a private copy is made under `data/`, a temporary
//! MySQL 5.6.51 opens the copy with `--skip-grant-tables --skip-networking` (socket only, low
//! priority), every user database is dumped and loaded with the MariaDB tools and then checked
//! (object counts, row counts, character sets, mariadb-check). Afterwards the server is stopped
//! and the copy and dumps are deleted. User accounts are not moved; the report lists them only.

use crate::Veranda;
use crate::config::DbEngine;
use crate::db::{DbCtx, SYSTEM_DBS};
use crate::manifest::Kind;
use crate::migrate::Progress;
use crate::packages::{Binaries, Installed, Registry};
use crate::paths::Layout;
use crate::{fsutil, process};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Process name of the temporary server (pid file and `logs/<name>.log`).
pub const INSTANCE: &str = "mysql56-import";
const WORK_PREFIX: &str = ".mysql56-import-";
const MARKER: &str = ".veranda-mysql56-import";

static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Stop after the current step; the server is stopped and the copy removed as usual.
pub fn request_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

fn cancelled() -> bool {
    CANCEL.load(Ordering::SeqCst)
}

#[derive(Debug, Clone, Serialize)]
pub struct Inspect {
    pub bytes: u64,
    /// Copy plus the largest dump, with some room to spare.
    pub needed: u64,
    pub free: Option<u64>,
    pub enough: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    /// `VERSION()` of the temporary MySQL and of the active MariaDB.
    pub source_version: String,
    pub target_version: String,
    /// Full MariaDB dump made before anything was imported.
    pub backup: String,
    /// Imported and verified.
    pub moved: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<String>,
    /// Imported, but the check found differences; kept for inspection.
    pub mismatched: Vec<String>,
    /// Collation changes that keep the character set.
    pub notes: Vec<String>,
    /// Old accounts, `'user'@'host'`, not moved.
    pub accounts: Vec<String>,
}

/// Canonical path of `dir` when it is a MySQL 5.6 data folder that no server is using.
fn check_source(app: &Veranda, dir: &Path) -> Result<PathBuf> {
    let real = dir
        .canonicalize()
        .with_context(|| format!("{} not found", dir.display()))?;
    if !real.is_dir() {
        bail!("{} is not a folder", real.display());
    }
    let has = |p: &str| real.join(p).exists();
    crate::services::db::refuse_newer_than_56(&real)?;
    if has("mysql/server_cost.frm") || has("mysql/gtid_executed.frm") {
        bail!("this is a MySQL 5.7 data folder; only MySQL 5.6 data is supported here");
    }
    if has("aria_log_control") || has("mysql/global_priv.frm") {
        bail!("this is a MariaDB data folder; only MySQL 5.6 data is supported here");
    }
    if !has("mysql/user.frm") || !has("ibdata1") {
        bail!("not a MySQL data folder: mysql/user.frm and ibdata1 are missing");
    }
    match std::fs::read_to_string(real.join("mysql_upgrade_info")) {
        Ok(v) if !v.trim().starts_with("5.6.") => {
            let v: String = v
                .trim()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || ".-".contains(*c))
                .take(24)
                .collect();
            bail!("this folder was last upgraded by MySQL {v}; only 5.6 is supported here");
        }
        Err(_) if !has("mysql/innodb_table_stats.frm") => {
            bail!("not a MySQL 5.6 data folder: mysql/innodb_table_stats.frm is missing");
        }
        _ => {}
    }
    let active = app.config.db_data_dir(&app.layout);
    if active.canonicalize().is_ok_and(|a| a == real) {
        bail!("this is the active database's own data folder");
    }
    if real
        .components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with(WORK_PREFIX))
    {
        bail!("this is a temporary copy made by Veranda");
    }
    // a copy of a running server's files would be inconsistent
    for e in std::fs::read_dir(&real)?.flatten() {
        if e.path().extension().is_some_and(|x| x == "pid")
            && let Some(pid) = std::fs::read_to_string(e.path())
                .ok()
                .and_then(|t| t.trim().parse::<u32>().ok())
            && process::is_pid_alive(pid)
        {
            bail!("a MySQL server is still using this folder; stop it first");
        }
    }
    Ok(real)
}

#[cfg(unix)]
fn free_bytes(p: &Path) -> Option<u64> {
    let out = process::new_cmd("df")
        .arg("-Pk")
        .arg(p)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let kb: u64 = text
        .lines()
        .nth(1)?
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()?;
    Some(kb * 1024)
}

#[cfg(not(unix))]
fn free_bytes(_p: &Path) -> Option<u64> {
    None
}

fn inspect_real(app: &Veranda, dir: &Path) -> Result<(PathBuf, Inspect)> {
    let real = check_source(app, dir)?;
    let bytes = crate::slim::dir_size(&real);
    let needed = bytes.saturating_mul(2).saturating_add(256 << 20);
    let free = free_bytes(&app.layout.data());
    let enough = free.is_none_or(|f| f >= needed);
    Ok((
        real,
        Inspect {
            bytes,
            needed,
            free,
            enough,
        },
    ))
}

/// Check the folder and estimate the disk space the import needs.
pub fn inspect(app: &Veranda, dir: &Path) -> Result<Inspect> {
    inspect_real(app, dir).map(|(_, i)| i)
}

/// The temporary files of one import; dropping it stops the server and deletes them.
struct Work {
    layout: Layout,
    root: PathBuf,
    run: PathBuf,
    owner: PathBuf,
}

impl Work {
    fn create(layout: &Layout, owner: PathBuf) -> Result<Self> {
        let w = Self {
            layout: layout.clone(),
            root: layout
                .data()
                .join(format!("{WORK_PREFIX}{}", fsutil::timestamp())),
            run: layout.run().join(INSTANCE),
            owner,
        };
        for d in [&w.root, &w.root.join("tmp"), &w.run] {
            std::fs::create_dir_all(d)?;
            fsutil::make_private_dir(d)?;
        }
        std::fs::write(w.root.join(MARKER), b"")?;
        Ok(w)
    }
    fn data(&self) -> PathBuf {
        self.root.join("data")
    }
    fn socket(&self) -> PathBuf {
        self.run.join("mysqld.sock")
    }
    fn dump(&self) -> PathBuf {
        self.root.join("dump.sql")
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        stop_server(&self.layout);
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(&self.run);
        let _ = std::fs::remove_file(&self.owner);
    }
}

/// SIGTERM is a clean shutdown for mysqld. Only a process that is mysqld is signalled.
fn stop_server(layout: &Layout) {
    if let Some(pid) = process::read_pid(layout, INSTANCE) {
        if process::pid_exe(pid).is_some_and(|e| e.file_name().is_some_and(|n| n == "mysqld")) {
            let _ = process::stop_pid(pid, Duration::from_secs(30));
        }
        process::clear_pid(layout, INSTANCE);
    }
}

/// Leftovers of an import that was killed (the app has `panic = "abort"`, so `Drop` does not
/// run then): the server is stopped and only folders carrying our marker are deleted.
fn cleanup_stale(layout: &Layout) {
    stop_server(layout);
    let _ = std::fs::remove_dir_all(layout.run().join(INSTANCE));
    let Ok(rd) = std::fs::read_dir(layout.data()) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_name().to_string_lossy().starts_with(WORK_PREFIX)
            && e.file_type().is_ok_and(|t| t.is_dir())
            && p.join(MARKER).is_file()
        {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

/// Mark this process as the one importing; refused while another Veranda process does.
fn claim(layout: &Layout) -> Result<PathBuf> {
    let owner = layout.run().join(format!("{INSTANCE}.owner"));
    if let Some(pid) = std::fs::read_to_string(&owner)
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
        && pid != std::process::id()
        && process::pid_exe(pid).is_some_and(|e| {
            e.file_name()
                .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().contains("veranda"))
        })
    {
        bail!("another Veranda window or command is already importing MySQL 5.6 data");
    }
    cleanup_stale(layout);
    fsutil::write_atomic(&owner, std::process::id().to_string().as_bytes())?;
    Ok(owner)
}

/// Copy regular files and folders; links, sockets, pid and error-log files are left out.
fn copy_tree(src: &Path, dst: &Path, copied: &mut u64, tick: &mut dyn FnMut(u64)) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    fsutil::make_private_dir(dst)?;
    let rd = std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))?;
    for e in rd {
        if cancelled() {
            bail!("cancelled");
        }
        let e = e?;
        let ty = e.file_type()?;
        let to = dst.join(e.file_name());
        if ty.is_dir() {
            copy_tree(&e.path(), &to, copied, tick)?;
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if !ty.is_file() || [".pid", ".sock", ".err"].iter().any(|x| name.ends_with(x)) {
            continue;
        }
        *copied += std::fs::copy(e.path(), &to)
            .with_context(|| format!("copying {}", e.path().display()))?;
        fsutil::make_private_file(&to)?;
        tick(*copied);
    }
    Ok(())
}

fn start_server(layout: &Layout, pkg: &Installed, work: &Work) -> Result<()> {
    let src = DbCtx {
        engine: DbEngine::MySql,
        port: 0,
        pkg: pkg.clone(),
        data_dir: work.data(),
        layout: layout.clone(),
    };
    crate::services::db::refuse_newer_than_56(&src.data_dir)?;
    crate::services::db::ensure_runtime_libs(&pkg.path)?;
    let mysqld = src.server_binary();
    if !mysqld.is_file() {
        bail!("mysqld not found in the MySQL 5.6 package");
    }
    let nice = Path::new("/usr/bin/nice");
    let mut cmd = if nice.is_file() {
        let mut c = process::new_cmd(nice);
        c.args(["-n", "19"]).arg(&mysqld);
        c
    } else {
        process::new_cmd(&mysqld)
    };
    src.apply_lib_path(&mut cmd);
    // --no-defaults must come first
    cmd.arg("--no-defaults")
        .arg(format!("--basedir={}", pkg.path.display()))
        .arg(format!("--datadir={}", src.data_dir.display()))
        .arg(format!("--tmpdir={}", work.root.join("tmp").display()))
        .arg(format!("--socket={}", work.socket().display()))
        .arg(format!(
            "--pid-file={}",
            work.run.join("mysqld.pid").display()
        ))
        .args([
            "--skip-grant-tables",
            "--skip-networking",
            "--skip-name-resolve",
            "--event-scheduler=OFF",
        ])
        .current_dir(&pkg.path);
    process::spawn_detached(layout, INSTANCE, cmd)?;
    Ok(())
}

/// SQL on the temporary server through the MariaDB client. No password: the server skips the
/// grant tables.
fn query(target: &DbCtx, socket: &Path, sql: &str) -> Result<String> {
    let mut cmd = target.cmd(Binaries::db_client(&target.pkg, target.engine));
    cmd.args(["--no-defaults", "--skip-ssl", "--protocol=socket"])
        .arg(format!("--socket={}", socket.display()))
        .args(["-u", "root", "-N", "-B", "-e", sql])
        .env_remove("MYSQL_PWD")
        .stdin(Stdio::null());
    let out = cmd.output().context("running the database client")?;
    if !out.status.success() {
        bail!("{}", short(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn wait_ready(layout: &Layout, target: &DbCtx, socket: &Path) -> Result<()> {
    let started = Instant::now();
    loop {
        if socket.exists() && query(target, socket, "SELECT 1").is_ok() {
            return Ok(());
        }
        if process::running_pid(layout, INSTANCE).is_none() {
            bail!("MySQL 5.6 stopped while opening the copy; see logs/{INSTANCE}.log");
        }
        if cancelled() {
            bail!("cancelled");
        }
        if started.elapsed() > Duration::from_secs(180) {
            bail!("MySQL 5.6 did not open the copy within 3 minutes; see logs/{INSTANCE}.log");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn dump_db(target: &DbCtx, socket: &Path, name: &str, dest: &Path) -> Result<()> {
    let file = std::fs::File::create(dest)?;
    fsutil::make_private_file(dest)?;
    let mut cmd = target.cmd(Binaries::db_dump(&target.pkg, target.engine));
    cmd.args(["--no-defaults", "--skip-ssl", "--protocol=socket"])
        .arg(format!("--socket={}", socket.display()))
        .args([
            "-u",
            "root",
            "--single-transaction",
            "--routines",
            "--triggers",
            "--events",
            "--default-character-set=utf8mb4",
            "--databases",
            name,
        ])
        .env_remove("MYSQL_PWD")
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::piped());
    let out = cmd.output().context("running mariadb-dump")?;
    if !out.status.success() {
        bail!(
            "dump failed: {}",
            short(&String::from_utf8_lossy(&out.stderr))
        );
    }
    Ok(())
}

/// First line of a tool's message, without the quoted SQL after "near" (it may hold data).
fn short(msg: &str) -> String {
    let line = msg
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.contains("MYSQL_PWD"))
        .unwrap_or("no message");
    let line = line.split(" near '").next().unwrap_or(line);
    line.chars().take(200).collect()
}

/// Full dump of the active MariaDB into `backups/before-mysql56-import/` (outside rotation).
fn backup_target(layout: &Layout, target: &DbCtx) -> Result<PathBuf> {
    let dir = layout.backups().join("before-mysql56-import");
    std::fs::create_dir_all(&dir)?;
    fsutil::make_private_dir(&dir)?;
    let dest = dir.join(format!(
        "{}-{}.sql.gz",
        target.engine.id(),
        fsutil::timestamp()
    ));
    crate::db::dump_all(target, &dest)
        .context("the full MariaDB backup failed; nothing was imported")?;
    Ok(dest)
}

fn quote_ident(s: &str) -> String {
    format!("`{}`", s.replace('`', "``"))
}

/// Character set of a collation name; MariaDB reports MySQL's `utf8` as `utf8mb3`.
fn charset_of(collation: &str) -> &str {
    match collation.split('_').next().unwrap_or("") {
        "utf8mb3" => "utf8",
        c => c,
    }
}

fn objects_sql(db: &str) -> String {
    format!(
        "SELECT 'tables', COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA='{db}' AND TABLE_TYPE='BASE TABLE' \
         UNION ALL SELECT 'views', COUNT(*) FROM information_schema.VIEWS WHERE TABLE_SCHEMA='{db}' \
         UNION ALL SELECT 'routines', COUNT(*) FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA='{db}' \
         UNION ALL SELECT 'triggers', COUNT(*) FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA='{db}' \
         UNION ALL SELECT 'events', COUNT(*) FROM information_schema.EVENTS WHERE EVENT_SCHEMA='{db}'"
    )
}

fn tables_sql(db: &str) -> String {
    format!(
        "SELECT TABLE_NAME, IFNULL(TABLE_COLLATION, '') FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA='{db}' AND TABLE_TYPE='BASE TABLE'"
    )
}

fn pairs(out: &str) -> std::collections::BTreeMap<String, String> {
    out.lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(a, b)| (a.to_string(), b.trim().to_string()))
        .collect()
}

#[derive(Default)]
struct Check {
    differences: Vec<String>,
    notes: Vec<String>,
}

/// Compare one imported database with the old copy. `name` passed `is_valid_db_name`, so it
/// is safe inside quotes.
fn verify(target: &DbCtx, pw: &str, socket: &Path, name: &str) -> Result<Check> {
    let mut c = Check::default();
    let old = |sql: &str| query(target, socket, sql);
    let new = |sql: &str| target.sql(sql, Some(pw));

    let (a, b) = (
        pairs(&old(&objects_sql(name))?),
        pairs(&new(&objects_sql(name))?),
    );
    for (kind, n) in &a {
        let m = b.get(kind).map(String::as_str).unwrap_or("?");
        if n != m {
            c.differences.push(format!("{kind}: {n} before, {m} after"));
        }
    }

    let schema = format!(
        "SELECT DEFAULT_CHARACTER_SET_NAME, DEFAULT_COLLATION_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME='{name}'"
    );
    let (sa, sb) = (pairs(&old(&schema)?), pairs(&new(&schema)?));
    if let (Some((cs_a, co_a)), Some((cs_b, co_b))) = (sa.iter().next(), sb.iter().next()) {
        if charset_of(cs_a) != charset_of(cs_b) {
            c.differences.push(format!(
                "database character set: {cs_a} before, {cs_b} after"
            ));
        } else if co_a != co_b {
            c.notes
                .push(format!("{name}: default collation {co_a} is now {co_b}"));
        }
    }

    let (ta, tb) = (
        pairs(&old(&tables_sql(name))?),
        pairs(&new(&tables_sql(name))?),
    );
    for (t, co_a) in &ta {
        match tb.get(t) {
            None => c.differences.push(format!("table {t} is missing")),
            Some(co_b) if charset_of(co_a) != charset_of(co_b) => c.differences.push(format!(
                "table {t}: character set {co_a} before, {co_b} after"
            )),
            Some(co_b) if co_a != co_b => c
                .notes
                .push(format!("{name}.{t}: collation {co_a} is now {co_b}")),
            _ => {}
        }
    }

    let tables: Vec<&String> = ta.keys().filter(|t| tb.contains_key(*t)).collect();
    if !tables.is_empty() {
        let sql: String = tables
            .iter()
            .map(|t| {
                format!(
                    "SELECT COUNT(*) FROM {}.{};",
                    quote_ident(name),
                    quote_ident(t)
                )
            })
            .collect();
        let (ra, rb) = (old(&sql)?, new(&sql)?);
        let (ra, rb): (Vec<&str>, Vec<&str>) = (ra.lines().collect(), rb.lines().collect());
        for (i, t) in tables.iter().enumerate() {
            let (x, y) = (ra.get(i).copied(), rb.get(i).copied());
            if x != y {
                c.differences.push(format!(
                    "table {t}: {} rows before, {} after",
                    x.unwrap_or("?"),
                    y.unwrap_or("?")
                ));
            }
        }
    }

    let mut cmd = target.cmd(Binaries::db_check(&target.pkg, target.engine));
    cmd.args(target.client_base_args());
    cmd.args([
        "--protocol=tcp",
        "-h",
        "127.0.0.1",
        "-P",
        &target.port.to_string(),
        "-u",
        "root",
        "--silent",
        "--databases",
        name,
    ])
    .env("MYSQL_PWD", pw)
    .stdin(Stdio::null());
    let out = cmd.output().context("running mariadb-check")?;
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    let errors: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.to_ascii_lowercase().starts_with("error"))
        .collect();
    if !out.status.success() || !errors.is_empty() {
        let first = errors.first().copied().unwrap_or("failed");
        c.differences
            .push(format!("mariadb-check: {}", short(first)));
    }
    Ok(c)
}

/// Run the import. Databases that already exist in the active server are skipped, never
/// overwritten; a failing database is reported and the others continue.
pub fn run(app: &Veranda, dir: &Path, progress: &dyn Fn(Progress)) -> Result<Report> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        bail!("a MySQL 5.6 import is already running");
    }
    struct Running;
    impl Drop for Running {
        fn drop(&mut self) {
            RUNNING.store(false, Ordering::SeqCst);
        }
    }
    let _running = Running;
    CANCEL.store(false, Ordering::SeqCst);
    let say = |message: String, percent: u8| {
        progress(Progress {
            message,
            percent,
            ..Default::default()
        })
    };
    let layout = &app.layout;
    let (real, insp) = inspect_real(app, dir)?;
    if !insp.enough {
        bail!(
            "not enough free disk space: about {} MB needed, {} MB free",
            insp.needed >> 20,
            insp.free.unwrap_or(0) >> 20
        );
    }
    let reg = Registry::load(layout)?;
    let target = DbCtx::from_app(app, &reg).context("install and start MariaDB first")?;
    if target.engine != DbEngine::MariaDb {
        bail!("the active database is not MariaDB; switch to MariaDB first");
    }
    if !process::port_accepting(target.port) {
        bail!("start MariaDB first: it receives the databases");
    }
    let pw = crate::db::root_password(layout)?.context("MariaDB root password unknown")?;
    let target_version = target
        .sql("SELECT VERSION();", Some(&pw))?
        .trim()
        .to_string();
    if !target_version.to_ascii_lowercase().contains("mariadb") {
        bail!("the server on the database port is not MariaDB ({target_version})");
    }
    let existing = crate::db::list_databases(&target)?;
    let pkg = reg
        .list(Kind::MySql)
        .into_iter()
        .find(|i| DbEngine::MySql.is_mysql56(&i.version))
        .cloned()
        .context("MySQL 5.6.51 is not installed; install it from the Versions tab first")?;
    let owner = claim(layout)?;
    let work = Work::create(layout, owner)?;
    if work.socket().as_os_str().len() > 100 {
        bail!("the Veranda folder path is too long for the server socket");
    }

    let mut report = Report {
        target_version,
        ..Default::default()
    };
    say("Backing up MariaDB completely…".into(), 0);
    report.backup = backup_target(layout, &target)?.display().to_string();
    if cancelled() {
        bail!("cancelled");
    }

    say("Copying the data folder…".into(), 10);
    let total = insp.bytes.max(1);
    let mut last = Instant::now();
    let mut tick = |copied: u64| {
        if last.elapsed() >= Duration::from_millis(300) {
            last = Instant::now();
            progress(Progress {
                message: "Copying the data folder…".into(),
                bytes: copied,
                percent: (10 + copied.min(total) * 25 / total) as u8,
                ..Default::default()
            });
        }
    };
    copy_tree(&real, &work.data(), &mut 0, &mut tick)?;

    say("Starting MySQL 5.6 on the copy…".into(), 35);
    start_server(layout, &pkg, &work)?;
    wait_ready(layout, &target, &work.socket())?;
    report.source_version = query(&target, &work.socket(), "SELECT VERSION()")?
        .trim()
        .to_string();
    if !report.source_version.starts_with("5.6.") {
        bail!(
            "the temporary server reports version {}, not 5.6",
            report.source_version
        );
    }

    if let Ok(out) = query(
        &target,
        &work.socket(),
        "SELECT User, Host FROM mysql.user ORDER BY User, Host",
    ) {
        report.accounts = out
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(u, h)| format!("'{u}'@'{h}'"))
            .collect();
    }
    let names: Vec<String> = query(&target, &work.socket(), "SHOW DATABASES")?
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|n| !n.is_empty() && !SYSTEM_DBS.contains(&n.as_str()))
        .collect();
    if names.is_empty() {
        report
            .skipped
            .push("no user databases in this folder".into());
    }
    let count = names.len();
    for (i, name) in names.into_iter().enumerate() {
        if cancelled() {
            report
                .skipped
                .push(format!("cancelled: {} databases not copied", count - i));
            break;
        }
        progress(Progress {
            message: format!("Copying database {name} ({} of {count})…", i + 1),
            done: i,
            total: count,
            current: name.clone(),
            bytes: 0,
            percent: (35 + i * 64 / count) as u8,
        });
        if existing.iter().any(|e| e.eq_ignore_ascii_case(&name)) {
            report
                .skipped
                .push(format!("{name}: already exists in MariaDB"));
            continue;
        }
        if name.starts_with('-') || !crate::db::is_valid_db_name(&name) {
            report.skipped.push(format!("{name}: unsupported name"));
            continue;
        }
        let r = dump_db(&target, &work.socket(), &name, &work.dump())
            .and_then(|()| crate::db::import_file(&target, &work.dump()));
        let _ = std::fs::remove_file(work.dump());
        match r.and_then(|()| verify(&target, &pw, &work.socket(), &name)) {
            Ok(c) => {
                report.notes.extend(c.notes);
                if c.differences.is_empty() {
                    report.moved.push(name);
                } else {
                    report
                        .mismatched
                        .push(format!("{name}: {}", c.differences.join("; ")));
                }
            }
            Err(e) => {
                // a half-loaded database would be skipped as "already exists" next time
                let _ = crate::db::drop_database(&target, &name);
                report
                    .failed
                    .push(format!("{name}: {}", short(&format!("{e:#}"))));
            }
        }
    }
    say("Stopping MySQL 5.6 and deleting the copy…".into(), 99);
    drop(work);
    tracing::info!(
        "MySQL 5.6 import: {} moved, {} skipped, {} failed, {} with differences",
        report.moved.len(),
        report.skipped.len(),
        report.failed.len(),
        report.mismatched.len()
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (tempfile::TempDir, Veranda) {
        let root = tempfile::tempdir().unwrap();
        let layout = crate::Layout::new(root.path());
        layout.ensure().unwrap();
        (
            root,
            Veranda {
                layout,
                config: crate::Config::default(),
            },
        )
    }

    #[test]
    fn recognises_only_mysql56_folders() {
        let (_root, app) = app();
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        assert!(check_source(&app, p).is_err());
        std::fs::create_dir_all(p.join("mysql")).unwrap();
        for f in ["mysql/user.frm", "mysql/innodb_table_stats.frm", "ibdata1"] {
            std::fs::write(p.join(f), "x").unwrap();
        }
        assert!(check_source(&app, p).is_ok());
        std::fs::write(p.join("mysql_upgrade_info"), "5.7.44").unwrap();
        assert!(check_source(&app, p).is_err());
        std::fs::write(p.join("mysql_upgrade_info"), "5.6.51").unwrap();
        assert!(check_source(&app, p).is_ok());
        std::fs::write(p.join("mysql.ibd"), "x").unwrap();
        assert!(check_source(&app, p).is_err());
    }

    #[test]
    fn charsets_compare_across_servers() {
        assert_eq!(
            charset_of("utf8_general_ci"),
            charset_of("utf8mb3_general_ci")
        );
        assert_ne!(
            charset_of("utf8_general_ci"),
            charset_of("utf8mb4_general_ci")
        );
        assert_eq!(charset_of("latin1_swedish_ci"), "latin1");
        assert_eq!(quote_ident("a`b"), "`a``b`");
    }

    #[test]
    fn short_messages_hide_sql() {
        assert_eq!(
            short("ERROR 1064 (42000) at line 9: syntax error near 'secret data' at line 1\nmore"),
            "ERROR 1064 (42000) at line 9: syntax error"
        );
    }
}
