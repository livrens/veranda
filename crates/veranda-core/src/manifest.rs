//! Package manifest: what can be installed, from where, and how to verify it.
//!
//! The built-in manifest (`packages.toml`) lists versions verified at release time.
//! `resolve` can additionally ask upstream sources for newer versions at run time.

use crate::platform::{Arch, Os, Platform};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Kinds of software Veranda can manage. The string form is the folder under `bin/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Php,
    Nginx,
    Apache,
    MariaDb,
    MySql,
    Node,
    Adminer,
    PhpMyAdmin,
    /// PostgreSQL server (zonky.io repackaging of the official binaries).
    Postgres,
    /// CPython from python-build-standalone (astral-sh).
    Python,
    /// MongoDB Community Server (official archives).
    MongoDb,
}

impl Kind {
    pub const ALL: [Kind; 11] = [
        Kind::Php,
        Kind::Nginx,
        Kind::Apache,
        Kind::MariaDb,
        Kind::MySql,
        Kind::Node,
        Kind::Adminer,
        Kind::PhpMyAdmin,
        Kind::Postgres,
        Kind::Python,
        Kind::MongoDb,
    ];

    pub const fn id(&self) -> &'static str {
        match self {
            Kind::Php => "php",
            Kind::Nginx => "nginx",
            Kind::Apache => "apache",
            Kind::MariaDb => "mariadb",
            Kind::MySql => "mysql",
            Kind::Node => "node",
            Kind::Adminer => "adminer",
            Kind::PhpMyAdmin => "phpmyadmin",
            Kind::Postgres => "postgres",
            Kind::Python => "python",
            Kind::MongoDb => "mongodb",
        }
    }

    pub const fn display(&self) -> &'static str {
        match self {
            Kind::Php => "PHP",
            Kind::Nginx => "Nginx",
            Kind::Apache => "Apache HTTPD",
            Kind::MariaDb => "MariaDB",
            Kind::MySql => "MySQL",
            Kind::Node => "Node.js",
            Kind::Adminer => "Adminer",
            Kind::PhpMyAdmin => "phpMyAdmin",
            Kind::Postgres => "PostgreSQL",
            Kind::Python => "Python",
            Kind::MongoDb => "MongoDB",
        }
    }

    /// Tools that are web apps served by the web server rather than runtimes.
    pub const fn is_web_app(&self) -> bool {
        matches!(self, Kind::Adminer | Kind::PhpMyAdmin)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.id())
    }
}

impl std::str::FromStr for Kind {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Kind::ALL
            .iter()
            .copied()
            .find(|k| k.id() == s.to_ascii_lowercase())
            .with_context(|| {
                format!(
                    "unknown package kind `{s}`; known: {}",
                    Kind::ALL.map(|k| k.id()).join(", ")
                )
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArchiveType {
    Zip,
    TarGz,
    TarXz,
    /// A single file copied as-is (adminer.php).
    File,
}

impl ArchiveType {
    pub fn guess(url: &str) -> ArchiveType {
        let u = url.to_ascii_lowercase();
        let u = u.split('?').next().unwrap_or("");
        if u.ends_with(".zip") {
            ArchiveType::Zip
        } else if u.ends_with(".tar.gz") || u.ends_with(".tgz") {
            ArchiveType::TarGz
        } else if u.ends_with(".tar.xz") || u.ends_with(".txz") {
            ArchiveType::TarXz
        } else {
            ArchiveType::File
        }
    }
}

/// Integrity check for a download. HTTPS is always required in addition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "algo")]
pub enum Checksum {
    Sha256 {
        hex: String,
    },
    Sha512 {
        hex: String,
    },
    /// Only offered by MySQL; accepted but treated as weak (still HTTPS + trust-on-first-use).
    Md5 {
        hex: String,
    },
    /// Verified against a remote checksum file fetched at download time (`url` = file, `name` = entry).
    Remote {
        url: String,
        name: String,
    },
    /// No upstream checksum: record the hash on first download and refuse silent changes.
    Tofu,
}

/// One downloadable file that is part of a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub url: String,
    pub archive: ArchiveType,
    pub checksum: Checksum,
    /// Strip this many leading path components when extracting (0 = keep as is).
    #[serde(default)]
    pub strip: u32,
    /// Sub folder inside the package dir to extract into ("" = package root).
    #[serde(default)]
    pub dest: String,
    /// Final file name for `ArchiveType::File`.
    #[serde(default)]
    pub file_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub kind: Kind,
    pub version: String,
    pub os: Os,
    pub arch: Arch,
    pub parts: Vec<Part>,
    /// Where it came from (`builtin`, `php.net`, `github`, ...).
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub notes: String,
}

impl Package {
    pub fn platform(&self) -> Platform {
        Platform {
            os: self.os,
            arch: self.arch,
        }
    }
    pub fn matches(&self, p: Platform) -> bool {
        self.os == p.os && self.arch == p.arch
    }
    /// Sort key: newest version first.
    pub fn version_key(&self) -> VersionKey {
        parse_version(&self.version)
    }
}

/// Ordering key for loosely formatted versions: every numeric group in order.
/// `8.5.12-1` -> [8,5,12,1], `2.4.69-261002` -> [2,4,69,261002]. A longer key with the
/// same prefix is newer, which is what vendors mean by a `-1` rebuild suffix.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VersionKey(pub Vec<u64>);

pub fn parse_version(v: &str) -> VersionKey {
    let mut nums = Vec::new();
    let mut cur = String::new();
    for c in v.trim().chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            nums.push(cur.parse::<u64>().unwrap_or(0));
            cur.clear();
        }
    }
    if !cur.is_empty() {
        nums.push(cur.parse::<u64>().unwrap_or(0));
    }
    VersionKey(nums)
}

/// `major.minor` of a version string, e.g. `8.5.12-1` -> `8.5`.
pub fn version_branch(v: &str) -> String {
    let k = parse_version(v);
    match k.0.as_slice() {
        [a, b, ..] => format!("{a}.{b}"),
        [a] => a.to_string(),
        [] => String::new(),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub packages: Vec<Package>,
}

/// Built-in manifest, embedded at compile time.
pub const BUILTIN_TOML: &str = include_str!("packages.toml");

impl Manifest {
    pub fn builtin() -> Result<Manifest> {
        toml::from_str(BUILTIN_TOML).context("parsing built-in packages.toml")
    }

    pub fn for_platform(&self, p: Platform) -> impl Iterator<Item = &Package> {
        self.packages.iter().filter(move |pk| pk.matches(p))
    }

    /// Newest package of a kind for a platform.
    pub fn latest(&self, kind: Kind, p: Platform) -> Option<&Package> {
        self.for_platform(p)
            .filter(|pk| pk.kind == kind)
            .max_by_key(|pk| pk.version_key())
    }

    /// All versions of a kind for a platform, newest first.
    pub fn versions(&self, kind: Kind, p: Platform) -> Vec<&Package> {
        let mut v: Vec<&Package> = self.for_platform(p).filter(|pk| pk.kind == kind).collect();
        v.sort_by_key(|pk| std::cmp::Reverse(pk.version_key()));
        v
    }

    /// Merge newer entries from another manifest (same kind/version/platform replaces).
    pub fn merge(&mut self, other: Manifest) {
        for pk in other.packages {
            self.packages.retain(|e| {
                !(e.kind == pk.kind
                    && e.version == pk.version
                    && e.os == pk.os
                    && e.arch == pk.arch)
            });
            self.packages.push(pk);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_parses_and_covers_platforms() {
        let m = Manifest::builtin().unwrap();
        for p in [
            Platform {
                os: Os::Linux,
                arch: Arch::X86_64,
            },
            Platform {
                os: Os::Windows,
                arch: Arch::X86_64,
            },
            Platform {
                os: Os::Macos,
                arch: Arch::Aarch64,
            },
        ] {
            assert!(m.latest(Kind::Php, p).is_some(), "php missing for {p}");
        }
        for pk in &m.packages {
            for part in &pk.parts {
                assert!(
                    part.url.starts_with("https://"),
                    "{} must be https",
                    part.url
                );
            }
        }
    }

    #[test]
    fn versions_parse() {
        assert!(parse_version("8.5.12-1") > parse_version("8.5.12"));
        assert!(parse_version("2.4.69-261002") > parse_version("2.4.66-260223"));
        assert!(parse_version("v2.11.7") == parse_version("2.11.7"));
        assert!(parse_version("26.10.0") > parse_version("9.7.4"));
        assert_eq!(version_branch("8.5.12-1"), "8.5");
    }
}
