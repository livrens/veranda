//! "Move to another computer" commands: plan, create, inspect and restore a `.veranda-move`
//! bundle, then finish the job (reinstall packages, put secrets back, clone repositories,
//! install VS Code extensions and, on Windows, missing tools). Progress lines are emitted as
//! `move-progress`.

use crate::{AppState, CmdResult, err, open_app};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use veranda_core::manifest::{Kind, Manifest as PkgManifest};
use veranda_core::transfer::{
    self, CreateRequest, Homes, Manifest, PkgRef, Plan, RepoRef, Report, RestoreRequest, ToolRef,
};
use veranda_core::{Platform, Veranda};

fn homes(app: &Veranda) -> CmdResult<Homes> {
    Homes::current(&app.layout).map_err(err)
}

fn emit(handle: &AppHandle, msg: String) {
    let _ = handle.emit("move-progress", msg);
}

#[tauri::command]
pub async fn move_plan(state: State<'_, AppState>, req: CreateRequest) -> CmdResult<Plan> {
    let app = open_app(&state)?;
    tauri::async_runtime::spawn_blocking(move || transfer::plan(&homes(&app)?, &req).map_err(err))
        .await
        .map_err(err)?
}

#[tauri::command]
pub async fn move_create(
    handle: AppHandle,
    state: State<'_, AppState>,
    mut req: CreateRequest,
) -> CmdResult<Manifest> {
    let app = open_app(&state)?;
    // keyring secrets travel inside the bundle, never in the plain store file
    if req.sections.is_empty() || req.sections.iter().any(|s| s == "veranda") {
        match crate::remote::export_secrets(&handle) {
            Ok(bytes) => req
                .extra_files
                .push(("veranda/etc/remote/secrets-export.json".into(), bytes)),
            Err(e) => emit(&handle, format!("remote secrets not exported: {e}")),
        }
    }
    // remember the folder list for next time
    {
        let mut cfg = app.config.clone();
        cfg.transfer.folders = req
            .folders
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        if cfg.transfer.folders != app.config.transfer.folders {
            if let Err(e) = cfg.save(&app.layout) {
                emit(&handle, format!("folder list not remembered: {e}"));
            } else if let Ok(mut g) = state.app.lock() {
                g.config = cfg;
            }
        }
    }
    let h = handle.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let progress = |m: String| emit(&h, m);
        transfer::create(&homes(&app)?, &req, &progress).map_err(err)
    })
    .await
    .map_err(err)?
}

/// Folders that look like projects, for the "Find projects" list.
#[tauri::command]
pub async fn move_suggest(state: State<'_, AppState>) -> CmdResult<Vec<transfer::Suggestion>> {
    let app = open_app(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(transfer::suggest_folders(&homes(&app)?)))
        .await
        .map_err(err)?
}

