use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use atlas_domain::{
    AtlasError, AtlasErrorCode, CanonicalDocument, DocumentFileState, DocumentId, ParseBackend,
    ParseSnapshot, ParseState, ParsedDocumentView,
};
use atlas_library::{DocumentRecord, DocumentStore};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};
use uuid::Uuid;

use crate::identity::digest;
use crate::{
    ArchiveLimits, CloudCredential, CloudParseErrorKind, CloudParseRequest, CloudParseStatus,
    CloudParseSubmission, CloudParserPort, LocalExtractRequest, LocalTextExtractor,
    MineruArchiveUnpacker, MineruAssetInput, MineruDocumentInput, MineruNormalizer,
    NORMALIZER_VERSION, NewParseOperation, ParseOperation, ParseOperationState, ParseStore,
    PublishArtifact,
    local::{LOCAL_NORMALIZER_VERSION, LOCAL_PARSER_VERSION},
};

const CLOUD_PARSER_VERSION: &str = "mineru-v4-vlm";
const MAX_DOWNLOAD_BYTES: u64 = 1_000_000_000;

#[derive(Clone, Debug)]
pub struct CloudParseConfiguration {
    pub profile_id: String,
    pub endpoint_base_url: String,
    pub endpoint_fingerprint: String,
    pub credential: CloudCredential,
    pub automatic: bool,
}

#[async_trait]
pub trait CloudParseConfigurationPort: Send + Sync {
    async fn load(&self) -> Result<Option<CloudParseConfiguration>, AtlasError>;
}

#[derive(Clone, Copy, Debug)]
pub struct ParsePollPolicy {
    pub initial_interval: Duration,
    pub medium_interval: Duration,
    pub slow_interval: Duration,
    pub medium_after: Duration,
    pub slow_after: Duration,
    pub remote_timeout: Duration,
}

impl Default for ParsePollPolicy {
    fn default() -> Self {
        Self {
            initial_interval: Duration::from_secs(2),
            medium_interval: Duration::from_secs(5),
            slow_interval: Duration::from_secs(10),
            medium_after: Duration::from_secs(30),
            slow_after: Duration::from_secs(120),
            remote_timeout: Duration::from_secs(600),
        }
    }
}

impl ParsePollPolicy {
    fn interval(self, elapsed: Duration) -> Duration {
        if elapsed >= self.slow_after {
            self.slow_interval
        } else if elapsed >= self.medium_after {
            self.medium_interval
        } else {
            self.initial_interval
        }
    }
}

#[async_trait]
pub trait ParseModule: Send + Sync {
    /// Returns cached content immediately. On a cache miss, persists a parse job
    /// before starting it in the background and returns its initial snapshot.
    async fn ensure(
        &self,
        document_id: DocumentId,
        session_id: String,
    ) -> Result<ParsedDocumentView, AtlasError>;

    async fn view(&self, document_id: &DocumentId) -> Result<ParsedDocumentView, AtlasError>;

    /// Queries a persisted remote batch again without uploading another PDF.
    async fn retry_remote_status(
        &self,
        document_id: &DocumentId,
    ) -> Result<ParseSnapshot, AtlasError>;

    /// Explicit duplicate-cost protection gate. This is the only operation that
    /// abandons an unknown remote batch and requests a fresh upload.
    async fn reupload(
        &self,
        document_id: DocumentId,
        session_id: String,
    ) -> Result<ParseSnapshot, AtlasError>;

    /// Restarts persisted nonterminal jobs. No source is uploaded merely because
    /// the app restarted: an operation that had reached upload first queries its
    /// saved batch id.
    async fn recover(&self) -> Result<usize, AtlasError>;
}

#[derive(Clone)]
pub struct DefaultParseModule {
    store: Arc<dyn ParseStore>,
    documents: Arc<dyn DocumentStore>,
    configuration: Arc<dyn CloudParseConfigurationPort>,
    cloud: Arc<dyn CloudParserPort>,
    local: Arc<dyn LocalTextExtractor>,
    artifact_root: PathBuf,
    in_flight: Arc<Mutex<HashSet<DocumentId>>>,
    poll_policy: ParsePollPolicy,
}

impl std::fmt::Debug for DefaultParseModule {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DefaultParseModule")
            .field("artifact_root", &self.artifact_root)
            .finish_non_exhaustive()
    }
}

impl DefaultParseModule {
    #[must_use]
    pub fn new(
        store: Arc<dyn ParseStore>,
        documents: Arc<dyn DocumentStore>,
        configuration: Arc<dyn CloudParseConfigurationPort>,
        cloud: Arc<dyn CloudParserPort>,
        local: Arc<dyn LocalTextExtractor>,
        artifact_root: PathBuf,
    ) -> Self {
        Self {
            store,
            documents,
            configuration,
            cloud,
            local,
            artifact_root,
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            poll_policy: ParsePollPolicy::default(),
        }
    }

    #[must_use]
    pub fn with_poll_policy(mut self, poll_policy: ParsePollPolicy) -> Self {
        self.poll_policy = poll_policy;
        self
    }

    async fn available_configuration(&self) -> Option<CloudParseConfiguration> {
        // ProviderStatusPort exposes the Keychain failure. Parsing treats it as
        // cloud-unavailable so cached and local content remain readable.
        let Ok(configuration) = self.configuration.load().await else {
            return None;
        };
        configuration
    }

