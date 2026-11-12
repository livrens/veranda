//! Remote access for Veranda: an SSH client in the spirit of Termius.
//!
//! * [`store`]: hosts, groups, identities, keys, snippets, port-forwarding rules, history and
//!   terminal settings, saved as one private JSON file under `etc/remote/`.
//! * [`secrets`]: passwords and passphrases in the OS credential store (Keychain, Credential
//!   Manager, Secret Service) with a private-file fallback.
//! * [`keys`]: generate/import/export SSH keys (private keys live in `etc/remote/keys/`).
//! * [`known_hosts`]: trust-on-first-use host keys in OpenSSH format.
//! * [`client`]: SSH connections (password, key, agent, keyboard-interactive, jump hosts).
//! * [`session`]: interactive terminals over SSH or a local PTY.
//! * [`vault`]: optional password lock that encrypts snippet bodies at rest.
//! * [`sftp`], [`forward`], [`import`], [`local`].
//!
//! Everything is pure Rust (russh + ring); no OpenSSH binary is needed.

pub mod client;
pub mod forward;
pub mod import;
pub mod keys;
pub mod known_hosts;
pub mod local;
pub mod secrets;
pub mod session;
pub mod sftp;
pub mod store;
pub mod vault;

/// The SSH key type used by [`client::AuthMethod::Key`] (re-exported so callers need no `russh` dependency).
pub use russh::keys::PrivateKey;
pub use store::Store;

/// Random 16-hex-character identifier for store items.
pub fn new_id() -> String {
    use rand::RngExt;
    let mut rng = rand::rng();
    let bytes: [u8; 8] = rng.random();
    hex::encode(bytes)
}

/// Unix time in seconds.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
