//! User configuration (`etc/veranda.toml`). Every field has a safe default so a missing
//! or partial file still loads.

use crate::paths::Layout;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub general: General,
    pub web: Web,
    pub php: Php,
    pub db: Db,
    pub pg: Pg,
    pub mongodb: Extra,
    pub backup: Backup,
    pub update: Update,
    pub node: Node,
    /// "Move to another computer": folders picked last time, so the next bundle starts from
    /// the same list.
    #[serde(rename = "move")]
    pub transfer: Transfer,
    /// Which optional parts of the app are shown. Everything stays installed; a module that is
    /// off only disappears from the sidebar and the pages, so the window can be as plain as a
    /// local server manager.
    pub modules: Modules,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Modules {
    /// SSH client: hosts, terminal, SFTP, port forwarding, snippets, keychain.
    pub remote: bool,
    /// The Files editor.
    pub files: bool,
    /// Move to another computer.
    #[serde(rename = "move")]
    pub transfer: bool,
}

impl Default for Modules {
    fn default() -> Self {
        Self {
            remote: true,
            files: true,
            transfer: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Transfer {
    pub folders: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct General {
    /// Reserved TLD (RFC 6761) used for auto virtual hosts.
    pub tld: String,
    /// `{name}` is replaced by the project folder name, `{tld}` by `tld`.
    pub hostname_format: String,
    /// Empty = `<root>/www`.
    pub document_root: String,
    /// Scan the document root and create a virtual host per folder.
    pub auto_vhosts: bool,
    /// If a project has `public/index.php`, serve `public/` (Laravel, Symfony...).
    pub smart_public_dir: bool,
    /// Start enabled services when the app launches.
    pub auto_start: bool,
    /// Start the app minimized to the tray.
    pub start_minimized: bool,
    /// What the window close button does: ask every time, hide to the tray, or quit.
    pub close_action: CloseAction,
    /// Older setting kept for compatibility (false used to mean "quit"); not written any more.
    #[serde(skip_serializing)]
    pub close_to_tray: bool,
    /// Services hidden from the dashboard (`web`, `php`, `db`, `cache`, `mail`). Hidden services are
    /// never started by "Start All".
    pub hidden_services: Vec<String>,
    /// Named sets of services started/stopped together, in order (like FlyEnv's startup groups).
    pub startup_groups: Vec<StartupGroup>,
    /// Group started on launch when "Start services on launch" is on (empty = every service that
    /// is switched on).
    pub default_group: String,
    /// Command for "Edit" actions, e.g. `code`. Empty = system default.
    pub editor: String,
    /// Command for "Open" actions. Empty = system default browser.
    pub browser: String,
    /// UI language code (`en`, `id`, ...).
    pub language: String,
    /// Linux only: let WebKitGTK render with the GPU (DMA-BUF). Turn off if the window
    /// flickers or stays black on an unusual driver. Needs a restart.
    pub hardware_acceleration: bool,
    /// Drop debug builds, test suites, static libraries, man pages and headers from packages
    /// right after extraction (MySQL alone loses ~550 MB). Nothing the stack runs is removed.
    pub slim_packages: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Web {
    pub enabled: bool,
    /// `nginx` (default) or `apache`. Older files saying `caddy` fall back to nginx.
    pub server: WebServer,
    pub http_port: u16,
    pub https_port: u16,
    /// Serve HTTPS with the local CA.
    pub ssl: bool,
    /// Installed version to use; empty = newest installed.
    pub version: String,
    /// Allow directory listings for folders without an index file (localhost only).
    pub autoindex: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum CloseAction {
    /// Show a small dialog (hide or quit) with a "remember my choice" box.
    #[default]
    Ask,
    /// Hide the window; services keep running; the tray icon stays.
    Tray,
    /// Stop services and exit.
    Quit,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum WebServer {
    Apache,
    /// The default; also what any unknown value (such as the former `caddy`) becomes.
    #[serde(other)]
    Nginx,
}

/// A startup group: services (`web`, `php`, `db`...) started in this order and stopped in reverse.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct StartupGroup {
    pub name: String,
    pub services: Vec<String>,
}

/// Which Node.js goes first on the `PATH` of terminals and project commands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Node {
    /// `""` = newest version installed by Veranda; `26.13.0` = that Veranda version;
    /// `nvm:24.22.2` (or `fnm:`/`volta:`) = a version managed by that tool.
    pub default: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Php {
    pub enabled: bool,
    /// Installed version to use; empty = newest installed.
    pub version: String,
    /// First FastCGI port; Windows spawns `workers` php-cgi processes on consecutive ports.
    pub fcgi_port: u16,
    pub workers: u16,
    pub memory_limit: String,
    pub upload_max_filesize: String,
    pub max_execution_time: u32,
    pub timezone: String,
    pub xdebug: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Db {
    pub enabled: bool,
    /// `mariadb` or `mysql`.
    pub engine: DbEngine,
    pub version: String,
    pub port: u16,
    /// Empty = `<root>/data/<engine>`.
    pub data_dir: String,
}

/// Settings of the single-binary services (MongoDB).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Extra {
    pub enabled: bool,
    pub version: String,
    /// 0 = the service's conventional port (27017).
    pub port: u16,
    /// Empty = `<root>/data/<service>`.
    pub data_dir: String,
}

/// PostgreSQL runs beside MariaDB/MySQL, not instead of it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Pg {
    pub enabled: bool,
    pub version: String,
    pub port: u16,
    /// Empty = `<root>/data/postgres`.
    pub data_dir: String,
}

impl Default for Pg {
    fn default() -> Self {
        Self {
            enabled: false,
            version: String::new(),
            port: 5432,
            data_dir: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum DbEngine {
    MariaDb,
    MySql,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Backup {
    pub enabled: bool,
    pub interval_hours: u32,
    pub keep: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Update {
    /// Advanced, not shown in the app: GitHub repository (`owner/repo`) to check for new app
    /// versions. Empty = the official Veranda repository. Only useful for forks.
    pub repo: String,
    pub check_on_start: bool,
    /// Also look for newer PHP / server versions on start (network access).
    pub check_packages: bool,
    /// Install app updates automatically when found (otherwise just notify).
    pub auto_install: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            tld: "test".into(),
            hostname_format: "{name}.{tld}".into(),
            document_root: String::new(),
            auto_vhosts: true,
            smart_public_dir: true,
            auto_start: false,
            start_minimized: false,
            close_action: CloseAction::Ask,
            close_to_tray: true,
            hidden_services: Vec::new(),
            startup_groups: Vec::new(),
            default_group: String::new(),
            editor: String::new(),
            browser: String::new(),
            language: "en".into(),
            hardware_acceleration: true,
            slim_packages: true,
        }
    }
}

impl Default for Web {
    fn default() -> Self {
        Self {
            enabled: false,
            server: WebServer::Nginx,
            http_port: 80,
            https_port: 443,
            ssl: true,
            version: String::new(),
            autoindex: true,
        }
    }
}

impl Default for Php {
    fn default() -> Self {
        Self {
            enabled: false,
            version: String::new(),
            fcgi_port: 9000,
            workers: 4,
            memory_limit: "512M".into(),
            upload_max_filesize: "256M".into(),
            max_execution_time: 300,
            timezone: "UTC".into(),
            xdebug: false,
        }
    }
}

impl Default for Db {
    fn default() -> Self {
        Self {
            enabled: false,
            engine: DbEngine::MariaDb,
            version: String::new(),
            port: 3306,
            data_dir: String::new(),
        }
    }
}

impl Default for Backup {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_hours: 8,
            keep: 5,
        }
    }
}

/// Where official Veranda releases are published.
pub const DEFAULT_UPDATE_REPO: &str = "livrens/veranda";
/// Earlier homes of the project; treated like an empty setting.
pub const LEGACY_UPDATE_REPOS: &[&str] = &[
    "forallgame/padu",
    "forallgame/veranda",
    "livrensandi1/veranda",
];

impl Update {
    /// Configured repository, or the official one when the field is empty.
    pub fn repo_or_default(&self) -> &str {
        let r = self.repo.trim();
        // Empty, or one of the addresses this project had before: a config written in the
        // Padu days or under the previous account would otherwise keep checking a repository
        // that no longer publishes releases.
        if r.is_empty()
            || LEGACY_UPDATE_REPOS
                .iter()
                .any(|old| old.eq_ignore_ascii_case(r))
        {
            DEFAULT_UPDATE_REPO
        } else {
            r
        }
    }
}

impl Default for Update {
    fn default() -> Self {
        Self {
            repo: String::new(),
            check_on_start: true,
            check_packages: false,
            auto_install: false,
        }
    }
}

impl Config {
    pub fn load_or_default(layout: &Layout) -> Result<Self> {
        let path = layout.config_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mut cfg: Config =
                    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
                // Migrate the old boolean: `close_to_tray = false` meant "quit on close".
                if !cfg.general.close_to_tray && cfg.general.close_action == CloseAction::Ask {
                    cfg.general.close_action = CloseAction::Quit;
                }
                // services that no longer exist (cache, mail...) vanish from startup groups
                for g in &mut cfg.general.startup_groups {
                    g.services
                        .retain(|s| s.parse::<crate::services::ServiceId>().is_ok());
                }
                cfg.general
                    .startup_groups
                    .retain(|g| !g.services.is_empty());
                cfg.validate()?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                cfg.save(layout)?;
                Ok(cfg)
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, layout: &Layout) -> Result<()> {
        self.validate()?;
        let text = toml::to_string_pretty(self)?;
        let header = "# Veranda configuration. Edit and reload; unknown keys are rejected.\n\n";
        crate::fsutil::write_atomic(&layout.config_file(), format!("{header}{text}").as_bytes())
    }

    pub fn validate(&self) -> Result<()> {
        let tld = &self.general.tld;
        if tld.is_empty()
            || !tld
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            anyhow::bail!("general.tld must be lowercase letters/digits (e.g. `test`)");
        }
        if !self.general.hostname_format.contains("{name}") {
            anyhow::bail!("general.hostname_format must contain {{name}}");
        }
        if self.web.http_port == self.web.https_port {
            anyhow::bail!("web.http_port and web.https_port must differ");
        }
        if self.php.workers == 0 || self.php.workers > 64 {
            anyhow::bail!("php.workers must be between 1 and 64");
        }
        for id in &self.general.hidden_services {
            if !matches!(
                id.as_str(),
                "web"
                    | "php"
                    | "db"
                    | "pg"
                    | "cache"
                    | "mail"
                    | "mongodb"
                    | "meilisearch"
                    | "qdrant"
            ) {
                anyhow::bail!("general.hidden_services contains unknown service `{id}`");
            }
        }
        if self.backup.keep == 0 {
            anyhow::bail!("backup.keep must be at least 1");
        }
        if !self.update.repo.is_empty() && !is_repo_slug(&self.update.repo) {
            anyhow::bail!("update.repo must look like owner/repo");
        }
        Ok(())
    }

    pub fn document_root(&self, layout: &Layout) -> PathBuf {
        if self.general.document_root.trim().is_empty() {
            layout.www()
        } else {
            PathBuf::from(self.general.document_root.trim())
        }
    }

    pub fn db_data_dir(&self, layout: &Layout) -> PathBuf {
        if self.db.data_dir.trim().is_empty() {
            layout.data().join(self.db.engine.id())
        } else {
            PathBuf::from(self.db.data_dir.trim())
        }
    }

    pub fn pg_data_dir(&self, layout: &Layout) -> PathBuf {
        if self.pg.data_dir.trim().is_empty() {
            layout.data().join("postgres")
        } else {
            PathBuf::from(self.pg.data_dir.trim())
        }
    }

    /// Hostname for a project folder, e.g. `my-app` -> `my-app.test`.
    pub fn hostname_for(&self, name: &str) -> String {
        self.general
            .hostname_format
            .replace("{name}", name)
            .replace("{tld}", &self.general.tld)
    }
}

impl WebServer {
    pub const fn id(&self) -> &'static str {
        match self {
            WebServer::Nginx => "nginx",
            WebServer::Apache => "apache",
        }
    }
}

impl DbEngine {
    pub const fn id(&self) -> &'static str {
        match self {
            DbEngine::MariaDb => "mariadb",
            DbEngine::MySql => "mysql",
        }
    }
}

pub fn is_repo_slug(s: &str) -> bool {
    let mut parts = s.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), None) => {
            let ok = |x: &str| {
                !x.is_empty()
                    && x.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
            };
            ok(a) && ok(b)
        }
        _ => false,
    }
}

