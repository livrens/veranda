//! Tauri shell around `veranda-core`: commands for the web UI, tray icon, background jobs.

mod files;
mod remote;
mod transfer;

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::image::Image;
use tauri::menu::{IconMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::UpdaterExt;
use veranda_core::manifest::{Kind, Manifest};
use veranda_core::packages::{Installed, Registry};
use veranda_core::services::{self, ServiceId, ServiceStatus};
use veranda_core::sites::Site;
use veranda_core::{Config, Platform, Veranda};

pub struct AppState {
    pub app: Mutex<Veranda>,
    /// Unix time of the last `ui_heartbeat`; 0 until the UI has rendered once.
    pub heartbeat: std::sync::atomic::AtomicU64,
    /// App lock (PIN/passphrase): while true every command that reads or changes state fails
    /// with "locked" until `app_unlock` succeeds. Set at launch when a lock file exists.
    pub locked: std::sync::atomic::AtomicBool,
    /// Consecutive failed unlock attempts and the unix time of the last one (rate limit).
    pub unlock_fails: Mutex<(u32, u64)>,
    /// Authenticator secret being set up (confirmed with one code before it is saved).
    pub totp_pending: Mutex<Option<String>>,
    /// Counter of the last accepted authenticator code: the same code is never accepted twice.
    pub totp_last: Mutex<u64>,
    /// The migration running in the background (dialog may be closed meanwhile) and its result.
    pub migrate: Mutex<MigrateJob>,
}

#[derive(Clone, Default, Serialize)]
pub struct MigrateJob {
    pub running: bool,
    pub paused: bool,
    /// 0 = running, 1 = stops after the current database, 2 = stopping now.
    pub cancel: u8,
    #[serde(skip)]
    pub control: std::sync::Arc<veranda_core::migrate::Control>,
    pub progress: veranda_core::migrate::Progress,
    pub report: Option<veranda_core::migrate::Report>,
    pub error: Option<String>,
    pub started_at: u64,
    pub finished_at: u64,
}

fn is_locked(state: &State<'_, AppState>) -> bool {
    state.locked.load(std::sync::atomic::Ordering::Relaxed)
}

/// The app without the lock gate: only for commands the lock screen itself needs
/// (showing the window, heartbeat, close handling).
fn peek_app(state: &State<'_, AppState>) -> CmdResult<Veranda> {
    Ok(state
        .app
        .lock()
        .map_err(|_| "state poisoned".to_string())?
        .clone())
}

#[derive(Serialize)]
struct LockStatus {
    enabled: bool,
    locked: bool,
    /// `pin` or `password`.
    kind: String,
    /// Second factor (authenticator app) enabled.
    totp: bool,
    auto_lock_minutes: u32,
    /// Seconds the next unlock attempt has to wait (0 = none); grows after repeated failures.
    wait_secs: u64,
}

fn unlock_wait(state: &State<'_, AppState>) -> u64 {
    let Ok(g) = state.unlock_fails.lock() else {
        return 0;
    };
    let (fails, last) = *g;
    if fails < 5 {
        return 0;
    }
    // 30 s after the 5th failure, doubling each time, capped at 10 minutes.
    let delay = (30u64 << (fails - 5).min(5)).min(600);
    (last + delay).saturating_sub(now_secs())
}

#[tauri::command]
fn lock_status(state: State<'_, AppState>) -> CmdResult<LockStatus> {
    let core = peek_app(&state)?;
    let file = veranda_core::applock::load(&core.layout).map_err(err)?;
    Ok(LockStatus {
        enabled: file.is_some(),
        locked: is_locked(&state),
        kind: file
            .as_ref()
            .map(|f| f.kind.clone())
            .unwrap_or_else(|| "pin".into()),
        totp: file.as_ref().is_some_and(|f| f.totp.is_some()),
        auto_lock_minutes: file.map(|f| f.auto_lock_minutes).unwrap_or(0),
        wait_secs: unlock_wait(&state),
    })
}

/// Errors: `wait:<s>` (rate limit), `wrong` (secret), `code` (second factor missing or wrong).
/// A wrong code counts as a failed attempt like a wrong secret.
#[tauri::command]
async fn app_unlock(
    state: State<'_, AppState>,
    secret: String,
    code: Option<String>,
) -> CmdResult<()> {
    let wait = unlock_wait(&state);
    if wait > 0 {
        return Err(format!("wait:{wait}"));
    }
    let core = peek_app(&state)?;
    let layout = core.layout.clone();
    let (ok, totp) =
        tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<(bool, Option<String>)> {
            let ok = veranda_core::applock::verify(&layout, &secret)?;
            let totp = veranda_core::applock::load(&layout)?.and_then(|f| f.totp);
            Ok((ok, totp))
        })
        .await
        .map_err(err)?
        .map_err(err)?;
    let fail = |what: &str| -> CmdResult<()> {
        let mut g = state
            .unlock_fails
            .lock()
            .map_err(|_| "state poisoned".to_string())?;
        *g = (g.0.saturating_add(1), now_secs());
        Err(what.into())
    };
    if !ok {
        return fail("wrong");
    }
    if let Some(secret) = totp {
        let counter =
            veranda_core::applock::totp_verify(&secret, code.as_deref().unwrap_or(""), now_secs())
                .map_err(err)?;
        let Some(counter) = counter else {
            return fail("code");
        };
        let mut last = state
            .totp_last
            .lock()
            .map_err(|_| "state poisoned".to_string())?;
        if counter <= *last {
            return fail("code");
        }
        *last = counter;
    }
    let mut g = state
        .unlock_fails
        .lock()
        .map_err(|_| "state poisoned".to_string())?;
    *g = (0, 0);
    state
        .locked
        .store(false, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

#[derive(Serialize)]
struct TotpSetup {
    secret: String,
    uri: String,
}

/// Start enabling the second factor: a new secret, shown to the user for the authenticator
/// app, saved only after `app_totp_confirm` proves the app produces the right codes.
#[tauri::command]
async fn app_totp_begin(state: State<'_, AppState>, current: String) -> CmdResult<TotpSetup> {
    let core = open_app(&state)?;
    let layout = core.layout.clone();
    let ok = tauri::async_runtime::spawn_blocking(move || {
        veranda_core::applock::verify(&layout, &current)
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    if !ok || !veranda_core::applock::is_enabled(&core.layout) {
        return Err("wrong".into());
    }
    let secret = veranda_core::applock::totp_new_secret();
    let account = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into());
    let uri = veranda_core::applock::totp_uri(&secret, &account);
    *state
        .totp_pending
        .lock()
        .map_err(|_| "state poisoned".to_string())? = Some(secret.clone());
    Ok(TotpSetup { secret, uri })
}

#[tauri::command]
fn app_totp_confirm(state: State<'_, AppState>, code: String) -> CmdResult<()> {
    let core = open_app(&state)?;
    let pending = state
        .totp_pending
        .lock()
        .map_err(|_| "state poisoned".to_string())?
        .clone();
    let Some(secret) = pending else {
        return Err("no setup in progress".into());
    };
    let Some(counter) =
        veranda_core::applock::totp_verify(&secret, &code, now_secs()).map_err(err)?
    else {
        return Err("code".into());
    };
    veranda_core::applock::set_totp(&core.layout, Some(&secret)).map_err(err)?;
    *state
        .totp_pending
        .lock()
        .map_err(|_| "state poisoned".to_string())? = None;
    *state
        .totp_last
        .lock()
        .map_err(|_| "state poisoned".to_string())? = counter;
    Ok(())
}

#[tauri::command]
async fn app_totp_disable(state: State<'_, AppState>, current: String) -> CmdResult<()> {
    let core = open_app(&state)?;
    let layout = core.layout.clone();
    let ok = tauri::async_runtime::spawn_blocking(move || {
        veranda_core::applock::verify(&layout, &current)
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    if !ok {
        return Err("wrong".into());
    }
    veranda_core::applock::set_totp(&core.layout, None).map_err(err)
}

#[tauri::command]
fn app_lock(state: State<'_, AppState>) -> CmdResult<()> {
    let core = peek_app(&state)?;
    if !veranda_core::applock::is_enabled(&core.layout) {
        return Err("the app lock is not enabled".into());
    }
    state
        .locked
        .store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// Enable, change or disable the lock. `current` must verify when a lock exists;
/// `new` = None disables it. Only possible while unlocked.
#[tauri::command]
async fn app_lock_set(
    state: State<'_, AppState>,
    current: Option<String>,
    new: Option<String>,
    auto_lock_minutes: u32,
    kind: Option<String>,
) -> CmdResult<bool> {
    let core = open_app(&state)?;
    let layout = core.layout.clone();
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<bool> {
        use veranda_core::applock;
        if applock::is_enabled(&layout)
            && !applock::verify(&layout, current.as_deref().unwrap_or(""))?
        {
            anyhow::bail!("wrong");
        }
        match new.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(secret) => applock::set(
                &layout,
                secret,
                auto_lock_minutes,
                kind.as_deref().unwrap_or("pin"),
            )?,
            None if applock::is_enabled(&layout) && current.is_some() => applock::clear(&layout)?,
            None => return Ok(false),
        }
        Ok(applock::is_enabled(&layout))
    })
    .await
    .map_err(err)?
    .map_err(err)
}

#[tauri::command]
fn app_lock_timer(state: State<'_, AppState>, minutes: u32) -> CmdResult<()> {
    let core = open_app(&state)?;
    veranda_core::applock::set_auto_lock(&core.layout, minutes).map_err(err)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    format!("{e:#}")
}

fn open_app(state: &State<'_, AppState>) -> CmdResult<Veranda> {
    if is_locked(state) {
        return Err("locked".into());
    }
    Ok(state
        .app
        .lock()
        .map_err(|_| "state poisoned".to_string())?
        .clone())
}

#[derive(Serialize)]
struct UiState {
    version: String,
    platform: String,
    root: String,
    config: Config,
    services: Vec<ServiceStatus>,
    sites: Vec<Site>,
    installed: Vec<Installed>,
    /// Runtimes managed by other tools (nvm, fnm, volta): shown, never touched.
    external: Vec<veranda_core::tools::ExternalRuntime>,
    /// nvm is installed: Node versions are installed through it instead of into `bin/node`.
    nvm_available: bool,
    hosts_in_sync: bool,
    db_password_known: bool,
    scheme: String,
    base_url: String,
    templates: Vec<(String, String)>,
    /// Optional UI hints for automated screenshots: VERANDA_UI_TAB / VERANDA_UI_THEME.
    ui_tab: Option<String>,
    ui_theme: Option<String>,
    /// VERANDA_UI_PERF=1 makes the UI run a scroll benchmark and report it via `perf_report`.
    ui_perf: bool,
    /// VERANDA_UI_AUTOTEST=<name> runs a scripted UI check (dev only), reported via `perf_report`.
    ui_autotest: Option<String>,
}

#[tauri::command]
fn get_state(state: State<'_, AppState>) -> CmdResult<UiState> {
    if is_locked(&state) {
        return Err("locked".into());
    }
    let app = open_app(&state)?;
    let services = services::status_all(&app).map_err(err)?;
    let sites = veranda_core::sites::discover(&app.layout, &app.config).map_err(err)?;
    let reg = Registry::load(&app.layout).map_err(err)?;
    let hosts_in_sync = veranda_core::hosts::is_in_sync(
        &Platform::current().hosts_file(),
        &veranda_core::sites::hostnames(&sites),
    );
    let db_password_known = veranda_core::db::root_password(&app.layout)
        .ok()
        .flatten()
        .is_some();
    let scheme = if app.config.web.ssl { "https" } else { "http" };
    let port = if app.config.web.ssl {
        app.config.web.https_port
    } else {
        app.config.web.http_port
    };
    let default = (scheme == "https" && port == 443) || (scheme == "http" && port == 80);
    let base_url = if default {
        format!("{scheme}://localhost")
    } else {
        format!("{scheme}://localhost:{port}")
    };
    Ok(UiState {
        version: veranda_core::VERSION.into(),
        platform: Platform::current().to_string(),
        root: app.layout.root().display().to_string(),
        config: app.config.clone(),
        services,
        sites,
        installed: reg.items,
        external: veranda_core::tools::external_node_versions(),
        nvm_available: veranda_core::tools::nvm_dir().is_some(),
        hosts_in_sync,
        db_password_known,
        scheme: scheme.into(),
        base_url,
        templates: veranda_core::projects::TEMPLATES
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
        ui_tab: std::env::var("VERANDA_UI_TAB").ok(),
        ui_theme: std::env::var("VERANDA_UI_THEME").ok(),
        ui_perf: std::env::var("VERANDA_UI_PERF").is_ok(),
        ui_autotest: std::env::var("VERANDA_UI_AUTOTEST").ok(),
    })
}

#[tauri::command]
async fn service_action(
    handle: AppHandle,
    state: State<'_, AppState>,
    id: String,
    action: String,
) -> CmdResult<Vec<ActionReport>> {
    let app = open_app(&state)?;
    let sid: ServiceId = id.parse().map_err(err)?;
    let r = tauri::async_runtime::spawn_blocking(move || {
        let r = match action.as_str() {
            "start" => services::start(&app, sid),
            "stop" => services::stop(&app, sid),
            "restart" => services::restart(&app, sid),
            "reload" => services::reload(&app, sid),
            other => Err(anyhow::anyhow!("unknown action {other}")),
        };
        to_report(vec![(sid, r)])
    })
    .await
    .map_err(err);
    refresh_tray(&handle);
    r
}

/// Linux only: grant the active web server binary permission to bind ports below 1024
/// (`setcap cap_net_bind_service`, asks for the user's password). No-op elsewhere.
#[tauri::command]
async fn grant_web_ports(state: State<'_, AppState>) -> CmdResult<()> {
    let app = open_app(&state)?;
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<()> {
        let reg = Registry::load(&app.layout)?;
        let sites = veranda_core::sites::discover(&app.layout, &app.config)?;
        let ctx = services::Ctx {
            app: &app,
            reg: &reg,
            sites: &sites,
        };
        let server = services::web::active(&ctx)
            .ok_or_else(|| anyhow::anyhow!("install a web server first"))?;
        veranda_core::tools::grant_bind_capability(&server.binary(&ctx))
    })
    .await
    .map_err(err)?
    .map_err(err)
}

#[derive(Serialize, Clone)]
struct ActionReport {
    id: String,
    ok: bool,
    error: String,
    /// Set when the OS refused a privileged port; the UI offers the one-time permission fix.
    port_permission: Option<u16>,
    /// Set when another program holds the port; the UI offers to move to `suggestion`.
    port_in_use: Option<PortConflict>,
}

#[derive(Serialize, Clone)]
struct PortConflict {
    port: u16,
    owner: String,
    suggestion: u16,
}

fn suggest_port(port: u16) -> u16 {
    let from = match port {
        80 => 8080,
        443 => 8443,
        p if p < 1024 => p + 8000,
        p => p + 1,
    };
    veranda_core::process::next_free_port(from).unwrap_or(from)
}

fn to_report(results: Vec<(ServiceId, anyhow::Result<()>)>) -> Vec<ActionReport> {
    results
        .into_iter()
        .map(|(id, r)| {
            let port_permission = r.as_ref().err().and_then(|e| {
                e.chain()
                    .find_map(|c| c.downcast_ref::<services::PortPermission>())
                    .map(|p| p.port)
            });
            let port_in_use = r.as_ref().err().and_then(|e| {
                e.chain()
                    .find_map(|c| c.downcast_ref::<services::PortInUse>())
                    .map(|p| PortConflict {
                        port: p.port,
                        owner: p.owner.clone(),
                        suggestion: suggest_port(p.port),
                    })
            });
            ActionReport {
                id: id.to_string(),
                ok: r.is_ok(),
                error: r.err().map(|e| format!("{e:#}")).unwrap_or_default(),
                port_permission,
                port_in_use,
            }
        })
        .collect()
}

/// Developer aid: the UI's scroll benchmark result (VERANDA_UI_PERF=1) goes to the log.
#[tauri::command]
fn perf_report(text: String) {
    tracing::info!("ui-perf {text}");
}

/// The UI calls this every few seconds. If it stops while the window is shown, the webview's
/// render process has died (WebKitGTK shows a blank page then); the page is reloaded.
#[tauri::command]
fn ui_heartbeat(state: State<'_, AppState>) {
    state
        .heartbeat
        .store(now_secs(), std::sync::atomic::Ordering::Relaxed);
}

/// Called by the UI after its first render; shows the (initially hidden) main window.
#[tauri::command]
fn ui_ready(handle: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    state
        .heartbeat
        .store(now_secs(), std::sync::atomic::Ordering::Relaxed);
    let core = peek_app(&state)?;
    if core.config.general.start_minimized {
        return Ok(());
    }
    show_main(&handle);
    // Dev/screenshot hint: VERANDA_UI_SIZE=WxH resizes the window after the first render.
    if let Ok(size) = std::env::var("VERANDA_UI_SIZE")
        && let Some((w, h)) = size.split_once('x')
        && let (Ok(w), Ok(h)) = (w.parse::<f64>(), h.parse::<f64>())
        && let Some(win) = handle.get_webview_window("main")
    {
        let _ = win.set_size(tauri::LogicalSize::new(w, h));
    }
    Ok(())
}

#[tauri::command]
async fn start_all(handle: AppHandle, state: State<'_, AppState>) -> CmdResult<Vec<ActionReport>> {
    let r = {
        let app = open_app(&state)?;
        tauri::async_runtime::spawn_blocking(move || {
            let _ = services::prepare_all(&app);
            to_report(services::start_all(&app))
        })
        .await
        .map_err(err)
    };
    refresh_tray(&handle);
    r
}

#[tauri::command]
async fn stop_all(handle: AppHandle, state: State<'_, AppState>) -> CmdResult<Vec<ActionReport>> {
    let r = {
        let app = open_app(&state)?;
        tauri::async_runtime::spawn_blocking(move || to_report(services::stop_all(&app)))
            .await
            .map_err(err)
    };
    refresh_tray(&handle);
    r
}

#[derive(Clone, Serialize)]
struct InstallProgress {
    kind: String,
    downloaded: u64,
    total: Option<u64>,
    /// Free text shown instead of byte counts (e.g. nvm output).
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[tauri::command]
async fn install_package(
    handle: AppHandle,
    state: State<'_, AppState>,
    kind: String,
    version: Option<String>,
    latest: bool,
) -> CmdResult<Installed> {
    let app = open_app(&state)?;
    let kind: Kind = kind.parse().map_err(err)?;
    let p = Platform::current();
    let builtin = Manifest::builtin().map_err(err)?;
    let version = version.filter(|v| !v.trim().is_empty());
    // Node with nvm present: let nvm own the version so `nvm ls` / `nvm use` see it.
    if kind == Kind::Node && veranda_core::tools::nvm_dir().is_some() {
        let v = match &version {
            Some(v) => v.clone(),
            None => {
                veranda_core::resolve::latest(kind, p)
                    .await
                    .map_err(err)?
                    .version
            }
        };
        let h = handle.clone();
        let vv = v.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let on_line = |line: String| {
                let _ = h.emit(
                    "install-progress",
                    InstallProgress {
                        kind: "node".into(),
                        downloaded: 0,
                        total: None,
                        note: Some(line),
                    },
                );
            };
            veranda_core::tools::nvm_install(&vv, &on_line)
        })
        .await
        .map_err(err)?
        .map_err(err)?;
        let found = veranda_core::tools::external_node_versions()
            .into_iter()
            .find(|x| x.version == v);
        return Ok(Installed {
            kind: Kind::Node,
            version: v,
            path: found.map(|x| x.bin_dir).unwrap_or_default(),
            source: "nvm".into(),
            installed_at: veranda_core::fsutil::timestamp(),
            digests: Vec::new(),
        });
    }
    let pkg = match version {
        // Explicit version: built-in list first, then the upstream resolver (newer than built-in).
        Some(v) => match builtin
            .versions(kind, p)
            .into_iter()
            .find(|x| x.version == v)
            .cloned()
        {
            Some(pk) => pk,
            None => veranda_core::resolve::versions(kind, p)
                .await
                .map_err(err)?
                .into_iter()
                .find(|x| x.version == v)
                .ok_or_else(|| {
                    format!(
                        "{} {v} is not available (built-in or upstream)",
                        kind.display()
                    )
                })?,
        },
        None if latest => veranda_core::resolve::latest(kind, p).await.map_err(err)?,
        None => builtin
            .latest(kind, p)
            .cloned()
            .ok_or_else(|| format!("no {kind} package for {p}"))?,
    };
    let kind_id = kind.id().to_string();
    let h = handle.clone();
    let progress = move |pr: veranda_core::download::Progress| {
        let _ = h.emit(
            "install-progress",
            InstallProgress {
                kind: kind_id.clone(),
                downloaded: pr.downloaded,
                total: pr.total,
                note: None,
            },
        );
    };
    let installed = veranda_core::packages::install(&app.layout, &pkg, Some(&progress))
        .await
        .map_err(err)?;
    reload_running(&handle, &state, &installed);
    Ok(installed)
}

/// A service that is running the version just (re)installed still executes the old binary
/// (for example PHP-FPM after a bundle change): restart it so the new build is what serves.
fn reload_running(handle: &AppHandle, state: &State<'_, AppState>, installed: &Installed) {
    let sid = match installed.kind {
        Kind::Php => ServiceId::Php,
        Kind::Nginx | Kind::Apache => ServiceId::Web,
        Kind::MariaDb | Kind::MySql => ServiceId::Db,
        Kind::Postgres => ServiceId::Pg,
        Kind::MongoDb => ServiceId::Mongo,
        _ => return,
    };
    let Ok(core) = open_app(state) else { return };
    let Ok(statuses) = services::status_all(&core) else {
        return;
    };
    let Some(st) = statuses
        .into_iter()
        .find(|s| s.id == sid && s.running && s.version == installed.version)
    else {
        return;
    };
    let h = handle.clone();
    let name = st.name.clone();
    let version = installed.version.clone();
    std::thread::spawn(move || {
        match services::restart(&core, sid) {
            Ok(()) => {
                tracing::info!("{name}: restarted to load the reinstalled {version}");
                let _ = h.emit(
                    "service-reloaded",
                    serde_json::json!({ "name": name, "version": version }),
                );
            }
            Err(e) => tracing::warn!("{name}: restart after reinstall failed: {e:#}"),
        }
        let _ = h.emit("services-changed", ());
        refresh_tray(&h);
    });
}

#[tauri::command]
async fn upstream_versions() -> CmdResult<Vec<veranda_core::manifest::Package>> {
    Ok(veranda_core::resolve::versions_all(Platform::current())
        .await
        .packages)
}

#[tauri::command]
fn builtin_versions() -> CmdResult<Vec<veranda_core::manifest::Package>> {
    let m = Manifest::builtin().map_err(err)?;
    let p = Platform::current();
    Ok(m.for_platform(p).cloned().collect())
}

#[tauri::command]
fn remove_package(state: State<'_, AppState>, kind: String, version: String) -> CmdResult<()> {
    let app = open_app(&state)?;
    let kind: Kind = kind.parse().map_err(err)?;
    // Refuse to delete a version that a running service is using right now.
    let statuses = services::status_all(&app).map_err(err)?;
    let uses_kind = |id: ServiceId| -> bool {
        use veranda_core::config::{DbEngine, WebServer};
        match id {
            ServiceId::Web => matches!(
                (app.config.web.server, kind),
                (WebServer::Nginx, Kind::Nginx) | (WebServer::Apache, Kind::Apache)
            ),
            ServiceId::Php => kind == Kind::Php,
            ServiceId::Db => matches!(
                (app.config.db.engine, kind),
                (DbEngine::MariaDb, Kind::MariaDb) | (DbEngine::MySql, Kind::MySql)
            ),
            ServiceId::Pg => kind == Kind::Postgres,
            ServiceId::Mongo => kind == Kind::MongoDb,
        }
    };
    if let Some(st) = statuses
        .iter()
        .find(|st| st.running && st.version == version && uses_kind(st.id))
    {
        return Err(format!(
            "{} {} is running. Stop it first.",
            st.name, version
        ));
    }
    veranda_core::packages::remove(&app.layout, kind, &version).map_err(err)
}

#[tauri::command]
fn save_config(handle: AppHandle, state: State<'_, AppState>, config: Config) -> CmdResult<()> {
    let (core, auto_on) = {
        let mut guard = state.app.lock().map_err(|_| "state poisoned".to_string())?;
        config.validate().map_err(err)?;
        let auto_on = config.update.auto_install && !guard.config.update.auto_install;
        guard.config = config;
        guard.save_config().map_err(err)?;
        (guard.clone(), auto_on)
    };
    if auto_on {
        // "Install updates automatically" was just switched on: do a round right away.
        let h = handle.clone();
        tauri::async_runtime::spawn(async move { auto_update(h).await });
    }
    // Ports may have moved: services still bound to the old ones are restarted in the background.
    std::thread::spawn(move || heal_stale(&handle, &core));
    Ok(())
}

/// Restart services running on outdated port settings and tell the UI about it.
fn heal_stale(handle: &AppHandle, core: &Veranda) {
    let healed = services::heal_stale_ports(core);
    if healed.is_empty() {
        return;
    }
    for (id, r) in &healed {
        match r {
            Ok(()) => tracing::info!("{id}: restarted to apply the new port settings"),
            Err(e) => tracing::warn!("{id}: restart for new ports failed: {e:#}"),
        }
    }
    let _ = handle.emit("services-changed", ());
    let _ = handle.emit("autostart-done", to_report(healed));
    refresh_tray(handle);
}

#[derive(Serialize)]
struct SyncResult {
    missing_hosts: Vec<String>,
}

/// Abort the download that is running; the install command then returns "download cancelled".
#[tauri::command]
fn cancel_install() {
    veranda_core::download::request_cancel();
}

/// Remove a Node version that nvm manages (the only external manager with a remove command).
/// Choose the Node.js that goes first on the PATH: `""` (newest Veranda version), a Veranda
/// version, or `nvm:<version>` (also made nvm's own default).
#[tauri::command]
async fn node_set_default(state: State<'_, AppState>, selector: String) -> CmdResult<String> {
    let app = open_app(&state)?;
    let sel = selector.trim().to_string();
    let label = if let Some((source, version)) = sel.split_once(':') {
        let known = veranda_core::tools::external_node_versions()
            .into_iter()
            .any(|x| x.kind == "node" && x.source == source && x.version == version);
        if !known {
            return Err(format!("Node {version} from {source} was not found"));
        }
        if source == "nvm" {
            let v = version.to_string();
            tauri::async_runtime::spawn_blocking(move || veranda_core::tools::nvm_set_default(&v))
                .await
                .map_err(err)?
                .map_err(err)?;
        }
        format!("{version} ({source})")
    } else if sel.is_empty() {
        "newest".into()
    } else {
        let reg = Registry::load(&app.layout).map_err(err)?;
        if reg.find(Kind::Node, &sel).is_none() {
            return Err(format!("Node {sel} is not installed"));
        }
        sel.clone()
    };
    let mut guard = state.app.lock().map_err(|_| "state poisoned".to_string())?;
    guard.config.node.default = sel;
    guard.save_config().map_err(err)?;
    Ok(label)
}

#[tauri::command]
async fn remove_external(kind: String, version: String, source: String) -> CmdResult<()> {
    if kind != "node" || source != "nvm" {
        return Err(format!(
            "{source} versions are managed outside Veranda; remove it there"
        ));
    }
    tauri::async_runtime::spawn_blocking(move || veranda_core::tools::nvm_uninstall(&version))
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
fn pg_password(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    let app = open_app(&state)?;
    veranda_core::services::pg::password(&app.layout).map_err(err)
}

#[tauri::command]
fn migrate_detect(state: State<'_, AppState>) -> CmdResult<Vec<veranda_core::migrate::Source>> {
    let app = open_app(&state)?;
    let mut v = veranda_core::migrate::detect_veranda(&app);
    v.extend(veranda_core::migrate::detect());
    Ok(v)
}

/// Import projects (and optionally databases) from another stack. Progress lines are emitted
/// as `migrate-progress`; the final report lists what was imported, skipped or failed.
/// Starts the import in the background and returns at once; `migrate_status` (and the
/// `migrate-progress` / `migrate-done` events) carry the rest, so the dialog can be closed.
#[tauri::command]
fn migrate_run(
    handle: AppHandle,
    state: State<'_, AppState>,
    req: veranda_core::migrate::Request,
) -> CmdResult<()> {
    let app = open_app(&state)?;
    {
        let mut job = state
            .migrate
            .lock()
            .map_err(|_| "state poisoned".to_string())?;
        if job.running {
            return Err("a migration is already running".into());
        }
        *job = MigrateJob {
            running: true,
            progress: veranda_core::migrate::Progress::msg("Starting…"),
            started_at: now_secs(),
            ..Default::default()
        };
    }
    let control = state
        .migrate
        .lock()
        .map_err(|_| "state poisoned".to_string())?
        .control
        .clone();
    let h = handle.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let progress = |p: veranda_core::migrate::Progress| {
            if let Ok(mut job) = h.state::<AppState>().migrate.lock() {
                job.progress = p.clone();
            }
            let _ = h.emit("migrate-progress", p);
        };
        let result = veranda_core::migrate::run(&app, &req, &control, &progress);
        // The report also goes to logs/migrate.log, so a result nobody watched (dialog closed,
        // toast gone) can still be read later from the Logs page.
        {
            use std::io::Write;
            let line = match &result {
                Ok(r) => format!(
                    "[{}] source={} sites={} databases={} skipped={} errors={}\n{}{}",
                    now_secs(),
                    req.platform,
                    r.sites.len(),
                    r.databases.len(),
                    r.skipped.len(),
                    r.errors.len(),
                    r.databases
                        .iter()
                        .map(|d| format!("  ok      {d}\n"))
                        .collect::<String>(),
                    r.skipped
                        .iter()
                        .map(|d| format!("  skipped {d}\n"))
                        .chain(r.errors.iter().map(|e| format!("  ERROR   {e}\n")))
                        .collect::<String>()
                ),
                Err(e) => format!("[{}] source={} FAILED: {e:#}\n", now_secs(), req.platform),
            };
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(app.layout.logs().join("migrate.log"))
            {
                let _ = f.write_all(line.as_bytes());
            }
        }
        let snapshot = {
            let st = h.state::<AppState>();
            let mut job = st.migrate.lock().unwrap_or_else(|e| e.into_inner());
            job.running = false;
            job.finished_at = now_secs();
            match result {
                Ok(r) => job.report = Some(r),
                Err(e) => job.error = Some(format!("{e:#}")),
            }
            job.clone()
        };
        let _ = h.emit("migrate-done", snapshot);
    });
    Ok(())
}

/// `pause`, `resume`, `cancel` (after the current database) or `stop` (now).
#[tauri::command]
fn migrate_control(state: State<'_, AppState>, action: String) -> CmdResult<MigrateJob> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut job = state
        .migrate
        .lock()
        .map_err(|_| "state poisoned".to_string())?;
    if !job.running {
        return Err("no migration is running".into());
    }
    match action.as_str() {
        "pause" => job.control.paused.store(true, Relaxed),
        "resume" => job.control.paused.store(false, Relaxed),
        "cancel" => job
            .control
            .cancel
            .store(job.control.cancel_level().max(1), Relaxed),
        "stop" => {
            job.control.cancel.store(2, Relaxed);
            job.control.paused.store(false, Relaxed);
        }
        other => return Err(format!("unknown action {other}")),
    }
    job.paused = job.control.is_paused();
    job.cancel = job.control.cancel_level();
    Ok(job.clone())
}

#[tauri::command]
fn migrate_status(state: State<'_, AppState>) -> CmdResult<MigrateJob> {
    Ok(state
        .migrate
        .lock()
        .map_err(|_| "state poisoned".to_string())?
        .clone())
}

#[tauri::command]
async fn sync_sites(state: State<'_, AppState>) -> CmdResult<SyncResult> {
    let app = open_app(&state)?;
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<SyncResult> {
        let missing = services::prepare_all(&app)?;
        if services::status_all(&app)?
            .iter()
            .any(|s| s.id == ServiceId::Web && s.running)
        {
            services::reload(&app, ServiceId::Web)?;
        }
        Ok(SyncResult {
            missing_hosts: missing,
        })
    })
    .await
    .map_err(err)?
    .map_err(err)
}

