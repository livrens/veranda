//! One-click import from other local stacks (Laragon, XAMPP, FlyEnv, or any folder of projects).
//!
//! Sites: every project folder becomes a Veranda site, either linked in place (manual site that
//! points at the existing folder) or copied into the document root (auto site).
//! Databases: optional; the source server must be running. Each user database is streamed with
//! the installed `mysqldump` into Veranda's own server, so no data directory is ever touched.

use crate::Veranda;
use crate::config::DbEngine;
use crate::db::DbCtx;
use crate::packages::{Binaries, Registry};
use crate::{fsutil, sites};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct Source {
    pub platform: String,
    pub label: String,
    /// Projects folder (Laragon `www`, XAMPP `htdocs`) or FlyEnv's `host.json`.
    pub path: Option<String>,
    pub found: bool,
    /// The application is installed here but holds no sites yet (FlyEnv without hosts).
    pub installed_at: Option<String>,
    pub db_port: u16,
    pub db_user: String,
    pub db_password: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    pub platform: String,
    pub path: String,
    /// `link` (keep folders where they are) or `copy` (into the document root).
    pub mode: String,
    pub databases: bool,
    #[serde(default)]
    pub db_host: String,
    #[serde(default)]
    pub db_port: u16,
    #[serde(default)]
    pub db_user: String,
    #[serde(default)]
    pub db_password: String,
}

/// What the UI shows while an import runs. `percent` is weighted by database size
/// (information_schema data+index length) and moves with the bytes streamed, so a large
/// database does not sit at the same number for minutes.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub message: String,
    /// Databases finished / total (0/0 while sites are imported).
    pub done: usize,
    pub total: usize,
    pub current: String,
    /// Bytes streamed for the current database so far.
    pub bytes: u64,
    pub percent: u8,
}

impl Progress {
    pub fn msg(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Default::default()
        }
    }
}

/// Pause / cancel switches the UI flips while an import runs. `cancel` 1 = stop after the
/// database being copied (nothing half done), 2 = stop now (that database is reported as
/// incomplete and copied again next time). `paused` holds the stream between dump and client.
#[derive(Debug, Default)]
pub struct Control {
    pub cancel: std::sync::atomic::AtomicU8,
    pub paused: std::sync::atomic::AtomicBool,
}

impl Control {
    pub fn cancel_level(&self) -> u8 {
        self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub sites: Vec<String>,
    pub databases: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

/// Folders XAMPP ships inside `htdocs` that are not user projects.
const XAMPP_BUILTIN: &[&str] = &["dashboard", "img", "webalizer", "xampp", "restricted"];
/// Server-side schemas that must never be copied between servers.
const SYSTEM_DBS: &[&str] = &["information_schema", "performance_schema", "mysql", "sys"];

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn first_existing(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.exists()).cloned()
}

/// Known stacks with their default locations on this OS.
pub fn detect() -> Vec<Source> {
    let h = home();
    // Laragon (Windows only) and XAMPP: every drive letter plus the usual folders.
    let mut laragon: Vec<PathBuf> = Vec::new();
    let mut xampp: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for letter in b'C'..=b'Z' {
            let d = format!("{}:\\", letter as char);
            laragon.push(PathBuf::from(format!("{d}laragon\\www")));
            xampp.push(PathBuf::from(format!("{d}xampp\\htdocs")));
        }
        for base in [
            "C:\\Program Files",
            "C:\\Program Files (x86)",
            "C:\\ProgramData",
        ] {
            laragon.push(PathBuf::from(format!("{base}\\laragon\\www")));
            xampp.push(PathBuf::from(format!("{base}\\xampp\\htdocs")));
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            laragon.push(PathBuf::from(&pf).join("laragon").join("www"));
            xampp.push(PathBuf::from(&pf).join("xampp").join("htdocs"));
        }
    }
    laragon.push(h.join("laragon").join("www"));
    laragon.push(h.join("Laragon").join("www"));
    xampp.push(PathBuf::from("/opt/lampp/htdocs"));
    xampp.push(h.join("lampp").join("htdocs"));
    xampp.push(h.join("xampp").join("htdocs"));
    xampp.push(PathBuf::from("/Applications/XAMPP/xamppfiles/htdocs"));
    xampp.push(PathBuf::from("/Applications/XAMPP/htdocs"));
    let flyenv_dirs = flyenv_config_dirs(&h);
    let flyenv_installed = flyenv_dirs.iter().find(|d| d.is_dir()).cloned();
    let flyenv = flyenv_installed
        .as_ref()
        .and_then(|d| flyenv_site_source(d));
    let mk = |platform: &str, label: &str, found: Option<PathBuf>, user: &str, pw: &str| Source {
        platform: platform.into(),
        label: label.into(),
        found: found.is_some(),
        path: found.map(|p| p.display().to_string()),
        installed_at: None,
        db_port: 3306,
        db_user: user.into(),
        db_password: pw.into(),
    };
    let mut fly = mk("flyenv", "FlyEnv", flyenv, "root", "root");
    fly.installed_at = flyenv_installed.map(|p| p.display().to_string());
    vec![
        mk("laragon", "Laragon", first_existing(&laragon), "root", ""),
        mk("xampp", "XAMPP", first_existing(&xampp), "root", ""),
        fly,
        mk("folder", "Other folder of projects", None, "root", ""),
    ]
}

