//! Application lock: a PIN or passphrase asked for before the window shows anything.
//!
//! What is stored: never the secret itself, only a salted PBKDF2-HMAC-SHA256 hash in
//! `etc/secrets/app-lock.json`, a file readable by the current user only. Verification is
//! constant-time (`ring::pbkdf2::verify`). The lock protects the Veranda window and its IPC
//! commands; it does not encrypt the files on disk - full-disk encryption (BitLocker, FileVault,
//! LUKS) is what protects data on a stolen device, and this lock complements it.

use crate::fsutil;
use crate::paths::Layout;
use anyhow::{Context, Result, bail};
use ring::pbkdf2;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;
use std::path::PathBuf;

const FILE: &str = "app-lock.json";
const ITERATIONS: u32 = 600_000;
const HASH_LEN: usize = 32;
pub const MIN_LEN: usize = 4;
pub const MAX_LEN: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockFile {
    pub version: u8,
    pub kdf: String,
    pub iterations: u32,
    pub salt: String,
    pub hash: String,
    /// Minutes of inactivity before the UI locks itself again; 0 = only manual lock.
    #[serde(default = "default_auto_lock")]
    pub auto_lock_minutes: u32,
    /// `pin` (digits, numeric keypad on the lock screen) or `password` (any text).
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Optional second factor: a base32 TOTP secret for an authenticator app (RFC 6238,
    /// SHA-1, 6 digits, 30 s). Lives in the same private file; it has to be readable to verify.
    #[serde(default)]
    pub totp: Option<String>,
}

fn default_kind() -> String {
    "pin".into()
}

fn default_auto_lock() -> u32 {
    10
}

pub fn path(layout: &Layout) -> PathBuf {
    layout.secrets().join(FILE)
}

pub fn is_enabled(layout: &Layout) -> bool {
    path(layout).is_file()
}

pub fn load(layout: &Layout) -> Result<Option<LockFile>> {
    let p = path(layout);
    if !p.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    let f: LockFile = serde_json::from_str(&text).context("app-lock.json is not valid")?;
    Ok(Some(f))
}

fn check_secret(secret: &str) -> Result<()> {
    let n = secret.chars().count();
    if n < MIN_LEN {
        bail!("the PIN or passphrase needs at least {MIN_LEN} characters");
    }
    if n > MAX_LEN {
        bail!("the passphrase is too long (max {MAX_LEN} characters)");
    }
    if secret.trim() != secret {
        bail!("no leading or trailing spaces");
    }
    Ok(())
}

/// Enable the lock or replace the secret. `auto_lock_minutes` 0 disables the idle timer. A
/// second factor that is already set up is kept.
pub fn set(layout: &Layout, secret: &str, auto_lock_minutes: u32, kind: &str) -> Result<()> {
    check_secret(secret)?;
    if kind == "pin" && !secret.chars().all(|c| c.is_ascii_digit()) {
        bail!("a PIN is digits only; choose \"password\" for letters and symbols");
    }
    let totp = load(layout)?.and_then(|f| f.totp);
    let salt: [u8; 16] = rand::random();
    let mut hash = [0u8; HASH_LEN];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(ITERATIONS).unwrap(),
        &salt,
        secret.as_bytes(),
        &mut hash,
    );
    let f = LockFile {
        version: 1,
        kdf: "pbkdf2-hmac-sha256".into(),
        iterations: ITERATIONS,
        salt: hex::encode(salt),
        hash: hex::encode(hash),
        auto_lock_minutes: auto_lock_minutes.min(24 * 60),
        kind: if kind == "password" {
            "password".into()
        } else {
            "pin".into()
        },
        totp,
    };
    let dir = layout.secrets();
    std::fs::create_dir_all(&dir)?;
    fsutil::make_private_dir(&dir)?;
    let p = path(layout);
    std::fs::write(&p, serde_json::to_string_pretty(&f)?)?;
    fsutil::make_private_file(&p)?;
    Ok(())
}

/// Change only the idle timer, keeping the secret.
pub fn set_auto_lock(layout: &Layout, minutes: u32) -> Result<()> {
    let Some(mut f) = load(layout)? else {
        bail!("the app lock is not enabled");
    };
    f.auto_lock_minutes = minutes.min(24 * 60);
    let p = path(layout);
    std::fs::write(&p, serde_json::to_string_pretty(&f)?)?;
    fsutil::make_private_file(&p)?;
    Ok(())
}

/// Store (`Some`) or remove (`None`) the authenticator secret.
pub fn set_totp(layout: &Layout, secret: Option<&str>) -> Result<()> {
    let Some(mut f) = load(layout)? else {
        bail!("the app lock is not enabled");
    };
    f.totp = secret.map(|s| s.to_string());
    let p = path(layout);
    std::fs::write(&p, serde_json::to_string_pretty(&f)?)?;
    fsutil::make_private_file(&p)?;
    Ok(())
}

/// A fresh 160-bit secret, base32 without padding (what authenticator apps expect).
pub fn totp_new_secret() -> String {
    let raw: [u8; 20] = rand::random();
    data_encoding::BASE32_NOPAD.encode(&raw)
}