/// Write the hosts file through an elevated copy of this executable.
#[tauri::command]
async fn hosts_sync(state: State<'_, AppState>) -> CmdResult<()> {
    let app = open_app(&state)?;
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<()> {
        let sites = veranda_core::sites::discover(&app.layout, &app.config)?;
        let hosts = veranda_core::sites::hostnames(&sites);
        let hosts_file = Platform::current().hosts_file();
        if veranda_core::hosts::is_in_sync(&hosts_file, &hosts) {
            return Ok(());
        }
        let payload = app.layout.tmp().join("hosts.payload");
        veranda_core::hosts::write_payload(&payload, &hosts, &app.config.general.tld)?;
        let exe = veranda_core::tools::current_exe()?;
        veranda_core::tools::run_elevated(
            &exe,
            &[
                "--hosts-apply".into(),
                payload.to_string_lossy().into_owned(),
            ],
        )?;
        if !veranda_core::hosts::is_in_sync(&hosts_file, &hosts) {
            anyhow::bail!("hosts file was not updated");
        }
        Ok(())
    })
    .await
    .map_err(err)?
    .map_err(err)
}

#[tauri::command]
fn add_site(
    state: State<'_, AppState>,
    hostname: String,
    path: String,
    public: String,
) -> CmdResult<()> {
    let app = open_app(&state)?;
    veranda_core::sites::add_manual(
        &app.layout,
        &app.config,
        &hostname,
        std::path::Path::new(&path),
        &public,
    )
    .map_err(err)
}

