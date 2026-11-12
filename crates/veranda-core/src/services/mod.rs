//! Service lifecycle: web server, PHP FastCGI, MariaDB/MySQL, PostgreSQL and MongoDB.

pub mod db;
pub mod extra;
pub mod pg;
pub mod php;
pub mod web;

use crate::Veranda;
use crate::packages::Registry;
use crate::sites::Site;
use anyhow::Context as _;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceId {
    Web,
    Php,
    Db,
    Pg,
    Mongo,
}

impl ServiceId {
    pub const ALL: [ServiceId; 5] = [
        ServiceId::Web,
        ServiceId::Php,
        ServiceId::Db,
        ServiceId::Pg,
        ServiceId::Mongo,
    ];

    pub const fn id(&self) -> &'static str {
        match self {
            ServiceId::Web => "web",
            ServiceId::Php => "php",
            ServiceId::Db => "db",
            ServiceId::Pg => "pg",
            ServiceId::Mongo => "mongodb",
        }
    }
}

impl fmt::Display for ServiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.id())
    }
}

impl std::str::FromStr for ServiceId {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "web" | "caddy" | "nginx" | "apache" | "httpd" => Ok(ServiceId::Web),
            "php" | "php-fpm" => Ok(ServiceId::Php),
            "db" | "mysql" | "mariadb" | "database" => Ok(ServiceId::Db),
            "pg" | "postgres" | "postgresql" => Ok(ServiceId::Pg),
            "mongodb" | "mongo" => Ok(ServiceId::Mongo),
            other => bail!("unknown service `{other}` (web, php, db, pg, mongodb)"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub id: ServiceId,
    /// Human name including the engine, e.g. `Caddy`, `MariaDB`.
    pub name: String,
    pub enabled: bool,
    pub installed: bool,
    pub version: String,
    pub running: bool,
    pub pids: Vec<u32>,
    pub ports: Vec<u16>,
    /// Hint for the UI (missing package, port conflict...).
    pub message: String,
}

/// Everything a service needs to prepare and start.
pub struct Ctx<'a> {
    pub app: &'a Veranda,
    pub reg: &'a Registry,
    pub sites: &'a [Site],
}

impl<'a> Ctx<'a> {
    pub fn path_entries(&self) -> Vec<std::path::PathBuf> {
        crate::packages::runtime_path(&self.app.layout, self.reg, &self.app.config)
    }
}