/// Where FlyEnv (formerly PhpWebStudy) keeps its data on each platform.
fn flyenv_config_dirs(h: &Path) -> Vec<PathBuf> {
    let mut v = vec![
        h.join(".config").join("FlyEnv"),
        h.join("Library").join("FlyEnv"),
        h.join("Library").join("PhpWebStudy"),
        PathBuf::from("C:\\FlyEnv-Data"),
        PathBuf::from("C:\\Program Files\\FlyEnv-Data"),
    ];
    if let Some(d) = dirs::config_dir() {
        v.push(d.join("FlyEnv"));
    }
    v
}

/// The site list inside a FlyEnv folder: `host.json` when it exists, else the vhost folder when
/// it holds at least one `.conf`. `None` = FlyEnv has no sites yet.
fn flyenv_site_source(dir: &Path) -> Option<PathBuf> {
    for p in [dir.join("server").join("host.json"), dir.join("host.json")] {
        if p.is_file() {
            return Some(p);
        }
    }
    let vhost = dir.join("server").join("vhost");
    let has_conf = |d: &Path| {
        std::fs::read_dir(d)
            .map(|rd| {
                rd.flatten()
                    .any(|e| e.path().extension().is_some_and(|x| x == "conf"))
            })
            .unwrap_or(false)
    };
    if has_conf(&vhost) || has_conf(&vhost.join("apache")) || has_conf(&vhost.join("nginx")) {
        return Some(vhost);
    }
    None
}

/// A project found in the source.
#[derive(Debug, Clone)]
struct Project {
    name: String,
    path: PathBuf,
    /// Hostname the source used, when it already fits Veranda's TLD.
    hostname: Option<String>,
}

fn projects_in_folder(dir: &Path, platform: &str) -> Result<Vec<Project>> {
    let mut out = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let Some(raw) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if raw.starts_with('.') || raw.starts_with('_') {
            continue;
        }
        if platform == "xampp" && XAMPP_BUILTIN.contains(&raw) {
            continue;
        }
        out.push(Project {
            name: raw.to_string(),
            path: p,
            hostname: None,
        });
    }
    Ok(out)
}

/// FlyEnv keeps its sites in `host.json`: `[{ "name": "demo.test", "root": "/path", ... }]`.
/// Accepts that file, the FlyEnv data folder, or its `server/vhost` folder (Apache/Nginx confs).
fn projects_in_flyenv(path: &Path) -> Result<Vec<Project>> {
    if path.is_dir() {
        let Some(src) = flyenv_site_source(path).or_else(|| {
            // a vhost folder itself
            path.file_name()
                .is_some_and(|n| n == "vhost")
                .then(|| path.to_path_buf())
        }) else {
            bail!(
                "FlyEnv at {} has no sites yet (no host.json and no vhost files); add hosts in FlyEnv first, or choose \"Other folder of projects\"",
                path.display()
            );
        };
        if src.is_dir() {
            return projects_in_vhost_confs(&src);
        }
        return projects_in_flyenv(&src);
    }
    let host_json = path;
    let text = std::fs::read_to_string(host_json)
        .with_context(|| format!("reading {}", host_json.display()))?;
    let hosts: Vec<serde_json::Value> = serde_json::from_str(text.trim()).context("host.json")?;
    let mut out = Vec::new();
    for h in hosts {
        let name = h.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
        let root = h.get("root").and_then(|v| v.as_str()).unwrap_or("").trim();
        if name.is_empty() || root.is_empty() {
            continue;
        }
        let path = PathBuf::from(root);
        // FlyEnv roots may point at `project/public`; the project itself is the parent then.
        let (path, name_hint) = if path.file_name().is_some_and(|f| f == "public")
            && path.parent().is_some_and(|p| p.is_dir())
        {
            (path.parent().unwrap().to_path_buf(), name)
        } else {
            (path, name)
        };
        out.push(Project {
            name: name_hint.split('.').next().unwrap_or(name_hint).to_string(),
            path,
            hostname: Some(name.to_ascii_lowercase()),
        });
    }
    Ok(out)
}

/// Sites from Apache (`ServerName` + `DocumentRoot`) or Nginx (`server_name` + `root`) vhost
/// files, one site per file, searched one level deep (`vhost/apache/*.conf`, `vhost/nginx/*.conf`).
fn projects_in_vhost_confs(dir: &Path) -> Result<Vec<Project>> {
    let mut files = Vec::new();
    for d in [dir.to_path_buf(), dir.join("apache"), dir.join("nginx")] {
        if let Ok(rd) = std::fs::read_dir(&d) {
            files.extend(
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "conf")),
            );
        }
    }
    files.sort();
    let mut out: Vec<Project> = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let mut name = None;
        let mut root = None;
        for line in text.lines() {
            let l = line.trim().trim_end_matches(';');
            let mut parts = l.split_whitespace();
            match parts.next() {
                Some("ServerName") | Some("server_name") if name.is_none() => {
                    name = parts
                        .next()
                        .map(|n| n.trim_matches('"').to_ascii_lowercase());
                }
                Some("DocumentRoot") | Some("root") if root.is_none() => {
                    root = parts.next().map(|r| r.trim_matches('"').to_string());
                }
                _ => {}
            }
        }
        let (Some(name), Some(root)) = (name, root) else {
            continue;
        };
        if name.is_empty()
            || root.is_empty()
            || out.iter().any(|p| p.hostname.as_deref() == Some(&name))
        {
            continue;
        }
        let path = PathBuf::from(&root);
        let path = if path.file_name().is_some_and(|f| f == "public")
            && path.parent().is_some_and(|p| p.is_dir())
        {
            path.parent().unwrap().to_path_buf()
        } else {
            path
        };
        out.push(Project {
            name: name.split('.').next().unwrap_or(&name).to_string(),
            path,
            hostname: Some(name),
        });
    }
    Ok(out)
}