#[tauri::command]
fn remove_site(state: State<'_, AppState>, hostname: String) -> CmdResult<bool> {
    let app = open_app(&state)?;
    veranda_core::sites::remove_manual(&app.layout, &hostname).map_err(err)
}

#[derive(Deserialize)]
struct NewProject {
    name: String,
    template: String,
    arg: String,
    create_db: bool,
}

#[tauri::command]
async fn create_project(state: State<'_, AppState>, req: NewProject) -> CmdResult<String> {
    let app = open_app(&state)?;
    use veranda_core::projects::Template;
    let t = match req.template.as_str() {
        "blank" => Template::Blank,
        "wordpress" => Template::WordPress,
        "laravel" => Template::Laravel,
        "symfony" => Template::Symfony,
        "composer" => Template::Composer {
            package: req.arg.clone(),
        },
        "git" => Template::Git {
            url: req.arg.clone(),
        },
        other => return Err(format!("unknown template {other}")),
    };
    let path = veranda_core::projects::create(&app, &req.name, &t, None)
        .await
        .map_err(err)?;
    if req.create_db {
        let reg = Registry::load(&app.layout).map_err(err)?;
        if let Some(db) = veranda_core::db::DbCtx::from_app(&app, &reg) {
            let _ = veranda_core::db::create_database(&db, &req.name.replace('-', "_"));
        }
    }
    Ok(path.display().to_string())
}

