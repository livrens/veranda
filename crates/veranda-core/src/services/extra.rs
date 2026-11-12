//! Single-binary services that need no config file: MongoDB.
//! Each one gets a data folder under `data/`, listens on loopback only, logs to `logs/<id>.log`.

use super::{Ctx, ServiceId, ServiceStatus};
use crate::config::Extra;
use crate::manifest::Kind;
use crate::packages::{Binaries, Installed};
use crate::paths::Layout;
use crate::{fsutil, process};
use anyhow::{Result, bail};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Mongo,
}

impl Which {
    pub const fn id(self) -> ServiceId {
        match self {
            Which::Mongo => ServiceId::Mongo,
        }
    }
    pub const fn kind(self) -> Kind {
        match self {
            Which::Mongo => Kind::MongoDb,
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Which::Mongo => "MongoDB",
        }
    }
    pub const fn default_port(self) -> u16 {
        match self {
            Which::Mongo => 27017,
        }
    }
    fn pid_name(self) -> &'static str {
        self.id().id()
    }
    pub fn settings(self, cfg: &crate::Config) -> &Extra {
        match self {
            Which::Mongo => &cfg.mongodb,
        }
    }
}

pub fn port(which: Which, cfg: &crate::Config) -> u16 {
    let p = which.settings(cfg).port;
    if p == 0 { which.default_port() } else { p }
}

pub fn data_dir(which: Which, layout: &Layout, cfg: &crate::Config) -> PathBuf {
    let d = which.settings(cfg).data_dir.trim();
    if d.is_empty() {
        layout.data().join(which.id().id())
    } else {
        PathBuf::from(d)
    }
}

fn active(which: Which, ctx: &Ctx) -> Option<Installed> {
    ctx.reg
        .active(which.kind(), &which.settings(&ctx.app.config).version)
        .cloned()
}

fn binary(which: Which, pkg: &Installed) -> PathBuf {
    let exe = |dir: PathBuf, n: &str| dir.join(crate::Platform::current().exe(n));
    match which {
        Which::Mongo => exe(Binaries::mongodb_bin_dir(pkg), "mongod"),
    }
}

pub fn status(which: Which, ctx: &Ctx) -> ServiceStatus {
    let pkg = active(which, ctx);
    let pid = process::running_pid(&ctx.app.layout, which.pid_name());
    let ports = vec![port(which, &ctx.app.config)];
    ServiceStatus {
        id: which.id(),
        name: which.name().into(),
        enabled: true,
        installed: pkg.is_some(),
        version: pkg.as_ref().map(|p| p.version.clone()).unwrap_or_default(),
        running: pid.is_some(),
        pids: pid.into_iter().collect(),
        ports,
        message: if pkg.is_none() {
            "not installed".into()
        } else {
            String::new()
        },
    }
}

pub fn start(which: Which, ctx: &Ctx) -> Result<()> {
    let pkg = active(which, ctx).ok_or_else(|| super::not_installed(which.kind()))?;
    let layout = &ctx.app.layout;
    let cfg = &ctx.app.config;
    if process::running_pid(layout, which.pid_name()).is_some() {
        return Ok(());
    }
    process::clear_pid(layout, which.pid_name());
    let p = port(which, cfg);
    super::ensure_port_free(p, which.name())?;
    let data = data_dir(which, layout, cfg);
    std::fs::create_dir_all(&data)?;
    fsutil::make_private_dir(&data)?;
    let bin = binary(which, &pkg);
    if !bin.is_file() {
        bail!("{} binary not found: {}", which.name(), bin.display());
    }
    let mut cmd = process::command(&bin, &ctx.path_entries());
    match which {
        Which::Mongo => {
            cmd.arg("--dbpath").arg(&data).args([
                "--port",
                &p.to_string(),
                "--bind_ip",
                "127.0.0.1",
            ]);
        }
    }
    cmd.current_dir(&pkg.path);
    process::spawn_detached(layout, which.pid_name(), cmd)?;
    if !process::wait_for_port(p, std::time::Duration::from_secs(60)) {
        bail!(
            "{} did not start listening on port {p}. See logs/{}.log",
            which.name(),
            which.pid_name()
        );
    }
    Ok(())
}

pub fn stop(which: Which, ctx: &Ctx) -> Result<()> {
    process::stop_named(
        &ctx.app.layout,
        which.pid_name(),
        std::time::Duration::from_secs(15),
    )
}