    fn build_operation(
        &self,
        document: &DocumentRecord,
        session_id: String,
        configuration: Option<&CloudParseConfiguration>,
    ) -> Result<ParseOperation, AtlasError> {
        let created_at = now_ms()?;
        let backend = if configuration.is_some() {
            ParseBackend::CloudMineru
        } else {
            ParseBackend::LocalText
        };
        Ok(ParseOperation::new(NewParseOperation {
            id: Uuid::new_v4().to_string(),
            job_id: Uuid::new_v4().to_string(),
            session_id,
            document_id: document.id.clone(),
            provider_profile_id: configuration.map(|value| value.profile_id.clone()),
            backend: backend.as_str().to_owned(),
            parser_version: match backend {
                ParseBackend::CloudMineru => CLOUD_PARSER_VERSION.to_owned(),
                ParseBackend::LocalText => LOCAL_PARSER_VERSION.to_owned(),
            },
            normalizer_version: match backend {
                ParseBackend::CloudMineru => NORMALIZER_VERSION.to_owned(),
                ParseBackend::LocalText => LOCAL_NORMALIZER_VERSION.to_owned(),
            },
            endpoint_origin: configuration.map(|value| value.endpoint_base_url.clone()),
            endpoint_fingerprint: configuration.map(|value| value.endpoint_fingerprint.clone()),
            data_id: document.sha256.clone(),
            created_at,
        }))
    }

    async fn make_operation(
        &self,
        document: &DocumentRecord,
        session_id: String,
        configuration: Option<&CloudParseConfiguration>,
    ) -> Result<ParseOperation, AtlasError> {
        let operation = self.build_operation(document, session_id, configuration)?;
        self.store.save_operation(&operation).await?;
        Ok(operation)
    }

    async fn start(
        &self,
        operation: ParseOperation,
        configuration: Option<CloudParseConfiguration>,
    ) -> bool {
        let document_id = operation.document_id.clone();
        if !self.reserve_document(&document_id).await {
            return false;
        }
        self.spawn_reserved(operation, configuration);
        true
    }

    async fn reserve_document(&self, document_id: &DocumentId) -> bool {
        self.in_flight.lock().await.insert(document_id.clone())
    }

    fn spawn_reserved(
        &self,
        operation: ParseOperation,
        configuration: Option<CloudParseConfiguration>,
    ) {
        let document_id = operation.document_id.clone();
        let module = self.clone();
        tokio::spawn(async move {
            module.execute(operation, configuration).await;
            module.in_flight.lock().await.remove(&document_id);
        });
    }

    async fn execute(
        &self,
        mut operation: ParseOperation,
        configuration: Option<CloudParseConfiguration>,
    ) {
        let document = match self.documents.get(&operation.document_id).await {
            Ok(Some(document)) => document,
            Ok(None) => {
                self.fail_operation(&mut operation, "document_missing", "The paper was removed")
                    .await;
                return;
            }
            Err(error) => {
                self.fail_operation(&mut operation, "storage_unavailable", &error.message)
                    .await;
                return;
            }
        };

        let result = match self
            .resume_finalized_artifact(&mut operation, &document)
            .await
        {
            Ok(true) => return,
            Ok(false) if operation.backend == ParseBackend::CloudMineru.as_str() => {
                match configuration {
                    Some(configuration) => {
                        self.execute_cloud(&mut operation, &document, &configuration)
                            .await
                    }
                    None => Err(OperationFailure::failed(
                        "provider_not_configured",
                        "Cloud MinerU is no longer configured",
                    )),
                }
            }
            Ok(false) => self.execute_local(&mut operation, &document).await,
            Err(failure) => Err(failure),
        };

        if let Err(failure) = result {
            match failure.disposition {
                FailureDisposition::StatusUnknown => {
                    self.unknown_operation(&mut operation, &failure.code, &failure.message)
                        .await;
                }
                FailureDisposition::RetryOnRestart => {
                    self.defer_operation(&mut operation, &failure.code, &failure.message)
                        .await;
                }
                FailureDisposition::Failed => {
                    self.fail_operation(&mut operation, &failure.code, &failure.message)
                        .await;
                    if operation.backend == ParseBackend::CloudMineru.as_str() {
                        self.run_local_fallback(&document, operation.session_id.clone())
                            .await;
                    }
                }
            }
            self.cleanup_temporary_files(&operation).await;
        }
    }

