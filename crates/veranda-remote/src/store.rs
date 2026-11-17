//! Persistent remote data: hosts, groups, identities, keys, snippets, forwarding rules, history.
//!
//! One JSON file, `etc/remote/store.json`, written atomically with private permissions. Secrets
//! (passwords, passphrases) are never written here; see [`crate::secrets`].

use crate::vault::Lock;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use veranda_core::paths::Layout;

const LOCKED: &str = "snippets are locked; unlock them first";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    /// Use an identity (username + password/key) from the keychain.
    Identity,
    /// Password stored for this host (or asked when connecting).
    #[default]
    Password,
    /// A key from the keychain.
    Key,
    /// Keys offered by the running SSH agent (`SSH_AUTH_SOCK`, Pageant).
    Agent,
    /// No stored credential: the server decides (keyboard-interactive / password prompt).
    None,
}

/// One entry of a host's startup sequence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartupStep {
    /// Send this snippet.
    Snippet { id: String },
    /// Answer a password prompt with the host's saved password (kept in the secret store only).
    SudoPassword,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Host {
    pub id: String,
    pub label: String,
    pub address: String,
    pub port: u16,
    pub username: String,
    pub group_id: Option<String>,
    pub tags: Vec<String>,
    /// Colour tag shown in lists (CSS colour or empty).
    pub color: String,
    pub auth: AuthKind,
    pub identity_id: Option<String>,
    pub key_id: Option<String>,
    /// A saved password exists in the secret store under `host:<id>:password`.
    pub has_password: bool,
    /// Connect through this host first (ProxyJump).
    pub jump_host_id: Option<String>,
    /// Sent to the terminal after login, in this order.
    pub startup_steps: Vec<StartupStep>,
    /// Single startup snippet of files written before 1.0.16; read only and folded into
    /// `startup_steps` on load.
    #[serde(skip_serializing)]
    pub startup_snippet_id: Option<String>,
    pub keep_alive: bool,
    pub agent_forwarding: bool,
    /// Environment variables requested from the server (`name=value`).
    pub env: Vec<String>,
    pub notes: String,
    /// Operating system reported by the server (`uname -sr`), filled after the first login.
    pub os: String,
    pub created_at: u64,
    pub last_connected: u64,
}

impl Default for Host {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            address: String::new(),
            port: 22,
            username: String::new(),
            group_id: None,
            tags: vec![],
            color: String::new(),
            auth: AuthKind::Password,
            identity_id: None,
            key_id: None,
            has_password: false,
            jump_host_id: None,
            startup_steps: vec![],
            startup_snippet_id: None,
            keep_alive: true,
            agent_forwarding: false,
            env: vec![],
            notes: String::new(),
            os: String::new(),
            created_at: 0,
            last_connected: 0,
        }
    }
}

impl Host {
    /// Turn the old single startup snippet into a one-step list.
    fn fold_startup(&mut self) {
        if let Some(id) = self.startup_snippet_id.take()
            && self.startup_steps.is_empty()
        {
            self.startup_steps.push(StartupStep::Snippet { id });
        }
    }

