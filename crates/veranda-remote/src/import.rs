//! Import hosts from `~/.ssh/config` (and PuTTY sessions on Windows).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct ImportedHost {
    pub alias: String,
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub identity_file: String,
    pub proxy_jump: String,
    /// `ssh-config` or `putty`.
    pub source: String,
}

pub fn ssh_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh").join("config"))
}

/// Parse an OpenSSH client config. Wildcard `Host` blocks are skipped; `Include` lines are
/// followed one level deep relative to `~/.ssh`.
pub fn parse_ssh_config(text: &str, base: Option<&Path>) -> Vec<ImportedHost> {
    let mut out: Vec<ImportedHost> = vec![];
    let mut current: Vec<ImportedHost> = vec![];
    let flush = |current: &mut Vec<ImportedHost>, out: &mut Vec<ImportedHost>| {
        for h in current.drain(..) {
            if !h.alias.contains(['*', '?', '!']) {
                out.push(h);
            }
        }
    };
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = match line.split_once([' ', '\t', '=']) {
            Some((k, v)) => (
                k.trim().to_ascii_lowercase(),
                v.trim().trim_matches('"').to_string(),
            ),
            None => continue,
        };
        match key.as_str() {
            "host" => {
                flush(&mut current, &mut out);
                current = value
                    .split_whitespace()
                    .map(|a| ImportedHost {
                        alias: a.to_string(),
                        hostname: a.to_string(),
                        port: 22,
                        source: "ssh-config".into(),
                        ..Default::default()
                    })
                    .collect();
            }
            "match" => {
                flush(&mut current, &mut out);
                current.clear();
            }
            "include" => {
                if let Some(base) = base {
                    for pat in value.split_whitespace() {
                        let p = if Path::new(pat).is_absolute() {
                            PathBuf::from(pat)
                        } else if let Some(rest) = pat.strip_prefix("~/") {
                            dirs::home_dir().unwrap_or_default().join(rest)
                        } else {
                            base.join(pat)
                        };
                        if let Ok(t) = std::fs::read_to_string(&p) {
                            out.extend(parse_ssh_config(&t, None));
                        }
                    }
                }
            }
            "hostname" => current.iter_mut().for_each(|h| h.hostname = value.clone()),
            "user" => current.iter_mut().for_each(|h| h.user = value.clone()),
            "port" => {
                if let Ok(p) = value.parse::<u16>() {
                    current.iter_mut().for_each(|h| h.port = p);
                }
            }
            "identityfile" => current.iter_mut().for_each(|h| {
                if h.identity_file.is_empty() {
                    h.identity_file = expand_home(&value);
                }
            }),
            "proxyjump" => current
                .iter_mut()
                .for_each(|h| h.proxy_jump = value.clone()),
            _ => {}
        }
    }
    flush(&mut current, &mut out);
    out
}

fn expand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest).to_string_lossy().into_owned();
    }
    p.to_string()
}

/// Hosts from the user's `~/.ssh/config` (empty when there is none).
pub fn from_ssh_config() -> Vec<ImportedHost> {
    let Some(path) = ssh_config_path() else {
        return vec![];
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return vec![];
    };
    parse_ssh_config(&text, path.parent())
}

/// PuTTY saved sessions (Windows registry) – read through `reg query`, nothing is written.
#[cfg(windows)]
pub fn from_putty() -> Vec<ImportedHost> {
    use std::process::Command;
    let out = veranda_core::process::new_cmd("reg")
        .args(["query", r"HKCU\Software\SimonTatham\PuTTY\Sessions", "/s"])
        .output();
    let Ok(out) = out else { return vec![] };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut hosts = vec![];
    let mut cur: Option<ImportedHost> = None;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("HKEY_") {
            if let Some(h) = cur.take().filter(|h| !h.hostname.is_empty()) {
                hosts.push(h);
            }
            let name = t.rsplit('\\').next().unwrap_or("").replace("%20", " ");
            cur = Some(ImportedHost {
                alias: name,
                port: 22,
                source: "putty".into(),
                ..Default::default()
            });
            continue;
        }
        let Some(h) = cur.as_mut() else { continue };
        let parts: Vec<&str> = t.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let value = parts[2..].join(" ");
        match parts[0] {
            "HostName" => h.hostname = value,
            "UserName" => h.user = value,
            "PortNumber" => {
                h.port = u32::from_str_radix(value.trim_start_matches("0x"), 16)
                    .ok()
                    .and_then(|p| u16::try_from(p).ok())
                    .unwrap_or(22)
            }
            "PublicKeyFile" => h.identity_file = value,
            _ => {}
        }
    }
    if let Some(h) = cur.take().filter(|h| !h.hostname.is_empty()) {
        hosts.push(h);
    }
    hosts
}

#[cfg(not(windows))]
pub fn from_putty() -> Vec<ImportedHost> {
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_config() {
        let cfg = r#"
# comment
Host *
    ServerAliveInterval 60

Host web api
    HostName 10.0.0.5
    User deploy
    Port 2222
    IdentityFile ~/.ssh/id_ed25519

Host bastion
  HostName bastion.example.com
  User=admin
  ProxyJump web

Match host *.internal
    User root
"#;
        let hosts = parse_ssh_config(cfg, None);
        assert_eq!(hosts.len(), 3);
        assert_eq!(hosts[0].alias, "web");
        assert_eq!(hosts[0].hostname, "10.0.0.5");
        assert_eq!(hosts[0].port, 2222);
        assert_eq!(hosts[0].user, "deploy");
        assert!(hosts[0].identity_file.ends_with("id_ed25519"));
        assert_eq!(hosts[1].alias, "api");
        assert_eq!(hosts[2].alias, "bastion");
        assert_eq!(hosts[2].user, "admin");
        assert_eq!(hosts[2].proxy_jump, "web");
        assert_eq!(hosts[2].port, 22);
    }
}