fn copy_dir(src: &Path, dst: &Path, progress: &dyn Fn(Progress), count: &mut usize) -> Result<()> {
    let say = |m: String| progress(Progress::msg(m));
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let ty = e.file_type()?;
        let to = dst.join(e.file_name());
        if ty.is_symlink() {
            continue; // links are not followed: they usually point outside the project
        }
        if ty.is_dir() {
            copy_dir(&e.path(), &to, progress, count)?;
        } else {
            std::fs::copy(e.path(), &to)?;
            *count += 1;
            if (*count).is_multiple_of(250) {
                say(format!("{} files copied…", count));
            }
        }
    }
    Ok(())
}

fn import_sites(
    app: &Veranda,
    req: &Request,
    report: &mut Report,
    progress: &dyn Fn(Progress),
) -> Result<()> {
    let say = |m: String| progress(Progress::msg(m));
    let src = PathBuf::from(req.path.trim());
    let projects = if req.platform == "flyenv" {
        projects_in_flyenv(&src)?
    } else {
        projects_in_folder(&src, &req.platform)?
    };
    if projects.is_empty() {
        bail!("no project folders found in {}", src.display());
    }
    let cfg = &app.config;
    let www = cfg.document_root(&app.layout);
    for p in projects {
        if !p.path.is_dir() {
            report.skipped.push(format!(
                "{}: folder not found ({})",
                p.name,
                p.path.display()
            ));
            continue;
        }
        let slug = fsutil::slugify(&p.name);
        if slug.is_empty() {
            report.skipped.push(format!("{}: unusable name", p.name));
            continue;
        }
        let hostname = match &p.hostname {
            Some(h) if crate::hosts::is_valid_hostname(h, &cfg.general.tld) => h.clone(),
            _ => cfg.hostname_for(&slug),
        };
        if req.mode == "copy" {
            let dest = www.join(&slug);
            if dest.exists() {
                report
                    .skipped
                    .push(format!("{}: already exists in {}", slug, www.display()));
                continue;
            }
            if p.path.starts_with(&www) {
                report
                    .skipped
                    .push(format!("{}: already inside the document root", slug));
                continue;
            }
            say(format!("Copying {}…", p.name));
            let mut n = 0usize;
            match copy_dir(&p.path, &dest, progress, &mut n) {
                Ok(()) => report.sites.push(format!("{hostname} ({n} files)")),
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&dest);
                    report.errors.push(format!("{}: {e:#}", p.name));
                }
            }
        } else {
            say(format!("Linking {}…", p.name));
            match sites::add_manual(&app.layout, cfg, &hostname, &p.path, "") {
                Ok(()) => report.sites.push(hostname),
                Err(e) => report.errors.push(format!("{}: {e:#}", p.name)),
            }
        }
    }
    Ok(())
}

fn source_client_args(engine: DbEngine, req: &Request) -> Vec<String> {
    let mut v = vec!["--no-defaults".to_string()];
    match engine {
        DbEngine::MariaDb => v.push("--skip-ssl".into()),
        DbEngine::MySql => {
            v.push("--ssl-mode=DISABLED".into());
            v.push("--get-server-public-key".into());
        }
    }
    let host = if req.db_host.trim().is_empty() {
        "127.0.0.1"
    } else {
        req.db_host.trim()
    };
    let port = if req.db_port == 0 { 3306 } else { req.db_port };
    let user = if req.db_user.trim().is_empty() {
        "root"
    } else {
        req.db_user.trim()
    };
    v.extend([
        "--protocol=tcp".into(),
        "-h".into(),
        host.into(),
        "-P".into(),
        port.to_string(),
        "-u".into(),
        user.into(),
    ]);
    v
}

