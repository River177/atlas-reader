use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::Mutex,
};

use atlas_domain::{AtlasError, DocumentId, ReaderSourceToken};
use uuid::Uuid;

const MAX_ACTIVE_SOURCES: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedPdfSource {
    pub document_id: DocumentId,
    pub path: PathBuf,
    pub file_size_bytes: u64,
    pub file_mtime_ms: u64,
    pub page_count: Option<u32>,
}

#[derive(Debug, Default)]
struct RegistryState {
    sources: HashMap<ReaderSourceToken, AuthorizedPdfSource>,
    token_by_document: HashMap<DocumentId, ReaderSourceToken>,
    issue_order: VecDeque<ReaderSourceToken>,
}

#[derive(Debug, Default)]
pub struct ReaderSourceRegistry {
    state: Mutex<RegistryState>,
}

impl ReaderSourceRegistry {
    pub fn issue(&self, source: AuthorizedPdfSource) -> Result<ReaderSourceToken, AtlasError> {
        let mut state = self.lock()?;
        if let Some(previous) = state.token_by_document.remove(&source.document_id) {
            state.sources.remove(&previous);
            state.issue_order.retain(|token| token != &previous);
        }

        let token = ReaderSourceToken::new(Uuid::new_v4().to_string());
        state
            .token_by_document
            .insert(source.document_id.clone(), token.clone());
        state.sources.insert(token.clone(), source);
        state.issue_order.push_back(token.clone());

        while state.issue_order.len() > MAX_ACTIVE_SOURCES {
            if let Some(expired) = state.issue_order.pop_front()
                && let Some(expired_source) = state.sources.remove(&expired)
            {
                state.token_by_document.remove(&expired_source.document_id);
            }
        }
        Ok(token)
    }

    pub fn resolve(
        &self,
        token: &ReaderSourceToken,
    ) -> Result<Option<AuthorizedPdfSource>, AtlasError> {
        Ok(self.lock()?.sources.get(token).cloned())
    }

    pub fn revoke(&self, token: &ReaderSourceToken) -> Result<bool, AtlasError> {
        let mut state = self.lock()?;
        let Some(source) = state.sources.remove(token) else {
            return Ok(false);
        };
        state.token_by_document.remove(&source.document_id);
        state.issue_order.retain(|issued| issued != token);
        Ok(true)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, RegistryState>, AtlasError> {
        self.state
            .lock()
            .map_err(|_| AtlasError::internal("reader source registry is unavailable"))
    }
}
