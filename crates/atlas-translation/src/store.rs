use async_trait::async_trait;
use atlas_domain::{
    AtlasError, BlockId, ChapterId, DocumentId, JobId, SessionId, StructuredContent,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranslationJobKind {
    Foreground,
    Prefetch,
}

impl TranslationJobKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Foreground => "translate",
            Self::Prefetch => "prefetch",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "translate" => Some(Self::Foreground),
            "prefetch" => Some(Self::Prefetch),
            _ => None,
        }
    }

    #[must_use]
    pub fn priority(self) -> i32 {
        match self {
            Self::Foreground => 100,
            Self::Prefetch => 10,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranslationJobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

impl TranslationJobState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "interrupted" => Some(Self::Interrupted),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranslationRecordState {
    Queued,
    Translating,
    Ready,
    Stale,
    Failed,
    Cancelled,
}

impl TranslationRecordState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Translating => "translating",
            Self::Ready => "ready",
            Self::Stale => "stale",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "translating" => Some(Self::Translating),
            "ready" => Some(Self::Ready),
            "stale" => Some(Self::Stale),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TranslationJob {
    pub id: JobId,
    pub session_id: SessionId,
    pub document_id: DocumentId,
    pub chapter_id: ChapterId,
    pub kind: TranslationJobKind,
    pub state: TranslationJobState,
    pub plan_digest: String,
    pub endpoint_fingerprint: String,
    pub model_id: String,
    pub block_ids: Vec<BlockId>,
    pub completed_block_ids: Vec<BlockId>,
    pub attempt_count: u32,
    pub error_code: Option<String>,
    pub safe_message: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub completed_at: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct NewTranslationRecord {
    pub id: String,
    pub block_id: BlockId,
    pub request_digest: String,
    pub source_digest: String,
    pub target_locale: String,
    pub endpoint_origin: String,
    pub provider_profile_fingerprint: String,
    pub model_id: String,
    pub prompt_version: String,
    pub applicable_preference_digest: String,
    pub created_at: u64,
}

#[derive(Clone, Debug)]
pub struct StoredTranslation {
    pub id: String,
    pub block_id: BlockId,
    pub request_digest: String,
    pub source_digest: String,
    pub model_id: String,
    pub state: TranslationRecordState,
    pub target: Option<StructuredContent>,
    pub target_plain_text: Option<String>,
    pub error_code: Option<String>,
    pub safe_message: Option<String>,
    pub updated_at: u64,
}

#[derive(Clone, Debug)]
pub struct CommittedTranslation {
    pub block_id: BlockId,
    pub target: StructuredContent,
    pub target_plain_text: String,
    pub validation_json: String,
}

#[derive(Clone, Debug)]
pub struct RecoveryTarget {
    pub job_id: JobId,
    pub session_id: SessionId,
    pub document_id: DocumentId,
    pub chapter_id: ChapterId,
    pub kind: TranslationJobKind,
}

#[async_trait]
pub trait TranslationStore: Send + Sync {
    async fn translation(
        &self,
        block_id: &BlockId,
        request_digest: &str,
    ) -> Result<Option<StoredTranslation>, AtlasError>;

    async fn active_for_chapter(
        &self,
        chapter_id: &ChapterId,
    ) -> Result<Vec<StoredTranslation>, AtlasError>;

    async fn latest_job(
        &self,
        chapter_id: &ChapterId,
        plan_digest: Option<&str>,
    ) -> Result<Option<TranslationJob>, AtlasError>;

    /// Atomically activates cache hits, queues misses, and persists the job
    /// before any model request can leave the process.
    async fn prepare_job(
        &self,
        job: &TranslationJob,
        records: &[NewTranslationRecord],
    ) -> Result<Vec<BlockId>, AtlasError>;

    async fn save_job(&self, job: &TranslationJob) -> Result<(), AtlasError>;

    /// Commits validated blocks and the job checkpoint in one transaction.
    async fn commit(
        &self,
        job: &TranslationJob,
        translations: &[CommittedTranslation],
    ) -> Result<(), AtlasError>;

    async fn fail(
        &self,
        job: &TranslationJob,
        failures: &[(BlockId, String, String)],
    ) -> Result<(), AtlasError>;

    async fn recoverable(&self) -> Result<Vec<RecoveryTarget>, AtlasError>;

    /// Durably cancels every non-terminal translation for a document before
    /// in-memory workers are signalled.
    async fn cancel_document(
        &self,
        document_id: &DocumentId,
        cancelled_at: u64,
    ) -> Result<usize, AtlasError>;

    /// Marks an orphaned recovery row superseded without touching a job that
    /// was successfully reclaimed and moved out of `interrupted`.
    async fn supersede_interrupted(
        &self,
        job_id: &JobId,
        superseded_at: u64,
    ) -> Result<bool, AtlasError>;

    async fn latest_prefetched_chapter(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<ChapterId>, AtlasError>;
}
