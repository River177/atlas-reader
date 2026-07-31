mod parse_store;
mod translation_store;

use std::{path::Path, str::FromStr};

use async_trait::async_trait;
use atlas_document_reader::{ReaderDocumentSource, ReaderStore};
use atlas_domain::{
    AtlasError, DocumentFileState, DocumentId, DocumentSummary, LibrarySort, ProviderKind,
    ReadingPosition,
};
use atlas_library::{
    DocumentImport, DocumentListRequest, DocumentRecord, DocumentSourceUpdate, DocumentStore,
    StoredImport,
};
use atlas_provider_settings::{ADAPTER_PROTOCOL_VERSION, ProviderProfile, ProviderSettingsStore};
use sqlx::{
    Row, SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

pub use parse_store::SqliteParseStore;
pub use translation_store::SqliteTranslationStore;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Clone, Debug)]
pub struct AtlasDatabase {
    pool: SqlitePool,
}

impl AtlasDatabase {
    pub async fn open(path: &Path) -> Result<Self, AtlasError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| AtlasError::storage(error.to_string()))?;
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .map_err(|error| AtlasError::storage(error.to_string()))?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal);
        Self::connect(options, 5).await
    }

    pub async fn open_in_memory() -> Result<Self, AtlasError> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(|error| AtlasError::storage(error.to_string()))?
            .foreign_keys(true);
        Self::connect(options, 1).await
    }

    async fn connect(
        options: SqliteConnectOptions,
        max_connections: u32,
    ) -> Result<Self, AtlasError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect_with(options)
            .await
            .map_err(map_sqlx)?;
        MIGRATOR.run(&pool).await.map_err(map_migrate)?;
        Ok(Self { pool })
    }

    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[derive(Clone, Debug)]
pub struct SqliteDocumentStore {
    pool: SqlitePool,
}

impl SqliteDocumentStore {
    #[must_use]
    pub fn new(database: &AtlasDatabase) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }
}

#[async_trait]
impl DocumentStore for SqliteDocumentStore {
    async fn list(
        &self,
        request: &DocumentListRequest,
    ) -> Result<Vec<DocumentSummary>, AtlasError> {
        let pattern = request.text.as_ref().map(|text| format!("%{text}%"));
        let rows = match request.sort {
            LibrarySort::Recent => {
                sqlx::query(
                    "SELECT id, sha256, title, authors_json, page_count, file_path,
                            file_size_bytes, file_mtime_ms, file_state, last_opened_at
                     FROM documents
                     WHERE (
                       ?1 IS NULL
                       OR title LIKE ?1 COLLATE NOCASE
                       OR authors_json LIKE ?1 COLLATE NOCASE
                     )
                     ORDER BY last_opened_at DESC, title COLLATE NOCASE ASC
                     LIMIT ?2 OFFSET ?3",
                )
                .bind(pattern)
                .bind(i64::from(request.limit))
                .bind(i64::from(request.offset))
                .fetch_all(&self.pool)
                .await
            }
            LibrarySort::Title => {
                sqlx::query(
                    "SELECT id, sha256, title, authors_json, page_count, file_path,
                            file_size_bytes, file_mtime_ms, file_state, last_opened_at
                     FROM documents
                     WHERE (
                       ?1 IS NULL
                       OR title LIKE ?1 COLLATE NOCASE
                       OR authors_json LIKE ?1 COLLATE NOCASE
                     )
                     ORDER BY title COLLATE NOCASE ASC, last_opened_at DESC
                     LIMIT ?2 OFFSET ?3",
                )
                .bind(pattern)
                .bind(i64::from(request.limit))
                .bind(i64::from(request.offset))
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(map_sqlx)?;

        rows.into_iter()
            .map(row_to_record)
            .map(|record| record.map(|value| value.summary()))
            .collect()
    }

    async fn import(&self, input: &DocumentImport) -> Result<StoredImport, AtlasError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        let duplicate =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM documents WHERE sha256 = ?1")
                .bind(&input.sha256)
                .fetch_one(&mut *transaction)
                .await
                .map_err(map_sqlx)?
                > 0;
        let authors_json = serde_json::to_string(&input.authors)
            .map_err(|error| AtlasError::storage(error.to_string()))?;

        sqlx::query(
            "INSERT INTO documents (
               id, sha256, title, authors_json, page_count, file_path,
               file_size_bytes, file_mtime_ms, file_state, created_at,
               updated_at, last_opened_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'available', ?9, ?9, ?9)
             ON CONFLICT(sha256) DO UPDATE SET
               title = excluded.title,
               authors_json = excluded.authors_json,
               page_count = excluded.page_count,
               file_path = excluded.file_path,
               file_size_bytes = excluded.file_size_bytes,
               file_mtime_ms = excluded.file_mtime_ms,
               file_state = 'available',
               updated_at = excluded.updated_at,
               last_opened_at = excluded.last_opened_at",
        )
        .bind(input.id.as_str())
        .bind(&input.sha256)
        .bind(&input.title)
        .bind(authors_json)
        .bind(i64::from(input.page_count))
        .bind(&input.file_path)
        .bind(to_i64(input.file_size_bytes, "file size")?)
        .bind(to_i64(input.file_mtime_ms, "file modification time")?)
        .bind(to_i64(input.imported_at, "import time")?)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        let row = select_record_by_sha256(&mut transaction, &input.sha256)
            .await?
            .ok_or_else(|| AtlasError::storage("imported document was not persisted"))?;
        transaction.commit().await.map_err(map_sqlx)?;

        Ok(StoredImport {
            document: row,
            duplicate,
        })
    }