/// Replace `{{KEY}}` placeholders. Unknown keys are left as-is (visible in the output).
pub fn render(tpl: &str, vars: &[(&str, String)]) -> String {
    let mut out = tpl.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

pub const STOP_GRACE: Duration = Duration::from_secs(5);

fn enabled(app: &Veranda, id: ServiceId) -> bool {
    let c = &app.config;
    if c.general.hidden_services.iter().any(|h| h == id.id()) {
        return false;
    }
    match id {
        ServiceId::Web => c.web.enabled,
        ServiceId::Php => c.php.enabled,
        ServiceId::Db => c.db.enabled,
        ServiceId::Pg => c.pg.enabled,
        ServiceId::Mongo => c.mongodb.enabled,
    }
}

pub fn status(ctx: &Ctx, id: ServiceId) -> ServiceStatus {
    let mut st = match id {
        ServiceId::Web => web::status(ctx),
        ServiceId::Php => php::status(ctx),
        ServiceId::Db => db::status(ctx),
        ServiceId::Pg => pg::status(ctx),
        ServiceId::Mongo => extra::status(extra::Which::Mongo, ctx),
    };
    st.enabled = enabled(ctx.app, id);
    if st.running
        && st.message.is_empty()
        && let Some(port) = st.ports.first()
        && !crate::process::port_accepting(*port)
    {
        st.message = format!("not listening on port {port}; restart to apply the new settings");
    }
    st
}

pub fn status_all(app: &Veranda) -> Result<Vec<ServiceStatus>> {
    let reg = Registry::load(&app.layout)?;
    let sites = crate::sites::discover(&app.layout, &app.config)?;
    let ctx = Ctx {
        app,
        reg: &reg,
        sites: &sites,
    };
    Ok(ServiceId::ALL.iter().map(|id| status(&ctx, *id)).collect())
}

/// Regenerate every config file, the site certificate and (if possible) the hosts file.
/// Returns the list of hostnames that are missing from the hosts file.
pub fn prepare_all(app: &Veranda) -> Result<Vec<String>> {
    let reg = Registry::load(&app.layout)?;
    let sites = crate::sites::discover(&app.layout, &app.config)?;
    let ctx = Ctx {
        app,
        reg: &reg,
        sites: &sites,
    };
    if app.config.web.ssl {
        crate::tls::ensure_site_cert(&app.layout, &crate::sites::hostnames(&sites))?;
    }
    php::prepare(&ctx)?;
    web::prepare(&ctx)?;
    db::prepare(&ctx)?;
    pg::prepare(&ctx)?;
    let hosts = crate::sites::hostnames(&sites);
    let hosts_file = app.platform().hosts_file();
    if crate::hosts::is_in_sync(&hosts_file, &hosts) {
        Ok(vec![])
    } else {
        Ok(hosts)
    }
}

pub fn start(app: &Veranda, id: ServiceId) -> Result<()> {
    let reg = Registry::load(&app.layout)?;
    let sites = crate::sites::discover(&app.layout, &app.config)?;
    let ctx = Ctx {
        app,
        reg: &reg,
        sites: &sites,
    };
    if app.config.web.ssl && matches!(id, ServiceId::Web) {
        crate::tls::ensure_site_cert(&app.layout, &crate::sites::hostnames(&sites))?;
    }
    // A service still running with an older port setting is restarted so the new one applies.
    let st = status(&ctx, id);
    if st.running
        && let Some(port) = st.ports.first()
        && !crate::process::port_accepting(*port)
    {
        stop(app, id)?;
    }
    match id {
        ServiceId::Web => web::start(&ctx),
        ServiceId::Php => php::start(&ctx),
        ServiceId::Db => db::start(&ctx),
        ServiceId::Pg => pg::start(&ctx),
        ServiceId::Mongo => extra::start(extra::Which::Mongo, &ctx),
    }
}

/// Start the services of a startup group in order; unknown ids are reported as errors.
pub fn start_group(app: &Veranda, name: &str) -> Result<Vec<(ServiceId, Result<()>)>> {
    let g = app
        .config
        .general
        .startup_groups
        .iter()
        .find(|g| g.name == name)
        .with_context(|| format!("no startup group named {name}"))?;
    prepare_all(app)?;
    Ok(g.services
        .iter()
        .map(|id| match id.parse::<ServiceId>() {
            Ok(sid) => (sid, start(app, sid)),
            Err(e) => (ServiceId::Web, Err(e)),
        })
        .collect())
}

/// Stop the services of a startup group, last one first.
pub fn stop_group(app: &Veranda, name: &str) -> Result<Vec<(ServiceId, Result<()>)>> {
    let g = app
        .config
        .general
        .startup_groups
        .iter()
        .find(|g| g.name == name)
        .with_context(|| format!("no startup group named {name}"))?;
    Ok(g.services
        .iter()
        .rev()
        .filter_map(|id| id.parse::<ServiceId>().ok())
        .map(|sid| (sid, stop(app, sid)))
        .collect())
}

/// Restart every running service whose configured port is not accepting connections: it is
/// still bound to an older port setting (ports moved while it ran, app updated in between...).
/// Returns what was restarted and how it went.
pub fn heal_stale_ports(app: &Veranda) -> Vec<(ServiceId, Result<()>)> {
    let Ok(statuses) = status_all(app) else {
        return vec![];
    };
    statuses
        .into_iter()
        .filter(|s| {
            s.running
                && s.installed
                && s.ports
                    .first()
                    .is_some_and(|p| !crate::process::port_accepting(*p))
        })
        .map(|s| {
            let r = stop(app, s.id).and_then(|_| start(app, s.id));
            (s.id, r)
        })
        .collect()
}

pub fn stop(app: &Veranda, id: ServiceId) -> Result<()> {
    let reg = Registry::load(&app.layout)?;
    let ctx = Ctx {
        app,
        reg: &reg,
        sites: &[],
    };
    match id {
        ServiceId::Web => web::stop(&ctx),
        ServiceId::Php => php::stop(&ctx),
        ServiceId::Db => db::stop(&ctx),
        ServiceId::Pg => pg::stop(&ctx),
        ServiceId::Mongo => extra::stop(extra::Which::Mongo, &ctx),
    }
}

pub fn restart(app: &Veranda, id: ServiceId) -> Result<()> {
    stop(app, id)?;
    start(app, id)
}

/// Reload configuration (web server picks up new sites). Falls back to restart.
pub fn reload(app: &Veranda, id: ServiceId) -> Result<()> {
    match id {
        ServiceId::Web => {
            let reg = Registry::load(&app.layout)?;
            let sites = crate::sites::discover(&app.layout, &app.config)?;
            let ctx = Ctx {
                app,
                reg: &reg,
                sites: &sites,
            };
            if app.config.web.ssl {
                crate::tls::ensure_site_cert(&app.layout, &crate::sites::hostnames(&sites))?;
            }
            web::reload(&ctx)
        }
        other => restart(app, other),
    }
}

/// Start every enabled service in dependency order (db, cache, mail, php, web).
/// Collects errors instead of stopping at the first one.
pub fn start_all(app: &Veranda) -> Vec<(ServiceId, Result<()>)> {
    let order = [
        ServiceId::Db,
        ServiceId::Pg,
        ServiceId::Mongo,
        ServiceId::Php,
        ServiceId::Web,
    ];
    order
        .iter()
        .filter(|id| enabled(app, **id))
        .map(|id| (*id, start(app, *id)))
        .collect()
}

pub fn stop_all(app: &Veranda) -> Vec<(ServiceId, Result<()>)> {
    let order = [
        ServiceId::Web,
        ServiceId::Php,
        ServiceId::Mongo,
        ServiceId::Pg,
        ServiceId::Db,
    ];
    order.iter().map(|id| (*id, stop(app, *id))).collect()
}

/// Error for a missing runtime with the exact command that fixes it.
pub fn not_installed(kind: crate::manifest::Kind) -> anyhow::Error {
    anyhow::anyhow!(
        "{} is not installed. Run: veranda install {}",
        kind.display(),
        kind.id()
    )
}

/// Fail early with a useful message when a port is taken by someone else.
/// The OS refused to bind a privileged port (Linux, ports < 1024 without root).
/// Front-ends detect this error to offer the one-time permission fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortPermission {
    pub port: u16,
}

