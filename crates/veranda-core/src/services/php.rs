//! PHP FastCGI backend: php-fpm (Linux/macOS) or N x php-cgi.exe (Windows).

use super::{Ctx, STOP_GRACE, ServiceId, ServiceStatus, render};
use crate::manifest::Kind;
use crate::packages::{Binaries, Installed};
use crate::paths::Layout;
use crate::process;
use anyhow::{Context, Result};
use std::path::PathBuf;

const INI_TPL: &str = include_str!("../templates/php.ini.tpl");
const FPM_TPL: &str = include_str!("../templates/php-fpm.conf.tpl");

/// Windows extensions shipped in the official zip that we enable by default.
const WIN_EXTENSIONS: &[&str] = &[
    "bz2",
    "curl",
    "fileinfo",
    "gd",
    "gettext",
    "gmp",
    "intl",
    "mbstring",
    "exif",
    "mysqli",
    "openssl",
    "pdo_mysql",
    "pdo_pgsql",
    "pdo_sqlite",
    "pgsql",
    "shmop",
    "soap",
    "sockets",
    "sodium",
    "sqlite3",
    "xsl",
    "zip",
];

pub fn active<'a>(ctx: &Ctx<'a>) -> Option<&'a Installed> {
    ctx.reg.active(Kind::Php, &ctx.app.config.php.version)
}

/// `host:port` list of FastCGI upstreams for the web server.
pub fn upstreams(ctx: &Ctx) -> Vec<String> {
    let base = ctx.app.config.php.fcgi_port;
    if ctx.app.platform().is_windows() {
        (0..ctx.app.config.php.workers)
            .map(|i| format!("127.0.0.1:{}", base + i))
            .collect()
    } else {
        vec![format!("127.0.0.1:{base}")]
    }
}

fn worker_names(ctx: &Ctx) -> Vec<String> {
    if ctx.app.platform().is_windows() {
        (0..ctx.app.config.php.workers)
            .map(|i| format!("php-{i}"))
            .collect()
    } else {
        vec!["php".into()]
    }
}

pub fn ini_dir(layout: &Layout) -> PathBuf {
    layout.etc().join("php")
}

pub fn ini_path(layout: &Layout) -> PathBuf {
    ini_dir(layout).join("php.ini")
}

/// Write php.ini + php-fpm.conf for the active version.
pub fn prepare(ctx: &Ctx) -> Result<()> {
    let Some(php) = active(ctx) else {
        return Ok(());
    };
    let layout = &ctx.app.layout;
    let cfg = &ctx.app.config;
    let dir = ini_dir(layout);
    std::fs::create_dir_all(&dir)?;
    let session_dir = layout.tmp().join("php-sessions");
    std::fs::create_dir_all(&session_dir)?;
    let extra = dir.join("conf.d").join("99-user.ini");
    if !extra.exists() {
        std::fs::create_dir_all(extra.parent().unwrap())?;
        std::fs::write(
            &extra,
            "; Your php.ini overrides. This file is never regenerated.\n",
        )?;
    }
    let (extension_dir, extensions) = if ctx.app.platform().is_windows() {
        let ext_dir = php.path.join("ext");
        let mut lines = String::new();
        for e in WIN_EXTENSIONS {
            if ext_dir.join(format!("php_{e}.dll")).is_file() {
                lines.push_str(&format!("extension={e}\n"));
            }
        }
        if ext_dir.join("php_opcache.dll").is_file() {
            lines.push_str("zend_extension=opcache\n");
        }
        if cfg.php.xdebug && ext_dir.join("php_xdebug.dll").is_file() {
            lines.push_str("zend_extension=xdebug\nxdebug.mode=debug,develop\nxdebug.client_host=127.0.0.1\nxdebug.start_with_request=trigger\n");
        }
        (
            format!("extension_dir = \"{}\"", Layout::slashes(&ext_dir)),
            lines,
        )
    } else {
        // static-php builds compile extensions in; nothing to load.
        (
            String::new(),
            "; extensions are compiled into this static build (php -m)".into(),
        )
    };
    // php-cgi.exe workers are separate processes: one OPcache per worker (`opcache.cache_id`
    // is passed per process in `start`), a file cache as fallback and no JIT - that combination
    // is what avoids "Unable to reattach to base address" crashes behind Apache/Nginx.
    let opcache_platform = if ctx.app.platform().is_windows() {
        let cache_dir = layout.tmp().join("php-opcache");
        std::fs::create_dir_all(&cache_dir)?;
        format!(
            "; php-cgi on Windows: private cache per worker, no JIT\nopcache.memory_consumption = 96\nopcache.jit = off\nopcache.jit_buffer_size = 0\nopcache.file_cache = \"{}\"\nopcache.file_cache_fallback = 1\n",
            Layout::slashes(&cache_dir)
        )
    } else {
        String::new()
    };
    let ini = render(
        INI_TPL,
        &[
            ("OPCACHE_PLATFORM", opcache_platform),
            ("EXTRA_INI", Layout::slashes(&extra)),
            ("MAX_EXECUTION_TIME", cfg.php.max_execution_time.to_string()),
            ("MEMORY_LIMIT", cfg.php.memory_limit.clone()),
            ("UPLOAD_MAX", cfg.php.upload_max_filesize.clone()),
            ("TMP_DIR", Layout::slashes(&layout.tmp())),
            ("LOG_DIR", Layout::slashes(&layout.logs())),
            ("EXTENSION_DIR", extension_dir),
            ("TIMEZONE", cfg.php.timezone.clone()),
            ("SESSION_DIR", Layout::slashes(&session_dir)),
            ("PLATFORM", ctx.app.platform().to_string()),
            ("EXTENSIONS", extensions),
            ("EXTRA_INCLUDE", String::new()),
        ],
    );
    // Append the user file verbatim (php.ini has no include directive).
    let user = std::fs::read_to_string(&extra).unwrap_or_default();
    crate::fsutil::write_if_changed(&ini_path(layout), &format!("{ini}\n{user}"))?;

    if !ctx.app.platform().is_windows() {
        let path = process::env_path(&ctx.path_entries());
        let fpm = render(
            FPM_TPL,
            &[
                ("RUN_DIR", Layout::slashes(&layout.run())),
                ("LOG_DIR", Layout::slashes(&layout.logs())),
                ("PORT", cfg.php.fcgi_port.to_string()),
                ("MAX_CHILDREN", (cfg.php.workers * 3).max(5).to_string()),
                ("MAX_EXECUTION_TIME", cfg.php.max_execution_time.to_string()),
                ("PATH", path.to_string_lossy().into_owned()),
            ],
        );
        crate::fsutil::write_if_changed(&dir.join("php-fpm.conf"), &fpm)?;
    }
    Ok(())
}

