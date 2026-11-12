//! Directory layout of a Veranda installation root.
//!
//! ```text
//! <root>/
//!   bin/<kind>/<version>/   downloaded runtimes (php, caddy, mariadb, ...)
//!   etc/                    generated configuration (veranda.toml, Caddyfile, php.ini, ...)
//!   etc/ssl/                local CA and site certificates
//!   etc/secrets/            generated passwords (0600)
//!   etc/apps/               web tools served under http://localhost/<tool> (adminer, phpmyadmin)
//!   data/                   database data directories
//!   www/                    document root; each sub folder becomes <name>.test
//!   logs/                   service logs
//!   run/                    pid files
//!   tmp/                    downloads and scratch space
//!   backups/                database dumps
//!   usr/                    user customisations (extra services, startup scripts)
//! ```

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const ROOT_ENV: &str = "VERANDA_HOME";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Default root: `<home>/veranda` on every platform (no spaces, user-writable).
    pub fn default_root() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("veranda")
    }

    /// Small pointer file in the per-user config dir that remembers a custom root.
    pub fn pointer_file() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("veranda").join("root"))
    }

    /// Resolution order: `$VERANDA_HOME`, pointer file, default root.
    pub fn discover() -> Result<Self> {
        if let Ok(v) = std::env::var(ROOT_ENV)
            && !v.trim().is_empty()
        {
            return Ok(Self::new(PathBuf::from(v.trim())));
        }
        if let Some(p) = Self::pointer_file()
            && let Ok(s) = std::fs::read_to_string(&p)
        {
            let s = s.trim();
            if !s.is_empty() {
                let l = Self::new(PathBuf::from(s));
                Self::migrate_config_file(l.root());
                return Ok(l);
            }
        }
        if let Some(p) = Self::legacy_pointer_file()
            && let Ok(s) = std::fs::read_to_string(&p)
            && !s.trim().is_empty()
        {
            let l = Self::new(PathBuf::from(s.trim()));
            let _ = l.remember();
            Self::migrate_config_file(l.root());
            return Ok(l);
        }
        let root = Self::default_root();
        Self::migrate_legacy(&root);
        Ok(Self::new(root))
    }

    /// Pointer file written by the app while it was still called Padu.
    fn legacy_pointer_file() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("padu").join("root"))
    }

    /// First start after the rename from Padu: move `~/padu` to the new default root when
    /// nothing from it is running, and rename the config file. Best effort; on any failure the
    /// old folder simply stays where it is and a fresh root is created.
    fn migrate_legacy(root: &Path) {
        if let Some(home) = dirs::home_dir() {
            let old = home.join("padu");
            if !root.exists() && old.is_dir() && !Self::has_live_pids(&old) {
                let _ = std::fs::rename(&old, root);
            }
        }
        Self::migrate_config_file(root);
    }

    fn migrate_config_file(root: &Path) {
        let etc = root.join("etc");
        let old = etc.join("padu.toml");
        let new = etc.join("veranda.toml");
        if old.is_file() && !new.exists() {
            let _ = std::fs::rename(old, new);
        }
    }

    fn has_live_pids(root: &Path) -> bool {
        let Ok(rd) = std::fs::read_dir(root.join("run")) else {
            return false;
        };
        rd.flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "pid"))
            .any(|e| {
                std::fs::read_to_string(e.path())
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .is_some_and(crate::process::is_pid_alive)
            })
    }

    /// Persist a custom root so later invocations find it.
    pub fn remember(&self) -> Result<()> {
        if let Some(p) = Self::pointer_file() {
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            crate::fsutil::write_atomic(&p, self.root.to_string_lossy().as_bytes())?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }
    pub fn bin_kind(&self, kind: &str) -> PathBuf {
        self.bin().join(kind)
    }
    pub fn etc(&self) -> PathBuf {
        self.root.join("etc")
    }
    pub fn ssl(&self) -> PathBuf {
        self.etc().join("ssl")
    }
    pub fn secrets(&self) -> PathBuf {
        self.etc().join("secrets")
    }
    pub fn apps(&self) -> PathBuf {
        self.etc().join("apps")
    }
    pub fn data(&self) -> PathBuf {
        self.root.join("data")
    }
    pub fn www(&self) -> PathBuf {
        self.root.join("www")
    }
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
    pub fn run(&self) -> PathBuf {
        self.root.join("run")
    }
    pub fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }
    pub fn downloads(&self) -> PathBuf {
        self.tmp().join("downloads")
    }
    pub fn backups(&self) -> PathBuf {
        self.root.join("backups")
    }
    pub fn usr(&self) -> PathBuf {
        self.root.join("usr")
    }
    pub fn config_file(&self) -> PathBuf {
        self.etc().join("veranda.toml")
    }
    pub fn sites_file(&self) -> PathBuf {
        self.etc().join("sites.toml")
    }
    pub fn checksums_file(&self) -> PathBuf {
        self.etc().join("checksums.json")
    }
    pub fn installed_file(&self) -> PathBuf {
        self.etc().join("installed.json")
    }

    /// Create every directory; secrets/ssl are private.
    pub fn ensure(&self) -> Result<()> {
        for d in [
            self.bin(),
            self.etc(),
            self.apps(),
            self.data(),
            self.www(),
            self.logs(),
            self.run(),
            self.tmp(),
            self.downloads(),
            self.backups(),
            self.usr(),
        ] {
            std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
        }
        for d in [self.ssl(), self.secrets()] {
            std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
            crate::fsutil::make_private_dir(&d)?;
        }
        Ok(())
    }

    /// Paths of installed runtime binaries, for prefixing `PATH` in terminals and child processes.
    pub fn path_entries(&self, extra: &[PathBuf]) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = extra.to_vec();
        v.push(self.usr().join("bin"));
        v
    }

    /// Normalise a path into forward slashes (used inside generated config files).
    pub fn slashes(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }
}
