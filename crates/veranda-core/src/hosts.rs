//! Managed block inside the system hosts file.
//!
//! Only the lines between the markers are ever touched. Writing needs administrator
//! rights, so the desired host list is handed to an elevated helper (`veranda hosts apply
//! --from <file>`) which re-validates every entry before writing.

use crate::fsutil;
use anyhow::{Context, Result, bail};
use std::path::Path;

pub const BEGIN: &str = "# >>> veranda managed block - do not edit between the markers >>>";
pub const END: &str = "# <<< veranda managed block <<<";
/// Markers written by releases before the rename; removed on the next write.
const LEGACY_BEGIN: &str = "# >>> padu managed block - do not edit between the markers >>>";
const LEGACY_END: &str = "# <<< padu managed block <<<";

/// Drop the block written under the old name, if any.
fn strip_legacy(lines: Vec<&str>) -> Vec<&str> {
    let start = lines.iter().position(|l| l.trim() == LEGACY_BEGIN);
    let end = lines.iter().position(|l| l.trim() == LEGACY_END);
    match (start, end) {
        (Some(s), Some(e)) if e >= s => {
            let mut out = lines[..s].to_vec();
            out.extend_from_slice(&lines[e + 1..]);
            out
        }
        _ => lines,
    }
}

/// Strict hostname check: lowercase labels, digits, hyphens, must end with `.tld`.
pub fn is_valid_hostname(host: &str, tld: &str) -> bool {
    if host.len() > 253 || !host.ends_with(&format!(".{tld}")) {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    })
}

/// Render the managed block for `hosts` (IPv4 + IPv6 loopback).
pub fn render_block(hosts: &[String]) -> String {
    let mut out = String::new();
    out.push_str(BEGIN);
    out.push('\n');
    let mut sorted: Vec<&String> = hosts.iter().collect();
    sorted.sort();
    sorted.dedup();
    for h in sorted {
        out.push_str(&format!("127.0.0.1 {h}\n::1 {h}\n"));
    }
    out.push_str(END);
    out.push('\n');
    out
}

/// Replace (or append) the managed block in `existing`.
pub fn merge(existing: &str, hosts: &[String]) -> String {
    let block = render_block(hosts);
    let lines = strip_legacy(existing.lines().collect());
    let start = lines.iter().position(|l| l.trim() == BEGIN);
    let end = lines.iter().position(|l| l.trim() == END);
    let newline = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut out: Vec<String> = Vec::new();
    match (start, end) {
        (Some(s), Some(e)) if e >= s => {
            out.extend(lines[..s].iter().map(|l| l.to_string()));
            if !hosts.is_empty() {
                out.extend(block.lines().map(|l| l.to_string()));
            }
            out.extend(lines[e + 1..].iter().map(|l| l.to_string()));
        }
        _ => {
            out.extend(lines.iter().map(|l| l.to_string()));
            if !hosts.is_empty() {
                if out.last().map(|l| !l.trim().is_empty()).unwrap_or(false) {
                    out.push(String::new());
                }
                out.extend(block.lines().map(|l| l.to_string()));
            }
        }
    }
    let mut text = out.join(newline);
    text.push_str(newline);
    text
}

/// Hosts currently listed in the managed block of `existing`.
pub fn current_block_hosts(existing: &str) -> Vec<String> {
    let mut inside = false;
    let mut v = Vec::new();
    for l in existing.lines() {
        let t = l.trim();
        if t == BEGIN {
            inside = true;
            continue;
        }
        if t == END {
            break;
        }
        if inside
            && let Some(h) = t.split_whitespace().nth(1)
            && !v.contains(&h.to_string())
        {
            v.push(h.to_string());
        }
    }
    v
}