#[tauri::command]
fn delete_project(state: State<'_, AppState>, name: String) -> CmdResult<()> {
    let app = open_app(&state)?;
    veranda_core::projects::delete(&app, &name).map_err(err)
}

fn db_ctx(app: &Veranda) -> CmdResult<veranda_core::db::DbCtx> {
    let reg = Registry::load(&app.layout).map_err(err)?;
    veranda_core::db::DbCtx::from_app(app, &reg)
        .ok_or_else(|| "database is not installed".to_string())
}

#[tauri::command]
async fn db_create(state: State<'_, AppState>, name: String) -> CmdResult<()> {
    let app = open_app(&state)?;
    let db = db_ctx(&app)?;
    tauri::async_runtime::spawn_blocking(move || veranda_core::db::create_database(&db, &name))
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
async fn db_list(state: State<'_, AppState>) -> CmdResult<Vec<String>> {
    let app = open_app(&state)?;
    let db = db_ctx(&app)?;
    tauri::async_runtime::spawn_blocking(move || veranda_core::db::list_databases(&db))
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
fn db_password(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    let app = open_app(&state)?;
    veranda_core::db::root_password(&app.layout).map_err(err)
}

#[tauri::command]
async fn db_set_password(state: State<'_, AppState>, new: Option<String>) -> CmdResult<String> {
    let app = open_app(&state)?;
    let db = db_ctx(&app)?;
    let current = veranda_core::db::root_password(&app.layout).map_err(err)?;
    // `null` asks for a fresh random password; an empty string removes the password.
    let new = match new {
        Some(s) => s.trim().to_string(),
        None => veranda_core::db::random_token(24),
    };
    let n2 = new.clone();
    tauri::async_runtime::spawn_blocking(move || {
        veranda_core::db::set_root_password(&db, current.as_deref(), &n2)
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    Ok(new)
}

/// Disk use of each part of the root (packages, data, backups, logs, www, tmp).
#[tauri::command]
async fn storage_usage(
    state: State<'_, AppState>,
) -> CmdResult<Vec<veranda_core::slim::UsageEntry>> {
    let app = open_app(&state)?;
    tauri::async_runtime::spawn_blocking(move || veranda_core::slim::usage(&app.layout))
        .await
        .map_err(err)
}

/// One-click cleanup: prune installed packages, gzip old backups, delete leftovers, trim logs.
#[tauri::command]
async fn free_space(
    handle: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<veranda_core::slim::CleanReport> {
    let app = open_app(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let progress = |m: String| {
            let _ = handle.emit("clean-progress", m);
        };
        veranda_core::slim::free_space(&app, &progress)
    })
    .await
    .map_err(err)?
    .map_err(err)
}

#[tauri::command]
async fn db_backup(state: State<'_, AppState>) -> CmdResult<String> {
    let app = open_app(&state)?;
    tauri::async_runtime::spawn_blocking(move || veranda_core::backup::run(&app))
        .await
        .map_err(err)?
        .map(|p| p.display().to_string())
        .map_err(err)
}

#[derive(Serialize)]
struct TrustReport {
    description: String,
    ok: bool,
    error: String,
}

#[tauri::command]
async fn ssl_trust(state: State<'_, AppState>) -> CmdResult<Vec<TrustReport>> {
    let app = open_app(&state)?;
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<Vec<TrustReport>> {
        veranda_core::tls::ensure_ca(&app.layout)?;
        let mut out = Vec::new();
        for step in veranda_core::tls::trust_commands(&app.layout) {
            let r = if step.needs_admin {
                veranda_core::tools::run_elevated(std::path::Path::new(&step.program), &step.args)
            } else {
                veranda_core::process::new_cmd(&step.program)
                    .args(&step.args)
                    .stdin(std::process::Stdio::null())
                    .output()
                    .map_err(anyhow::Error::from)
                    .and_then(|o| {
                        if o.status.success() {
                            Ok(())
                        } else {
                            let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                            let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
                            anyhow::bail!("{} ({})", if last.is_empty() { format!("exit {}", o.status) } else { last }, o.status)
                        }
                    })
            };
            let error = match r {
                Ok(()) => String::new(),
                Err(e) if step.program == "certutil"
                    && cfg!(target_os = "linux")
                    && which::which("certutil").is_err() =>
                {
                    format!(
                        "certutil is not installed (sudo apt install libnss3-tools, or dnf install nss-tools), then try again; or import {} by hand in the browser's certificate settings ({e:#})",
                        app.layout.ssl().join("ca.crt").display()
                    )
                }
                Err(e) => format!("{e:#}"),
            };
            out.push(TrustReport {
                description: step.description,
                ok: error.is_empty(),
                error,
            });
        }
        Ok(out)
    })
    .await
    .map_err(err)?
    .map_err(err)
}

#[tauri::command]
async fn group_action(
    handle: AppHandle,
    state: State<'_, AppState>,
    name: String,
    action: String,
) -> CmdResult<Vec<ActionReport>> {
    let app = open_app(&state)?;
    let r = tauri::async_runtime::spawn_blocking(move || {
        let res = match action.as_str() {
            "start" => services::start_group(&app, &name),
            "stop" => services::stop_group(&app, &name),
            other => Err(anyhow::anyhow!("unknown action {other}")),
        };
        res.map(to_report)
    })
    .await
    .map_err(err)?
    .map_err(err);
    refresh_tray(&handle);
    r
}

#[tauri::command]
fn open_target(state: State<'_, AppState>, what: String) -> CmdResult<()> {
    let app = open_app(&state)?;
    let cfg = &app.config;
    let st = get_state(state.clone())?;
    // Local tools open over plain http: it is this computer, and https would need the local CA to
    // be trusted first. Sites keep the scheme from Settings (https when enabled).
    let base = if cfg.web.http_port == 80 {
        "http://localhost".to_string()
    } else {
        format!("http://localhost:{}", cfg.web.http_port)
    };
    match what.as_str() {
        "web" => veranda_core::tools::open_url(cfg, &format!("{base}/")),
        "adminer" => veranda_core::tools::open_url(cfg, &format!("{base}/adminer/")),
        "phpmyadmin" => veranda_core::tools::open_url(cfg, &format!("{base}/phpmyadmin/")),
        "adminer-pg" => veranda_core::tools::open_url(
            cfg,
            &format!(
                "{base}/adminer/?pgsql=127.0.0.1%3A{}&username=postgres",
                cfg.pg.port
            ),
        ),
        "root" => veranda_core::tools::open_path(app.layout.root()),
        "www" => veranda_core::tools::open_path(&cfg.document_root(&app.layout)),
        "logs" => veranda_core::tools::open_path(&app.layout.logs()),
        "config" => veranda_core::tools::open_editor(cfg, &app.layout.config_file()),
        "phpini" => veranda_core::tools::open_editor(
            cfg,
            &veranda_core::services::php::ini_path(&app.layout),
        ),
        "terminal" => {
            let reg = Registry::load(&app.layout).map_err(err)?;
            let path = veranda_core::packages::runtime_path(&app.layout, &reg, cfg);
            veranda_core::tools::open_terminal(&cfg.document_root(&app.layout), &path)
        }
        s if s.starts_with("site:") => veranda_core::tools::open_url(
            cfg,
            &format!(
                "{}://{}{}/",
                st.scheme,
                &s[5..],
                st.base_url
                    .rsplit_once(':')
                    .filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit()))
                    .map(|(_, p)| format!(":{p}"))
                    .unwrap_or_default()
            ),
        ),
        s if s.starts_with("path:") => {
            veranda_core::tools::open_path(std::path::Path::new(&s[5..]))
        }
        s if s.starts_with("edit:") => {
            veranda_core::tools::open_editor(cfg, std::path::Path::new(&s[5..]))
        }
        other => Err(anyhow::anyhow!("unknown target {other}")),
    }
    .map_err(err)
}

#[tauri::command]
fn open_url_external(state: State<'_, AppState>, url: String) -> CmdResult<()> {
    let app = open_app(&state)?;
    if !url.starts_with("https://github.com/") {
        return Err("only GitHub release pages are opened from here".into());
    }
    veranda_core::tools::open_url(&app.config, &url).map_err(err)
}

#[tauri::command]
async fn check_update(
    state: State<'_, AppState>,
) -> CmdResult<Option<veranda_core::update::Release>> {
    let app = open_app(&state)?;
    veranda_core::update::check(app.config.update.repo_or_default())
        .await
        .map_err(err)
}

#[tauri::command]
fn change_root(state: State<'_, AppState>, root: String) -> CmdResult<()> {
    let app = Veranda::open_at(std::path::PathBuf::from(root)).map_err(err)?;
    app.layout.remember().map_err(err)?;
    let mut guard = state.app.lock().map_err(|_| "state poisoned".to_string())?;
    *guard = app;
    Ok(())
}

#[derive(Serialize)]
struct LogFile {
    name: String,
    size: u64,
    modified: String,
}

fn log_path(app: &Veranda, name: &str) -> CmdResult<std::path::PathBuf> {
    if name.is_empty()
        || name.contains(['/', '\\'])
        || name.starts_with('.')
        || !name.ends_with(".log")
    {
        return Err("invalid log name".into());
    }
    let p = app.layout.logs().join(name);
    if !p.is_file() {
        return Err(format!("{name} does not exist"));
    }
    Ok(p)
}

#[tauri::command]
fn list_logs(state: State<'_, AppState>) -> CmdResult<Vec<LogFile>> {
    let app = open_app(&state)?;
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(app.layout.logs()) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".log") {
                continue;
            }
            let meta = match e.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs().to_string())
                .unwrap_or_default();
            v.push(LogFile {
                name,
                size: meta.len(),
                modified,
            });
        }
    }
    v.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(v)
}