    async fn execute_cloud(
        &self,
        operation: &mut ParseOperation,
        document: &DocumentRecord,
        configuration: &CloudParseConfiguration,
    ) -> Result<(), OperationFailure> {
        if !configuration_matches_operation(operation, configuration) {
            return Err(OperationFailure::failed(
                "provider_settings_changed",
                "Cloud MinerU settings changed before this operation could resume",
            ));
        }
        validate_source(document)
            .await
            .map_err(OperationFailure::from_atlas)?;
        let source_path = PathBuf::from(&document.file_path);
        let file_name = source_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                OperationFailure::failed("source_unreadable", "PDF filename is invalid")
            })?
            .to_owned();
        let request = CloudParseRequest {
            document_id: document.id.clone(),
            data_id: operation.data_id.clone(),
            file_name,
            file_path: source_path.clone(),
            file_size_bytes: document.file_size_bytes,
            endpoint_base_url: operation
                .endpoint_origin
                .clone()
                .unwrap_or_else(|| configuration.endpoint_base_url.clone()),
            credential: configuration.credential.clone(),
            language: "en".to_owned(),
            model_version: "vlm".to_owned(),
        };

        let mut resumed_after_upload = operation.batch_id.is_some();
        let mut completed_download_url = None;
        if operation.batch_id.is_none() {
            let submission = self
                .cloud
                .request_upload(&request)
                .await
                .map_err(OperationFailure::from_cloud)?;
            if submission.data_id != operation.data_id {
                return Err(OperationFailure::failed(
                    "protocol_incompatible",
                    "Cloud MinerU changed Atlas's document correlation id",
                ));
            }
            operation.batch_id = Some(submission.batch_id.clone());
            operation.upload_url = Some(submission.upload_url.clone());
            operation.state = ParseOperationState::Uploading;
            operation.progress = Some(0.0);
            operation.updated_at = now_ms().map_err(OperationFailure::from_atlas)?;
            self.store
                .save_operation(operation)
                .await
                .map_err(OperationFailure::from_atlas)?;

            if let Err(error) = self.cloud.upload(&submission, &source_path).await {
                completed_download_url = self
                    .resolve_upload_error(operation, &request, error)
                    .await?;
            }
            operation.state = ParseOperationState::Processing;
            operation.progress = Some(0.0);
            operation.updated_at = now_ms().map_err(OperationFailure::from_atlas)?;
            self.store
                .save_operation(operation)
                .await
                .map_err(OperationFailure::from_atlas)?;
        } else if operation.state == ParseOperationState::Uploading {
            let batch_id = operation.batch_id.as_deref().ok_or_else(|| {
                OperationFailure::failed("protocol_incompatible", "batch id is missing")
            })?;
            match self
                .cloud
                .status(&request, batch_id)
                .await
                .map_err(OperationFailure::from_resume_status)?
            {
                CloudParseStatus::Missing => {
                    let submission = CloudParseSubmission {
                        batch_id: batch_id.to_owned(),
                        data_id: operation.data_id.clone(),
                        upload_url: operation.upload_url.clone().ok_or_else(|| {
                            OperationFailure::failed(
                                "protocol_incompatible",
                                "persisted upload URL is missing",
                            )
                        })?,
                    };
                    if let Err(error) = self.cloud.upload(&submission, &source_path).await {
                        completed_download_url = self
                            .resolve_upload_error(operation, &request, error)
                            .await?;
                    }
                    resumed_after_upload = false;
                    operation.state = ParseOperationState::Processing;
                    operation.progress = Some(0.0);
                    operation.updated_at = now_ms().map_err(OperationFailure::from_atlas)?;
                    self.store
                        .save_operation(operation)
                        .await
                        .map_err(OperationFailure::from_atlas)?;
                }
                CloudParseStatus::Done { download_url } => {
                    completed_download_url = Some(download_url);
                }
                CloudParseStatus::Failed { safe_message } => {
                    return Err(OperationFailure::failed("remote_failed", safe_message));
                }
                CloudParseStatus::Pending | CloudParseStatus::Running(_) => {}
            }
        }

        let download_url = match completed_download_url {
            Some(download_url) => download_url,
            None => {
                self.poll_until_complete(operation, &request, resumed_after_upload)
                    .await?
            }
        };
        operation.download_url = Some(download_url.clone());
        operation.state = ParseOperationState::Downloading;
        operation.progress = Some(1.0);
        operation.updated_at = now_ms().map_err(OperationFailure::from_atlas)?;
        self.store
            .save_operation(operation)
            .await
            .map_err(OperationFailure::from_atlas)?;

        let download_dir = self.artifact_root.join(".downloads");
        fs::create_dir_all(&download_dir)
            .await
            .map_err(|error| OperationFailure::storage(error.to_string()))?;
        let archive_path = download_dir.join(format!("{}.zip", operation.id));
        let max_bytes = document
            .file_size_bytes
            .saturating_mul(10)
            .min(MAX_DOWNLOAD_BYTES);
        self.cloud
            .download(&download_url, &archive_path, max_bytes)
            .await
            .map_err(OperationFailure::from_cloud)?;

        operation.state = ParseOperationState::Normalizing;
        operation.updated_at = now_ms().map_err(OperationFailure::from_atlas)?;
        self.store
            .save_operation(operation)
            .await
            .map_err(OperationFailure::from_atlas)?;

        let artifact_id = format!("artifact-{}", operation.id);
        let staging = self.artifact_root.join(".staging").join(&artifact_id);
        if fs::try_exists(&staging)
            .await
            .map_err(|error| OperationFailure::storage(error.to_string()))?
        {
            fs::remove_dir_all(&staging)
                .await
                .map_err(|error| OperationFailure::storage(error.to_string()))?;
        }
        let unpacker =
            MineruArchiveUnpacker::new(ArchiveLimits::for_source_size(document.file_size_bytes));
        let archive_for_unpack = archive_path.clone();
        let staging_for_unpack = staging.clone();
        let extracted = tokio::task::spawn_blocking(move || {
            unpacker.unpack_file(&archive_for_unpack, &staging_for_unpack)
        })
        .await
        .map_err(|error| OperationFailure::failed("invalid_artifact", error.to_string()))?
        .map_err(OperationFailure::from_atlas)?;
        let _ = fs::remove_file(&archive_path).await;

        let content_list = fs::read(&extracted.content_list_path)
            .await
            .map_err(|error| OperationFailure::storage(error.to_string()))?;
        let layout = fs::read(&extracted.layout_path)
            .await
            .map_err(|error| OperationFailure::storage(error.to_string()))?;
        let assets = extracted
            .assets
            .iter()
            .map(|asset| MineruAssetInput {
                relative_path: asset.relative_path.clone(),
                sha256: asset.sha256.clone(),
                mime_type: asset.mime_type,
                size_bytes: asset.size_bytes,
            })
            .collect::<Vec<_>>();
        let canonical = MineruNormalizer::new()
            .normalize(MineruDocumentInput {
                document_id: &document.id,
                artifact_id: &artifact_id,
                source_sha256: &document.sha256,
                parser_version: &operation.parser_version,
                content_list_json: &content_list,
                layout_json: &layout,
                assets: &assets,
            })
            .map_err(OperationFailure::from_atlas)?;
        self.publish_document(operation, artifact_id, canonical, staging)
            .await
    }

    async fn resolve_upload_error(
        &self,
        operation: &ParseOperation,
        request: &CloudParseRequest,
        upload_error: crate::CloudParseError,
    ) -> Result<Option<String>, OperationFailure> {
        let batch_id = operation
            .batch_id
            .as_deref()
            .ok_or_else(|| OperationFailure::from_cloud(upload_error.clone()))?;
        match self.cloud.status(request, batch_id).await {
            Ok(CloudParseStatus::Missing) => Err(OperationFailure::unknown(
                "upload_status_unknown",
                "Cloud MinerU did not confirm whether the PDF upload completed",
            )),
            Ok(CloudParseStatus::Done { download_url }) => Ok(Some(download_url)),
            Ok(CloudParseStatus::Failed { safe_message }) => {
                Err(OperationFailure::failed("remote_failed", safe_message))
            }
            Ok(CloudParseStatus::Pending | CloudParseStatus::Running(_)) => Ok(None),
            Err(error) if error.kind == CloudParseErrorKind::Unauthorized => {
                Err(OperationFailure::from_cloud(error))
            }
            Err(_) => Err(OperationFailure::unknown(
                "upload_status_unknown",
                "Atlas could not confirm whether Cloud MinerU received the PDF",
            )),
        }
    }

    async fn poll_until_complete(
        &self,
        operation: &mut ParseOperation,
        request: &CloudParseRequest,
        resumed_after_upload: bool,
    ) -> Result<String, OperationFailure> {
        let batch_id = operation
            .batch_id
            .as_deref()
            .ok_or_else(|| {
                OperationFailure::failed("protocol_incompatible", "batch id is missing")
            })?
            .to_owned();
        let started = Instant::now();
        loop {
            let status = self
                .cloud
                .status(request, &batch_id)
                .await
                .map_err(|error| {
                    if error.kind == CloudParseErrorKind::Unauthorized {
                        OperationFailure::from_cloud(error)
                    } else {
                        OperationFailure::unknown(
                            "remote_status_unavailable",
                            "Cloud MinerU status is temporarily unavailable",
                        )
                    }
                })?;
            match status {
                CloudParseStatus::Done { download_url } => return Ok(download_url),
                CloudParseStatus::Failed { safe_message } => {
                    return Err(OperationFailure::failed("remote_failed", safe_message));
                }
                CloudParseStatus::Missing
                    if resumed_after_upload
                        || operation.state == ParseOperationState::StatusUnknown =>
                {
                    return Err(OperationFailure::unknown(
                        "remote_job_not_found",
                        "Cloud MinerU has not confirmed the persisted upload",
                    ));
                }
                CloudParseStatus::Running(progress) => {
                    operation.progress = progress.ratio();
                    operation.remote_status_json = Some(
                        json!({
                            "state": "running",
                            "extractedPages": progress.extracted_pages,
                            "totalPages": progress.total_pages
                        })
                        .to_string(),
                    );
                }
                CloudParseStatus::Pending | CloudParseStatus::Missing => {
                    operation.progress = Some(0.0);
                    operation.remote_status_json = Some(r#"{"state":"pending"}"#.to_owned());
                }
            }
            operation.state = ParseOperationState::Processing;
            operation.updated_at = now_ms().map_err(OperationFailure::from_atlas)?;
            self.store
                .save_operation(operation)
                .await
                .map_err(OperationFailure::from_atlas)?;
            if started.elapsed() >= self.poll_policy.remote_timeout {
                return Err(OperationFailure::unknown(
                    "remote_timeout",
                    "Cloud MinerU is still processing; Atlas will resume this job later",
                ));
            }
            tokio::time::sleep(self.poll_policy.interval(started.elapsed())).await;
        }
    }

    async fn execute_local(
        &self,
        operation: &mut ParseOperation,
        document: &DocumentRecord,
    ) -> Result<(), OperationFailure> {
        validate_source(document)
            .await
            .map_err(OperationFailure::from_atlas)?;
        operation.state = ParseOperationState::Normalizing;
        operation.progress = Some(0.0);
        operation.updated_at = now_ms().map_err(OperationFailure::from_atlas)?;
        self.store
            .save_operation(operation)
            .await
            .map_err(OperationFailure::from_atlas)?;

        let artifact_id = format!("artifact-{}", operation.id);
        let canonical = self
            .local
            .extract(LocalExtractRequest {
                document_id: document.id.clone(),
                artifact_id: artifact_id.clone(),
                source_sha256: document.sha256.clone(),
                source_path: PathBuf::from(&document.file_path),
                document_title: document.title.clone(),
            })
            .await
            .map_err(OperationFailure::from_atlas)?;
        let staging = self.artifact_root.join(".staging").join(&artifact_id);
        fs::create_dir_all(&staging)
            .await
            .map_err(|error| OperationFailure::storage(error.to_string()))?;
        self.publish_document(operation, artifact_id, canonical, staging)
            .await
    }

    async fn publish_document(
        &self,
        operation: &mut ParseOperation,
        artifact_id: String,
        canonical: CanonicalDocument,
        staging: PathBuf,
    ) -> Result<(), OperationFailure> {
        let manifest = serde_json::to_vec(&canonical)
            .map_err(|error| OperationFailure::storage(error.to_string()))?;
        let content_digest = digest(&manifest);
        let manifest_path = staging.join("manifest.json");
        let mut file = fs::File::create(&manifest_path)
            .await
            .map_err(|error| OperationFailure::storage(error.to_string()))?;
        file.write_all(&manifest)
            .await
            .map_err(|error| OperationFailure::storage(error.to_string()))?;
        file.sync_all()
            .await
            .map_err(|error| OperationFailure::storage(error.to_string()))?;
        drop(file);

        let document_dir = self.artifact_root.join(operation.document_id.as_str());
        fs::create_dir_all(&document_dir)
            .await
            .map_err(|error| OperationFailure::storage(error.to_string()))?;
        let final_dir = document_dir.join(&artifact_id);
        fs::rename(&staging, &final_dir)
            .await
            .map_err(|error| OperationFailure::storage(error.to_string()))?;

        self.commit_artifact(operation, artifact_id, canonical, content_digest)
            .await
    }

    async fn resume_finalized_artifact(
        &self,
        operation: &mut ParseOperation,
        source: &DocumentRecord,
    ) -> Result<bool, OperationFailure> {
        let artifact_id = format!("artifact-{}", operation.id);
        let final_dir = self
            .artifact_root
            .join(operation.document_id.as_str())
            .join(&artifact_id);
        if !fs::try_exists(&final_dir)
            .await
            .map_err(|error| OperationFailure::storage(error.to_string()))?
        {
            return Ok(false);
        }
        let manifest_path = final_dir.join("manifest.json");
        let bytes = match fs::read(&manifest_path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::remove_dir_all(&final_dir)
                    .await
                    .map_err(|error| OperationFailure::storage(error.to_string()))?;
                return Ok(false);
            }
            Err(error) => return Err(OperationFailure::storage(error.to_string())),
        };
        let canonical = match serde_json::from_slice::<CanonicalDocument>(&bytes) {
            Ok(canonical)
                if canonical.artifact_id == artifact_id
                    && canonical.document_id == operation.document_id
                    && canonical.source_sha256 == source.sha256
                    && canonical.parser.backend == operation.backend
                    && canonical.parser.version == operation.parser_version
                    && canonical.normalizer_version == operation.normalizer_version =>
            {
                canonical
            }
            Ok(_) | Err(_) => {
                fs::remove_dir_all(&final_dir)
                    .await
                    .map_err(|error| OperationFailure::storage(error.to_string()))?;
                return Ok(false);
            }
        };
        let content_digest = digest(&bytes);
        self.commit_artifact(operation, artifact_id, canonical, content_digest)
            .await?;
        Ok(true)
    }

    async fn commit_artifact(
        &self,
        operation: &mut ParseOperation,
        artifact_id: String,
        canonical: CanonicalDocument,
        content_digest: String,
    ) -> Result<(), OperationFailure> {
        let created_at = now_ms().map_err(OperationFailure::from_atlas)?;
        let mut completed = operation.clone();
        completed.state = ParseOperationState::Succeeded;
        completed.progress = Some(1.0);
        completed.error_code = None;
        completed.error_safe_json = None;
        completed.updated_at = created_at;
        completed.completed_at = Some(created_at);
        let relative_manifest = Path::new(operation.document_id.as_str())
            .join(&artifact_id)
            .join("manifest.json")
            .to_string_lossy()
            .into_owned();
        self.store
            .publish(&PublishArtifact {
                id: artifact_id,
                operation: completed.clone(),
                document: canonical,
                manifest_relative_path: relative_manifest,
                content_digest,
                created_at,
            })
            .await
            .map_err(OperationFailure::from_atlas)?;
        *operation = completed;
        Ok(())
    }

    async fn run_local_fallback(&self, document: &DocumentRecord, session_id: String) {
        let Ok(mut operation) = self.make_operation(document, session_id, None).await else {
            return;
        };
        if let Err(failure) = self.execute_local(&mut operation, document).await {
            if failure.disposition == FailureDisposition::RetryOnRestart {
                self.defer_operation(&mut operation, &failure.code, &failure.message)
                    .await;
            } else {
                self.fail_operation(&mut operation, &failure.code, &failure.message)
                    .await;
            }
            self.cleanup_temporary_files(&operation).await;
        }
    }

    async fn fail_operation(&self, operation: &mut ParseOperation, code: &str, message: &str) {
        operation.state = ParseOperationState::Failed;
        operation.progress = None;
        operation.error_code = Some(code.to_owned());
        operation.error_safe_json = Some(json!({ "message": message }).to_string());
        if let Ok(now) = now_ms() {
            operation.updated_at = now;
            operation.completed_at = Some(now);
        }
        let _ = self.store.save_operation(operation).await;
    }

    async fn unknown_operation(&self, operation: &mut ParseOperation, code: &str, message: &str) {
        operation.state = ParseOperationState::StatusUnknown;
        operation.progress = None;
        operation.error_code = Some(code.to_owned());
        operation.error_safe_json = Some(json!({ "message": message }).to_string());
        if let Ok(now) = now_ms() {
            operation.updated_at = now;
        }
        let _ = self.store.save_operation(operation).await;
    }

    async fn defer_operation(&self, operation: &mut ParseOperation, code: &str, message: &str) {
        operation.error_code = Some(code.to_owned());
        operation.error_safe_json = Some(json!({ "message": message }).to_string());
        if let Ok(now) = now_ms() {
            operation.updated_at = now;
        }
        let _ = self.store.save_operation(operation).await;
    }

    async fn cleanup_temporary_files(&self, operation: &ParseOperation) {
        let archive = self
            .artifact_root
            .join(".downloads")
            .join(format!("{}.zip", operation.id));
        let staging = self
            .artifact_root
            .join(".staging")
            .join(format!("artifact-{}", operation.id));
        let _ = fs::remove_file(archive).await;
        let _ = fs::remove_dir_all(staging).await;
    }

    async fn snapshot_for(
        &self,
        document_id: &DocumentId,
        automatic: bool,
    ) -> Result<ParsedDocumentView, AtlasError> {
        let document = self.store.active_document(document_id).await?;
        let operation = self.store.latest_operation(document_id, None).await?;
        let parse = match (&document, operation.as_ref()) {
            (_, Some(operation)) if !operation.state.is_terminal() => {
                operation_snapshot(operation, automatic)
            }
            (Some(document), _) => ParseSnapshot {
                state: if document.parser.backend == ParseBackend::CloudMineru.as_str() {
                    ParseState::Ready
                } else {
                    ParseState::Degraded
                },
                backend: ParseBackend::parse(&document.parser.backend),
                progress: Some(1.0),
                parse_operation_id: operation.map(|value| value.id),
                automatic_cloud_parsing_enabled: automatic,
                safe_message: (document.parser.backend == ParseBackend::LocalText.as_str()).then(
                    || "Using the PDF text layer; tables and formulas may be incomplete".to_owned(),
                ),
            },
            (None, Some(operation)) => operation_snapshot(operation, automatic),
            (None, None) => ParseSnapshot {
                automatic_cloud_parsing_enabled: automatic,
                ..ParseSnapshot::default()
            },
        };
        Ok(ParsedDocumentView { parse, document })
    }
}

