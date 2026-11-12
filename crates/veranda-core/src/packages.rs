//! Installed runtimes under `bin/<kind>/<version>/` and the registry that tracks them.

use crate::archive;
use crate::config::{DbEngine, WebServer};
use crate::download::{self, ProgressFn};
use crate::fsutil;
use crate::manifest::{ArchiveType, Kind, Package};
use crate::paths::Layout;
use crate::platform::Platform;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub kind: Kind,
    pub version: String,
    pub path: PathBuf,
    pub source: String,
    pub installed_at: String,
    /// SHA-256 of each downloaded part, for audit.
    #[serde(default)]
    pub digests: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub items: Vec<Installed>,
    /// Entries this build cannot read (written by a newer Veranda with package kinds we do not
    /// know). Kept verbatim and written back on save, so an older build never erases them.
    #[serde(skip)]
    foreign: Vec<serde_json::Value>,
}

impl Registry {
    /// Load `etc/installed.json`. The file is never the only source of truth: entries whose
    /// folder moved are healed, unreadable entries are preserved, and package folders that are
    /// missing from the file are re-registered, so a corrupt or stale file cannot "uninstall"
    /// anything.
    pub fn load(layout: &Layout) -> Result<Self> {
        let raw: serde_json::Value = match std::fs::read_to_string(layout.installed_file()) {
            Ok(t) => serde_json::from_str(&t).unwrap_or_else(|e| {
                // Keep a copy of the damaged file; the folders below rebuild the registry.
                tracing::warn!("installed.json is not valid JSON ({e}); backing it up");
                let backup = layout
                    .etc()
                    .join(format!("installed.broken-{}.json", fsutil::timestamp()));
                let _ = std::fs::copy(layout.installed_file(), backup);
                serde_json::Value::Null
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Value::Null,
            Err(e) => return Err(e.into()),
        };
        let mut reg = Registry::default();
        if let Some(arr) = raw.get("items").and_then(|v| v.as_array()) {
            for v in arr {
                match serde_json::from_value::<Installed>(v.clone()) {
                    Ok(i) => reg.items.push(i),
                    Err(_) => reg.foreign.push(v.clone()),
                }
            }
        }
        let mut changed = false;
        // Self-heal paths after the root moved (e.g. the Padu -> Veranda rename): every package
        // lives at the canonical `<root>/bin/<kind>/<version>`.
        for i in &mut reg.items {
            if !i.path.is_dir() {
                let expected = package_dir(layout, i.kind, &i.version);
                if expected.is_dir() {
                    i.path = expected;
                    changed = true;
                }
            }
        }
        // Drop entries whose folder disappeared (user deleted it by hand).
        let before = reg.items.len();
        reg.items.retain(|i| i.path.is_dir());
        changed |= reg.items.len() != before;
        // Re-register package folders the file does not mention.
        for kind in Kind::ALL {
            let Ok(rd) = std::fs::read_dir(layout.bin_kind(kind.id())) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !p.is_dir()
                    || name.starts_with('.')
                    || !fsutil::is_safe_name(&name.to_ascii_lowercase().replace('.', "-"))
                    || reg.find(kind, name).is_some()
                    || std::fs::read_dir(&p)
                        .map(|mut d| d.next().is_none())
                        .unwrap_or(true)
                {
                    continue;
                }
                tracing::info!("re-registering {kind} {name} found in {}", p.display());
                reg.items.push(Installed {
                    kind,
                    version: name.to_string(),
                    path: p.clone(),
                    source: "recovered".into(),
                    installed_at: fsutil::timestamp(),
                    digests: Vec::new(),
                });
                changed = true;
            }
        }
        if changed {
            let _ = reg.save(layout);
        }
        Ok(reg)
    }

    pub fn save(&self, layout: &Layout) -> Result<()> {
        let mut all: Vec<serde_json::Value> = self
            .items
            .iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<_, _>>()?;
        all.extend(self.foreign.iter().cloned());
        let text = serde_json::to_string_pretty(&serde_json::json!({ "items": all }))?;
        fsutil::write_atomic(&layout.installed_file(), text.as_bytes())
    }

    /// Installed versions of `kind`, newest first.
    pub fn list(&self, kind: Kind) -> Vec<&Installed> {
        let mut v: Vec<&Installed> = self.items.iter().filter(|i| i.kind == kind).collect();
        v.sort_by_key(|i| std::cmp::Reverse(crate::manifest::parse_version(&i.version)));
        v
    }

    pub fn find(&self, kind: Kind, version: &str) -> Option<&Installed> {
        self.items
            .iter()
            .find(|i| i.kind == kind && i.version == version)
    }

    pub fn newest(&self, kind: Kind) -> Option<&Installed> {
        self.list(kind).into_iter().next()
    }

    /// The version to use: configured one if installed, otherwise the newest installed.
    pub fn active(&self, kind: Kind, configured: &str) -> Option<&Installed> {
        if !configured.trim().is_empty()
            && let Some(i) = self.find(kind, configured.trim())
        {
            return Some(i);
        }
        self.newest(kind)
    }
}

pub fn package_dir(layout: &Layout, kind: Kind, version: &str) -> PathBuf {
    layout.bin_kind(kind.id()).join(version)
}

/// Download, verify, extract and register a package. Safe to call again (reinstalls).
pub async fn install(
    layout: &Layout,
    pkg: &Package,
    progress: Option<&ProgressFn>,
) -> Result<Installed> {
    if !pkg.matches(Platform::current()) {
        bail!(
            "package {} {} is for {} not {}",
            pkg.kind,
            pkg.version,
            pkg.platform(),
            Platform::current()
        );
    }
    if !fsutil::is_safe_name(&pkg.version.to_ascii_lowercase().replace('.', "-")) {
        bail!("refusing suspicious version string {:?}", pkg.version);
    }
    download::clear_cancel();
    let client = download::client()?;
    let final_dir = package_dir(layout, pkg.kind, &pkg.version);
    let staging = layout
        .bin_kind(pkg.kind.id())
        .join(format!(".staging-{}", pkg.version));
    fsutil::remove_dir_all_if_exists(&staging)?;
    std::fs::create_dir_all(&staging)?;

    let mut digests = Vec::new();
    let result: Result<()> = async {
        for part in &pkg.parts {
            let file_name = url_file_name(&part.url)?;
            let dl = layout
                .downloads()
                .join(format!("{}-{}-{}", pkg.kind, pkg.version, file_name));
            let digest =
                download::download(&client, layout, &part.url, &dl, &part.checksum, progress)
                    .await?;
            digests.push(digest);
            let dest = if part.dest.is_empty() {
                staging.clone()
            } else {
                staging.join(&part.dest)
            };
            std::fs::create_dir_all(&dest)?;
            match part.archive {
                ArchiveType::File => {
                    let name = if part.file_name.is_empty() {
                        file_name.clone()
                    } else {
                        part.file_name.clone()
                    };
                    if !fsutil::is_safe_name(&name.to_ascii_lowercase().replace('.', "-")) {
                        bail!("unsafe file name {name:?}");
                    }
                    std::fs::copy(&dl, dest.join(&name))?;
                }
                t => archive::extract(&dl, t, &dest, part.strip as usize as u32)?,
            }
            let _ = std::fs::remove_file(&dl);
        }
        post_install(pkg.kind, &staging)?;
        if crate::config::Config::load_or_default(layout)
            .map(|c| c.general.slim_packages)
            .unwrap_or(true)
        {
            crate::slim::prune_package(pkg.kind, &staging);
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        let _ = fsutil::remove_dir_all_if_exists(&staging);
        return Err(e);
    }

    fsutil::remove_dir_all_if_exists(&final_dir)?;
    std::fs::rename(&staging, &final_dir)
        .with_context(|| format!("moving into {}", final_dir.display()))?;

    let installed = Installed {
        kind: pkg.kind,
        version: pkg.version.clone(),
        path: final_dir,
        source: pkg.source.clone(),
        installed_at: fsutil::timestamp(),
        digests,
    };
    let mut reg = Registry::load(layout)?;
    reg.items
        .retain(|i| !(i.kind == pkg.kind && i.version == pkg.version));
    reg.items.push(installed.clone());
    reg.save(layout)?;
    Ok(installed)
}

pub fn remove(layout: &Layout, kind: Kind, version: &str) -> Result<()> {
    let mut reg = Registry::load(layout)?;
    let Some(item) = reg.find(kind, version).cloned() else {
        bail!("{kind} {version} is not installed")
    };
    if !item.path.starts_with(layout.bin()) {
        bail!("refusing to delete {} outside bin/", item.path.display());
    }
    fsutil::remove_dir_all_if_exists(&item.path)?;
    reg.items
        .retain(|i| !(i.kind == kind && i.version == version));
    reg.save(layout)
}

fn url_file_name(url: &str) -> Result<String> {
    let path = url.split('?').next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or("").to_string();
    if name.is_empty() || name.contains("..") || name.contains('\\') {
        bail!("cannot derive a file name from {url}");
    }
    Ok(name)
}

/// Kind specific fix-ups after extraction (executable bits, layout normalisation).
fn post_install(kind: Kind, dir: &Path) -> Result<()> {
    match kind {
        Kind::Php => {
            for n in ["php", "php-fpm", "php-cgi"] {
                let p = dir.join(n);
                if p.is_file() {
                    fsutil::make_executable(&p)?;
                }
            }
        }
        Kind::PhpMyAdmin => {
            // Generated config: connect to 127.0.0.1 only, cookie auth, random blowfish secret.
            let secret: String = crate::db::random_token(32);
            let cfg = format!(
                "<?php\n$cfg['blowfish_secret'] = '{secret}';\n$i = 1;\n$cfg['Servers'][$i]['auth_type'] = 'cookie';\n$cfg['Servers'][$i]['host'] = '127.0.0.1';\n$cfg['Servers'][$i]['AllowNoPassword'] = false;\n$cfg['TempDir'] = __DIR__ . '/tmp';\n$cfg['CheckConfigurationPermissions'] = false;\n"
            );
            std::fs::write(dir.join("config.inc.php"), cfg)?;
            std::fs::create_dir_all(dir.join("tmp"))?;
        }
        Kind::Postgres => {
            // The Maven jar is a zip holding one `postgres-<os>-<arch>.txz`; unpack it in place.
            let txz: Vec<PathBuf> = std::fs::read_dir(dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "txz"))
                .collect();
            for t in txz {
                archive::extract(&t, ArchiveType::TarXz, dir, 0)?;
                let _ = std::fs::remove_file(&t);
            }
            fsutil::remove_dir_all_if_exists(&dir.join("META-INF"))?;
            make_bin_executable(&dir.join("bin"))?;
        }
        Kind::Python | Kind::MongoDb => make_bin_executable(&dir.join("bin"))?,
        _ => {}
    }
    Ok(())
}

/// Tar/zip extraction does not always keep the mode bits: mark every file in `dir` runnable.
fn make_bin_executable(dir: &Path) -> Result<()> {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_file() {
                fsutil::make_executable(&e.path())?;
            }
        }
    }
    Ok(())
}

