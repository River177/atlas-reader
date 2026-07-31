use std::path::{Path, PathBuf};

use atlas_adapters::MineruCloudHttpAdapter;
use atlas_domain::DocumentId;
use atlas_parse::{
    CloudCredential, CloudParseErrorKind, CloudParseRequest, CloudParseStatus,
    CloudParseSubmission, CloudParserPort,
};
use serde_json::json;
use tempfile::TempDir;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

fn request(server: &MockServer, source: PathBuf) -> CloudParseRequest {
    CloudParseRequest {
        document_id: DocumentId::from("document-1"),
        data_id: "a".repeat(64),
        file_name: "paper.pdf".to_owned(),
        file_size_bytes: std::fs::metadata(&source).expect("source metadata").len(),
        file_path: source,
        endpoint_base_url: format!("{}/api/v4", server.uri()),
        credential: CloudCredential::new("test-token"),
        language: "en".to_owned(),
        model_version: "vlm".to_owned(),
    }
}

fn source(temporary: &TempDir) -> PathBuf {
    let path = temporary.path().join("paper.pdf");
    std::fs::write(&path, b"%PDF-1.7 synthetic").expect("source should write");
    path
}

#[tokio::test]
async fn allocates_one_correlated_batch_with_the_verified_protocol() {
    let server = MockServer::start().await;
    let temporary = TempDir::new().expect("temporary directory");
    Mock::given(method("POST"))
        .and(path("/api/v4/file-urls/batch"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_json(json!({
            "files": [{
                "name": "paper.pdf",
                "data_id": "a".repeat(64),
                "is_ocr": false
            }],
            "model_version": "vlm",
            "enable_formula": true,
            "enable_table": true,
            "language": "en"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "msg": "ok",
            "data": {
                "batch_id": "batch-1",
                "file_urls": [format!("{}/upload", server.uri())]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let submission = MineruCloudHttpAdapter::new()
        .expect("adapter should build")
        .request_upload(&request(&server, source(&temporary)))
        .await
        .expect("batch should allocate");

    assert_eq!(submission.batch_id, "batch-1");
    assert_eq!(submission.data_id, "a".repeat(64));
    assert_eq!(submission.upload_url, format!("{}/upload", server.uri()));
}

#[tokio::test]
async fn oss_upload_streams_bytes_without_a_content_type_header() {
    let server = MockServer::start().await;
    let temporary = TempDir::new().expect("temporary directory");
    let source = source(&temporary);
    Mock::given(method("PUT"))
        .and(path("/upload"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let submission = CloudParseSubmission {
        batch_id: "batch-1".to_owned(),
        data_id: "a".repeat(64),
        upload_url: format!("{}/upload", server.uri()),
    };

    MineruCloudHttpAdapter::new()
        .expect("adapter should build")
        .upload(&submission, &source)
        .await
        .expect("upload should succeed");

    let requests = server
        .received_requests()
        .await
        .expect("requests should be recorded");
    let upload = requests
        .iter()
        .find(|request| request.url.path() == "/upload")
        .expect("upload request should exist");
    assert!(
        upload.headers.get("content-type").is_none(),
        "Content-Type changes the OSS signature"
    );
    assert_eq!(upload.body, b"%PDF-1.7 synthetic");
}

#[tokio::test]
async fn status_aligns_results_by_data_id_instead_of_array_position() {
    let server = MockServer::start().await;
    let temporary = TempDir::new().expect("temporary directory");
    Mock::given(method("GET"))
        .and(path("/api/v4/extract-results/batch/batch-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "msg": "ok",
            "data": {
                "extract_result": [
                    {
                        "data_id": "another-document",
                        "state": "done",
                        "full_zip_url": "https://cdn.example/wrong.zip"
                    },
                    {
                        "data_id": "a".repeat(64),
                        "state": "running",
                        "extract_progress": {
                            "extracted_pages": 3,
                            "total_pages": 12
                        }
                    }
                ]
            }
        })))
        .mount(&server)
        .await;

    let status = MineruCloudHttpAdapter::new()
        .expect("adapter should build")
        .status(&request(&server, source(&temporary)), "batch-1")
        .await
        .expect("status should parse");

    assert!(matches!(
        status,
        CloudParseStatus::Running(progress)
            if progress.extracted_pages == 3 && progress.total_pages == 12
    ));
}

#[tokio::test]
async fn understands_gateway_authentication_and_business_not_found_envelopes() {
    let server = MockServer::start().await;
    let temporary = TempDir::new().expect("temporary directory");
    let source = source(&temporary);
    Mock::given(method("GET"))
        .and(path("/api/v4/extract-results/batch/missing"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": -60012,
            "msg": "task not found or expire"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/extract-results/batch/rejected"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "traceId": "trace",
            "msgCode": "A0202",
            "msg": "user authenticate failed",
            "success": false
        })))
        .mount(&server)
        .await;
    let adapter = MineruCloudHttpAdapter::new().expect("adapter should build");
    let request = request(&server, source);

    assert_eq!(
        adapter
            .status(&request, "missing")
            .await
            .expect("missing batch should be understood"),
        CloudParseStatus::Missing
    );
    let error = adapter
        .status(&request, "rejected")
        .await
        .expect_err("rejected key should fail");
    assert_eq!(error.kind, CloudParseErrorKind::Unauthorized);
}

#[tokio::test]
async fn download_enforces_the_streaming_cap_and_removes_partial_output() {
    let server = MockServer::start().await;
    let temporary = TempDir::new().expect("temporary directory");
    Mock::given(method("GET"))
        .and(path("/result.zip"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![7_u8; 64]))
        .mount(&server)
        .await;
    let destination = temporary.path().join("result.zip");

    let error = MineruCloudHttpAdapter::new()
        .expect("adapter should build")
        .download(
            &format!("{}/result.zip", server.uri()),
            Path::new(&destination),
            32,
        )
        .await
        .expect_err("oversized download should fail");

    assert_eq!(error.kind, CloudParseErrorKind::DownloadTooLarge);
    assert!(!destination.exists());
}
