use std::{
    collections::HashMap,
    fs,
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};

use async_trait::async_trait;
use atlas_document_reader::{
    DefaultDocumentReader, DocumentReaderModule, ReaderDocumentSource, ReaderSourceRegistry,
    ReaderStore,
};
use atlas_domain::{
    AtlasError, AtlasErrorCode, DocumentFileState, DocumentId, DocumentSummary, ReadingPosition,
    ReadingPositionUpdate,
};
use tempfile::tempdir;

#[derive(Debug)]
struct MemoryReaderStore {
    source: Mutex<ReaderDocumentSource>,
    positions: Mutex<HashMap<DocumentId, ReadingPosition>>,
}

#[async_trait]
impl ReaderStore for MemoryReaderStore {
    async fn open_source(
        &self,
        document_id: &DocumentId,
        opened_at: u64,
    ) -> Result<Option<ReaderDocumentSource>, AtlasError> {
        let mut source = self.source.lock().expect("source lock");
        if &source.document.id != document_id {
            return Ok(None);
        }
        source.document.last_opened_at = opened_at;
        Ok(Some(source.clone()))
    }

    async fn load_position(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<ReadingPosition>, AtlasError> {
        Ok(self
            .positions
            .lock()
            .expect("position lock")
            .get(document_id)
            .cloned())
    }

    async fn save_position(
        &self,
        document_id: &DocumentId,
        position: &ReadingPosition,
    ) -> Result<(), AtlasError> {
        self.positions
            .lock()
            .expect("position lock")
            .insert(document_id.clone(), position.clone());
        Ok(())
    }
}

#[tokio::test]
async fn close_persists_the_final_position_and_revokes_the_source_token() {
    let directory = tempdir().expect("temporary directory");
    let pdf = directory.path().join("paper.pdf");
    fs::write(&pdf, b"%PDF-1.7\nreader fixture").expect("fixture should write");
    let store = Arc::new(MemoryReaderStore {
        source: Mutex::new(source(&pdf, DocumentFileState::Available)),
        positions: Mutex::new(HashMap::new()),
    });
    let registry = Arc::new(ReaderSourceRegistry::default());
    let reader = DefaultDocumentReader::new(store, registry.clone());
    let opened = reader
        .open(DocumentId::from("document-1"))
        .await
        .expect("reader should open");
    assert_eq!(opened.position.page, 1);
    assert_eq!(opened.position.scale_value, "page-width");

    reader
        .save_position(
            &opened.source_token,
            ReadingPositionUpdate {
                page: 2,
                page_offset_ratio: 0.25,
                scale_value: "1.25".to_owned(),
            },
        )
        .await
        .expect("periodic position should save");
    reader
        .close(
            &opened.source_token,
            Some(ReadingPositionUpdate {
                page: 3,
                page_offset_ratio: 0.75,
                scale_value: "page-fit".to_owned(),
            }),
        )
        .await
        .expect("reader should close");
    assert!(
        registry
            .resolve(&opened.source_token)
            .expect("registry should resolve")
            .is_none()
    );

    let reopened = reader
        .open(DocumentId::from("document-1"))
        .await
        .expect("reader should reopen");
    assert_eq!(reopened.position.page, 3);
    assert_eq!(reopened.position.page_offset_ratio, 0.75);
    assert_eq!(reopened.position.scale_value, "page-fit");
}

#[tokio::test]
async fn open_rejects_a_source_that_changed_after_import() {
    let directory = tempdir().expect("temporary directory");
    let pdf = directory.path().join("paper.pdf");
    fs::write(&pdf, b"%PDF-1.7\nreader fixture").expect("fixture should write");
    let store = Arc::new(MemoryReaderStore {
        source: Mutex::new(source(&pdf, DocumentFileState::Available)),
        positions: Mutex::new(HashMap::new()),
    });
    fs::write(&pdf, b"%PDF-1.7\nchanged reader fixture").expect("fixture should change");
    let reader = DefaultDocumentReader::new(store, Arc::new(ReaderSourceRegistry::default()));

    let error = reader
        .open(DocumentId::from("document-1"))
        .await
        .expect_err("changed source should fail");

    assert_eq!(error.code, AtlasErrorCode::DocumentChanged);
}

#[tokio::test]
async fn position_validation_rejects_invalid_page_offset_and_scale() {
    let directory = tempdir().expect("temporary directory");
    let pdf = directory.path().join("paper.pdf");
    fs::write(&pdf, b"%PDF-1.7\nreader fixture").expect("fixture should write");
    let store = Arc::new(MemoryReaderStore {
        source: Mutex::new(source(&pdf, DocumentFileState::Available)),
        positions: Mutex::new(HashMap::new()),
    });
    let reader = DefaultDocumentReader::new(store, Arc::new(ReaderSourceRegistry::default()));
    let opened = reader
        .open(DocumentId::from("document-1"))
        .await
        .expect("reader should open");

    for update in [
        ReadingPositionUpdate {
            page: 0,
            page_offset_ratio: 0.0,
            scale_value: "page-width".to_owned(),
        },
        ReadingPositionUpdate {
            page: 1,
            page_offset_ratio: 1.1,
            scale_value: "page-width".to_owned(),
        },
        ReadingPositionUpdate {
            page: 1,
            page_offset_ratio: 0.0,
            scale_value: "unbounded".to_owned(),
        },
    ] {
        let error = reader
            .save_position(&opened.source_token, update)
            .await
            .expect_err("invalid position should fail");
        assert_eq!(error.code, AtlasErrorCode::InvalidInput);
    }
}

#[tokio::test]
async fn opening_the_same_document_revokes_the_previous_token() {
    let directory = tempdir().expect("temporary directory");
    let pdf = directory.path().join("paper.pdf");
    fs::write(&pdf, b"%PDF-1.7\nreader fixture").expect("fixture should write");
    let store = Arc::new(MemoryReaderStore {
        source: Mutex::new(source(&pdf, DocumentFileState::Available)),
        positions: Mutex::new(HashMap::new()),
    });
    let registry = Arc::new(ReaderSourceRegistry::default());
    let reader = DefaultDocumentReader::new(store, registry.clone());
    let first = reader
        .open(DocumentId::from("document-1"))
        .await
        .expect("first reader should open");
    let second = reader
        .open(DocumentId::from("document-1"))
        .await
        .expect("second reader should open");

    assert_ne!(first.source_token, second.source_token);
    assert!(
        registry
            .resolve(&first.source_token)
            .expect("registry should resolve")
            .is_none()
    );
    assert!(
        registry
            .resolve(&second.source_token)
            .expect("registry should resolve")
            .is_some()
    );

    reader
        .save_position(
            &second.source_token,
            ReadingPositionUpdate {
                page: 4,
                page_offset_ratio: 0.5,
                scale_value: "page-fit".to_owned(),
            },
        )
        .await
        .expect("active session should save");
    let stale = reader
        .save_position(
            &first.source_token,
            ReadingPositionUpdate {
                page: 1,
                page_offset_ratio: 0.0,
                scale_value: "page-width".to_owned(),
            },
        )
        .await
        .expect_err("superseded session must not overwrite the newer position");
    assert_eq!(stale.code, AtlasErrorCode::NotFound);

    let reopened = reader
        .open(DocumentId::from("document-1"))
        .await
        .expect("reader should reopen");
    assert_eq!(reopened.position.page, 4);
    assert_eq!(reopened.position.scale_value, "page-fit");
}

fn source(path: &std::path::Path, file_state: DocumentFileState) -> ReaderDocumentSource {
    let metadata = fs::metadata(path).expect("fixture metadata");
    let modified = metadata
        .modified()
        .expect("fixture modified time")
        .duration_since(UNIX_EPOCH)
        .expect("fixture mtime after epoch");
    ReaderDocumentSource {
        document: DocumentSummary {
            id: DocumentId::from("document-1"),
            title: "Reader Paper".to_owned(),
            authors: vec!["Atlas Team".to_owned()],
            page_count: Some(4),
            file_name: "paper.pdf".to_owned(),
            source_state: file_state,
            last_opened_at: 0,
        },
        file_path: path.to_str().expect("fixture path UTF-8").to_owned(),
        file_size_bytes: metadata.len(),
        file_mtime_ms: u64::try_from(modified.as_millis()).expect("mtime in range"),
        file_state,
    }
}
