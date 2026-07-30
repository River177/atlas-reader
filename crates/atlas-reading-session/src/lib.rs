use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use async_trait::async_trait;
use atlas_domain::{
    AtlasError, CommandId, CommandReceipt, CommandStatus, DocumentId, OpenSessionInput,
    OpenSessionResult, ParseState, ProviderStatusSnapshot, ReadingCommand, SessionId,
    SessionLifecycle, SessionSnapshot,
};
use tokio::sync::Mutex;
use uuid::Uuid;

const MAX_PROCESSED_COMMANDS: usize = 1_024;

#[async_trait]
pub trait ProviderStatusPort: Send + Sync {
    async fn snapshot(&self) -> ProviderStatusSnapshot;
}

#[async_trait]
pub trait ReadingSessionModule: Send + Sync {
    async fn open(&self, input: OpenSessionInput) -> Result<OpenSessionResult, AtlasError>;

    async fn dispatch(
        &self,
        session_id: &SessionId,
        command_id: CommandId,
        expected_revision: Option<u32>,
        command: ReadingCommand,
    ) -> Result<CommandReceipt, AtlasError>;

    async fn close(&self, session_id: &SessionId) -> Result<(), AtlasError>;
}

#[derive(Default)]
struct Registry {
    sessions: HashMap<SessionId, SessionSnapshot>,
    sessions_by_document: HashMap<DocumentId, SessionId>,
    receipts: HashMap<(SessionId, CommandId), CommandReceipt>,
    receipt_order: VecDeque<(SessionId, CommandId)>,
}

pub struct DefaultReadingSession {
    providers: Arc<dyn ProviderStatusPort>,
    registry: Mutex<Registry>,
}

impl std::fmt::Debug for DefaultReadingSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DefaultReadingSession")
            .finish_non_exhaustive()
    }
}

impl DefaultReadingSession {
    #[must_use]
    pub fn new(providers: Arc<dyn ProviderStatusPort>) -> Self {
        Self {
            providers,
            registry: Mutex::new(Registry::default()),
        }
    }
}

#[async_trait]
impl ReadingSessionModule for DefaultReadingSession {
    async fn open(&self, input: OpenSessionInput) -> Result<OpenSessionResult, AtlasError> {
        if input.document_id.as_str().trim().is_empty() {
            return Err(AtlasError::invalid_input("document id cannot be empty"));
        }

        let provider_status = self.providers.snapshot().await;
        let mut registry = self.registry.lock().await;
        if let Some(session_id) = registry.sessions_by_document.get(&input.document_id)
            && let Some(snapshot) = registry.sessions.get(session_id)
        {
            return Ok(OpenSessionResult {
                session_id: session_id.clone(),
                restored: true,
                snapshot: snapshot.clone(),
            });
        }

        let session_id = SessionId::new(Uuid::new_v4().to_string());
        let snapshot = SessionSnapshot {
            schema_version: 1,
            session_id: session_id.clone(),
            document_id: input.document_id.clone(),
            revision: 0,
            lifecycle: SessionLifecycle::Ready,
            parse_state: ParseState::NotStarted,
            active_chapter_id: input.initial_chapter_id,
            active_job_ids: Vec::new(),
            provider_status,
        };
        registry
            .sessions_by_document
            .insert(input.document_id, session_id.clone());
        registry
            .sessions
            .insert(session_id.clone(), snapshot.clone());

        Ok(OpenSessionResult {
            session_id,
            restored: false,
            snapshot,
        })
    }

    async fn dispatch(
        &self,
        session_id: &SessionId,
        command_id: CommandId,
        expected_revision: Option<u32>,
        command: ReadingCommand,
    ) -> Result<CommandReceipt, AtlasError> {
        let mut registry = self.registry.lock().await;
        let receipt_key = (session_id.clone(), command_id.clone());
        if let Some(receipt) = registry.receipts.get(&receipt_key) {
            let mut duplicate = receipt.clone();
            duplicate.status = CommandStatus::Duplicate;
            return Ok(duplicate);
        }

        let snapshot = registry
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| AtlasError::not_found("reading session was not found"))?;

        if let Some(expected) = expected_revision
            && expected != snapshot.revision
        {
            let receipt = CommandReceipt {
                command_id,
                status: CommandStatus::Rejected,
                revision: snapshot.revision,
                rejection: Some(AtlasError::stale_revision(expected, snapshot.revision)),
            };
            store_receipt(&mut registry, receipt_key, receipt.clone());
            return Ok(receipt);
        }

        let rejection = apply_command(snapshot, command);
        if rejection.is_none() {
            snapshot.revision += 1;
        }
        let receipt = CommandReceipt {
            command_id,
            status: if rejection.is_some() {
                CommandStatus::Rejected
            } else {
                CommandStatus::Accepted
            },
            revision: snapshot.revision,
            rejection,
        };
        store_receipt(&mut registry, receipt_key, receipt.clone());
        Ok(receipt)
    }

    async fn close(&self, session_id: &SessionId) -> Result<(), AtlasError> {
        let mut registry = self.registry.lock().await;
        let snapshot = registry
            .sessions
            .remove(session_id)
            .ok_or_else(|| AtlasError::not_found("reading session was not found"))?;
        if registry
            .sessions_by_document
            .get(&snapshot.document_id)
            .is_some_and(|registered| registered == session_id)
        {
            registry.sessions_by_document.remove(&snapshot.document_id);
        }
        registry
            .receipts
            .retain(|(receipt_session_id, _), _| receipt_session_id != session_id);
        registry
            .receipt_order
            .retain(|(receipt_session_id, _)| receipt_session_id != session_id);
        Ok(())
    }
}

