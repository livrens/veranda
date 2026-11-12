//! Veranda core: a lightweight, security-first local web development stack.
//!
//! The crate is a pure library (no UI). It is shared by the CLI and the desktop app.
//! Everything that touches the network, the filesystem or child processes lives here
//! so it can be tested without a GUI.

pub mod applock;
pub mod archive;
pub mod backup;
pub mod config;
pub mod db;
pub mod download;
pub mod files;
pub mod fsutil;
pub mod hosts;
pub mod manifest;
pub mod migrate;
pub mod packages;
pub mod paths;
pub mod platform;
pub mod process;
pub mod projects;
pub mod resolve;
pub mod services;
pub mod sites;
pub mod slim;
pub mod tls;
pub mod tools;
pub mod transfer;
pub mod update;

pub use config::Config;
pub use paths::Layout;
pub use platform::{Arch, Os, Platform};

/// Application version (from Cargo.toml of this crate; kept in sync with the workspace).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Product name shown in UIs and used for folder names.
pub const APP_NAME: &str = "Veranda";

/// Root object that ties the layout and configuration together.
#[derive(Debug, Clone)]
pub struct Veranda {
    pub layout: Layout,
    pub config: Config,
}

impl Veranda {
    /// Discover the installation root and load (or create) its configuration.
    pub fn open() -> anyhow::Result<Self> {
        let layout = Layout::discover()?;
        layout.ensure()?;
        let config = Config::load_or_default(&layout)?;
        Ok(Self { layout, config })
    }

    /// Open a specific root (used by `veranda init --root`).
    pub fn open_at(root: std::path::PathBuf) -> anyhow::Result<Self> {
        let layout = Layout::new(root);
        layout.ensure()?;
        let config = Config::load_or_default(&layout)?;
        Ok(Self { layout, config })
    }

    pub fn save_config(&self) -> anyhow::Result<()> {
        self.config.save(&self.layout)
    }

    pub fn platform(&self) -> Platform {
        Platform::current()
    }
}