#[tauri::command]
pub async fn move_inspect(file: String, password: String) -> CmdResult<Manifest> {
    tauri::async_runtime::spawn_blocking(move || {
        transfer::inspect(std::path::Path::new(&file), &password).map_err(err)
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub async fn move_restore(
    handle: AppHandle,
    state: State<'_, AppState>,
    req: RestoreRequest,
) -> CmdResult<Report> {
    let app = open_app(&state)?;
    let hm = homes(&app)?;
    let h = handle.clone();
    let hm2 = hm.clone();
    let mut report = tauri::async_runtime::spawn_blocking(move || {
        let progress = |m: String| emit(&h, m);
        transfer::restore(&hm2, &req, &progress).map_err(err)
    })
    .await
    .map_err(err)??;
    // the configuration on disk may have changed: reload it for every later command
    match Veranda::open_at(hm.root.clone()) {
        Ok(fresh) => {
            if let Ok(mut g) = state.app.lock() {
                *g = fresh;
            }
        }
        Err(e) => report
            .failed
            .push(format!("reloading the configuration: {e}")),
    }
    let export = hm
        .root
        .join("etc")
        .join("remote")
        .join("secrets-export.json");
    if export.is_file() {
        match std::fs::read(&export)
            .map_err(anyhow::Error::from)
            .and_then(|b| crate::remote::import_secrets(&handle, &b))
        {
            Ok(n) => emit(&handle, format!("{n} remote secret(s) stored")),
            Err(e) => report.failed.push(format!("remote secrets: {e}")),
        }
        let _ = std::fs::remove_file(&export);
    }
    let _ = handle.emit("services-changed", ());
    let _ = handle.emit("config-changed", ());
    Ok(report)
}

#[derive(Debug, Clone, Deserialize)]
pub struct FinishRequest {
    pub manifest: Manifest,
    pub packages: Vec<PkgRef>,
    pub repos: Vec<RepoRef>,
    pub extensions: Vec<String>,
    pub tools: Vec<ToolRef>,
    #[serde(default)]
    pub database_dumps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct FinishReport {
    pub installed: Vec<String>,
    pub failed: Vec<String>,
}

/// The slow part after restore. Every step is best effort; failures are listed, not fatal.
#[tauri::command]
pub async fn move_finish(
    handle: AppHandle,
    state: State<'_, AppState>,
    req: FinishRequest,
) -> CmdResult<FinishReport> {
    let app = open_app(&state)?;
    let hm = homes(&app)?;
    let mut out = FinishReport::default();
    let p = Platform::current();
    let builtin = PkgManifest::builtin().map_err(err)?;
    for pk in &req.packages {
        let Ok(kind) = pk.kind.parse::<Kind>() else {
            out.failed
                .push(format!("{}: unknown package kind", pk.kind));
            continue;
        };
        if veranda_core::packages::Registry::load(&app.layout)
            .map(|r| r.find(kind, &pk.version).is_some())
            .unwrap_or(false)
        {
            continue;
        }
        emit(
            &handle,
            format!("installing {} {}…", kind.display(), pk.version),
        );
        let found = match builtin
            .versions(kind, p)
            .into_iter()
            .find(|x| x.version == pk.version)
            .cloned()
        {
            Some(x) => Some(x),
            None => veranda_core::resolve::versions(kind, p)
                .await
                .ok()
                .and_then(|v| v.into_iter().find(|x| x.version == pk.version)),
        };
        let pkg = match found {
            Some(x) => x,
            None => match veranda_core::resolve::latest(kind, p).await {
                Ok(x) => {
                    emit(
                        &handle,
                        format!(
                            "{} {} is not available here; using {}",
                            kind.display(),
                            pk.version,
                            x.version
                        ),
                    );
                    x
                }
                Err(e) => {
                    out.failed
                        .push(format!("{} {}: {e}", kind.display(), pk.version));
                    continue;
                }
            },
        };
        let h = handle.clone();
        let label = format!("{} {}", kind.display(), pkg.version);
        let progress = move |pr: veranda_core::download::Progress| {
            if let Some(t) = pr.total
                && t > 0
            {
                emit(&h, format!("{label}: {}%", pr.downloaded * 100 / t));
            }
        };
        match veranda_core::packages::install(&app.layout, &pkg, Some(&progress)).await {
            Ok(i) => out
                .installed
                .push(format!("{} {}", kind.display(), i.version)),
            Err(e) => out
                .failed
                .push(format!("{} {}: {e}", kind.display(), pkg.version)),
        }
    }
    if !req.database_dumps.is_empty() {
        let app2 = match Veranda::open_at(hm.root.clone()) {
            Ok(a) => a,
            Err(_) => app.clone(),
        };
        let dumps = req.database_dumps.clone();
        let h = handle.clone();
        let r = tauri::async_runtime::spawn_blocking(move || {
            transfer::load_database_dumps(&app2, &dumps, &|m| emit(&h, m))
        })
        .await
        .map_err(err)?;
        out.installed.extend(r.0);
        out.failed.extend(r.1);
    }
    let h = handle.clone();
    let (manifest, repos, exts, tools) = (req.manifest, req.repos, req.extensions, req.tools);
    let hm2 = hm.clone();
    let more = tauri::async_runtime::spawn_blocking(move || {
        let progress = |m: String| emit(&h, m);
        let mut failed = vec![];
        failed.extend(transfer::clone_repos(&hm2, &manifest, &repos, &progress));
        failed.extend(transfer::install_vscode_extensions(&exts, &progress));
        if cfg!(windows) && !tools.is_empty() {
            failed.extend(transfer::winget_install(&tools, &progress));
        }
        failed
    })
    .await
    .map_err(err)?;
    out.failed.extend(more);
    if !out.installed.is_empty() {
        let _ = veranda_core::services::prepare_all(&app);
        let _ = handle.emit("services-changed", ());
    }
    Ok(out)
}

/// `true` on Windows, where missing tools can be installed with winget.
#[tauri::command]
pub fn move_can_winget() -> bool {
    cfg!(windows) && which::which("winget").is_ok()
}