fn list_source_databases(db: &DbCtx, req: &Request) -> Result<Vec<String>> {
    let mut cmd = crate::process::new_cmd(Binaries::db_client(&db.pkg, db.engine));
    cmd.args(source_client_args(db.engine, req))
        .args(["-N", "-B", "-e", "SHOW DATABASES"])
        .env("MYSQL_PWD", &req.db_password)
        .stdin(Stdio::null());
    let out = cmd.output().context("running the database client")?;
    if !out.status.success() {
        bail!(
            "cannot connect to the source database: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !SYSTEM_DBS.contains(&l.as_str()))
        .collect())
}

/// Approximate size of each database on the source (data + index length), for the progress bar.
fn source_db_sizes(
    db: &DbCtx,
    req: &Request,
    names: &[String],
) -> std::collections::HashMap<String, u64> {
    let mut out = std::collections::HashMap::new();
    if names.is_empty() {
        return out;
    }
    let list = names
        .iter()
        .map(|n| format!("'{}'", n.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let mut cmd = crate::process::new_cmd(Binaries::db_client(&db.pkg, db.engine));
    cmd.args(source_client_args(db.engine, req))
        .args(["-N", "-B", "-e", &format!("SELECT table_schema, COALESCE(SUM(data_length + index_length), 0) FROM information_schema.tables WHERE table_schema IN ({list}) GROUP BY table_schema")])
        .env("MYSQL_PWD", &req.db_password)
        .stdin(Stdio::null());
    if let Ok(o) = cmd.output()
        && o.status.success()
    {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let mut w = line.split('\t');
            if let (Some(name), Some(size)) = (w.next(), w.next())
                && let Ok(size) = size.trim().parse::<u64>()
            {
                out.insert(name.to_string(), size);
            }
        }
    }
    out
}

/// Progress reporter for the database loop: percent weighted by size, bytes of the current
/// database counted as they stream.
struct DbProgress<'a> {
    progress: &'a dyn Fn(Progress),
    sizes: std::collections::HashMap<String, u64>,
    total_bytes: u64,
    done_bytes: u64,
    done: usize,
    total: usize,
}

impl<'a> DbProgress<'a> {
    fn new(
        progress: &'a dyn Fn(Progress),
        names: &[String],
        sizes: std::collections::HashMap<String, u64>,
    ) -> Self {
        // unknown sizes count as 1 MB so every database still moves the bar
        let total_bytes = names
            .iter()
            .map(|n| sizes.get(n).copied().unwrap_or(1 << 20).max(1))
            .sum();
        Self {
            progress,
            sizes,
            total_bytes,
            done_bytes: 0,
            done: 0,
            total: names.len(),
        }
    }
    fn size_of(&self, name: &str) -> u64 {
        self.sizes.get(name).copied().unwrap_or(1 << 20).max(1)
    }
    fn report(&self, name: &str, bytes: u64) {
        let cur = bytes.min(self.size_of(name));
        let pct = ((self.done_bytes + cur) * 100)
            .checked_div(self.total_bytes)
            .map_or(0, |p| p.min(99) as u8);
        (self.progress)(Progress {
            message: format!(
                "Copying database {name} ({} of {})…",
                self.done + 1,
                self.total
            ),
            done: self.done,
            total: self.total,
            current: name.into(),
            bytes,
            percent: pct,
        });
    }
    fn finish(&mut self, name: &str) {
        self.done_bytes += self.size_of(name);
        self.done += 1;
    }
}

/// `mariadb-dump` (MariaDB 10.6.18+/11.x/13) starts its output with
/// `/*!999999\- enable the sandbox mode */`. MySQL's `mysql` client does not know the `\-`
/// command and stops with "Unknown command '\-'" before the first real statement, so the
/// line is dropped when it is the first thing in the stream. Harmless for MariaDB targets.
fn strip_sandbox_header(chunk: &[u8]) -> &[u8] {
    const MARK: &[u8] = b"/*!999999";
    if chunk.starts_with(MARK) {
        match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => &chunk[i + 1..],
            None => &[],
        }
    } else {
        chunk
    }
}

/// Collations MariaDB 11.5+/13 uses by default that MySQL does not know. Only the `COLLATE`
/// clause is rewritten (table and database definitions), never free text in the data.
const MARIADB_COLLATIONS: &[(&[u8], &[u8])] = &[
    (
        b"COLLATE=utf8mb4_uca1400_ai_ci",
        b"COLLATE=utf8mb4_0900_ai_ci",
    ),
    (
        b"COLLATE utf8mb4_uca1400_ai_ci",
        b"COLLATE utf8mb4_0900_ai_ci",
    ),
    (
        b"COLLATE=utf8mb4_uca1400_as_ci",
        b"COLLATE=utf8mb4_0900_as_ci",
    ),
    (
        b"COLLATE utf8mb4_uca1400_as_ci",
        b"COLLATE utf8mb4_0900_as_ci",
    ),
    (
        b"COLLATE=utf8mb4_uca1400_as_cs",
        b"COLLATE=utf8mb4_0900_as_cs",
    ),
    (
        b"COLLATE utf8mb4_uca1400_as_cs",
        b"COLLATE utf8mb4_0900_as_cs",
    ),
    (
        b"COLLATE=utf8mb4_uca1400_ai_cs",
        b"COLLATE=utf8mb4_0900_as_cs",
    ),
    (
        b"COLLATE utf8mb4_uca1400_ai_cs",
        b"COLLATE utf8mb4_0900_as_cs",
    ),
    (
        b"COLLATE=utf8mb3_uca1400_ai_ci",
        b"COLLATE=utf8mb3_unicode_ci",
    ),
    (
        b"COLLATE utf8mb3_uca1400_ai_ci",
        b"COLLATE utf8mb3_unicode_ci",
    ),
    (
        b"COLLATE=utf8mb4_uca1400_nopad_ai_ci",
        b"COLLATE=utf8mb4_0900_ai_ci",
    ),
    (
        b"COLLATE utf8mb4_uca1400_nopad_ai_ci",
        b"COLLATE utf8mb4_0900_ai_ci",
    ),
];
/// Longest pattern above: bytes kept back between chunks so a name split by a chunk boundary
/// is still seen whole.
const CARRY: usize = 40;

