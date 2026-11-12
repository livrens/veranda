//! HTTPS-only downloader with integrity verification.
//!
//! * TLS via rustls (no system OpenSSL), certificate validation always on.
//! * Streams to `<file>.part` and renames on success (no half files).
//! * Verifies SHA-256 / SHA-512 / MD5 / remote checksum files.
//! * Trust-on-first-use ledger (`etc/checksums.json`) for sources without checksums:
//!   a changed hash for a previously seen URL is an error, not a warning.

use crate::manifest::Checksum;
use crate::paths::Layout;
use anyhow::{Context, Result, bail};
use md5::Md5;
use sha2::{Digest, Sha256, Sha512};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// Hard cap on a single download (defensive; the largest runtime is < 1 GiB).
pub const MAX_DOWNLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub const USER_AGENT: &str = concat!(
    "Veranda/",
    env!("CARGO_PKG_VERSION"),
    " (+https://localhost)"
);

#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

pub type ProgressFn = dyn Fn(Progress) + Send + Sync;

/// Cooperative cancellation for the one download that may run at a time: the UI sets the flag,
/// the chunk loop sees it and aborts with [`Cancelled`]. Reset by [`clear_cancel`] before a
/// new download starts.
static CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn request_cancel() {
    CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn clear_cancel() {
    CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
}

fn cancel_requested() -> bool {
    CANCEL.load(std::sync::atomic::Ordering::SeqCst)
}

/// The user cancelled a download; not a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("download cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Install the `ring` crypto provider once (smaller than aws-lc, no extra toolchain needs).
fn init_tls() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub fn client() -> Result<reqwest::Client> {
    init_tls();
    Ok(reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .https_only(true)
        // HTTP/1.1 only: several download CDNs (static-php.dev among them) reset HTTP/2 streams
        // part-way through large files, and nothing here gains from HTTP/2.
        .http1_only()
        .connect_timeout(Duration::from_secs(20))
        // No overall deadline: a big archive on a slow link must not fail while it still makes
        // progress. Stalls are caught by the read timeout instead.
        .read_timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?)
}

/// Same client settings but HTTP/1.1 only: some CDNs reset HTTP/2 streams mid-body
/// ("stream error ... unexpected internal error"); a retry over HTTP/1.1 gets through.
pub fn client_http1() -> Result<reqwest::Client> {
    init_tls();
    Ok(reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .https_only(true)
        .http1_only()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?)
}

fn is_transient(e: &anyhow::Error) -> bool {
    let t = format!("{e:#}").to_ascii_lowercase();
    [
        "stream error",
        "body error",
        "reading a body",
        "connection reset",
        "connection closed",
        "timed out",
        "broken pipe",
        "502",
        "503",
        "504",
        "incomplete message",
    ]
    .iter()
    .any(|k| t.contains(k))
}

/// Fetch a small text document (checksum files, JSON APIs). Capped at 8 MiB. Transient network
/// errors are retried twice, the retries over HTTP/1.1.
pub async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String> {
    require_https(url)?;
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..3u32 {
        let c = if attempt == 0 {
            client.clone()
        } else {
            client_http1()?
        };
        match fetch_text_once(&c, url).await {
            Ok(t) => return Ok(t),
            Err(e) if attempt < 2 && is_transient(&e) => {
                tracing::debug!("{url}: attempt {} failed ({e:#}); retrying", attempt + 1);
                tokio::time::sleep(Duration::from_millis(400 * (attempt as u64 + 1))).await;
                last = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("{url}: request failed")))
}

async fn fetch_text_once(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = client.get(url).send().await?.error_for_status()?;
    if let Some(len) = resp.content_length()
        && len > 8 * 1024 * 1024
    {
        bail!("{url}: response too large");
    }
    let bytes = resp.bytes().await?;
    if bytes.len() > 8 * 1024 * 1024 {
        bail!("{url}: response too large");
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn require_https(url: &str) -> Result<()> {
    if !url.starts_with("https://") {
        bail!("refusing non-https download: {url}");
    }
    Ok(())
}

/// Download `url` into `dest` (final path), verify `checksum`, return the hex SHA-256.
/// Download, verify and move into place. Transient network failures (stream resets, dropped
/// connections, timeouts, 5xx) are retried, resuming the partial file with an HTTP `Range`
/// request when the server allows it, for as long as each attempt makes progress.
/// Cancellation and checksum mismatches are not retried.
pub async fn download(
    client: &reqwest::Client,
    layout: &Layout,
    url: &str,
    dest: &Path,
    checksum: &Checksum,
    progress: Option<&ProgressFn>,
) -> Result<String> {
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let mut last: Option<anyhow::Error> = None;
    let mut stalls = 0u32;
    for attempt in 0..12u32 {
        let before = if attempt == 0 { 0 } else { size(&part) };
        match download_once(client, layout, url, dest, checksum, progress, attempt > 0).await {
            Ok(v) => return Ok(v),
            Err(e) if e.downcast_ref::<Cancelled>().is_none() && is_transient(&e) => {
                let after = size(&part);
                stalls = if after > before { 0 } else { stalls + 1 };
                if stalls >= 3 {
                    let _ = tokio::fs::remove_file(&part).await;
                    return Err(e.context("the server keeps dropping the connection"));
                }
                tracing::warn!(
                    "{url}: attempt {} failed after {after} bytes ({e:#}); resuming",
                    attempt + 1
                );
                tokio::time::sleep(Duration::from_millis(700 * (stalls as u64 + 1))).await;
                last = Some(e);
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(e);
            }
        }
    }
    let _ = tokio::fs::remove_file(&part).await;
    Err(last.unwrap_or_else(|| anyhow::anyhow!("{url}: download failed")))
}

async fn download_once(
    client: &reqwest::Client,
    layout: &Layout,
    url: &str,
    dest: &Path,
    checksum: &Checksum,
    progress: Option<&ProgressFn>,
    resume: bool,
) -> Result<String> {
    require_https(url)?;
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let part = PathBuf::from(format!("{}.part", dest.display()));

    // Bytes already on disk from a previous attempt; resumed with a Range request when the
    // server answers 206, otherwise the file starts over.
    let existing = if resume {
        tokio::fs::metadata(&part)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };
    let mut req = client.get(url);
    if existing > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={existing}-"));
    }
    let resp = req.send().await.with_context(|| format!("GET {url}"))?;
    let resp = resp
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    let append = existing > 0 && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let total = if append {
        resp.headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|t| t.trim().parse::<u64>().ok())
    } else {
        resp.content_length()
    };
    if let Some(t) = total
        && t > MAX_DOWNLOAD_BYTES
    {
        bail!("{url}: {t} bytes exceeds the download limit");
    }

    let mut sha256 = Sha256::new();
    let mut sha512 = Sha512::new();
    let mut md5 = Md5::new();
    let mut downloaded: u64 = 0;
    let mut file = if append {
        // the hashes must cover the bytes already on disk
        let mut f = tokio::fs::File::open(&part).await?;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut f, &mut buf).await?;
            if n == 0 {
                break;
            }
            sha256.update(&buf[..n]);
            sha512.update(&buf[..n]);
            md5.update(&buf[..n]);
            downloaded += n as u64;
        }
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(&part)
            .await?
    } else {
        tokio::fs::File::create(&part).await?
    };
    if let Some(cb) = progress {
        cb(Progress { downloaded, total });
    }
    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await? {
        if cancel_requested() {
            drop(file);
            let _ = tokio::fs::remove_file(&part).await;
            return Err(Cancelled.into());
        }
        downloaded += chunk.len() as u64;
        if downloaded > MAX_DOWNLOAD_BYTES {
            let _ = tokio::fs::remove_file(&part).await;
            bail!("{url}: exceeded the download limit");
        }
        sha256.update(&chunk);
        sha512.update(&chunk);
        md5.update(&chunk);
        file.write_all(&chunk).await?;
        if let Some(cb) = progress {
            cb(Progress { downloaded, total });
        }
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);

    let got256 = hex::encode(sha256.finalize());
    let got512 = hex::encode(sha512.finalize());
    let gotmd5 = hex::encode(md5.finalize());

    let verify = async {
        match checksum {
            Checksum::Sha256 { hex: want } => expect_eq("sha256", want, &got256, url),
            Checksum::Sha512 { hex: want } => expect_eq("sha512", want, &got512, url),
            Checksum::Md5 { hex: want } => {
                tracing::warn!(
                    "{url}: upstream only offers MD5; also recording SHA-256 for future checks"
                );
                expect_eq("md5", want, &gotmd5, url)
            }
            Checksum::Remote {
                url: list_url,
                name,
            } => {
                let text = fetch_text(client, list_url).await?;
                let want = find_in_checksum_file(&text, name)
                    .with_context(|| format!("{name} not found in {list_url}"))?;
                match want.len() {
                    64 => expect_eq("sha256", &want, &got256, url),
                    128 => expect_eq("sha512", &want, &got512, url),
                    32 => expect_eq("md5", &want, &gotmd5, url),
                    n => bail!("unsupported checksum length {n} in {list_url}"),
                }
            }
            Checksum::Tofu => Ok(()),
        }
    };
    if let Err(e) = verify.await {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }

    // Trust-on-first-use ledger applies to every URL, not only TOFU ones.
    if let Err(e) = Ledger::load(layout)?.check_and_record(layout, url, &got256) {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }

    tokio::fs::rename(&part, dest).await?;
    Ok(got256)
}

