use std::{
    env,
    fs::OpenOptions,
    io::ErrorKind,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use atlas_adapters::{
    HttpConnectionProbe, MacOsKeychainAdapter, MineruCloudHttpAdapter,
    OpenAiCompatibleReadingAssistantAdapter, OpenAiCompatibleTranslationAdapter,
    ProviderRuntimeAdapter,
};
use atlas_document_reader::{DefaultDocumentReader, DocumentReaderModule, ReaderSourceRegistry};
use atlas_domain::{
    DocumentId, DocumentSummary, ImportPdfResult, OpenSessionInput, OpenSessionResult,
    OpenedReaderDocument, ParseSnapshot, ReaderSourceToken, RefreshSourcesResult, SessionId,
};
use atlas_library::{DefaultLibraryModule, DocumentStore, LibraryModule};
use atlas_parse::{DefaultParseModule, ParseModule};
use atlas_provider_settings::{
    DefaultProviderSettings, EnvironmentSecretOverride, ProviderSettingsModule,
    ProviderSettingsStore, SecretStore,
};
use atlas_reading_assistant::{DefaultReadingAssistantModule, ReadingAssistantModule};
use atlas_reading_session::{DefaultReadingSession, ReadingSessionModule};
use atlas_storage::{
    AtlasDatabase, SqliteDocumentStore, SqliteParseStore, SqliteProviderSettingsStore,
    SqliteReadingAssistantStore, SqliteTranslationStore,
};
use atlas_translation::{DefaultTranslationModule, TranslationModule};
use fs2::FileExt;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::{
    auth::{AuthState, ClientId},
    error::{ApiError, internal},
    imports::ManagedImports,
    routes::router,
};

#[derive(Clone, Debug)]
pub struct RunOptions {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub frontend_dir: PathBuf,
    pub open_browser: bool,
}

impl RunOptions {
    #[must_use]
    pub fn from_env() -> Self {
        let port = env::var("ATLAS_WEB_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(0);
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            data_dir: env::var_os("ATLAS_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(default_data_dir),
            frontend_dir: env::var_os("ATLAS_WEB_DIST")
                .map(PathBuf::from)
                .unwrap_or_else(default_frontend_dir),
            open_browser: env::var("ATLAS_WEB_NO_OPEN").as_deref() != Ok("1"),
        }
    }
}

pub struct WebState {
    pub library: Arc<dyn LibraryModule>,
    pub imports: ManagedImports,
    pub document_reader: Arc<dyn DocumentReaderModule>,
    pub sources: Arc<ReaderSourceRegistry>,
    pub provider_settings: Arc<dyn ProviderSettingsModule>,
    pub parse: Arc<dyn ParseModule>,
    pub parse_store: Arc<SqliteParseStore>,
    pub translation: Arc<dyn TranslationModule>,
    pub assistant: Arc<dyn ReadingAssistantModule>,
    pub reading_session: Arc<dyn ReadingSessionModule>,
    pub artifact_root: PathBuf,
    pub auth: AuthState,
    leases: Mutex<LeaseState>,
    document_transitions: Mutex<()>,
    pub authority: String,
    pub origin: String,
}

#[derive(Debug)]
struct LeaseEntry {
    client_id: ClientId,
    document_id: DocumentId,
    touched_at: Instant,
}

#[derive(Debug, Default)]
struct LeaseState {
    readers: std::collections::HashMap<ReaderSourceToken, LeaseEntry>,
    sessions: std::collections::HashMap<(ClientId, SessionId), LeaseEntry>,
}

impl std::fmt::Debug for WebState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebState")
            .field("authority", &self.authority)
            .finish_non_exhaustive()
    }
}

pub async fn run(options: RunOptions) -> Result<(), ApiError> {
    tokio::fs::create_dir_all(&options.data_dir)
        .await
        .map_err(internal)?;
    let data_lock = acquire_data_lock(&options.data_dir)?;
    // Detached parse/provider tasks are owned by the process runtime. Keep the
    // OS lock until process termination so a replacement process cannot overlap
    // their final cancellation window.
    std::mem::forget(data_lock);
    if !options.frontend_dir.join("index.html").is_file() {
        return Err(internal(format!(
            "web frontend is not built at {}",
            options.frontend_dir.display()
        )));
    }

    let listener = TcpListener::bind(options.bind).await.map_err(internal)?;
    let address = listener.local_addr().map_err(internal)?;
    let state = Arc::new(build_state(&options.data_dir, address).await?);
    let launch_token = state
        .auth
        .launch_token()
        .await
        .ok_or_else(|| internal("launch token is unavailable"))?;
    let launch_url = format!("{}/#launch={launch_token}", state.origin);
    let cleanup_state = state.clone();
    let application = router(state, options.frontend_dir);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            if let Err(error) = cleanup_state.cleanup_stale_leases().await {
                eprintln!("Atlas lease cleanup failed: {error}");
            }
        }
    });

    println!("Atlas Reader web is running at {}", address);
    if options.open_browser {
        launch_browser(&launch_url)?;
    } else {
        println!("Open {launch_url}");
    }

    axum::serve(
        listener,
        application.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(internal)
}

