use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum AtlasErrorCode {
    InvalidInput,
    UnsupportedFileType,
    SourceMissing,
    SourceUnreadable,
    InvalidPdf,
    PdfTooLarge,
    PdfTooManyPages,
    DocumentChanged,
    NotFound,
    StaleRevision,
    StorageUnavailable,
    ProviderNotConfigured,
    Internal,
}

#[derive(Clone, Debug, Deserialize, Error, PartialEq, Serialize, TS)]
#[error("{message}")]
#[ts(export)]
pub struct AtlasError {
    pub code: AtlasErrorCode,
    pub message: String,
    pub recoverable: bool,
}

impl AtlasError {
    #[must_use]
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            code: AtlasErrorCode::InvalidInput,
            message: message.into(),
            recoverable: true,
        }
    }

    #[must_use]
    pub fn unsupported_file_type() -> Self {
        Self {
            code: AtlasErrorCode::UnsupportedFileType,
            message: "Atlas Reader only imports PDF files".to_owned(),
            recoverable: true,
        }
    }

    #[must_use]
    pub fn source_missing() -> Self {
        Self {
            code: AtlasErrorCode::SourceMissing,
            message: "The selected PDF no longer exists".to_owned(),
            recoverable: true,
        }
    }

    #[must_use]
    pub fn source_unreadable(message: impl Into<String>) -> Self {
        Self {
            code: AtlasErrorCode::SourceUnreadable,
            message: message.into(),
            recoverable: true,
        }
    }

    #[must_use]
    pub fn invalid_pdf(message: impl Into<String>) -> Self {
        Self {
            code: AtlasErrorCode::InvalidPdf,
            message: message.into(),
            recoverable: true,
        }
    }

    #[must_use]
    pub fn pdf_too_large(max_megabytes: u64) -> Self {
        Self {
            code: AtlasErrorCode::PdfTooLarge,
            message: format!("The PDF exceeds the {max_megabytes} MB import limit"),
            recoverable: true,
        }
    }

    #[must_use]
    pub fn pdf_too_many_pages(max_pages: u32) -> Self {
        Self {
            code: AtlasErrorCode::PdfTooManyPages,
            message: format!("The PDF exceeds the {max_pages}-page import limit"),
            recoverable: true,
        }
    }

    #[must_use]
    pub fn document_changed() -> Self {
        Self {
            code: AtlasErrorCode::DocumentChanged,
            message: "The selected PDF is not the same document".to_owned(),
            recoverable: true,
        }
    }

    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: AtlasErrorCode::NotFound,
            message: message.into(),
            recoverable: true,
        }
    }

    #[must_use]
    pub fn stale_revision(expected: u32, actual: u32) -> Self {
        Self {
            code: AtlasErrorCode::StaleRevision,
            message: format!("session revision is stale: expected {expected}, current {actual}"),
            recoverable: true,
        }
    }

    #[must_use]
    pub fn storage(message: impl Into<String>) -> Self {
        Self {
            code: AtlasErrorCode::StorageUnavailable,
            message: message.into(),
            recoverable: true,
        }
    }

    #[must_use]
    pub fn provider_not_configured(message: impl Into<String>) -> Self {
        Self {
            code: AtlasErrorCode::ProviderNotConfigured,
            message: message.into(),
            recoverable: true,
        }
    }

    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: AtlasErrorCode::Internal,
            message: message.into(),
            recoverable: false,
        }
    }
}
