use atlas_domain::{AtlasError, AtlasErrorCode};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

#[derive(Debug)]
pub struct ApiError(pub AtlasError);

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl From<AtlasError> for ApiError {
    fn from(error: AtlasError) -> Self {
        Self(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.0.code {
            AtlasErrorCode::InvalidInput
            | AtlasErrorCode::UnsupportedFileType
            | AtlasErrorCode::InvalidPdf
            | AtlasErrorCode::PdfTooLarge
            | AtlasErrorCode::PdfTooManyPages
            | AtlasErrorCode::DocumentChanged
            | AtlasErrorCode::StaleRevision
            | AtlasErrorCode::StaleSelection
            | AtlasErrorCode::AssistantBusy => StatusCode::BAD_REQUEST,
            AtlasErrorCode::SourceMissing | AtlasErrorCode::NotFound => StatusCode::NOT_FOUND,
            AtlasErrorCode::ProviderNotConfigured => StatusCode::PRECONDITION_REQUIRED,
            AtlasErrorCode::SourceUnreadable | AtlasErrorCode::StorageUnavailable => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            AtlasErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(self.0)).into_response()
    }
}

pub fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError(AtlasError::internal(error.to_string()))
}