    /// `user@host:port` style summary.
    pub fn summary(&self) -> String {
        let user = if self.username.is_empty() {
            String::new()
        } else {
            format!("{}@", self.username)
        };
        if self.port == 22 {
            format!("{user}{}", self.address)
        } else {
            format!("{user}{}:{}", self.address, self.port)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    /// Hosts in the group without their own identity use this one.
    pub identity_id: Option<String>,
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Identity {
    pub id: String,
    pub label: String,
    pub username: String,
    /// A password exists under `identity:<id>:password`.
    pub has_password: bool,
    pub key_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct KeyMeta {
    pub id: String,
    pub label: String,
    /// `ssh-ed25519`, `ssh-rsa`, `ecdsa-sha2-nistp256`...
    pub algorithm: String,
    /// `SHA256:...` of the public key.
    pub fingerprint: String,
    /// One-line OpenSSH public key.
    pub public_key: String,
    /// The private key file is passphrase-protected.
    pub encrypted: bool,
    /// The passphrase is saved under `key:<id>:passphrase`.
    pub has_passphrase: bool,
    pub created_at: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SnippetKind {
    /// Text typed into a terminal or run on hosts.
    #[default]
    Command,
    /// A secret: stored sealed only, never sent to the UI, typed only at a password prompt.
    Password,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Snippet {
    pub id: String,
    pub label: String,
    /// Package/folder name used to group snippets.
    pub folder: String,
    /// Missing in files written before 1.0.16: read as a command.
    pub kind: SnippetKind,
    /// The command text. Empty while the body is stored encrypted in `script_enc`, and in the
    /// data sent to the UI when the snippet lock is on and not unlocked. Always empty for a
    /// password snippet, on disk and in the UI.
    pub script: String,
    /// `script` sealed by [`crate::vault::Key::seal`] when a snippet lock is set.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub script_enc: String,
}

/// Why a snippet was not saved or run, or the snippet lock not removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnippetRefusal {
    /// A password snippet needs a snippet lock: it is never stored as plain text.
    NeedsLock,
    /// A password is one line.
    OneLine,
    /// A new password snippet without a password.
    NoPassword,
    /// A password snippet turned into a command without a new body.
    NewBody,
    /// The lock cannot be removed while password snippets exist.
    KeepLock,
    /// A password snippet is never run on hosts.
    NoRun,
}

impl std::fmt::Display for SnippetRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NeedsLock => "a password snippet needs a snippet password; set one first",
            Self::OneLine => "a password is a single line",
            Self::NoPassword => "enter the password",
            Self::NewBody => "enter the new command; the saved password is never shown",
            Self::KeepLock => "password snippets exist; the snippet password stays",
            Self::NoRun => "a password snippet cannot be run on hosts",
        })
    }
}

impl std::error::Error for SnippetRefusal {}

impl Snippet {
    /// Get a snippet coming from the UI ready for the store: check it against the saved one
    /// (`old`) and seal its body. Whatever the UI put in `script_enc` is ignored. A password
    /// snippet with an empty body keeps the saved one; its plain text is wiped here.
    pub fn prepare(&mut self, old: Option<&Snippet>, lock: Lock<'_>) -> Result<()> {
        let done = self.seal_body(old, lock);
        if self.kind == SnippetKind::Password {
            crate::vault::forget(std::mem::take(&mut self.script));
        }
        done
    }