    async fn get(&self, document_id: &DocumentId) -> Result<Option<DocumentRecord>, AtlasError> {
        let row = sqlx::query(
            "SELECT id, sha256, title, authors_json, page_count, file_path,
                    file_size_bytes, file_mtime_ms, file_state, last_opened_at
             FROM documents
             WHERE id = ?1",
        )
        .bind(document_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        row.map(row_to_record).transpose()
    }

    async fn list_sources(&self) -> Result<Vec<DocumentRecord>, AtlasError> {
        sqlx::query(
            "SELECT id, sha256, title, authors_json, page_count, file_path,
                    file_size_bytes, file_mtime_ms, file_state, last_opened_at
             FROM documents
             ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?
        .into_iter()
        .map(row_to_record)
        .collect()
    }

    async fn update_source(
        &self,
        document_id: &DocumentId,
        update: &DocumentSourceUpdate,
        updated_at: u64,
    ) -> Result<DocumentRecord, AtlasError> {
        let result = sqlx::query(
            "UPDATE documents
             SET file_path = ?2,
                 file_size_bytes = ?3,
                 file_mtime_ms = ?4,
                 file_state = ?5,
                 updated_at = ?6
             WHERE id = ?1",
        )
        .bind(document_id.as_str())
        .bind(&update.file_path)
        .bind(to_i64(update.file_size_bytes, "file size")?)
        .bind(to_i64(update.file_mtime_ms, "file modification time")?)
        .bind(file_state_value(update.file_state))
        .bind(to_i64(updated_at, "update time")?)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx)?;
        if result.rows_affected() == 0 {
            return Err(AtlasError::not_found("document was not found"));
        }
        self.get(document_id)
            .await?
            .ok_or_else(|| AtlasError::storage("updated document could not be loaded"))
    }

    async fn remove(&self, document_id: &DocumentId) -> Result<bool, AtlasError> {
        let result = sqlx::query("DELETE FROM documents WHERE id = ?1")
            .bind(document_id.as_str())
            .execute(&self.pool)
            .await
            .map_err(map_sqlx)?;
        Ok(result.rows_affected() > 0)
    }
}

#[async_trait]
impl ReaderStore for SqliteDocumentStore {
    async fn open_source(
        &self,
        document_id: &DocumentId,
        opened_at: u64,
    ) -> Result<Option<ReaderDocumentSource>, AtlasError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        let updated = sqlx::query(
            "UPDATE documents
             SET last_opened_at = ?2, updated_at = ?2
             WHERE id = ?1",
        )
        .bind(document_id.as_str())
        .bind(to_i64(opened_at, "opened time")?)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        if updated.rows_affected() == 0 {
            transaction.commit().await.map_err(map_sqlx)?;
            return Ok(None);
        }
        let row = sqlx::query(
            "SELECT id, sha256, title, authors_json, page_count, file_path,
                    file_size_bytes, file_mtime_ms, file_state, last_opened_at
             FROM documents
             WHERE id = ?1",
        )
        .bind(document_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        transaction.commit().await.map_err(map_sqlx)?;
        let record = row_to_record(row)?;

        Ok(Some(ReaderDocumentSource {
            document: record.summary(),
            file_path: record.file_path,
            file_size_bytes: record.file_size_bytes,
            file_mtime_ms: record.file_mtime_ms,
            file_state: record.file_state,
        }))
    }

