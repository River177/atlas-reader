use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use atlas_domain::{
    AtlasError, DocumentFileState, DocumentId, OpenedReaderDocument, ReaderSourceToken,
    ReadingPosition, ReadingPositionUpdate,
};

use crate::{AuthorizedPdfSource, ReaderSourceRegistry, ReaderStore};

#[async_trait]
pub trait DocumentReaderModule: Send + Sync {
    async fn open(&self, document_id: DocumentId) -> Result<OpenedReaderDocument, AtlasError>;

    async fn save_position(
        &self,
        source_token: &ReaderSourceToken,
        update: ReadingPositionUpdate,
    ) -> Result<ReadingPosition, AtlasError>;

    async fn close(
        &self,
        source_token: &ReaderSourceToken,
        final_position: Option<ReadingPositionUpdate>,
    ) -> Result<(), AtlasError>;
}

pub struct DefaultDocumentReader {
    store: Arc<dyn ReaderStore>,
    sources: Arc<ReaderSourceRegistry>,
}

impl std::fmt::Debug for DefaultDocumentReader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DefaultDocumentReader")
            .finish_non_exhaustive()
    }
}

impl DefaultDocumentReader {
    #[must_use]
    pub fn new(store: Arc<dyn ReaderStore>, sources: Arc<ReaderSourceRegistry>) -> Self {
        Self { store, sources }
    }
}

#[async_trait]
impl DocumentReaderModule for DefaultDocumentReader {
    async fn open(&self, document_id: DocumentId) -> Result<OpenedReaderDocument, AtlasError> {
        let opened_at = now_ms()?;
        let source = self
            .store
            .open_source(&document_id, opened_at)
            .await?
            .ok_or_else(|| AtlasError::not_found("document was not found"))?;
        match source.file_state {
            DocumentFileState::Available => {}
            DocumentFileState::Missing => return Err(AtlasError::source_missing()),
            DocumentFileState::Changed => return Err(AtlasError::document_changed()),
            DocumentFileState::Unreadable => {
                return Err(AtlasError::source_unreadable(
                    "The PDF source is not readable",
                ));
            }
        }
        validate_source(
            Path::new(&source.file_path),
            source.file_size_bytes,
            source.file_mtime_ms,
        )?;

        let mut position = self
            .store
            .load_position(&document_id)
            .await?
            .unwrap_or_default();
        validate_position(
            &ReadingPositionUpdate {
                page: position.page,
                page_offset_ratio: position.page_offset_ratio,
                scale_value: position.scale_value.clone(),
            },
            source.document.page_count,
        )?;
        if position.updated_at == 0 {
            position.updated_at = opened_at;
        }
        let source_token = self.sources.issue(AuthorizedPdfSource {
            document_id,
            path: source.file_path.into(),
            file_size_bytes: source.file_size_bytes,
            file_mtime_ms: source.file_mtime_ms,
            page_count: source.document.page_count,
        })?;

        Ok(OpenedReaderDocument {
            document: source.document,
            source_token,
            position,
        })
    }

    async fn save_position(
        &self,
        source_token: &ReaderSourceToken,
        update: ReadingPositionUpdate,
    ) -> Result<ReadingPosition, AtlasError> {
        let source = self
            .sources
            .resolve(source_token)?
            .ok_or_else(|| AtlasError::not_found("reader source token is no longer active"))?;
        validate_position(&update, source.page_count)?;
        let position = ReadingPosition {
            page: update.page,
            page_offset_ratio: update.page_offset_ratio,
            scale_value: update.scale_value,
            updated_at: now_ms()?,
        };
        self.store
            .save_position(&source.document_id, &position)
            .await?;
        Ok(position)
    }

    async fn close(
        &self,
        source_token: &ReaderSourceToken,
        final_position: Option<ReadingPositionUpdate>,
    ) -> Result<(), AtlasError> {
        if let Some(position) = final_position {
            self.save_position(source_token, position).await?;
        }
        if self.sources.revoke(source_token)? {
            Ok(())
        } else {
            Err(AtlasError::not_found(
                "reader source token is no longer active",
            ))
        }
    }
}

fn validate_source(path: &Path, expected_size: u64, expected_mtime: u64) -> Result<(), AtlasError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AtlasError::source_missing());
        }
        Err(error) => {
            return Err(AtlasError::source_unreadable(format!(
                "The PDF source cannot be opened: {error}"
            )));
        }
    };
    if !metadata.is_file() {
        return Err(AtlasError::source_unreadable(
            "The PDF source is not a regular file",
        ));
    }
    let mtime = modified_ms(&metadata)?;
    if metadata.len() != expected_size || mtime != expected_mtime {
        return Err(AtlasError::document_changed());
    }
    Ok(())
}

fn validate_position(
    position: &ReadingPositionUpdate,
    page_count: Option<u32>,
) -> Result<(), AtlasError> {
    if position.page == 0 || page_count.is_some_and(|count| position.page > count) {
        return Err(AtlasError::invalid_input(
            "reading position page is outside the document",
        ));
    }
    if !position.page_offset_ratio.is_finite() || !(0.0..=1.0).contains(&position.page_offset_ratio)
    {
        return Err(AtlasError::invalid_input(
            "reading position offset must be between 0 and 1",
        ));
    }
    if !valid_scale(&position.scale_value) {
        return Err(AtlasError::invalid_input(
            "reading position scale is invalid",
        ));
    }
    Ok(())
}

fn valid_scale(value: &str) -> bool {
    matches!(value, "auto" | "page-actual" | "page-fit" | "page-width")
        || value
            .parse::<f64>()
            .is_ok_and(|scale| scale.is_finite() && (0.25..=5.0).contains(&scale))
}

fn modified_ms(metadata: &fs::Metadata) -> Result<u64, AtlasError> {
    let modified = metadata.modified().map_err(|error| {
        AtlasError::source_unreadable(format!("The PDF modification time is unavailable: {error}"))
    })?;
    let duration = modified.duration_since(UNIX_EPOCH).map_err(|_| {
        AtlasError::source_unreadable("The PDF modification time predates the Unix epoch")
    })?;
    u64::try_from(duration.as_millis())
        .map_err(|_| AtlasError::source_unreadable("The PDF modification time is out of range"))
}

fn now_ms() -> Result<u64, AtlasError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AtlasError::internal("system clock predates the Unix epoch"))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| AtlasError::internal("system clock is outside the supported range"))
}