/// `otpauth://` URI for the QR code or manual entry in an authenticator app.
pub fn totp_uri(secret: &str, account: &str) -> String {
    let acct: String = account
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "otpauth://totp/Veranda:{acct}?secret={secret}&issuer=Veranda&algorithm=SHA1&digits=6&period=30"
    )
}

/// The 6-digit code for one 30 s counter (RFC 6238 with HMAC-SHA1).
pub fn totp_code(secret: &str, counter: u64) -> Result<String> {
    let key = data_encoding::BASE32_NOPAD
        .decode(
            secret
                .trim()
                .trim_end_matches('=')
                .to_ascii_uppercase()
                .as_bytes(),
        )
        .context("the authenticator secret is not valid base32")?;
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &key);
    let tag = ring::hmac::sign(&key, &counter.to_be_bytes());
    let h = tag.as_ref();
    let off = (h[19] & 0x0f) as usize;
    let bin = ((h[off] as u32 & 0x7f) << 24)
        | ((h[off + 1] as u32) << 16)
        | ((h[off + 2] as u32) << 8)
        | (h[off + 3] as u32);
    Ok(format!("{:06}", bin % 1_000_000))
}

/// Check a code against the current counter and one step either side (clock drift). Returns
/// the matching counter so the caller can refuse a replay of the same code.
pub fn totp_verify(secret: &str, code: &str, now_secs: u64) -> Result<Option<u64>> {
    let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
    if code.len() != 6 {
        return Ok(None);
    }
    let base = now_secs / 30;
    for counter in [base, base.saturating_sub(1), base + 1] {
        let expected = totp_code(secret, counter)?;
        let diff = expected
            .bytes()
            .zip(code.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b));
        if diff == 0 && expected.len() == code.len() {
            return Ok(Some(counter));
        }
    }
    Ok(None)
}

/// Constant-time check. `Ok(false)` = wrong secret; `Ok(true)` when no lock is set.
pub fn verify(layout: &Layout, secret: &str) -> Result<bool> {
    let Some(f) = load(layout)? else {
        return Ok(true);
    };
    let salt = hex::decode(&f.salt).context("app-lock.json salt")?;
    let hash = hex::decode(&f.hash).context("app-lock.json hash")?;
    let iters = NonZeroU32::new(f.iterations.max(1)).unwrap();
    Ok(pbkdf2::verify(
        pbkdf2::PBKDF2_HMAC_SHA256,
        iters,
        &salt,
        secret.as_bytes(),
        &hash,
    )
    .is_ok())
}

/// Remove the lock (caller must have verified the current secret).
pub fn clear(layout: &Layout) -> Result<()> {
    let p = path(layout);
    if p.exists() {
        std::fs::remove_file(&p)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path().to_path_buf());
        assert!(!is_enabled(&layout));
        assert!(verify(&layout, "anything").unwrap());
        set(&layout, "1234", 5, "pin").unwrap();
        assert!(is_enabled(&layout));
        assert!(verify(&layout, "1234").unwrap());
        assert!(!verify(&layout, "1235").unwrap());
        assert!(set(&layout, "12", 5, "pin").is_err());
        assert!(
            set(&layout, "ab12", 5, "pin").is_err(),
            "pin must be digits"
        );
        set(&layout, "hunter2 is fine", 5, "password").unwrap();
        assert_eq!(load(&layout).unwrap().unwrap().kind, "password");
        set_totp(&layout, Some("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ")).unwrap();
        set(&layout, "9876", 5, "pin").unwrap();
        assert!(
            load(&layout).unwrap().unwrap().totp.is_some(),
            "second factor survives a new secret"
        );
        set_totp(&layout, None).unwrap();
        assert!(load(&layout).unwrap().unwrap().totp.is_none());
        set_auto_lock(&layout, 0).unwrap();
        assert_eq!(load(&layout).unwrap().unwrap().auto_lock_minutes, 0);
        clear(&layout).unwrap();
        assert!(!is_enabled(&layout));
    }

    #[test]
    fn totp_matches_rfc6238_vectors() {
        // RFC 6238 appendix B, SHA-1, secret "12345678901234567890" (8-digit codes there;
        // the last six digits are the 6-digit code).
        let secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
        assert_eq!(totp_code(secret, 59 / 30).unwrap(), "287082");
        assert_eq!(totp_code(secret, 1_111_111_109 / 30).unwrap(), "081804");
        assert_eq!(totp_code(secret, 1_234_567_890 / 30).unwrap(), "005924");
        assert_eq!(totp_verify(secret, "287082", 59).unwrap(), Some(1));
        assert_eq!(
            totp_verify(secret, "287 082", 75).unwrap(),
            Some(1),
            "spaces ignored, one step of drift accepted"
        );
        assert_eq!(totp_verify(secret, "000000", 59).unwrap(), None);
        assert!(totp_uri(secret, "QA IndoGold").contains("Veranda:QA_IndoGold?secret="));
        assert_eq!(totp_new_secret().len(), 32);
    }
}
