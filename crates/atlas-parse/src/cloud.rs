use std::{
    fmt,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use atlas_domain::DocumentId;

/// Credential value available only to the network adapter. Its debug
/// representation is deliberately useless so a derived `Debug` on a request
/// can never leak the token.
#[derive(Clone, Eq, PartialEq)]
pub struct CloudCredential(String);

impl CloudCredential {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CloudCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CloudCredential(<redacted>)")
    }
}

#[derive(Clone, Debug)]
pub struct CloudParseRequest {
    pub document_id: DocumentId,
    pub data_id: String,
    pub file_name: String,
    pub file_path: PathBuf,
    pub file_size_bytes: u64,
    pub endpoint_base_url: String,
    pub credential: CloudCredential,
    pub language: String,
    pub model_version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloudParseSubmission {
    pub batch_id: String,
    pub data_id: String,
    /// This URL is a short-lived bearer capability. It may be persisted for
    /// crash recovery, but must never appear in logs, UI errors, or events.
    pub upload_url: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CloudParseProgress {
    pub extracted_pages: u32,
    pub total_pages: u32,
}

impl CloudParseProgress {
    #[must_use]
    pub fn ratio(self) -> Option<f64> {
        (self.total_pages > 0).then(|| {
            f64::from(self.extracted_pages.min(self.total_pages)) / f64::from(self.total_pages)
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CloudParseStatus {
    Missing,
    Pending,
    Running(CloudParseProgress),
    Done { download_url: String },
    Failed { safe_message: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelCapability {
    Cancelled,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloudParseErrorKind {
    Unauthorized,
    RateLimited,
    Timeout,
    Transport,
    Protocol,
    Remote,
    DownloadTooLarge,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloudParseError {
    pub kind: CloudParseErrorKind,
    pub safe_message: String,
    /// True only when Atlas cannot tell whether the upload reached the remote
    /// endpoint. The state machine then queries the persisted batch before it
    /// considers sending bytes again.
    pub outcome_unknown: bool,
}

impl CloudParseError {
    #[must_use]
    pub fn new(kind: CloudParseErrorKind, safe_message: impl Into<String>) -> Self {
        Self {
            kind,
            safe_message: safe_message.into(),
            outcome_unknown: false,
        }
    }

    #[must_use]
    pub fn unknown_upload(safe_message: impl Into<String>) -> Self {
        Self {
            kind: CloudParseErrorKind::Transport,
            safe_message: safe_message.into(),
            outcome_unknown: true,
        }
    }
}

impl fmt::Display for CloudParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.safe_message)
    }
}

impl std::error::Error for CloudParseError {}

#[async_trait]
pub trait CloudParserPort: Send + Sync {
    /// Requests a batch and returns its upload capability without uploading.
    ///
    /// The caller persists this result before invoking `upload`, which is what
    /// makes a lost upload response recoverable without creating a second
    /// billable remote task.
    async fn request_upload(
        &self,
        request: &CloudParseRequest,
    ) -> Result<CloudParseSubmission, CloudParseError>;

    /// Streams the source PDF using the already-persisted upload capability.
    /// The adapter must not add `Content-Type`; MinerU's OSS signature is
    /// computed over an empty value.
    async fn upload(
        &self,
        submission: &CloudParseSubmission,
        file_path: &Path,
    ) -> Result<(), CloudParseError>;

    async fn status(
        &self,
        request: &CloudParseRequest,
        batch_id: &str,
    ) -> Result<CloudParseStatus, CloudParseError>;

    /// Streams a completed result to `destination`, stopping before the body
    /// can exceed `max_bytes`.
    async fn download(
        &self,
        download_url: &str,
        destination: &Path,
        max_bytes: u64,
    ) -> Result<u64, CloudParseError>;

    async fn cancel(
        &self,
        request: &CloudParseRequest,
        batch_id: &str,
    ) -> Result<CancelCapability, CloudParseError>;
}
