//! Remote (SSH) commands for the UI: hosts, terminals, SFTP, port forwarding, keys, snippets.
//!
//! Live objects (terminal sessions, SFTP sessions, forwarding rules) are kept in [`RemoteState`];
//! persistent data goes through [`veranda_remote::Store`] (one JSON file) and secrets through
//! [`veranda_remote::secrets::Secrets`]. Questions a connection asks (unknown host key, password,
//! passphrase, one-time code) are emitted as `remote-prompt` events and answered with
//! [`remote_prompt_answer`].

use crate::{AppState, CmdResult, err, open_app};
use base64::prelude::{BASE64_STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, oneshot};
use veranda_core::Veranda;
use veranda_remote::client::{
    self, Auth, AuthMethod, BoxFut, ConnectRequest, Connection, HostKeyRequest, HostKeyStatus,
    Prompter, SecretKind, SecretRequest, Target,
};
use veranda_remote::secrets::{self, Secrets};
use veranda_remote::store::{
    AuthKind, ForwardRule, Group, HistoryEntry, Host, Identity, KeyMeta, Settings, Snippet,
};
use veranda_remote::{Store, forward, import, keys, known_hosts, local, session, sftp, vault};

/* ---------------- state ---------------- */

#[derive(Clone, Serialize)]
pub struct SessionInfo {
    pub id: String,
    /// `ssh` or `local`.
    pub kind: String,
    pub host_id: Option<String>,
    pub label: String,
    pub target: String,
    /// `connecting`, `connected`, `closed`, `error`.
    pub state: String,
    pub message: String,
    pub started_at: u64,
}

struct TermEntry {
    handle: Option<session::TermHandle>,
    info: SessionInfo,
    /// The SSH connection behind the shell (None for local terminals); used for the server
    /// stats strip, which runs one small command on a separate channel now and then.
    conn: Option<Arc<Connection>>,
    /// Last `/proc/stat` sample (busy, total jiffies) so CPU% can be a delta, plus the last
    /// result with its time (calls more often than every 5 s get the cached value).
    cpu_prev: Option<(u64, u64)>,
    stats: Option<(Instant, ServerStats)>,
    stats_busy: bool,
}

/// What the terminal shows about the server it is connected to. Everything optional: a
/// non-Linux host answers with whatever `/proc` and `df` it has.
#[derive(Clone, Default, Serialize)]
pub struct ServerStats {
    /// Overall CPU busy % over the last second, counted the way htop does (iowait is idle).
    pub cpu: Option<f32>,
    /// Same per core, in /proc/stat order (empty when the server could not sample twice).
    pub cpus: Vec<f32>,
    pub load1: Option<f32>,
    pub load5: Option<f32>,
    pub load15: Option<f32>,
    pub procs_running: Option<u32>,
    pub procs_total: Option<u32>,
    pub cores: Option<u32>,
    pub mem_total_kb: Option<u64>,
    /// "Used" like htop / free: total − free − buffers − cache − reclaimable + shmem.
    pub mem_used_kb: Option<u64>,
    /// Buffers + page cache + reclaimable slab (minus shmem): memory the kernel can hand back.
    pub mem_cache_kb: Option<u64>,
    pub mem_avail_kb: Option<u64>,
    pub swap_total_kb: Option<u64>,
    pub swap_used_kb: Option<u64>,
    /// The fullest local filesystem (what breaks first); every mount is in `disks`.
    pub disk_total_kb: Option<u64>,
    pub disk_used_kb: Option<u64>,
    pub disk_avail_kb: Option<u64>,
    pub disk_mount: Option<String>,
    pub disks: Vec<DiskStat>,
    pub uptime_secs: Option<u64>,
}

#[derive(Clone, Default, Serialize)]
pub struct DiskStat {
    pub mount: String,
    pub total_kb: u64,
    pub used_kb: u64,
    /// Blocks a normal user can still write (`df` computes Use% from used / (used + avail),
    /// which is why it differs from used / total on filesystems with reserved blocks).
    pub avail_kb: u64,
}

/// One round trip, under 1 KB back. No `top`, no `ps`: reading /proc costs the server nothing.
/// /proc/stat is read twice, one second apart, so the CPU figure is a fresh one-second
/// average like htop's instead of "since the last poll" (which could be minutes when the tab
/// was hidden). `cpusep` marks the boundary between the two samples.
const STATS_CMD: &str = "grep '^cpu' /proc/stat 2>/dev/null; echo cpusep; sleep 1; grep '^cpu' /proc/stat 2>/dev/null; grep -E '^(MemTotal|MemFree|MemAvailable|Buffers|Cached|SReclaimable|Shmem|SwapTotal|SwapFree):' /proc/meminfo 2>/dev/null; echo \"load $(cat /proc/loadavg 2>/dev/null)\"; echo \"cores $(nproc 2>/dev/null)\"; echo \"up $(cut -d' ' -f1 /proc/uptime 2>/dev/null)\"; df -Pk -l 2>/dev/null | sed -n '2,$s/^/disk /p'";

/// (busy, total) jiffies of one `/proc/stat` cpu line, counted like htop: idle + iowait are
/// idle, guest time is already inside user/nice so only the first eight fields are summed.
fn cpu_jiffies(fields: &[u64]) -> Option<(u64, u64)> {
    if fields.len() < 4 {
        return None;
    }
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    let total: u64 = fields.iter().take(8).sum();
    Some((total.saturating_sub(idle), total))
}

fn cpu_pct(prev: (u64, u64), now: (u64, u64)) -> Option<f32> {
    if now.1 <= prev.1 {
        return None;
    }
    let busy = now.0.saturating_sub(prev.0) as f32;
    Some((busy / (now.1 - prev.1) as f32 * 100.0).clamp(0.0, 100.0))
}