    async fn load_position(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<ReadingPosition>, AtlasError> {
        let row = sqlx::query(
            "SELECT pdf_page, pdf_scroll_offset, pdf_scale_value, updated_at
             FROM reading_positions
             WHERE document_id = ?1 AND view_mode = 'pdf'",
        )
        .bind(document_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;

        row.map(|row| {
            Ok(ReadingPosition {
                page: u32::try_from(row.try_get::<i64, _>("pdf_page").map_err(map_sqlx)?)
                    .map_err(|_| AtlasError::storage("PDF page is outside u32 range"))?,
                page_offset_ratio: row.try_get("pdf_scroll_offset").map_err(map_sqlx)?,
                scale_value: row.try_get("pdf_scale_value").map_err(map_sqlx)?,
                updated_at: to_u64(
                    row.try_get::<i64, _>("updated_at").map_err(map_sqlx)?,
                    "position update time",
                )?,
            })
        })
        .transpose()
    }

    async fn save_position(
        &self,
        document_id: &DocumentId,
        position: &ReadingPosition,
    ) -> Result<(), AtlasError> {
        sqlx::query(
            "INSERT INTO reading_positions (
               document_id, chapter_id, block_id, pdf_page, pdf_scroll_offset,
               view_mode, updated_at, pdf_scale_value
             ) VALUES (?1, NULL, NULL, ?2, ?3, 'pdf', ?4, ?5)
             ON CONFLICT(document_id) DO UPDATE SET
               chapter_id = NULL,
               block_id = NULL,
               pdf_page = excluded.pdf_page,
               pdf_scroll_offset = excluded.pdf_scroll_offset,
               view_mode = 'pdf',
               updated_at = excluded.updated_at,
               pdf_scale_value = excluded.pdf_scale_value",
        )
        .bind(document_id.as_str())
        .bind(i64::from(position.page))
        .bind(position.page_offset_ratio)
        .bind(to_i64(position.updated_at, "position update time")?)
        .bind(&position.scale_value)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx)?;
        Ok(())
    }
}

async fn select_record_by_sha256(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    sha256: &str,
) -> Result<Option<DocumentRecord>, AtlasError> {
    let row = sqlx::query(
        "SELECT id, sha256, title, authors_json, page_count, file_path,
                file_size_bytes, file_mtime_ms, file_state, last_opened_at
         FROM documents
         WHERE sha256 = ?1",
    )
    .bind(sha256)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_sqlx)?;
    row.map(row_to_record).transpose()
}