#[async_trait]
impl ParseModule for DefaultParseModule {
    async fn ensure(
        &self,
        document_id: DocumentId,
        session_id: String,
    ) -> Result<ParsedDocumentView, AtlasError> {
        let available_configuration = self.available_configuration().await;
        let automatic_configuration = available_configuration
            .clone()
            .filter(|configuration| configuration.automatic);
        let automatic = automatic_configuration.is_some();
        let mut in_flight = self.in_flight.lock().await;
        if !in_flight.insert(document_id.clone()) {
            drop(in_flight);
            return self.snapshot_for(&document_id, automatic).await;
        }
        let preparation: Result<Option<ParseOperation>, AtlasError> = async {
            let active = self.store.active_document(&document_id).await?;
            if active.as_ref().is_some_and(|document| {
                document.parser.backend == ParseBackend::CloudMineru.as_str()
            }) {
                return Ok(None);
            }
            if let Some(latest) = self.store.latest_operation(&document_id, None).await?
                && !latest.state.is_terminal()
            {
                if latest.backend == ParseBackend::CloudMineru.as_str()
                    && available_configuration
                        .as_ref()
                        .is_some_and(|configuration| {
                            !configuration_matches_operation(&latest, configuration)
                        })
                {
                    let mut changed = latest;
                    mark_provider_settings_changed(&mut changed)?;
                    self.store.save_operation(&changed).await?;
                    return Ok(None);
                }
                let resumable = latest.state != ParseOperationState::StatusUnknown
                    && (latest.backend == ParseBackend::LocalText.as_str()
                        || automatic_configuration
                            .as_ref()
                            .is_some_and(|configuration| {
                                configuration_matches_operation(&latest, configuration)
                            }));
                return Ok(resumable.then_some(latest));
            }
            if automatic
                && self
                    .store
                    .latest_operation(&document_id, Some(ParseBackend::CloudMineru.as_str()))
                    .await?
                    .is_some_and(|cloud| {
                        matches!(
                            cloud.state,
                            ParseOperationState::Failed | ParseOperationState::StatusUnknown
                        )
                    })
            {
                return Ok(None);
            }
            if active.is_some() && !automatic {
                return Ok(None);
            }
            let document = self
                .documents
                .get(&document_id)
                .await?
                .ok_or_else(|| AtlasError::not_found("document was not found"))?;
            let operation =
                self.build_operation(&document, session_id, automatic_configuration.as_ref())?;
            self.store.save_operation(&operation).await?;
            Ok(Some(operation))
        }
        .await;
        let operation = match preparation {
            Ok(Some(operation)) => operation,
            Ok(None) => {
                in_flight.remove(&document_id);
                drop(in_flight);
                return self.snapshot_for(&document_id, automatic).await;
            }
            Err(error) => {
                in_flight.remove(&document_id);
                return Err(error);
            }
        };
        drop(in_flight);
        self.spawn_reserved(operation, automatic_configuration);
        self.snapshot_for(&document_id, automatic).await
    }