fn expect_eq(algo: &str, want: &str, got: &str, url: &str) -> Result<()> {
    if want.trim().eq_ignore_ascii_case(got) {
        Ok(())
    } else {
        bail!("{algo} mismatch for {url}\n  expected {want}\n  got      {got}")
    }
}

/// Find the hash for `name` in a checksum listing. Supports `hash  name`, `hash *name`,
/// `name: hash`, and the Apache Lounge style `SHA256-Checksum for: name:` followed by the hash.
pub fn find_in_checksum_file(text: &str, name: &str) -> Option<String> {
    let is_hex =
        |s: &str| matches!(s.len(), 32 | 64 | 128) && s.chars().all(|c| c.is_ascii_hexdigit());
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim_start_matches('\u{feff}').trim())
        .collect();
    for (i, line) in lines.iter().enumerate() {
        if !line.contains(name) {
            continue;
        }
        for tok in
            line.split(|c: char| c.is_whitespace() || c == '*' || c == '(' || c == ')' || c == '=')
        {
            if is_hex(tok) {
                return Some(tok.to_ascii_lowercase());
            }
        }
        // "SHA256-Checksum for: name:" + hash on the next non-empty line
        if line.to_ascii_uppercase().contains("SHA256") {
            for next in lines.iter().skip(i + 1) {
                if next.is_empty() {
                    continue;
                }
                if is_hex(next) && next.len() == 64 {
                    return Some(next.to_ascii_lowercase());
                }
                break;
            }
        }
    }
    None
}