impl fmt::Display for PortPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "port {} needs permission: on Linux, ports below 1024 require a one-time grant \
             (veranda doctor --fix-ports) or use ports 8080/8443 in Settings",
            self.port
        )
    }
}

impl std::error::Error for PortPermission {}

/// Like [`ensure_port_free`], but for a server binary that may hold `cap_net_bind_service`:
/// Veranda itself can never bind a privileged port, so when the OS refuses the probe we only
/// fail if something already listens or the binary has not been granted the capability.
/// Something else already listens on a port a service needs. Front-ends offer to move the
/// service to the next free port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInUse {
    pub port: u16,
    pub what: String,
    pub owner: String,
}

impl fmt::Display for PortInUse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.owner.is_empty() {
            write!(
                f,
                "port {} needed by {} is already in use",
                self.port, self.what
            )
        } else {
            write!(
                f,
                "port {} needed by {} is already in use by {}",
                self.port, self.what, self.owner
            )
        }
    }
}

impl std::error::Error for PortInUse {}

fn in_use(port: u16, what: &str) -> anyhow::Error {
    PortInUse {
        port,
        what: what.to_string(),
        owner: crate::process::port_owner(port).unwrap_or_default(),
    }
    .into()
}

pub fn ensure_port_free_for(port: u16, what: &str, binary: &std::path::Path) -> Result<()> {
    match crate::process::port_state(port) {
        crate::process::PortState::Free => Ok(()),
        crate::process::PortState::InUse => Err(in_use(port, what)),
        crate::process::PortState::Denied => {
            if crate::process::port_accepting(port) {
                return Err(in_use(port, what));
            }
            if crate::tools::has_bind_capability(binary) {
                Ok(())
            } else {
                Err(PortPermission { port }.into())
            }
        }
    }
}

pub fn ensure_port_free(port: u16, what: &str) -> Result<()> {
    match crate::process::port_state(port) {
        crate::process::PortState::Free => Ok(()),
        crate::process::PortState::Denied => Err(PortPermission { port }.into()),
        crate::process::PortState::InUse => Err(in_use(port, what)),
    }
}