    async fn view(&self, document_id: &DocumentId) -> Result<ParsedDocumentView, AtlasError> {
        let automatic = self
            .available_configuration()
            .await
            .is_some_and(|configuration| configuration.automatic);
        self.snapshot_for(document_id, automatic).await
    }

    async fn retry_remote_status(
        &self,
        document_id: &DocumentId,
    ) -> Result<ParseSnapshot, AtlasError> {
        let mut operation = self
            .store
            .latest_operation(document_id, Some(ParseBackend::CloudMineru.as_str()))
            .await?
            .ok_or_else(|| AtlasError::not_found("no Cloud MinerU operation exists"))?;
        if operation.state != ParseOperationState::StatusUnknown || operation.batch_id.is_none() {
            return Err(AtlasError::invalid_input(
                "only an unknown remote operation with a batch id can be queried",
            ));
        }
        let configuration = self
            .configuration
            .load()
            .await?
            .ok_or_else(|| AtlasError::invalid_input("Cloud MinerU is not configured"))?;
        if !configuration_matches_operation(&operation, &configuration) {
            return Err(AtlasError::invalid_input(
                "Cloud MinerU settings changed; confirm a new upload instead",
            ));
        }
        let resumed_at = now_ms()?;
        if !self.reserve_document(document_id).await {
            return Err(AtlasError::invalid_input(
                "a parse recovery is already running for this paper",
            ));
        }
        operation.state = ParseOperationState::Processing;
        operation.progress = Some(0.0);
        operation.error_code = None;
        operation.error_safe_json = None;
        operation.updated_at = resumed_at;
        operation.completed_at = None;
        if let Err(error) = self.store.save_operation(&operation).await {
            self.in_flight.lock().await.remove(document_id);
            return Err(error);
        }
        self.spawn_reserved(operation, Some(configuration));
        Ok(self.view(document_id).await?.parse)
    }

