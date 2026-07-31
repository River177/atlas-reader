use std::{
    fs,
    path::{Path, PathBuf},
};

use tauri::http::{
    Method, Request, Response, StatusCode,
    header::{
        ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
        CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE,
    },
};

const MAX_WEBVIEW_ASSET_BYTES: u64 = 64 * 1024 * 1024;

pub fn respond(root: Option<&Path>, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    if request.method() == Method::OPTIONS {
        return response(StatusCode::NO_CONTENT, Vec::new(), "text/plain");
    }
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return response(
            StatusCode::METHOD_NOT_ALLOWED,
            b"method not allowed".to_vec(),
            "text/plain",
        );
    }
    let Some(root) = root else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            b"artifact cache unavailable".to_vec(),
            "text/plain",
        );
    };
    let Some((relative, mime_type)) = validated_path(request.uri().path()) else {
        return not_found();
    };
    let Ok(canonical_root) = root.canonicalize() else {
        return not_found();
    };
    let target = root.join(relative);
    let Ok(canonical_target) = target.canonicalize() else {
        return not_found();
    };
    if !canonical_target.starts_with(&canonical_root) {
        return not_found();
    }
    let Ok(metadata) = canonical_target.metadata() else {
        return not_found();
    };
    if !metadata.is_file() || metadata.len() > MAX_WEBVIEW_ASSET_BYTES {
        return response(
            StatusCode::PAYLOAD_TOO_LARGE,
            b"asset is too large".to_vec(),
            "text/plain",
        );
    }
    let body = if request.method() == Method::HEAD {
        Vec::new()
    } else {
        match fs::read(canonical_target) {
            Ok(body) => body,
            Err(_) => return not_found(),
        }
    };
    response_with_length(StatusCode::OK, body, mime_type, metadata.len())
}

fn validated_path(path: &str) -> Option<(PathBuf, &'static str)> {
    let parts = path.strip_prefix('/')?.split('/').collect::<Vec<_>>();
    if parts.len() != 4
        || !safe_id(parts[0])
        || !parts[1].starts_with("artifact-")
        || !safe_id(parts[1])
        || parts[2] != "images"
    {
        return None;
    }
    let file = Path::new(parts[3]);
    let stem = file.file_stem()?.to_str()?;
    if stem.len() != 64 || !stem.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mime_type = match file.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => return None,
    };
    Some((
        Path::new(parts[0])
            .join(parts[1])
            .join("images")
            .join(parts[3]),
        mime_type,
    ))
}

fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn not_found() -> Response<Vec<u8>> {
    response(
        StatusCode::NOT_FOUND,
        b"artifact not found".to_vec(),
        "text/plain",
    )
}

fn response(status: StatusCode, body: Vec<u8>, mime_type: &'static str) -> Response<Vec<u8>> {
    let length = body.len() as u64;
    response_with_length(status, body, mime_type, length)
}

fn response_with_length(
    status: StatusCode,
    body: Vec<u8>,
    mime_type: &'static str,
    length: u64,
) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, mime_type)
        .header(CONTENT_LENGTH, length.to_string())
        .header(CACHE_CONTROL, "public, max-age=31536000, immutable")
        .header(ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(ACCESS_CONTROL_ALLOW_METHODS, "GET, HEAD, OPTIONS")
        .header(ACCESS_CONTROL_ALLOW_HEADERS, "*")
        .body(body)
        .expect("artifact protocol response should be valid")
}

#[cfg(test)]
mod tests {
    use tauri::http::Request;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn serves_only_a_content_addressed_image_inside_the_artifact_root() {
        let temporary = TempDir::new().expect("temporary directory");
        let hash = "a".repeat(64);
        let path = temporary
            .path()
            .join("document-1/artifact-operation-1/images")
            .join(format!("{hash}.jpg"));
        fs::create_dir_all(path.parent().expect("asset parent")).expect("directory should exist");
        fs::write(&path, [0xff, 0xd8, 0xff]).expect("asset should write");
        let request = Request::builder()
            .uri(format!(
                "atlas-artifact://localhost/document-1/artifact-operation-1/images/{hash}.jpg"
            ))
            .body(Vec::new())
            .expect("request should build");

        let result = respond(Some(temporary.path()), &request);

        assert_eq!(result.status(), StatusCode::OK);
        assert_eq!(result.headers()[CONTENT_TYPE], "image/jpeg");
        assert_eq!(result.body(), &[0xff, 0xd8, 0xff]);
    }

    #[test]
    fn rejects_traversal_and_unknown_files() {
        let temporary = TempDir::new().expect("temporary directory");
        let request = Request::builder()
            .uri("atlas-artifact://localhost/document-1/artifact-1/images/../../secret.jpg")
            .body(Vec::new())
            .expect("request should build");

        assert_eq!(
            respond(Some(temporary.path()), &request).status(),
            StatusCode::NOT_FOUND
        );
    }
}