fn parse_stats(out: &str, prev: &mut Option<(u64, u64)>) -> ServerStats {
    let mut s = ServerStats::default();
    let num = |w: Option<&str>| w.and_then(|x| x.parse::<u64>().ok());
    let mut mem = std::collections::HashMap::<&str, u64>::new();
    // cpu lines of the first and second sample: (name, (busy, total))
    let mut first: Vec<(String, (u64, u64))> = Vec::new();
    let mut second: Vec<(String, (u64, u64))> = Vec::new();
    let mut after_sep = false;
    for line in out.lines() {
        let mut w = line.split_whitespace();
        match w.next() {
            Some("cpusep") => after_sep = true,
            Some(name) if name.starts_with("cpu") => {
                let v: Vec<u64> = w.filter_map(|x| x.parse().ok()).collect();
                if let Some(j) = cpu_jiffies(&v) {
                    if after_sep { &mut second } else { &mut first }.push((name.to_string(), j));
                }
            }
            Some(
                k @ ("MemTotal:" | "MemFree:" | "MemAvailable:" | "Buffers:" | "Cached:"
                | "SReclaimable:" | "Shmem:" | "SwapTotal:" | "SwapFree:"),
            ) => {
                if let Some(v) = num(w.next()) {
                    mem.insert(k.trim_end_matches(':'), v);
                }
            }
            Some("load") => {
                s.load1 = w.next().and_then(|x| x.parse().ok());
                s.load5 = w.next().and_then(|x| x.parse().ok());
                s.load15 = w.next().and_then(|x| x.parse().ok());
                if let Some((r, t)) = w.next().and_then(|x| x.split_once('/')) {
                    s.procs_running = r.parse().ok();
                    s.procs_total = t.parse().ok();
                }
            }
            Some("cores") => s.cores = w.next().and_then(|x| x.parse().ok()),
            Some("up") => {
                s.uptime_secs = w
                    .next()
                    .and_then(|x| x.parse::<f64>().ok())
                    .map(|x| x as u64)
            }
            Some("disk") => {
                // "<fs> <1k-blocks> <used> <avail> <use%> <mount>": real block devices only
                // (no tmpfs, overlay, snap squashfs); the mount may contain spaces.
                let v: Vec<&str> = w.collect();
                if v.len() >= 6 && v[0].starts_with("/dev/") && !v[0].contains("/loop") {
                    if let (Ok(total), Ok(used), Ok(avail)) = (
                        v[1].parse::<u64>(),
                        v[2].parse::<u64>(),
                        v[3].parse::<u64>(),
                    ) {
                        if total > 0 {
                            s.disks.push(DiskStat {
                                mount: v[5..].join(" "),
                                total_kb: total,
                                used_kb: used,
                                avail_kb: avail,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // if a single sample came back (no `sleep`, odd shell) fall back to the previous poll
    let (before, now) = if after_sep {
        (first, second)
    } else {
        (Vec::new(), first)
    };
    let agg = |v: &[(String, (u64, u64))]| v.iter().find(|(n, _)| n == "cpu").map(|(_, j)| *j);
    if let Some(cur) = agg(&now) {
        s.cpu = match agg(&before) {
            Some(prev_cur) => cpu_pct(prev_cur, cur),
            None => prev.and_then(|p| cpu_pct(p, cur)),
        };
        *prev = Some(cur);
    }
    if !before.is_empty() {
        s.cpus = now
            .iter()
            .filter(|(n, _)| n.len() > 3)
            .filter_map(|(n, cur)| {
                before
                    .iter()
                    .find(|(m, _)| m == n)
                    .and_then(|(_, p)| cpu_pct(*p, *cur))
            })
            .collect();
    }
    let g = |k: &str| mem.get(k).copied();
    s.mem_total_kb = g("MemTotal");
    s.mem_avail_kb = g("MemAvailable");
    if let Some(t) = s.mem_total_kb {
        match (g("MemFree"), g("Buffers"), g("Cached")) {
            (Some(free), Some(buf), Some(cached)) => {
                let recl = g("SReclaimable").unwrap_or(0);
                let shm = g("Shmem").unwrap_or(0);
                let cache = (buf + cached + recl).saturating_sub(shm);
                s.mem_cache_kb = Some(cache);
                s.mem_used_kb = Some(t.saturating_sub(free).saturating_sub(cache));
            }
            _ => s.mem_used_kb = s.mem_avail_kb.map(|a| t.saturating_sub(a)),
        }
    }
    if let (Some(st), Some(sf)) = (g("SwapTotal"), g("SwapFree"))
        && st > 0
    {
        s.swap_total_kb = Some(st);
        s.swap_used_kb = Some(st.saturating_sub(sf));
    }
    // one entry per device: bind mounts and btrfs subvolumes repeat the same disk
    let mut seen = std::collections::HashSet::new();
    s.disks.retain(|d| seen.insert((d.total_kb, d.used_kb)));
    let df_pct = |d: &DiskStat| d.used_kb * 1000 / (d.used_kb + d.avail_kb).max(1);
    if let Some(fullest) = s.disks.iter().max_by_key(|d| df_pct(d)) {
        s.disk_total_kb = Some(fullest.total_kb);
        s.disk_used_kb = Some(fullest.used_kb);
        s.disk_avail_kb = Some(fullest.avail_kb);
        s.disk_mount = Some(fullest.mount.clone());
    }
    s
}

/// CPU, memory and disk of the server behind an SSH terminal. Cached for 5 s; one call in
/// flight per terminal; `None` for local terminals and sessions that are not connected.
#[tauri::command]
pub async fn term_stats(
    state: State<'_, RemoteState>,
    id: String,
) -> CmdResult<Option<ServerStats>> {
    let conn = {
        let mut g = lock(&state.terms);
        let Some(e) = g.get_mut(&id) else {
            return Ok(None);
        };
        if e.info.state != "connected" {
            return Ok(None);
        }
        if let Some((at, st)) = &e.stats
            && at.elapsed() < Duration::from_secs(4)
        {
            return Ok(Some(st.clone()));
        }
        if e.stats_busy {
            return Ok(e.stats.as_ref().map(|(_, st)| st.clone()));
        }
        let Some(c) = e.conn.clone() else {
            return Ok(None);
        };
        e.stats_busy = true;
        c
    };
    let r = client::exec(&conn, STATS_CMD, 8).await;
    let mut g = lock(&state.terms);
    let Some(e) = g.get_mut(&id) else {
        return Ok(None);
    };
    e.stats_busy = false;
    match r {
        Ok(out) => {
            let st = parse_stats(&out.stdout, &mut e.cpu_prev);
            e.stats = Some((Instant::now(), st.clone()));
            Ok(Some(st))
        }
        Err(e2) => Err(e2.to_string()),
    }
}

#[derive(Clone, Serialize)]
pub struct SftpInfo {
    pub id: String,
    pub host_id: String,
    pub label: String,
    pub home: String,
}

struct SftpEntry {
    sftp: Arc<sftp::Sftp>,
    info: SftpInfo,
}

#[derive(Clone, Serialize)]
pub struct ForwardStatus {
    pub rule_id: String,
    pub connections: usize,
    pub total: usize,
    pub bound_port: u16,
}

/// The snippet lock while it is open: the key plus when it was last used (for auto-lock).
struct VaultSession {
    key: vault::Key,
    last_used: Instant,
    auto_lock_minutes: u32,
}

#[derive(Default)]
pub struct RemoteState {
    secrets: Mutex<Option<(PathBuf, Arc<Secrets>)>>,
    vault: Mutex<Option<VaultSession>>,
    /// Wrong snippet passwords in a row; each one adds a delay to the next try.
    vault_failures: AtomicU32,
    /// Serialises load-modify-save cycles on the store file.
    store_lock: Mutex<()>,
    terms: Mutex<HashMap<String, TermEntry>>,
    sftps: tokio::sync::Mutex<HashMap<String, SftpEntry>>,
    forwards: tokio::sync::Mutex<HashMap<String, forward::ActiveForward>>,
    prompts: Arc<Mutex<HashMap<String, oneshot::Sender<PromptAnswer>>>>,
    transfers: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl RemoteState {
    fn secrets(&self, core: &Veranda) -> Arc<Secrets> {
        let root = core.layout.root().to_path_buf();
        let mut g = lock(&self.secrets);
        if let Some((r, s)) = g.as_ref()
            && *r == root
        {
            return s.clone();
        }
        let s = Arc::new(Secrets::new(&core.layout));
        *g = Some((root, s.clone()));
        s
    }

    /// Load the store, let `f` change it, save when `f` returns `true`.
    fn with_store<R>(
        &self,
        core: &Veranda,
        f: impl FnOnce(&mut Store) -> (bool, R),
    ) -> anyhow::Result<R> {
        let _g = lock(&self.store_lock);
        let mut store = Store::load(&core.layout)?;
        let (changed, r) = f(&mut store);
        if changed {
            store.save(&core.layout)?;
        }
        Ok(r)
    }

    /// Snippet lock: `off` (no password set), `locked` or `unlocked`.
    fn lock_status(&self, core: &Veranda) -> String {
        if !vault::path(&core.layout).is_file() {
            return "off".into();
        }
        self.with_key(|k| if k.is_some() { "unlocked" } else { "locked" })
            .into()
    }

    /// Run `f` with the unlocked snippet key, if any. Drops the key first when it sat unused
    /// for longer than the auto-lock time.
    fn with_key<R>(&self, f: impl FnOnce(Option<&vault::Key>) -> R) -> R {
        let mut g = lock(&self.vault);
        if let Some(s) = g.as_ref()
            && s.auto_lock_minutes > 0
            && s.last_used.elapsed() > Duration::from_secs(u64::from(s.auto_lock_minutes) * 60)
        {
            *g = None;
        }
        if let Some(s) = g.as_mut() {
            s.last_used = Instant::now();
        }
        f(g.as_ref().map(|s| &s.key))
    }

    fn set_key(&self, key: Option<vault::Key>, auto_lock_minutes: u32) {
        *lock(&self.vault) = key.map(|key| VaultSession {
            key,
            last_used: Instant::now(),
            auto_lock_minutes,
        });
    }

    /// The command text of a snippet, decrypting it when the lock is open.
    fn snippet_script(&self, s: &Snippet) -> anyhow::Result<String> {
        if s.script_enc.is_empty() {
            return Ok(s.script.clone());
        }
        self.with_key(|k| match k {
            Some(k) => k.open(&s.script_enc),
            None => anyhow::bail!("snippets are locked; unlock them first"),
        })
    }

    /// Prepare snippets for the UI: plain text when unlocked, empty when locked, never the
    /// ciphertext.
    fn reveal_snippets(&self, store: &mut Store) {
        self.with_key(|k| {
            for s in &mut store.snippets {
                if !s.script_enc.is_empty() {
                    s.script = k
                        .and_then(|k| k.open(&s.script_enc).ok())
                        .unwrap_or_default();
                    s.script_enc.clear();
                }
            }
        });
    }

    fn set_term_state(&self, app: &AppHandle, id: &str, state: &str, message: &str) {
        let info = {
            let mut g = lock(&self.terms);
            let Some(e) = g.get_mut(id) else { return };
            e.info.state = state.into();
            e.info.message = message.into();
            e.info.clone()
        };
        let _ = app.emit("term-state", info);
    }
}

fn core_of(app: &AppHandle) -> CmdResult<Veranda> {
    open_app(&app.state::<AppState>())
}

/// Keyring secrets of the remote store as JSON, for a move bundle.
pub fn export_secrets(app: &AppHandle) -> anyhow::Result<Vec<u8>> {
    let core = core_of(app).map_err(anyhow::Error::msg)?;
    let rs = app.state::<RemoteState>();
    let store = Store::load(&core.layout)?;
    let map = secrets::export_all(&rs.secrets(&core), &store)?;
    Ok(serde_json::to_vec_pretty(&map)?)
}

/// Put exported secrets into this computer's credential store; returns how many.
pub fn import_secrets(app: &AppHandle, json: &[u8]) -> anyhow::Result<usize> {
    let core = core_of(app).map_err(anyhow::Error::msg)?;
    let rs = app.state::<RemoteState>();
    let map: std::collections::BTreeMap<String, String> = serde_json::from_slice(json)?;
    secrets::import_all(&rs.secrets(&core), &map)
}

/* ---------------- prompts ---------------- */

#[derive(Clone, Deserialize)]
pub struct PromptAnswer {
    pub accept: bool,
    pub value: Option<String>,
    #[serde(default)]
    pub remember: bool,
}

#[derive(Clone, Serialize)]
struct PromptEvent {
    id: String,
    session: String,
    /// `hostkey`, `password`, `passphrase`, `interactive`.
    kind: String,
    title: String,
    prompt: String,
    echo: bool,
    host: String,
    port: u16,
    algorithm: String,
    fingerprint: String,
    /// `unknown` or `changed` (host keys).
    status: String,
    /// The answer may be saved (passwords, passphrases).
    can_remember: bool,
}

/// Forwards connection questions to the web UI and waits for the answer.
struct UiPrompter {
    app: AppHandle,
    prompts: Arc<Mutex<HashMap<String, oneshot::Sender<PromptAnswer>>>>,
    session: String,
    title: String,
    /// `remember` ticked on the last password/passphrase prompt.
    remember: Mutex<bool>,
    /// The last passphrase typed (so the caller can save it on success).
    last_passphrase: Mutex<Option<String>>,
}

impl UiPrompter {
    fn new(app: &AppHandle, session: &str, title: &str) -> Arc<Self> {
        let prompts = app.state::<RemoteState>().prompts.clone();
        Arc::new(Self {
            app: app.clone(),
            prompts,
            session: session.to_string(),
            title: title.to_string(),
            remember: Mutex::new(false),
            last_passphrase: Mutex::new(None),
        })
    }

    async fn ask(&self, ev: PromptEvent) -> Option<PromptAnswer> {
        let (tx, rx) = oneshot::channel();
        lock(&self.prompts).insert(ev.id.clone(), tx);
        let id = ev.id.clone();
        if self.app.emit("remote-prompt", ev).is_err() {
            lock(&self.prompts).remove(&id);
            return None;
        }
        let r = tokio::time::timeout(std::time::Duration::from_secs(600), rx).await;
        lock(&self.prompts).remove(&id);
        let _ = self.app.emit("remote-prompt-done", id);
        match r {
            Ok(Ok(a)) => Some(a),
            _ => None,
        }
    }

    fn event(&self, kind: &str) -> PromptEvent {
        PromptEvent {
            id: veranda_remote::new_id(),
            session: self.session.clone(),
            kind: kind.into(),
            title: self.title.clone(),
            prompt: String::new(),
            echo: false,
            host: String::new(),
            port: 0,
            algorithm: String::new(),
            fingerprint: String::new(),
            status: String::new(),
            can_remember: false,
        }
    }

    fn remembered(&self) -> bool {
        *lock(&self.remember)
    }
}

impl Prompter for UiPrompter {
    fn host_key(&self, req: HostKeyRequest) -> BoxFut<'_, bool> {
        Box::pin(async move {
            let mut ev = self.event("hostkey");
            ev.host = req.host;
            ev.port = req.port;
            ev.algorithm = req.algorithm;
            ev.fingerprint = req.fingerprint;
            ev.status = match req.status {
                HostKeyStatus::Unknown => "unknown",
                HostKeyStatus::Changed => "changed",
            }
            .into();
            self.ask(ev).await.map(|a| a.accept).unwrap_or(false)
        })
    }

    fn secret(&self, req: SecretRequest) -> BoxFut<'_, Option<String>> {
        Box::pin(async move {
            let kind = match req.kind {
                SecretKind::Password => "password",
                SecretKind::Passphrase => "passphrase",
                SecretKind::Interactive => "interactive",
            };
            let mut ev = self.event(kind);
            if !req.title.is_empty() {
                ev.title = req.title;
            }
            ev.prompt = req.prompt;
            ev.echo = req.echo;
            ev.can_remember = matches!(req.kind, SecretKind::Password | SecretKind::Passphrase);
            let a = self.ask(ev).await?;
            if !a.accept {
                return None;
            }
            *lock(&self.remember) = a.remember;
            if req.kind == SecretKind::Passphrase {
                *lock(&self.last_passphrase) = a.value.clone();
            }
            Some(a.value.unwrap_or_default())
        })
    }

    fn notice(&self, text: String) -> BoxFut<'_, ()> {
        Box::pin(async move {
            let _ = self.app.emit(
                "remote-notice",
                serde_json::json!({ "session": self.session, "text": text }),
            );
        })
    }
}

#[tauri::command]
pub fn remote_prompt_answer(
    state: State<'_, RemoteState>,
    id: String,
    accept: bool,
    value: Option<String>,
    remember: Option<bool>,
) -> CmdResult<()> {
    let tx = lock(&state.prompts).remove(&id);
    match tx {
        Some(tx) => {
            let _ = tx.send(PromptAnswer {
                accept,
                value,
                remember: remember.unwrap_or(false),
            });
            Ok(())
        }
        None => Err("this question is no longer open".into()),
    }
}

/* ---------------- resolving credentials ---------------- */

fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "root".into())
}

async fn load_key(
    core: &Veranda,
    store: &Store,
    secrets: &Secrets,
    prompter: &UiPrompter,
    key_id: &str,
) -> anyhow::Result<(Arc<veranda_remote::PrivateKey>, String)> {
    let meta = store
        .key(key_id)
        .ok_or_else(|| anyhow::anyhow!("key {key_id} no longer exists"))?;
    let dir = veranda_remote::store::Paths::new(&core.layout).keys_dir();
    if !meta.encrypted {
        return Ok((
            Arc::new(keys::load(&dir, key_id, None)?),
            meta.label.clone(),
        ));
    }
    let saved = secrets.get(&secrets::key_passphrase_key(key_id))?;
    if let Some(p) = saved
        && let Ok(k) = keys::load(&dir, key_id, Some(&p))
    {
        return Ok((Arc::new(k), meta.label.clone()));
    }
    for attempt in 0..3 {
        let Some(p) = prompter
            .secret(SecretRequest {
                kind: SecretKind::Passphrase,
                title: meta.label.clone(),
                prompt: if attempt == 0 {
                    format!("Passphrase for key \"{}\"", meta.label)
                } else {
                    format!("Wrong passphrase. Passphrase for key \"{}\"", meta.label)
                },
                echo: false,
            })
            .await
        else {
            anyhow::bail!("cancelled");
        };
        if let Ok(k) = keys::load(&dir, key_id, Some(&p)) {
            if prompter.remembered() {
                let _ = secrets.set(&secrets::key_passphrase_key(key_id), &p);
            }
            return Ok((Arc::new(k), meta.label.clone()));
        }
    }
    anyhow::bail!("could not unlock key \"{}\"", meta.label)
}

async fn resolve_auth(
    core: &Veranda,
    store: &Store,
    secrets: &Secrets,
    prompter: &UiPrompter,
    host: &Host,
) -> anyhow::Result<Auth> {
    let identity = store.effective_identity(host);
    let mut username = host.username.trim().to_string();
    if username.is_empty() {
        username = identity
            .map(|i| i.username.trim().to_string())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(current_user);
    }
    let mut methods: Vec<AuthMethod> = vec![];
    match host.auth {
        AuthKind::Identity => {
            if let Some(i) = identity {
                if let Some(kid) = &i.key_id {
                    let (key, label) = load_key(core, store, secrets, prompter, kid).await?;
                    methods.push(AuthMethod::Key { key, label });
                }
                if i.has_password
                    && let Some(p) = secrets.get(&secrets::identity_password_key(&i.id))?
                {
                    methods.push(AuthMethod::Password(p));
                }
            }
        }
        AuthKind::Key => {
            if let Some(kid) = &host.key_id {
                let (key, label) = load_key(core, store, secrets, prompter, kid).await?;
                methods.push(AuthMethod::Key { key, label });
            }
        }
        AuthKind::Password => {
            if host.has_password
                && let Some(p) = secrets.get(&secrets::host_password_key(&host.id))?
            {
                methods.push(AuthMethod::Password(p));
            }
        }
        AuthKind::Agent => methods.push(AuthMethod::Agent),
        AuthKind::None => {
            if client::agent_available().await {
                methods.push(AuthMethod::Agent);
            }
        }
    }
    methods.push(AuthMethod::AskPassword);
    methods.push(AuthMethod::Interactive);
    Ok(Auth { username, methods })
}

fn build_request<'a>(
    core: &'a Veranda,
    store: &'a Store,
    secrets: &'a Secrets,
    prompter: &'a UiPrompter,
    host: &'a Host,
    depth: u8,
) -> BoxFut<'a, anyhow::Result<ConnectRequest>> {
    Box::pin(async move {
        if depth > 5 {
            anyhow::bail!("jump host chain is too deep");
        }
        let auth = resolve_auth(core, store, secrets, prompter, host).await?;
        let jump = match host.jump_host_id.as_deref().and_then(|id| store.host(id)) {
            Some(j) if j.id != host.id => Some(Box::new(
                build_request(core, store, secrets, prompter, j, depth + 1).await?,
            )),
            _ => None,
        };
        Ok(ConnectRequest {
            target: Target {
                host: host.address.trim().to_string(),
                port: if host.port == 0 { 22 } else { host.port },
                label: host.label.clone(),
            },
            auth,
            jump,
            keepalive_secs: if host.keep_alive {
                store.settings.keepalive_secs
            } else {
                0
            },
            known_hosts: veranda_remote::store::Paths::new(&core.layout).known_hosts(),
            timeout_secs: 20,
        })
    })
}

/// Connect to a stored host; saves the typed password when asked to, records history.
async fn connect_host(
    app: &AppHandle,
    core: &Veranda,
    host: &Host,
    session: &str,
    kind: &str,
) -> anyhow::Result<Arc<Connection>> {
    let rs = app.state::<RemoteState>();
    let secrets = rs.secrets(core);
    let store = Store::load(&core.layout)?;
    let prompter = UiPrompter::new(app, session, &host.label);
    let started = veranda_remote::now();
    let req = build_request(core, &store, &secrets, &prompter, host, 0).await?;
    let result = client::connect(req, prompter.clone()).await;
    let ok = result.is_ok();
    let error = result
        .as_ref()
        .err()
        .map(|e| format!("{e:#}"))
        .unwrap_or_default();
    let host_id = host.id.clone();
    let _ = rs.with_store(core, |s| {
        s.push_history(HistoryEntry {
            id: String::new(),
            host_id: Some(host_id.clone()),
            label: host.label.clone(),
            target: host.summary(),
            kind: kind.into(),
            started_at: started,
            ended_at: if ok { 0 } else { veranda_remote::now() },
            ok,
            error: error.clone(),
        });
        if ok && let Some(h) = s.host_mut(&host_id) {
            h.last_connected = veranda_remote::now();
        }
        (true, ())
    });
    let conn = result?;
    if let Some(p) = &conn.typed_password
        && prompter.remembered()
        && !host.id.is_empty()
    {
        secrets.set(&secrets::host_password_key(&host.id), p)?;
        let _ = rs.with_store(core, |s| {
            if let Some(h) = s.host_mut(&host.id) {
                h.has_password = true;
                if h.auth == AuthKind::None {
                    h.auth = AuthKind::Password;
                }
            }
            (true, ())
        });
    }
    let _ = app.emit("remote-changed", ());
    Ok(conn)
}

/// Parse `user@host:port` / `host` for Quick connect.
fn quick_host(spec: &str) -> anyhow::Result<Host> {
    let s = spec.trim().trim_start_matches("ssh://");
    if s.is_empty() {
        anyhow::bail!("enter user@host[:port]");
    }
    let (user, rest) = match s.rsplit_once('@') {
        Some((u, r)) => (u.to_string(), r),
        None => (String::new(), s),
    };
    let (addr, port) = if let Some(r) = rest.strip_prefix('[') {
        // [ipv6]:port
        let (a, p) = r.split_once(']').unwrap_or((r, ""));
        (
            a.to_string(),
            p.trim_start_matches(':').parse().unwrap_or(22),
        )
    } else if let Some((a, p)) = rest.rsplit_once(':').filter(|(a, _)| !a.contains(':')) {
        (a.to_string(), p.parse().unwrap_or(22))
    } else {
        (rest.to_string(), 22)
    };
    if addr.is_empty() {
        anyhow::bail!("missing host name");
    }
    Ok(Host {
        label: if user.is_empty() {
            addr.clone()
        } else {
            format!("{user}@{addr}")
        },
        address: addr,
        port,
        username: user,
        auth: AuthKind::None,
        ..Default::default()
    })
}

/* ---------------- store commands ---------------- */

#[derive(Serialize)]
pub struct RemoteView {
    store: Store,
    sessions: Vec<SessionInfo>,
    sftps: Vec<SftpInfo>,
    forwards_active: Vec<ForwardStatus>,
    secret_backend: String,
    /// `off`, `locked` or `unlocked` (see [`snippet_lock_set`]).
    snippet_lock: String,
    snippet_lock_minutes: u32,
    agent: bool,
    local_shell: String,
    home: String,
    /// Hosts found in `~/.ssh/config` / PuTTY (for the import button).
    importable: usize,
}

#[tauri::command]
pub async fn remote_state(app: AppHandle) -> CmdResult<RemoteView> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let mut store = Store::load(&core.layout).map_err(err)?;
    rs.reveal_snippets(&mut store);
    let snippet_lock_minutes = vault::LockFile::load(&core.layout)
        .ok()
        .flatten()
        .map(|l| l.auto_lock_minutes)
        .unwrap_or(15);
    let sessions: Vec<SessionInfo> = {
        let g = lock(&rs.terms);
        let mut v: Vec<SessionInfo> = g.values().map(|e| e.info.clone()).collect();
        v.sort_by_key(|s| s.started_at);
        v
    };
    let sftps: Vec<SftpInfo> = rs
        .sftps
        .lock()
        .await
        .values()
        .map(|e| e.info.clone())
        .collect();
    let forwards_active: Vec<ForwardStatus> = rs
        .forwards
        .lock()
        .await
        .values()
        .filter(|f| !f.is_stopped())
        .map(|f| ForwardStatus {
            rule_id: f.rule_id.clone(),
            connections: f.connections.load(Ordering::Relaxed),
            total: f.total.load(Ordering::Relaxed),
            bound_port: f.bound_port,
        })
        .collect();
    let (shell, _) = local::default_shell(&store.settings.local_shell);
    let importable = import::from_ssh_config().len() + import::from_putty().len();
    Ok(RemoteView {
        secret_backend: rs.secrets(&core).backend().into(),
        snippet_lock: rs.lock_status(&core),
        snippet_lock_minutes,
        store,
        sessions,
        sftps,
        forwards_active,
        agent: client::agent_available().await,
        local_shell: shell,
        home: sftp::local_home().display().to_string(),
        importable,
    })
}

#[tauri::command]
pub fn remote_save_host(app: AppHandle, host: Host, password: Option<String>) -> CmdResult<String> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let secrets = rs.secrets(&core);
    let mut host = host;
    if host.address.trim().is_empty() {
        return Err("address is required".into());
    }
    host.address = host.address.trim().to_string();
    host.username = host.username.trim().to_string();
    rs.with_store(&core, |s| {
        let existing = if host.id.is_empty() {
            None
        } else {
            s.host(&host.id).cloned()
        };
        if host.id.is_empty() {
            host.id = veranda_remote::new_id();
            host.created_at = veranda_remote::now();
        }
        if let Some(e) = &existing {
            host.has_password = e.has_password;
            host.last_connected = e.last_connected;
            host.created_at = e.created_at;
            if host.os.is_empty() {
                host.os = e.os.clone();
            }
        }
        match password.as_deref() {
            Some(p) if !p.is_empty() => {
                host.has_password = secrets
                    .set(&secrets::host_password_key(&host.id), p)
                    .is_ok();
            }
            Some(_) => {
                let _ = secrets.delete(&secrets::host_password_key(&host.id));
                host.has_password = false;
            }
            None => {}
        }
        let id = s.upsert_host(host.clone());
        (true, id)
    })
    .map_err(err)
}

