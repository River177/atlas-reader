mod app_state;
mod artifact_protocol;
mod commands;
mod pdf_protocol;

use std::sync::{Arc, PoisonError, RwLock};

use app_state::AppState;
use atlas_adapters::{
    HttpConnectionProbe, MacOsKeychainAdapter, MineruCloudHttpAdapter,
    OpenAiCompatibleTranslationAdapter, ProviderRuntimeAdapter,
};
use atlas_document_reader::{DefaultDocumentReader, ReaderSourceRegistry};
use atlas_library::DefaultLibraryModule;
use atlas_parse::{DefaultParseModule, LocalPdfExtractor, ParseModule};
use atlas_provider_settings::{
    DefaultProviderSettings, EnvironmentSecretOverride, ProviderSettingsStore, SecretStore,
};
use atlas_reading_session::DefaultReadingSession;
use atlas_storage::{
    AtlasDatabase, SqliteDocumentStore, SqliteParseStore, SqliteProviderSettingsStore,
    SqliteTranslationStore,
};
use atlas_translation::{DefaultTranslationModule, TranslationModule};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let reader_sources = Arc::new(ReaderSourceRegistry::default());
    let protocol_sources = reader_sources.clone();
    let module_sources = reader_sources.clone();
    let artifact_root = Arc::new(RwLock::new(None));
    let protocol_artifact_root = artifact_root.clone();
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
        .register_asynchronous_uri_scheme_protocol(
            "atlas-artifact",
            move |_context, request, responder| {
                let root = protocol_artifact_root
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                std::thread::spawn(move || {
                    responder.respond(artifact_protocol::respond(root.as_deref(), &request));
                });
            },
        )
        .setup(move |app| {
            let data_dir = app.path().app_data_dir()?;
            *artifact_root
                .write()
                .unwrap_or_else(PoisonError::into_inner) = Some(data_dir.join("parse-artifacts"));
            let database_path = data_dir.join("atlas.sqlite3");
            let database = tauri::async_runtime::block_on(AtlasDatabase::open(&database_path))?;
            let document_store = Arc::new(SqliteDocumentStore::new(&database));
            let library = Arc::new(DefaultLibraryModule::new(document_store.clone()));
            let document_reader = Arc::new(DefaultDocumentReader::new(
                document_store.clone(),
                module_sources,
            ));
            let profile_store: Arc<dyn ProviderSettingsStore> =
                Arc::new(SqliteProviderSettingsStore::new(&database));
            let secrets: Arc<dyn SecretStore> =
                Arc::new(EnvironmentSecretOverride::new(MacOsKeychainAdapter::new()));
            let provider_settings_impl = Arc::new(DefaultProviderSettings::new(
                profile_store.clone(),
                secrets.clone(),
                Arc::new(HttpConnectionProbe::new()?),
            ));
            let provider_settings = provider_settings_impl.clone();
            let providers = Arc::new(ProviderRuntimeAdapter::new(provider_settings_impl));
            let parse_store = Arc::new(SqliteParseStore::new(
                &database,
                data_dir.join("parse-artifacts"),
            ));
            let parse: Arc<dyn ParseModule> = Arc::new(DefaultParseModule::new(
                parse_store.clone(),
                document_store,
                providers.clone(),
                Arc::new(MineruCloudHttpAdapter::new()?),
                Arc::new(LocalPdfExtractor::new()),
                data_dir.join("parse-artifacts"),
            ));
            tauri::async_runtime::block_on(parse.recover())?;
            let translation: Arc<dyn TranslationModule> = Arc::new(DefaultTranslationModule::new(
                parse_store,
                Arc::new(SqliteTranslationStore::new(&database)),
                providers.clone(),
                Arc::new(OpenAiCompatibleTranslationAdapter::new()?),
            ));
            tauri::async_runtime::block_on(translation.recover())?;
            let reading_session = Arc::new(DefaultReadingSession::new(
                providers,
                parse.clone(),
                translation,
            ));
            app.manage(AppState::new(
                library,
                document_reader,
                provider_settings,
                parse,
                reading_session,
            ));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::library::library_import_pdf,
            commands::library::library_query,
            commands::library::library_refresh_sources,
            commands::library::library_relocate,
            commands::library::library_remove,
            commands::parse::parse_reupload,
            commands::parse::parse_retry_remote,
            commands::parse::parse_view,
            commands::provider_settings::provider_settings_delete_secret,
            commands::provider_settings::provider_settings_get,
            commands::provider_settings::provider_settings_save_mineru,
            commands::provider_settings::provider_settings_save_translation,
            commands::provider_settings::provider_settings_test,
            commands::reader::reader_close,
            commands::reader::reader_open,
            commands::reader::reader_save_position,
            commands::reading_session::reading_session_close,
            commands::reading_session::reading_session_dispatch,
            commands::reading_session::reading_session_open,
            commands::reading_session::reading_session_snapshot,
        ])
        .run(tauri::generate_context!())
        .expect("Atlas Reader failed to start");
}
