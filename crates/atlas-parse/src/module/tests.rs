use std::{
    collections::{HashMap, VecDeque},
    io::{Cursor, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};

use async_trait::async_trait;
use atlas_domain::{
    AtlasError, BlockId, BlockKind, CANONICAL_SCHEMA_VERSION, CanonicalBlock, CanonicalChapter,
    CanonicalDocument, ChapterId, ChapterRole, DocumentFileState, DocumentId, DocumentSummary,
    ParseState, ParserIdentity, StructuredContent,
};
use atlas_library::{
    DocumentImport, DocumentListRequest, DocumentRecord, DocumentSourceUpdate, DocumentStore,
    StoredImport,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::{Mutex, Notify};
use zip::{ZipWriter, write::SimpleFileOptions};

use super::*;
use crate::{
    CancelCapability, CloudParseError, CloudParseStatus, CloudParseSubmission, CloudParserPort,
    ParseStore, PublishArtifact,
};

#[derive(Default)]
struct MemoryState {
    operations: Vec<ParseOperation>,
    active: HashMap<DocumentId, CanonicalDocument>,
}

#[derive(Default)]
struct MemoryParseStore {
    state: Mutex<MemoryState>,
    publish_failures: AtomicUsize,
}

#[async_trait]
impl ParseStore for MemoryParseStore {
    async fn active_document(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<CanonicalDocument>, AtlasError> {
        Ok(self.state.lock().await.active.get(document_id).cloned())
    }

    async fn latest_operation(
        &self,
        document_id: &DocumentId,
        backend: Option<&str>,
    ) -> Result<Option<ParseOperation>, AtlasError> {
        Ok(self
            .state
            .lock()
            .await
            .operations
            .iter()
            .rev()
            .find(|operation| {
                &operation.document_id == document_id
                    && backend.is_none_or(|value| operation.backend == value)
            })
            .cloned())
    }

    async fn recoverable_operations(&self) -> Result<Vec<ParseOperation>, AtlasError> {
        Ok(self
            .state
            .lock()
            .await
            .operations
            .iter()
            .filter(|operation| !operation.state.is_terminal())
            .cloned()
            .collect())
    }

    async fn save_operation(&self, operation: &ParseOperation) -> Result<(), AtlasError> {
        let mut state = self.state.lock().await;
        if let Some(stored) = state
            .operations
            .iter_mut()
            .find(|stored| stored.id == operation.id)
        {
            *stored = operation.clone();
        } else {
            state.operations.push(operation.clone());
        }
        Ok(())
    }

    async fn supersede_operation(
        &self,
        operation: &ParseOperation,
        replacement: &ParseOperation,
    ) -> Result<(), AtlasError> {
        if operation.document_id != replacement.document_id
            || operation.state != ParseOperationState::Cancelled
            || replacement.state != ParseOperationState::Queued
        {
            return Err(AtlasError::invalid_input("invalid replacement operation"));
        }
        let mut state = self.state.lock().await;
        if let Some(stored) = state
            .operations
            .iter_mut()
            .find(|stored| stored.id == operation.id)
        {
            if stored.state != ParseOperationState::StatusUnknown {
                return Err(AtlasError::invalid_input("operation is no longer unknown"));
            }
            *stored = operation.clone();
        } else {
            return Err(AtlasError::not_found("operation was not found"));
        }
        state.operations.push(replacement.clone());
        Ok(())
    }

    async fn publish(&self, artifact: &PublishArtifact) -> Result<(), AtlasError> {
        if self
            .publish_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(AtlasError::storage("scripted publish failure"));
        }
        let mut state = self.state.lock().await;
        state.active.insert(
            artifact.document.document_id.clone(),
            artifact.document.clone(),
        );
        if let Some(stored) = state
            .operations
            .iter_mut()
            .find(|stored| stored.id == artifact.operation.id)
        {
            *stored = artifact.operation.clone();
        }
        Ok(())
    }
}

#[derive(Clone)]
struct OneDocumentStore {
    document: DocumentRecord,
}

#[async_trait]
impl DocumentStore for OneDocumentStore {
    async fn list(
        &self,
        _request: &DocumentListRequest,
    ) -> Result<Vec<DocumentSummary>, AtlasError> {
        Ok(vec![self.document.summary()])
    }

    async fn import(&self, _input: &DocumentImport) -> Result<StoredImport, AtlasError> {
        Err(AtlasError::internal("test store does not import"))
    }

    async fn get(&self, document_id: &DocumentId) -> Result<Option<DocumentRecord>, AtlasError> {
        Ok((&self.document.id == document_id).then(|| self.document.clone()))
    }

    async fn list_sources(&self) -> Result<Vec<DocumentRecord>, AtlasError> {
        Ok(vec![self.document.clone()])
    }

    async fn update_source(
        &self,
        _document_id: &DocumentId,
        _update: &DocumentSourceUpdate,
        _updated_at: u64,
    ) -> Result<DocumentRecord, AtlasError> {
        Err(AtlasError::internal("test store does not update"))
    }

    async fn remove(&self, _document_id: &DocumentId) -> Result<bool, AtlasError> {
        Ok(false)
    }
}

#[derive(Clone)]
struct FixedConfiguration {
    value: Option<CloudParseConfiguration>,
}

struct FailingConfiguration;

#[async_trait]
impl CloudParseConfigurationPort for FailingConfiguration {
    async fn load(&self) -> Result<Option<CloudParseConfiguration>, AtlasError> {
        Err(AtlasError::storage("keychain temporarily unavailable"))
    }
}

#[async_trait]
impl CloudParseConfigurationPort for FixedConfiguration {
    async fn load(&self) -> Result<Option<CloudParseConfiguration>, AtlasError> {
        Ok(self.value.clone())
    }
}

struct FixedLocalExtractor {
    calls: AtomicUsize,
}

impl FixedLocalExtractor {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl LocalTextExtractor for FixedLocalExtractor {
    async fn extract(&self, request: LocalExtractRequest) -> Result<CanonicalDocument, AtlasError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(canonical(
            request.document_id,
            &request.artifact_id,
            &request.source_sha256,
            "local_text",
        ))
    }
}

struct ScriptedCloudParser {
    store: Arc<MemoryParseStore>,
    statuses: Mutex<VecDeque<CloudParseStatus>>,
    upload_error: Mutex<Option<CloudParseError>>,
    archive: Vec<u8>,
    requests: AtomicUsize,
    uploads: AtomicUsize,
    status_calls: AtomicUsize,
    downloads: AtomicUsize,
    submission_was_persisted: AtomicBool,
    pause_status: AtomicBool,
    status_started: Notify,
    release_status: Notify,
    uploaded_urls: Mutex<Vec<String>>,
}

impl ScriptedCloudParser {
    fn new(
        store: Arc<MemoryParseStore>,
        statuses: impl IntoIterator<Item = CloudParseStatus>,
    ) -> Self {
        Self {
            store,
            statuses: Mutex::new(statuses.into_iter().collect()),
            upload_error: Mutex::new(None),
            archive: cloud_archive(),
            requests: AtomicUsize::new(0),
            uploads: AtomicUsize::new(0),
            status_calls: AtomicUsize::new(0),
            downloads: AtomicUsize::new(0),
            submission_was_persisted: AtomicBool::new(false),
            pause_status: AtomicBool::new(false),
            status_started: Notify::new(),
            release_status: Notify::new(),
            uploaded_urls: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl CloudParserPort for ScriptedCloudParser {
    async fn request_upload(
        &self,
        request: &CloudParseRequest,
    ) -> Result<CloudParseSubmission, CloudParseError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        Ok(CloudParseSubmission {
            batch_id: "batch-1".to_owned(),
            data_id: request.data_id.clone(),
            upload_url: "https://upload.example.test/presigned".to_owned(),
        })
    }

    async fn upload(
        &self,
        submission: &CloudParseSubmission,
        _file_path: &Path,
    ) -> Result<(), CloudParseError> {
        self.uploads.fetch_add(1, Ordering::SeqCst);
        self.uploaded_urls
            .lock()
            .await
            .push(submission.upload_url.clone());
        let persisted = self
            .store
            .state
            .lock()
            .await
            .operations
            .last()
            .is_some_and(|operation| {
                operation.batch_id.as_deref() == Some("batch-1")
                    && operation.upload_url.is_some()
                    && operation.state == ParseOperationState::Uploading
            });
        self.submission_was_persisted
            .store(persisted, Ordering::SeqCst);
        if let Some(error) = self.upload_error.lock().await.take() {
            Err(error)
        } else {
            Ok(())
        }
    }

    async fn status(
        &self,
        _request: &CloudParseRequest,
        _batch_id: &str,
    ) -> Result<CloudParseStatus, CloudParseError> {
        self.status_calls.fetch_add(1, Ordering::SeqCst);
        if self.pause_status.load(Ordering::SeqCst) {
            self.status_started.notify_one();
            self.release_status.notified().await;
        }
        Ok(self
            .statuses
            .lock()
            .await
            .pop_front()
            .unwrap_or(CloudParseStatus::Pending))
    }

    async fn download(
        &self,
        _download_url: &str,
        destination: &Path,
        _max_bytes: u64,
    ) -> Result<u64, CloudParseError> {
        self.downloads.fetch_add(1, Ordering::SeqCst);
        tokio::fs::write(destination, &self.archive)
            .await
            .expect("scripted archive should write");
        Ok(self.archive.len() as u64)
    }

    async fn cancel(
        &self,
        _request: &CloudParseRequest,
        _batch_id: &str,
    ) -> Result<CancelCapability, CloudParseError> {
        Ok(CancelCapability::Unsupported)
    }
}

struct Rig {
    _temporary: TempDir,
    store: Arc<MemoryParseStore>,
    cloud: Arc<ScriptedCloudParser>,
    local: Arc<FixedLocalExtractor>,
    module: DefaultParseModule,
    document_id: DocumentId,
}

impl Rig {
    async fn new(automatic: bool, statuses: impl IntoIterator<Item = CloudParseStatus>) -> Self {
        let temporary = TempDir::new().expect("temporary directory");
        let source = temporary.path().join("paper.pdf");
        std::fs::write(&source, b"%PDF-1.7\nsynthetic").expect("source should write");
        let metadata = std::fs::metadata(&source).expect("source metadata");
        let modified = metadata
            .modified()
            .expect("modification time")
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch");
        let bytes = std::fs::read(&source).expect("source should read");
        let document_id = DocumentId::from("document-1");
        let document = DocumentRecord {
            id: document_id.clone(),
            sha256: hex::encode(Sha256::digest(&bytes)),
            title: "Synthetic paper".to_owned(),
            authors: Vec::new(),
            page_count: Some(1),
            file_path: source.to_string_lossy().into_owned(),
            file_size_bytes: metadata.len(),
            file_mtime_ms: u64::try_from(modified.as_millis()).expect("mtime should fit"),
            file_state: DocumentFileState::Available,
            last_opened_at: 1,
        };
        let store = Arc::new(MemoryParseStore::default());
        let cloud = Arc::new(ScriptedCloudParser::new(store.clone(), statuses));
        let local = Arc::new(FixedLocalExtractor::new());
        let configuration = FixedConfiguration {
            value: Some(CloudParseConfiguration {
                profile_id: "cloud_mineru".to_owned(),
                endpoint_base_url: "https://mineru.example/api/v4".to_owned(),
                endpoint_fingerprint: "fingerprint".to_owned(),
                credential: CloudCredential::new("secret"),
                automatic,
            }),
        };
        let module = DefaultParseModule::new(
            store.clone(),
            Arc::new(OneDocumentStore { document }),
            Arc::new(configuration),
            cloud.clone(),
            local.clone(),
            temporary.path().join("artifacts"),
        )
        .with_poll_policy(ParsePollPolicy {
            initial_interval: Duration::from_millis(1),
            medium_interval: Duration::from_millis(1),
            slow_interval: Duration::from_millis(1),
            medium_after: Duration::from_millis(1),
            slow_after: Duration::from_millis(2),
            remote_timeout: Duration::from_millis(100),
        });
        Self {
            _temporary: temporary,
            store,
            cloud,
            local,
            module,
            document_id,
        }
    }

    async fn wait_for(&self, expected: ParseState) -> ParsedDocumentView {
        for _ in 0..200 {
            let view = self
                .module
                .view(&self.document_id)
                .await
                .expect("parse view should load");
            if view.parse.state == expected {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("parse did not reach {expected:?}");
    }
}

#[tokio::test]
async fn automatic_cloud_parsing_off_never_crosses_the_cloud_port() {
    let rig = Rig::new(false, []).await;

    rig.module
        .ensure(rig.document_id.clone(), "session-1".to_owned())
        .await
        .expect("parse should start");
    let view = rig.wait_for(ParseState::Degraded).await;

    assert_eq!(
        view.document.expect("local document").parser.backend,
        "local_text"
    );
    assert_eq!(rig.cloud.requests.load(Ordering::SeqCst), 0);
    assert_eq!(rig.cloud.uploads.load(Ordering::SeqCst), 0);
    assert_eq!(rig.cloud.status_calls.load(Ordering::SeqCst), 0);
    assert_eq!(rig.local.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cloud_cache_miss_persists_the_batch_before_sending_pdf_bytes() {
    let rig = Rig::new(
        true,
        [CloudParseStatus::Done {
            download_url: "https://cdn.example.test/result.zip".to_owned(),
        }],
    )
    .await;

    rig.module
        .ensure(rig.document_id.clone(), "session-1".to_owned())
        .await
        .expect("parse should start");
    let view = rig.wait_for(ParseState::Ready).await;

    assert_eq!(
        view.document.expect("cloud document").parser.backend,
        "cloud_mineru"
    );
    assert!(rig.cloud.submission_was_persisted.load(Ordering::SeqCst));
    assert_eq!(rig.cloud.requests.load(Ordering::SeqCst), 1);
    assert_eq!(rig.cloud.uploads.load(Ordering::SeqCst), 1);
    assert_eq!(rig.cloud.downloads.load(Ordering::SeqCst), 1);
    assert_eq!(rig.local.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn concurrent_ensure_calls_create_only_one_persisted_cloud_operation() {
    let rig = Rig::new(
        true,
        [CloudParseStatus::Done {
            download_url: "https://cdn.example.test/result.zip".to_owned(),
        }],
    )
    .await;

    let (first, second) = tokio::join!(
        rig.module
            .ensure(rig.document_id.clone(), "session-1".to_owned()),
        rig.module
            .ensure(rig.document_id.clone(), "session-2".to_owned()),
    );
    first.expect("first ensure should succeed");
    second.expect("second ensure should share the operation");
    rig.wait_for(ParseState::Ready).await;

    assert_eq!(rig.store.state.lock().await.operations.len(), 1);
    assert_eq!(rig.cloud.requests.load(Ordering::SeqCst), 1);
    assert_eq!(rig.cloud.uploads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ambiguous_upload_enters_status_unknown_without_a_second_upload() {
    let rig = Rig::new(true, [CloudParseStatus::Missing]).await;
    *rig.cloud.upload_error.lock().await = Some(CloudParseError::unknown_upload(
        "the upload response was lost",
    ));

    rig.module
        .ensure(rig.document_id.clone(), "session-1".to_owned())
        .await
        .expect("parse should start");
    let view = rig.wait_for(ParseState::StatusUnknown).await;

    assert!(view.document.is_none());
    assert_eq!(rig.cloud.requests.load(Ordering::SeqCst), 1);
    assert_eq!(rig.cloud.uploads.load(Ordering::SeqCst), 1);
    assert_eq!(rig.local.calls.load(Ordering::SeqCst), 0);
    let operation = rig
        .store
        .latest_operation(&rig.document_id, None)
        .await
        .expect("operation should load")
        .expect("operation should exist");
    assert_eq!(operation.batch_id.as_deref(), Some("batch-1"));
    assert_eq!(operation.state, ParseOperationState::StatusUnknown);
}

#[tokio::test]
async fn a_confirmed_batch_continues_after_the_upload_response_is_lost() {
    let rig = Rig::new(
        true,
        [
            CloudParseStatus::Pending,
            CloudParseStatus::Done {
                download_url: "https://cdn.example.test/result.zip".to_owned(),
            },
        ],
    )
    .await;
    *rig.cloud.upload_error.lock().await = Some(CloudParseError::unknown_upload(
        "the upload response was lost",
    ));

    rig.module
        .ensure(rig.document_id.clone(), "session-1".to_owned())
        .await
        .expect("parse should start");
    rig.wait_for(ParseState::Ready).await;

    assert_eq!(rig.cloud.requests.load(Ordering::SeqCst), 1);
    assert_eq!(rig.cloud.uploads.load(Ordering::SeqCst), 1);
    assert_eq!(rig.cloud.status_calls.load(Ordering::SeqCst), 2);
    assert_eq!(rig.cloud.downloads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retry_unknown_queries_the_saved_batch_and_never_reuploads() {
    let rig = Rig::new(
        true,
        [
            CloudParseStatus::Missing,
            CloudParseStatus::Done {
                download_url: "https://cdn.example.test/result.zip".to_owned(),
            },
        ],
    )
    .await;
    *rig.cloud.upload_error.lock().await = Some(CloudParseError::unknown_upload(
        "the upload response was lost",
    ));
    rig.module
        .ensure(rig.document_id.clone(), "session-1".to_owned())
        .await
        .expect("parse should start");
    rig.wait_for(ParseState::StatusUnknown).await;

    rig.cloud.pause_status.store(true, Ordering::SeqCst);
    let immediate = rig
        .module
        .retry_remote_status(&rig.document_id)
        .await
        .expect("saved batch should retry");
    assert_ne!(immediate.state, ParseState::StatusUnknown);
    tokio::time::timeout(Duration::from_secs(1), rig.cloud.status_started.notified())
        .await
        .expect("remote status query should start");
    rig.cloud.release_status.notify_one();
    rig.wait_for(ParseState::Ready).await;

    assert_eq!(rig.cloud.requests.load(Ordering::SeqCst), 1);
    assert_eq!(rig.cloud.uploads.load(Ordering::SeqCst), 1);
    assert_eq!(rig.cloud.status_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn startup_recovery_resumes_a_batch_without_allocating_or_uploading() {
    let rig = Rig::new(
        true,
        [CloudParseStatus::Done {
            download_url: "https://cdn.example.test/result.zip".to_owned(),
        }],
    )
    .await;
    let mut operation = ParseOperation::new(NewParseOperation {
        id: "operation-1".to_owned(),
        job_id: "job-1".to_owned(),
        session_id: "session-1".to_owned(),
        document_id: rig.document_id.clone(),
        provider_profile_id: Some("cloud_mineru".to_owned()),
        backend: "cloud_mineru".to_owned(),
        parser_version: CLOUD_PARSER_VERSION.to_owned(),
        normalizer_version: NORMALIZER_VERSION.to_owned(),
        endpoint_origin: Some("https://mineru.example/api/v4".to_owned()),
        endpoint_fingerprint: Some("fingerprint".to_owned()),
        data_id: rig
            .store
            .state
            .lock()
            .await
            .operations
            .first()
            .map_or_else(|| "unused".to_owned(), |value| value.data_id.clone()),
        created_at: 1,
    });
    // The real data id is the document hash; recover uses it to align results.
    operation.data_id = OneDocumentStore {
        document: rig
            .module
            .documents
            .get(&rig.document_id)
            .await
            .expect("document query")
            .expect("document exists"),
    }
    .document
    .sha256;
    operation.state = ParseOperationState::Processing;
    operation.batch_id = Some("batch-1".to_owned());
    operation.upload_url = Some("https://upload.example.test/presigned".to_owned());
    rig.store
        .save_operation(&operation)
        .await
        .expect("operation should persist");
    let stale_staging = rig
        .module
        .artifact_root
        .join(".staging")
        .join("artifact-operation-1");
    tokio::fs::create_dir_all(&stale_staging)
        .await
        .expect("stale staging should exist");
    tokio::fs::write(stale_staging.join("partial"), b"interrupted")
        .await
        .expect("stale staging should contain a partial file");

    assert_eq!(
        rig.module.recover().await.expect("recovery should start"),
        1
    );
    rig.wait_for(ParseState::Ready).await;

    assert_eq!(rig.cloud.requests.load(Ordering::SeqCst), 0);
    assert_eq!(rig.cloud.uploads.load(Ordering::SeqCst), 0);
    assert_eq!(rig.cloud.status_calls.load(Ordering::SeqCst), 1);
    assert!(
        !tokio::fs::try_exists(&stale_staging)
            .await
            .expect("staging existence should be readable")
    );
}

#[tokio::test]
async fn startup_recovery_defers_cloud_work_when_the_provider_is_temporarily_unavailable() {
    let mut rig = Rig::new(true, []).await;
    let document = rig
        .module
        .documents
        .get(&rig.document_id)
        .await
        .expect("document query")
        .expect("document exists");
    let mut operation = ParseOperation::new(NewParseOperation {
        id: "operation-1".to_owned(),
        job_id: "job-1".to_owned(),
        session_id: "session-1".to_owned(),
        document_id: rig.document_id.clone(),
        provider_profile_id: Some("cloud_mineru".to_owned()),
        backend: "cloud_mineru".to_owned(),
        parser_version: CLOUD_PARSER_VERSION.to_owned(),
        normalizer_version: NORMALIZER_VERSION.to_owned(),
        endpoint_origin: Some("https://mineru.example/api/v4".to_owned()),
        endpoint_fingerprint: Some("fingerprint".to_owned()),
        data_id: document.sha256,
        created_at: 1,
    });
    operation.state = ParseOperationState::Processing;
    operation.batch_id = Some("batch-1".to_owned());
    operation.upload_url = Some("https://upload.example.test/presigned".to_owned());
    rig.store
        .save_operation(&operation)
        .await
        .expect("operation should persist");
    rig.module.configuration = Arc::new(FixedConfiguration { value: None });

    assert_eq!(
        rig.module
            .recover()
            .await
            .expect("unavailable provider should not abort recovery"),
        0
    );

    let deferred = rig
        .store
        .latest_operation(&rig.document_id, None)
        .await
        .expect("operation should load")
        .expect("operation should remain");
    assert_eq!(deferred.state, ParseOperationState::Processing);
    assert_eq!(rig.cloud.status_calls.load(Ordering::SeqCst), 0);
    assert_eq!(rig.local.calls.load(Ordering::SeqCst), 0);

    rig.module.configuration = Arc::new(FixedConfiguration {
        value: Some(CloudParseConfiguration {
            profile_id: "cloud_mineru".to_owned(),
            endpoint_base_url: "https://mineru.example/api/v4".to_owned(),
            endpoint_fingerprint: "fingerprint".to_owned(),
            credential: CloudCredential::new("secret"),
            automatic: true,
        }),
    });
    rig.module
        .ensure(rig.document_id.clone(), "session-2".to_owned())
        .await
        .expect("ensure should resume deferred cloud work");
    tokio::time::timeout(Duration::from_secs(1), async {
        while rig.cloud.status_calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("deferred cloud operation should resume");
}

#[tokio::test]
async fn provider_lookup_failure_does_not_hide_a_cached_local_document() {
    let mut rig = Rig::new(false, []).await;
    rig.module
        .ensure(rig.document_id.clone(), "session-1".to_owned())
        .await
        .expect("local parse should start");
    rig.wait_for(ParseState::Degraded).await;
    rig.module.configuration = Arc::new(FailingConfiguration);

    let view = rig
        .module
        .view(&rig.document_id)
        .await
        .expect("cached local document should remain readable");

    assert_eq!(view.parse.state, ParseState::Degraded);
    assert!(view.document.is_some());
}

#[tokio::test]
async fn deferred_cloud_work_never_uses_credentials_for_a_different_endpoint() {
    let mut rig = Rig::new(
        true,
        [CloudParseStatus::Done {
            download_url: "https://cdn.example.test/result.zip".to_owned(),
        }],
    )
    .await;
    let document = rig
        .module
        .documents
        .get(&rig.document_id)
        .await
        .expect("document query")
        .expect("document exists");
    let mut operation = ParseOperation::new(NewParseOperation {
        id: "old-endpoint-operation".to_owned(),
        job_id: "old-endpoint-job".to_owned(),
        session_id: "session-1".to_owned(),
        document_id: rig.document_id.clone(),
        provider_profile_id: Some("cloud_mineru".to_owned()),
        backend: ParseBackend::CloudMineru.as_str().to_owned(),
        parser_version: CLOUD_PARSER_VERSION.to_owned(),
        normalizer_version: NORMALIZER_VERSION.to_owned(),
        endpoint_origin: Some("https://old-mineru.example/api/v4".to_owned()),
        endpoint_fingerprint: Some("old-fingerprint".to_owned()),
        data_id: document.sha256,
        created_at: 1,
    });
    operation.state = ParseOperationState::Processing;
    operation.batch_id = Some("old-batch".to_owned());
    rig.store
        .save_operation(&operation)
        .await
        .expect("deferred operation should persist");
    rig.module.configuration = Arc::new(FixedConfiguration {
        value: Some(CloudParseConfiguration {
            profile_id: "cloud_mineru".to_owned(),
            endpoint_base_url: "https://mineru.example/api/v4".to_owned(),
            endpoint_fingerprint: "fingerprint".to_owned(),
            credential: CloudCredential::new("secret"),
            automatic: false,
        }),
    });

    rig.module
        .ensure(rig.document_id.clone(), "session-2".to_owned())
        .await
        .expect("ensure should remain readable");
    tokio::time::sleep(Duration::from_millis(25)).await;

    assert_eq!(
        rig.cloud.status_calls.load(Ordering::SeqCst),
        0,
        "the current key must never be sent to the persisted old endpoint"
    );
    assert_eq!(
        rig.store
            .latest_operation(&rig.document_id, None)
            .await
            .expect("operation should load")
            .expect("operation should exist")
            .state,
        ParseOperationState::StatusUnknown
    );
}

#[tokio::test]
async fn recovery_and_explicit_retry_reject_a_changed_endpoint_fingerprint() {
    let rig = Rig::new(true, []).await;
    let document = rig
        .module
        .documents
        .get(&rig.document_id)
        .await
        .expect("document query")
        .expect("document exists");
    let mut operation = ParseOperation::new(NewParseOperation {
        id: "changed-endpoint-operation".to_owned(),
        job_id: "changed-endpoint-job".to_owned(),
        session_id: "session-1".to_owned(),
        document_id: rig.document_id.clone(),
        provider_profile_id: Some("cloud_mineru".to_owned()),
        backend: ParseBackend::CloudMineru.as_str().to_owned(),
        parser_version: CLOUD_PARSER_VERSION.to_owned(),
        normalizer_version: NORMALIZER_VERSION.to_owned(),
        endpoint_origin: Some("https://old-mineru.example/api/v4".to_owned()),
        endpoint_fingerprint: Some("old-fingerprint".to_owned()),
        data_id: document.sha256,
        created_at: 1,
    });
    operation.state = ParseOperationState::Processing;
    operation.batch_id = Some("old-batch".to_owned());
    rig.store
        .save_operation(&operation)
        .await
        .expect("unknown operation should persist");

    assert_eq!(
        rig.module
            .recover()
            .await
            .expect("recovery should inspect work"),
        0
    );
    assert_eq!(
        rig.store
            .latest_operation(&rig.document_id, None)
            .await
            .expect("operation should load")
            .expect("operation should exist")
            .state,
        ParseOperationState::StatusUnknown
    );
    let error = rig
        .module
        .retry_remote_status(&rig.document_id)
        .await
        .expect_err("changed endpoint must require a new upload");

    assert!(error.message.contains("settings changed"));
    assert_eq!(rig.cloud.status_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn startup_recovery_reuses_the_persisted_upload_url_when_the_batch_is_missing() {
    let rig = Rig::new(
        true,
        [
            CloudParseStatus::Missing,
            CloudParseStatus::Done {
                download_url: "https://cdn.example.test/result.zip".to_owned(),
            },
        ],
    )
    .await;
    let document = rig
        .module
        .documents
        .get(&rig.document_id)
        .await
        .expect("document query")
        .expect("document exists");
    let mut operation = ParseOperation::new(NewParseOperation {
        id: "operation-1".to_owned(),
        job_id: "job-1".to_owned(),
        session_id: "session-1".to_owned(),
        document_id: rig.document_id.clone(),
        provider_profile_id: Some("cloud_mineru".to_owned()),
        backend: "cloud_mineru".to_owned(),
        parser_version: CLOUD_PARSER_VERSION.to_owned(),
        normalizer_version: NORMALIZER_VERSION.to_owned(),
        endpoint_origin: Some("https://mineru.example/api/v4".to_owned()),
        endpoint_fingerprint: Some("fingerprint".to_owned()),
        data_id: document.sha256,
        created_at: 1,
    });
    operation.state = ParseOperationState::Uploading;
    operation.batch_id = Some("batch-1".to_owned());
    operation.upload_url = Some("https://upload.example.test/original-presigned".to_owned());
    rig.store
        .save_operation(&operation)
        .await
        .expect("operation should persist");

    assert_eq!(
        rig.module.recover().await.expect("recovery should start"),
        1
    );
    rig.wait_for(ParseState::Ready).await;

    assert_eq!(rig.cloud.requests.load(Ordering::SeqCst), 0);
    assert_eq!(rig.cloud.uploads.load(Ordering::SeqCst), 1);
    assert_eq!(
        *rig.cloud.uploaded_urls.lock().await,
        vec!["https://upload.example.test/original-presigned"]
    );
    assert_eq!(rig.cloud.status_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn explicit_reupload_cancels_the_unknown_batch_before_starting_its_replacement() {
    let rig = Rig::new(
        true,
        [
            CloudParseStatus::Missing,
            CloudParseStatus::Done {
                download_url: "https://cdn.example.test/result.zip".to_owned(),
            },
        ],
    )
    .await;
    *rig.cloud.upload_error.lock().await = Some(CloudParseError::unknown_upload(
        "the upload response was lost",
    ));
    rig.module
        .ensure(rig.document_id.clone(), "session-1".to_owned())
        .await
        .expect("parse should start");
    rig.wait_for(ParseState::StatusUnknown).await;

    rig.module
        .reupload(rig.document_id.clone(), "session-2".to_owned())
        .await
        .expect("confirmed re-upload should start");
    rig.wait_for(ParseState::Ready).await;

    let state = rig.store.state.lock().await;
    assert_eq!(state.operations.len(), 2);
    assert_eq!(state.operations[0].state, ParseOperationState::Cancelled);
    assert_eq!(
        state.operations[0].error_code.as_deref(),
        Some("superseded_by_reupload")
    );
    assert_eq!(state.operations[1].state, ParseOperationState::Succeeded);
    assert_eq!(rig.cloud.requests.load(Ordering::SeqCst), 2);
    assert_eq!(rig.cloud.uploads.load(Ordering::SeqCst), 2);
    drop(state);
    assert_eq!(
        rig.module
            .recover()
            .await
            .expect("completed replacement should leave nothing to recover"),
        0
    );
}

#[tokio::test]
async fn explicit_reupload_is_rejected_while_remote_recovery_owns_the_document() {
    let rig = Rig::new(true, [CloudParseStatus::Missing]).await;
    let document = rig
        .module
        .documents
        .get(&rig.document_id)
        .await
        .expect("document query")
        .expect("document exists");
    seed_unknown_cloud_operation(&rig, &document, "operation-unknown").await;
    assert!(
        rig.module.reserve_document(&rig.document_id).await,
        "test should own the parse slot"
    );

    let result = rig
        .module
        .reupload(rig.document_id.clone(), "session-2".to_owned())
        .await;

    assert_eq!(
        result
            .expect_err("concurrent re-upload should be rejected")
            .code,
        atlas_domain::AtlasErrorCode::InvalidInput
    );
    let state = rig.store.state.lock().await;
    assert_eq!(state.operations.len(), 1);
    assert_eq!(
        state.operations[0].state,
        ParseOperationState::StatusUnknown
    );
    drop(state);
    rig.module.in_flight.lock().await.remove(&rig.document_id);
}

#[tokio::test]
async fn explicit_reupload_remains_available_when_automatic_cloud_parsing_is_off() {
    let rig = Rig::new(
        false,
        [CloudParseStatus::Done {
            download_url: "https://cdn.example.test/result.zip".to_owned(),
        }],
    )
    .await;
    let document = rig
        .module
        .documents
        .get(&rig.document_id)
        .await
        .expect("document query")
        .expect("document exists");
    seed_unknown_cloud_operation(&rig, &document, "operation-unknown").await;

    rig.module
        .reupload(rig.document_id.clone(), "session-2".to_owned())
        .await
        .expect("manual recovery should not depend on the automatic toggle");
    rig.wait_for(ParseState::Ready).await;

    assert_eq!(rig.cloud.requests.load(Ordering::SeqCst), 1);
    assert_eq!(rig.cloud.uploads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn recovery_ignores_an_older_unknown_operation_when_a_replacement_is_queued() {
    let rig = Rig::new(
        true,
        [CloudParseStatus::Done {
            download_url: "https://cdn.example.test/result.zip".to_owned(),
        }],
    )
    .await;
    let source = rig
        .module
        .documents
        .get(&rig.document_id)
        .await
        .expect("document query")
        .expect("document exists");
    let mut old = ParseOperation::new(NewParseOperation {
        id: "operation-old".to_owned(),
        job_id: "job-old".to_owned(),
        session_id: "session-1".to_owned(),
        document_id: rig.document_id.clone(),
        provider_profile_id: Some("cloud_mineru".to_owned()),
        backend: "cloud_mineru".to_owned(),
        parser_version: CLOUD_PARSER_VERSION.to_owned(),
        normalizer_version: NORMALIZER_VERSION.to_owned(),
        endpoint_origin: Some("https://mineru.example/api/v4".to_owned()),
        endpoint_fingerprint: Some("fingerprint".to_owned()),
        data_id: source.sha256.clone(),
        created_at: 1,
    });
    old.state = ParseOperationState::StatusUnknown;
    old.batch_id = Some("batch-old".to_owned());
    old.upload_url = Some("https://upload.example.test/old".to_owned());
    let replacement = ParseOperation::new(NewParseOperation {
        id: "operation-new".to_owned(),
        job_id: "job-new".to_owned(),
        session_id: "session-2".to_owned(),
        document_id: rig.document_id.clone(),
        provider_profile_id: Some("cloud_mineru".to_owned()),
        backend: "cloud_mineru".to_owned(),
        parser_version: CLOUD_PARSER_VERSION.to_owned(),
        normalizer_version: NORMALIZER_VERSION.to_owned(),
        endpoint_origin: Some("https://mineru.example/api/v4".to_owned()),
        endpoint_fingerprint: Some("fingerprint".to_owned()),
        data_id: source.sha256,
        created_at: 2,
    });
    rig.store
        .save_operation(&old)
        .await
        .expect("old operation should persist");
    rig.store
        .save_operation(&replacement)
        .await
        .expect("replacement should persist");

    assert_eq!(
        rig.module.recover().await.expect("recovery should start"),
        1
    );
    rig.wait_for(ParseState::Ready).await;

    assert_eq!(rig.cloud.requests.load(Ordering::SeqCst), 1);
    assert_eq!(rig.cloud.uploads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn recovery_publishes_a_finalized_manifest_without_repeating_local_extraction() {
    let rig = Rig::new(false, []).await;
    let source = rig
        .module
        .documents
        .get(&rig.document_id)
        .await
        .expect("document query")
        .expect("document exists");
    let mut operation = ParseOperation::new(NewParseOperation {
        id: "operation-1".to_owned(),
        job_id: "job-1".to_owned(),
        session_id: "session-1".to_owned(),
        document_id: rig.document_id.clone(),
        provider_profile_id: None,
        backend: "local_text".to_owned(),
        parser_version: LOCAL_PARSER_VERSION.to_owned(),
        normalizer_version: LOCAL_NORMALIZER_VERSION.to_owned(),
        endpoint_origin: None,
        endpoint_fingerprint: None,
        data_id: source.sha256.clone(),
        created_at: 1,
    });
    operation.state = ParseOperationState::Normalizing;
    rig.store
        .save_operation(&operation)
        .await
        .expect("operation should persist");
    let artifact_id = "artifact-operation-1";
    let finalized = rig
        .module
        .artifact_root
        .join(rig.document_id.as_str())
        .join(artifact_id);
    tokio::fs::create_dir_all(&finalized)
        .await
        .expect("artifact directory should exist");
    let document = canonical(
        rig.document_id.clone(),
        artifact_id,
        &source.sha256,
        "local_text",
    );
    tokio::fs::write(
        finalized.join("manifest.json"),
        serde_json::to_vec(&document).expect("manifest should encode"),
    )
    .await
    .expect("manifest should write");

    assert_eq!(
        rig.module.recover().await.expect("recovery should start"),
        1
    );
    let view = rig.wait_for(ParseState::Degraded).await;

    assert_eq!(view.document, Some(document));
    assert_eq!(rig.local.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_transient_publish_failure_is_recovered_from_the_finalized_manifest() {
    let rig = Rig::new(false, []).await;
    rig.store.publish_failures.store(1, Ordering::SeqCst);

    rig.module
        .ensure(rig.document_id.clone(), "session-1".to_owned())
        .await
        .expect("parse should start");
    for _ in 0..200 {
        if rig.module.in_flight.lock().await.is_empty()
            && rig.local.calls.load(Ordering::SeqCst) == 1
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let deferred = rig
        .store
        .latest_operation(&rig.document_id, None)
        .await
        .expect("operation should load")
        .expect("operation should exist");
    assert_eq!(deferred.state, ParseOperationState::Normalizing);

    assert_eq!(
        rig.module.recover().await.expect("recovery should start"),
        1
    );
    rig.wait_for(ParseState::Degraded).await;

    assert_eq!(rig.local.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_remote_parse_failure_falls_back_to_local_text() {
    let rig = Rig::new(
        true,
        [CloudParseStatus::Failed {
            safe_message: "remote parse failed".to_owned(),
        }],
    )
    .await;

    rig.module
        .ensure(rig.document_id.clone(), "session-1".to_owned())
        .await
        .expect("parse should start");
    let view = rig.wait_for(ParseState::Degraded).await;

    assert_eq!(
        view.document.expect("fallback document").parser.backend,
        "local_text"
    );
    assert_eq!(rig.local.calls.load(Ordering::SeqCst), 1);
}

async fn seed_unknown_cloud_operation(rig: &Rig, document: &DocumentRecord, operation_id: &str) {
    let mut operation = ParseOperation::new(NewParseOperation {
        id: operation_id.to_owned(),
        job_id: format!("job-{operation_id}"),
        session_id: "session-1".to_owned(),
        document_id: rig.document_id.clone(),
        provider_profile_id: Some("cloud_mineru".to_owned()),
        backend: "cloud_mineru".to_owned(),
        parser_version: CLOUD_PARSER_VERSION.to_owned(),
        normalizer_version: NORMALIZER_VERSION.to_owned(),
        endpoint_origin: Some("https://mineru.example/api/v4".to_owned()),
        endpoint_fingerprint: Some("fingerprint".to_owned()),
        data_id: document.sha256.clone(),
        created_at: 1,
    });
    operation.state = ParseOperationState::StatusUnknown;
    operation.batch_id = Some(format!("batch-{operation_id}"));
    operation.upload_url = Some(format!("https://upload.example.test/{operation_id}"));
    rig.store
        .save_operation(&operation)
        .await
        .expect("unknown operation should persist");
}

fn canonical(
    document_id: DocumentId,
    artifact_id: &str,
    source_sha256: &str,
    backend: &str,
) -> CanonicalDocument {
    let content = StructuredContent::text("A synthetic paragraph");
    let source_digest = hex::encode(Sha256::digest(
        serde_json::to_vec(&content).expect("content should encode"),
    ));
    CanonicalDocument {
        schema_version: CANONICAL_SCHEMA_VERSION,
        artifact_id: artifact_id.to_owned(),
        document_id,
        source_sha256: source_sha256.to_owned(),
        parser: ParserIdentity {
            name: "Synthetic".to_owned(),
            version: if backend == "local_text" {
                LOCAL_PARSER_VERSION.to_owned()
            } else {
                CLOUD_PARSER_VERSION.to_owned()
            },
            backend: backend.to_owned(),
        },
        normalizer_version: if backend == "local_text" {
            LOCAL_NORMALIZER_VERSION.to_owned()
        } else {
            NORMALIZER_VERSION.to_owned()
        },
        page_count: 1,
        title: Some("Synthetic paper".to_owned()),
        chapters: vec![CanonicalChapter {
            id: ChapterId::new(format!("chapter-{artifact_id}")),
            order_index: 0,
            depth: 1,
            role: ChapterRole::Body,
            source_title: "1 Introduction".to_owned(),
            page_start: 1,
            page_end: 1,
            blocks: vec![CanonicalBlock {
                id: BlockId::new(format!("block-{artifact_id}")),
                order_index: 0,
                kind: BlockKind::Paragraph,
                page_start: 1,
                page_end: 1,
                bounding_boxes: Vec::new(),
                content,
                source_digest,
            }],
        }],
        assets: Vec::new(),
    }
}

fn cloud_archive() -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file("synthetic_content_list.json", SimpleFileOptions::default())
        .expect("content list should start");
    writer
        .write_all(
            br#"[
              {"type":"text","text":"Synthetic paper","text_level":1,"bbox":[100,100,900,150],"page_idx":0},
              {"type":"text","text":"1 Introduction","text_level":2,"bbox":[100,200,600,250],"page_idx":0},
              {"type":"text","text":"A synthetic paragraph.","bbox":[100,300,600,400],"page_idx":0}
            ]"#,
        )
        .expect("content list should write");
    writer
        .start_file("layout.json", SimpleFileOptions::default())
        .expect("layout should start");
    writer
        .write_all(br#"{"pdf_info":[{"page_idx":0,"page_size":[612,792]}]}"#)
        .expect("layout should write");
    writer.finish().expect("zip should finish").into_inner()
}