#[tauri::command]
pub fn remote_delete_host(app: AppHandle, id: String) -> CmdResult<bool> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let _ = rs.secrets(&core).delete(&secrets::host_password_key(&id));
    rs.with_store(&core, |s| {
        let r = s.remove_host(&id);
        (r, r)
    })
    .map_err(err)
}

#[tauri::command]
pub fn remote_save_group(app: AppHandle, group: Group) -> CmdResult<String> {
    let core = core_of(&app)?;
    if group.name.trim().is_empty() {
        return Err("name is required".into());
    }
    app.state::<RemoteState>()
        .with_store(&core, |s| (true, s.upsert_group(group)))
        .map_err(err)
}

#[tauri::command]
pub fn remote_delete_group(app: AppHandle, id: String) -> CmdResult<bool> {
    let core = core_of(&app)?;
    app.state::<RemoteState>()
        .with_store(&core, |s| {
            let r = s.remove_group(&id);
            (r, r)
        })
        .map_err(err)
}

#[tauri::command]
pub fn remote_save_identity(
    app: AppHandle,
    identity: Identity,
    password: Option<String>,
) -> CmdResult<String> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let secrets = rs.secrets(&core);
    let mut identity = identity;
    if identity.label.trim().is_empty() {
        identity.label = identity.username.clone();
    }
    rs.with_store(&core, |s| {
        if identity.id.is_empty() {
            identity.id = veranda_remote::new_id();
        } else if let Some(e) = s.identity(&identity.id) {
            identity.has_password = e.has_password;
        }
        match password.as_deref() {
            Some(p) if !p.is_empty() => {
                identity.has_password = secrets
                    .set(&secrets::identity_password_key(&identity.id), p)
                    .is_ok();
            }
            Some(_) => {
                let _ = secrets.delete(&secrets::identity_password_key(&identity.id));
                identity.has_password = false;
            }
            None => {}
        }
        (true, s.upsert_identity(identity.clone()))
    })
    .map_err(err)
}