fn mariadb_to_mysql(data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    for (from, to) in MARIADB_COLLATIONS {
        if find(&out, from).is_some() {
            let mut res = Vec::with_capacity(out.len());
            let mut i = 0;
            while let Some(j) = find(&out[i..], from) {
                res.extend_from_slice(&out[i..i + j]);
                res.extend_from_slice(to);
                i += j + from.len();
            }
            res.extend_from_slice(&out[i..]);
            out = res;
        }
    }
    out
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Stream one database from the source server straight into Veranda's server. `tools` is the
/// package whose dump program reads the source (the source's own engine when we have it, so
/// mysqldump never has to talk to a MariaDB and vice versa).
fn transfer_database(
    db: &DbCtx,
    tools: &DbCtx,
    req: &Request,
    name: &str,
    root_pw: &str,
    control: &Control,
    on_bytes: &dyn Fn(u64),
) -> Result<()> {
    let mut dump = crate::process::new_cmd(Binaries::db_dump(&tools.pkg, tools.engine));
    dump.args(source_client_args(tools.engine, req))
        .args([
            "--databases",
            name,
            "--single-transaction",
            "--skip-lock-tables",
            "--routines",
            "--events",
            "--triggers",
        ])
        .env("MYSQL_PWD", &req.db_password)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut dump = dump.spawn().context("starting mysqldump")?;
    let pipe = dump.stdout.take().context("dump pipe")?;
    let mut load = crate::process::new_cmd(Binaries::db_client(&db.pkg, db.engine));
    load.args(db.client_base_args())
        .args([
            "--protocol=tcp",
            "-h",
            "127.0.0.1",
            "-P",
            &db.port.to_string(),
            "-u",
            "root",
        ])
        .env("MYSQL_PWD", root_pw)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut load = load.spawn().context("starting the database client")?;
    // stderr of both tools is drained on its own threads: left unread, a few kilobytes of
    // warnings would fill the pipe and freeze the whole copy with both processes waiting.
    let drain = |stream: Option<std::process::ChildStderr>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut s) = stream {
                let _ = std::io::Read::read_to_end(&mut s, &mut buf);
            }
            buf
        })
    };
    let dump_err = drain(dump.stderr.take());
    let load_err = drain(load.stderr.take());
    // Pump dump -> client ourselves so the bytes can be counted for the progress bar; a
    // broken pipe here means the client died, which its stderr explains below.
    let mut sink = load.stdin.take().context("client stdin")?;
    let mut src = pipe;
    let mut buf = vec![0u8; 256 * 1024];
    let mut total: u64 = 0;
    let mut first = true;
    let translate = tools.engine == DbEngine::MariaDb && db.engine == DbEngine::MySql;
    let mut carry: Vec<u8> = Vec::new();
    let mut client_died = false;
    let mut last_report = std::time::Instant::now();
    let mut stopped = false;
    loop {
        // Pause: stop pulling from the dump; it blocks on its pipe, the client waits on stdin,
        // nothing is lost. "Stop now": the dump is killed and this database reported.
        while control.is_paused() && control.cancel_level() < 2 {
            std::thread::sleep(Duration::from_millis(150));
        }
        if control.cancel_level() >= 2 {
            let _ = dump.kill();
            stopped = true;
            break;
        }
        let n = std::io::Read::read(&mut src, &mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = if first {
            first = false;
            strip_sandbox_header(&buf[..n])
        } else {
            &buf[..n]
        };
        let written = if translate {
            // rewrite MariaDB-only collations; keep a tail back so a name split across two
            // chunks is still matched on the next round
            carry.extend_from_slice(chunk);
            let mut data = mariadb_to_mysql(&carry);
            let keep = data.len().min(CARRY);
            let rest = data.split_off(data.len() - keep);
            carry = rest;
            std::io::Write::write_all(&mut sink, &data)
        } else {
            std::io::Write::write_all(&mut sink, chunk)
        };
        if written.is_err() {
            // The client is gone (its stderr says why). Without this the dump would block on
            // a pipe nobody reads any more, and the whole migration with it.
            client_died = true;
            let _ = dump.kill();
            break;
        }
        total += n as u64;
        if last_report.elapsed() >= Duration::from_millis(300) {
            on_bytes(total);
            last_report = std::time::Instant::now();
        }
    }
    if stopped {
        drop(sink);
        drop(src);
        let _ = load.wait();
        let _ = dump.wait();
        bail!("stopped by you; copied again next time");
    }
    if !client_died && !carry.is_empty() {
        let data = mariadb_to_mysql(&carry);
        if std::io::Write::write_all(&mut sink, &data).is_err() {
            client_died = true;
        }
    }
    drop(sink);
    drop(src);
    on_bytes(total);
    let load_status = load.wait()?;
    let dump_status = dump.wait()?;
    let dump_err = dump_err.join().unwrap_or_default();
    let load_err = load_err.join().unwrap_or_default();
    let tail = |b: &[u8]| {
        let s = String::from_utf8_lossy(b);
        let lines: Vec<&str> = s
            .lines()
            .filter(|l| !l.contains("MYSQL_PWD") && !l.trim().is_empty())
            .collect();
        lines[lines.len().saturating_sub(6)..].join(" | ")
    };
    // The client's complaint is the useful one: a dump that was stopped because the client
    // left has nothing to say.
    if !load_status.success() || client_died {
        let msg = tail(&load_err);
        bail!(
            "import failed: {}",
            if msg.is_empty() {
                "the database client exited without a message".into()
            } else {
                msg
            }
        );
    }
    if !dump_status.success() {
        bail!("dump failed: {}", tail(&dump_err));
    }
    Ok(())
}

