//! Web server front-end: Nginx (default) or Apache HTTPD.

use super::{Ctx, STOP_GRACE, ServiceId, ServiceStatus, render};
use crate::config::WebServer;
use crate::manifest::Kind;
use crate::packages::{Binaries, Installed};
use crate::paths::Layout;
use crate::process;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

const PID: &str = "web";

fn kind(ctx: &Ctx) -> Kind {
    match ctx.app.config.web.server {
        WebServer::Nginx => Kind::Nginx,
        WebServer::Apache => Kind::Apache,
    }
}

/// Installed package, or a system binary on Linux/macOS for nginx/apache.
pub enum Server {
    Installed(Installed),
    System(PathBuf),
}

impl Server {
    pub fn version(&self) -> String {
        match self {
            Server::Installed(i) => i.version.clone(),
            Server::System(p) => format!("system ({})", p.display()),
        }
    }
    pub fn binary(&self, ctx: &Ctx) -> PathBuf {
        match self {
            Server::Installed(i) => match ctx.app.config.web.server {
                WebServer::Nginx => Binaries::nginx(i),
                WebServer::Apache => Binaries::httpd(i),
            },
            Server::System(p) => p.clone(),
        }
    }
}

pub fn active(ctx: &Ctx) -> Option<Server> {
    if let Some(i) = ctx.reg.active(kind(ctx), &ctx.app.config.web.version) {
        return Some(Server::Installed(i.clone()));
    }
    if !ctx.app.platform().is_windows() {
        let names: &[&str] = match ctx.app.config.web.server {
            WebServer::Nginx => &["nginx"],
            WebServer::Apache => &["httpd", "apache2"],
        };
        for n in names {
            if let Ok(p) = which::which(n) {
                return Some(Server::System(p));
            }
            for dir in [
                "/usr/sbin",
                "/usr/local/sbin",
                "/opt/homebrew/bin",
                "/opt/homebrew/opt/httpd/bin",
                "/usr/local/bin",
            ] {
                let p = PathBuf::from(dir).join(n);
                if p.is_file() {
                    return Some(Server::System(p));
                }
            }
        }
    }
    None
}

pub fn conf_dir(layout: &Layout, server: WebServer) -> PathBuf {
    layout.etc().join(server.id())
}

pub fn status(ctx: &Ctx) -> ServiceStatus {
    let cfg = &ctx.app.config;
    let server = active(ctx);
    let pid = process::running_pid(&ctx.app.layout, PID);
    let mut ports = vec![cfg.web.http_port];
    if cfg.web.ssl {
        ports.push(cfg.web.https_port);
    }
    let name = match cfg.web.server {
        WebServer::Nginx => "Nginx",
        WebServer::Apache => "Apache",
    };
    ServiceStatus {
        id: ServiceId::Web,
        name: name.into(),
        enabled: true,
        installed: server.is_some(),
        version: server.as_ref().map(|s| s.version()).unwrap_or_default(),
        running: pid.is_some(),
        pids: pid.into_iter().collect(),
        ports,
        message: if server.is_none() {
            "not installed".into()
        } else {
            String::new()
        },
    }
}

pub fn prepare(ctx: &Ctx) -> Result<()> {
    let layout = &ctx.app.layout;
    let dir = conf_dir(layout, ctx.app.config.web.server);
    std::fs::create_dir_all(&dir)?;
    let text = match ctx.app.config.web.server {
        WebServer::Nginx => nginx_conf(ctx)?,
        WebServer::Apache => httpd_conf(ctx)?,
    };
    let file = config_file(ctx);
    crate::fsutil::write_if_changed(&file, &text)?;
    Ok(())
}

pub fn config_file(ctx: &Ctx) -> PathBuf {
    let dir = conf_dir(&ctx.app.layout, ctx.app.config.web.server);
    match ctx.app.config.web.server {
        WebServer::Nginx => dir.join("nginx.conf"),
        WebServer::Apache => dir.join("httpd.conf"),
    }
}