#[tauri::command]
pub fn remote_delete_identity(app: AppHandle, id: String) -> CmdResult<bool> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let _ = rs
        .secrets(&core)
        .delete(&secrets::identity_password_key(&id));
    rs.with_store(&core, |s| {
        let r = s.remove_identity(&id);
        (r, r)
    })
    .map_err(err)
}

#[tauri::command]
pub fn remote_save_snippet(app: AppHandle, mut snippet: Snippet) -> CmdResult<String> {
    let core = core_of(&app)?;
    if snippet.label.trim().is_empty() {
        return Err("name is required".into());
    }
    let rs = app.state::<RemoteState>();
    if vault::path(&core.layout).is_file() {
        // the lock is on: only the sealed body goes to disk
        let sealed = rs
            .with_key(|k| match k {
                Some(k) => k.seal(&snippet.script),
                None => anyhow::bail!("snippets are locked; unlock them first"),
            })
            .map_err(err)?;
        snippet.script_enc = sealed;
        snippet.script.clear();
    } else {
        snippet.script_enc.clear();
    }
    rs.with_store(&core, |s| (true, s.upsert_snippet(snippet)))
        .map_err(err)
}

/* ---------------- snippet lock ---------------- */

/// Set, change or remove the snippet password. `current` is required once a lock exists; an
/// empty `new` removes the lock and stores the snippets in plain text again. Every snippet body
/// is re-encrypted with the new key. Returns the new lock state.
#[tauri::command]
pub async fn snippet_lock_set(
    app: AppHandle,
    current: Option<String>,
    new: String,
    auto_lock_minutes: u32,
) -> CmdResult<String> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let existing = vault::LockFile::load(&core.layout).map_err(err)?;
    let old_key = match &existing {
        Some(lock_file) => {
            let (lf, pw) = (lock_file.clone(), current.unwrap_or_default());
            match tauri::async_runtime::spawn_blocking(move || lf.unlock(&pw))
                .await
                .map_err(err)?
                .map_err(err)?
            {
                Some(k) => Some(k),
                None => {
                    rs.vault_failures.fetch_add(1, Ordering::Relaxed);
                    return Err("the current password is wrong".into());
                }
            }
        }
        None => None,
    };
    let new = new.trim().to_string();
    let plain_of = |s: &Snippet| -> anyhow::Result<String> {
        if s.script_enc.is_empty() {
            return Ok(s.script.clone());
        }
        match &old_key {
            Some(k) => k.open(&s.script_enc),
            None => anyhow::bail!("snippet {} cannot be decrypted", s.label),
        }
    };
    if new.is_empty() {
        if existing.is_none() {
            return Ok("off".into());
        }
        rs.with_store(&core, |s| {
            for sn in &mut s.snippets {
                match plain_of(sn) {
                    Ok(p) => {
                        sn.script = p;
                        sn.script_enc.clear();
                    }
                    Err(e) => return (false, Err(e)),
                }
            }
            (true, Ok(()))
        })
        .map_err(err)?
        .map_err(err)?;
        vault::LockFile::remove(&core.layout).map_err(err)?;
        rs.set_key(None, 0);
        let _ = app.emit("remote-changed", ());
        return Ok("off".into());
    }
    let minutes = auto_lock_minutes.min(24 * 60);
    let (lock_file, key) =
        tauri::async_runtime::spawn_blocking(move || vault::LockFile::create(&new, minutes))
            .await
            .map_err(err)?
            .map_err(err)?;
    rs.with_store(&core, |s| {
        for sn in &mut s.snippets {
            match plain_of(sn).and_then(|p| key.seal(&p)) {
                Ok(sealed) => {
                    sn.script_enc = sealed;
                    sn.script.clear();
                }
                Err(e) => return (false, Err(e)),
            }
        }
        (true, Ok(()))
    })
    .map_err(err)?
    .map_err(err)?;
    lock_file.save(&core.layout).map_err(err)?;
    rs.set_key(Some(key), minutes);
    rs.vault_failures.store(0, Ordering::Relaxed);
    let _ = app.emit("remote-changed", ());
    Ok("unlocked".into())
}

