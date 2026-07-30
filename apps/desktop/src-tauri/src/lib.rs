mod app_state;
mod commands;
mod pdf_protocol;

use std::sync::Arc;

use app_state::AppState;
use atlas_adapters::UnconfiguredProviderStatusAdapter;
use atlas_document_reader::{DefaultDocumentReader, ReaderSourceRegistry};
use atlas_library::DefaultLibraryModule;
use atlas_reading_session::DefaultReadingSession;
use atlas_storage::{AtlasDatabase, SqliteDocumentStore};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let reader_sources = Arc::new(ReaderSourceRegistry::default());
    let protocol_sources = reader_sources.clone();
    let module_sources = reader_sources.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .register_asynchronous_uri_scheme_protocol(
            "atlas-reader",
            move |_context, request, responder| {
                let sources = protocol_sources.clone();
                std::thread::spawn(move || {
                    responder.respond(pdf_protocol::respond(&sources, &request));
                });
            },
        )
        .setup(move |app| {
            let data_dir = app.path().app_data_dir()?;
            let database_path = data_dir.join("atlas.sqlite3");
            let database = tauri::async_runtime::block_on(AtlasDatabase::open(&database_path))?;
            let document_store = Arc::new(SqliteDocumentStore::new(&database));
            let library = Arc::new(DefaultLibraryModule::new(document_store.clone()));
            let document_reader =
                Arc::new(DefaultDocumentReader::new(document_store, module_sources));
            let providers = Arc::new(UnconfiguredProviderStatusAdapter);
            let reading_session = Arc::new(DefaultReadingSession::new(providers));
            app.manage(AppState::new(library, document_reader, reading_session));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::library::library_import_pdf,
            commands::library::library_query,
            commands::library::library_refresh_sources,
            commands::library::library_relocate,
            commands::library::library_remove,
            commands::reader::reader_close,
            commands::reader::reader_open,
            commands::reader::reader_save_position,
            commands::reading_session::reading_session_close,
            commands::reading_session::reading_session_dispatch,
            commands::reading_session::reading_session_open,
        ])
        .run(tauri::generate_context!())
        .expect("Atlas Reader failed to start");
}
