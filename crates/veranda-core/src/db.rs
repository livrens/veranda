//! Database operations: initialisation, root password, SQL helpers, dumps.
//! Passwords are passed through the `MYSQL_PWD` environment variable, never on the
//! command line (visible in process lists), and stored in `etc/secrets/db-root.txt` (0600).

use crate::Veranda;
use crate::config::DbEngine;
use crate::fsutil;
use crate::packages::{Binaries, Installed, Registry};
use crate::paths::Layout;
use crate::process;
use anyhow::{Context, Result, bail};
use rand::RngExt;
use rand::distr::Alphanumeric;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub fn random_token(len: usize) -> String {
    let mut rng = rand::rng();
    (&mut rng)
        .sample_iter(Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

#[derive(Debug, Clone)]
pub struct DbCtx {
    pub engine: DbEngine,
    pub port: u16,
    pub pkg: Installed,
    pub data_dir: PathBuf,
    pub layout: Layout,
}

impl DbCtx {
    pub fn from_app(app: &Veranda, reg: &Registry) -> Option<Self> {
        let engine = app.config.db.engine;
        let kind = crate::services::db::kind(engine);
        let pkg = reg.active(kind, &app.config.db.version)?.clone();
        Some(Self {
            engine,
            port: app.config.db.port,
            pkg,
            data_dir: app.config.db_data_dir(&app.layout),
            layout: app.layout.clone(),
        })
    }

    pub fn is_initialized(&self) -> bool {
        self.data_dir.join("mysql").is_dir()
    }

    pub fn server_binary(&self) -> PathBuf {
        Binaries::db_server(&self.pkg, self.engine)
    }

    /// Arguments every client tool gets first: ignore the user's own ~/.my.cnf (which may carry a
    /// password for another server) and skip TLS on loopback (MariaDB 11.4+ clients require it
    /// by default and the local server has no certificate).
    pub fn client_base_args(&self) -> Vec<String> {
        let conf = crate::services::db::conf_path(&self.layout, self.engine);
        let mut v = vec![format!("--defaults-file={}", conf.display())];
        match self.engine {
            DbEngine::MariaDb => v.push("--skip-ssl".into()),
            // caching_sha2_password needs a secure channel for the first login after a server
            // start; without TLS that is the RSA key exchange, which the client only does when
            // asked. Otherwise every cold connection fails with ERROR 2061.
            DbEngine::MySql => {
                v.push("--ssl-mode=DISABLED".into());
                v.push("--get-server-public-key".into());
            }
        }
        v
    }

    fn client(&self, password: Option<&str>) -> Command {
        let mut cmd = crate::process::new_cmd(Binaries::db_client(&self.pkg, self.engine));
        cmd.args(self.client_base_args());
        cmd.args([
            "--protocol=tcp",
            "-h",
            "127.0.0.1",
            "-P",
            &self.port.to_string(),
            "-u",
            "root",
            "--batch",
            "--skip-column-names",
        ]);
        cmd.env_remove("MYSQL_PWD");
        if let Some(pw) = password {
            cmd.env("MYSQL_PWD", pw);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Run SQL statements through the client; returns stdout.
    pub fn sql(&self, sql: &str, password: Option<&str>) -> Result<String> {
        let mut child = self
            .client(password)
            .spawn()
            .context("starting the database client")?;
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().context("client stdin")?;
            stdin.write_all(sql.as_bytes())?;
            stdin.write_all(b"\n")?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!(
                "SQL failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

pub fn root_password_file(layout: &Layout) -> PathBuf {
    layout.secrets().join("db-root.txt")
}

/// `None` until the database was set up once; `Some("")` when root deliberately has no
/// password (Laragon/XAMPP style, see [`set_root_password`]).
pub fn root_password(layout: &Layout) -> Result<Option<String>> {
    match std::fs::read_to_string(root_password_file(layout)) {
        Ok(s) => Ok(Some(s.trim().to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Generate and persist a random root password if none exists yet.
pub fn ensure_root_password_file(layout: &Layout) -> Result<String> {
    if let Some(pw) = root_password(layout)? {
        return Ok(pw);
    }
    let pw = random_token(24);
    fsutil::write_private(&root_password_file(layout), pw.as_bytes())?;
    Ok(pw)
}

/// Create the data directory (server must be stopped).
pub fn initialize(db: &DbCtx) -> Result<()> {
    std::fs::create_dir_all(&db.data_dir)?;
    fsutil::make_private_dir(&db.data_dir)?;
    match db.engine {
        DbEngine::MariaDb => {
            let tool = Binaries::mariadb_install_db(&db.pkg)
                .context("mariadb-install-db not found in the package")?;
            let mut cmd = crate::process::new_cmd(&tool);
            cmd.arg(format!("--datadir={}", db.data_dir.display()));
            if cfg!(windows) {
                // The Windows tool also creates my.ini and sets the password directly.
                let pw = ensure_root_password_file(&db.layout)?;
                cmd.arg(format!("--password={pw}"));
            } else {
                cmd.arg(format!("--basedir={}", db.pkg.path.display()))
                    .arg("--auth-root-authentication-method=normal")
                    .arg("--skip-test-db")
                    .arg("--skip-name-resolve");
            }
            cmd.current_dir(&db.pkg.path);
            process::run_capture(cmd).context("mariadb-install-db")?;
        }
        DbEngine::MySql => {
            let mut cmd = crate::process::new_cmd(db.server_binary());
            cmd.arg("--initialize-insecure")
                .arg(format!("--basedir={}", db.pkg.path.display()))
                .arg(format!("--datadir={}", db.data_dir.display()))
                .arg(format!(
                    "--log-error={}",
                    db.layout.logs().join("mysql-init.log").display()
                ))
                .current_dir(&db.pkg.path);
            process::run_capture(cmd).context("mysqld --initialize-insecure")?;
        }
    }
    Ok(())
}

/// Change root's password (both localhost and 127.0.0.1 identities). An empty `new` removes
/// the password: the server only listens on 127.0.0.1, so this matches what projects written
/// for Laragon or XAMPP expect (`root` with no password) while staying local-only.
pub fn set_root_password(db: &DbCtx, current: Option<&str>, new: &str) -> Result<()> {
    if !new.is_empty()
        && (new.len() < 8
            || !new
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.!@#%^*+=".contains(c)))
    {
        bail!(
            "password must be at least 8 characters and only contain letters, digits and -_.!@#%^*+="
        );
    }
    let q = new.replace('\\', "\\\\").replace('\'', "\\'");
    let sql = match db.engine {
        DbEngine::MariaDb => format!(
            "ALTER USER 'root'@'localhost' IDENTIFIED BY '{q}';\nCREATE USER IF NOT EXISTS 'root'@'127.0.0.1' IDENTIFIED BY '{q}';\nGRANT ALL PRIVILEGES ON *.* TO 'root'@'127.0.0.1' WITH GRANT OPTION;\nALTER USER 'root'@'127.0.0.1' IDENTIFIED BY '{q}';\nFLUSH PRIVILEGES;"
        ),
        DbEngine::MySql => format!(
            "ALTER USER 'root'@'localhost' IDENTIFIED BY '{q}';\nCREATE USER IF NOT EXISTS 'root'@'127.0.0.1' IDENTIFIED BY '{q}';\nGRANT ALL PRIVILEGES ON *.* TO 'root'@'127.0.0.1' WITH GRANT OPTION;\nALTER USER 'root'@'127.0.0.1' IDENTIFIED BY '{q}';\nFLUSH PRIVILEGES;"
        ),
    };
    let attempt = db.sql(&sql, current);
    let result = match attempt {
        Ok(_) => Ok(()),
        // Windows mariadb-install-db already set the password from the file.
        Err(_) if current.is_none() => db
            .sql("SELECT 1", root_password(&db.layout)?.as_deref())
            .map(|_| ()),
        Err(e) => Err(e),
    };
    result?;
    fsutil::write_private(&root_password_file(&db.layout), new.as_bytes())?;
    Ok(())
}

pub fn is_valid_db_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn create_database(db: &DbCtx, name: &str) -> Result<()> {
    if !is_valid_db_name(name) {
        bail!("database name may only contain letters, digits, _ and - (max 64)");
    }
    let pw = root_password(&db.layout)?
        .context("root password unknown; run `veranda db reset-password`")?;
    db.sql(&format!("CREATE DATABASE IF NOT EXISTS `{name}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;"), Some(&pw))?;
    Ok(())
}

pub fn drop_database(db: &DbCtx, name: &str) -> Result<()> {
    if !is_valid_db_name(name)
        || ["mysql", "sys", "information_schema", "performance_schema"].contains(&name)
    {
        bail!("refusing to drop `{name}`");
    }
    let pw = root_password(&db.layout)?.context("root password unknown")?;
    db.sql(&format!("DROP DATABASE IF EXISTS `{name}`;"), Some(&pw))?;
    Ok(())
}

pub fn list_databases(db: &DbCtx) -> Result<Vec<String>> {
    let pw = root_password(&db.layout)?.context("root password unknown")?;
    let out = db.sql("SHOW DATABASES;", Some(&pw))?;
    Ok(out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Databases that belong to the server itself and must never be dumped into a bundle.
pub const SYSTEM_DBS: &[&str] = &["information_schema", "performance_schema", "mysql", "sys"];

/// Dump the user databases (never the system ones) into one SQL file with `CREATE DATABASE`
/// and `USE` statements, so it can be loaded into an empty server. Returns the names.
pub fn dump_user_databases(db: &DbCtx, dest: &std::path::Path) -> Result<Vec<String>> {
    let pw = root_password(&db.layout)?.context("root password unknown")?;
    let names: Vec<String> = list_databases(db)?
        .into_iter()
        .filter(|n| !SYSTEM_DBS.contains(&n.as_str()))
        .collect();
    if names.is_empty() {
        std::fs::write(dest, "-- no user databases\n")?;
        return Ok(names);
    }
    let file = std::fs::File::create(dest)?;
    let mut cmd = crate::process::new_cmd(Binaries::db_dump(&db.pkg, db.engine));
    cmd.args(db.client_base_args());
    cmd.args([
        "--protocol=tcp",
        "-h",
        "127.0.0.1",
        "-P",
        &db.port.to_string(),
        "-u",
        "root",
        "--single-transaction",
        "--skip-lock-tables",
        "--routines",
        "--events",
        "--triggers",
        "--databases",
    ])
    .args(&names)
    .env("MYSQL_PWD", pw)
    .stdin(Stdio::null())
    .stdout(Stdio::from(file))
    .stderr(Stdio::piped());
    let out = cmd.output()?;
    if !out.status.success() {
        let _ = std::fs::remove_file(dest);
        bail!(
            "dump failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    fsutil::make_private_file(dest)?;
    Ok(names)
}

/// Load a plain SQL file (as written by [`dump_user_databases`] or [`dump_all`]) into the
/// running server as root.
pub fn import_file(db: &DbCtx, file: &std::path::Path) -> Result<()> {
    let pw = root_password(&db.layout)?.context("root password unknown")?;
    let input = std::fs::File::open(file).with_context(|| format!("opening {}", file.display()))?;
    let gzip = file.extension().is_some_and(|x| x == "gz");
    let mut cmd = crate::process::new_cmd(Binaries::db_client(&db.pkg, db.engine));
    cmd.args(db.client_base_args());
    cmd.args([
        "--protocol=tcp",
        "-h",
        "127.0.0.1",
        "-P",
        &db.port.to_string(),
        "-u",
        "root",
    ])
    .env("MYSQL_PWD", pw)
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("running the database client")?;
    let mut stdin = child.stdin.take().context("client stdin")?;
    let fed: std::io::Result<u64> = if gzip {
        let mut dec = flate2::read::GzDecoder::new(std::io::BufReader::new(input));
        std::io::copy(&mut dec, &mut stdin)
    } else {
        let mut r = std::io::BufReader::new(input);
        std::io::copy(&mut r, &mut stdin)
    };
    drop(stdin);
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "import failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    fed.context("reading the dump")?;
    Ok(())
}

/// Dump every database into `dest` (plain SQL).
pub fn dump_all(db: &DbCtx, dest: &std::path::Path) -> Result<()> {
    let pw = root_password(&db.layout)?.context("root password unknown")?;
    let mut cmd = crate::process::new_cmd(Binaries::db_dump(&db.pkg, db.engine));
    cmd.args(db.client_base_args());
    cmd.args([
        "--protocol=tcp",
        "-h",
        "127.0.0.1",
        "-P",
        &db.port.to_string(),
        "-u",
        "root",
    ])
    .args([
        "--all-databases",
        "--single-transaction",
        "--routines",
        "--events",
        "--triggers",
    ])
    .env("MYSQL_PWD", pw);
    run_dump_to(cmd, dest)
}

/// Run a dump command, writing its output to `dest`; a `.gz` destination is gzip-compressed
/// on the fly (SQL dumps shrink 5-10x).
fn run_dump_to(mut cmd: Command, dest: &std::path::Path) -> Result<()> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("starting the dump tool")?;
    let mut pipe = child.stdout.take().context("dump pipe")?;
    let file = std::fs::File::create(dest)?;
    let gzip = dest.extension().is_some_and(|x| x == "gz");
    let copied: Result<()> = (|| {
        if gzip {
            let mut enc = flate2::write::GzEncoder::new(
                std::io::BufWriter::new(file),
                flate2::Compression::default(),
            );
            std::io::copy(&mut pipe, &mut enc)?;
            enc.finish()?
                .into_inner()
                .map_err(|e| e.into_error())?
                .sync_all()?;
        } else {
            let mut w = std::io::BufWriter::new(file);
            std::io::copy(&mut pipe, &mut w)?;
            w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        }
        Ok(())
    })();
    let out = child.wait_with_output()?;
    if !out.status.success() || copied.is_err() {
        let _ = std::fs::remove_file(dest);
        if let Err(e) = copied {
            bail!("writing the dump: {e}");
        }
        bail!(
            "dump failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    fsutil::make_private_file(dest)?;
    Ok(())
}

pub fn shutdown(db: &DbCtx, password: &str) -> Result<()> {
    let mut cmd = crate::process::new_cmd(Binaries::db_admin(&db.pkg, db.engine));
    cmd.args(db.client_base_args());
    cmd.args([
        "--protocol=tcp",
        "-h",
        "127.0.0.1",
        "-P",
        &db.port.to_string(),
        "-u",
        "root",
        "shutdown",
    ])
    .env("MYSQL_PWD", password);
    process::run_capture(cmd)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_names() {
        let t = random_token(24);
        assert_eq!(t.len(), 24);
        assert!(t.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(is_valid_db_name("my_app-1"));
        assert!(!is_valid_db_name("drop table"));
    }
}
