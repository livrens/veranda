//! Snippet lock: snippet bodies encrypted at rest with a key derived from a password the user
//! chooses. PBKDF2-HMAC-SHA256 (600 000 rounds) turns the password and a random salt into 64
//! bytes: the first half is the AES-256-GCM key, the second half is a verifier kept in
//! `etc/remote/snippet-lock.json` so a wrong password can be told apart from a corrupt file
//! without revealing the key. The key itself only ever lives in memory while unlocked.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ring::rand::{SecureRandom, SystemRandom};
use ring::{aead, pbkdf2};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use veranda_core::paths::Layout;

pub const ITERATIONS: u32 = 600_000;
/// Shortest password accepted.
pub const MIN_PASSWORD: usize = 6;
const PREFIX: &str = "v1:";

/// What is written to disk: everything needed to check a password, nothing that recovers the key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockFile {
    pub version: u32,
    pub salt: String,
    pub iterations: u32,
    pub verifier: String,
    /// Lock again after this many minutes without use (0 = only when Veranda closes).
    #[serde(default = "default_minutes")]
    pub auto_lock_minutes: u32,
}

fn default_minutes() -> u32 {
    15
}

/// The unlocked AES-256-GCM key. Wiped when dropped.
pub struct Key {
    raw: [u8; 32],
}

impl Drop for Key {
    fn drop(&mut self) {
        for b in self.raw.iter_mut() {
            // volatile so the wipe is not optimised away
            unsafe { std::ptr::write_volatile(b, 0) };
        }
    }
}

pub fn path(layout: &Layout) -> PathBuf {
    layout.etc().join("remote").join("snippet-lock.json")
}

fn derive(password: &str, salt: &[u8], iterations: u32) -> [u8; 64] {
    let mut out = [0u8; 64];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(iterations.max(1)).expect("non-zero"),
        salt,
        password.as_bytes(),
        &mut out,
    );
    out
}

/// Constant-time comparison (no early exit on the first differing byte).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn random(n: usize) -> Result<Vec<u8>> {
    let mut v = vec![0u8; n];
    SystemRandom::new()
        .fill(&mut v)
        .map_err(|_| anyhow::anyhow!("no random bytes"))?;
    Ok(v)
}

impl LockFile {
    /// Make a new lock for `password` and return the key that goes with it.
    pub fn create(password: &str, auto_lock_minutes: u32) -> Result<(LockFile, Key)> {
        Self::create_with(password, auto_lock_minutes, ITERATIONS)
    }

    pub fn create_with(
        password: &str,
        auto_lock_minutes: u32,
        iterations: u32,
    ) -> Result<(LockFile, Key)> {
        if password.chars().count() < MIN_PASSWORD {
            bail!("the password needs at least {MIN_PASSWORD} characters");
        }
        let salt = random(16)?;
        let d = derive(password, &salt, iterations);
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&d[..32]);
        Ok((
            LockFile {
                version: 1,
                salt: hex::encode(&salt),
                iterations,
                verifier: hex::encode(&d[32..]),
                auto_lock_minutes,
            },
            Key { raw },
        ))
    }

    /// The key when `password` is right, `None` otherwise.
    pub fn unlock(&self, password: &str) -> Result<Option<Key>> {
        let salt = hex::decode(&self.salt).context("salt in snippet-lock.json")?;
        let verifier = hex::decode(&self.verifier).context("verifier in snippet-lock.json")?;
        let d = derive(password, &salt, self.iterations);
        if !ct_eq(&d[32..], &verifier) {
            return Ok(None);
        }
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&d[..32]);
        Ok(Some(Key { raw }))
    }

    pub fn load(layout: &Layout) -> Result<Option<LockFile>> {
        Self::load_from(&path(layout))
    }

    pub fn load_from(file: &Path) -> Result<Option<LockFile>> {
        if !file.exists() {
            return Ok(None);
        }
        let text =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        Ok(Some(
            serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))?,
        ))
    }

    pub fn save(&self, layout: &Layout) -> Result<()> {
        self.save_to(&path(layout))
    }

    pub fn save_to(&self, file: &Path) -> Result<()> {
        if let Some(p) = file.parent() {
            std::fs::create_dir_all(p)?;
            veranda_core::fsutil::make_private_dir(p)?;
        }
        veranda_core::fsutil::write_private(file, &serde_json::to_vec_pretty(self)?)
    }

    pub fn remove(layout: &Layout) -> Result<()> {
        let p = path(layout);
        if p.exists() {
            std::fs::remove_file(&p).with_context(|| format!("removing {}", p.display()))?;
        }
        Ok(())
    }
}

