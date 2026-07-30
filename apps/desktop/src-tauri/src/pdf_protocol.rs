use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    sync::Arc,
    time::UNIX_EPOCH,
};

use atlas_contracts::ReaderSourceToken;
use atlas_document_reader::{AuthorizedPdfSource, ReaderSourceRegistry};
use tauri::http::{
    Method, Request, Response, StatusCode,
    header::{
        ACCEPT_RANGES, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
        ACCESS_CONTROL_ALLOW_ORIGIN, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE,
        RANGE,
    },
};

pub fn respond(
    sources: &Arc<ReaderSourceRegistry>,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    if request.method() == Method::OPTIONS {
        return build_response(StatusCode::NO_CONTENT, Vec::new(), 0, None);
    }
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return build_response(
            StatusCode::METHOD_NOT_ALLOWED,
            b"method not allowed".to_vec(),
            18,
            None,
        );
    }

    let Some(token) = request.uri().path().strip_prefix("/pdf/") else {
        return not_found();
    };
    let token = ReaderSourceToken::new(token);
    let source = match sources.resolve(&token) {
        Ok(Some(source)) => source,
        Ok(None) => return not_found(),
        Err(_) => {
            return build_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                b"reader registry unavailable".to_vec(),
                27,
                None,
            );
        }
    };
    if !source_is_current(&source) {
        return build_response(
            StatusCode::CONFLICT,
            b"PDF source changed".to_vec(),
            18,
            None,
        );
    }

    let requested_range = request
        .headers()
        .get(RANGE)
        .and_then(|value| value.to_str().ok());
    let range = match parse_range(requested_range, source.file_size_bytes) {
        Ok(range) => range,
        Err(()) => {
            return build_response(
                StatusCode::RANGE_NOT_SATISFIABLE,
                Vec::new(),
                0,
                Some(format!("bytes */{}", source.file_size_bytes)),
            );
        }
    };
    let (start, end) = range.unwrap_or((0, source.file_size_bytes.saturating_sub(1)));
    let response_length = end.saturating_sub(start).saturating_add(1);
    let body = if request.method() == Method::HEAD {
        Vec::new()
    } else {
        match read_range(&source, start, response_length) {
            Ok(body) => body,
            Err(()) => {
                return build_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    b"PDF source cannot be read".to_vec(),
                    25,
                    None,
                );
            }
        }
    };
    let partial = requested_range.is_some();
    build_response(
        if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        },
        body,
        response_length,
        partial.then(|| format!("bytes {start}-{end}/{}", source.file_size_bytes)),
    )
}

fn read_range(source: &AuthorizedPdfSource, start: u64, length: u64) -> Result<Vec<u8>, ()> {
    let mut file = File::open(&source.path).map_err(|_| ())?;
    file.seek(SeekFrom::Start(start)).map_err(|_| ())?;
    let length = usize::try_from(length).map_err(|_| ())?;
    let mut body = vec![0_u8; length];
    file.read_exact(&mut body).map_err(|_| ())?;
    Ok(body)
}

fn parse_range(header: Option<&str>, file_size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(header) = header else {
        return Ok(None);
    };
    if file_size == 0 || !header.starts_with("bytes=") || header.contains(',') {
        return Err(());
    }
    let range = &header[6..];
    let (start, end) = range.split_once('-').ok_or(())?;
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
    if end < start {
        return Err(());
    }
    Ok(Some((start, end)))
}

fn source_is_current(source: &AuthorizedPdfSource) -> bool {
    let Ok(metadata) = fs::metadata(&source.path) else {
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

fn not_found() -> Response<Vec<u8>> {
    build_response(
        StatusCode::NOT_FOUND,
        b"PDF source not found".to_vec(),
        20,
        None,
    )
}

fn build_response(
    status: StatusCode,
    body: Vec<u8>,
    content_length: u64,
    content_range: Option<String>,
) -> Response<Vec<u8>> {
    let mut builder = Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/pdf")
        .header(CONTENT_LENGTH, content_length.to_string())
        .header(ACCEPT_RANGES, "bytes")
        .header(ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(ACCESS_CONTROL_ALLOW_METHODS, "GET, HEAD, OPTIONS")
        .header(ACCESS_CONTROL_ALLOW_HEADERS, "Range")
        .header(CACHE_CONTROL, "private, no-store");
    if let Some(value) = content_range {
        builder = builder.header(CONTENT_RANGE, value);
    }
    builder
        .body(body)
        .expect("static PDF protocol headers are valid")
}

#[cfg(test)]
mod tests {
    use atlas_contracts::DocumentId;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn serves_valid_byte_ranges_for_active_tokens() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("paper.pdf");
        fs::write(&path, b"0123456789").expect("fixture should write");
        let metadata = fs::metadata(&path).expect("fixture metadata");
        let modified = metadata
            .modified()
            .expect("fixture mtime")
            .duration_since(UNIX_EPOCH)
            .expect("mtime after epoch");
        let sources = Arc::new(ReaderSourceRegistry::default());
        let token = sources
            .issue(AuthorizedPdfSource {
                document_id: DocumentId::from("document-1"),
                path,
                file_size_bytes: metadata.len(),
                file_mtime_ms: u64::try_from(modified.as_millis()).expect("mtime in range"),
                page_count: Some(1),
            })
            .expect("token should issue");
        let request = Request::builder()
            .uri(format!("atlas-reader://localhost/pdf/{}", token.as_str()))
            .header(RANGE, "bytes=2-5")
            .body(Vec::new())
            .expect("request should build");

        let response = respond(&sources, &request);

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.body(), b"2345");
        assert_eq!(
            response
                .headers()
                .get(CONTENT_RANGE)
                .expect("content range"),
            "bytes 2-5/10"
        );
    }

    #[test]
    fn rejects_unknown_tokens_and_invalid_ranges() {
        let sources = Arc::new(ReaderSourceRegistry::default());
        let unknown = Request::builder()
            .uri("atlas-reader://localhost/pdf/unknown")
            .body(Vec::new())
            .expect("request should build");
        assert_eq!(respond(&sources, &unknown).status(), StatusCode::NOT_FOUND);

        assert_eq!(parse_range(Some("bytes=10-1"), 10), Err(()));
        assert_eq!(parse_range(Some("bytes=10-"), 10), Err(()));
        assert_eq!(parse_range(Some("bytes=-0"), 10), Err(()));
    }
}
