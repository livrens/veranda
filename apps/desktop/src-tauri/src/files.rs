//! Files page commands: browse folders, open text files in the built-in editor, save them,
//! create / rename / delete. Everything goes through `veranda_core::files`.

use crate::{AppState, CmdResult, err, open_app};
use std::path::PathBuf;
use tauri::State;
use veranda_core::files::{self, Document, Entry};

#[tauri::command]
pub async fn fs_roots(state: State<'_, AppState>) -> CmdResult<Vec<String>> {
    let app = open_app(&state)?;
    Ok(files::roots(&app.layout, &app.config)
        .into_iter()
        .map(|p| p.display().to_string())
        .collect())
}

#[tauri::command]
pub async fn fs_list(path: String) -> CmdResult<Vec<Entry>> {
    tauri::async_runtime::spawn_blocking(move || files::list(&PathBuf::from(path)).map_err(err))
        .await
        .map_err(err)?
}

#[tauri::command]
pub async fn fs_read(path: String) -> CmdResult<Document> {
    tauri::async_runtime::spawn_blocking(move || files::read(&PathBuf::from(path)).map_err(err))
        .await
        .map_err(err)?
}

/// Returns the new modification time (ms), to be passed back as `expect_mtime` next time.
#[tauri::command]
pub async fn fs_write(
    path: String,
    text: String,
    crlf: bool,
    expect_mtime: Option<u64>,
) -> CmdResult<u64> {
    tauri::async_runtime::spawn_blocking(move || {
        files::write(&PathBuf::from(path), &text, crlf, expect_mtime).map_err(err)
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub async fn fs_create(path: String, dir: bool) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || {
        files::create(&PathBuf::from(path), dir).map_err(err)
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub async fn fs_rename(from: String, to: String) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || {
        files::rename(&PathBuf::from(from), &PathBuf::from(to)).map_err(err)
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub async fn fs_delete(path: String) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || files::delete(&PathBuf::from(path)).map_err(err))
        .await
        .map_err(err)?
}