/// Resolve the main executables of an installed package.
pub struct Binaries;

impl Binaries {
    fn exe(p: &Path, name: &str) -> PathBuf {
        p.join(Platform::current().exe(name))
    }

    pub fn php(i: &Installed) -> PathBuf {
        Self::exe(&i.path, "php")
    }
    /// FastCGI server: `php-fpm` on unix, `php-cgi.exe` on Windows.
    pub fn php_fcgi(i: &Installed) -> PathBuf {
        if Platform::current().is_windows() {
            Self::exe(&i.path, "php-cgi")
        } else {
            i.path.join("php-fpm")
        }
    }
    pub fn nginx(i: &Installed) -> PathBuf {
        Self::exe(&i.path, "nginx")
    }
    pub fn httpd(i: &Installed) -> PathBuf {
        Self::exe(&i.path.join("bin"), "httpd")
    }
    pub fn db_server(i: &Installed, engine: DbEngine) -> PathBuf {
        let bin = i.path.join("bin");
        match engine {
            DbEngine::MariaDb => {
                let p = Self::exe(&bin, "mariadbd");
                if p.exists() {
                    p
                } else {
                    Self::exe(&bin, "mysqld")
                }
            }
            DbEngine::MySql => Self::exe(&bin, "mysqld"),
        }
    }
    pub fn db_client(i: &Installed, engine: DbEngine) -> PathBuf {
        let bin = i.path.join("bin");
        match engine {
            DbEngine::MariaDb => {
                let p = Self::exe(&bin, "mariadb");
                if p.exists() {
                    p
                } else {
                    Self::exe(&bin, "mysql")
                }
            }
            DbEngine::MySql => Self::exe(&bin, "mysql"),
        }
    }
    pub fn db_dump(i: &Installed, engine: DbEngine) -> PathBuf {
        let bin = i.path.join("bin");
        match engine {
            DbEngine::MariaDb => {
                let p = Self::exe(&bin, "mariadb-dump");
                if p.exists() {
                    p
                } else {
                    Self::exe(&bin, "mysqldump")
                }
            }
            DbEngine::MySql => Self::exe(&bin, "mysqldump"),
        }
    }
    pub fn db_admin(i: &Installed, engine: DbEngine) -> PathBuf {
        let bin = i.path.join("bin");
        match engine {
            DbEngine::MariaDb => {
                let p = Self::exe(&bin, "mariadb-admin");
                if p.exists() {
                    p
                } else {
                    Self::exe(&bin, "mysqladmin")
                }
            }
            DbEngine::MySql => Self::exe(&bin, "mysqladmin"),
        }
    }
    /// MariaDB's data directory initialiser (unix: scripts/, windows: bin/).
    pub fn mariadb_install_db(i: &Installed) -> Option<PathBuf> {
        let candidates = [
            i.path.join("scripts").join("mariadb-install-db"),
            i.path.join("scripts").join("mysql_install_db"),
            i.path.join("bin").join("mariadb-install-db.exe"),
            i.path.join("bin").join("mysql_install_db.exe"),
        ];
        candidates.into_iter().find(|p| p.exists())
    }
    /// Folder with `python3` / `python.exe` (the standalone build keeps the exe at the root on Windows).
    pub fn python_bin_dir(i: &Installed) -> PathBuf {
        if Platform::current().is_windows() {
            i.path.clone()
        } else {
            i.path.join("bin")
        }
    }
    pub fn postgres_bin_dir(i: &Installed) -> PathBuf {
        i.path.join("bin")
    }
    pub fn mongodb_bin_dir(i: &Installed) -> PathBuf {
        i.path.join("bin")
    }
    pub fn node_bin_dir(i: &Installed) -> PathBuf {
        if Platform::current().is_windows() {
            i.path.clone()
        } else {
            i.path.join("bin")
        }
    }
}

