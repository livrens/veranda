//! Known server keys, kept in OpenSSH `known_hosts` format at `etc/remote/known_hosts`.

use anyhow::{Context, Result};
use russh::keys::PublicKey;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct KnownHost {
    /// 1-based line in the file.
    pub line: usize,
    /// Host pattern as written (`host`, `[host]:port`, `|1|...` for hashed entries).
    pub hosts: String,
    pub algorithm: String,
    pub fingerprint: String,
    pub comment: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Known,
    Unknown,
    /// A different key is recorded on this line.
    Changed {
        line: usize,
    },
}

pub fn list(path: &Path) -> Result<Vec<KnownHost>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut out = vec![];
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let mut parts = t.split_whitespace();
        let (Some(hosts), Some(algorithm), Some(b64)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let fingerprint = russh::keys::parse_public_key_base64(b64)
            .map(|k| crate::keys::fingerprint(&k))
            .unwrap_or_else(|_| "(unreadable key)".into());
        out.push(KnownHost {
            line: i + 1,
            hosts: hosts.to_string(),
            algorithm: algorithm.to_string(),
            fingerprint,
            comment: parts.collect::<Vec<_>>().join(" "),
        });
    }
    Ok(out)
}

/// Remove one line (1-based).
pub fn remove_line(path: &Path, line: usize) -> Result<bool> {
    if !path.exists() || line == 0 {
        return Ok(false);
    }
    let text = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = text.lines().collect();
    if line > lines.len() {
        return Ok(false);
    }
    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != line - 1)
        .map(|(_, l)| *l)
        .collect();
    let mut body = kept.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    veranda_core::fsutil::write_private(path, body.as_bytes())?;
    Ok(true)
}

pub fn check(path: &Path, host: &str, port: u16, key: &PublicKey) -> Status {
    match russh::keys::check_known_hosts_path(host, port, key, path) {
        Ok(true) => Status::Known,
        Ok(false) => Status::Unknown,
        Err(russh::keys::Error::KeyChanged { line }) => Status::Changed { line },
        Err(e) => {
            tracing::warn!("known_hosts check failed: {e}");
            Status::Unknown
        }
    }
}

/// Record the key; a changed key replaces the old line.
pub fn learn(
    path: &Path,
    host: &str,
    port: u16,
    key: &PublicKey,
    replace_line: Option<usize>,
) -> Result<()> {
    if let Some(line) = replace_line {
        remove_line(path, line)?;
    }
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    russh::keys::known_hosts::learn_known_hosts_path(host, port, key, path)
        .context("recording the host key")?;
    // russh starts a fresh file with a blank line; keep the file tidy so line numbers start at 1
    let text = std::fs::read_to_string(path)?;
    let trimmed = text.trim_start_matches('\n');
    if trimmed.len() != text.len() {
        veranda_core::fsutil::write_private(path, trimmed.as_bytes())?;
    } else {
        veranda_core::fsutil::make_private_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::{Algorithm, PrivateKey};

    #[test]
    fn learn_check_list_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kh");
        let k1 = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let k2 = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        assert_eq!(
            check(&path, "example.com", 22, k1.public_key()),
            Status::Unknown
        );
        learn(&path, "example.com", 22, k1.public_key(), None).unwrap();
        learn(&path, "example.com", 2222, k2.public_key(), None).unwrap();
        assert_eq!(
            check(&path, "example.com", 22, k1.public_key()),
            Status::Known
        );
        assert_eq!(
            check(&path, "example.com", 2222, k2.public_key()),
            Status::Known
        );
        assert!(matches!(
            check(&path, "example.com", 22, k2.public_key()),
            Status::Changed { line: 1 }
        ));
        let l = list(&path).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].hosts, "example.com");
        assert_eq!(l[1].hosts, "[example.com]:2222");
        assert_eq!(l[0].fingerprint, crate::keys::fingerprint(k1.public_key()));
        // replace the changed key
        learn(&path, "example.com", 22, k2.public_key(), Some(1)).unwrap();
        assert_eq!(
            check(&path, "example.com", 22, k2.public_key()),
            Status::Known
        );
        assert!(remove_line(&path, 1).unwrap());
        assert_eq!(list(&path).unwrap().len(), 1);
        assert!(!remove_line(&path, 9).unwrap());
    }
}
