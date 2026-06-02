mod compositor;
mod pipeline;
mod spreadsheet;

use std::path::PathBuf;

use tauri::{AppHandle, Manager};

#[tauri::command]
async fn process_batch(
    app: AppHandle,
    working_folder: String,
    plan: pipeline::Plan,
) -> Result<pipeline::ProcessResult, String> {
    let folder = PathBuf::from(working_folder);
    tauri::async_runtime::spawn_blocking(move || pipeline::process(app, folder, plan))
        .await
        .map_err(|e| format!("task interrupted: {e}"))?
}

#[tauri::command]
async fn preview_batch(working_folder: String) -> Result<pipeline::PreviewResult, String> {
    let folder = PathBuf::from(working_folder);
    tauri::async_runtime::spawn_blocking(move || pipeline::preview(&folder))
        .await
        .map_err(|e| format!("task interrupted: {e}"))?
}

#[tauri::command]
async fn scan_workspace(working_folder: String) -> Result<pipeline::Workspace, String> {
    let folder = PathBuf::from(working_folder);
    tauri::async_runtime::spawn_blocking(move || pipeline::scan_workspace(&folder))
        .await
        .map_err(|e| format!("task interrupted: {e}"))?
}

/// Reveal a folder/file in the OS file manager (Finder / Explorer).
#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let program = "xdg-open";

    std::process::Command::new(program)
        .arg(&path)
        .spawn()
        .map_err(|e| format!("open {path}: {e}"))?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            #[cfg(debug_assertions)]
            {
                if let Some(window) = app.get_webview_window("main") {
                    window.open_devtools();
                }
            }
            let _ = app;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            process_batch,
            preview_batch,
            scan_workspace,
            open_path
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri");
}