fn import_databases(
    app: &Veranda,
    req: &Request,
    control: &Control,
    report: &mut Report,
    progress: &dyn Fn(Progress),
) -> Result<()> {
    let reg = Registry::load(&app.layout)?;
    let db = DbCtx::from_app(app, &reg).context("install and start Veranda's database first")?;
    if !crate::process::port_accepting(db.port) {
        bail!("start Veranda's database first (it receives the imported data)");
    }
    let host = req.db_host.trim();
    if (host.is_empty() || host == "127.0.0.1" || host == "localhost" || host == "::1")
        && req.db_port == db.port
    {
        bail!(
            "{host}:{} is Veranda's own database, not the source. Start the other stack's database server and enter its port here; if both use port {}, move Veranda's database to another port in Settings first",
            db.port,
            db.port
        );
    }
    let root_pw = crate::db::root_password(&app.layout)?.context("root password unknown")?;
    let names = list_source_databases(&db, req)?;
    if names.is_empty() {
        report
            .skipped
            .push("no user databases on the source server".into());
        return Ok(());
    }
    let sizes = source_db_sizes(&db, req, &names);
    let mut prog = DbProgress::new(progress, &names, sizes);
    let mut remaining = names.len();
    for name in names {
        if control.cancel_level() >= 1 {
            report
                .skipped
                .push(format!("cancelled: {remaining} databases not copied"));
            break;
        }
        remaining -= 1;
        prog.report(&name, 0);
        let r = transfer_database(&db, &db, req, &name, &root_pw, control, &|b| {
            prog.report(&name, b)
        });
        prog.finish(&name);
        match r {
            Ok(()) => report.databases.push(name),
            Err(e) => report.errors.push(format!("{name}: {e:#}")),
        }
    }
    Ok(())
}

fn engine_name(e: DbEngine) -> &'static str {
    match e {
        DbEngine::MariaDb => "MariaDB",
        DbEngine::MySql => "MySQL",
    }
}

fn engine_by_id(id: &str) -> Option<DbEngine> {
    match id {
        "mariadb" => Some(DbEngine::MariaDb),
        "mysql" => Some(DbEngine::MySql),
        _ => None,
    }
}

/// The data of Veranda's *other* engine (MariaDB while MySQL is active, or the reverse), offered
/// as a source so switching engines does not strand the databases. `found` is false when that
/// engine's package was removed; the user installs it again and the entry becomes usable.
pub fn detect_veranda(app: &Veranda) -> Vec<Source> {
    let Ok(reg) = Registry::load(&app.layout) else {
        return vec![];
    };
    let active = app.config.db.engine;
    let mut v = vec![];
    for engine in [DbEngine::MariaDb, DbEngine::MySql] {
        if engine == active {
            continue;
        }
        let data = app.layout.data().join(engine.id());
        if !data.join("mysql").is_dir() {
            continue;
        }
        let installed = reg.newest(crate::services::db::kind(engine)).is_some();
        v.push(Source {
            platform: format!("veranda:{}", engine.id()),
            label: format!(
                "Veranda {} → {} (databases stored here)",
                engine_name(engine),
                engine_name(active)
            ),
            path: Some(data.display().to_string()),
            found: installed,
            installed_at: if installed {
                None
            } else {
                Some(data.display().to_string())
            },
            db_port: 0,
            db_user: "root".into(),
            db_password: String::new(),
        });
    }
    v
}