/// Last `lines` lines of a log file (capped at 256 KiB read from the end).
#[tauri::command]
fn read_log(state: State<'_, AppState>, name: String, lines: usize) -> CmdResult<String> {
    use std::io::{Read, Seek, SeekFrom};
    let app = open_app(&state)?;
    let path = log_path(&app, &name)?;
    let mut f = std::fs::File::open(&path).map_err(err)?;
    let len = f.metadata().map_err(err)?.len();
    let cap: u64 = 256 * 1024;
    let start = len.saturating_sub(cap);
    f.seek(SeekFrom::Start(start)).map_err(err)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(err)?;
    let text = String::from_utf8_lossy(&buf);
    let all: Vec<&str> = text.lines().collect();
    let n = lines.clamp(10, 2000);
    let tail = if all.len() > n {
        &all[all.len() - n..]
    } else {
        &all[..]
    };
    Ok(tail.join("\n"))
}

#[tauri::command]
fn clear_log(state: State<'_, AppState>, name: String) -> CmdResult<()> {
    let app = open_app(&state)?;
    let path = log_path(&app, &name)?;
    std::fs::write(path, b"").map_err(err)
}

#[derive(Serialize, Clone)]
struct UpdateInfo {
    version: String,
    notes: String,
    date: String,
    /// true when the updater can install it in place (signed bundle available for this platform)
    installable: bool,
    /// release page, for the manual fallback
    url: String,
}