fn acquire_data_lock(data_dir: &std::path::Path) -> Result<std::fs::File, ApiError> {
    let path = data_dir.join(".atlas-web.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(internal)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(file),
        Err(error) if error.kind() == ErrorKind::WouldBlock => Err(internal(
            "Atlas Reader is already running for this data directory",
        )),
        Err(error) => Err(internal(error)),
    }
}

async fn build_state(
    data_dir: &std::path::Path,
    address: SocketAddr,
) -> Result<WebState, ApiError> {
    let database = AtlasDatabase::open(&data_dir.join("atlas.sqlite3")).await?;
    let document_store: Arc<dyn DocumentStore> = Arc::new(SqliteDocumentStore::new(&database));
    let library: Arc<dyn LibraryModule> =
        Arc::new(DefaultLibraryModule::new(document_store.clone()));
    let sources = Arc::new(ReaderSourceRegistry::default());
    let document_reader: Arc<dyn DocumentReaderModule> = Arc::new(DefaultDocumentReader::new(
        Arc::new(SqliteDocumentStore::new(&database)),
        sources.clone(),
    ));
    let profile_store: Arc<dyn ProviderSettingsStore> =
        Arc::new(SqliteProviderSettingsStore::new(&database));
    let secrets: Arc<dyn SecretStore> =
        Arc::new(EnvironmentSecretOverride::new(MacOsKeychainAdapter::new()));
    let provider_settings_impl = Arc::new(DefaultProviderSettings::new(
        profile_store,
        secrets,
        Arc::new(HttpConnectionProbe::new()?),
    ));
    let provider_settings: Arc<dyn ProviderSettingsModule> = provider_settings_impl.clone();
    let providers = Arc::new(ProviderRuntimeAdapter::new(provider_settings_impl));
    let artifact_root = data_dir.join("parse-artifacts");
    let parse_store = Arc::new(SqliteParseStore::new(&database, artifact_root.clone()));
    let managed_root = data_dir.join("managed-pdfs");
    let imports = ManagedImports::new(
        library.clone(),
        document_store.clone(),
        managed_root,
        artifact_root.clone(),
        sources.clone(),
    );
    imports.prepare().await?;
    let parse: Arc<dyn ParseModule> = Arc::new(DefaultParseModule::new(
        parse_store.clone(),
        document_store.clone(),
        providers.clone(),
        Arc::new(MineruCloudHttpAdapter::new()?),
        artifact_root.clone(),
    ));
    parse.recover().await?;
    let translation: Arc<dyn TranslationModule> = Arc::new(DefaultTranslationModule::new(
        parse_store.clone(),
        Arc::new(SqliteTranslationStore::new(&database)),
        providers.clone(),
        Arc::new(OpenAiCompatibleTranslationAdapter::new()?),
    ));
    translation.recover().await?;
    let assistant: Arc<dyn ReadingAssistantModule> = Arc::new(DefaultReadingAssistantModule::new(
        parse_store.clone(),
        translation.clone(),
        Arc::new(SqliteReadingAssistantStore::new(&database)),
        providers.clone(),
        Arc::new(OpenAiCompatibleReadingAssistantAdapter::new()?),
    ));
    assistant.recover().await?;
    let reading_session: Arc<dyn ReadingSessionModule> = Arc::new(DefaultReadingSession::new(
        providers,
        parse.clone(),
        translation.clone(),
        assistant.clone(),
    ));
    let authority = address.to_string();
    let origin = format!("http://{authority}");

    Ok(WebState {
        library,
        imports,
        document_reader,
        sources,
        provider_settings,
        parse,
        parse_store,
        translation,
        assistant,
        reading_session,
        artifact_root,
        auth: AuthState::new(),
        leases: Mutex::new(LeaseState::default()),
        document_transitions: Mutex::new(()),
        authority,
        origin,
    })
}

impl WebState {
    pub async fn import_document(
        &self,
        field: axum::extract::multipart::Field<'_>,
    ) -> Result<ImportPdfResult, ApiError> {
        let _transition = self.document_transitions.lock().await;
        Ok(self.imports.import(field).await?)
    }

