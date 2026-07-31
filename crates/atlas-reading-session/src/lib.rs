use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use async_trait::async_trait;
use atlas_domain::{
    AtlasError, CommandId, CommandReceipt, CommandStatus, DocumentId, OpenSessionInput,
    OpenSessionResult, ParseState, ProviderStatusSnapshot, ReadingCommand, SessionId,
    SessionLifecycle, SessionSnapshot, TranslationSnapshot,
};
use atlas_parse::ParseModule;
use atlas_translation::{EnsureTranslationInput, RetryTranslationInput, TranslationModule};
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

    async fn snapshot(&self, session_id: &SessionId) -> Result<SessionSnapshot, AtlasError>;
}

#[derive(Default)]
struct Registry {
    sessions: HashMap<SessionId, SessionSnapshot>,
    sessions_by_document: HashMap<DocumentId, SessionId>,
    subscribers: HashMap<SessionId, u32>,
    receipts: HashMap<(SessionId, CommandId), CommandReceipt>,
    receipt_order: VecDeque<(SessionId, CommandId)>,
}

pub struct DefaultReadingSession {
    providers: Arc<dyn ProviderStatusPort>,
    parser: Arc<dyn ParseModule>,
    translator: Arc<dyn TranslationModule>,
    registry: Mutex<Registry>,
    transitions: Mutex<()>,
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
    pub fn new(
        providers: Arc<dyn ProviderStatusPort>,
        parser: Arc<dyn ParseModule>,
        translator: Arc<dyn TranslationModule>,
    ) -> Self {
        Self {
            providers,
            parser,
            translator,
            registry: Mutex::new(Registry::default()),
            transitions: Mutex::new(()),
        }
    }

    async fn refresh_snapshot(
        &self,
        session_id: &SessionId,
        document_id: &DocumentId,
        foreground_intent: bool,
    ) -> Result<SessionSnapshot, AtlasError> {
        let parsed = self.parser.view(document_id).await?;
        let provider_status = self.providers.snapshot().await;
        let (current_chapter, session_document_id, parsed_before, chapter_changed) = {
            let registry = self.registry.lock().await;
            let snapshot = registry
                .sessions
                .get(session_id)
                .ok_or_else(|| AtlasError::not_found("reading session was not found"))?;
            let current_chapter = match parsed.document.as_ref() {
                Some(document) => snapshot
                    .active_chapter_id
                    .as_ref()
                    .filter(|chapter_id| {
                        document
                            .chapters
                            .iter()
                            .any(|chapter| &chapter.id == *chapter_id)
                    })
                    .cloned()
                    .or_else(|| document.chapters.first().map(|chapter| chapter.id.clone())),
                None => snapshot.active_chapter_id.clone(),
            };
            (
                current_chapter.clone(),
                snapshot.document_id.clone(),
                matches!(
                    snapshot.parse_state,
                    ParseState::Ready | ParseState::Degraded
                ),
                current_chapter != snapshot.active_chapter_id,
            )
        };
        let translation = match current_chapter.as_ref() {
            Some(chapter_id) if parsed.document.is_some() => {
                let input = EnsureTranslationInput {
                    session_id: session_id.clone(),
                    document_id: session_document_id,
                    focused_chapter_id: chapter_id.clone(),
                };
                if foreground_intent || !parsed_before || chapter_changed {
                    self.translator.ensure(input).await?
                } else {
                    self.translator.view(input).await?
                }
            }
            _ => TranslationSnapshot::default(),
        };
        let mut registry = self.registry.lock().await;
        let snapshot = registry
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| AtlasError::not_found("reading session was not found"))?;
        snapshot.parse_state = parsed.parse.state;
        snapshot.lifecycle = lifecycle_for(parsed.parse.state);
        snapshot.provider_status = provider_status;
        snapshot.active_chapter_id = current_chapter;
        snapshot.active_job_ids = translation
            .active_chapter
            .as_ref()
            .filter(|chapter| chapter.job_active)
            .and_then(|chapter| chapter.job_id.clone())
            .into_iter()
            .collect();
        snapshot.translation = translation;
        Ok(snapshot.clone())
    }

    async fn rollback_open(&self, session_id: &SessionId, document_id: &DocumentId, created: bool) {
        let mut registry = self.registry.lock().await;
        if created {
            registry.sessions.remove(session_id);
            registry.subscribers.remove(session_id);
            if registry
                .sessions_by_document
                .get(document_id)
                .is_some_and(|registered| registered == session_id)
            {
                registry.sessions_by_document.remove(document_id);
            }
        } else if let Some(subscribers) = registry.subscribers.get_mut(session_id) {
            *subscribers = subscribers.saturating_sub(1);
        }
    }
}

