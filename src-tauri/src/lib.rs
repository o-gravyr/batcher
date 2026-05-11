mod compositor;
mod pipeline;
mod spreadsheet;

use std::path::PathBuf;

use tauri::{AppHandle, Manager};

#[tauri::command]
async fn process_batch(
    app: AppHandle,
    spreadsheet_path: String,
    image_inputs: Vec<String>,
) -> Result<pipeline::ProcessResult, String> {
    let sheet = PathBuf::from(spreadsheet_path);
    let images: Vec<PathBuf> = image_inputs.into_iter().map(PathBuf::from).collect();
    tauri::async_runtime::spawn_blocking(move || pipeline::process(app, sheet, images))
        .await
        .map_err(|e| format!("task interrupted: {e}"))?
}

#[tauri::command]
async fn preview_batch(
    spreadsheet_path: String,
    image_inputs: Vec<String>,
) -> Result<pipeline::PreviewResult, String> {
    let sheet = PathBuf::from(spreadsheet_path);
    let images: Vec<PathBuf> = image_inputs.into_iter().map(PathBuf::from).collect();
    tauri::async_runtime::spawn_blocking(move || pipeline::preview(&sheet, images))
        .await
        .map_err(|e| format!("task interrupted: {e}"))?
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
        .invoke_handler(tauri::generate_handler![process_batch, preview_batch])
        .run(tauri::generate_context!())
        .expect("error while running Tauri");
}
