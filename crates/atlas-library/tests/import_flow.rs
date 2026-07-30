use std::{
    fs,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use atlas_domain::{
    AtlasError, AtlasErrorCode, DocumentFileState, DocumentId, DocumentSummary, LibraryQuery,
    LibrarySort,
};
use atlas_library::{
    DefaultLibraryModule, DocumentImport, DocumentListRequest, DocumentRecord,
    DocumentSourceUpdate, DocumentStore, LibraryLimits, LibraryModule, StoredImport,
};
use lopdf::{Document, Object, dictionary};
use tempfile::tempdir;

#[derive(Debug, Default)]
struct MemoryDocumentStore {
    records: Mutex<Vec<DocumentRecord>>,
}

impl MemoryDocumentStore {
    fn count(&self) -> usize {
        self.records.lock().expect("records lock").len()
    }
}

#[async_trait]
impl DocumentStore for MemoryDocumentStore {
    async fn list(
        &self,
        request: &DocumentListRequest,
    ) -> Result<Vec<DocumentSummary>, AtlasError> {
        let mut records = self.records.lock().expect("records lock").clone();
        if let Some(text) = &request.text {
            let needle = text.to_lowercase();
            records.retain(|record| {
                record.title.to_lowercase().contains(&needle)
                    || record
                        .authors
                        .iter()
                        .any(|author| author.to_lowercase().contains(&needle))
            });
        }
        match request.sort {
            LibrarySort::Recent => {
                records.sort_by_key(|record| std::cmp::Reverse(record.last_opened_at));
            }
            LibrarySort::Title => {
                records.sort_by_key(|record| record.title.to_lowercase());
            }
        }
        Ok(records
            .into_iter()
            .skip(request.offset as usize)
            .take(request.limit as usize)
            .map(|record| record.summary())
            .collect())
    }

    async fn import(&self, input: &DocumentImport) -> Result<StoredImport, AtlasError> {
        let mut records = self.records.lock().expect("records lock");
        if let Some(record) = records
            .iter_mut()
            .find(|record| record.sha256 == input.sha256)
        {
            record.title.clone_from(&input.title);
            record.authors.clone_from(&input.authors);
            record.page_count = Some(input.page_count);
            record.file_path.clone_from(&input.file_path);
            record.file_size_bytes = input.file_size_bytes;
            record.file_mtime_ms = input.file_mtime_ms;
            record.file_state = DocumentFileState::Available;
            record.last_opened_at = input.imported_at;
            return Ok(StoredImport {
                document: record.clone(),
                duplicate: true,
            });
        }

        let record = DocumentRecord {
            id: input.id.clone(),
            sha256: input.sha256.clone(),
            title: input.title.clone(),
            authors: input.authors.clone(),
            page_count: Some(input.page_count),
            file_path: input.file_path.clone(),
            file_size_bytes: input.file_size_bytes,
            file_mtime_ms: input.file_mtime_ms,
            file_state: DocumentFileState::Available,
            last_opened_at: input.imported_at,
        };
        records.push(record.clone());
        Ok(StoredImport {
            document: record,
            duplicate: false,
        })
    }

    async fn get(&self, document_id: &DocumentId) -> Result<Option<DocumentRecord>, AtlasError> {
        Ok(self
            .records
            .lock()
            .expect("records lock")
            .iter()
            .find(|record| &record.id == document_id)
            .cloned())
    }

    async fn list_sources(&self) -> Result<Vec<DocumentRecord>, AtlasError> {
        Ok(self.records.lock().expect("records lock").clone())
    }

    async fn update_source(
        &self,
        document_id: &DocumentId,
        update: &DocumentSourceUpdate,
        _updated_at: u64,
    ) -> Result<DocumentRecord, AtlasError> {
        let mut records = self.records.lock().expect("records lock");
        let record = records
            .iter_mut()
            .find(|record| &record.id == document_id)
            .ok_or_else(|| AtlasError::not_found("document was not found"))?;
        record.file_path.clone_from(&update.file_path);
        record.file_size_bytes = update.file_size_bytes;
        record.file_mtime_ms = update.file_mtime_ms;
        record.file_state = update.file_state;
        Ok(record.clone())
    }

    async fn remove(&self, document_id: &DocumentId) -> Result<bool, AtlasError> {
        let mut records = self.records.lock().expect("records lock");
        let previous = records.len();
        records.retain(|record| &record.id != document_id);
        Ok(records.len() != previous)
    }
}

#[tokio::test]
async fn import_is_deduplicated_and_remove_preserves_the_source_file() {
    let directory = tempdir().expect("temporary directory");
    let pdf = directory.path().join("paper.pdf");
    write_pdf(&pdf, "Atlas Retrieval", "Ada Researcher", 2);
    let store = Arc::new(MemoryDocumentStore::default());
    let library = DefaultLibraryModule::new(store.clone());

    let first = library
        .import_pdf(path_string(&pdf))
        .await
        .expect("first import should succeed");
    let second = library
        .import_pdf(path_string(&pdf))
        .await
        .expect("duplicate import should succeed");

    assert!(!first.duplicate);
    assert!(second.duplicate);
    assert_eq!(first.document.id, second.document.id);
    assert_eq!(second.document.title, "Atlas Retrieval");
    assert_eq!(second.document.authors, vec!["Ada Researcher"]);
    assert_eq!(second.document.page_count, Some(2));
    assert_eq!(second.document.source_state, DocumentFileState::Available);
    assert_eq!(store.count(), 1);

    library
        .remove(first.document.id)
        .await
        .expect("remove should succeed");
    assert!(pdf.exists(), "removing metadata must not delete the PDF");
    assert_eq!(store.count(), 0);
}

#[tokio::test]
async fn refresh_marks_a_moved_file_missing_and_relocate_restores_it() {
    let directory = tempdir().expect("temporary directory");
    let original = directory.path().join("original.pdf");
    let relocated = directory.path().join("relocated.pdf");
    write_pdf(&original, "Moving Papers", "Atlas Team", 1);
    let store = Arc::new(MemoryDocumentStore::default());
    let library = DefaultLibraryModule::new(store);
    let imported = library
        .import_pdf(path_string(&original))
        .await
        .expect("import should succeed");
    fs::rename(&original, &relocated).expect("fixture should move");

    let refresh = library
        .refresh_sources()
        .await
        .expect("refresh should succeed");
    assert_eq!(refresh.updated.len(), 1);
    assert_eq!(refresh.updated[0].source_state, DocumentFileState::Missing);

    let restored = library
        .relocate(imported.document.id, path_string(&relocated))
        .await
        .expect("relocate should succeed");
    assert_eq!(restored.source_state, DocumentFileState::Available);
    assert_eq!(restored.file_name, "relocated.pdf");
}

#[tokio::test]
async fn relocate_rejects_a_different_pdf() {
    let directory = tempdir().expect("temporary directory");
    let original = directory.path().join("original.pdf");
    let different = directory.path().join("different.pdf");
    write_pdf(&original, "Original", "Atlas Team", 1);
    write_pdf(&different, "Different", "Atlas Team", 1);
    let store = Arc::new(MemoryDocumentStore::default());
    let library = DefaultLibraryModule::new(store);
    let imported = library
        .import_pdf(path_string(&original))
        .await
        .expect("import should succeed");

    let error = library
        .relocate(imported.document.id, path_string(&different))
        .await
        .expect_err("different content must be rejected");

    assert_eq!(error.code, AtlasErrorCode::DocumentChanged);
}

#[tokio::test]
async fn import_rejects_non_pdf_invalid_oversized_and_overlong_documents() {
    let directory = tempdir().expect("temporary directory");
    let text_file = directory.path().join("notes.txt");
    fs::write(&text_file, b"not a pdf").expect("fixture should write");
    let invalid_pdf = directory.path().join("invalid.pdf");
    fs::write(&invalid_pdf, b"%PDF-1.7\nnot a real PDF").expect("fixture should write");
    let oversized_pdf = directory.path().join("oversized.pdf");
    fs::write(&oversized_pdf, b"%PDF-1.7").expect("fixture should write");
    fs::OpenOptions::new()
        .write(true)
        .open(&oversized_pdf)
        .expect("fixture should open")
        .set_len(101)
        .expect("fixture should resize");
    let two_pages = directory.path().join("two-pages.pdf");
    write_pdf(&two_pages, "Two Pages", "Atlas Team", 2);
    let store = Arc::new(MemoryDocumentStore::default());

    let default_library = DefaultLibraryModule::new(store.clone());
    assert_eq!(
        default_library
            .import_pdf(path_string(&text_file))
            .await
            .expect_err("non-PDF should fail")
            .code,
        AtlasErrorCode::UnsupportedFileType
    );
    assert_eq!(
        default_library
            .import_pdf(path_string(&invalid_pdf))
            .await
            .expect_err("invalid PDF should fail")
            .code,
        AtlasErrorCode::InvalidPdf
    );

    let size_limited = DefaultLibraryModule::with_limits(
        store.clone(),
        LibraryLimits {
            max_file_size_bytes: 100,
            max_pages: 500,
        },
    );
    assert_eq!(
        size_limited
            .import_pdf(path_string(&oversized_pdf))
            .await
            .expect_err("oversized PDF should fail")
            .code,
        AtlasErrorCode::PdfTooLarge
    );

    let page_limited = DefaultLibraryModule::with_limits(
        store,
        LibraryLimits {
            max_file_size_bytes: 10 * 1024 * 1024,
            max_pages: 1,
        },
    );
    assert_eq!(
        page_limited
            .import_pdf(path_string(&two_pages))
            .await
            .expect_err("page limit should fail")
            .code,
        AtlasErrorCode::PdfTooManyPages
    );
}

#[tokio::test]
async fn query_trims_search_text_and_paginates_results() {
    let directory = tempdir().expect("temporary directory");
    let first = directory.path().join("first.pdf");
    let second = directory.path().join("second.pdf");
    write_pdf(&first, "Atlas Systems", "Ada", 1);
    write_pdf(&second, "Atlas Retrieval", "Grace", 1);
    let store = Arc::new(MemoryDocumentStore::default());
    let library = DefaultLibraryModule::new(store);
    library
        .import_pdf(path_string(&first))
        .await
        .expect("first import");
    library
        .import_pdf(path_string(&second))
        .await
        .expect("second import");

    let page = library
        .query(LibraryQuery {
            text: Some(" atlas ".to_owned()),
            sort: LibrarySort::Title,
            cursor: None,
            limit: 1,
        })
        .await
        .expect("query should succeed");

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].title, "Atlas Retrieval");
    assert_eq!(page.next_cursor.as_deref(), Some("1"));
}

fn write_pdf(path: &std::path::Path, title: &str, author: &str, page_count: u32) {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let mut pages = Vec::new();
    for _ in 0..page_count {
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        pages.push(Object::Reference(page_id));
    }
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => pages,
            "Count" => i64::from(page_count),
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    let info_id = document.add_object(dictionary! {
        "Title" => Object::string_literal(title),
        "Author" => Object::string_literal(author),
    });
    document.trailer.set("Root", catalog_id);
    document.trailer.set("Info", info_id);
    document.compress();
    document.save(path).expect("fixture PDF should save");
}

fn path_string(path: &std::path::Path) -> String {
    path.to_str()
        .expect("fixture path should be UTF-8")
        .to_owned()
}
