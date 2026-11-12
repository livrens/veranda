//! Self-update check against GitHub releases of the configured repository.

use crate::download::{client, fetch_text};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub published_at: String,
    pub notes: String,
    /// Downloadable files of the release (installers and their `.sig` files).
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
    /// The first asset whose name ends with `suffix` (and is not a signature).
    pub fn asset_ending_with(&self, suffix: &str) -> Option<&Asset> {
        self.assets
            .iter()
            .find(|a| a.name.ends_with(suffix) && !a.name.ends_with(".sig"))
    }
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    published_at: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

/// Check a Tauri-style signature: `pubkey_b64` and `sig_b64` are the base64 texts Tauri writes
/// into `tauri.conf.json` and the `.sig` files (minisign under the hood).
pub fn verify_signature(pubkey_b64: &str, sig_b64: &str, data: &[u8]) -> Result<()> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let pk = String::from_utf8(
        b64.decode(pubkey_b64.trim())
            .context("public key is not base64")?,
    )
    .context("public key is not text")?;
    let sig = String::from_utf8(
        b64.decode(sig_b64.trim())
            .context("signature is not base64")?,
    )
    .context("signature is not text")?;
    let pk = minisign_verify::PublicKey::decode(&pk).context("public key")?;
    let sig = minisign_verify::Signature::decode(&sig).context("signature")?;
    pk.verify(data, &sig, false)
        .map_err(|e| anyhow::anyhow!("signature does not match the download: {e}"))
}

/// Returns the newest release if it is newer than the running version.
pub async fn check(repo: &str) -> Result<Option<Release>> {
    if !crate::config::is_repo_slug(repo) {
        anyhow::bail!("update.repo is not configured (owner/repo)");
    }
    let c = client()?;
    let text = fetch_text(
        &c,
        &format!("https://api.github.com/repos/{repo}/releases/latest"),
    )
    .await?;
    let rel: GhRelease = serde_json::from_str(&text).context("parsing GitHub release")?;
    if rel.prerelease || rel.draft {
        return Ok(None);
    }
    let latest = semver::Version::parse(rel.tag_name.trim_start_matches('v'))
        .context("release tag is not semver")?;
    let current = semver::Version::parse(crate::VERSION)?;
    if latest > current {
        Ok(Some(Release {
            version: latest.to_string(),
            url: rel.html_url,
            published_at: rel.published_at,
            notes: rel.body,
            assets: rel
                .assets
                .into_iter()
                .map(|a| Asset {
                    name: a.name,
                    url: a.browser_download_url,
                    size: a.size,
                })
                .collect(),
        }))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_helper_rejects_garbage() {
        // Veranda's own updater public key (apps/desktop/src-tauri/tauri.conf.json)
        let conf: serde_json::Value = serde_json::from_str(include_str!(
            "../../../apps/desktop/src-tauri/tauri.conf.json"
        ))
        .unwrap();
        let pk = conf["plugins"]["updater"]["pubkey"].as_str().unwrap();
        assert!(verify_signature(pk, "bm90IGEgc2lnbmF0dXJl", b"data").is_err());
        assert!(verify_signature("??", "??", b"data").is_err());
    }

    /// Checks a real signed installer: `VERANDA_SIG_FILE=<installer> VERANDA_SIG_SIG=<.sig>`
    /// then `cargo test -p veranda-core --lib update -- --ignored`.
    #[test]
    #[ignore]
    fn verifies_a_real_release_file() {
        let conf: serde_json::Value = serde_json::from_str(include_str!(
            "../../../apps/desktop/src-tauri/tauri.conf.json"
        ))
        .unwrap();
        let pk = conf["plugins"]["updater"]["pubkey"].as_str().unwrap();
        let data = std::fs::read(std::env::var("VERANDA_SIG_FILE").unwrap()).unwrap();
        let sig = std::fs::read_to_string(std::env::var("VERANDA_SIG_SIG").unwrap()).unwrap();
        verify_signature(pk, &sig, &data).unwrap();
        let mut tampered = data.clone();
        tampered[100] ^= 1;
        assert!(verify_signature(pk, &sig, &tampered).is_err());
    }
}