    pub async fn refresh_library(&self) -> Result<RefreshSourcesResult, ApiError> {
        let _transition = self.document_transitions.lock().await;
        Ok(self.library.refresh_sources().await?)
    }

    pub async fn open_reader(
        &self,
        client_id: ClientId,
        document_id: DocumentId,
    ) -> Result<OpenedReaderDocument, ApiError> {
        let _transition = self.document_transitions.lock().await;
        let opened = self.document_reader.open(document_id.clone()).await?;
        self.register_reader(client_id, opened.source_token.clone(), document_id)
            .await;
        Ok(opened)
    }

    pub async fn open_session(
        &self,
        client_id: ClientId,
        input: OpenSessionInput,
    ) -> Result<OpenSessionResult, ApiError> {
        let _transition = self.document_transitions.lock().await;
        let document_id = input.document_id.clone();
        let opened = self.reading_session.open(input).await?;
        self.register_session(client_id, opened.session_id.clone(), document_id)
            .await;
        Ok(opened)
    }

    pub async fn relocate_document(
        &self,
        document_id: DocumentId,
        field: axum::extract::multipart::Field<'_>,
    ) -> Result<DocumentSummary, ApiError> {
        let _transition = self.document_transitions.lock().await;
        Ok(self.imports.relocate(document_id, field).await?)
    }

    pub async fn retry_parse(&self, document_id: &DocumentId) -> Result<ParseSnapshot, ApiError> {
        let _transition = self.document_transitions.lock().await;
        Ok(self.parse.retry_remote_status(document_id).await?)
    }

    pub async fn reupload_parse(
        &self,
        document_id: DocumentId,
        session_id: SessionId,
    ) -> Result<ParseSnapshot, ApiError> {
        let _transition = self.document_transitions.lock().await;
        Ok(self
            .parse
            .reupload(document_id, session_id.as_str().to_owned())
            .await?)
    }

    pub async fn remove_document(&self, document_id: DocumentId) -> Result<(), ApiError> {
        let _transition = self.document_transitions.lock().await;
        self.close_document_leases(&document_id).await?;
        self.parse.cancel_document(&document_id).await?;
        self.translation.close_document(&document_id).await?;
        self.assistant.close_document(&document_id).await?;
        self.imports.remove(document_id).await?;
        Ok(())
    }

    pub async fn register_reader(
        &self,
        client_id: ClientId,
        token: ReaderSourceToken,
        document_id: DocumentId,
    ) {
        self.leases.lock().await.readers.insert(
            token,
            LeaseEntry {
                client_id,
                document_id,
                touched_at: Instant::now(),
            },
        );
    }

    pub async fn touch_reader(&self, token: &ReaderSourceToken, client_id: Option<&ClientId>) {
        if let Some(entry) = self.leases.lock().await.readers.get_mut(token)
            && client_id.is_none_or(|client_id| client_id == &entry.client_id)
        {
            entry.touched_at = Instant::now();
        }
    }

    pub async fn unregister_reader(&self, client_id: &ClientId, token: &ReaderSourceToken) {
        let mut leases = self.leases.lock().await;
        if leases
            .readers
            .get(token)
            .is_some_and(|entry| &entry.client_id == client_id)
        {
            leases.readers.remove(token);
        }
    }

    pub async fn register_session(
        &self,
        client_id: ClientId,
        session_id: SessionId,
        document_id: DocumentId,
    ) {
        self.leases.lock().await.sessions.insert(
            (client_id.clone(), session_id),
            LeaseEntry {
                client_id,
                document_id,
                touched_at: Instant::now(),
            },
        );
    }

    pub async fn touch_session(&self, client_id: &ClientId, session_id: &SessionId) {
        if let Some(entry) = self
            .leases
            .lock()
            .await
            .sessions
            .get_mut(&(client_id.clone(), session_id.clone()))
        {
            entry.touched_at = Instant::now();
        }
    }

    pub async fn unregister_session(&self, client_id: &ClientId, session_id: &SessionId) {
        self.leases
            .lock()
            .await
            .sessions
            .remove(&(client_id.clone(), session_id.clone()));
    }

    pub async fn heartbeat(
        &self,
        client_id: &ClientId,
        readers: &[ReaderSourceToken],
        sessions: &[SessionId],
    ) {
        let now = Instant::now();
        let mut leases = self.leases.lock().await;
        for token in readers {
            if let Some(entry) = leases.readers.get_mut(token)
                && &entry.client_id == client_id
            {
                entry.touched_at = now;
            }
        }
        for session_id in sessions {
            if let Some(entry) = leases
                .sessions
                .get_mut(&(client_id.clone(), session_id.clone()))
            {
                entry.touched_at = now;
            }
        }
    }