/// Copy every database of the inactive engine into the active one: its server is started
/// temporarily on a free port with its own data directory, each database is streamed over with
/// its own dump tool, then it is shut down again. Nothing in its data directory is changed.
fn import_from_engine(
    app: &Veranda,
    engine: DbEngine,
    req: &Request,
    control: &Control,
    report: &mut Report,
    progress: &dyn Fn(Progress),
) -> Result<()> {
    let say = |m: String| progress(Progress::msg(m));
    const INSTANCE: &str = "db-migrate";
    let layout = &app.layout;
    let reg = Registry::load(layout)?;
    let target =
        DbCtx::from_app(app, &reg).context("install and start Veranda's database first")?;
    if engine == target.engine {
        bail!("the source is the same engine as the active database");
    }
    if !crate::process::port_accepting(target.port) {
        bail!(
            "start {} first: it receives the copied databases",
            engine_name(target.engine)
        );
    }
    let pkg = reg
        .newest(crate::services::db::kind(engine))
        .cloned()
        .with_context(|| {
            format!(
                "{} is not installed any more; install it from the sidebar (Versions tab) and try again",
                engine_name(engine)
            )
        })?;
    let port = crate::process::next_free_port(target.port + 1)
        .context("no free port for the temporary server")?;
    let src = DbCtx {
        engine,
        port,
        pkg,
        data_dir: layout.data().join(engine.id()),
        layout: layout.clone(),
    };
    if !src.is_initialized() {
        bail!(
            "no {} data found in {}",
            engine_name(engine),
            src.data_dir.display()
        );
    }
    if crate::process::running_pid(layout, INSTANCE).is_some() {
        bail!("a previous migration server is still running; wait a moment and try again");
    }
    let root_pw = if req.db_password.is_empty() {
        crate::db::root_password(layout)?.context("root password unknown")?
    } else {
        req.db_password.clone()
    };
    let conf = layout.etc().join(engine.id()).join("migrate.cnf");
    std::fs::create_dir_all(conf.parent().unwrap())?;
    let socket = crate::services::db::socket_path(layout, &src);
    crate::services::db::write_conf(layout, &src, cfg!(windows), &conf, &socket, INSTANCE)?;
    crate::services::db::ensure_runtime_libs(&src.pkg.path)?;
    say(format!(
        "Starting the stored {} on port {port}…",
        engine_name(engine)
    ));
    let runtime = crate::packages::runtime_path(layout, &reg, &app.config);
    let mut cmd = crate::process::command(&src.server_binary(), &layout.path_entries(&runtime));
    cmd.arg(format!("--defaults-file={}", conf.display()))
        .current_dir(&src.pkg.path);
    if cfg!(windows) {
        cmd.arg("--console");
    }
    crate::process::spawn_detached(layout, INSTANCE, cmd)?;
    let source_req = Request {
        platform: req.platform.clone(),
        path: String::new(),
        mode: "link".into(),
        databases: true,
        db_host: "127.0.0.1".into(),
        db_port: port,
        db_user: "root".into(),
        db_password: root_pw.clone(),
    };
    let mut work = || -> Result<()> {
        if !crate::process::wait_for_port(port, Duration::from_secs(60)) {
            bail!(
                "the {} server did not start; see logs/{INSTANCE}.log",
                engine_name(engine)
            );
        }
        // SQL may still be a moment away after the port opens; a wrong root password shows up
        // here as "Access denied" and is reported as such.
        let started = std::time::Instant::now();
        let names = loop {
            match list_source_databases(&src, &source_req) {
                Ok(n) => break n,
                Err(e) if crate::process::running_pid(layout, INSTANCE).is_none() => {
                    bail!(
                        "the {} server exited: {e:#} (see logs/{INSTANCE}.log)",
                        engine_name(engine)
                    )
                }
                Err(e) if started.elapsed() > Duration::from_secs(45) => {
                    let hint = if format!("{e:#}").contains("Access denied") {
                        " The stored root password differs from the current one (it was changed after switching engines); enter the old password in the dialog."
                    } else {
                        ""
                    };
                    bail!("{e:#}.{hint}");
                }
                Err(_) => std::thread::sleep(Duration::from_millis(500)),
            }
        };
        if names.is_empty() {
            report.skipped.push(format!(
                "no user databases in the stored {}",
                engine_name(engine)
            ));
            return Ok(());
        }
        let sizes = source_db_sizes(&src, &source_req, &names);
        let mut prog = DbProgress::new(progress, &names, sizes);
        let mut remaining = names.len();
        for name in names {
            if control.cancel_level() >= 1 {
                report
                    .skipped
                    .push(format!("cancelled: {remaining} databases not copied"));
                break;
            }
            remaining -= 1;
            prog.report(&name, 0);
            let r = transfer_database(&target, &src, &source_req, &name, &root_pw, control, &|b| {
                prog.report(&name, b)
            });
            prog.finish(&name);
            match r {
                Ok(()) => report.databases.push(name),
                Err(e) => report.errors.push(format!("{name}: {e:#}")),
            }
        }
        Ok(())
    };
    let result = work();
    say("Stopping the temporary server…".into());
    let mut shutdown = crate::process::new_cmd(Binaries::db_client(&src.pkg, src.engine));
    shutdown
        .args(source_client_args(src.engine, &source_req))
        .args(["-e", "SHUTDOWN"])
        .env("MYSQL_PWD", &root_pw)
        .stdin(Stdio::null());
    let _ = shutdown.output();
    let started = std::time::Instant::now();
    while crate::process::running_pid(layout, INSTANCE).is_some()
        && started.elapsed() < Duration::from_secs(20)
    {
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = crate::process::stop_named(layout, INSTANCE, Duration::from_secs(10));
    let _ = std::fs::remove_file(&conf);
    result
}

/// Run an import. Site errors never abort the database step and vice versa; everything is
/// collected in the report.
pub fn run(
    app: &Veranda,
    req: &Request,
    control: &Control,
    progress: &dyn Fn(Progress),
) -> Result<Report> {
    if let Some(id) = req.platform.strip_prefix("veranda:") {
        let engine = engine_by_id(id).context("unknown engine")?;
        let mut report = Report::default();
        if let Err(e) = import_from_engine(app, engine, req, control, &mut report, progress) {
            report.errors.push(format!("databases: {e:#}"));
        }
        return Ok(report);
    }
    if req.path.trim().is_empty() {
        bail!("choose the source folder first");
    }
    if !matches!(req.mode.as_str(), "link" | "copy") {
        bail!("mode must be link or copy");
    }
    let mut report = Report::default();
    if let Err(e) = import_sites(app, req, &mut report, progress) {
        report.errors.push(format!("sites: {e:#}"));
    }
    if req.databases
        && let Err(e) = import_databases(app, req, control, &mut report, progress)
    {
        report.errors.push(format!("databases: {e:#}"));
    }
    Ok(report)
}

#[cfg(test)]
mod sandbox_tests {
    use super::{mariadb_to_mysql, strip_sandbox_header};

    #[test]
    fn rewrites_mariadb_only_collations_in_definitions_only() {
        let ddl = b"CREATE DATABASE `x` /*!40100 DEFAULT CHARACTER SET utf8mb4 COLLATE utf8mb4_uca1400_ai_ci */;\nCREATE TABLE t (a varchar(9) COLLATE utf8mb4_uca1400_as_cs) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_uca1400_ai_ci;\nINSERT INTO t VALUES ('utf8mb4_uca1400_ai_ci');\n";
        let out = String::from_utf8(mariadb_to_mysql(ddl)).unwrap();
        assert!(out.contains("COLLATE utf8mb4_0900_ai_ci */"));
        assert!(out.contains("COLLATE utf8mb4_0900_as_cs)"));
        assert!(out.contains("COLLATE=utf8mb4_0900_ai_ci;"));
        assert!(
            out.contains("VALUES ('utf8mb4_uca1400_ai_ci')"),
            "data untouched"
        );
        assert_eq!(mariadb_to_mysql(b"nothing here"), b"nothing here");
    }

    #[test]
    fn drops_only_the_mariadb_sandbox_line() {
        let dump =
            b"/*!999999\\- enable the sandbox mode */ \n-- MariaDB dump\nCREATE DATABASE x;\n";
        assert_eq!(
            strip_sandbox_header(dump),
            b"-- MariaDB dump\nCREATE DATABASE x;\n"
        );
        let plain = b"-- MySQL dump 10.13\nCREATE DATABASE x;\n";
        assert_eq!(strip_sandbox_header(plain), plain);
        assert_eq!(strip_sandbox_header(b"/*!999999 no newline yet"), b"");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_projects_skip_hidden_and_builtin() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["blog", ".git", "_tmp", "dashboard", "shop"] {
            std::fs::create_dir_all(dir.path().join(n)).unwrap();
        }
        std::fs::write(dir.path().join("index.php"), "<?php").unwrap();
        let names: Vec<String> = projects_in_folder(dir.path(), "xampp")
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["blog", "shop"]);
        let all = projects_in_folder(dir.path(), "laragon").unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn flyenv_hosts_parse() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("demo");
        std::fs::create_dir_all(proj.join("public")).unwrap();
        // Built with serde so Windows paths (backslashes) are escaped correctly.
        let json = serde_json::json!([
            { "id": 1, "name": "demo.test", "root": proj.join("public").display().to_string(), "alias": "" },
            { "id": 2, "name": "", "root": "" }
        ]);
        let f = dir.path().join("host.json");
        std::fs::write(&f, serde_json::to_string(&json).unwrap()).unwrap();
        let p = projects_in_flyenv(&f).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].name, "demo");
        assert_eq!(p[0].path, proj);
        assert_eq!(p[0].hostname.as_deref(), Some("demo.test"));
    }

    #[test]
    fn flyenv_dir_and_vhost_confs() {
        let dir = tempfile::tempdir().unwrap();
        let fly = dir.path().join("FlyEnv");
        let proj = dir.path().join("shop");
        std::fs::create_dir_all(proj.join("public")).unwrap();
        // installed but empty: a clear error, no sites
        std::fs::create_dir_all(fly.join("server").join("vhost").join("apache")).unwrap();
        assert!(flyenv_site_source(&fly).is_none());
        let e = projects_in_flyenv(&fly).unwrap_err().to_string();
        assert!(e.contains("no sites yet"), "{e}");
        // an apache vhost appears
        std::fs::write(
            fly.join("server/vhost/apache/shop.test.conf"),
            format!("<VirtualHost *:80>\n  ServerName shop.test\n  DocumentRoot \"{}\"\n</VirtualHost>\n", proj.join("public").display()),
        )
        .unwrap();
        // and an nginx one for the same host must not duplicate it
        std::fs::create_dir_all(fly.join("server/vhost/nginx")).unwrap();
        std::fs::write(
            fly.join("server/vhost/nginx/shop.test.conf"),
            format!(
                "server {{\n  server_name shop.test;\n  root {};\n}}\n",
                proj.join("public").display()
            ),
        )
        .unwrap();
        assert_eq!(
            flyenv_site_source(&fly).unwrap(),
            fly.join("server").join("vhost")
        );
        let p = projects_in_flyenv(&fly).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].name, "shop");
        assert_eq!(p[0].path, proj);
        assert_eq!(p[0].hostname.as_deref(), Some("shop.test"));
        // host.json wins when present
        std::fs::write(fly.join("server/host.json"), "[]").unwrap();
        assert_eq!(
            flyenv_site_source(&fly).unwrap(),
            fly.join("server").join("host.json")
        );
    }

    #[test]
    fn link_import_adds_manual_sites() {
        let root = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("My App")).unwrap();
        let layout = crate::Layout::new(root.path());
        layout.ensure().unwrap();
        let app = Veranda {
            layout,
            config: crate::Config::default(),
        };
        let req = Request {
            platform: "folder".into(),
            path: src.path().display().to_string(),
            mode: "link".into(),
            databases: false,
            db_host: String::new(),
            db_port: 0,
            db_user: String::new(),
            db_password: String::new(),
        };
        let rep = run(&app, &req, &Control::default(), &|_| {}).unwrap();
        assert_eq!(rep.sites, vec!["my-app.test"]);
        assert!(rep.errors.is_empty());
        let sites = sites::discover(&app.layout, &app.config).unwrap();
        assert!(sites.iter().any(|s| s.hostname == "my-app.test" && !s.auto));
    }
}