    async fn reupload(
        &self,
        document_id: DocumentId,
        session_id: String,
    ) -> Result<ParseSnapshot, AtlasError> {
        let mut superseded = self
            .store
            .latest_operation(&document_id, Some(ParseBackend::CloudMineru.as_str()))
            .await?
            .ok_or_else(|| AtlasError::not_found("no Cloud MinerU operation exists"))?;
        if superseded.state != ParseOperationState::StatusUnknown {
            return Err(AtlasError::invalid_input(
                "only an unknown Cloud MinerU operation can be re-uploaded",
            ));
        }
        let configuration = self
            .configuration
            .load()
            .await?
            .ok_or_else(|| AtlasError::invalid_input("Cloud MinerU is not configured"))?;
        let document = self
            .documents
            .get(&document_id)
            .await?
            .ok_or_else(|| AtlasError::not_found("document was not found"))?;
        let operation = self.build_operation(&document, session_id, Some(&configuration))?;
        let superseded_at = now_ms()?;
        superseded.state = ParseOperationState::Cancelled;
        superseded.progress = None;
        superseded.error_code = Some("superseded_by_reupload".to_owned());
        superseded.error_safe_json =
            Some(r#"{"message":"Replaced by a user-confirmed re-upload"}"#.to_owned());
        superseded.updated_at = superseded_at;
        superseded.completed_at = Some(superseded_at);
        if !self.reserve_document(&document_id).await {
            return Err(AtlasError::invalid_input(
                "a parse recovery is already running for this paper",
            ));
        }
        if let Err(error) = self
            .store
            .supersede_operation(&superseded, &operation)
            .await
        {
            self.in_flight.lock().await.remove(&document_id);
            return Err(error);
        }
        self.spawn_reserved(operation, Some(configuration));
        Ok(self.view(&document_id).await?.parse)
    }

    async fn recover(&self) -> Result<usize, AtlasError> {
        let mut latest_by_document = HashMap::<DocumentId, ParseOperation>::new();
        for operation in self.store.recoverable_operations().await? {
            let replace = latest_by_document
                .get(&operation.document_id)
                .is_none_or(|current| operation_is_newer(&operation, current));
            if replace {
                latest_by_document.insert(operation.document_id.clone(), operation);
            }
        }
        let mut operations = latest_by_document.into_values().collect::<Vec<_>>();
        operations.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let configuration = self.configuration.load().await.unwrap_or(None);
        let mut started = 0;
        for mut operation in operations {
            let operation_configuration = if operation.backend == ParseBackend::CloudMineru.as_str()
            {
                let Some(configuration) = configuration.clone() else {
                    continue;
                };
                if !configuration_matches_operation(&operation, &configuration) {
                    mark_provider_settings_changed(&mut operation)?;
                    self.store.save_operation(&operation).await?;
                    continue;
                }
                Some(configuration)
            } else {
                None
            };
            if self.start(operation, operation_configuration).await {
                started += 1;
            }
        }
        Ok(started)
    }
}

fn operation_snapshot(operation: &ParseOperation, automatic: bool) -> ParseSnapshot {
    let safe_message = operation
        .error_safe_json
        .as_deref()
        .and_then(|encoded| serde_json::from_str::<serde_json::Value>(encoded).ok())
        .and_then(|value| value.get("message")?.as_str().map(str::to_owned));
    ParseSnapshot {
        state: match operation.state {
            ParseOperationState::Queued => ParseState::Queued,
            ParseOperationState::Uploading => ParseState::Uploading,
            ParseOperationState::Processing => ParseState::Processing,
            ParseOperationState::Downloading => ParseState::Downloading,
            ParseOperationState::Normalizing => ParseState::Normalizing,
            ParseOperationState::Succeeded => {
                if operation.backend == ParseBackend::CloudMineru.as_str() {
                    ParseState::Ready
                } else {
                    ParseState::Degraded
                }
            }
            ParseOperationState::Failed | ParseOperationState::Cancelled => ParseState::Failed,
            ParseOperationState::StatusUnknown => ParseState::StatusUnknown,
        },
        backend: ParseBackend::parse(&operation.backend),
        progress: operation.progress,
        parse_operation_id: Some(operation.id.clone()),
        automatic_cloud_parsing_enabled: automatic,
        safe_message,
    }
}

async fn validate_source(document: &DocumentRecord) -> Result<(), AtlasError> {
    if document.file_state != DocumentFileState::Available {
        return Err(AtlasError::source_missing());
    }
    let path = Path::new(&document.file_path);
    let metadata = fs::metadata(path)
        .await
        .map_err(|error| AtlasError::source_unreadable(error.to_string()))?;
    if metadata.len() != document.file_size_bytes {
        return Err(AtlasError::document_changed());
    }
    let modified = metadata
        .modified()
        .map_err(|error| AtlasError::source_unreadable(error.to_string()))?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AtlasError::source_unreadable("PDF modification time is invalid"))?;
    let modified_ms = u64::try_from(modified.as_millis())
        .map_err(|_| AtlasError::source_unreadable("PDF modification time is too large"))?;
    if modified_ms != document.file_mtime_ms {
        return Err(AtlasError::document_changed());
    }

