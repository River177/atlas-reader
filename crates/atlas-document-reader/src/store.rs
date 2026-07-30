use async_trait::async_trait;
use atlas_domain::{AtlasError, DocumentFileState, DocumentId, DocumentSummary, ReadingPosition};

#[derive(Clone, Debug, PartialEq)]
pub struct ReaderDocumentSource {
    pub document: DocumentSummary,
    pub file_path: String,
    pub file_size_bytes: u64,
    pub file_mtime_ms: u64,
    pub file_state: DocumentFileState,
}

#[async_trait]
pub trait ReaderStore: Send + Sync {
    async fn open_source(
        &self,
        document_id: &DocumentId,
        opened_at: u64,
    ) -> Result<Option<ReaderDocumentSource>, AtlasError>;

    async fn load_position(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<ReadingPosition>, AtlasError>;

    async fn save_position(
        &self,
        document_id: &DocumentId,
        position: &ReadingPosition,
    ) -> Result<(), AtlasError>;
}