/// Open the snippet lock for this run of Veranda. `false` = wrong password (each wrong try
/// slows the next one down).
#[tauri::command]
pub async fn snippet_unlock(app: AppHandle, password: String) -> CmdResult<bool> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let Some(lock_file) = vault::LockFile::load(&core.layout).map_err(err)? else {
        return Ok(true);
    };
    let failures = rs.vault_failures.load(Ordering::Relaxed);
    if failures > 0 {
        tokio::time::sleep(Duration::from_millis(500 * u64::from(failures.min(6)))).await;
    }
    let lf = lock_file.clone();
    let key = tauri::async_runtime::spawn_blocking(move || lf.unlock(&password))
        .await
        .map_err(err)?
        .map_err(err)?;
    match key {
        Some(k) => {
            rs.set_key(Some(k), lock_file.auto_lock_minutes);
            rs.vault_failures.store(0, Ordering::Relaxed);
            Ok(true)
        }
        None => {
            rs.vault_failures.fetch_add(1, Ordering::Relaxed);
            Ok(false)
        }
    }
}

/// Forget the snippet key until the password is entered again.
#[tauri::command]
pub fn snippet_lock_now(state: State<'_, RemoteState>) -> CmdResult<()> {
    state.set_key(None, 0);
    Ok(())
}

/// Type a snippet into terminal `id` (with Enter when `enter`). The body never has to pass
/// through the UI, so it works the same whether the snippet is stored sealed or plain.
#[tauri::command]
pub async fn term_send_snippet(
    app: AppHandle,
    id: String,
    snippet_id: String,
    enter: bool,
) -> CmdResult<()> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let store = Store::load(&core.layout).map_err(err)?;
    let snippet = store.snippet(&snippet_id).ok_or("snippet not found")?;
    let mut text = rs.snippet_script(snippet).map_err(err)?;
    while text.ends_with('\n') || text.ends_with('\r') {
        text.pop();
    }
    if text.is_empty() {
        return Err("the snippet is empty".into());
    }
    if enter {
        text.push('\n');
    }
    let h = lock(&rs.terms).get(&id).and_then(|e| e.handle.clone());
    match h {
        Some(h) => {
            h.write(text.into_bytes()).await;
            Ok(())
        }
        None => Err("no connected terminal".into()),
    }
}

#[tauri::command]
pub fn remote_delete_snippet(app: AppHandle, id: String) -> CmdResult<bool> {
    let core = core_of(&app)?;
    app.state::<RemoteState>()
        .with_store(&core, |s| {
            let r = s.remove_snippet(&id);
            (r, r)
        })
        .map_err(err)
}

#[tauri::command]
pub fn remote_save_forward(app: AppHandle, rule: ForwardRule) -> CmdResult<String> {
    let core = core_of(&app)?;
    if rule.host_id.is_empty() {
        return Err("choose a host".into());
    }
    if rule.bind_port == 0 && rule.kind != veranda_remote::store::ForwardKind::Remote {
        return Err("local port is required".into());
    }
    if rule.kind != veranda_remote::store::ForwardKind::Dynamic
        && (rule.dest_host.trim().is_empty() || rule.dest_port == 0)
    {
        return Err("destination host and port are required".into());
    }
    app.state::<RemoteState>()
        .with_store(&core, |s| (true, s.upsert_forward(rule)))
        .map_err(err)
}

#[tauri::command]
pub async fn remote_delete_forward(app: AppHandle, id: String) -> CmdResult<bool> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    if let Some(f) = rs.forwards.lock().await.remove(&id) {
        f.stop().await;
    }
    rs.with_store(&core, |s| {
        let r = s.remove_forward(&id);
        (r, r)
    })
    .map_err(err)
}

#[tauri::command]
pub fn remote_save_settings(app: AppHandle, settings: Settings) -> CmdResult<()> {
    let core = core_of(&app)?;
    app.state::<RemoteState>()
        .with_store(&core, |s| {
            s.settings = settings;
            (true, ())
        })
        .map_err(err)
}

#[tauri::command]
pub fn remote_history_clear(app: AppHandle) -> CmdResult<()> {
    let core = core_of(&app)?;
    app.state::<RemoteState>()
        .with_store(&core, |s| {
            s.history.clear();
            (true, ())
        })
        .map_err(err)
}

#[tauri::command]
pub fn remote_known_hosts(app: AppHandle) -> CmdResult<Vec<known_hosts::KnownHost>> {
    let core = core_of(&app)?;
    known_hosts::list(&veranda_remote::store::Paths::new(&core.layout).known_hosts()).map_err(err)
}

#[tauri::command]
pub fn remote_known_host_remove(app: AppHandle, line: usize) -> CmdResult<bool> {
    let core = core_of(&app)?;
    known_hosts::remove_line(
        &veranda_remote::store::Paths::new(&core.layout).known_hosts(),
        line,
    )
    .map_err(err)
}

/* ---------------- keys ---------------- */

fn keys_dir(core: &Veranda) -> PathBuf {
    veranda_remote::store::Paths::new(&core.layout).keys_dir()
}

#[tauri::command]
pub fn remote_key_generate(
    app: AppHandle,
    label: String,
    kind: String,
    passphrase: Option<String>,
    save_passphrase: Option<bool>,
) -> CmdResult<KeyMeta> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let label = if label.trim().is_empty() {
        format!("{kind} key")
    } else {
        label.trim().to_string()
    };
    let mut meta =
        keys::generate(&keys_dir(&core), &label, &kind, passphrase.as_deref()).map_err(err)?;
    if meta.encrypted && save_passphrase.unwrap_or(false) {
        meta.has_passphrase = rs
            .secrets(&core)
            .set(
                &secrets::key_passphrase_key(&meta.id),
                passphrase.as_deref().unwrap_or(""),
            )
            .is_ok();
    }
    let m = meta.clone();
    rs.with_store(&core, |s| {
        s.keys.push(m);
        (true, ())
    })
    .map_err(err)?;
    Ok(meta)
}

#[tauri::command]
pub fn remote_key_import(
    app: AppHandle,
    label: String,
    text: Option<String>,
    path: Option<String>,
    passphrase: Option<String>,
    save_passphrase: Option<bool>,
) -> CmdResult<KeyMeta> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let text = match (text, path) {
        (Some(t), _) if !t.trim().is_empty() => t,
        (_, Some(p)) if !p.trim().is_empty() => {
            std::fs::read_to_string(p.trim()).map_err(|e| format!("reading {p}: {e}"))?
        }
        _ => return Err("paste a private key or choose a file".into()),
    };
    let label = if label.trim().is_empty() {
        "imported key".to_string()
    } else {
        label.trim().to_string()
    };
    let mut meta =
        keys::import(&keys_dir(&core), &label, &text, passphrase.as_deref()).map_err(err)?;
    if meta.encrypted && save_passphrase.unwrap_or(false) {
        meta.has_passphrase = rs
            .secrets(&core)
            .set(
                &secrets::key_passphrase_key(&meta.id),
                passphrase.as_deref().unwrap_or(""),
            )
            .is_ok();
    }
    let m = meta.clone();
    rs.with_store(&core, |s| {
        s.keys.push(m);
        (true, ())
    })
    .map_err(err)?;
    Ok(meta)
}

#[tauri::command]
pub fn remote_key_candidates() -> Vec<String> {
    keys::user_ssh_keys()
        .into_iter()
        .map(|p| p.display().to_string())
        .collect()
}