    let mut file = fs::File::open(path)
        .await
        .map_err(|error| AtlasError::source_unreadable(error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| AtlasError::source_unreadable(error.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if hex::encode(hasher.finalize()) != document.sha256 {
        return Err(AtlasError::document_changed());
    }
    Ok(())
}

fn now_ms() -> Result<u64, AtlasError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AtlasError::internal("system clock predates the Unix epoch"))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| AtlasError::internal("system time does not fit in storage"))
}

fn operation_is_newer(candidate: &ParseOperation, current: &ParseOperation) -> bool {
    candidate.created_at > current.created_at
        || (candidate.created_at == current.created_at && candidate.id > current.id)
}

fn configuration_matches_operation(
    operation: &ParseOperation,
    configuration: &CloudParseConfiguration,
) -> bool {
    operation.endpoint_fingerprint.as_deref() == Some(configuration.endpoint_fingerprint.as_str())
}

fn mark_provider_settings_changed(operation: &mut ParseOperation) -> Result<(), AtlasError> {
    let changed_at = now_ms()?;
    operation.state = ParseOperationState::StatusUnknown;
    operation.progress = None;
    operation.error_code = Some("provider_settings_changed".to_owned());
    operation.error_safe_json = Some(
        r#"{"message":"Cloud MinerU settings changed; confirm a new upload to continue"}"#
            .to_owned(),
    );
    operation.updated_at = changed_at;
    operation.completed_at = None;
    Ok(())
}