#[async_trait]
impl ReadingSessionModule for DefaultReadingSession {
    async fn open(&self, input: OpenSessionInput) -> Result<OpenSessionResult, AtlasError> {
        let _transition = self.transitions.lock().await;
        if input.document_id.as_str().trim().is_empty() {
            return Err(AtlasError::invalid_input("document id cannot be empty"));
        }

        let provider_status = self.providers.snapshot().await;
        let mut registry = self.registry.lock().await;
        if let Some(session_id) = registry
            .sessions_by_document
            .get(&input.document_id)
            .cloned()
            && let Some(snapshot) = registry.sessions.get(&session_id).cloned()
        {
            let document_id = snapshot.document_id.clone();
            let pending_cleanup = registry
                .subscribers
                .get(&session_id)
                .copied()
                .unwrap_or_default()
                == 0;
            if !pending_cleanup {
                *registry.subscribers.entry(session_id.clone()).or_default() += 1;
            }
            drop(registry);
            if pending_cleanup {
                self.translator.close_document(&document_id).await?;
                let mut registry = self.registry.lock().await;
                if !registry.sessions.contains_key(&session_id) {
                    return Err(AtlasError::not_found("reading session was not found"));
                }
                registry.subscribers.insert(session_id.clone(), 1);
            }
            let snapshot = match async {
                self.parser
                    .ensure(document_id.clone(), session_id.as_str().to_owned())
                    .await?;
                self.refresh_snapshot(&session_id, &document_id, true).await
            }
            .await
            {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.rollback_open(&session_id, &document_id, false).await;
                    if pending_cleanup
                        && let Err(cleanup) = self.translator.close_document(&document_id).await
                    {
                        return Err(AtlasError {
                            message: format!(
                                "{}; translation cleanup also failed: {}",
                                error.message, cleanup.message
                            ),
                            ..error
                        });
                    }
                    return Err(error);
                }
            };
            return Ok(OpenSessionResult {
                session_id,
                restored: true,
                snapshot,
            });
        }

        let session_id = SessionId::new(Uuid::new_v4().to_string());
        let snapshot = SessionSnapshot {
            schema_version: 2,
            session_id: session_id.clone(),
            document_id: input.document_id.clone(),
            revision: 0,
            lifecycle: SessionLifecycle::Opening,
            parse_state: ParseState::NotStarted,
            active_chapter_id: input.initial_chapter_id,
            active_job_ids: Vec::new(),
            provider_status,
            translation: TranslationSnapshot::default(),
        };
        registry
            .sessions_by_document
            .insert(input.document_id.clone(), session_id.clone());
        registry
            .sessions
            .insert(session_id.clone(), snapshot.clone());
        registry.subscribers.insert(session_id.clone(), 1);

