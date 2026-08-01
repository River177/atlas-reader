use std::{
    path::{Component, Path},
    sync::Arc,
    time::UNIX_EPOCH,
};

use atlas_document_reader::AuthorizedPdfSource;
use atlas_domain::{AtlasError, DocumentId, ReaderSourceToken};
use atlas_parse::ParseStore;
use axum::{
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{
        HeaderMap, Method, StatusCode,
        header::{
            ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
        },
    },
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt},
};
use tokio_util::io::ReaderStream;

use crate::{app::WebState, error::ApiError};

const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct AssetAccess {
    access: String,
}

pub async fn pdf(
    State(state): State<Arc<WebState>>,
    AxumPath(token): AxumPath<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if method != Method::GET && method != Method::HEAD {
        return Ok(StatusCode::METHOD_NOT_ALLOWED.into_response());
    }
    if token.is_empty() || token.contains('/') {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let token = ReaderSourceToken::new(token);
    state.touch_reader(&token, None).await;
    let source = state
        .sources
        .resolve(&token)?
        .ok_or_else(|| AtlasError::not_found("PDF source not found"))?;
    if !source_is_current(&source).await {
        return Err(AtlasError::document_changed().into());
    }
    let range_header = headers.get(RANGE).and_then(|value| value.to_str().ok());
    let range = parse_range(range_header, source.file_size_bytes)
        .map_err(|_| AtlasError::invalid_input("PDF byte range is invalid"))?;
    let (start, end) = range.unwrap_or((0, source.file_size_bytes.saturating_sub(1)));
    let length = end.saturating_sub(start).saturating_add(1);
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let mut file = fs::File::open(&source.path)
            .await
            .map_err(|error| AtlasError::source_unreadable(error.to_string()))?;
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|error| AtlasError::source_unreadable(error.to_string()))?;
        Body::from_stream(ReaderStream::new(file.take(length)))
    };
    let partial = range_header.is_some();
    let mut response = Response::builder()
        .status(if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(CONTENT_TYPE, "application/pdf")
        .header(CONTENT_LENGTH, length)
        .header(ACCEPT_RANGES, "bytes")
        .header(CACHE_CONTROL, "private, no-store")
        .body(body)
        .map_err(|error| AtlasError::internal(error.to_string()))?;
    if partial {
        response.headers_mut().insert(
            CONTENT_RANGE,
            format!("bytes {start}-{end}/{}", source.file_size_bytes)
                .parse()
                .map_err(|_| AtlasError::internal("PDF content range is invalid"))?,
        );
    }
    Ok(response)
}

pub async fn asset(
    State(state): State<Arc<WebState>>,
    AxumPath((document_id, artifact_id, file_name)): AxumPath<(String, String, String)>,
    Query(access): Query<AssetAccess>,
    method: Method,
) -> Result<Response, ApiError> {
    if method != Method::GET && method != Method::HEAD {
        return Ok(StatusCode::METHOD_NOT_ALLOWED.into_response());
    }
    if !state.auth.resource_matches(&access.access) {
        return Ok(StatusCode::UNAUTHORIZED.into_response());
    }
    if !safe_id(&document_id) || !safe_id(&artifact_id) || !safe_asset_name(&file_name) {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let document_id = DocumentId::from(document_id);
    let document = state
        .parse_store
        .active_document(&document_id)
        .await?
        .ok_or_else(|| AtlasError::not_found("parse artifact not found"))?;
    if document.artifact_id != artifact_id {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let relative_path = format!("images/{file_name}");
    let asset = document
        .assets
        .iter()
        .find(|asset| asset.relative_path == relative_path)
        .ok_or_else(|| AtlasError::not_found("parse asset not found"))?;
    if asset.size_bytes > MAX_ASSET_BYTES {
        return Err(AtlasError::invalid_input("parse asset is too large").into());
    }
    let target = state
        .artifact_root
        .join(document_id.as_str())
        .join(&artifact_id)
        .join(&relative_path);
    let bytes = fs::read(&target)
        .await
        .map_err(|_| AtlasError::not_found("parse asset not found"))?;
    if bytes.len() as u64 != asset.size_bytes || hex::encode(Sha256::digest(&bytes)) != asset.sha256
    {
        return Err(AtlasError::storage("parse asset does not match its manifest").into());
    }
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, asset.mime_type.as_str())
        .header(CONTENT_LENGTH, asset.size_bytes)
        .header(CACHE_CONTROL, "private, max-age=31536000, immutable")
        .body(body)
        .map_err(|error| AtlasError::internal(error.to_string()).into())
}

async fn source_is_current(source: &AuthorizedPdfSource) -> bool {
    let Ok(metadata) = fs::metadata(&source.path).await else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let Ok(duration) = modified.duration_since(UNIX_EPOCH) else {
        return false;
    };
    let Ok(modified_ms) = u64::try_from(duration.as_millis()) else {
        return false;
    };
    metadata.is_file()
        && metadata.len() == source.file_size_bytes
        && modified_ms == source.file_mtime_ms
}

fn parse_range(header: Option<&str>, file_size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(header) = header else {
        return Ok(None);
    };
    if file_size == 0 || !header.starts_with("bytes=") || header.contains(',') {
        return Err(());
    }
    let (start, end) = header[6..].split_once('-').ok_or(())?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let length = suffix.min(file_size);
        return Ok(Some((file_size - length, file_size - 1)));
    }
    let start = start.parse::<u64>().map_err(|_| ())?;
    if start >= file_size {
        return Err(());
    }
    let end = if end.is_empty() {
        file_size - 1
    } else {
        end.parse::<u64>().map_err(|_| ())?.min(file_size - 1)
    };
    (end >= start).then_some(Some((start, end))).ok_or(())
}

fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn safe_asset_name(value: &str) -> bool {
    let path = Path::new(value);
    path.components()
        .all(|part| matches!(part, Component::Normal(_)))
        && path.components().count() == 1
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "webp"
                )
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_standard_and_suffix_ranges() {
        assert_eq!(parse_range(Some("bytes=2-5"), 10), Ok(Some((2, 5))));
        assert_eq!(parse_range(Some("bytes=-3"), 10), Ok(Some((7, 9))));
        assert_eq!(parse_range(Some("bytes=10-"), 10), Err(()));
    }

    #[test]
    fn rejects_nested_asset_names() {
        assert!(safe_asset_name(&format!("{}.jpg", "a".repeat(64))));
        assert!(!safe_asset_name("../secret.jpg"));
        assert!(!safe_asset_name("nested/image.jpg"));
    }
}
