//! SSH key management: generate, import, export, unlock.
//!
//! Private keys are stored one per file in `etc/remote/keys/<id>.key` (OpenSSH format, private
//! permissions), optionally passphrase-protected. Only metadata and the public key go into the
//! store.

use crate::store::KeyMeta;
use anyhow::{Context, Result, bail};
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, EcdsaCurve, HashAlg, PrivateKey, PublicKey};
use std::path::{Path, PathBuf};

pub fn key_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.key"))
}

/// `ed25519` (default), `rsa` (4096 bit) or `ecdsa` (P-256).
pub fn algorithm(name: &str) -> Result<Algorithm> {
    Ok(match name.trim().to_ascii_lowercase().as_str() {
        "" | "ed25519" | "ssh-ed25519" => Algorithm::Ed25519,
        "rsa" | "ssh-rsa" => Algorithm::Rsa { hash: None },
        "ecdsa" | "ecdsa-sha2-nistp256" | "p256" => Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        },
        other => bail!("unsupported key type {other}"),
    })
}

pub fn fingerprint(pk: &PublicKey) -> String {
    pk.fingerprint(HashAlg::Sha256).to_string()
}

fn meta_for(id: &str, label: &str, key: &PrivateKey, encrypted: bool) -> Result<KeyMeta> {
    let pk = key.public_key();
    Ok(KeyMeta {
        id: id.to_string(),
        label: label.to_string(),
        algorithm: pk.algorithm().to_string(),
        fingerprint: fingerprint(pk),
        public_key: pk.to_openssh()?,
        encrypted,
        has_passphrase: false,
        created_at: crate::now(),
    })
}

fn write_key(dir: &Path, id: &str, text: &str) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    veranda_core::fsutil::make_private_dir(dir)?;
    veranda_core::fsutil::write_private(&key_file(dir, id), text.as_bytes())
}

/// Generate a new key pair; the private key is encrypted when a passphrase is given.
pub fn generate(dir: &Path, label: &str, kind: &str, passphrase: Option<&str>) -> Result<KeyMeta> {
    let algo = algorithm(kind)?;
    let mut rng = rand::rng();
    let mut key = PrivateKey::random(&mut rng, algo).context("generating key")?;
    key.set_comment(if label.trim().is_empty() {
        "veranda".to_string()
    } else {
        label.trim().to_string()
    });
    let id = crate::new_id();
    let encrypted = matches!(passphrase, Some(p) if !p.is_empty());
    let text = if let Some(p) = passphrase.filter(|p| !p.is_empty()) {
        key.encrypt(&mut rng, p.as_bytes())
            .context("encrypting key")?
            .to_openssh(LineEnding::LF)?
    } else {
        key.to_openssh(LineEnding::LF)?
    };
    write_key(dir, &id, &text)?;
    meta_for(&id, label, &key, encrypted)
}

/// Import a private key given as text (OpenSSH, PKCS#8 or PEM). An encrypted key needs its
/// passphrase once, to read the public half; the file is stored as given (still encrypted).
pub fn import(dir: &Path, label: &str, text: &str, passphrase: Option<&str>) -> Result<KeyMeta> {
    let text = text.trim().to_string() + "\n";
    let encrypted = looks_encrypted(&text);
    let key = match russh::keys::decode_secret_key(&text, passphrase.filter(|p| !p.is_empty())) {
        Ok(k) => k,
        Err(e) if encrypted && passphrase.is_none_or(|p| p.is_empty()) => {
            bail!("this key is passphrase-protected; enter its passphrase ({e})")
        }
        Err(e) => bail!("cannot read the key: {e}"),
    };
    let id = crate::new_id();
    write_key(dir, &id, &text)?;
    meta_for(&id, label, &key, encrypted)
}

/// Heuristic used before decoding: OpenSSH keys carry their cipher in the blob, PEM keys say so.
pub fn looks_encrypted(text: &str) -> bool {
    if text.contains("ENCRYPTED") {
        return true;
    }
    PrivateKey::from_openssh(text)
        .map(|k| k.is_encrypted())
        .unwrap_or(false)
}

/// Decode the stored private key, deciphering it with the passphrase when needed.
pub fn load(dir: &Path, id: &str, passphrase: Option<&str>) -> Result<PrivateKey> {
    let path = key_file(dir, id);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading key {}", path.display()))?;
    russh::keys::decode_secret_key(&text, passphrase.filter(|p| !p.is_empty())).map_err(|e| {
        if looks_encrypted(&text) {
            anyhow::anyhow!("wrong passphrase ({e})")
        } else {
            anyhow::anyhow!("cannot read the key: {e}")
        }
    })
}

/// The private key file as text (for "Export private key").
pub fn export_private(dir: &Path, id: &str) -> Result<String> {
    let path = key_file(dir, id);
    std::fs::read_to_string(&path).with_context(|| format!("reading key {}", path.display()))
}

/// Re-encrypt (or decrypt, with an empty passphrase) the private key file.
pub fn change_passphrase(
    dir: &Path,
    id: &str,
    current: Option<&str>,
    new: Option<&str>,
) -> Result<bool> {
    let key = load(dir, id, current)?;
    let mut rng = rand::rng();
    let (text, encrypted) = match new.filter(|p| !p.is_empty()) {
        Some(p) => (
            key.encrypt(&mut rng, p.as_bytes())?
                .to_openssh(LineEnding::LF)?
                .as_str()
                .to_owned(),
            true,
        ),
        None => (key.to_openssh(LineEnding::LF)?.as_str().to_owned(), false),
    };
    write_key(dir, id, &text)?;
    Ok(encrypted)
}

pub fn delete(dir: &Path, id: &str) -> Result<()> {
    let path = key_file(dir, id);
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

/// Candidate private keys in `~/.ssh` (for the import dialog).
pub fn user_ssh_keys() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return vec![];
    };
    let dir = home.join(".ssh");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            p.is_file()
                && name.starts_with("id_")
                && !name.ends_with(".pub")
                && std::fs::read_to_string(p)
                    .map(|t| t.contains("PRIVATE KEY"))
                    .unwrap_or(false)
        })
        .collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_import_unlock_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("keys");
        let m = generate(&d, "test key", "ed25519", None).unwrap();
        assert_eq!(m.algorithm, "ssh-ed25519");
        assert!(m.fingerprint.starts_with("SHA256:"));
        assert!(m.public_key.starts_with("ssh-ed25519 "));
        assert!(!m.encrypted);
        let k = load(&d, &m.id, None).unwrap();
        assert_eq!(fingerprint(k.public_key()), m.fingerprint);

        let enc = generate(&d, "locked", "ecdsa", Some("s3cret")).unwrap();
        assert!(enc.encrypted);
        assert!(load(&d, &enc.id, None).is_err());
        assert!(load(&d, &enc.id, Some("nope")).is_err());
        let k2 = load(&d, &enc.id, Some("s3cret")).unwrap();
        assert_eq!(fingerprint(k2.public_key()), enc.fingerprint);
        assert!(!change_passphrase(&d, &enc.id, Some("s3cret"), None).unwrap());
        assert!(load(&d, &enc.id, None).is_ok());

        // import what we exported (encrypted) with the passphrase
        let text = export_private(&d, &m.id).unwrap();
        let again = import(&d, "copy", &text, None).unwrap();
        assert_eq!(again.fingerprint, m.fingerprint);
        delete(&d, &again.id).unwrap();
        assert!(!key_file(&d, &again.id).exists());
        assert!(import(&d, "bad", "not a key", None).is_err());
    }
}