fn tool_apps(ctx: &Ctx) -> Vec<(String, PathBuf)> {
    let mut v = Vec::new();
    if let Some(a) = ctx.reg.newest(Kind::Adminer) {
        v.push(("adminer".to_string(), a.path.clone()));
    }
    if let Some(p) = ctx.reg.newest(Kind::PhpMyAdmin) {
        v.push(("phpmyadmin".to_string(), p.path.clone()));
    }
    v
}

// ------------------------------------------------------------------ Nginx

const NGINX_TPL: &str = include_str!("../templates/nginx.conf.tpl");
const NGINX_SERVER_TPL: &str = include_str!("../templates/nginx-server.tpl");
const MIME_TYPES: &str = include_str!("../templates/mime.types");

fn nginx_conf(ctx: &Ctx) -> Result<String> {
    let cfg = &ctx.app.config;
    let layout = &ctx.app.layout;
    let dir = conf_dir(layout, WebServer::Nginx);
    for sub in ["logs", "temp"] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    // mime.types: prefer the one shipped with the server, else our compact list.
    let mime = dir.join("mime.types");
    let shipped = match active(ctx) {
        Some(Server::Installed(i)) => Some(i.path.join("conf").join("mime.types")),
        Some(Server::System(_)) => Some(PathBuf::from("/etc/nginx/mime.types")),
        None => None,
    };
    match shipped.filter(|p| p.is_file()) {
        Some(p) => {
            std::fs::copy(p, &mime)?;
        }
        None => std::fs::write(&mime, MIME_TYPES)?,
    }
    let tls = crate::tls::paths(layout);
    let php_upstreams = super::php::upstreams(ctx)
        .iter()
        .map(|u| format!("        server {u} max_fails=1 fail_timeout=2s;"))
        .collect::<Vec<_>>()
        .join("\n");
    let ssl_listen = |default: &str| {
        if cfg.web.ssl {
            format!(
                "        listen 127.0.0.1:{p} ssl{default};\n        listen [::1]:{p} ssl{default};\n        http2 on;\n        ssl_certificate \"{c}\";\n        ssl_certificate_key \"{k}\";",
                p = cfg.web.https_port,
                c = Layout::slashes(&tls.sites_crt),
                k = Layout::slashes(&tls.sites_key)
            )
        } else {
            String::new()
        }
    };
    let autoindex = if cfg.web.autoindex {
        "autoindex on;"
    } else {
        ""
    };
    let mut tools = String::new();
    for (name, path) in tool_apps(ctx) {
        tools.push_str(&format!(
            "        location ^~ /{name}/ {{\n            alias \"{p}/\";\n            index index.php;\n            location ~ \\.php$ {{\n                fastcgi_pass veranda_php;\n                fastcgi_param SCRIPT_FILENAME $request_filename;\n                include fastcgi_params_veranda;\n            }}\n        }}\n",
            p = Layout::slashes(&path)
        ));
    }
    if !tools.is_empty() {
        // Minimal param set for the alias blocks.
        std::fs::write(
            dir.join("fastcgi_params_veranda"),
            "fastcgi_param QUERY_STRING $query_string;\nfastcgi_param REQUEST_METHOD $request_method;\nfastcgi_param CONTENT_TYPE $content_type;\nfastcgi_param CONTENT_LENGTH $content_length;\nfastcgi_param SCRIPT_NAME $fastcgi_script_name;\nfastcgi_param REQUEST_URI $request_uri;\nfastcgi_param DOCUMENT_URI $document_uri;\nfastcgi_param DOCUMENT_ROOT $document_root;\nfastcgi_param SERVER_PROTOCOL $server_protocol;\nfastcgi_param REMOTE_ADDR $remote_addr;\nfastcgi_param SERVER_NAME $server_name;\nfastcgi_param SERVER_PORT $server_port;\nfastcgi_param REDIRECT_STATUS 200;\n",
        )?;
    }
    let server_block = |names: String, root: String, default: &str, tools: &str| {
        render(
            NGINX_SERVER_TPL,
            &[
                ("HTTP_PORT", cfg.web.http_port.to_string()),
                ("DEFAULT", default.to_string()),
                ("SSL_LISTEN", ssl_listen(default)),
                ("SERVER_NAMES", names),
                ("ROOT", root),
                ("TOOLS", tools.to_string()),
                ("AUTOINDEX", autoindex.to_string()),
                (
                    "MAX_EXECUTION_TIME",
                    (cfg.php.max_execution_time + 5).to_string(),
                ),
            ],
        )
    };
    let mut servers = String::new();
    servers.push_str(&server_block(
        "localhost 127.0.0.1".into(),
        Layout::slashes(&cfg.document_root(layout)),
        " default_server",
        &tools,
    ));
    for site in ctx.sites {
        servers.push_str(&server_block(
            format!("{h} *.{h}", h = site.hostname),
            Layout::slashes(&site.root),
            "",
            "",
        ));
    }
    Ok(render(
        NGINX_TPL,
        &[
            ("RUN_DIR", Layout::slashes(&layout.run())),
            ("LOG_DIR", Layout::slashes(&layout.logs())),
            ("TMP_DIR", Layout::slashes(&layout.tmp())),
            ("UPLOAD_MAX", cfg.php.upload_max_filesize.clone()),
            ("PHP_UPSTREAMS", php_upstreams),
            ("SERVERS", servers),
        ],
    ))
}