    fn seal_body(&mut self, old: Option<&Snippet>, lock: Lock<'_>) -> Result<()> {
        let saved = old.filter(|o| o.kind == SnippetKind::Password);
        if self.kind == SnippetKind::Password {
            if self.script.contains(['\n', '\r']) {
                return Err(SnippetRefusal::OneLine.into());
            }
            let key = match lock {
                Lock::Off => return Err(SnippetRefusal::NeedsLock.into()),
                Lock::Locked => anyhow::bail!(LOCKED),
                Lock::Open(key) => key,
            };
            self.script_enc = if !self.script.is_empty() {
                key.seal(&self.script)?
            } else {
                match saved {
                    Some(o) if !o.script_enc.is_empty() => o.script_enc.clone(),
                    _ => return Err(SnippetRefusal::NoPassword.into()),
                }
            };
            return Ok(());
        }
        if saved.is_some() && self.script.trim().is_empty() {
            return Err(SnippetRefusal::NewBody.into());
        }
        match lock {
            Lock::Off => self.script_enc.clear(),
            Lock::Locked => anyhow::bail!(LOCKED),
            Lock::Open(key) => {
                self.script_enc = key.seal(&self.script)?;
                self.script.clear();
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ForwardKind {
    /// `bind_port` on this computer -> `dest_host:dest_port` as seen from the server.
    #[default]
    Local,
    /// `bind_port` on the server -> `dest_host:dest_port` as seen from this computer.
    Remote,
    /// SOCKS5 proxy on `bind_port` here, tunnelled through the server.
    Dynamic,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ForwardRule {
    pub id: String,
    pub label: String,
    pub kind: ForwardKind,
    pub host_id: String,
    pub bind_address: String,
    pub bind_port: u16,
    pub dest_host: String,
    pub dest_port: u16,
}

impl Default for ForwardRule {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            kind: ForwardKind::Local,
            host_id: String::new(),
            bind_address: "127.0.0.1".into(),
            bind_port: 0,
            dest_host: "127.0.0.1".into(),
            dest_port: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct HistoryEntry {
    pub id: String,
    pub host_id: Option<String>,
    pub label: String,
    pub target: String,
    /// `ssh`, `sftp`, `forward`, `local`, `snippet`.
    pub kind: String,
    pub started_at: u64,
    pub ended_at: u64,
    pub ok: bool,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    pub font_family: String,
    pub font_size: u8,
    /// `block`, `underline`, `bar`.
    pub cursor_style: String,
    pub cursor_blink: bool,
    pub scrollback: u32,
    /// `auto` (follow the app theme), `dark`, `light`, `solarized`, `dracula`.
    pub theme: String,
    pub bell: bool,
    pub copy_on_select: bool,
    pub right_click_paste: bool,
    pub keepalive_secs: u32,
    pub confirm_close: bool,
    /// Local folder the SFTP browser opens first (empty = home).
    pub sftp_local_root: String,
    pub show_hidden: bool,
    /// Program for local terminals (empty = system shell).
    pub local_shell: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: 13,
            cursor_style: "block".into(),
            cursor_blink: true,
            scrollback: 5000,
            theme: "auto".into(),
            bell: false,
            copy_on_select: true,
            right_click_paste: true,
            keepalive_secs: 30,
            confirm_close: true,
            sftp_local_root: String::new(),
            show_hidden: false,
            local_shell: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Store {
    pub version: u32,
    pub groups: Vec<Group>,
    pub hosts: Vec<Host>,
    pub identities: Vec<Identity>,
    pub keys: Vec<KeyMeta>,
    pub snippets: Vec<Snippet>,
    pub forwards: Vec<ForwardRule>,
    pub history: Vec<HistoryEntry>,
    pub settings: Settings,
}

const MAX_HISTORY: usize = 500;

/// Where the remote data lives inside a Veranda root.
pub struct Paths {
    pub dir: PathBuf,
}

impl Paths {
    pub fn new(layout: &Layout) -> Self {
        Self {
            dir: layout.etc().join("remote"),
        }
    }
    pub fn store_file(&self) -> PathBuf {
        self.dir.join("store.json")
    }
    pub fn keys_dir(&self) -> PathBuf {
        self.dir.join("keys")
    }
    pub fn known_hosts(&self) -> PathBuf {
        self.dir.join("known_hosts")
    }
    /// Create the folders with private permissions.
    pub fn ensure(&self) -> Result<()> {
        for d in [&self.dir, &self.keys_dir()] {
            std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
            veranda_core::fsutil::make_private_dir(d)?;
        }
        Ok(())
    }
}

impl Store {
    pub fn load(layout: &Layout) -> Result<Self> {
        let paths = Paths::new(layout);
        Self::load_from(&paths.store_file())
    }

    pub fn load_from(file: &Path) -> Result<Self> {
        if !file.exists() {
            return Ok(Self {
                version: 1,
                ..Default::default()
            });
        }
        let text =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let mut store: Store =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;
        if store.version == 0 {
            store.version = 1;
        }
        store.hosts.iter_mut().for_each(Host::fold_startup);
        Ok(store)
    }

    pub fn save(&self, layout: &Layout) -> Result<()> {
        let paths = Paths::new(layout);
        paths.ensure()?;
        self.save_to(&paths.store_file())
    }

    pub fn save_to(&self, file: &Path) -> Result<()> {
        if let Some(p) = file.parent() {
            std::fs::create_dir_all(p)?;
        }
        let text = serde_json::to_vec_pretty(self)?;
        veranda_core::fsutil::write_private(file, &text)
    }

    pub fn host(&self, id: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.id == id)
    }
    pub fn host_mut(&mut self, id: &str) -> Option<&mut Host> {
        self.hosts.iter_mut().find(|h| h.id == id)
    }
    pub fn identity(&self, id: &str) -> Option<&Identity> {
        self.identities.iter().find(|i| i.id == id)
    }
    pub fn key(&self, id: &str) -> Option<&KeyMeta> {
        self.keys.iter().find(|k| k.id == id)
    }
    pub fn snippet(&self, id: &str) -> Option<&Snippet> {
        self.snippets.iter().find(|s| s.id == id)
    }
    pub fn has_password_snippet(&self) -> bool {
        self.snippets
            .iter()
            .any(|s| s.kind == SnippetKind::Password)
    }
    pub fn forward(&self, id: &str) -> Option<&ForwardRule> {
        self.forwards.iter().find(|f| f.id == id)
    }
    pub fn group(&self, id: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// Insert or replace a host (by id). A missing id gets a fresh one. Returns the id.
    pub fn upsert_host(&mut self, mut host: Host) -> String {
        if host.id.is_empty() {
            host.id = crate::new_id();
            host.created_at = crate::now();
        }
        if host.port == 0 {
            host.port = 22;
        }
        if host.label.trim().is_empty() {
            host.label = host.address.clone();
        }
        host.fold_startup();
        host.tags = host
            .tags
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        let id = host.id.clone();
        match self.hosts.iter_mut().find(|h| h.id == id) {
            Some(h) => *h = host,
            None => self.hosts.push(host),
        }
        id
    }

    pub fn remove_host(&mut self, id: &str) -> bool {
        let before = self.hosts.len();
        self.hosts.retain(|h| h.id != id);
        for h in &mut self.hosts {
            if h.jump_host_id.as_deref() == Some(id) {
                h.jump_host_id = None;
            }
        }
        self.forwards.retain(|f| f.host_id != id);
        self.hosts.len() != before
    }

    pub fn upsert_group(&mut self, mut group: Group) -> String {
        if group.id.is_empty() {
            group.id = crate::new_id();
        }
        let id = group.id.clone();
        match self.groups.iter_mut().find(|g| g.id == id) {
            Some(g) => *g = group,
            None => self.groups.push(group),
        }
        id
    }

    /// Delete a group; its hosts and sub-groups move to the parent.
    pub fn remove_group(&mut self, id: &str) -> bool {
        let Some(pos) = self.groups.iter().position(|g| g.id == id) else {
            return false;
        };
        let parent = self.groups[pos].parent_id.clone();
        self.groups.remove(pos);
        for h in &mut self.hosts {
            if h.group_id.as_deref() == Some(id) {
                h.group_id = parent.clone();
            }
        }
        for g in &mut self.groups {
            if g.parent_id.as_deref() == Some(id) {
                g.parent_id = parent.clone();
            }
        }
        true
    }

    pub fn upsert_identity(&mut self, mut identity: Identity) -> String {
        if identity.id.is_empty() {
            identity.id = crate::new_id();
        }
        let id = identity.id.clone();
        match self.identities.iter_mut().find(|i| i.id == id) {
            Some(i) => *i = identity,
            None => self.identities.push(identity),
        }
        id
    }

    pub fn remove_identity(&mut self, id: &str) -> bool {
        let before = self.identities.len();
        self.identities.retain(|i| i.id != id);
        for h in &mut self.hosts {
            if h.identity_id.as_deref() == Some(id) {
                h.identity_id = None;
                if h.auth == AuthKind::Identity {
                    h.auth = AuthKind::Password;
                }
            }
        }
        for g in &mut self.groups {
            if g.identity_id.as_deref() == Some(id) {
                g.identity_id = None;
            }
        }
        self.identities.len() != before
    }

    pub fn remove_key(&mut self, id: &str) -> bool {
        let before = self.keys.len();
        self.keys.retain(|k| k.id != id);
        for h in &mut self.hosts {
            if h.key_id.as_deref() == Some(id) {
                h.key_id = None;
                if h.auth == AuthKind::Key {
                    h.auth = AuthKind::Password;
                }
            }
        }
        for i in &mut self.identities {
            if i.key_id.as_deref() == Some(id) {
                i.key_id = None;
            }
        }
        self.keys.len() != before
    }

    pub fn upsert_snippet(&mut self, mut snippet: Snippet) -> String {
        if snippet.id.is_empty() {
            snippet.id = crate::new_id();
        }
        let id = snippet.id.clone();
        match self.snippets.iter_mut().find(|s| s.id == id) {
            Some(s) => *s = snippet,
            None => self.snippets.push(snippet),
        }
        id
    }

    pub fn remove_snippet(&mut self, id: &str) -> bool {
        let before = self.snippets.len();
        self.snippets.retain(|s| s.id != id);
        for h in &mut self.hosts {
            h.startup_steps
                .retain(|s| !matches!(s, StartupStep::Snippet { id: sid } if sid == id));
        }
        self.snippets.len() != before
    }

    pub fn upsert_forward(&mut self, mut rule: ForwardRule) -> String {
        if rule.id.is_empty() {
            rule.id = crate::new_id();
        }
        if rule.bind_address.trim().is_empty() {
            rule.bind_address = "127.0.0.1".into();
        }
        if rule.label.trim().is_empty() {
            rule.label = match rule.kind {
                ForwardKind::Dynamic => format!("SOCKS :{}", rule.bind_port),
                _ => format!(
                    ":{} -> {}:{}",
                    rule.bind_port, rule.dest_host, rule.dest_port
                ),
            };
        }
        let id = rule.id.clone();
        match self.forwards.iter_mut().find(|f| f.id == id) {
            Some(f) => *f = rule,
            None => self.forwards.push(rule),
        }
        id
    }

    pub fn remove_forward(&mut self, id: &str) -> bool {
        let before = self.forwards.len();
        self.forwards.retain(|f| f.id != id);
        self.forwards.len() != before
    }

    /// Prepend a history entry (newest first) and trim the list.
    pub fn push_history(&mut self, mut entry: HistoryEntry) {
        if entry.id.is_empty() {
            entry.id = crate::new_id();
        }
        self.history.insert(0, entry);
        self.history.truncate(MAX_HISTORY);
    }

    /// Resolve the identity a host should use: its own, else the group's (walking up).
    pub fn effective_identity(&self, host: &Host) -> Option<&Identity> {
        if let Some(id) = &host.identity_id
            && let Some(i) = self.identity(id)
        {
            return Some(i);
        }
        let mut group = host.group_id.as_deref().and_then(|g| self.group(g));
        let mut guard = 0;
        while let Some(g) = group {
            if let Some(id) = &g.identity_id
                && let Some(i) = self.identity(id)
            {
                return Some(i);
            }
            guard += 1;
            if guard > 32 {
                break;
            }
            group = g.parent_id.as_deref().and_then(|p| self.group(p));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_roundtrip_and_cascades() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("remote").join("store.json");
        let mut s = Store::load_from(&file).unwrap();
        assert_eq!(s.version, 1);
        let gid = s.upsert_group(Group {
            name: "Prod".into(),
            ..Default::default()
        });
        let kid = "key1".to_string();
        s.keys.push(KeyMeta {
            id: kid.clone(),
            label: "k".into(),
            ..Default::default()
        });
        let iid = s.upsert_identity(Identity {
            label: "deploy".into(),
            username: "deploy".into(),
            key_id: Some(kid.clone()),
            ..Default::default()
        });
        let hid = s.upsert_host(Host {
            address: "example.com".into(),
            port: 0,
            group_id: Some(gid.clone()),
            auth: AuthKind::Key,
            key_id: Some(kid.clone()),
            tags: vec![" web ".into(), "".into()],
            ..Default::default()
        });
        let h = s.host(&hid).unwrap();
        assert_eq!(h.port, 22);
        assert_eq!(h.label, "example.com");
        assert_eq!(h.tags, vec!["web"]);
        assert_eq!(h.summary(), "example.com");
        s.upsert_forward(ForwardRule {
            host_id: hid.clone(),
            bind_port: 8080,
            dest_host: "db".into(),
            dest_port: 5432,
            ..Default::default()
        });
        s.save_to(&file).unwrap();
        let back = Store::load_from(&file).unwrap();
        assert_eq!(back, s);

        // group identity inheritance
        s.groups[0].identity_id = Some(iid.clone());
        assert_eq!(s.effective_identity(s.host(&hid).unwrap()).unwrap().id, iid);
        // removing the key falls back to password auth and detaches it from the identity
        assert!(s.remove_key(&kid));
        assert_eq!(s.host(&hid).unwrap().auth, AuthKind::Password);
        assert_eq!(s.identity(&iid).unwrap().key_id, None);
        // removing the group moves the host to the parent (none)
        assert!(s.remove_group(&gid));
        assert_eq!(s.host(&hid).unwrap().group_id, None);
        // removing the host drops its forwarding rules
        assert!(s.remove_host(&hid));
        assert!(s.forwards.is_empty());
        for i in 0..600 {
            s.push_history(HistoryEntry {
                label: format!("h{i}"),
                ..Default::default()
            });
        }
        assert_eq!(s.history.len(), MAX_HISTORY);
        assert_eq!(s.history[0].label, "h599");
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let json =
            r#"{"version":1,"hosts":[{"id":"a","address":"x","future_field":true}],"extra":{}}"#;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("store.json");
        std::fs::write(&file, json).unwrap();
        let s = Store::load_from(&file).unwrap();
        assert_eq!(s.hosts[0].address, "x");
        assert_eq!(s.hosts[0].port, 22);
    }

    #[test]
    fn old_startup_snippet_becomes_one_step() {
        let json = r#"{"version":1,"hosts":[{"id":"a","address":"x","startup_snippet_id":"s1"},{"id":"b","address":"y","startup_snippet_id":null}],"snippets":[{"id":"s1","label":"one"}]}"#;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("store.json");
        std::fs::write(&file, json).unwrap();
        let mut s = Store::load_from(&file).unwrap();
        let step = StartupStep::Snippet { id: "s1".into() };
        assert_eq!(s.hosts[0].startup_steps, vec![step]);
        assert!(s.hosts[1].startup_steps.is_empty());

        s.hosts[0].startup_steps.push(StartupStep::SudoPassword);
        s.save_to(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("startup_snippet_id"));
        assert!(text.contains(r#""kind": "sudo_password""#));
        let mut back = Store::load_from(&file).unwrap();
        assert_eq!(back, s);

        assert!(back.remove_snippet("s1"));
        assert_eq!(back.hosts[0].startup_steps, vec![StartupStep::SudoPassword]);
    }

    #[test]
    fn snippet_without_a_kind_is_a_command() {
        let json = r#"{"version":1,"snippets":[{"id":"s1","label":"one","script":"df -h"},{"id":"s2","label":"two","kind":"password","script_enc":"v1:AAAA"}]}"#;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("store.json");
        std::fs::write(&file, json).unwrap();
        let s = Store::load_from(&file).unwrap();
        assert_eq!(s.snippets[0].kind, SnippetKind::Command);
        assert_eq!(s.snippets[0].script, "df -h");
        assert_eq!(s.snippets[1].kind, SnippetKind::Password);
        assert!(s.has_password_snippet());

        s.save_to(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains(r#""kind": "command""#));
        assert!(text.contains(r#""kind": "password""#));
        assert_eq!(Store::load_from(&file).unwrap(), s);
    }

    fn refusal(r: Result<()>) -> Option<SnippetRefusal> {
        r.err()?.downcast_ref::<SnippetRefusal>().copied()
    }

    #[test]
    fn password_snippet_is_sealed_and_an_empty_body_keeps_the_saved_one() {
        let (_, key) = crate::vault::LockFile::create_with("hunter22", 15, 1000).unwrap();
        let entry = |script: &str| Snippet {
            id: "s1".into(),
            label: "sudo".into(),
            kind: SnippetKind::Password,
            script: script.into(),
            ..Default::default()
        };

        // no lock, a locked lock, more than one line, nothing to store
        let mut s = entry("first one");
        assert_eq!(
            refusal(s.prepare(None, Lock::Off)),
            Some(SnippetRefusal::NeedsLock)
        );
        assert!(s.script.is_empty() && s.script_enc.is_empty());
        let mut s = entry("first one");
        assert!(s.prepare(None, Lock::Locked).is_err());
        assert!(s.script.is_empty() && s.script_enc.is_empty());
        assert_eq!(
            refusal(entry("a\nb").prepare(None, Lock::Open(&key))),
            Some(SnippetRefusal::OneLine)
        );
        assert_eq!(
            refusal(entry("").prepare(None, Lock::Open(&key))),
            Some(SnippetRefusal::NoPassword)
        );

        let mut saved = entry("first one");
        saved.script_enc = "from the UI".into();
        saved.prepare(None, Lock::Open(&key)).unwrap();
        assert!(saved.script.is_empty());
        assert_eq!(key.open(&saved.script_enc).unwrap(), "first one");

        // edited with the body left empty: the saved password stays
        let mut renamed = entry("");
        renamed.label = "sudo prod".into();
        renamed.prepare(Some(&saved), Lock::Open(&key)).unwrap();
        assert_eq!(renamed.script_enc, saved.script_enc);
        assert!(renamed.script.is_empty());

        // a new body replaces it
        let mut changed = entry("second one");
        changed.prepare(Some(&saved), Lock::Open(&key)).unwrap();
        assert_eq!(key.open(&changed.script_enc).unwrap(), "second one");

        // an empty body over a command snippet has nothing to keep
        let command = Snippet {
            id: "s1".into(),
            script_enc: key.seal("df -h").unwrap(),
            ..Default::default()
        };
        assert_eq!(
            refusal(entry("").prepare(Some(&command), Lock::Open(&key))),
            Some(SnippetRefusal::NoPassword)
        );
    }

    #[test]
    fn password_snippet_never_becomes_a_command_by_itself() {
        let (_, key) = crate::vault::LockFile::create_with("hunter22", 15, 1000).unwrap();
        let saved = Snippet {
            id: "s1".into(),
            kind: SnippetKind::Password,
            script_enc: key.seal("first one").unwrap(),
            ..Default::default()
        };
        let command = |script: &str| Snippet {
            id: "s1".into(),
            script: script.into(),
            ..Default::default()
        };
        assert_eq!(
            refusal(command(" \n").prepare(Some(&saved), Lock::Open(&key))),
            Some(SnippetRefusal::NewBody)
        );
        let mut s = command("whoami");
        s.prepare(Some(&saved), Lock::Open(&key)).unwrap();
        assert_eq!(key.open(&s.script_enc).unwrap(), "whoami");

        // command snippets keep their old rules: plain without a lock, sealed with one
        let mut plain = command("df -h");
        plain.script_enc = "stale".into();
        plain.prepare(None, Lock::Off).unwrap();
        assert_eq!(
            (plain.script.as_str(), plain.script_enc.as_str()),
            ("df -h", "")
        );
        assert!(command("df -h").prepare(None, Lock::Locked).is_err());
    }
}