        drop(registry);
        let snapshot = match async {
            self.parser
                .ensure(input.document_id.clone(), session_id.as_str().to_owned())
                .await?;
            self.refresh_snapshot(&session_id, &input.document_id, true)
                .await
        }
        .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.rollback_open(&session_id, &input.document_id, true)
                    .await;
                if let Err(cleanup) = self.translator.close_document(&input.document_id).await {
                    return Err(AtlasError {
                        message: format!(
                            "{}; translation cleanup also failed: {}",
                            error.message, cleanup.message
                        ),
                        ..error
                    });
                }
                return Err(error);
            }
        };

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
        let _transition = self.transitions.lock().await;
        let mut registry = self.registry.lock().await;
        let receipt_key = (session_id.clone(), command_id.clone());
        if let Some(receipt) = registry.receipts.get(&receipt_key) {
            let mut duplicate = receipt.clone();
            duplicate.status = CommandStatus::Duplicate;
            return Ok(duplicate);
        }

        let snapshot = registry
            .sessions
            .get(session_id)
            .ok_or_else(|| AtlasError::not_found("reading session was not found"))?;

        if let Some(expected) = expected_revision
            && expected != snapshot.revision
            && !matches!(command, ReadingCommand::FocusChapter { .. })
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

        let current_revision = snapshot.revision;
        let mut candidate = snapshot.clone();
        let rejection = apply_command(&mut candidate, command.clone());
        let document_id = candidate.document_id.clone();
        drop(registry);
        let translation_result = if rejection.is_none() {
            match command {
                ReadingCommand::FocusChapter { chapter_id } => {
                    self.translator
                        .ensure(EnsureTranslationInput {
                            session_id: session_id.clone(),
                            document_id,
                            focused_chapter_id: chapter_id,
                        })
                        .await
                }
                ReadingCommand::RetryTranslation { chapter_id } => {
                    self.translator
                        .retry(RetryTranslationInput {
                            session_id: session_id.clone(),
                            document_id,
                            chapter_id,
                        })
                        .await
                }
                ReadingCommand::ClearDocumentPreferences { .. } => {
                    Ok(candidate.translation.clone())
                }
            }
        } else {
            Ok(candidate.translation.clone())
        };
        let rejection = rejection.or_else(|| translation_result.as_ref().err().cloned());
        if let Ok(translation) = translation_result {
            candidate.translation = translation;
        }
        if rejection.is_none() {
            candidate.revision = current_revision.saturating_add(1);
        }
        let mut registry = self.registry.lock().await;
        if rejection.is_none() {
            registry
                .sessions
                .insert(session_id.clone(), candidate.clone());
        }
        let receipt = CommandReceipt {
            command_id,
            status: if rejection.is_some() {
                CommandStatus::Rejected
            } else {
                CommandStatus::Accepted
            },
            revision: candidate.revision,
            rejection,
        };
        store_receipt(&mut registry, receipt_key, receipt.clone());
        Ok(receipt)
    }

    async fn close(&self, session_id: &SessionId) -> Result<(), AtlasError> {
        let _transition = self.transitions.lock().await;
        let mut registry = self.registry.lock().await;
        let snapshot = registry
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| AtlasError::not_found("reading session was not found"))?;
        let subscribers = registry
            .subscribers
            .get_mut(session_id)
            .ok_or_else(|| AtlasError::internal("reading session subscriber count is missing"))?;
        if *subscribers > 1 {
            *subscribers -= 1;
            return Ok(());
        }
        *subscribers = 0;
        drop(registry);
        self.translator
            .close_document(&snapshot.document_id)
            .await?;

        let mut registry = self.registry.lock().await;
        registry.sessions.remove(session_id);
        registry.subscribers.remove(session_id);
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

    async fn snapshot(&self, session_id: &SessionId) -> Result<SessionSnapshot, AtlasError> {
        let _transition = self.transitions.lock().await;
        let document_id = self
            .registry
            .lock()
            .await
            .sessions
            .get(session_id)
            .map(|snapshot| snapshot.document_id.clone())
            .ok_or_else(|| AtlasError::not_found("reading session was not found"))?;
        self.refresh_snapshot(session_id, &document_id, false).await
    }
}