fn store_receipt(registry: &mut Registry, key: (SessionId, CommandId), receipt: CommandReceipt) {
    registry.receipts.insert(key.clone(), receipt);
    registry.receipt_order.push_back(key);
    while registry.receipt_order.len() > MAX_PROCESSED_COMMANDS {
        if let Some(expired) = registry.receipt_order.pop_front() {
            registry.receipts.remove(&expired);
        }
    }
}

fn apply_command(snapshot: &mut SessionSnapshot, command: ReadingCommand) -> Option<AtlasError> {
    match command {
        ReadingCommand::FocusChapter { chapter_id } => {
            snapshot.active_chapter_id = Some(chapter_id);
            None
        }
        ReadingCommand::ClearDocumentPreferences { document_id } => (document_id
            != snapshot.document_id)
            .then(|| AtlasError::invalid_input("document does not belong to this session")),
    }
}

#[cfg(test)]
mod tests {
    use atlas_domain::{ChapterId, ProviderState};

    use super::*;

    #[derive(Debug)]
    struct TestProviderStatus;

    #[async_trait]
    impl ProviderStatusPort for TestProviderStatus {
        async fn snapshot(&self) -> ProviderStatusSnapshot {
            ProviderStatusSnapshot {
                mineru: ProviderState::NotConfigured,
                translation: ProviderState::NotConfigured,
                translation_model: None,
            }
        }
    }

    fn module() -> DefaultReadingSession {
        DefaultReadingSession::new(Arc::new(TestProviderStatus))
    }

    #[tokio::test]
    async fn open_restores_the_same_document_session() {
        let module = module();
        let input = OpenSessionInput {
            document_id: DocumentId::from("document-1"),
            initial_chapter_id: None,
        };

        let first = module
            .open(input.clone())
            .await
            .expect("session should open");
        let second = module.open(input).await.expect("session should restore");

        assert!(!first.restored);
        assert!(second.restored);
        assert_eq!(first.session_id, second.session_id);
    }

    #[tokio::test]
    async fn dispatch_is_idempotent_by_command_id() {
        let module = module();
        let opened = module
            .open(OpenSessionInput {
                document_id: DocumentId::from("document-1"),
                initial_chapter_id: None,
            })
            .await
            .expect("session should open");
        let command_id = CommandId::from("command-1");

        let accepted = module
            .dispatch(
                &opened.session_id,
                command_id.clone(),
                Some(0),
                ReadingCommand::FocusChapter {
                    chapter_id: ChapterId::from("chapter-1"),
                },
            )
            .await
            .expect("command should dispatch");
        let duplicate = module
            .dispatch(
                &opened.session_id,
                command_id,
                Some(0),
                ReadingCommand::FocusChapter {
                    chapter_id: ChapterId::from("chapter-2"),
                },
            )
            .await
            .expect("duplicate should return a receipt");

        assert_eq!(accepted.status, CommandStatus::Accepted);
        assert_eq!(accepted.revision, 1);
        assert_eq!(duplicate.status, CommandStatus::Duplicate);
        assert_eq!(duplicate.revision, 1);
    }

    #[tokio::test]
    async fn dispatch_rejects_a_stale_revision() {
        let module = module();
        let opened = module
            .open(OpenSessionInput {
                document_id: DocumentId::from("document-1"),
                initial_chapter_id: None,
            })
            .await
            .expect("session should open");

        let receipt = module
            .dispatch(
                &opened.session_id,
                CommandId::from("command-1"),
                Some(10),
                ReadingCommand::ClearDocumentPreferences {
                    document_id: DocumentId::from("document-1"),
                },
            )
            .await
            .expect("stale commands return a receipt");

        assert_eq!(receipt.status, CommandStatus::Rejected);
        assert_eq!(
            receipt.rejection.expect("rejection should exist").code,
            atlas_domain::AtlasErrorCode::StaleRevision
        );
    }

    #[tokio::test]
    async fn close_releases_session_state() {
        let module = module();
        let input = OpenSessionInput {
            document_id: DocumentId::from("document-1"),
            initial_chapter_id: None,
        };
        let first = module
            .open(input.clone())
            .await
            .expect("session should open");
        module
            .dispatch(
                &first.session_id,
                CommandId::from("command-1"),
                Some(0),
                ReadingCommand::ClearDocumentPreferences {
                    document_id: DocumentId::from("document-1"),
                },
            )
            .await
            .expect("command should dispatch");

        module
            .close(&first.session_id)
            .await
            .expect("session should close");
        let reopened = module.open(input).await.expect("document should reopen");

        assert!(!reopened.restored);
        assert_ne!(first.session_id, reopened.session_id);
        assert_eq!(reopened.snapshot.revision, 0);
    }
}
