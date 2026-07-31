use std::path::Path;

use atlas_domain::{
    BlockId, BlockKind, CANONICAL_SCHEMA_VERSION, CanonicalBlock, CanonicalChapter,
    CanonicalDocument, ChapterId, ChapterRole, DocumentId, ParserIdentity, StructuredContent,
};
use atlas_parse::{
    NewParseOperation, ParseOperation, ParseOperationState, ParseStore, PublishArtifact,
};
use atlas_storage::{AtlasDatabase, SqliteParseStore};
use sha2::{Digest, Sha256};
use sqlx::Row;
use tempfile::TempDir;

#[tokio::test]
async fn operation_checkpoint_and_publish_survive_sqlite_reload() {
    let temporary = TempDir::new().expect("temporary directory");
    let database = AtlasDatabase::open(&temporary.path().join("atlas.sqlite3"))
        .await
        .expect("database should open");
    insert_document(&database).await;
    let artifacts = temporary.path().join("artifacts");
    let store = SqliteParseStore::new(&database, artifacts.clone());
    let mut operation = operation("operation-1", "job-1");

    store
        .save_operation(&operation)
        .await
        .expect("queued operation should persist");
    operation.state = ParseOperationState::Uploading;
    operation.batch_id = Some("batch-1".to_owned());
    operation.upload_url = Some("https://upload.example/presigned-secret".to_owned());
    operation.updated_at = 2;
    store
        .save_operation(&operation)
        .await
        .expect("upload checkpoint should persist");

    let recovered = store
        .recoverable_operations()
        .await
        .expect("recoverable operations should load");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].batch_id.as_deref(), Some("batch-1"));
    assert_eq!(
        recovered[0].upload_url.as_deref(),
        Some("https://upload.example/presigned-secret")
    );
    let event_payloads =
        sqlx::query("SELECT payload_json FROM job_events WHERE job_id = 'job-1' ORDER BY sequence")
            .fetch_all(database.pool())
            .await
            .expect("job events should load")
            .into_iter()
            .map(|row| row.get::<String, _>("payload_json"))
            .collect::<Vec<_>>();
    assert_eq!(event_payloads.len(), 2);
    assert!(
        event_payloads
            .iter()
            .all(|payload| !payload.contains("presigned-secret"))
    );

    let document = canonical("artifact-1", "A searchable paragraph");
    let manifest = persist_manifest(&artifacts, &document, "artifact-1");
    operation.state = ParseOperationState::Succeeded;
    operation.progress = Some(1.0);
    operation.updated_at = 3;
    operation.completed_at = Some(3);
    store
        .publish(&PublishArtifact {
            id: "artifact-1".to_owned(),
            operation: operation.clone(),
            document: document.clone(),
            manifest_relative_path: manifest.0,
            content_digest: manifest.1,
            created_at: 3,
        })
        .await
        .expect("artifact should publish");

    drop(store);
    drop(database);
    let reopened = AtlasDatabase::open(&temporary.path().join("atlas.sqlite3"))
        .await
        .expect("database should reopen");
    let reopened_store = SqliteParseStore::new(&reopened, artifacts);
    let loaded = reopened_store
        .active_document(&DocumentId::from("document-1"))
        .await
        .expect("active document should load")
        .expect("active document should exist");
    assert_eq!(loaded, document);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM blocks_fts WHERE blocks_fts MATCH 'searchable'",
        )
        .fetch_one(reopened.pool())
        .await
        .expect("FTS should query"),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM jobs WHERE id = 'job-1'")
            .fetch_one(reopened.pool())
            .await
            .expect("job should load"),
        "succeeded"
    );
}

#[tokio::test]
async fn a_failed_replacement_publish_keeps_the_previous_artifact_active() {
    let temporary = TempDir::new().expect("temporary directory");
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    insert_document(&database).await;
    let artifacts = temporary.path().join("artifacts");
    let store = SqliteParseStore::new(&database, artifacts.clone());

    let mut first_operation = operation("operation-1", "job-1");
    store
        .save_operation(&first_operation)
        .await
        .expect("operation should persist");
    first_operation.state = ParseOperationState::Succeeded;
    first_operation.completed_at = Some(2);
    first_operation.updated_at = 2;
    let first_document = canonical("artifact-1", "First");
    let first_manifest = persist_manifest(&artifacts, &first_document, "artifact-1");
    store
        .publish(&PublishArtifact {
            id: "artifact-1".to_owned(),
            operation: first_operation,
            document: first_document.clone(),
            manifest_relative_path: first_manifest.0,
            content_digest: first_manifest.1,
            created_at: 2,
        })
        .await
        .expect("first artifact should publish");

    let mut second_operation = operation("operation-2", "job-2");
    store
        .save_operation(&second_operation)
        .await
        .expect("second operation should persist");
    second_operation.state = ParseOperationState::Succeeded;
    second_operation.completed_at = Some(4);
    second_operation.updated_at = 4;
    // Reusing the first chapter id violates the global primary key after the
    // transaction has already retired the old active row. The whole publish
    // must roll back, including that retirement.
    let second_document = canonical("artifact-1", "Second");
    let second_manifest = persist_manifest(&artifacts, &second_document, "artifact-2");
    let result = store
        .publish(&PublishArtifact {
            id: "artifact-2".to_owned(),
            operation: second_operation,
            document: second_document,
            manifest_relative_path: second_manifest.0,
            content_digest: second_manifest.1,
            created_at: 4,
        })
        .await;

    assert!(result.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT id FROM parse_artifacts WHERE document_id = 'document-1' AND is_active = 1",
        )
        .fetch_one(database.pool())
        .await
        .expect("active artifact should remain"),
        "artifact-1"
    );
    assert_eq!(
        store
            .active_document(&DocumentId::from("document-1"))
            .await
            .expect("active document should load"),
        Some(first_document)
    );
}