impl Key {
    fn aead(&self) -> Result<aead::LessSafeKey> {
        let k = aead::UnboundKey::new(&aead::AES_256_GCM, &self.raw)
            .map_err(|_| anyhow::anyhow!("bad key length"))?;
        Ok(aead::LessSafeKey::new(k))
    }

    /// Encrypt `plain`; the result is `v1:` + base64(nonce || ciphertext || tag).
    pub fn seal(&self, plain: &str) -> Result<String> {
        let nonce_bytes = random(12)?;
        let nonce = aead::Nonce::try_assume_unique_for_key(&nonce_bytes)
            .map_err(|_| anyhow::anyhow!("bad nonce"))?;
        let mut buf = plain.as_bytes().to_vec();
        self.aead()?
            .seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut buf)
            .map_err(|_| anyhow::anyhow!("encrypting the snippet"))?;
        let mut out = nonce_bytes;
        out.extend_from_slice(&buf);
        Ok(format!("{PREFIX}{}", B64.encode(out)))
    }

    /// Decrypt what [`Key::seal`] produced. Fails on the wrong key or a tampered blob.
    pub fn open(&self, blob: &str) -> Result<String> {
        let Some(b64) = blob.strip_prefix(PREFIX) else {
            bail!("unknown snippet encryption format");
        };
        let mut data = B64.decode(b64).context("decoding the encrypted snippet")?;
        if data.len() < 12 + aead::AES_256_GCM.tag_len() {
            bail!("encrypted snippet is too short");
        }
        let (nonce_bytes, rest) = data.split_at_mut(12);
        let nonce = aead::Nonce::try_assume_unique_for_key(nonce_bytes)
            .map_err(|_| anyhow::anyhow!("bad nonce"))?;
        let plain = self
            .aead()?
            .open_in_place(nonce, aead::Aad::empty(), rest)
            .map_err(|_| anyhow::anyhow!("wrong password or damaged snippet"))?;
        String::from_utf8(plain.to_vec()).context("snippet is not UTF-8")
    }
}

/// `true` when `s` looks like a sealed blob.
pub fn is_sealed(s: &str) -> bool {
    s.starts_with(PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_roundtrip_and_wrong_password() {
        let (lock, key) = LockFile::create_with("hunter22", 15, 1000).unwrap();
        assert!(LockFile::create_with("short", 15, 1000).is_err());
        let blob = key.seal("echo hi\n").unwrap();
        assert!(is_sealed(&blob));
        assert_eq!(key.open(&blob).unwrap(), "echo hi\n");
        assert!(lock.unlock("hunter23").unwrap().is_none());
        let again = lock.unlock("hunter22").unwrap().expect("right password");
        assert_eq!(again.open(&blob).unwrap(), "echo hi\n");
        // another key or a flipped byte must not decrypt
        let (_, other) = LockFile::create_with("hunter22", 15, 1000).unwrap();
        assert!(other.open(&blob).is_err());
        let mut bad = B64.decode(&blob[PREFIX.len()..]).unwrap();
        bad[14] ^= 1;
        assert!(key.open(&format!("{PREFIX}{}", B64.encode(bad))).is_err());
        // file roundtrip
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("remote").join("snippet-lock.json");
        lock.save_to(&file).unwrap();
        assert_eq!(LockFile::load_from(&file).unwrap(), Some(lock));
        assert_eq!(
            LockFile::load_from(&dir.path().join("none.json")).unwrap(),
            None
        );
    }
}
