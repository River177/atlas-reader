use async_trait::async_trait;
use atlas_domain::{AtlasError, CanonicalDocument, DocumentId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseOperationState {
    Queued,
    Uploading,
    Processing,
    Downloading,
    Normalizing,
    Succeeded,
    Failed,
    Cancelled,
    StatusUnknown,
}

impl ParseOperationState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Uploading => "uploading",
            Self::Processing => "processing",
            Self::Downloading => "downloading",
            Self::Normalizing => "normalizing",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::StatusUnknown => "status_unknown",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "uploading" => Some(Self::Uploading),
            "processing" => Some(Self::Processing),
            "downloading" => Some(Self::Downloading),
            "normalizing" => Some(Self::Normalizing),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "status_unknown" => Some(Self::StatusUnknown),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    #[must_use]
    pub fn job_state(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Uploading | Self::Downloading | Self::Normalizing => "running",
            Self::Processing => "waiting_remote",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::StatusUnknown => "status_unknown",
        }
    }
}

#[derive(Clone, Debug)]
pub struct NewParseOperation {
    pub id: String,
    pub job_id: String,
    pub session_id: String,
    pub document_id: DocumentId,
    pub provider_profile_id: Option<String>,
    pub backend: String,
    pub parser_version: String,
    pub normalizer_version: String,
    pub endpoint_origin: Option<String>,
    pub endpoint_fingerprint: Option<String>,
    pub data_id: String,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParseOperation {
    pub id: String,
    pub job_id: String,
    pub session_id: String,
    pub document_id: DocumentId,
    pub provider_profile_id: Option<String>,
    pub backend: String,
    pub parser_version: String,
    pub normalizer_version: String,
    pub endpoint_origin: Option<String>,
    pub endpoint_fingerprint: Option<String>,
    pub state: ParseOperationState,
    pub progress: Option<f64>,
    pub data_id: String,
    pub batch_id: Option<String>,
    pub upload_url: Option<String>,
    pub download_url: Option<String>,
    pub remote_status_json: Option<String>,
    pub retry_count: u32,
    pub error_code: Option<String>,
    pub error_safe_json: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub completed_at: Option<u64>,
}

impl ParseOperation {
    #[must_use]
    pub fn new(input: NewParseOperation) -> Self {
        Self {
            id: input.id,
            job_id: input.job_id,
            session_id: input.session_id,
            document_id: input.document_id,
            provider_profile_id: input.provider_profile_id,
            backend: input.backend,
            parser_version: input.parser_version,
            normalizer_version: input.normalizer_version,
            endpoint_origin: input.endpoint_origin,
            endpoint_fingerprint: input.endpoint_fingerprint,
            state: ParseOperationState::Queued,
            progress: None,
            data_id: input.data_id,
            batch_id: None,
            upload_url: None,
            download_url: None,
            remote_status_json: None,
            retry_count: 0,
            error_code: None,
            error_safe_json: None,
            created_at: input.created_at,
            updated_at: input.created_at,
            completed_at: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PublishArtifact {
    pub id: String,
    pub operation: ParseOperation,
    pub document: CanonicalDocument,
    pub manifest_relative_path: String,
    pub content_digest: String,
    pub created_at: u64,
}

#[async_trait]
pub trait ParseStore: Send + Sync {
    async fn active_document(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<CanonicalDocument>, AtlasError>;

    async fn latest_operation(
        &self,
        document_id: &DocumentId,
        backend: Option<&str>,
    ) -> Result<Option<ParseOperation>, AtlasError>;

    async fn recoverable_operations(&self) -> Result<Vec<ParseOperation>, AtlasError>;

    /// Persists the complete checkpoint and appends a job event when its state
    /// changed. Implementations must make those two writes atomic.
    async fn save_operation(&self, operation: &ParseOperation) -> Result<(), AtlasError>;

    /// Atomically cancels an unknown operation and inserts the replacement
    /// created by an explicit user-confirmed re-upload.
    async fn supersede_operation(
        &self,
        operation: &ParseOperation,
        replacement: &ParseOperation,
    ) -> Result<(), AtlasError>;

    /// Atomically activates the artifact, inserts its chapters and blocks,
    /// retires the previous artifact, and marks both operation and job as
    /// succeeded. A reader can therefore never observe a half-published tree.
    async fn publish(&self, artifact: &PublishArtifact) -> Result<(), AtlasError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_names_round_trip() {
        for state in [
            ParseOperationState::Queued,
            ParseOperationState::Uploading,
            ParseOperationState::Processing,
            ParseOperationState::Downloading,
            ParseOperationState::Normalizing,
            ParseOperationState::Succeeded,
            ParseOperationState::Failed,
            ParseOperationState::Cancelled,
            ParseOperationState::StatusUnknown,
        ] {
            assert_eq!(ParseOperationState::parse(state.as_str()), Some(state));
        }
        assert_eq!(ParseOperationState::parse("running"), None);
    }
}