#[tokio::test]
async fn a_confirmed_reupload_atomically_cancels_the_unknown_operation() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    insert_document(&database).await;
    let store = SqliteParseStore::new(&database, std::env::temp_dir().join("atlas-artifacts-test"));
    let mut unknown = operation("operation-1", "job-1");
    unknown.state = ParseOperationState::StatusUnknown;
    unknown.updated_at = 2;
    store
        .save_operation(&unknown)
        .await
        .expect("unknown operation should persist");
    let replacement = operation("operation-2", "job-2");
    unknown.state = ParseOperationState::Cancelled;
    unknown.error_code = Some("superseded_by_reupload".to_owned());
    unknown.completed_at = Some(3);
    unknown.updated_at = 3;

    store
        .supersede_operation(&unknown, &replacement)
        .await
        .expect("replacement should be atomic");

    let states = sqlx::query(
        "SELECT id, state FROM parse_operations
         WHERE document_id = 'document-1' ORDER BY id",
    )
    .fetch_all(database.pool())
    .await
    .expect("operation states should load")
    .into_iter()
    .map(|row| (row.get::<String, _>("id"), row.get::<String, _>("state")))
    .collect::<Vec<_>>();
    assert_eq!(
        states,
        vec![
            ("operation-1".to_owned(), "cancelled".to_owned()),
            ("operation-2".to_owned(), "queued".to_owned()),
        ]
    );

    let stale_replacement = operation("operation-3", "job-3");
    assert!(
        store
            .supersede_operation(&unknown, &stale_replacement)
            .await
            .is_err(),
        "a stale caller must not supersede an operation twice"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM parse_operations WHERE document_id = 'document-1'",
        )
        .fetch_one(database.pool())
        .await
        .expect("operation count should load"),
        2
    );
}

async fn insert_document(database: &AtlasDatabase) {
    sqlx::query(
        "INSERT INTO documents (
           id, sha256, title, authors_json, page_count, file_path,
           file_size_bytes, file_mtime_ms, file_state, created_at,
           updated_at, last_opened_at
         ) VALUES (
           'document-1',
           'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
           'Synthetic', '[]', 1, '/tmp/synthetic.pdf', 10, 1,
           'available', 1, 1, 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("document should insert");
}

fn operation(id: &str, job_id: &str) -> ParseOperation {
    ParseOperation::new(NewParseOperation {
        id: id.to_owned(),
        job_id: job_id.to_owned(),
        session_id: "session-1".to_owned(),
        document_id: DocumentId::from("document-1"),
        provider_profile_id: None,
        backend: "local_text".to_owned(),
        parser_version: "test-parser".to_owned(),
        normalizer_version: "test-normalizer".to_owned(),
        endpoint_origin: None,
        endpoint_fingerprint: None,
        data_id: "a".repeat(64),
        created_at: 1,
    })
}

fn canonical(artifact_id: &str, text: &str) -> CanonicalDocument {
    let content = StructuredContent::text(text);
    let source_digest = hex::encode(Sha256::digest(
        serde_json::to_vec(&content).expect("content should encode"),
    ));
    CanonicalDocument {
        schema_version: CANONICAL_SCHEMA_VERSION,
        artifact_id: artifact_id.to_owned(),
        document_id: DocumentId::from("document-1"),
        source_sha256: "a".repeat(64),
        parser: ParserIdentity {
            name: "Synthetic".to_owned(),
            version: "1".to_owned(),
            backend: "local_text".to_owned(),
        },
        normalizer_version: "1".to_owned(),
        page_count: 1,
        title: Some("Synthetic".to_owned()),
        chapters: vec![CanonicalChapter {
            id: ChapterId::new(format!("chapter-{artifact_id}")),
            order_index: 0,
            depth: 1,
            role: ChapterRole::Body,
            source_title: "1 Introduction".to_owned(),
            page_start: 1,
            page_end: 1,
            blocks: vec![CanonicalBlock {
                id: BlockId::new(format!("block-{artifact_id}")),
                order_index: 0,
                kind: BlockKind::Paragraph,
                page_start: 1,
                page_end: 1,
                bounding_boxes: Vec::new(),
                content,
                source_digest,
            }],
        }],
        assets: Vec::new(),
    }
}

fn persist_manifest(
    root: &Path,
    document: &CanonicalDocument,
    artifact_id: &str,
) -> (String, String) {
    let relative = Path::new("document-1")
        .join(artifact_id)
        .join("manifest.json");
    let path = root.join(&relative);
    std::fs::create_dir_all(path.parent().expect("manifest parent"))
        .expect("manifest directory should exist");
    let bytes = serde_json::to_vec(document).expect("manifest should encode");
    std::fs::write(&path, &bytes).expect("manifest should write");
    (
        relative.to_string_lossy().into_owned(),
        hex::encode(Sha256::digest(bytes)),
    )
}