    pub async fn close_client_leases(
        &self,
        client_id: &ClientId,
        readers: Vec<ReaderSourceToken>,
        sessions: Vec<SessionId>,
    ) -> Result<(), ApiError> {
        let (readers, sessions) = {
            let mut leases = self.leases.lock().await;
            let readers = readers
                .into_iter()
                .filter(|token| {
                    leases
                        .readers
                        .get(token)
                        .is_some_and(|entry| &entry.client_id == client_id)
                })
                .collect::<Vec<_>>();
            let sessions = sessions
                .into_iter()
                .filter(|session_id| {
                    leases
                        .sessions
                        .contains_key(&(client_id.clone(), session_id.clone()))
                })
                .collect::<Vec<_>>();
            for token in &readers {
                leases.readers.remove(token);
            }
            for session_id in &sessions {
                leases
                    .sessions
                    .remove(&(client_id.clone(), session_id.clone()));
            }
            (readers, sessions)
        };
        self.close_leases(readers, sessions).await
    }

    pub async fn close_document_leases(&self, document_id: &DocumentId) -> Result<(), ApiError> {
        let (readers, sessions) = {
            let mut leases = self.leases.lock().await;
            let readers = leases
                .readers
                .iter()
                .filter_map(|(token, entry)| {
                    (&entry.document_id == document_id).then_some(token.clone())
                })
                .collect::<Vec<_>>();
            let session_keys = leases
                .sessions
                .iter()
                .filter_map(|(key, entry)| {
                    (&entry.document_id == document_id).then_some(key.clone())
                })
                .collect::<Vec<_>>();
            let sessions = session_keys
                .iter()
                .map(|(_, session_id)| session_id.clone())
                .collect::<Vec<_>>();
            for token in &readers {
                leases.readers.remove(token);
            }
            for key in session_keys {
                leases.sessions.remove(&key);
            }
            (readers, sessions)
        };
        self.close_leases(readers, sessions).await
    }

    async fn cleanup_stale_leases(&self) -> Result<(), ApiError> {
        let cutoff = Instant::now() - Duration::from_secs(120);
        let (readers, sessions) = {
            let mut leases = self.leases.lock().await;
            let readers = leases
                .readers
                .iter()
                .filter_map(|(token, entry)| (entry.touched_at < cutoff).then_some(token.clone()))
                .collect::<Vec<_>>();
            let session_keys = leases
                .sessions
                .iter()
                .filter_map(|(key, entry)| (entry.touched_at < cutoff).then_some(key.clone()))
                .collect::<Vec<_>>();
            let sessions = session_keys
                .iter()
                .map(|(_, session_id)| session_id.clone())
                .collect::<Vec<_>>();
            for token in &readers {
                leases.readers.remove(token);
            }
            for key in session_keys {
                leases.sessions.remove(&key);
            }
            (readers, sessions)
        };
        self.close_leases(readers, sessions).await
    }

    async fn close_leases(
        &self,
        readers: Vec<ReaderSourceToken>,
        sessions: Vec<SessionId>,
    ) -> Result<(), ApiError> {
        for token in readers {
            if let Err(error) = self.document_reader.close(&token, None).await
                && error.code != atlas_domain::AtlasErrorCode::NotFound
            {
                return Err(error.into());
            }
        }
        for session_id in sessions {
            if let Err(error) = self.reading_session.close(&session_id).await
                && error.code != atlas_domain::AtlasErrorCode::NotFound
            {
                return Err(error.into());
            }
        }
        Ok(())
    }
}

fn default_data_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library/Application Support/com.atlasreader.desktop");
    }

    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("atlas-reader")
}

fn default_frontend_dir() -> PathBuf {
    if let Ok(executable) = env::current_exe()
        && let Some(parent) = executable.parent()
    {
        let packaged = parent.join("web-dist");
        if packaged.join("index.html").is_file() {
            return packaged;
        }
    }
    PathBuf::from("apps/web/dist")
}

fn launch_browser(url: &str) -> Result<(), ApiError> {
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .status()
    } else {
        std::process::Command::new("xdg-open").arg(url).status()
    }
    .map_err(internal)?;
    if status.success() {
        Ok(())
    } else {
        Err(internal("browser launcher returned an error"))
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn data_directory_allows_only_one_server_process() {
        let temporary = TempDir::new().expect("temporary directory");
        let first = acquire_data_lock(temporary.path()).expect("first lock should succeed");
        let second = acquire_data_lock(temporary.path()).expect_err("second lock should fail");
        assert!(second.to_string().contains("already running"));
        drop(first);
        acquire_data_lock(temporary.path()).expect("lock should release");
    }
}
