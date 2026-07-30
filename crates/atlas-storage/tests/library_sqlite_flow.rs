use std::{fs, sync::Arc};

use atlas_domain::{DocumentFileState, LibraryQuery};
use atlas_library::{DefaultLibraryModule, LibraryModule};
use atlas_storage::{AtlasDatabase, SqliteDocumentStore};
use lopdf::{Document, Object, dictionary};
use tempfile::tempdir;

#[tokio::test]
async fn library_flow_persists_across_database_reopen() {
    let directory = tempdir().expect("temporary directory");
    let database_path = directory.path().join("atlas.sqlite3");
    let original = directory.path().join("paper.pdf");
    let relocated = directory.path().join("moved-paper.pdf");
    write_pdf(&original);

    let imported_id = {
        let database = AtlasDatabase::open(&database_path)
            .await
            .expect("database should open");
        let store = Arc::new(SqliteDocumentStore::new(&database));
        let library = DefaultLibraryModule::new(store);
        let imported = library
            .import_pdf(path_string(&original))
            .await
            .expect("import should succeed");
        assert!(!imported.duplicate);
        imported.document.id
    };

    let database = AtlasDatabase::open(&database_path)
        .await
        .expect("database should reopen");
    let store = Arc::new(SqliteDocumentStore::new(&database));
    let library = DefaultLibraryModule::new(store);
    let page = library
        .query(LibraryQuery::default())
        .await
        .expect("persisted library should query");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].title, "Persistent Paper");

    fs::rename(&original, &relocated).expect("fixture should move");
    let refresh = library
        .refresh_sources()
        .await
        .expect("refresh should succeed");
    assert_eq!(refresh.updated[0].source_state, DocumentFileState::Missing);

    let restored = library
        .relocate(imported_id.clone(), path_string(&relocated))
        .await
        .expect("relocate should succeed");
    assert_eq!(restored.source_state, DocumentFileState::Available);
    assert_eq!(restored.file_name, "moved-paper.pdf");

    library
        .remove(imported_id)
        .await
        .expect("remove should succeed");
    assert!(relocated.exists(), "source PDF must remain on disk");
    assert!(
        library
            .query(LibraryQuery::default())
            .await
            .expect("empty library should query")
            .items
            .is_empty()
    );
}

fn write_pdf(path: &std::path::Path) {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    let info_id = document.add_object(dictionary! {
        "Title" => Object::string_literal("Persistent Paper"),
        "Author" => Object::string_literal("Atlas Team"),
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