/// Directories to prepend to PATH for terminals and child processes.
/// The `bin` folder of the Node.js selected by `[node] default` (see `config::Node`).
pub fn node_bin_dir_for(reg: &Registry, selector: &str) -> Option<PathBuf> {
    let sel = selector.trim();
    if let Some((source, version)) = sel.split_once(':') {
        return crate::tools::external_node_versions()
            .into_iter()
            .find(|x| x.source == source && x.version == version)
            .map(|x| x.bin_dir);
    }
    if !sel.is_empty()
        && let Some(i) = reg.find(Kind::Node, sel)
    {
        return Some(Binaries::node_bin_dir(i));
    }
    if let Some(i) = reg.newest(Kind::Node) {
        return Some(Binaries::node_bin_dir(i));
    }
    // nothing installed by Veranda: the newest version nvm/fnm/volta manage
    let mut ext: Vec<_> = crate::tools::external_node_versions()
        .into_iter()
        .filter(|x| x.kind == "node")
        .collect();
    ext.sort_by_key(|x| std::cmp::Reverse(crate::manifest::parse_version(&x.version)));
    ext.into_iter().next().map(|x| x.bin_dir)
}

pub fn runtime_path(layout: &Layout, reg: &Registry, cfg: &crate::Config) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(i) = reg.active(Kind::Php, &cfg.php.version) {
        v.push(i.path.clone());
    }
    if let Some(dir) = node_bin_dir_for(reg, &cfg.node.default) {
        v.push(dir);
    }
    if let Some(i) = reg.newest(Kind::Python) {
        v.push(Binaries::python_bin_dir(i));
    }
    if let Some(i) = reg.active(Kind::Postgres, &cfg.pg.version) {
        v.push(Binaries::postgres_bin_dir(i));
    }
    if let Some(i) = reg.active(Kind::MongoDb, &cfg.mongodb.version) {
        v.push(Binaries::mongodb_bin_dir(i));
    }
    let db_kind = match cfg.db.engine {
        DbEngine::MariaDb => Kind::MariaDb,
        DbEngine::MySql => Kind::MySql,
    };
    if let Some(i) = reg.active(db_kind, &cfg.db.version) {
        v.push(i.path.join("bin"));
    }
    let web_kind = match cfg.web.server {
        WebServer::Nginx => Kind::Nginx,
        WebServer::Apache => Kind::Apache,
    };
    if let Some(i) = reg.active(web_kind, &cfg.web.version) {
        v.push(if web_kind == Kind::Apache {
            i.path.join("bin")
        } else {
            i.path.clone()
        });
    }
    v.push(layout.usr().join("bin"));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_keeps_unknown_entries_and_recovers_folders() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        // a folder that exists but is missing from the file
        let php = layout.bin_kind("php").join("8.5.8");
        std::fs::create_dir_all(&php).unwrap();
        std::fs::write(php.join("php"), "x").unwrap();
        // an entry written by a future build with a kind we do not know
        let future = layout.bin_kind("rustfs").join("1.0.0");
        std::fs::create_dir_all(&future).unwrap();
        // serde builds the JSON so Windows paths (backslashes) are escaped correctly
        let file = serde_json::json!({ "items": [{
            "kind": "rustfs", "version": "1.0.0", "path": future.display().to_string(),
            "source": "x", "installed_at": "now"
        }] });
        std::fs::write(
            layout.installed_file(),
            serde_json::to_string(&file).unwrap(),
        )
        .unwrap();
        let reg = Registry::load(&layout).unwrap();
        assert_eq!(reg.items.len(), 1);
        assert_eq!(reg.items[0].kind, Kind::Php);
        assert_eq!(reg.items[0].source, "recovered");
        // saved again: both the recovered php and the foreign rustfs entry survive
        let text = std::fs::read_to_string(layout.installed_file()).unwrap();
        assert!(text.contains("rustfs") && text.contains("8.5.8"));
        let again = Registry::load(&layout).unwrap();
        assert_eq!(again.items.len(), 1);
        assert!(text.matches("\"kind\"").count() == 2);
        let _ = again;
    }

    #[test]
    fn file_names() {
        assert_eq!(url_file_name("https://x/y/php.zip?x=1").unwrap(), "php.zip");
        assert!(url_file_name("https://x/").is_err());
    }
}