#[cfg(test)]
mod tests;

#[derive(Debug)]
struct OperationFailure {
    code: String,
    message: String,
    disposition: FailureDisposition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailureDisposition {
    Failed,
    StatusUnknown,
    RetryOnRestart,
}

impl OperationFailure {
    fn failed(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            disposition: FailureDisposition::Failed,
        }
    }

    fn unknown(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            disposition: FailureDisposition::StatusUnknown,
        }
    }

    fn storage(message: impl Into<String>) -> Self {
        Self {
            code: "storage_unavailable".to_owned(),
            message: message.into(),
            disposition: FailureDisposition::RetryOnRestart,
        }
    }

    fn from_atlas(error: AtlasError) -> Self {
        if error.code == AtlasErrorCode::StorageUnavailable {
            Self::storage(error.message)
        } else {
            Self::failed(
                format!("{:?}", error.code).to_ascii_lowercase(),
                error.message,
            )
        }
    }

    fn from_resume_status(error: crate::CloudParseError) -> Self {
        if error.kind == CloudParseErrorKind::Unauthorized {
            Self::from_cloud(error)
        } else {
            Self::unknown(
                "remote_status_unavailable",
                "Atlas could not confirm whether Cloud MinerU received the PDF",
            )
        }
    }

    fn from_cloud(error: crate::CloudParseError) -> Self {
        let code = match error.kind {
            CloudParseErrorKind::Unauthorized => "unauthorized",
            CloudParseErrorKind::RateLimited => "rate_limited",
            CloudParseErrorKind::Timeout => "timeout",
            CloudParseErrorKind::Transport => "unreachable",
            CloudParseErrorKind::Protocol => "protocol_incompatible",
            CloudParseErrorKind::Remote => "remote_failed",
            CloudParseErrorKind::DownloadTooLarge => "download_too_large",
        };
        if error.outcome_unknown {
            Self::unknown(code, error.safe_message)
        } else {
            Self::failed(code, error.safe_message)
        }
    }
}