/// Persistent `url -> sha256` ledger.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Ledger {
    #[serde(default)]
    pub entries: BTreeMap<String, String>,
}

impl Ledger {
    pub fn load(layout: &Layout) -> Result<Self> {
        match std::fs::read_to_string(layout.checksums_file()) {
            Ok(t) => Ok(serde_json::from_str(&t).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn check_and_record(mut self, layout: &Layout, url: &str, sha256: &str) -> Result<()> {
        if let Some(seen) = self.entries.get(url) {
            if !seen.eq_ignore_ascii_case(sha256) {
                bail!(
                    "content of {url} changed since it was first downloaded\n  before {seen}\n  now    {sha256}\nIf this is expected, remove the entry from {}",
                    layout.checksums_file().display()
                );
            }
            return Ok(());
        }
        self.entries.insert(url.to_string(), sha256.to_string());
        let text = serde_json::to_string_pretty(&self)?;
        crate::fsutil::write_atomic(&layout.checksums_file(), text.as_bytes())
    }
}

/// SHA-256 of a local file, hex encoded.
pub fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_formats() {
        let t = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd  caddy_linux.tar.gz\n";
        assert_eq!(
            find_in_checksum_file(t, "caddy_linux.tar.gz")
                .unwrap()
                .len(),
            64
        );
        let t2 = "\u{feff}Checksums\n\nSHA1-Checksum for: x.zip:\nB7372A7DE3E05FD0D11C4E72FF80A11AF0EF4FA5\n\nSHA256-Checksum for: x.zip:\n2d2e13c735366d318425c78e4ee2cc8fc648d77faba3ddea2cd516e43885733f\n";
        assert_eq!(
            find_in_checksum_file(t2, "x.zip").unwrap(),
            "b7372a7de3e05fd0d11c4e72ff80a11af0ef4fa5"
                .to_string()
                .chars()
                .take(0)
                .collect::<String>()
                + "2d2e13c735366d318425c78e4ee2cc8fc648d77faba3ddea2cd516e43885733f"
        );
        let t3 = "e05aaa40639d8548e0f68b7a70685a3b  mysql-9.7.4-winx64.zip";
        assert_eq!(
            find_in_checksum_file(t3, "mysql-9.7.4-winx64.zip")
                .unwrap()
                .len(),
            32
        );
        assert!(find_in_checksum_file(t3, "other.zip").is_none());
    }
}
