//! Virtual hosts: auto-discovered project folders plus manual entries in `etc/sites.toml`.

use crate::config::Config;
use crate::fsutil;
use crate::paths::Layout;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Site {
    /// Project name (folder name for auto sites).
    pub name: String,
    pub hostname: String,
    /// Project folder.
    pub path: PathBuf,
    /// Folder actually served (`path` or `path/public`).
    pub root: PathBuf,
    pub auto: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManualSites {
    #[serde(default, rename = "site")]
    pub sites: Vec<ManualSite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualSite {
    pub hostname: String,
    pub path: PathBuf,
    /// Optional sub folder to serve, e.g. `public`.
    #[serde(default)]
    pub public: String,
}

pub fn load_manual(layout: &Layout) -> Result<ManualSites> {
    match std::fs::read_to_string(layout.sites_file()) {
        Ok(t) => Ok(toml::from_str(&t)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ManualSites::default()),
        Err(e) => Err(e.into()),
    }
}

pub fn save_manual(layout: &Layout, sites: &ManualSites) -> Result<()> {
    let text = toml::to_string_pretty(sites)?;
    fsutil::write_atomic(
        &layout.sites_file(),
        format!("# Manual sites. Auto sites come from the document root.\n\n{text}").as_bytes(),
    )
}

pub fn add_manual(
    layout: &Layout,
    cfg: &Config,
    hostname: &str,
    path: &Path,
    public: &str,
) -> Result<()> {
    let hostname = hostname.trim().to_ascii_lowercase();
    if !crate::hosts::is_valid_hostname(&hostname, &cfg.general.tld) {
        bail!(
            "hostname must be lowercase and end with .{}",
            cfg.general.tld
        );
    }
    if !path.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    let mut m = load_manual(layout)?;
    m.sites.retain(|s| s.hostname != hostname);
    m.sites.push(ManualSite {
        hostname,
        path: path.to_path_buf(),
        public: public.trim().trim_matches(['/', '\\']).to_string(),
    });
    save_manual(layout, &m)
}

pub fn remove_manual(layout: &Layout, hostname: &str) -> Result<bool> {
    let mut m = load_manual(layout)?;
    let before = m.sites.len();
    m.sites
        .retain(|s| s.hostname != hostname.trim().to_ascii_lowercase());
    save_manual(layout, &m)?;
    Ok(m.sites.len() != before)
}

/// Discover every site: folders in the document root (when enabled) + manual sites.
pub fn discover(layout: &Layout, cfg: &Config) -> Result<Vec<Site>> {
    let mut sites = Vec::new();
    if cfg.general.auto_vhosts {
        let docroot = cfg.document_root(layout);
        if let Ok(rd) = std::fs::read_dir(&docroot) {
            let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
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
                let name = fsutil::slugify(raw);
                if name.is_empty() {
                    continue;
                }
                let hostname = cfg.hostname_for(&name);
                if !crate::hosts::is_valid_hostname(&hostname, &cfg.general.tld) {
                    continue;
                }
                let root = serving_root(&p, cfg.general.smart_public_dir);
                sites.push(Site {
                    name,
                    hostname,
                    path: p.clone(),
                    root,
                    auto: true,
                });
            }
        }
    }
    for m in load_manual(layout)?.sites {
        if !m.path.is_dir() {
            continue;
        }
        let root = if m.public.is_empty() {
            serving_root(&m.path, cfg.general.smart_public_dir)
        } else {
            m.path.join(&m.public)
        };
        let name = m
            .hostname
            .split('.')
            .next()
            .unwrap_or(&m.hostname)
            .to_string();
        sites.retain(|s| s.hostname != m.hostname);
        sites.push(Site {
            name,
            hostname: m.hostname,
            path: m.path,
            root,
            auto: false,
        });
    }
    Ok(sites)
}

/// `path/public` if it has an index file (Laravel, Symfony, Slim...), else `path`.
pub fn serving_root(path: &Path, smart: bool) -> PathBuf {
    if smart {
        let public = path.join("public");
        if public.join("index.php").is_file() || public.join("index.html").is_file() {
            return public;
        }
    }
    path.to_path_buf()
}

pub fn hostnames(sites: &[Site]) -> Vec<String> {
    sites.iter().map(|s| s.hostname.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_folders() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        let cfg = Config::default();
        std::fs::create_dir_all(layout.www().join("My Blog")).unwrap();
        std::fs::create_dir_all(layout.www().join("shop/public")).unwrap();
        std::fs::write(layout.www().join("shop/public/index.php"), "<?php").unwrap();
        std::fs::create_dir_all(layout.www().join(".hidden")).unwrap();
        std::fs::write(layout.www().join("file.txt"), "x").unwrap();
        let sites = discover(&layout, &cfg).unwrap();
        let names: Vec<&str> = sites.iter().map(|s| s.hostname.as_str()).collect();
        assert_eq!(names, vec!["my-blog.test", "shop.test"]);
        assert!(sites[1].root.ends_with("public"));
        add_manual(&layout, &cfg, "custom.test", dir.path(), "").unwrap();
        let sites = discover(&layout, &cfg).unwrap();
        assert_eq!(sites.len(), 3);
        assert!(!sites[2].auto);
        assert!(remove_manual(&layout, "custom.test").unwrap());
    }
}