/// One update round: check, tell the UI, and install when "Install updates automatically" is on
/// and this install can update itself.
async fn auto_update(handle: AppHandle) {
    let state = handle.state::<AppState>();
    let Ok(core) = open_app(&state) else { return };
    match update_check(handle.clone(), state.clone()).await {
        Ok(Some(info)) => {
            let auto = core.config.update.auto_install && info.installable;
            let _ = handle.emit("update-available", info);
            if auto {
                let _ = handle.emit("update-auto", ());
                if let Err(e) = update_install(handle.clone(), state.clone()).await {
                    let _ = handle.emit("update-error", e);
                }
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!("update check failed: {e}"),
    }
}

fn updater_for(app: &AppHandle, core: &Veranda) -> CmdResult<tauri_plugin_updater::Updater> {
    let repo = core.config.update.repo_or_default();
    if !veranda_core::config::is_repo_slug(repo) {
        return Err("update.repo is not a valid owner/repo (Settings > GitHub repo)".into());
    }
    let url: tauri::Url = format!("https://github.com/{repo}/releases/latest/download/latest.json")
        .parse()
        .map_err(err)?;
    app.updater_builder()
        .endpoints(vec![url])
        .map_err(err)?
        .build()
        .map_err(err)
}

/// `deb` or `rpm` when this is a Linux package install (the Tauri updater only handles
/// AppImages there); `None` for AppImages and other platforms.
fn linux_package() -> Option<&'static str> {
    if !cfg!(target_os = "linux") || std::env::var_os("APPIMAGE").is_some() {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    if !(exe.starts_with("/usr") || exe.starts_with("/opt")) {
        return None;
    }
    if which::which("dpkg").is_ok() {
        Some("deb")
    } else if which::which("rpm").is_ok() {
        Some("rpm")
    } else {
        None
    }
}

/// Installer file name suffix for this machine's package format and CPU.
fn linux_package_suffix(kind: &str) -> String {
    let arch = std::env::consts::ARCH;
    match kind {
        "deb" => format!("_{}.deb", if arch == "aarch64" { "arm64" } else { "amd64" }),
        _ => format!(".{arch}.rpm"),
    }
}

fn updater_pubkey(handle: &AppHandle) -> Option<String> {
    handle
        .config()
        .plugins
        .0
        .get("updater")?
        .get("pubkey")?
        .as_str()
        .map(|s| s.to_string())
}

/// Download the .deb/.rpm of `release`, check its signature with the updater key, install it
/// through polkit (`pkexec`) and restart. Used where the Tauri updater cannot (package installs).
async fn linux_package_update(
    handle: &AppHandle,
    core: &Veranda,
    kind: &str,
    release: &veranda_core::update::Release,
) -> CmdResult<()> {
    let suffix = linux_package_suffix(kind);
    let asset = release
        .asset_ending_with(&suffix)
        .ok_or_else(|| format!("this release has no {suffix} file for your computer"))?;
    let sig = release
        .asset(&format!("{}.sig", asset.name))
        .ok_or("the installer has no signature file")?;
    let pubkey = updater_pubkey(handle).ok_or("updater public key missing from the app config")?;
    let client = veranda_core::download::client().map_err(err)?;
    let dir = core.layout.tmp().join("update");
    std::fs::create_dir_all(&dir).map_err(err)?;
    let file = dir.join(&asset.name);
    // signature first (tiny), then the installer with progress
    let sig_text = client
        .get(&sig.url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(err)?
        .text()
        .await
        .map_err(err)?;
    let mut resp = client
        .get(&asset.url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(err)?;
    let total = resp
        .content_length()
        .or(Some(asset.size).filter(|s| *s > 0));
    let mut out = tokio::fs::File::create(&file).await.map_err(err)?;
    let mut downloaded: u64 = 0;
    while let Some(chunk) = resp.chunk().await.map_err(err)? {
        tokio::io::AsyncWriteExt::write_all(&mut out, &chunk)
            .await
            .map_err(err)?;
        downloaded += chunk.len() as u64;
        let _ = handle.emit(
            "update-progress",
            UpdateProgress {
                downloaded,
                total,
                done: false,
            },
        );
    }
    tokio::io::AsyncWriteExt::flush(&mut out)
        .await
        .map_err(err)?;
    drop(out);
    let data = std::fs::read(&file).map_err(err)?;
    if let Err(e) = veranda_core::update::verify_signature(&pubkey, &sig_text, &data) {
        let _ = std::fs::remove_file(&file);
        return Err(format!("{e}; the download was discarded"));
    }
    let pkexec = which::which("pkexec").map_err(|_| {
        format!(
            "pkexec is not available; install by hand: sudo {} {}",
            if kind == "deb" { "dpkg -i" } else { "rpm -U" },
            file.display()
        )
    })?;
    let f = file.clone();
    let k = kind.to_string();
    let out = tauri::async_runtime::spawn_blocking(move || {
        let mut cmd = veranda_core::process::new_cmd(pkexec);
        if k == "deb" {
            cmd.arg("dpkg").arg("-i").arg(&f);
        } else {
            cmd.arg("rpm").arg("-U").arg("--replacepkgs").arg(&f);
        }
        cmd.output()
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if msg.is_empty() {
            format!(
                "installation was cancelled or failed; the file is at {}",
                file.display()
            )
        } else {
            format!("{msg}; the file is at {}", file.display())
        });
    }
    let _ = std::fs::remove_file(&file);
    Ok(())
}

/// Check for a newer Veranda. Tries the signed updater feed first, then the GitHub API: on a
/// Linux .deb/.rpm install the package is installable through polkit, elsewhere the fallback
/// is read-only.
#[tauri::command]
async fn update_check(
    handle: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Option<UpdateInfo>> {
    let core = open_app(&state)?;
    let repo = core.config.update.repo_or_default().to_string();
    let page = format!("https://github.com/{repo}/releases/latest");
    let why = match updater_for(&handle, &core) {
        Ok(updater) => match updater.check().await {
            Ok(found) => {
                return Ok(found.map(|u| UpdateInfo {
                    version: u.version.clone(),
                    notes: u.body.clone().unwrap_or_default(),
                    date: u.date.map(|d| d.to_string()).unwrap_or_default(),
                    installable: true,
                    url: page.clone(),
                }));
            }
            Err(e) => e.to_string(),
        },
        Err(e) => e,
    };
    tracing::info!("updater feed unusable ({why}); falling back to the GitHub API");
    match veranda_core::update::check(&repo).await.map_err(err)? {
        Some(r) => {
            let installable = linux_package().is_some_and(|k| {
                r.asset_ending_with(&linux_package_suffix(k))
                    .is_some_and(|a| r.asset(&format!("{}.sig", a.name)).is_some())
            });
            Ok(Some(UpdateInfo {
                version: r.version,
                notes: if installable {
                    r.notes.lines().next().unwrap_or("").to_string()
                } else {
                    format!("{} (updater: {why})", r.notes.lines().next().unwrap_or(""))
                },
                date: r.published_at,
                installable,
                url: r.url,
            }))
        }
        None => Ok(None),
    }
}

#[derive(Serialize, Clone)]
struct UpdateProgress {
    downloaded: u64,
    total: Option<u64>,
    done: bool,
}

/// Download, verify (signature), install and restart.
#[tauri::command]
async fn update_install(handle: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let core = open_app(&state)?;
    let updater = updater_for(&handle, &core)?;
    let feed = updater.check().await;
    if let (Err(_), Some(kind)) = (&feed, linux_package()) {
        // package install: the signed .deb/.rpm from the release, installed through polkit
        let repo = core.config.update.repo_or_default().to_string();
        let Some(release) = veranda_core::update::check(&repo).await.map_err(err)? else {
            return Err("already up to date".into());
        };
        linux_package_update(&handle, &core, kind, &release).await?;
        let _ = handle.emit(
            "update-progress",
            UpdateProgress {
                downloaded: 1,
                total: Some(1),
                done: true,
            },
        );
        let _ = services::stop_all(&core);
        teardown_webview(&handle);
        handle.restart();
    }
    let Some(update) = feed.map_err(err)? else {
        return Err("already up to date".into());
    };
    let h = handle.clone();
    let mut downloaded: u64 = 0;
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = h.emit(
                    "update-progress",
                    UpdateProgress {
                        downloaded,
                        total,
                        done: false,
                    },
                );
            },
            || {},
        )
        .await
        .map_err(err)?;
    let _ = handle.emit(
        "update-progress",
        UpdateProgress {
            downloaded,
            total: Some(downloaded),
            done: true,
        },
    );
    // Stop services before the binary is swapped and the app relaunches.
    let _ = services::stop_all(&core);
    teardown_webview(&handle);
    handle.restart();
}

/// Answer from the "hide or quit?" dialog. `remember` stores the choice in the config.
#[tauri::command]
fn close_decision(
    handle: AppHandle,
    state: State<'_, AppState>,
    action: String,
    remember: bool,
) -> CmdResult<()> {
    use veranda_core::config::CloseAction;
    let chosen = match action.as_str() {
        "tray" => CloseAction::Tray,
        "quit" => CloseAction::Quit,
        other => return Err(format!("unknown close action {other}")),
    };
    if remember {
        let mut guard = state.app.lock().map_err(|_| "state poisoned".to_string())?;
        guard.config.general.close_action = chosen;
        guard.save_config().map_err(err)?;
    }
    if let Some(w) = handle.get_webview_window("main") {
        let _ = w.hide();
    }
    if chosen == CloseAction::Quit {
        quit_app(handle);
    }
    Ok(())
}

/// Stop services on a worker thread (never blocks the UI), then exit. Hard deadline of 15 s.
fn quit_app(app: AppHandle) {
    let state = app.state::<AppState>();
    let core = open_app(&state).ok();
    std::thread::spawn(move || {
        remote::shutdown(&app);
        if let Some(core) = core {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = services::stop_all(&core);
                let _ = tx.send(());
            });
            let _ = rx.recv_timeout(std::time::Duration::from_secs(15));
        }
        teardown_webview(&app);
        app.exit(0);
    });
}

/// Close the webview before the process exits. Ending the process while WebKitGTK's web
/// process is still attached makes that helper die with a fatal error (a "WebKitWebProcess
/// crashed" notice on KDE/GNOME); destroying the window first lets it shut down cleanly.
fn teardown_webview(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
    #[cfg(target_os = "linux")]
    {
        let n = veranda_core::process::kill_children_named("WebKit");
        tracing::debug!("stopped {n} WebKit helper process(es)");
    }
}

/// 16x16 RGBA disc for the tray menu: green when running, red when stopped (anti-aliased edge).
fn status_dot(running: bool) -> Image<'static> {
    const N: usize = 16;
    let (r, g, b) = if running {
        (110u8, 170u8, 100u8)
    } else {
        (200u8, 80u8, 65u8)
    };
    let mut px = vec![0u8; N * N * 4];
    let c = (N as f32 - 1.0) / 2.0;
    let radius = 5.5f32;
    for y in 0..N {
        for x in 0..N {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let a = (radius + 0.5 - d).clamp(0.0, 1.0);
            let i = (y * N + x) * 4;
            px[i] = r;
            px[i + 1] = g;
            px[i + 2] = b;
            px[i + 3] = (a * 255.0) as u8;
        }
    }
    Image::new_owned(px, N as u32, N as u32)
}