fn lifecycle_for(parse_state: ParseState) -> SessionLifecycle {
    match parse_state {
        ParseState::Queued
        | ParseState::Uploading
        | ParseState::Processing
        | ParseState::Downloading
        | ParseState::Normalizing => SessionLifecycle::Parsing,
        ParseState::Ready | ParseState::NotStarted => SessionLifecycle::Ready,
        ParseState::Degraded | ParseState::Failed | ParseState::StatusUnknown => {
            SessionLifecycle::Degraded
        }
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
        ReadingCommand::RetryTranslation { chapter_id } => {
            snapshot.active_chapter_id = Some(chapter_id);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use atlas_domain::{
        CanonicalChapter, CanonicalDocument, ChapterId, ChapterRole, ParseSnapshot,
        ParsedDocumentView, ParserIdentity, ProviderState,
    };
    use atlas_translation::{EnsureTranslationInput, RetryTranslationInput};

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

    #[derive(Debug)]
    struct TestParseModule;

    #[async_trait]
    impl ParseModule for TestParseModule {
        async fn ensure(
            &self,
            _document_id: DocumentId,
            _session_id: String,
        ) -> Result<ParsedDocumentView, AtlasError> {
            Ok(ParsedDocumentView {
                parse: ParseSnapshot {
                    state: ParseState::Ready,
                    ..ParseSnapshot::default()
                },
                document: None,
            })
        }

        async fn view(&self, _document_id: &DocumentId) -> Result<ParsedDocumentView, AtlasError> {
            self.ensure(DocumentId::from("ignored"), "ignored".to_owned())
                .await
        }

        async fn retry_remote_status(
            &self,
            _document_id: &DocumentId,
        ) -> Result<ParseSnapshot, AtlasError> {
            Ok(ParseSnapshot::default())
        }

        async fn reupload(
            &self,
            _document_id: DocumentId,
            _session_id: String,
        ) -> Result<ParseSnapshot, AtlasError> {
            Ok(ParseSnapshot::default())
        }

        async fn recover(&self) -> Result<usize, AtlasError> {
            Ok(0)
        }
    }

    #[derive(Debug, Default)]
    struct FlakyParseModule {
        calls: AtomicUsize,
    }

    #[derive(Debug)]
    struct FixedParsedDocument(CanonicalDocument);

    #[async_trait]
    impl ParseModule for FixedParsedDocument {
        async fn ensure(
            &self,
            _document_id: DocumentId,
            _session_id: String,
        ) -> Result<ParsedDocumentView, AtlasError> {
            self.view(&self.0.document_id).await
        }

        async fn view(&self, _document_id: &DocumentId) -> Result<ParsedDocumentView, AtlasError> {
            Ok(ParsedDocumentView {
                parse: ParseSnapshot {
                    state: ParseState::Ready,
                    ..ParseSnapshot::default()
                },
                document: Some(self.0.clone()),
            })
        }

        async fn retry_remote_status(
            &self,
            _document_id: &DocumentId,
        ) -> Result<ParseSnapshot, AtlasError> {
            Ok(ParseSnapshot::default())
        }

        async fn reupload(
            &self,
            _document_id: DocumentId,
            _session_id: String,
        ) -> Result<ParseSnapshot, AtlasError> {
            Ok(ParseSnapshot::default())
        }

        async fn recover(&self) -> Result<usize, AtlasError> {
            Ok(0)
        }
    }

    #[async_trait]
    impl ParseModule for FlakyParseModule {
        async fn ensure(
            &self,
            _document_id: DocumentId,
            _session_id: String,
        ) -> Result<ParsedDocumentView, AtlasError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(AtlasError::storage("parse store temporarily unavailable"));
            }
            Ok(ParsedDocumentView {
                parse: ParseSnapshot {
                    state: ParseState::Ready,
                    ..ParseSnapshot::default()
                },
                document: None,
            })
        }

        async fn view(&self, _document_id: &DocumentId) -> Result<ParsedDocumentView, AtlasError> {
            self.ensure(DocumentId::from("ignored"), "ignored".to_owned())
                .await
        }

        async fn retry_remote_status(
            &self,
            _document_id: &DocumentId,
        ) -> Result<ParseSnapshot, AtlasError> {
            Ok(ParseSnapshot::default())
        }

        async fn reupload(
            &self,
            _document_id: DocumentId,
            _session_id: String,
        ) -> Result<ParseSnapshot, AtlasError> {
            Ok(ParseSnapshot::default())
        }

        async fn recover(&self) -> Result<usize, AtlasError> {
            Ok(0)
        }
    }

    #[derive(Debug)]
    struct TestTranslationModule;

    #[async_trait]
    impl TranslationModule for TestTranslationModule {
        async fn ensure(
            &self,
            _input: EnsureTranslationInput,
        ) -> Result<TranslationSnapshot, AtlasError> {
            Ok(TranslationSnapshot::default())
        }

        async fn retry(
            &self,
            _input: RetryTranslationInput,
        ) -> Result<TranslationSnapshot, AtlasError> {
            Ok(TranslationSnapshot::default())
        }

        async fn view(
            &self,
            _input: EnsureTranslationInput,
        ) -> Result<TranslationSnapshot, AtlasError> {
            Ok(TranslationSnapshot::default())
        }

        async fn recover(&self) -> Result<usize, AtlasError> {
            Ok(0)
        }

        async fn close_document(&self, _document_id: &DocumentId) -> Result<(), AtlasError> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct FlakyCloseTranslationModule {
        close_calls: AtomicUsize,
    }

    #[async_trait]
    impl TranslationModule for FlakyCloseTranslationModule {
        async fn ensure(
            &self,
            _input: EnsureTranslationInput,
        ) -> Result<TranslationSnapshot, AtlasError> {
            Ok(TranslationSnapshot::default())
        }

        async fn retry(
            &self,
            _input: RetryTranslationInput,
        ) -> Result<TranslationSnapshot, AtlasError> {
            Ok(TranslationSnapshot::default())
        }

        async fn view(
            &self,
            _input: EnsureTranslationInput,
        ) -> Result<TranslationSnapshot, AtlasError> {
            Ok(TranslationSnapshot::default())
        }

        async fn recover(&self) -> Result<usize, AtlasError> {
            Ok(0)
        }

        async fn close_document(&self, _document_id: &DocumentId) -> Result<(), AtlasError> {
            if self.close_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(AtlasError::storage(
                    "translation cleanup temporarily unavailable",
                ))
            } else {
                Ok(())
            }
        }
    }

    #[derive(Debug, Default)]
    struct CountingTranslationModule {
        ensure_calls: AtomicUsize,
        view_calls: AtomicUsize,
    }

    #[async_trait]
    impl TranslationModule for CountingTranslationModule {
        async fn ensure(
            &self,
            _input: EnsureTranslationInput,
        ) -> Result<TranslationSnapshot, AtlasError> {
            self.ensure_calls.fetch_add(1, Ordering::SeqCst);
            Ok(TranslationSnapshot::default())
        }

        async fn retry(
            &self,
            _input: RetryTranslationInput,
        ) -> Result<TranslationSnapshot, AtlasError> {
            Ok(TranslationSnapshot::default())
        }

        async fn view(
            &self,
            _input: EnsureTranslationInput,
        ) -> Result<TranslationSnapshot, AtlasError> {
            self.view_calls.fetch_add(1, Ordering::SeqCst);
            Ok(TranslationSnapshot::default())
        }

        async fn recover(&self) -> Result<usize, AtlasError> {
            Ok(0)
        }

        async fn close_document(&self, _document_id: &DocumentId) -> Result<(), AtlasError> {
            Ok(())
        }
    }

    fn module() -> DefaultReadingSession {
        DefaultReadingSession::new(
            Arc::new(TestProviderStatus),
            Arc::new(TestParseModule),
            Arc::new(TestTranslationModule),
        )
    }

    fn parsed_document(chapter_id: &str) -> CanonicalDocument {
        CanonicalDocument {
            schema_version: 1,
            artifact_id: "artifact-new".to_owned(),
            document_id: DocumentId::from("document-1"),
            source_sha256: "source".to_owned(),
            parser: ParserIdentity {
                name: "test".to_owned(),
                version: "1".to_owned(),
                backend: "test".to_owned(),
            },
            normalizer_version: "1".to_owned(),
            page_count: 1,
            title: Some("Test".to_owned()),
            chapters: vec![CanonicalChapter {
                id: ChapterId::from(chapter_id),
                order_index: 0,
                depth: 1,
                role: ChapterRole::Body,
                source_title: "Introduction".to_owned(),
                page_start: 1,
                page_end: 1,
                blocks: Vec::new(),
            }],
            assets: Vec::new(),
        }
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
    async fn a_reused_session_stays_alive_until_its_final_subscriber_closes() {
        let module = module();
        let input = OpenSessionInput {
            document_id: DocumentId::from("document-1"),
            initial_chapter_id: None,
        };
        let first = module
            .open(input.clone())
            .await
            .expect("first subscriber should open");
        let second = module
            .open(input)
            .await
            .expect("second subscriber should open");

        module
            .close(&first.session_id)
            .await
            .expect("first subscriber should close");
        module
            .snapshot(&second.session_id)
            .await
            .expect("shared session should remain");
        module
            .close(&second.session_id)
            .await
            .expect("final subscriber should close");
        assert!(module.snapshot(&second.session_id).await.is_err());
    }

    #[tokio::test]
    async fn a_failed_open_rolls_back_its_session_and_subscriber_reference() {
        let module = DefaultReadingSession::new(
            Arc::new(TestProviderStatus),
            Arc::new(FlakyParseModule::default()),
            Arc::new(TestTranslationModule),
        );
        let input = OpenSessionInput {
            document_id: DocumentId::from("document-1"),
            initial_chapter_id: None,
        };

        module
            .open(input.clone())
            .await
            .expect_err("first open should fail");
        {
            let registry = module.registry.lock().await;
            assert!(registry.sessions.is_empty());
            assert!(registry.sessions_by_document.is_empty());
            assert!(registry.subscribers.is_empty());
        }

        let opened = module.open(input).await.expect("retry should open");
        module
            .close(&opened.session_id)
            .await
            .expect("single subscriber should fully close");
        assert!(module.registry.lock().await.sessions.is_empty());
    }

    #[tokio::test]
    async fn artifact_replacement_remaps_a_missing_active_chapter() {
        let module = DefaultReadingSession::new(
            Arc::new(TestProviderStatus),
            Arc::new(FixedParsedDocument(parsed_document("new-chapter"))),
            Arc::new(TestTranslationModule),
        );

        let opened = module
            .open(OpenSessionInput {
                document_id: DocumentId::from("document-1"),
                initial_chapter_id: Some(ChapterId::from("old-artifact-chapter")),
            })
            .await
            .expect("session should remap the old chapter");

        assert_eq!(
            opened
                .snapshot
                .active_chapter_id
                .expect("new chapter should be selected")
                .as_str(),
            "new-chapter"
        );
    }

    #[tokio::test]
    async fn snapshot_refresh_reads_translation_without_reasserting_foreground_intent() {
        let translator = Arc::new(CountingTranslationModule::default());
        let module = DefaultReadingSession::new(
            Arc::new(TestProviderStatus),
            Arc::new(FixedParsedDocument(parsed_document("chapter-1"))),
            translator.clone(),
        );
        let opened = module
            .open(OpenSessionInput {
                document_id: DocumentId::from("document-1"),
                initial_chapter_id: None,
            })
            .await
            .expect("session should open");
        module
            .snapshot(&opened.session_id)
            .await
            .expect("snapshot should refresh");

        assert_eq!(translator.ensure_calls.load(Ordering::SeqCst), 1);
        assert_eq!(translator.view_calls.load(Ordering::SeqCst), 1);
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
    async fn focus_chapter_is_last_write_wins_even_with_a_stale_revision() {
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
                CommandId::from("focus-latest"),
                Some(99),
                ReadingCommand::FocusChapter {
                    chapter_id: ChapterId::from("chapter-2"),
                },
            )
            .await
            .expect("focus should dispatch");

        assert_eq!(receipt.status, CommandStatus::Accepted);
        assert_eq!(
            module
                .snapshot(&opened.session_id)
                .await
                .expect("snapshot should load")
                .active_chapter_id
                .expect("chapter should be focused")
                .as_str(),
            "chapter-2"
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

    #[tokio::test]
    async fn failed_final_cleanup_leaves_zero_subscribers_and_is_retried_on_open() {
        let translator = Arc::new(FlakyCloseTranslationModule::default());
        let module = DefaultReadingSession::new(
            Arc::new(TestProviderStatus),
            Arc::new(TestParseModule),
            translator.clone(),
        );
        let input = OpenSessionInput {
            document_id: DocumentId::from("document-1"),
            initial_chapter_id: None,
        };
        let opened = module
            .open(input.clone())
            .await
            .expect("session should open");

        module
            .close(&opened.session_id)
            .await
            .expect_err("first cleanup should fail");
        assert_eq!(
            module
                .registry
                .lock()
                .await
                .subscribers
                .get(&opened.session_id)
                .copied(),
            Some(0)
        );

        let reopened = module
            .open(input)
            .await
            .expect("opening again should retry pending cleanup");
        assert_eq!(reopened.session_id, opened.session_id);
        assert_eq!(translator.close_calls.load(Ordering::SeqCst), 2);
        module
            .close(&reopened.session_id)
            .await
            .expect("final cleanup should now succeed");
        assert!(module.registry.lock().await.sessions.is_empty());
    }
}