#[cfg(test)]
mod update_repo_tests {
    use super::*;

    #[test]
    fn legacy_repositories_fall_back_to_the_current_one() {
        let mut u = Update::default();
        assert_eq!(u.repo_or_default(), DEFAULT_UPDATE_REPO);
        u.repo = "forallgame/padu".into();
        assert_eq!(u.repo_or_default(), DEFAULT_UPDATE_REPO);
        u.repo = "LivrenSandi1/veranda".into();
        assert_eq!(u.repo_or_default(), DEFAULT_UPDATE_REPO);
        u.repo = "someone/fork".into();
        assert_eq!(u.repo_or_default(), "someone/fork");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_defaults() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg, back);
        assert_eq!(cfg.hostname_for("blog"), "blog.test");
    }

    #[test]
    fn partial_file_loads() {
        let cfg: Config = toml::from_str("[web]\nhttp_port = 8080\n").unwrap();
        assert_eq!(cfg.web.http_port, 8080);
        assert_eq!(cfg.web.https_port, 443);
    }

    #[test]
    fn unknown_key_ignored() {
        // Settings written by a newer Veranda must not stop an older build from starting.
        let cfg: Config =
            toml::from_str("[web]\nfoo = 1\nhttp_port = 8081\n[future_section]\nx = 2\n").unwrap();
        assert_eq!(cfg.web.http_port, 8081);
    }
}