#[tauri::command]
pub fn remote_key_delete(app: AppHandle, id: String) -> CmdResult<bool> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let _ = rs.secrets(&core).delete(&secrets::key_passphrase_key(&id));
    keys::delete(&keys_dir(&core), &id).map_err(err)?;
    rs.with_store(&core, |s| {
        let r = s.remove_key(&id);
        (r, r)
    })
    .map_err(err)
}

/// Write the public (`private = false`) or private key to a file the user picked.
#[tauri::command]
pub fn remote_key_export(app: AppHandle, id: String, path: String, private: bool) -> CmdResult<()> {
    let core = core_of(&app)?;
    let store = Store::load(&core.layout).map_err(err)?;
    let meta = store.key(&id).ok_or("unknown key")?;
    let target = Path::new(path.trim());
    if private {
        let text = keys::export_private(&keys_dir(&core), &id).map_err(err)?;
        veranda_core::fsutil::write_private(target, text.as_bytes()).map_err(err)
    } else {
        std::fs::write(target, format!("{}\n", meta.public_key)).map_err(err)
    }
}

#[tauri::command]
pub fn remote_key_passphrase(
    app: AppHandle,
    id: String,
    current: Option<String>,
    new: Option<String>,
    save_passphrase: Option<bool>,
) -> CmdResult<KeyMeta> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let secrets = rs.secrets(&core);
    let current = match current.filter(|c| !c.is_empty()) {
        Some(c) => Some(c),
        None => secrets
            .get(&secrets::key_passphrase_key(&id))
            .map_err(err)?,
    };
    let encrypted =
        keys::change_passphrase(&keys_dir(&core), &id, current.as_deref(), new.as_deref())
            .map_err(err)?;
    let has = if encrypted && save_passphrase.unwrap_or(false) {
        secrets
            .set(
                &secrets::key_passphrase_key(&id),
                new.as_deref().unwrap_or(""),
            )
            .is_ok()
    } else {
        let _ = secrets.delete(&secrets::key_passphrase_key(&id));
        false
    };
    rs.with_store(&core, |s| {
        let m = s.keys.iter_mut().find(|k| k.id == id).map(|k| {
            k.encrypted = encrypted;
            k.has_passphrase = has;
            k.clone()
        });
        (true, m)
    })
    .map_err(err)?
    .ok_or_else(|| "unknown key".to_string())
}

/// Install a key's public half into `~/.ssh/authorized_keys` on a host (like `ssh-copy-id`).
#[tauri::command]
pub async fn remote_key_deploy(
    app: AppHandle,
    host_id: String,
    key_id: String,
) -> CmdResult<String> {
    let core = core_of(&app)?;
    let store = Store::load(&core.layout).map_err(err)?;
    let host = store.host(&host_id).cloned().ok_or("unknown host")?;
    let meta = store.key(&key_id).cloned().ok_or("unknown key")?;
    let conn = connect_host(&app, &core, &host, &format!("deploy:{host_id}"), "ssh")
        .await
        .map_err(err)?;
    let r = client::deploy_public_key(&conn, &meta.public_key)
        .await
        .map_err(err);
    conn.disconnect().await;
    r?;
    Ok(format!("{} installed on {}", meta.label, host.label))
}

/* ---------------- terminals ---------------- */

#[derive(Deserialize)]
pub struct OpenTerm {
    pub id: String,
    pub host_id: Option<String>,
    /// `user@host:port` for Quick connect (not saved).
    pub quick: Option<String>,
    #[serde(default)]
    pub local: bool,
    pub cols: u16,
    pub rows: u16,
    pub cwd: Option<String>,
}

fn pump_terminal(app: AppHandle, id: String, mut rx: mpsc::Receiver<session::TermEvent>) {
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                session::TermEvent::Data(d) => {
                    let _ = app.emit(
                        "term-out",
                        serde_json::json!({ "id": id, "data": BASE64_STANDARD.encode(&d) }),
                    );
                }
                session::TermEvent::Error(e) => {
                    app.state::<RemoteState>()
                        .set_term_state(&app, &id, "error", &e);
                }
                session::TermEvent::Exit(code) => {
                    let rs = app.state::<RemoteState>();
                    rs.set_term_state(
                        &app,
                        &id,
                        "closed",
                        &code.map(|c| format!("exit {c}")).unwrap_or_default(),
                    );
                    let info = lock(&rs.terms).get(&id).map(|e| e.info.clone());
                    if let (Some(info), Ok(core)) = (info, core_of(&app))
                        && info.kind == "ssh"
                    {
                        let _ = rs.with_store(&core, |s| {
                            if let Some(h) = s.history.iter_mut().find(|h| {
                                h.host_id == info.host_id
                                    && h.started_at == info.started_at
                                    && h.ended_at == 0
                            }) {
                                h.ended_at = veranda_remote::now();
                            }
                            (true, ())
                        });
                    }
                    let _ = app.emit("term-exit", serde_json::json!({ "id": id, "code": code }));
                    break;
                }
            }
        }
    });
}

#[tauri::command]
pub async fn term_open(app: AppHandle, req: OpenTerm) -> CmdResult<SessionInfo> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let store = Store::load(&core.layout).map_err(err)?;
    let (host, kind) = if req.local {
        (None, "local")
    } else if let Some(hid) = &req.host_id {
        (Some(store.host(hid).cloned().ok_or("unknown host")?), "ssh")
    } else if let Some(q) = &req.quick {
        (Some(quick_host(q).map_err(err)?), "ssh")
    } else {
        return Err("nothing to open".into());
    };
    let started = veranda_remote::now();
    let info = SessionInfo {
        id: req.id.clone(),
        kind: kind.into(),
        host_id: host
            .as_ref()
            .map(|h| h.id.clone())
            .filter(|i| !i.is_empty()),
        label: host
            .as_ref()
            .map(|h| h.label.clone())
            .unwrap_or_else(|| "Local".into()),
        target: host.as_ref().map(|h| h.summary()).unwrap_or_default(),
        state: "connecting".into(),
        message: String::new(),
        started_at: started,
    };
    lock(&rs.terms).insert(
        req.id.clone(),
        TermEntry {
            handle: None,
            info: info.clone(),
            conn: None,
            cpu_prev: None,
            stats: None,
            stats_busy: false,
        },
    );
    let _ = app.emit("term-state", info.clone());
    let (sink, sink_rx) = mpsc::channel(512);
    let result: anyhow::Result<session::TermHandle> = async {
        if let Some(host) = &host {
            let conn = connect_host(&app, &core, host, &req.id, "ssh").await?;
            if let Some(e) = lock(&rs.terms).get_mut(&req.id) {
                e.conn = Some(conn.clone());
            }
            let startup = match host
                .startup_snippet_id
                .as_deref()
                .and_then(|id| store.snippet(id))
                .map(|s| rs.snippet_script(s))
            {
                Some(Ok(script)) => Some(script),
                Some(Err(e)) => {
                    let _ = app.emit(
                        "remote-notice",
                        serde_json::json!({ "session": req.id, "text": format!("startup snippet skipped: {e}") }),
                    );
                    None
                }
                None => None,
            };
            let handle = session::open_ssh_shell(
                conn.clone(),
                session::ShellOptions {
                    cols: req.cols,
                    rows: req.rows,
                    agent_forwarding: host.agent_forwarding,
                    env: host.env.clone(),
                    startup,
                },
                sink,
            )
            .await?;
            // remember the server's OS for the host list (best effort, in the background)
            if !host.id.is_empty() && host.os.is_empty() {
                let (app2, core2, hid) = (app.clone(), core.clone(), host.id.clone());
                tauri::async_runtime::spawn(async move {
                    let os = client::detect_os(&conn).await;
                    if !os.is_empty() {
                        let rs = app2.state::<RemoteState>();
                        let _ = rs.with_store(&core2, |s| {
                            if let Some(h) = s.host_mut(&hid) {
                                h.os = os.clone();
                            }
                            (true, ())
                        });
                        let _ = app2.emit("remote-changed", ());
                    }
                });
            }
            Ok(handle)
        } else {
            let (program, args) = local::default_shell(&store.settings.local_shell);
            let reg = veranda_core::packages::Registry::load(&core.layout)?;
            let runtime = veranda_core::packages::runtime_path(&core.layout, &reg, &core.config);
            let path = veranda_core::process::env_path(&core.layout.path_entries(&runtime));
            let cwd = req
                .cwd
                .as_deref()
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
                .unwrap_or_else(sftp::local_home);
            session::open_local(
                session::LocalOptions {
                    program,
                    args,
                    cwd: Some(cwd),
                    env: vec![("PATH".into(), path.to_string_lossy().into_owned())],
                    cols: req.cols,
                    rows: req.rows,
                },
                sink,
            )
        }
    }
    .await;
    match result {
        Ok(handle) => {
            {
                let mut g = lock(&rs.terms);
                if let Some(e) = g.get_mut(&req.id) {
                    e.handle = Some(handle);
                }
            }
            pump_terminal(app.clone(), req.id.clone(), sink_rx);
            rs.set_term_state(&app, &req.id, "connected", "");
            Ok(lock(&rs.terms)
                .get(&req.id)
                .map(|e| e.info.clone())
                .unwrap_or(info))
        }
        Err(e) => {
            let msg = format!("{e:#}");
            rs.set_term_state(&app, &req.id, "error", &msg);
            Err(msg)
        }
    }
}

