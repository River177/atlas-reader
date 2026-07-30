use std::{path::Path, str::FromStr};

use async_trait::async_trait;
use atlas_domain::{AtlasError, DocumentId, DocumentSummary, LibrarySort};
use atlas_library::{DocumentListRequest, DocumentStore};
use sqlx::{
    Row, SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

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
                    "SELECT id, title, authors_json, page_count, file_state, last_opened_at
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
                    "SELECT id, title, authors_json, page_count, file_state, last_opened_at
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
            .map(|row| {
                let authors_json: String = row.try_get("authors_json").map_err(map_sqlx)?;
                let authors = serde_json::from_str(&authors_json)
                    .map_err(|error| AtlasError::storage(error.to_string()))?;
                let page_count = row
                    .try_get::<Option<i64>, _>("page_count")
                    .map_err(map_sqlx)?
                    .map(|value| {
                        u32::try_from(value)
                            .map_err(|_| AtlasError::storage("page count is outside u32 range"))
                    })
                    .transpose()?;
                let last_opened_at =
                    u64::try_from(row.try_get::<i64, _>("last_opened_at").map_err(map_sqlx)?)
                        .map_err(|_| AtlasError::storage("last_opened_at cannot be negative"))?;
                let file_state: String = row.try_get("file_state").map_err(map_sqlx)?;

                Ok(DocumentSummary {
                    id: DocumentId::new(row.try_get::<String, _>("id").map_err(map_sqlx)?),
                    title: row.try_get("title").map_err(map_sqlx)?,
                    authors,
                    page_count,
                    source_available: file_state == "available",
                    last_opened_at,
                })
            })
            .collect()
    }
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
        assert!(documents[0].source_available);
    }
}