/// Does the system hosts file already contain exactly these hosts in our block?
pub fn is_in_sync(hosts_file: &Path, hosts: &[String]) -> bool {
    let Ok(text) = std::fs::read_to_string(hosts_file) else {
        return false;
    };
    if text.contains(LEGACY_BEGIN) {
        return false; // old block still present: rewrite once to clean it up
    }
    let mut want: Vec<String> = hosts.to_vec();
    want.sort();
    want.dedup();
    let mut have = current_block_hosts(&text);
    have.sort();
    have == want
}

/// Payload exchanged with the elevated helper: one hostname per line, validated with `tld`.
pub fn write_payload(path: &Path, hosts: &[String], tld: &str) -> Result<()> {
    for h in hosts {
        if !is_valid_hostname(h, tld) {
            bail!("invalid hostname {h:?}");
        }
    }
    let mut text = format!("tld={tld}\n");
    for h in hosts {
        text.push_str(h);
        text.push('\n');
    }
    fsutil::write_private(path, text.as_bytes())
}

pub fn read_payload(path: &Path) -> Result<(String, Vec<String>)> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut lines = text.lines();
    let tld = lines
        .next()
        .and_then(|l| l.strip_prefix("tld="))
        .context("payload missing tld")?
        .trim()
        .to_string();
    if tld.is_empty()
        || !tld
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        bail!("payload has an invalid tld");
    }
    let mut hosts = Vec::new();
    for l in lines {
        let h = l.trim();
        if h.is_empty() {
            continue;
        }
        if !is_valid_hostname(h, &tld) {
            bail!("payload contains invalid hostname {h:?}");
        }
        hosts.push(h.to_string());
    }
    Ok((tld, hosts))
}

/// Apply a payload to the hosts file. Runs with elevated rights; validates everything again,
/// keeps a backup next to the file and writes atomically.
pub fn apply_payload(hosts_file: &Path, payload: &Path) -> Result<()> {
    let (_tld, hosts) = read_payload(payload)?;
    let existing = std::fs::read_to_string(hosts_file).unwrap_or_default();
    let merged = merge(&existing, &hosts);
    if merged == existing {
        return Ok(());
    }
    let backup = hosts_file.with_extension("veranda.bak");
    let _ = std::fs::write(&backup, &existing);
    // /etc/hosts must stay world-readable; write in place rather than rename (keeps owner/mode).
    std::fs::write(hosts_file, merged.as_bytes())
        .with_context(|| format!("writing {}", hosts_file.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate() {
        assert!(is_valid_hostname("my-app.test", "test"));
        assert!(!is_valid_hostname("My-App.test", "test"));
        assert!(!is_valid_hostname("evil.com", "test"));
        assert!(!is_valid_hostname("-x.test", "test"));
        assert!(!is_valid_hostname("a..test", "test"));
    }

    #[test]
    fn merge_replaces_block_only() {
        let before = "127.0.0.1 localhost\n# keep me\n";
        let v1 = merge(before, &["a.test".into()]);
        assert!(v1.contains("127.0.0.1 a.test"));
        assert!(v1.contains("# keep me"));
        let v2 = merge(&v1, &["b.test".into()]);
        assert!(!v2.contains("a.test"));
        assert!(v2.contains("::1 b.test"));
        assert_eq!(v2.matches(BEGIN).count(), 1);
        let v3 = merge(&v2, &[]);
        assert!(!v3.contains(BEGIN));
        assert!(v3.contains("# keep me"));
        assert_eq!(current_block_hosts(&v2), vec!["b.test".to_string()]);
    }

    #[test]
    fn payload_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("payload");
        write_payload(&p, &["x.test".into()], "test").unwrap();
        let (tld, hosts) = read_payload(&p).unwrap();
        assert_eq!(tld, "test");
        assert_eq!(hosts, vec!["x.test".to_string()]);
        assert!(write_payload(&p, &["bad host.test".into()], "test").is_err());
        let hf = dir.path().join("hosts");
        std::fs::write(&hf, "127.0.0.1 localhost\n").unwrap();
        apply_payload(&hf, &p).unwrap();
        assert!(is_in_sync(&hf, &["x.test".into()]));
    }
}
