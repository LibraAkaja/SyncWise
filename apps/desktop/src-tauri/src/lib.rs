use std::path::Path;

use syncwise_engine::{FolderPair, SyncError, validate_folder_pair as validate_folder_roots};

#[tauri::command]
fn validate_folder_pair(
    source_path: String,
    destination_path: String,
) -> Result<FolderPair, SyncError> {
    validate_folder_roots(Path::new(&source_path), Path::new(&destination_path))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![validate_folder_pair])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
