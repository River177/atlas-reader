use async_trait::async_trait;
use atlas_domain::{AtlasError, DocumentFileState, DocumentId, DocumentSummary, LibrarySort};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentListRequest {
    pub text: Option<String>,
    pub sort: LibrarySort,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentImport {
    pub id: DocumentId,
    pub sha256: String,
    pub title: String,
    pub authors: Vec<String>,
    pub page_count: u32,
    pub file_path: String,
    pub file_size_bytes: u64,
    pub file_mtime_ms: u64,
    pub imported_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentRecord {
    pub id: DocumentId,
    pub sha256: String,
    pub title: String,
    pub authors: Vec<String>,
    pub page_count: Option<u32>,
    pub file_path: String,
    pub file_size_bytes: u64,
    pub file_mtime_ms: u64,
    pub file_state: DocumentFileState,
    pub last_opened_at: u64,
}

impl DocumentRecord {
    #[must_use]
    pub fn summary(&self) -> DocumentSummary {
        let file_name = std::path::Path::new(&self.file_path)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or(&self.file_path)
            .to_owned();

        DocumentSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            authors: self.authors.clone(),
            page_count: self.page_count,
            file_name,
            source_state: self.file_state,
            last_opened_at: self.last_opened_at,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentSourceUpdate {
    pub file_path: String,
    pub file_size_bytes: u64,
    pub file_mtime_ms: u64,
    pub file_state: DocumentFileState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredImport {
    pub document: DocumentRecord,
    pub duplicate: bool,
}

#[async_trait]
pub trait DocumentStore: Send + Sync {
    async fn list(&self, request: &DocumentListRequest)
    -> Result<Vec<DocumentSummary>, AtlasError>;

    async fn import(&self, input: &DocumentImport) -> Result<StoredImport, AtlasError>;

    async fn get(&self, document_id: &DocumentId) -> Result<Option<DocumentRecord>, AtlasError>;

    async fn list_sources(&self) -> Result<Vec<DocumentRecord>, AtlasError>;

    async fn update_source(
        &self,
        document_id: &DocumentId,
        update: &DocumentSourceUpdate,
        updated_at: u64,
    ) -> Result<DocumentRecord, AtlasError>;

    async fn remove(&self, document_id: &DocumentId) -> Result<bool, AtlasError>;
}