/// The tray menu: actions plus one line per service that is on (name, ports, state).
/// Returns the menu and how many of those services are running.
fn tray_menu(app: &AppHandle) -> tauri::Result<(Menu<tauri::Wry>, usize)> {
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(
        app,
        "show",
        "Show Veranda",
        true,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    let statuses = open_app(&app.state::<AppState>())
        .ok()
        .and_then(|core| services::status_all(&core).ok())
        .unwrap_or_default();
    let shown: Vec<&ServiceStatus> = statuses
        .iter()
        .filter(|s| s.installed && (s.enabled || s.running))
        .collect();
    let running = shown.iter().filter(|s| s.running).count();
    if shown.is_empty() {
        menu.append(&MenuItem::with_id(
            app,
            "svc-none",
            "No service is on",
            false,
            None::<&str>,
        )?)?;
    }
    for s in &shown {
        let ports = s
            .ports
            .iter()
            .map(|p| format!(":{p}"))
            .collect::<Vec<_>>()
            .join(" ");
        let text = if s.running {
            format!("{}  {ports}  \u{2014} running", s.name)
        } else {
            format!("{}  \u{2014} stopped", s.name)
        };
        // A coloured dot the menu cannot dim: green = running, red = stopped. Clicking the
        // line opens the window; the text itself is informational.
        menu.append(&IconMenuItem::with_id(
            app,
            format!("svc-{}", s.id.id()),
            text,
            true,
            Some(status_dot(s.running)),
            None::<&str>,
        )?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        "start_all",
        "Start All",
        true,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        "stop_all",
        "Stop All",
        true,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        "open_web",
        "Open localhost",
        true,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        "quit",
        "Quit (stops services)",
        true,
        None::<&str>,
    )?)?;
    Ok((menu, running))
}

/// Rebuild the tray menu and tooltip from the current service states.
fn refresh_tray(app: &AppHandle) {
    let Some(tray) = app.tray_by_id("main") else {
        return;
    };
    let Ok((menu, running)) = tray_menu(app) else {
        return;
    };
    let _ = tray.set_menu(Some(menu));
    let tooltip = if running == 0 {
        "Veranda \u{2014} stopped".to_string()
    } else {
        format!("Veranda \u{2014} {running} running")
    };
    let _ = tray.set_tooltip(Some(tooltip));
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse().unwrap()),
        )
        .init();
    let core = Veranda::open().expect("cannot open the Veranda root");
    let _ = veranda_core::tls::ensure_ca(&core.layout);
    let auto_start = core.config.general.auto_start;
    let start_minimized = core.config.general.start_minimized;

    tauri::Builder::default()
        // Must be registered first: a second launch focuses the running window instead of
        // starting another process (and another tray icon).
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState {
            locked: std::sync::atomic::AtomicBool::new(veranda_core::applock::is_enabled(
                &core.layout,
            )),
            unlock_fails: Mutex::new((0, 0)),
            totp_pending: Mutex::new(None),
            totp_last: Mutex::new(0),
            migrate: Mutex::new(MigrateJob::default()),
            app: Mutex::new(core),
            heartbeat: std::sync::atomic::AtomicU64::new(0),
        })
        .manage(remote::RemoteState::default())
        .setup(move |app| {
            let (menu, _) = tray_menu(app.handle())?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().cloned().expect("icon"))
                .tooltip("Veranda")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| {
                    let state = app.state::<AppState>();
                    match event.id.as_ref() {
                        "show" => show_main(app),
                        "start_all" => {
                            if let Ok(core) = open_app(&state) {
                                let h = app.clone();
                                std::thread::spawn(move || {
                                    let _ = services::prepare_all(&core);
                                    let _ = services::start_all(&core);
                                    let _ = h.emit("services-changed", ());
                                    refresh_tray(&h);
                                });
                            }
                        }
                        "stop_all" => {
                            if let Ok(core) = open_app(&state) {
                                let h = app.clone();
                                std::thread::spawn(move || {
                                    let _ = services::stop_all(&core);
                                    let _ = h.emit("services-changed", ());
                                    refresh_tray(&h);
                                });
                            }
                        }
                        "open_web" => {
                            if let Ok(core) = open_app(&state) {
                                let scheme = if core.config.web.ssl { "https" } else { "http" };
                                let _ = veranda_core::tools::open_url(
                                    &core.config,
                                    &format!("{scheme}://localhost/"),
                                );
                            }
                        }
                        "quit" => {
                            quit_app(app.clone());
                        }
                        id if id.starts_with("svc-") => show_main(app),
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            // The window starts hidden (tauri.conf.json) and is shown by `ui_ready` once the
            // first render is done, so the user never sees a blank page. Safety net: show it
            // anyway after a moment if the UI failed to call back.
            if !start_minimized {
                let h = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(2500));
                    if let Some(w) = h.get_webview_window("main")
                        && !w.is_visible().unwrap_or(true)
                    {
                        let _ = w.show();
                    }
                });
            }
            let handle = app.handle().clone();
            if auto_start {
                let state = handle.state::<AppState>();
                if let Ok(core) = open_app(&state) {
                    std::thread::spawn(move || {
                        let _ = services::prepare_all(&core);
                        let default_group = core.config.general.default_group.trim().to_string();
                        let report = if !default_group.is_empty()
                            && let Ok(r) = services::start_group(&core, &default_group)
                        {
                            to_report(r)
                        } else {
                            to_report(services::start_all(&core))
                        };
                        let _ = handle.emit("services-changed", ());
                        refresh_tray(&handle);
                        // The UI shows the outcome (and the port dialogs) like a manual Start All.
                        let _ = handle.emit("autostart-done", report);
                    });
                }
            }
            // Services left running by a previous app version may still use old ports: heal once.
            let handle_h = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(4));
                let state = handle_h.state::<AppState>();
                if let Ok(core) = open_app(&state) {
                    heal_stale(&handle_h, &core);
                }
            });
            // Tray menu: keep the service list current even when nothing is clicked.
            let handle_t = app.handle().clone();
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(20));
                    refresh_tray(&handle_t);
                }
            });
            // Watchdog: reload the page when the UI stops sending heartbeats (web process died).
            let handle_w = app.handle().clone();
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(10));
                    let state = handle_w.state::<AppState>();
                    let last = state.heartbeat.load(std::sync::atomic::Ordering::Relaxed);
                    if last == 0 || now_secs().saturating_sub(last) < 30 {
                        continue;
                    }
                    if let Some(w) = handle_w.get_webview_window("main")
                        && w.is_visible().unwrap_or(false)
                        && let Ok(url) = w.url()
                    {
                        tracing::warn!("UI heartbeat lost; reloading the page");
                        state
                            .heartbeat
                            .store(now_secs(), std::sync::atomic::Ordering::Relaxed);
                        let _ = w.navigate(url);
                    }
                }
            });
            // Background: update check on start, then hourly (signed feed, GitHub API fallback;
            // the UI also checks when the window comes back after 30+ minutes). Installs
            // automatically when Settings say so.
            let handle_u = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                loop {
                    let check = {
                        let state = handle_u.state::<AppState>();
                        open_app(&state)
                            .map(|c| c.config.update.check_on_start)
                            .unwrap_or(false)
                    };
                    if check {
                        auto_update(handle_u.clone()).await;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                }
            });
            // Background: periodic database backups.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                    let core = {
                        let state = handle.state::<AppState>();
                        open_app(&state).ok()
                    };
                    if let Some(core) = core
                        && veranda_core::backup::is_due(&core)
                    {
                        let res = tauri::async_runtime::spawn_blocking(move || {
                            veranda_core::backup::run(&core)
                        })
                        .await;
                        if let Ok(Ok(p)) = res {
                            let _ = handle.emit("backup-done", p.display().to_string());
                        }
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                use veranda_core::config::CloseAction;
                let action = window
                    .state::<AppState>()
                    .app
                    .lock()
                    .map(|a| a.config.general.close_action)
                    .unwrap_or(CloseAction::Ask);
                api.prevent_close();
                match action {
                    CloseAction::Tray => {
                        let _ = window.hide();
                    }
                    CloseAction::Quit => {
                        let _ = window.hide();
                        quit_app(window.app_handle().clone());
                    }
                    CloseAction::Ask => {
                        // The UI shows a dialog and answers through `close_decision`.
                        let _ = window.emit("close-requested", ());
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            lock_status,
            app_unlock,
            app_lock,
            app_lock_set,
            app_lock_timer,
            app_totp_begin,
            app_totp_confirm,
            app_totp_disable,
            get_state,
            service_action,
            grant_web_ports,
            ui_ready,
            ui_heartbeat,
            perf_report,
            migrate_detect,
            migrate_run,
            migrate_status,
            migrate_control,
            pg_password,
            remove_external,
            node_set_default,
            group_action,
            cancel_install,
            start_all,
            stop_all,
            install_package,
            upstream_versions,
            builtin_versions,
            remove_package,
            save_config,
            sync_sites,
            hosts_sync,
            add_site,
            remove_site,
            create_project,
            delete_project,
            db_create,
            db_list,
            db_password,
            db_set_password,
            db_backup,
            storage_usage,
            free_space,
            ssl_trust,
            open_target,
            check_update,
            change_root,
            list_logs,
            read_log,
            clear_log,
            update_check,
            update_install,
            open_url_external,
            close_decision,
            remote::remote_state,
            remote::remote_prompt_answer,
            remote::remote_save_host,
            remote::remote_delete_host,
            remote::remote_save_group,
            remote::remote_delete_group,
            remote::remote_save_identity,
            remote::remote_delete_identity,
            files::fs_roots,
            files::fs_list,
            files::fs_read,
            files::fs_write,
            files::fs_create,
            files::fs_rename,
            files::fs_delete,
            transfer::move_plan,
            transfer::move_suggest,
            transfer::move_create,
            transfer::move_inspect,
            transfer::move_restore,
            transfer::move_finish,
            transfer::move_can_winget,
            remote::remote_save_snippet,
            remote::snippet_lock_set,
            remote::snippet_unlock,
            remote::snippet_lock_now,
            remote::term_send_snippet,
            remote::remote_delete_snippet,
            remote::remote_save_forward,
            remote::remote_delete_forward,
            remote::remote_save_settings,
            remote::remote_history_clear,
            remote::remote_known_hosts,
            remote::remote_known_host_remove,
            remote::remote_key_generate,
            remote::remote_key_import,
            remote::remote_key_candidates,
            remote::remote_key_delete,
            remote::remote_key_export,
            remote::remote_key_passphrase,
            remote::remote_key_deploy,
            remote::term_open,
            remote::term_write,
            remote::term_resize,
            remote::term_close,
            remote::term_stats,
            remote::sftp_open,
            remote::sftp_list,
            remote::sftp_mkdir,
            remote::sftp_rename,
            remote::sftp_delete,
            remote::sftp_chmod,
            remote::sftp_read,
            remote::sftp_write,
            remote::sftp_download,
            remote::sftp_upload,
            remote::sftp_cancel,
            remote::sftp_close,
            remote::local_list,
            remote::local_mkdir,
            remote::local_rename,
            remote::local_delete,
            remote::snippet_run,
            remote::forward_start,
            remote::forward_stop,
            remote::remote_import_detect,
            remote::remote_import_apply,
            remote::remote_open_link
        ])
        .run(tauri::generate_context!())
        .expect("error while running Veranda");
}
