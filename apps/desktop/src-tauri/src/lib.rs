mod app_state;
mod commands;

use std::sync::Arc;

use app_state::AppState;
use atlas_adapters::UnconfiguredProviderStatusAdapter;
use atlas_library::DefaultLibraryModule;
use atlas_reading_session::DefaultReadingSession;
use atlas_storage::{AtlasDatabase, SqliteDocumentStore};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let database_path = data_dir.join("atlas.sqlite3");
            let database = tauri::async_runtime::block_on(AtlasDatabase::open(&database_path))?;
            let document_store = Arc::new(SqliteDocumentStore::new(&database));
            let library = Arc::new(DefaultLibraryModule::new(document_store));
            let providers = Arc::new(UnconfiguredProviderStatusAdapter);
            let reading_session = Arc::new(DefaultReadingSession::new(providers));
            app.manage(AppState::new(library, reading_session));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::library::library_query,
            commands::reading_session::reading_session_close,
            commands::reading_session::reading_session_dispatch,
            commands::reading_session::reading_session_open,
        ])
        .run(tauri::generate_context!())
        .expect("Atlas Reader failed to start");
}
