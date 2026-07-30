use std::{
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use atlas_domain::{
    AtlasError, DocumentId, DocumentSummary, ImportPdfResult, LibraryPage, LibraryQuery,
    RefreshSourcesResult,
};
use tokio::task;
use uuid::Uuid;

use crate::{
    DocumentImport, DocumentListRequest, DocumentSourceUpdate, DocumentStore,
    document_file::{inspect_existing_source, inspect_pdf},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LibraryLimits {
    pub max_file_size_bytes: u64,
    pub max_pages: u32,
}

impl Default for LibraryLimits {
    fn default() -> Self {
        Self {
            max_file_size_bytes: 200 * 1024 * 1024,
            max_pages: 500,
        }
    }
}

#[async_trait]
pub trait LibraryModule: Send + Sync {
    async fn import_pdf(&self, path: String) -> Result<ImportPdfResult, AtlasError>;

    async fn query(&self, input: LibraryQuery) -> Result<LibraryPage, AtlasError>;

    async fn refresh_sources(&self) -> Result<RefreshSourcesResult, AtlasError>;

    async fn relocate(
        &self,
        document_id: DocumentId,
        new_path: String,
    ) -> Result<DocumentSummary, AtlasError>;

    async fn remove(&self, document_id: DocumentId) -> Result<(), AtlasError>;
}

#[derive(Clone)]
pub struct DefaultLibraryModule {
    store: Arc<dyn DocumentStore>,
    limits: LibraryLimits,
}

impl std::fmt::Debug for DefaultLibraryModule {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DefaultLibraryModule")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl DefaultLibraryModule {
    #[must_use]
    pub fn new(store: Arc<dyn DocumentStore>) -> Self {
        Self::with_limits(store, LibraryLimits::default())
    }

    #[must_use]
    pub fn with_limits(store: Arc<dyn DocumentStore>, limits: LibraryLimits) -> Self {
        Self { store, limits }
    }
}

#[async_trait]
impl LibraryModule for DefaultLibraryModule {
    async fn import_pdf(&self, path: String) -> Result<ImportPdfResult, AtlasError> {
        let limits = self.limits;
        let inspected = task::spawn_blocking(move || inspect_pdf(PathBuf::from(path), limits))
            .await
            .map_err(|error| AtlasError::internal(format!("PDF inspection failed: {error}")))??;
        let imported_at = now_ms()?;
        let stored = self
            .store
            .import(&DocumentImport {
                id: DocumentId::new(Uuid::new_v4().to_string()),
                sha256: inspected.sha256,
                title: inspected.title,
                authors: inspected.authors,
                page_count: inspected.page_count,
                file_path: inspected.canonical_path,
                file_size_bytes: inspected.file_size_bytes,
                file_mtime_ms: inspected.file_mtime_ms,
                imported_at,
            })
            .await?;

        Ok(ImportPdfResult {
            document: stored.document.summary(),
            duplicate: stored.duplicate,
        })
    }

    async fn query(&self, input: LibraryQuery) -> Result<LibraryPage, AtlasError> {
        if !(1..=100).contains(&input.limit) {
            return Err(AtlasError::invalid_input(
                "library query limit must be between 1 and 100",
            ));
        }

        let offset = parse_cursor(input.cursor.as_deref())?;
        let text = input
            .text
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let requested_limit = input.limit;
        let request = DocumentListRequest {
            text,
            sort: input.sort,
            offset,
            limit: requested_limit + 1,
        };
        let mut items = self.store.list(&request).await?;
        let has_more = items.len() > requested_limit as usize;
        items.truncate(requested_limit as usize);

        Ok(LibraryPage {
            items,
            next_cursor: has_more.then(|| (offset + requested_limit).to_string()),
        })
    }

    async fn refresh_sources(&self) -> Result<RefreshSourcesResult, AtlasError> {
        let records = self.store.list_sources().await?;
        let inspections = task::spawn_blocking(move || {
            records
                .into_iter()
                .map(|record| {
                    let update = inspect_existing_source(&record);
                    (record, update)
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|error| AtlasError::internal(format!("source refresh failed: {error}")))?;
        let updated_at = now_ms()?;
        let mut updated = Vec::new();

        for (record, update) in inspections {
            if source_changed(&record, &update) {
                updated.push(
                    self.store
                        .update_source(&record.id, &update, updated_at)
                        .await?
                        .summary(),
                );
            }
        }

        Ok(RefreshSourcesResult { updated })
    }

    async fn relocate(
        &self,
        document_id: DocumentId,
        new_path: String,
    ) -> Result<DocumentSummary, AtlasError> {
        let existing = self
            .store
            .get(&document_id)
            .await?
            .ok_or_else(|| AtlasError::not_found("document was not found"))?;
        let limits = self.limits;
        let inspected = task::spawn_blocking(move || inspect_pdf(PathBuf::from(new_path), limits))
            .await
            .map_err(|error| AtlasError::internal(format!("PDF inspection failed: {error}")))??;
        if inspected.sha256 != existing.sha256 {
            return Err(AtlasError::document_changed());
        }

        let update = DocumentSourceUpdate {
            file_path: inspected.canonical_path,
            file_size_bytes: inspected.file_size_bytes,
            file_mtime_ms: inspected.file_mtime_ms,
            file_state: atlas_domain::DocumentFileState::Available,
        };
        Ok(self
            .store
            .update_source(&document_id, &update, now_ms()?)
            .await?
            .summary())
    }

    async fn remove(&self, document_id: DocumentId) -> Result<(), AtlasError> {
        if self.store.remove(&document_id).await? {
            Ok(())
        } else {
            Err(AtlasError::not_found("document was not found"))
        }
    }
}

fn source_changed(record: &crate::DocumentRecord, update: &DocumentSourceUpdate) -> bool {
    record.file_path != update.file_path
        || record.file_size_bytes != update.file_size_bytes
        || record.file_mtime_ms != update.file_mtime_ms
        || record.file_state != update.file_state
}

fn parse_cursor(cursor: Option<&str>) -> Result<u32, AtlasError> {
    match cursor {
        None => Ok(0),
        Some(value) => value
            .parse::<u32>()
            .map_err(|_| AtlasError::invalid_input("library cursor is invalid")),
    }
}

fn now_ms() -> Result<u64, AtlasError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AtlasError::internal("system clock predates the Unix epoch"))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| AtlasError::internal("system clock is outside the supported range"))
}