pub fn status(ctx: &Ctx) -> ServiceStatus {
    let php = active(ctx);
    let pids: Vec<u32> = worker_names(ctx)
        .iter()
        .filter_map(|n| process::running_pid(&ctx.app.layout, n))
        .collect();
    let ports: Vec<u16> = upstreams(ctx)
        .iter()
        .filter_map(|u| u.rsplit(':').next()?.parse().ok())
        .collect();
    ServiceStatus {
        id: ServiceId::Php,
        name: "PHP".into(),
        enabled: true,
        installed: php.is_some(),
        version: php.map(|p| p.version.clone()).unwrap_or_default(),
        running: !pids.is_empty(),
        pids,
        ports,
        message: if php.is_none() {
            "not installed".into()
        } else {
            String::new()
        },
    }
}

pub fn start(ctx: &Ctx) -> Result<()> {
    let php = active(ctx).ok_or_else(|| super::not_installed(Kind::Php))?;
    prepare(ctx)?;
    let layout = &ctx.app.layout;
    let cfg = &ctx.app.config;
    let fcgi = Binaries::php_fcgi(php);
    if !fcgi.is_file() {
        anyhow::bail!("{} not found in PHP {}", fcgi.display(), php.version);
    }
    let names = worker_names(ctx);
    if names
        .iter()
        .all(|n| process::running_pid(layout, n).is_some())
    {
        return Ok(());
    }
    stop(ctx)?;
    let path = ctx.path_entries();
    if ctx.app.platform().is_windows() {
        for (i, name) in names.iter().enumerate() {
            let port = cfg.php.fcgi_port + i as u16;
            super::ensure_port_free(port, "PHP FastCGI")?;
            let mut cmd = process::command(&fcgi, &path);
            cmd.args(["-b", &format!("127.0.0.1:{port}")])
                .args(["-d", &format!("opcache.cache_id=veranda-{i}")])
                .env("PHP_FCGI_MAX_REQUESTS", "0")
                .env("PHPRC", ini_dir(layout))
                .current_dir(&php.path);
            process::spawn_detached(layout, name, cmd)?;
        }
    } else {
        super::ensure_port_free(cfg.php.fcgi_port, "PHP-FPM")?;
        let mut cmd = process::command(&fcgi, &path);
        cmd.arg("-F")
            .arg("-y")
            .arg(ini_dir(layout).join("php-fpm.conf"))
            .arg("-c")
            .arg(ini_path(layout))
            .current_dir(&php.path);
        process::spawn_detached(layout, "php", cmd)?;
    }
    if !process::wait_for_port(cfg.php.fcgi_port, std::time::Duration::from_secs(10)) {
        anyhow::bail!(
            "PHP did not start listening on port {}. See logs/php*.log",
            cfg.php.fcgi_port
        );
    }
    Ok(())
}

pub fn stop(ctx: &Ctx) -> Result<()> {
    for name in worker_names(ctx) {
        process::stop_named(&ctx.app.layout, &name, STOP_GRACE)
            .with_context(|| format!("stopping {name}"))?;
    }
    Ok(())
}
