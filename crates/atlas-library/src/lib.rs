use std::sync::Arc;

use async_trait::async_trait;
use atlas_domain::{AtlasError, DocumentSummary, LibraryPage, LibraryQuery, LibrarySort};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentListRequest {
    pub text: Option<String>,
    pub sort: LibrarySort,
    pub offset: u32,
    pub limit: u32,
}

#[async_trait]
pub trait DocumentStore: Send + Sync {
    async fn list(&self, request: &DocumentListRequest)
    -> Result<Vec<DocumentSummary>, AtlasError>;
}

#[async_trait]
pub trait LibraryModule: Send + Sync {
    async fn query(&self, input: LibraryQuery) -> Result<LibraryPage, AtlasError>;
}

#[derive(Clone)]
pub struct DefaultLibraryModule {
    store: Arc<dyn DocumentStore>,
}

impl std::fmt::Debug for DefaultLibraryModule {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DefaultLibraryModule")
            .finish_non_exhaustive()
    }
}

impl DefaultLibraryModule {
    #[must_use]
    pub fn new(store: Arc<dyn DocumentStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl LibraryModule for DefaultLibraryModule {
    async fn query(&self, input: LibraryQuery) -> Result<LibraryPage, AtlasError> {
        if !(1..=100).contains(&input.limit) {
            return Err(AtlasError::invalid_input(
                "library query limit must be between 1 and 100",
            ));
        }

        let offset = parse_cursor(input.cursor.as_deref())?;
        let text = input
            .text
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let requested_limit = input.limit;
        let request = DocumentListRequest {
            text,
            sort: input.sort,
            offset,
            limit: requested_limit + 1,
        };
        let mut items = self.store.list(&request).await?;
        let has_more = items.len() > requested_limit as usize;
        items.truncate(requested_limit as usize);

        Ok(LibraryPage {
            items,
            next_cursor: has_more.then(|| (offset + requested_limit).to_string()),
        })
    }
}

fn parse_cursor(cursor: Option<&str>) -> Result<u32, AtlasError> {
    match cursor {
        None => Ok(0),
        Some(value) => value
            .parse::<u32>()
            .map_err(|_| AtlasError::invalid_input("library cursor is invalid")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_domain::{DocumentId, LibrarySort};

    #[derive(Debug)]
    struct MemoryDocumentStore {
        documents: Vec<DocumentSummary>,
    }

    #[async_trait]
    impl DocumentStore for MemoryDocumentStore {
        async fn list(
            &self,
            request: &DocumentListRequest,
        ) -> Result<Vec<DocumentSummary>, AtlasError> {
            let mut documents = self.documents.clone();
            if let Some(text) = &request.text {
                let needle = text.to_lowercase();
                documents.retain(|document| {
                    document.title.to_lowercase().contains(&needle)
                        || document
                            .authors
                            .iter()
                            .any(|author| author.to_lowercase().contains(&needle))
                });
            }
            match request.sort {
                LibrarySort::Recent => {
                    documents.sort_by_key(|document| std::cmp::Reverse(document.last_opened_at));
                }
                LibrarySort::Title => {
                    documents.sort_by_key(|document| document.title.to_lowercase());
                }
            }
            Ok(documents
                .into_iter()
                .skip(request.offset as usize)
                .take(request.limit as usize)
                .collect())
        }
    }

    fn document(id: &str, title: &str, last_opened_at: u64) -> DocumentSummary {
        DocumentSummary {
            id: DocumentId::from(id),
            title: title.to_owned(),
            authors: vec!["Ada Researcher".to_owned()],
            page_count: Some(12),
            source_available: true,
            last_opened_at,
        }
    }

    #[tokio::test]
    async fn query_normalizes_text_and_returns_cursor() {
        let store = Arc::new(MemoryDocumentStore {
            documents: vec![
                document("one", "Atlas Systems", 2),
                document("two", "Atlas Retrieval", 3),
                document("three", "Other Work", 1),
            ],
        });
        let library = DefaultLibraryModule::new(store);

        let first_page = library
            .query(LibraryQuery {
                text: Some("  atlas ".to_owned()),
                sort: LibrarySort::Recent,
                cursor: None,
                limit: 1,
            })
            .await
            .expect("query should succeed");

        assert_eq!(first_page.items[0].title, "Atlas Retrieval");
        assert_eq!(first_page.next_cursor.as_deref(), Some("1"));
    }

    #[tokio::test]
    async fn query_rejects_unbounded_page_sizes() {
        let store = Arc::new(MemoryDocumentStore { documents: vec![] });
        let library = DefaultLibraryModule::new(store);

        let error = library
            .query(LibraryQuery {
                limit: 101,
                ..LibraryQuery::default()
            })
            .await
            .expect_err("large pages should be rejected");

        assert_eq!(error.code, atlas_domain::AtlasErrorCode::InvalidInput);
    }
}