// ------------------------------------------------------------------ Apache

const HTTPD_TPL: &str = include_str!("../templates/httpd.conf.tpl");
const VHOST_TPL: &str = include_str!("../templates/httpd-vhost.tpl");

const APACHE_MODULES: &[(&str, &str)] = &[
    ("mpm_event_module", "mod_mpm_event.so"),
    ("unixd_module", "mod_unixd.so"),
    ("authz_core_module", "mod_authz_core.so"),
    ("authz_host_module", "mod_authz_host.so"),
    ("dir_module", "mod_dir.so"),
    ("mime_module", "mod_mime.so"),
    ("log_config_module", "mod_log_config.so"),
    ("alias_module", "mod_alias.so"),
    ("rewrite_module", "mod_rewrite.so"),
    ("headers_module", "mod_headers.so"),
    ("setenvif_module", "mod_setenvif.so"),
    ("autoindex_module", "mod_autoindex.so"),
    ("env_module", "mod_env.so"),
    ("proxy_module", "mod_proxy.so"),
    ("proxy_fcgi_module", "mod_proxy_fcgi.so"),
    ("proxy_balancer_module", "mod_proxy_balancer.so"),
    ("slotmem_shm_module", "mod_slotmem_shm.so"),
    ("lbmethod_byrequests_module", "mod_lbmethod_byrequests.so"),
    ("ssl_module", "mod_ssl.so"),
    ("socache_shmcb_module", "mod_socache_shmcb.so"),
    ("deflate_module", "mod_deflate.so"),
    ("filter_module", "mod_filter.so"),
    ("negotiation_module", "mod_negotiation.so"),
];

fn apache_module_dir(server: &Server) -> Option<PathBuf> {
    match server {
        Server::Installed(i) => Some(i.path.join("modules")),
        Server::System(_) => [
            "/usr/lib/apache2/modules",
            "/usr/lib64/httpd/modules",
            "/usr/lib/httpd/modules",
            "/opt/homebrew/opt/httpd/lib/httpd/modules",
            "/usr/local/opt/httpd/lib/httpd/modules",
            "/usr/libexec/apache2",
        ]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_dir()),
    }
}

