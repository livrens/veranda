//! Quick project creation inside the document root.

use crate::Veranda;
use crate::download::{ProgressFn, client, download};
use crate::fsutil;
use crate::manifest::{ArchiveType, Checksum, Kind};
use crate::packages::{Binaries, Registry};
use crate::process;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Template {
    /// Empty folder with a starter index.php.
    Blank,
    /// Latest WordPress from wordpress.org.
    WordPress,
    /// `composer create-project laravel/laravel`.
    Laravel,
    /// `composer create-project symfony/skeleton`.
    Symfony,
    /// Any Packagist project template, e.g. `slim/slim-skeleton`.
    Composer { package: String },
    /// `git clone` of an https repository.
    Git { url: String },
}

pub const TEMPLATES: &[(&str, &str)] = &[
    ("blank", "Blank (index.php)"),
    ("wordpress", "WordPress (latest)"),
    ("laravel", "Laravel (composer)"),
    ("symfony", "Symfony (composer)"),
    ("composer", "Composer package (vendor/name)"),
    ("git", "Git repository (https)"),
];

fn is_packagist_name(s: &str) -> bool {
    let mut parts = s.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), None) => {
            let ok = |x: &str| {
                !x.is_empty()
                    && x.chars().all(|c| {
                        c.is_ascii_lowercase()
                            || c.is_ascii_digit()
                            || c == '-'
                            || c == '_'
                            || c == '.'
                    })
            };
            ok(a) && ok(b)
        }
        _ => false,
    }
}

/// Create a project named `name` under the document root. Returns the project path.
pub async fn create(
    app: &Veranda,
    name: &str,
    template: &Template,
    progress: Option<&ProgressFn>,
) -> Result<PathBuf> {
    let name = name.trim().to_ascii_lowercase();
    if !fsutil::is_safe_name(&name) {
        bail!("project name may only contain lowercase letters, digits, - and _");
    }
    let docroot = app.config.document_root(&app.layout);
    let dest = docroot.join(&name);
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }
    let reg = Registry::load(&app.layout)?;
    let path_entries = crate::packages::runtime_path(&app.layout, &reg, &app.config);
    match template {
        Template::Blank => {
            std::fs::create_dir_all(&dest)?;
            std::fs::write(
                dest.join("index.php"),
                format!("<?php\n// {name} - created by Veranda\nphpinfo();\n"),
            )?;
        }
        Template::WordPress => {
            let c = client()?;
            let archive = app.layout.downloads().join("wordpress-latest.tar.gz");
            download(
                &c,
                &app.layout,
                "https://wordpress.org/latest.tar.gz",
                &archive,
                &Checksum::Remote {
                    url: "https://wordpress.org/latest.tar.gz.sha1".into(),
                    name: "latest.tar.gz".into(),
                },
                progress,
            )
            .await
            .or_else(|_| Ok::<_, anyhow::Error>(String::new()))?;
            if !archive.is_file() {
                // sha1 listing is a bare hash without the name; retry with TOFU (still https + ledger).
                download(
                    &c,
                    &app.layout,
                    "https://wordpress.org/latest.tar.gz",
                    &archive,
                    &Checksum::Tofu,
                    progress,
                )
                .await?;
            }
            crate::archive::extract(&archive, ArchiveType::TarGz, &dest, 1)?;
            let _ = std::fs::remove_file(&archive);
        }
        Template::Laravel => {
            composer_create(app, &reg, &path_entries, "laravel/laravel", &dest).await?
        }
        Template::Symfony => {
            composer_create(app, &reg, &path_entries, "symfony/skeleton", &dest).await?
        }
        Template::Composer { package } => {
            if !is_packagist_name(package) {
                bail!("composer package must look like vendor/name");
            }
            composer_create(app, &reg, &path_entries, package, &dest).await?
        }
        Template::Git { url } => {
            if !url.starts_with("https://") || url.contains(['\'', '"', ' ']) {
                bail!("git url must be https");
            }
            let git = which::which("git").context("git is not installed")?;
            let mut cmd = process::command(&git, &path_entries);
            cmd.args(["clone", "--depth", "1", "--", url])
                .arg(&dest)
                .current_dir(&docroot);
            process::run_capture(cmd)?;
        }
    }
    Ok(dest)
}

async fn composer_create(
    app: &Veranda,
    reg: &Registry,
    path_entries: &[PathBuf],
    package: &str,
    dest: &std::path::Path,
) -> Result<()> {
    let php = reg
        .active(Kind::Php, &app.config.php.version)
        .ok_or_else(|| crate::services::not_installed(Kind::Php))?;
    // Composer comes from the system (`composer` or `composer.phar` on PATH); Veranda's own PHP
    // runs it so the project gets the same PHP version it will be served with.
    let composer = which::which("composer")
        .or_else(|_| which::which("composer.phar"))
        .map_err(|_| anyhow::anyhow!("Composer is not installed; get it from getcomposer.org and make sure `composer` is on PATH"))?;
    let mut cmd = process::command(&Binaries::php(php), path_entries);
    cmd.arg(&composer)
        .args([
            "create-project",
            "--prefer-dist",
            "--no-interaction",
            "--no-progress",
            package,
        ])
        .arg(dest)
        .env("COMPOSER_HOME", app.layout.data().join("composer"))
        .env("COMPOSER_NO_INTERACTION", "1")
        .current_dir(dest.parent().unwrap());
    let out = tokio::task::spawn_blocking(move || process::run_capture(cmd)).await??;
    tracing::info!("composer: {}", out.lines().last().unwrap_or(""));
    Ok(())
}

/// Delete a project folder inside the document root (and nothing else).
pub fn delete(app: &Veranda, name: &str) -> Result<()> {
    if !fsutil::is_safe_name(name) {
        bail!("invalid project name");
    }
    let docroot = app.config.document_root(&app.layout);
    let dest = docroot.join(name);
    if !dest.is_dir() || !dest.starts_with(&docroot) {
        bail!("{} is not a project folder", dest.display());
    }
    fsutil::remove_dir_all_if_exists(&dest)
}