fn row_to_record(row: sqlx::sqlite::SqliteRow) -> Result<DocumentRecord, AtlasError> {
    let authors_json: String = row.try_get("authors_json").map_err(map_sqlx)?;
    let authors = serde_json::from_str(&authors_json)
        .map_err(|error| AtlasError::storage(error.to_string()))?;
    let page_count = row
        .try_get::<Option<i64>, _>("page_count")
        .map_err(map_sqlx)?
        .map(|value| {
            u32::try_from(value).map_err(|_| AtlasError::storage("page count is outside u32 range"))
        })
        .transpose()?;
    let file_state: String = row.try_get("file_state").map_err(map_sqlx)?;

    Ok(DocumentRecord {
        id: DocumentId::new(row.try_get::<String, _>("id").map_err(map_sqlx)?),
        sha256: row.try_get("sha256").map_err(map_sqlx)?,
        title: row.try_get("title").map_err(map_sqlx)?,
        authors,
        page_count,
        file_path: row.try_get("file_path").map_err(map_sqlx)?,
        file_size_bytes: to_u64(
            row.try_get::<i64, _>("file_size_bytes").map_err(map_sqlx)?,
            "file size",
        )?,
        file_mtime_ms: to_u64(
            row.try_get::<i64, _>("file_mtime_ms").map_err(map_sqlx)?,
            "file modification time",
        )?,
        file_state: parse_file_state(&file_state)?,
        last_opened_at: to_u64(
            row.try_get::<i64, _>("last_opened_at").map_err(map_sqlx)?,
            "last opened time",
        )?,
    })
}

/// Provider configuration without credentials. API keys live in the operating
/// system keychain, so this table only stores the account that points at them.
#[derive(Clone, Debug)]
pub struct SqliteProviderSettingsStore {
    pool: SqlitePool,
}

impl SqliteProviderSettingsStore {
    #[must_use]
    pub fn new(database: &AtlasDatabase) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }
}

#[async_trait]
impl ProviderSettingsStore for SqliteProviderSettingsStore {
    async fn load_profiles(&self) -> Result<Vec<ProviderProfile>, AtlasError> {
        let rows = sqlx::query(
            "SELECT kind, endpoint_origin, base_path, endpoint_fingerprint, model_id,
                    context_window_override, automatic_cloud_parsing_enabled, secret_account
             FROM provider_profiles
             ORDER BY kind",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;

        rows.into_iter()
            .map(|row| {
                let kind: String = row.try_get("kind").map_err(map_sqlx)?;
                let context_window_override: Option<i64> =
                    row.try_get("context_window_override").map_err(map_sqlx)?;
                Ok(ProviderProfile {
                    kind: parse_provider_kind(&kind)?,
                    endpoint_origin: row.try_get("endpoint_origin").map_err(map_sqlx)?,
                    base_path: row.try_get("base_path").map_err(map_sqlx)?,
                    endpoint_fingerprint: row.try_get("endpoint_fingerprint").map_err(map_sqlx)?,
                    model_id: row.try_get("model_id").map_err(map_sqlx)?,
                    context_window_override: context_window_override
                        .map(|value| {
                            u32::try_from(value).map_err(|_| {
                                AtlasError::storage("context window is outside u32 range")
                            })
                        })
                        .transpose()?,
                    automatic_cloud_parsing_enabled: row
                        .try_get::<i64, _>("automatic_cloud_parsing_enabled")
                        .map_err(map_sqlx)?
                        != 0,
                    secret_account: row.try_get("secret_account").map_err(map_sqlx)?,
                })
            })
            .collect()
    }

    async fn save_profile(
        &self,
        profile: &ProviderProfile,
        saved_at: u64,
    ) -> Result<(), AtlasError> {
        let saved_at = to_i64(saved_at, "provider save time")?;
        let context_window_override = profile.context_window_override.map(i64::from);

        sqlx::query(
            "INSERT INTO provider_profiles (
                id, kind, display_name, endpoint_origin, base_path, endpoint_fingerprint,
                adapter_protocol_version, model_id, context_window_override, secret_account,
                automatic_cloud_parsing_enabled, created_at, updated_at
             ) VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
             ON CONFLICT(id) DO UPDATE SET
                endpoint_origin = excluded.endpoint_origin,
                base_path = excluded.base_path,
                endpoint_fingerprint = excluded.endpoint_fingerprint,
                adapter_protocol_version = excluded.adapter_protocol_version,
                model_id = excluded.model_id,
                context_window_override = excluded.context_window_override,
                secret_account = excluded.secret_account,
                automatic_cloud_parsing_enabled = excluded.automatic_cloud_parsing_enabled,
                updated_at = excluded.updated_at",
        )
        .bind(profile.kind.as_str())
        .bind(profile.kind.display_name())
        .bind(&profile.endpoint_origin)
        .bind(&profile.base_path)
        .bind(&profile.endpoint_fingerprint)
        .bind(ADAPTER_PROTOCOL_VERSION)
        .bind(profile.model_id.as_deref())
        .bind(context_window_override)
        .bind(&profile.secret_account)
        .bind(i64::from(profile.automatic_cloud_parsing_enabled))
        .bind(saved_at)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx)?;