fn httpd_conf(ctx: &Ctx) -> Result<String> {
    let cfg = &ctx.app.config;
    let layout = &ctx.app.layout;
    let server = active(ctx).ok_or_else(|| super::not_installed(Kind::Apache))?;
    let server_root = match &server {
        Server::Installed(i) => i.path.clone(),
        Server::System(_) => conf_dir(layout, WebServer::Apache),
    };
    let modules_dir =
        apache_module_dir(&server).context("cannot find the Apache modules directory")?;
    let mut modules = String::new();
    for (name, file) in APACHE_MODULES {
        let p = modules_dir.join(file);
        if p.is_file() {
            modules.push_str(&format!("LoadModule {name} \"{}\"\n", Layout::slashes(&p)));
        }
    }
    let mime_types = ["conf/mime.types", "mime.types"]
        .iter()
        .map(|r| server_root.join(r))
        .chain(["/etc/mime.types", "/etc/apache2/mime.types", "/etc/httpd/conf/mime.types", "/opt/homebrew/etc/httpd/mime.types"].iter().map(PathBuf::from))
        .find(|p| p.is_file())
        .map(|p| Layout::slashes(&p))
        .unwrap_or_else(|| {
            let p = conf_dir(layout, WebServer::Apache).join("mime.types");
            let _ = std::fs::write(&p, "text/html html htm\ntext/css css\ntext/javascript js mjs\napplication/json json\nimage/png png\nimage/jpeg jpg jpeg\nimage/svg+xml svg\nimage/gif gif\nimage/webp webp\nimage/x-icon ico\nfont/woff2 woff2\napplication/pdf pdf\ntext/plain txt\n");
            Layout::slashes(&p)
        });
    let tls = crate::tls::paths(layout);
    let php_members = super::php::upstreams(ctx)
        .iter()
        .map(|u| format!("    BalancerMember \"fcgi://{u}\""))
        .collect::<Vec<_>>()
        .join("\n");
    let ssl_listen = if cfg.web.ssl {
        format!(
            "Listen 127.0.0.1:{p}\nListen [::1]:{p}",
            p = cfg.web.https_port
        )
    } else {
        String::new()
    };
    let unix_user = if ctx.app.platform().is_windows() {
        String::new()
    } else {
        "# runs as the current user; no User/Group directive needed".into()
    };
    let mutex = if ctx.app.platform().is_windows() {
        String::new()
    } else {
        format!("Mutex default:{}", Layout::slashes(&layout.run()))
    };
    // Apache appends the request's file name to the SetHandler URL as-is. On Windows that name
    // starts with a drive letter, so without a trailing slash the balancer name would read
    // "verandaphpC:" and every PHP request would fail with "No protocol handler was valid".
    // php-cgi.exe (unlike PHP-FPM) also cannot strip the "proxy:fcgi://host:port/" prefix
    // mod_proxy_fcgi puts in SCRIPT_FILENAME, so it is rewritten to the plain path here.
    let (php_handler, fcgi_env) = if ctx.app.platform().is_windows() {
        (
            "proxy:balancer://verandaphp/",
            "<IfModule proxy_fcgi_module>\n    ProxyFCGISetEnvIf \"reqenv('SCRIPT_FILENAME') =~ m#^proxy:[a-z]+://[^/]+/+(.+)$#\" SCRIPT_FILENAME \"%{unescape:$1}\"\n</IfModule>",
        )
    } else {
        ("proxy:balancer://verandaphp", "")
    };
    let options = if cfg.web.autoindex {
        "Indexes"
    } else {
        "-Indexes"
    };
    let mut tools = String::new();
    for (name, path) in tool_apps(ctx) {
        tools.push_str(&format!(
            "    Alias /{name} \"{p}\"\n    <Directory \"{p}\">\n        Options FollowSymLinks\n        AllowOverride All\n        Require local\n    </Directory>\n",
            p = Layout::slashes(&path)
        ));
    }
    let vhost = |name: String, alias: String, root: String, tools: &str| {
        let mut v = render(
            VHOST_TPL,
            &[
                ("PORT", cfg.web.http_port.to_string()),
                ("SERVER_NAME", name.clone()),
                ("SERVER_ALIAS", alias.clone()),
                ("ROOT", root.clone()),
                ("OPTIONS", options.to_string()),
                ("TOOLS", tools.to_string()),
                ("SSL", String::new()),
            ],
        );
        if cfg.web.ssl {
            v.push_str(&render(
                VHOST_TPL,
                &[
                    ("PORT", cfg.web.https_port.to_string()),
                    ("SERVER_NAME", name),
                    ("SERVER_ALIAS", alias),
                    ("ROOT", root),
                    ("OPTIONS", options.to_string()),
                    ("TOOLS", tools.to_string()),
                    (
                        "SSL",
                        format!(
                            "    SSLEngine on\n    SSLCertificateFile \"{}\"\n    SSLCertificateKeyFile \"{}\"",
                            Layout::slashes(&tls.sites_crt),
                            Layout::slashes(&tls.sites_key)
                        ),
                    ),
                ],
            ));
        }
        v
    };
    let mut vhosts = vhost(
        "localhost".into(),
        "127.0.0.1".into(),
        Layout::slashes(&cfg.document_root(layout)),
        &tools,
    );
    for site in ctx.sites {
        vhosts.push_str(&vhost(
            site.hostname.clone(),
            format!("*.{}", site.hostname),
            Layout::slashes(&site.root),
            "",
        ));
    }
    Ok(render(
        HTTPD_TPL,
        &[
            ("SERVER_ROOT", Layout::slashes(&server_root)),
            ("HTTP_PORT", cfg.web.http_port.to_string()),
            ("SSL_LISTEN", ssl_listen),
            ("RUN_DIR", Layout::slashes(&layout.run())),
            ("LOG_DIR", Layout::slashes(&layout.logs())),
            ("MIME_TYPES", mime_types),
            ("UNIX_USER", unix_user),
            ("MODULES", modules),
            ("MUTEX", mutex),
            ("PHP_HANDLER", php_handler.to_string()),
            ("FCGI_ENV", fcgi_env.to_string()),
            ("PHP_MEMBERS", php_members),
            (
                "MAX_EXECUTION_TIME",
                (cfg.php.max_execution_time + 5).to_string(),
            ),
            ("VHOSTS", vhosts),
        ],
    ))
}

