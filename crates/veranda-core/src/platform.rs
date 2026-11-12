//! Host OS / CPU detection and OS-specific constants.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    X86_64,
    Aarch64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
}

impl Platform {
    pub const fn current() -> Self {
        let os = if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Linux
        };
        let arch = if cfg!(target_arch = "aarch64") {
            Arch::Aarch64
        } else {
            Arch::X86_64
        };
        Self { os, arch }
    }

    /// Executable file suffix for this platform.
    pub const fn exe_suffix(&self) -> &'static str {
        match self.os {
            Os::Windows => ".exe",
            _ => "",
        }
    }

    pub fn exe(&self, name: &str) -> String {
        format!("{name}{}", self.exe_suffix())
    }

    pub const fn is_windows(&self) -> bool {
        matches!(self.os, Os::Windows)
    }
    pub const fn is_macos(&self) -> bool {
        matches!(self.os, Os::Macos)
    }
    pub const fn is_linux(&self) -> bool {
        matches!(self.os, Os::Linux)
    }

    /// Path to the system hosts file.
    pub fn hosts_file(&self) -> std::path::PathBuf {
        match self.os {
            Os::Windows => {
                let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
                std::path::PathBuf::from(root)
                    .join("System32")
                    .join("drivers")
                    .join("etc")
                    .join("hosts")
            }
            _ => std::path::PathBuf::from("/etc/hosts"),
        }
    }

    /// Short identifier used in manifests, e.g. `linux-x86_64`.
    pub fn id(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Os::Linux => "linux",
            Os::Macos => "macos",
            Os::Windows => "windows",
        })
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        })
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.os, self.arch)
    }
}

impl std::str::FromStr for Os {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "linux" => Ok(Os::Linux),
            "macos" | "darwin" | "mac" => Ok(Os::Macos),
            "windows" | "win" => Ok(Os::Windows),
            other => anyhow::bail!("unknown os: {other}"),
        }
    }
}

impl std::str::FromStr for Arch {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "x86_64" | "amd64" | "x64" => Ok(Arch::X86_64),
            "aarch64" | "arm64" => Ok(Arch::Aarch64),
            other => anyhow::bail!("unknown arch: {other}"),
        }
    }
}