#[tauri::command]
pub async fn term_write(state: State<'_, RemoteState>, id: String, data: String) -> CmdResult<()> {
    let h = lock(&state.terms).get(&id).and_then(|e| e.handle.clone());
    if let Some(h) = h {
        h.write(data.into_bytes()).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn term_resize(
    state: State<'_, RemoteState>,
    id: String,
    cols: u16,
    rows: u16,
) -> CmdResult<()> {
    let h = lock(&state.terms).get(&id).and_then(|e| e.handle.clone());
    if let Some(h) = h {
        h.resize(cols, rows).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn term_close(app: AppHandle, id: String) -> CmdResult<()> {
    let rs = app.state::<RemoteState>();
    let entry = lock(&rs.terms).remove(&id);
    if let Some(e) = entry
        && let Some(h) = e.handle
    {
        h.close().await;
    }
    Ok(())
}

/* ---------------- SFTP ---------------- */

#[tauri::command]
pub async fn sftp_open(app: AppHandle, host_id: String) -> CmdResult<SftpInfo> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let store = Store::load(&core.layout).map_err(err)?;
    let host = store.host(&host_id).cloned().ok_or("unknown host")?;
    let id = veranda_remote::new_id();
    let conn = connect_host(&app, &core, &host, &format!("sftp:{id}"), "sftp")
        .await
        .map_err(err)?;
    let s = sftp::Sftp::open(conn).await.map_err(err)?;
    let home = s.home().await.unwrap_or_else(|_| "/".into());
    let info = SftpInfo {
        id: id.clone(),
        host_id,
        label: host.label.clone(),
        home,
    };
    rs.sftps.lock().await.insert(
        id,
        SftpEntry {
            sftp: Arc::new(s),
            info: info.clone(),
        },
    );
    Ok(info)
}

async fn sftp_of(rs: &RemoteState, id: &str) -> CmdResult<Arc<sftp::Sftp>> {
    rs.sftps
        .lock()
        .await
        .get(id)
        .map(|e| e.sftp.clone())
        .ok_or_else(|| "SFTP session closed; open it again".to_string())
}

#[derive(Serialize)]
pub struct Listing {
    path: String,
    entries: Vec<sftp::Entry>,
    roots: Vec<String>,
}

#[tauri::command]
pub async fn sftp_list(
    state: State<'_, RemoteState>,
    id: String,
    path: String,
) -> CmdResult<Listing> {
    let s = sftp_of(&state, &id).await?;
    let path = if path.trim().is_empty() {
        s.home().await.map_err(err)?
    } else {
        s.canonicalize(&sftp::normalize(path.trim()))
            .await
            .unwrap_or_else(|_| sftp::normalize(path.trim()))
    };
    let entries = s.list(&path).await.map_err(err)?;
    Ok(Listing {
        path,
        entries,
        roots: vec!["/".into()],
    })
}

#[tauri::command]
pub async fn sftp_mkdir(state: State<'_, RemoteState>, id: String, path: String) -> CmdResult<()> {
    sftp_of(&state, &id).await?.mkdir(&path).await.map_err(err)
}

#[tauri::command]
pub async fn sftp_rename(
    state: State<'_, RemoteState>,
    id: String,
    from: String,
    to: String,
) -> CmdResult<()> {
    sftp_of(&state, &id)
        .await?
        .rename(&from, &to)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn sftp_delete(state: State<'_, RemoteState>, id: String, path: String) -> CmdResult<()> {
    sftp_of(&state, &id).await?.remove(&path).await.map_err(err)
}

#[tauri::command]
pub async fn sftp_chmod(
    state: State<'_, RemoteState>,
    id: String,
    path: String,
    mode: u32,
) -> CmdResult<()> {
    sftp_of(&state, &id)
        .await?
        .chmod(&path, mode)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn sftp_read(
    state: State<'_, RemoteState>,
    id: String,
    path: String,
) -> CmdResult<String> {
    sftp_of(&state, &id)
        .await?
        .read_text(&path, 2 * 1024 * 1024)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn sftp_write(
    state: State<'_, RemoteState>,
    id: String,
    path: String,
    text: String,
) -> CmdResult<()> {
    sftp_of(&state, &id)
        .await?
        .write_text(&path, &text)
        .await
        .map_err(err)
}

fn transfer_flag(rs: &RemoteState, transfer: &str) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    lock(&rs.transfers).insert(transfer.to_string(), flag.clone());
    flag
}

fn progress_emitter(
    app: &AppHandle,
    transfer: &str,
    name: &str,
) -> impl Fn(u64, u64) + Sync + use<> {
    let app = app.clone();
    let (transfer, name) = (transfer.to_string(), name.to_string());
    let last = Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(1));
    move |done, total| {
        let mut l = lock(&last);
        if l.elapsed().as_millis() >= 100 || done >= total {
            *l = std::time::Instant::now();
            let _ = app.emit(
                "sftp-progress",
                serde_json::json!({ "transfer": transfer, "name": name, "done": done, "total": total }),
            );
        }
    }
}

#[tauri::command]
pub async fn sftp_download(
    app: AppHandle,
    id: String,
    remote: String,
    local: String,
    transfer: String,
) -> CmdResult<()> {
    let rs = app.state::<RemoteState>();
    let s = sftp_of(&rs, &id).await?;
    let flag = transfer_flag(&rs, &transfer);
    let name = remote.rsplit('/').next().unwrap_or(&remote).to_string();
    let progress = progress_emitter(&app, &transfer, &name);
    let r = s
        .download(&remote, Path::new(&local), &flag, &progress)
        .await
        .map_err(err);
    lock(&rs.transfers).remove(&transfer);
    r
}

#[tauri::command]
pub async fn sftp_upload(
    app: AppHandle,
    id: String,
    local: String,
    remote: String,
    transfer: String,
) -> CmdResult<()> {
    let rs = app.state::<RemoteState>();
    let s = sftp_of(&rs, &id).await?;
    let flag = transfer_flag(&rs, &transfer);
    let name = Path::new(&local)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let progress = progress_emitter(&app, &transfer, &name);
    let r = s
        .upload(Path::new(&local), &remote, &flag, &progress)
        .await
        .map_err(err);
    lock(&rs.transfers).remove(&transfer);
    r
}

#[tauri::command]
pub fn sftp_cancel(state: State<'_, RemoteState>, transfer: String) {
    if let Some(f) = lock(&state.transfers).get(&transfer) {
        f.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
pub async fn sftp_close(state: State<'_, RemoteState>, id: String) -> CmdResult<()> {
    let e = state.sftps.lock().await.remove(&id);
    if let Some(e) = e {
        e.sftp.close().await;
    }
    Ok(())
}

#[tauri::command]
pub fn local_list(path: Option<String>) -> CmdResult<Listing> {
    let p = path
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(sftp::local_home);
    let p = p.canonicalize().unwrap_or(p);
    let entries = sftp::local_list(&p).map_err(err)?;
    Ok(Listing {
        path: p.display().to_string(),
        entries,
        roots: sftp::local_roots(),
    })
}

#[tauri::command]
pub fn local_mkdir(path: String) -> CmdResult<()> {
    std::fs::create_dir_all(path.trim()).map_err(err)
}

#[tauri::command]
pub fn local_rename(from: String, to: String) -> CmdResult<()> {
    std::fs::rename(from.trim(), to.trim()).map_err(err)
}

#[tauri::command]
pub fn local_delete(path: String) -> CmdResult<()> {
    let p = Path::new(path.trim());
    if p.is_dir() {
        std::fs::remove_dir_all(p).map_err(err)
    } else {
        std::fs::remove_file(p).map_err(err)
    }
}

/* ---------------- snippets ---------------- */

#[derive(Serialize)]
pub struct SnippetOutcome {
    host_id: String,
    label: String,
    exit: Option<u32>,
    stdout: String,
    stderr: String,
    error: String,
}

/// Run a snippet (or ad-hoc script) on each host without a terminal and collect the output.
#[tauri::command]
pub async fn snippet_run(
    app: AppHandle,
    snippet_id: Option<String>,
    script: Option<String>,
    host_ids: Vec<String>,
) -> CmdResult<Vec<SnippetOutcome>> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let store = Store::load(&core.layout).map_err(err)?;
    let script = match (
        snippet_id.as_deref().and_then(|id| store.snippet(id)),
        script,
    ) {
        (Some(s), _) => rs.snippet_script(s).map_err(err)?,
        (None, Some(t)) if !t.trim().is_empty() => t,
        _ => return Err("nothing to run".into()),
    };
    let mut out = vec![];
    for hid in host_ids {
        let Some(host) = store.host(&hid).cloned() else {
            continue;
        };
        let mut o = SnippetOutcome {
            host_id: hid.clone(),
            label: host.label.clone(),
            exit: None,
            stdout: String::new(),
            stderr: String::new(),
            error: String::new(),
        };
        match connect_host(&app, &core, &host, &format!("snippet:{hid}"), "snippet").await {
            Ok(conn) => {
                match client::exec(&conn, &script, 300).await {
                    Ok(r) => {
                        o.exit = r.exit;
                        o.stdout = r.stdout;
                        o.stderr = r.stderr;
                    }
                    Err(e) => o.error = format!("{e:#}"),
                }
                conn.disconnect().await;
            }
            Err(e) => o.error = format!("{e:#}"),
        }
        out.push(o);
    }
    Ok(out)
}

/* ---------------- port forwarding ---------------- */

#[tauri::command]
pub async fn forward_start(app: AppHandle, id: String) -> CmdResult<ForwardStatus> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let store = Store::load(&core.layout).map_err(err)?;
    let rule = store.forward(&id).cloned().ok_or("unknown rule")?;
    let host = store
        .host(&rule.host_id)
        .cloned()
        .ok_or("the rule's host no longer exists")?;
    if let Some(old) = rs.forwards.lock().await.remove(&id) {
        old.stop().await;
    }
    let conn = connect_host(&app, &core, &host, &format!("forward:{id}"), "forward")
        .await
        .map_err(err)?;
    let active = forward::start(conn, &rule).await.map_err(err)?;
    let status = ForwardStatus {
        rule_id: id.clone(),
        connections: 0,
        total: 0,
        bound_port: active.bound_port,
    };
    rs.forwards.lock().await.insert(id, active);
    Ok(status)
}

#[tauri::command]
pub async fn forward_stop(app: AppHandle, id: String) -> CmdResult<()> {
    let rs = app.state::<RemoteState>();
    let f = rs.forwards.lock().await.remove(&id);
    if let Some(f) = f {
        f.stop().await;
    }
    Ok(())
}

/* ---------------- import ---------------- */

#[tauri::command]
pub fn remote_import_detect() -> Vec<import::ImportedHost> {
    let mut v = import::from_ssh_config();
    v.extend(import::from_putty());
    v
}

#[derive(Serialize)]
pub struct ImportReport {
    hosts: usize,
    keys: usize,
    skipped: Vec<String>,
}

#[tauri::command]
pub fn remote_import_apply(
    app: AppHandle,
    hosts: Vec<import::ImportedHost>,
    import_keys: bool,
) -> CmdResult<ImportReport> {
    let core = core_of(&app)?;
    let rs = app.state::<RemoteState>();
    let dir = keys_dir(&core);
    rs.with_store(&core, |s| {
        let mut report = ImportReport {
            hosts: 0,
            keys: 0,
            skipped: vec![],
        };
        let mut key_by_file: HashMap<String, String> = HashMap::new();
        let mut id_by_alias: HashMap<String, String> = HashMap::new();
        let mut pending_jumps: Vec<(String, String)> = vec![];
        for h in hosts {
            if h.hostname.trim().is_empty() {
                continue;
            }
            if s.hosts
                .iter()
                .any(|x| x.label == h.alias && x.address == h.hostname)
            {
                report.skipped.push(format!("{} (already exists)", h.alias));
                continue;
            }
            let mut host = Host {
                label: h.alias.clone(),
                address: h.hostname.clone(),
                port: if h.port == 0 { 22 } else { h.port },
                username: h.user.clone(),
                auth: AuthKind::None,
                tags: vec![h.source.clone()],
                ..Default::default()
            };
            if import_keys && !h.identity_file.is_empty() {
                let file = h.identity_file.clone();
                let key_id = match key_by_file.get(&file) {
                    Some(id) => Some(id.clone()),
                    None => match std::fs::read_to_string(&file) {
                        Ok(text) => {
                            let label = Path::new(&file)
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "imported".into());
                            match s
                                .keys
                                .iter()
                                .find(|k| k.label == label)
                                .map(|k| k.id.clone())
                            {
                                Some(existing) => Some(existing),
                                None => match keys::import(&dir, &label, &text, None) {
                                    Ok(meta) => {
                                        let id = meta.id.clone();
                                        s.keys.push(meta);
                                        report.keys += 1;
                                        Some(id)
                                    }
                                    Err(e) => {
                                        report.skipped.push(format!("key {file}: {e}"));
                                        None
                                    }
                                },
                            }
                        }
                        Err(e) => {
                            report.skipped.push(format!("key {file}: {e}"));
                            None
                        }
                    },
                };
                if let Some(id) = key_id {
                    key_by_file.insert(file, id.clone());
                    host.key_id = Some(id);
                    host.auth = AuthKind::Key;
                }
            }
            let id = s.upsert_host(host);
            id_by_alias.insert(h.alias.clone(), id.clone());
            if !h.proxy_jump.is_empty() {
                pending_jumps.push((
                    id,
                    h.proxy_jump
                        .split(',')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                ));
            }
            report.hosts += 1;
        }
        for (id, jump) in pending_jumps {
            let target = id_by_alias.get(&jump).cloned().or_else(|| {
                s.hosts
                    .iter()
                    .find(|x| x.label == jump)
                    .map(|x| x.id.clone())
            });
            if let (Some(t), Some(h)) = (target, s.host_mut(&id)) {
                h.jump_host_id = Some(t);
            }
        }
        (report.hosts > 0 || report.keys > 0, report)
    })
    .map_err(err)
}

/// Open an http(s) link clicked inside a terminal in the system browser.
#[tauri::command]
pub fn remote_open_link(app: AppHandle, url: String) -> CmdResult<()> {
    let core = core_of(&app)?;
    let u = url.trim();
    if !(u.starts_with("https://") || u.starts_with("http://")) || u.len() > 2048 {
        return Err("only http(s) links are opened".into());
    }
    veranda_core::tools::open_url(&core.config, u).map_err(err)
}

/// Close every live session (app quit).
pub fn shutdown(app: &AppHandle) {
    let rs = app.state::<RemoteState>();
    let handles: Vec<session::TermHandle> = lock(&rs.terms)
        .drain()
        .filter_map(|(_, e)| e.handle)
        .collect();
    tauri::async_runtime::block_on(async move {
        for h in handles {
            h.close().await;
        }
        let forwards: Vec<forward::ActiveForward> =
            rs.forwards.lock().await.drain().map(|(_, f)| f).collect();
        for f in forwards {
            f.stop().await;
        }
        let sftps: Vec<SftpEntry> = rs.sftps.lock().await.drain().map(|(_, e)| e).collect();
        for e in sftps {
            e.sftp.close().await;
        }
    });
}

#[cfg(test)]
mod stats_tests {
    use super::parse_stats;

    const SAMPLE1: &str = "cpu  1000 0 500 8000 100 0 0 0 0 0\ncpu0 500 0 250 4000 50 0 0 0 0 0\ncpu1 500 0 250 4000 50 0 0 0 0 0\ncpusep\ncpu  1300 0 600 8600 100 0 0 0 0 0\ncpu0 800 0 350 4100 50 0 0 0 0 0\ncpu1 500 0 250 4500 50 0 0 0 0 0\nMemTotal:        3980000 kB\nMemFree:         1000000 kB\nMemAvailable:    2600000 kB\nBuffers:          200000 kB\nCached:          1500000 kB\nSReclaimable:     100000 kB\nShmem:             50000 kB\nSwapTotal:       1000000 kB\nSwapFree:         750000 kB\nload 0.52 0.40 0.35 1/230 4567\ncores 4\nup 123456.78\ndisk /dev/vda1 41943040 33554432 8388608 81% /\ndisk tmpfs 1000 10 990 1% /run\ndisk /dev/vdb1 104857600 10485760 94371840 10% /data\ndisk /dev/vda1 41943040 33554432 8388608 81% /var/lib/docker/overlay2/x/merged\n";
    // a host whose shell could not take the second sample: one block, no `cpusep`
    const SAMPLE2: &str = "cpu  2000 0 800 9700 100 0 0 0 0 0\n";

    #[test]
    fn parses_linux_proc_and_df() {
        let mut prev = None;
        let s = parse_stats(SAMPLE1, &mut prev);
        // busy went 1500 -> 1900 (+400) while total went 9600 -> 10600 (+1000): 40 %
        assert!(
            (s.cpu.unwrap() - 40.0).abs() < 0.01,
            "one-second delta inside the command"
        );
        assert_eq!(s.cpus.len(), 2);
        assert!((s.cpus[0] - 80.0).abs() < 0.01 && s.cpus[1].abs() < 0.01);
        assert_eq!(s.mem_total_kb, Some(3_980_000));
        // like htop: total − free − (buffers + cached + reclaimable − shmem)
        assert_eq!(s.mem_cache_kb, Some(1_750_000));
        assert_eq!(s.mem_used_kb, Some(1_230_000));
        assert_eq!(s.mem_avail_kb, Some(2_600_000));
        assert_eq!(s.swap_total_kb, Some(1_000_000));
        assert_eq!(s.swap_used_kb, Some(250_000));
        assert_eq!(
            (s.load1, s.load5, s.load15),
            (Some(0.52), Some(0.40), Some(0.35))
        );
        assert_eq!((s.procs_running, s.procs_total), (Some(1), Some(230)));
        assert_eq!(s.cores, Some(4));
        assert_eq!(s.uptime_secs, Some(123_456));
        assert_eq!(s.disk_total_kb, Some(41_943_040));
        assert_eq!(s.disk_used_kb, Some(33_554_432));
        assert_eq!(s.disk_avail_kb, Some(8_388_608));
        assert_eq!(s.disk_mount.as_deref(), Some("/"));
        assert_eq!(s.disks.len(), 2, "tmpfs skipped, duplicate device folded");
        assert_eq!(s.disks[1].mount, "/data");
        let s2 = parse_stats(SAMPLE2, &mut prev);
        // against the previous poll: busy 1900 -> 2800 (+900), total 10600 -> 12600 (+2000): 45 %
        assert!((s2.cpu.unwrap() - 45.0).abs() < 0.01);
        assert!(
            s2.cpus.is_empty(),
            "no per-core figures without a pair of samples"
        );
    }

    #[test]
    fn tolerates_a_host_without_proc() {
        let mut prev = None;
        let s = parse_stats("cpusep\nload \ncores \nup \ndisk \n", &mut prev);
        assert!(s.cpu.is_none() && s.mem_total_kb.is_none() && s.disk_total_kb.is_none());
        assert!(s.cpus.is_empty() && s.swap_total_kb.is_none() && s.load1.is_none());
    }
}
