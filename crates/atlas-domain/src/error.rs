use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum AtlasErrorCode {
    InvalidInput,
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
}