        Ok(())
    }
}

fn parse_provider_kind(value: &str) -> Result<ProviderKind, AtlasError> {
    match value {
        "cloud_mineru" => Ok(ProviderKind::Mineru),
        "openai_compatible" => Ok(ProviderKind::Translation),
        _ => Err(AtlasError::storage(format!(
            "unknown provider kind: {value}"
        ))),
    }
}

fn parse_file_state(value: &str) -> Result<DocumentFileState, AtlasError> {
    match value {
        "available" => Ok(DocumentFileState::Available),
        "missing" => Ok(DocumentFileState::Missing),
        "changed" => Ok(DocumentFileState::Changed),
        "unreadable" => Ok(DocumentFileState::Unreadable),
        _ => Err(AtlasError::storage(format!(
            "unknown document file state: {value}"
        ))),
    }
}

fn file_state_value(value: DocumentFileState) -> &'static str {
    match value {
        DocumentFileState::Available => "available",
        DocumentFileState::Missing => "missing",
        DocumentFileState::Changed => "changed",
        DocumentFileState::Unreadable => "unreadable",
    }
}

fn to_i64(value: u64, field: &str) -> Result<i64, AtlasError> {
    i64::try_from(value).map_err(|_| AtlasError::storage(format!("{field} is outside i64 range")))
}

fn to_u64(value: i64, field: &str) -> Result<u64, AtlasError> {
    u64::try_from(value).map_err(|_| AtlasError::storage(format!("{field} cannot be negative")))
}

fn map_sqlx(error: sqlx::Error) -> AtlasError {
    AtlasError::storage(error.to_string())
}