// ------------------------------------------------------------------ lifecycle

pub fn start(ctx: &Ctx) -> Result<()> {
    let cfg = &ctx.app.config;
    let layout = &ctx.app.layout;
    let server = active(ctx).ok_or_else(|| super::not_installed(kind(ctx)))?;
    prepare(ctx)?;
    if process::running_pid(layout, PID).is_some() {
        return reload(ctx);
    }
    process::clear_pid(layout, PID);
    let bin = server.binary(ctx);
    if !bin.is_file() {
        bail!("web server binary not found: {}", bin.display());
    }
    super::ensure_port_free_for(cfg.web.http_port, "the web server", &bin)?;
    if cfg.web.ssl {
        super::ensure_port_free_for(cfg.web.https_port, "the web server (https)", &bin)?;
    }
    let conf = config_file(ctx);
    let mut cmd = process::command(&bin, &ctx.path_entries());
    match cfg.web.server {
        WebServer::Nginx => {
            let prefix = conf_dir(layout, WebServer::Nginx);
            cmd.arg("-p").arg(&prefix).arg("-c").arg(&conf);
            if !ctx.app.platform().is_windows() {
                cmd.args(["-g", "daemon off;"]);
            }
            cmd.current_dir(&prefix);
        }
        WebServer::Apache => {
            let server_root = match &server {
                Server::Installed(i) => i.path.clone(),
                Server::System(_) => conf_dir(layout, WebServer::Apache),
            };
            // mod_slotmem_shm refuses to start when a .shm file from an unclean exit is still
            // there ("balancer slotmem_create failed", "Configuration Failed").
            remove_stale_shm(&layout.run());
            remove_stale_shm(&server_root.join("logs"));
            cmd.arg("-f").arg(&conf).arg("-d").arg(&server_root);
            if !ctx.app.platform().is_windows() {
                cmd.args(["-D", "FOREGROUND"]);
            }
            cmd.current_dir(&server_root);
        }
    }
    process::spawn_detached(layout, PID, cmd)?;
    if !process::wait_for_port(cfg.web.http_port, std::time::Duration::from_secs(10)) {
        if cfg!(target_os = "linux")
            && cfg.web.http_port < 1024
            && !crate::tools::has_bind_capability(&bin)
        {
            return Err(super::PortPermission {
                port: cfg.web.http_port,
            }
            .into());
        }
        let hint = if cfg!(target_os = "linux") && cfg.web.http_port < 1024 {
            " On Linux, ports below 1024 need a one-time permission (veranda doctor --fix-ports) or use ports 8080/8443 in Settings."
        } else {
            ""
        };
        bail!(
            "web server did not start listening on port {}. See logs/web.log.{hint}",
            cfg.web.http_port
        );
    }
    Ok(())
}

