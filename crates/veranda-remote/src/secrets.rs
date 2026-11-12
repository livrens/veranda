//! Passwords and passphrases.
//!
//! Preferred backend: the operating system's credential store through the `keyring` crate
//! (macOS Keychain, Windows Credential Manager, Secret Service / KWallet on Linux). When that
//! store is unavailable (no Secret Service on a headless Linux, access refused), secrets are kept
//! in `secrets/remote-secrets.json`, a file readable only by the current user. The UI shows which
//! backend is in use so the user can decide.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use veranda_core::paths::Layout;

const SERVICE: &str = "Veranda";
/// How long a credential-store call may block (KWallet/GNOME Keyring may show a consent dialog).
const STORE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Run a keyring call on its own thread so a store that never answers cannot freeze the app.
/// `None` = timed out.
fn keyring_op<T: Send + 'static>(
    f: impl FnOnce() -> keyring::Result<T> + Send + 'static,
) -> Option<keyring::Result<T>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("veranda-keyring".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .ok()?;
    rx.recv_timeout(STORE_TIMEOUT).ok()
}

pub struct Secrets {
    file: PathBuf,
    /// `true` once the OS store failed for anything but "no such entry".
    file_only: Mutex<bool>,
}

impl Secrets {
    pub fn new(layout: &Layout) -> Self {
        Self {
            file: layout.secrets().join("remote-secrets.json"),
            file_only: Mutex::new(keyring::Entry::store_status().is_err()),
        }
    }

    /// `keyring` or `file`.
    pub fn backend(&self) -> &'static str {
        if *self.file_only.lock().unwrap_or_else(|e| e.into_inner()) {
            "file"
        } else {
            "keyring"
        }
    }

    fn file_only(&self) -> bool {
        *self.file_only.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn degrade(&self, why: &str) {
        tracing::warn!("OS credential store unavailable ({why}); using the private secrets file");
        *self.file_only.lock().unwrap_or_else(|e| e.into_inner()) = true;
    }

    fn read_file(&self) -> Result<BTreeMap<String, String>> {
        if !self.file.exists() {
            return Ok(BTreeMap::new());
        }
        let text = std::fs::read_to_string(&self.file)
            .with_context(|| format!("reading {}", self.file.display()))?;
        Ok(serde_json::from_str(&text).unwrap_or_default())
    }

    fn write_file(&self, map: &BTreeMap<String, String>) -> Result<()> {
        if let Some(p) = self.file.parent() {
            std::fs::create_dir_all(p)?;
            veranda_core::fsutil::make_private_dir(p)?;
        }
        veranda_core::fsutil::write_private(&self.file, &serde_json::to_vec_pretty(map)?)
    }

    pub fn get(&self, key: &str) -> Result<Option<String>> {
        if !self.file_only() {
            let k = key.to_string();
            match keyring_op(move || {
                keyring::Entry::new(SERVICE, &k).and_then(|e| e.get_password())
            }) {
                Some(Ok(v)) => return Ok(Some(v)),
                Some(Err(keyring::Error::NoEntry)) => {}
                Some(Err(e)) => self.degrade(&e.to_string()),
                None => self.degrade("no answer within the time limit"),
            }
        }
        Ok(self.read_file()?.get(key).cloned())
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        if !self.file_only() {
            let (k, v) = (key.to_string(), value.to_string());
            match keyring_op(move || {
                keyring::Entry::new(SERVICE, &k).and_then(|e| e.set_password(&v))
            }) {
                Some(Ok(())) => {
                    // a stale copy in the fallback file must not shadow the keyring value
                    let mut map = self.read_file()?;
                    if map.remove(key).is_some() {
                        self.write_file(&map)?;
                    }
                    return Ok(());
                }
                Some(Err(e)) => self.degrade(&e.to_string()),
                None => self.degrade("no answer within the time limit"),
            }
        }
        let mut map = self.read_file()?;
        map.insert(key.to_string(), value.to_string());
        self.write_file(&map)
    }

    pub fn delete(&self, key: &str) -> Result<()> {
        if !self.file_only() {
            let k = key.to_string();
            match keyring_op(move || {
                keyring::Entry::new(SERVICE, &k).and_then(|e| e.delete_credential())
            }) {
                Some(Ok(())) | Some(Err(keyring::Error::NoEntry)) => {}
                Some(Err(e)) => self.degrade(&e.to_string()),
                None => self.degrade("no answer within the time limit"),
            }
        }
        let mut map = self.read_file()?;
        if map.remove(key).is_some() {
            self.write_file(&map)?;
        }
        Ok(())
    }

    /// Store or clear a secret in one call (`None`/empty clears).
    pub fn put(&self, key: &str, value: Option<&str>) -> Result<bool> {
        match value {
            Some(v) if !v.is_empty() => {
                self.set(key, v)?;
                Ok(true)
            }
            _ => {
                self.delete(key)?;
                Ok(false)
            }
        }
    }
}

/// Every secret the store refers to, keyed like the credential store, for moving to another
/// computer. Entries that are missing (never saved, or the OS store refused) are left out.
pub fn export_all(
    secrets: &Secrets,
    store: &crate::store::Store,
) -> Result<BTreeMap<String, String>> {
    let mut keys: Vec<String> = vec![];
    for h in store.hosts.iter().filter(|h| h.has_password) {
        keys.push(host_password_key(&h.id));
    }
    for i in store.identities.iter().filter(|i| i.has_password) {
        keys.push(identity_password_key(&i.id));
    }
    for k in store.keys.iter().filter(|k| k.has_passphrase) {
        keys.push(key_passphrase_key(&k.id));
    }
    let mut out = BTreeMap::new();
    for k in keys {
        if let Some(v) = secrets.get(&k)? {
            out.insert(k, v);
        }
    }
    Ok(out)
}

/// Store exported secrets on this computer. Returns how many were written.
pub fn import_all(secrets: &Secrets, map: &BTreeMap<String, String>) -> Result<usize> {
    let mut n = 0;
    for (k, v) in map {
        if !v.is_empty()
            && (k.starts_with("host:") || k.starts_with("identity:") || k.starts_with("key:"))
        {
            secrets.set(k, v)?;
            n += 1;
        }
    }
    Ok(n)
}

pub fn host_password_key(id: &str) -> String {
    format!("host:{id}:password")
}
pub fn identity_password_key(id: &str) -> String {
    format!("identity:{id}:password")
}
pub fn key_passphrase_key(id: &str) -> String {
    format!("key:{id}:passphrase")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_backend_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path().to_path_buf());
        let s = Secrets {
            file: layout.secrets().join("remote-secrets.json"),
            file_only: Mutex::new(true),
        };
        assert_eq!(s.get("a").unwrap(), None);
        assert!(s.put("a", Some("pw")).unwrap());
        assert_eq!(s.get("a").unwrap().as_deref(), Some("pw"));
        assert!(!s.put("a", Some("")).unwrap());
        assert_eq!(s.get("a").unwrap(), None);
        assert_eq!(s.backend(), "file");
    }
}