fn map_migrate(error: sqlx::migrate::MigrateError) -> AtlasError {
    AtlasError::storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_create_a_queryable_library() {
        let database = AtlasDatabase::open_in_memory()
            .await
            .expect("database should open");
        sqlx::query(
            "INSERT INTO documents (
                id, sha256, title, authors_json, page_count, file_path,
                file_size_bytes, file_mtime_ms, file_state, created_at,
                updated_at, last_opened_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )
        .bind("document-1")
        .bind("sha256")
        .bind("A Maintainable Reader")
        .bind(r#"["Ada Researcher"]"#)
        .bind(24_i64)
        .bind("/tmp/paper.pdf")
        .bind(1024_i64)
        .bind(1_i64)
        .bind("available")
        .bind(1_i64)
        .bind(1_i64)
        .bind(2_i64)
        .execute(database.pool())
        .await
        .expect("fixture should insert");

        let store = SqliteDocumentStore::new(&database);
        let documents = store
            .list(&DocumentListRequest {
                text: Some("maintainable".to_owned()),
                sort: LibrarySort::Recent,
                offset: 0,
                limit: 10,
            })
            .await
            .expect("documents should load");

        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].title, "A Maintainable Reader");
        assert_eq!(documents[0].source_state, DocumentFileState::Available);
    }

    #[tokio::test]
    async fn import_is_atomic_and_duplicate_updates_the_existing_record() {
        let database = AtlasDatabase::open_in_memory()
            .await
            .expect("database should open");
        let store = SqliteDocumentStore::new(&database);
        let first = DocumentImport {
            id: DocumentId::from("document-1"),
            sha256: "same-content".to_owned(),
            title: "First title".to_owned(),
            authors: vec!["Ada".to_owned()],
            page_count: 10,
            file_path: "/tmp/first.pdf".to_owned(),
            file_size_bytes: 100,
            file_mtime_ms: 1,
            imported_at: 1,
        };
        let second = DocumentImport {
            id: DocumentId::from("document-2"),
            title: "Updated title".to_owned(),
            file_path: "/tmp/second.pdf".to_owned(),
            imported_at: 2,
            ..first.clone()
        };

        let inserted = store.import(&first).await.expect("first import");
        let duplicate = store.import(&second).await.expect("duplicate import");
        let sources = store.list_sources().await.expect("sources should load");

        assert!(!inserted.duplicate);
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.document.id, DocumentId::from("document-1"));
        assert_eq!(duplicate.document.title, "Updated title");
        assert_eq!(duplicate.document.file_path, "/tmp/second.pdf");
        assert_eq!(sources.len(), 1);
    }

    #[tokio::test]
    async fn provider_profiles_upsert_by_kind_and_survive_reload() {
        let database = AtlasDatabase::open_in_memory()
            .await
            .expect("database should open");
        let store = SqliteProviderSettingsStore::new(&database);
        let mineru = ProviderProfile {
            kind: ProviderKind::Mineru,
            endpoint_origin: "https://mineru.example.com".to_owned(),
            base_path: "/api/v4".to_owned(),
            endpoint_fingerprint: "fingerprint-1".to_owned(),
            model_id: None,
            context_window_override: None,
            automatic_cloud_parsing_enabled: true,
            secret_account: "atlas.cloud_mineru".to_owned(),
        };
        let translation = ProviderProfile {
            kind: ProviderKind::Translation,
            endpoint_origin: "https://models.example.com".to_owned(),
            base_path: "/v1".to_owned(),
            endpoint_fingerprint: "fingerprint-2".to_owned(),
            model_id: Some("gpt-4o-mini".to_owned()),
            context_window_override: Some(128_000),
            automatic_cloud_parsing_enabled: false,
            secret_account: "atlas.openai_compatible".to_owned(),
        };

        store
            .save_profile(&mineru, 10)
            .await
            .expect("mineru profile should save");
        store
            .save_profile(&translation, 11)
            .await
            .expect("translation profile should save");
        store
            .save_profile(
                &ProviderProfile {
                    base_path: "/api/v5".to_owned(),
                    automatic_cloud_parsing_enabled: false,
                    ..mineru.clone()
                },
                12,
            )
            .await
            .expect("mineru profile should update in place");

        let profiles = store
            .load_profiles()
            .await
            .expect("profiles should load")
            .into_iter()
            .map(|profile| (profile.kind, profile))
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(profiles.len(), 2);
        let stored_mineru = &profiles[&ProviderKind::Mineru];
        assert_eq!(stored_mineru.base_path, "/api/v5");
        assert!(!stored_mineru.automatic_cloud_parsing_enabled);
        assert_eq!(stored_mineru.endpoint_fingerprint, "fingerprint-1");
        assert_eq!(profiles[&ProviderKind::Translation], translation);

        let versions: Vec<String> =
            sqlx::query_scalar("SELECT adapter_protocol_version FROM provider_profiles")
                .fetch_all(database.pool())
                .await
                .expect("versions should load");
        assert!(
            versions
                .iter()
                .all(|version| version == ADAPTER_PROTOCOL_VERSION)
        );
    }
}