fn remove_stale_shm(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with("slotmem-shm-") || name.starts_with("ssl_scache") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

pub fn stop(ctx: &Ctx) -> Result<()> {
    let layout = &ctx.app.layout;
    if let (WebServer::Apache, Some(server), Some(pid), true) = (
        ctx.app.config.web.server,
        active(ctx),
        process::running_pid(layout, PID),
        ctx.app.platform().is_windows(),
    ) {
        // Ask the parent httpd to shut down through its own signal event: a plain
        // TerminateProcess leaves the child serving requests and its shared-memory files on
        // disk, and the next start then fails until the child notices and exits.
        let server_root = match &server {
            Server::Installed(i) => i.path.clone(),
            Server::System(_) => conf_dir(layout, WebServer::Apache),
        };
        let mut cmd = crate::process::new_cmd(server.binary(ctx));
        cmd.arg("-f")
            .arg(config_file(ctx))
            .arg("-d")
            .arg(&server_root)
            // "-k shutdown" signals a console instance through its PidFile; "-k stop" is the
            // Windows-service variant and does nothing for one started by Veranda.
            .args(["-k", "shutdown"])
            .current_dir(&server_root);
        let _ = process::run_capture(cmd);
        let start = std::time::Instant::now();
        while process::is_pid_alive(pid) && start.elapsed() < STOP_GRACE {
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }
    if let (WebServer::Nginx, Some(server), Some(_)) = (
        ctx.app.config.web.server,
        active(ctx),
        process::running_pid(layout, PID),
    ) {
        // Graceful quit; nginx forks workers that a plain kill of the master would orphan on Windows.
        let prefix = conf_dir(layout, WebServer::Nginx);
        let mut cmd = crate::process::new_cmd(server.binary(ctx));
        cmd.arg("-p")
            .arg(&prefix)
            .arg("-c")
            .arg(config_file(ctx))
            .args(["-s", "quit"])
            .current_dir(&prefix);
        let _ = process::run_capture(cmd);
    }
    process::stop_named(layout, PID, STOP_GRACE)
}

pub fn reload(ctx: &Ctx) -> Result<()> {
    let layout = &ctx.app.layout;
    prepare(ctx)?;
    let Some(pid) = process::running_pid(layout, PID) else {
        return start(ctx);
    };
    match ctx.app.config.web.server {
        WebServer::Nginx => {
            if let Some(server) = active(ctx) {
                let prefix = conf_dir(layout, WebServer::Nginx);
                let mut cmd = crate::process::new_cmd(server.binary(ctx));
                cmd.arg("-p")
                    .arg(&prefix)
                    .arg("-c")
                    .arg(config_file(ctx))
                    .args(["-s", "reload"])
                    .current_dir(&prefix);
                process::run_capture(cmd)?;
            }
            Ok(())
        }
        _ => {
            // Apache's graceful restart needs a signal that is not portable; a restart takes
            // well under a second.
            let _ = pid;
            stop(ctx)?;
            start(ctx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packages::Registry;

    fn app() -> (tempfile::TempDir, Veranda) {
        let dir = tempfile::tempdir().unwrap();
        let app = Veranda::open_at(dir.path().to_path_buf()).unwrap();
        (dir, app)
    }
    use crate::Veranda;

    #[test]
    fn nginx_conf_renders() {
        let (_d, app) = app();
        let reg = Registry::default();
        std::fs::create_dir_all(app.layout.www().join("shop")).unwrap();
        let sites = crate::sites::discover(&app.layout, &app.config).unwrap();
        let ctx = Ctx {
            app: &app,
            reg: &reg,
            sites: &sites,
        };
        let text = nginx_conf(&ctx).unwrap();
        assert!(text.contains("server_name shop.test *.shop.test;"));
        assert!(text.contains("listen 127.0.0.1:80 default_server;"));
        assert!(!text.contains("{{"));
    }
}
