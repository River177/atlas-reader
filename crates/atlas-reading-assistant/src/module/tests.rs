use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use atlas_domain::{
    AtlasError, BlockId, BlockKind, BlockTranslationState, CanonicalBlock, CanonicalChapter,
    CanonicalDocument, ChapterId, ChapterRole, ChapterTranslationView, ConversationId, DocumentId,
    JobId, ParserIdentity, ReadingAssistantCommand, ReadingAssistantSnapshot, ReadingMessageId,
    ReadingMessageView, SelectionContextInput, SessionId, StructuredContent, TranslatedBlockView,
    TranslationSnapshot, TranslationState,
};
use atlas_parse::{ParseOperation, ParseStore, PublishArtifact};
use atlas_translation::{
    EnsureTranslationInput, RetryTranslationInput, TranslationCompletion, TranslationConfiguration,
    TranslationConfigurationPort, TranslationCredential, TranslationModule,
    TranslationProviderError, TranslationProviderErrorKind,
};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::{
    NewAssistantResponse, NewReaderMessage, QueuedReadingResponse, ReadingAssistantProviderRequest,
    RecoverableReadingResponse, ScriptedReadingAssistantAdapter, ScriptedReadingAssistantResponse,
};

#[derive(Clone)]
struct FixedParseStore {
    document: CanonicalDocument,
}

#[async_trait]
impl ParseStore for FixedParseStore {
    async fn active_document(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<CanonicalDocument>, AtlasError> {
        Ok((&self.document.document_id == document_id).then(|| self.document.clone()))
    }

    async fn latest_operation(
        &self,
        _document_id: &DocumentId,
        _backend: Option<&str>,
    ) -> Result<Option<ParseOperation>, AtlasError> {
        Ok(None)
    }

    async fn recoverable_operations(&self) -> Result<Vec<ParseOperation>, AtlasError> {
        Ok(Vec::new())
    }

    async fn save_operation(&self, _operation: &ParseOperation) -> Result<(), AtlasError> {
        Err(AtlasError::internal("parse writes are not used"))
    }

    async fn supersede_operation(
        &self,
        _operation: &ParseOperation,
        _replacement: &ParseOperation,
    ) -> Result<(), AtlasError> {
        Err(AtlasError::internal("parse writes are not used"))
    }

    async fn publish(&self, _artifact: &PublishArtifact) -> Result<(), AtlasError> {
        Err(AtlasError::internal("parse writes are not used"))
    }
}

#[derive(Clone)]
struct FixedTranslationModule {
    snapshot: TranslationSnapshot,
    view_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl TranslationModule for FixedTranslationModule {
    async fn ensure(
        &self,
        _input: EnsureTranslationInput,
    ) -> Result<TranslationSnapshot, AtlasError> {
        Ok(self.snapshot.clone())
    }

    async fn retry(
        &self,
        _input: RetryTranslationInput,
    ) -> Result<TranslationSnapshot, AtlasError> {
        Ok(self.snapshot.clone())
    }

    async fn view(
        &self,
        _input: EnsureTranslationInput,
    ) -> Result<TranslationSnapshot, AtlasError> {
        self.view_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.snapshot.clone())
    }

    async fn recover(&self) -> Result<usize, AtlasError> {
        Ok(0)
    }

    async fn close_document(&self, _document_id: &DocumentId) -> Result<(), AtlasError> {
        Ok(())
    }
}

struct FixedConfiguration;

#[async_trait]
impl TranslationConfigurationPort for FixedConfiguration {
    async fn load(&self) -> Result<Option<TranslationConfiguration>, AtlasError> {
        Ok(Some(configuration()))
    }
}

fn configuration() -> TranslationConfiguration {
    TranslationConfiguration {
        profile_id: "openai_compatible".to_owned(),
        endpoint_base_url: "https://models.example/v1".to_owned(),
        endpoint_fingerprint: "endpoint-1".to_owned(),
        model_id: "model-1".to_owned(),
        context_window: 32_768,
        credential: Some(TranslationCredential::new("not-a-real-key")),
    }
}

#[derive(Default)]
struct MemoryStore {
    snapshots: Mutex<HashMap<DocumentId, ReadingAssistantSnapshot>>,
    fail_checkpoints: AtomicBool,
    fail_clear: AtomicBool,
}

#[async_trait]
impl ReadingAssistantStore for MemoryStore {
    async fn view(&self, document_id: &DocumentId) -> Result<ReadingAssistantSnapshot, AtlasError> {
        Ok(self
            .snapshots
            .lock()
            .await
            .get(document_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn queue_response(
        &self,
        response: &QueuedReadingResponse,
    ) -> Result<ReadingAssistantSnapshot, AtlasError> {
        let mut snapshots = self.snapshots.lock().await;
        let snapshot = snapshots.entry(response.document_id.clone()).or_default();
        if snapshot.active_assistant_message_id.is_some() {
            return Err(AtlasError::assistant_busy());
        }
        if let Some(existing) = snapshot.conversation_id.as_ref()
            && existing != &response.conversation_id
        {
            return Err(AtlasError::storage("conversation changed"));
        }
        snapshot.conversation_id = Some(response.conversation_id.clone());
        if let Some(reader) = response.reader.as_ref() {
            snapshot.messages.push(ReadingMessageView::Reader {
                id: reader.id.clone(),
                text: reader.text.clone(),
                selection_context: reader.selection.clone(),
                created_at: reader.created_at,
            });
            if reader.selection.is_some() {
                snapshot.latest_selection = reader.selection.clone();
            }
        }
        snapshot.messages.push(ReadingMessageView::Assistant {
            id: response.assistant.id.clone(),
            responding_to: response.assistant.responding_to.clone(),
            state: AssistantMessageState::Queued,
            text: String::new(),
            citations: Vec::new(),
            retry_of_message_id: response.assistant.retry_of_message_id.clone(),
            safe_message: None,
            created_at: response.assistant.created_at,
            updated_at: response.assistant.created_at,
        });
        snapshot.active_assistant_message_id = Some(response.assistant.id.clone());
        Ok(snapshot.clone())
    }

    async fn checkpoint_response(
        &self,
        checkpoint: &AssistantResponseCheckpoint,
    ) -> Result<(), AtlasError> {
        if self.fail_checkpoints.load(Ordering::SeqCst) {
            return Err(AtlasError::storage("checkpoint unavailable"));
        }
        let mut snapshots = self.snapshots.lock().await;
        let snapshot = snapshots
            .values_mut()
            .find(|snapshot| snapshot.conversation_id.as_ref() == Some(&checkpoint.conversation_id))
            .ok_or_else(|| AtlasError::not_found("conversation missing"))?;
        let message = snapshot
            .messages
            .iter_mut()
            .find(|message| {
                matches!(message, ReadingMessageView::Assistant { id, state, .. }
                    if id == &checkpoint.assistant_message_id
                        && matches!(state, AssistantMessageState::Queued | AssistantMessageState::Streaming))
            })
            .ok_or_else(|| AtlasError::storage("response is terminal"))?;
        let ReadingMessageView::Assistant {
            state,
            text,
            citations,
            safe_message,
            updated_at,
            ..
        } = message
        else {
            unreachable!();
        };
        *state = checkpoint.state;
        *text = checkpoint.text.clone();
        *citations = checkpoint.citations.clone();
        *safe_message = checkpoint.safe_message.clone();
        *updated_at = checkpoint.updated_at;
        if !matches!(
            checkpoint.state,
            AssistantMessageState::Queued | AssistantMessageState::Streaming
        ) {
            snapshot.active_assistant_message_id = None;
        }
        Ok(())
    }

    async fn clear(&self, document_id: &DocumentId) -> Result<bool, AtlasError> {
        if self.fail_clear.load(Ordering::SeqCst) {
            return Err(AtlasError::storage("clear unavailable"));
        }
        Ok(self.snapshots.lock().await.remove(document_id).is_some())
    }

    async fn recoverable_responses(&self) -> Result<Vec<RecoverableReadingResponse>, AtlasError> {
        let snapshots = self.snapshots.lock().await;
        Ok(snapshots
            .iter()
            .filter_map(|(document_id, snapshot)| {
                let active = snapshot.active_assistant_message_id.as_ref()?;
                let responding_to = snapshot.messages.iter().find_map(|message| match message {
                    ReadingMessageView::Assistant {
                        id, responding_to, ..
                    } if id == active => Some(responding_to.clone()),
                    _ => None,
                })?;
                Some(RecoverableReadingResponse {
                    conversation_id: snapshot.conversation_id.clone()?,
                    document_id: document_id.clone(),
                    assistant_message_id: active.clone(),
                    responding_to,
                })
            })
            .collect())
    }
}

#[derive(Default)]
struct CitationProvider {
    requests: Mutex<Vec<ReadingAssistantProviderRequest>>,
}

#[async_trait]
impl ReadingAssistantProviderPort for CitationProvider {
    async fn stream(
        &self,
        _configuration: &TranslationConfiguration,
        request: ReadingAssistantProviderRequest,
        sink: Arc<dyn ReadingAssistantStreamSink>,
        _cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        let citation = request
            .allowed_citation_ids
            .first()
            .cloned()
            .expect("context should have a citation id");
        self.requests.lock().await.push(request);
        sink.push(ReadingAssistantStreamEvent::Text(
            "该假设限制了比较范围。".to_owned(),
        ))
        .await
        .expect("text should checkpoint");
        sink.push(ReadingAssistantStreamEvent::Citation(citation))
            .await
            .expect("citation should checkpoint");
        Ok(TranslationCompletion {
            finish_reason: Some("stop".to_owned()),
        })
    }
}

#[derive(Default)]
struct BlockingProvider {
    started: Notify,
}

#[derive(Default)]
struct TrailingProvider {
    second_sent: Notify,
    release: Notify,
}

#[async_trait]
impl ReadingAssistantProviderPort for TrailingProvider {
    async fn stream(
        &self,
        _configuration: &TranslationConfiguration,
        _request: ReadingAssistantProviderRequest,
        sink: Arc<dyn ReadingAssistantStreamSink>,
        _cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        sink.push(ReadingAssistantStreamEvent::Text("A".to_owned()))
            .await
            .expect("first text should checkpoint");
        sink.push(ReadingAssistantStreamEvent::Text("B".to_owned()))
            .await
            .expect("second text should buffer");
        self.second_sent.notify_one();
        self.release.notified().await;
        Ok(TranslationCompletion {
            finish_reason: Some("stop".to_owned()),
        })
    }
}

#[async_trait]
impl ReadingAssistantProviderPort for BlockingProvider {
    async fn stream(
        &self,
        _configuration: &TranslationConfiguration,
        _request: ReadingAssistantProviderRequest,
        sink: Arc<dyn ReadingAssistantStreamSink>,
        cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        sink.push(ReadingAssistantStreamEvent::Text("部分回答".to_owned()))
            .await
            .expect("partial text should checkpoint");
        self.started.notify_one();
        cancellation.cancelled().await;
        Err(TranslationProviderError::new(
            TranslationProviderErrorKind::Cancelled,
            "cancelled",
        ))
    }
}

fn block(id: &str, order_index: u32, source: &str) -> CanonicalBlock {
    CanonicalBlock {
        id: BlockId::from(id),
        order_index,
        kind: BlockKind::Paragraph,
        page_start: order_index.saturating_add(1),
        page_end: order_index.saturating_add(1),
        bounding_boxes: Vec::new(),
        content: StructuredContent::text(source),
        source_digest: format!("digest-{id}"),
    }
}

fn document() -> CanonicalDocument {
    CanonicalDocument {
        schema_version: 1,
        artifact_id: "artifact-1".to_owned(),
        document_id: DocumentId::from("document-1"),
        source_sha256: "source".to_owned(),
        parser: ParserIdentity {
            name: "test".to_owned(),
            version: "1".to_owned(),
            backend: "test".to_owned(),
        },
        normalizer_version: "1".to_owned(),
        page_count: 3,
        title: Some("Synthetic".to_owned()),
        chapters: vec![CanonicalChapter {
            id: ChapterId::from("chapter-1"),
            order_index: 0,
            depth: 1,
            role: ChapterRole::Body,
            source_title: "Method".to_owned(),
            page_start: 1,
            page_end: 3,
            blocks: vec![
                block("block-1", 0, "Previous."),
                block("block-2", 1, "The model adopts this assumption."),
                block("block-3", 2, "Next."),
            ],
        }],
        assets: Vec::new(),
    }
}

fn translation() -> TranslationSnapshot {
    TranslationSnapshot {
        target_locale: "zh-CN".to_owned(),
        model_id: Some("model-1".to_owned()),
        active_chapter: Some(ChapterTranslationView {
            chapter_id: ChapterId::from("chapter-1"),
            state: TranslationState::Complete,
            progress: 1.0,
            job_id: Some(JobId::from("translation-job")),
            job_active: false,
            blocks: [
                ("block-1", "上一段。"),
                ("block-2", "模型🙂采用该假设。"),
                ("block-3", "下一段。"),
            ]
            .into_iter()
            .map(|(id, target)| TranslatedBlockView {
                block_id: BlockId::from(id),
                source_digest: format!("digest-{id}"),
                state: BlockTranslationState::Ready,
                target: Some(StructuredContent::text(target)),
                safe_message: None,
            })
            .collect(),
            prefetched: false,
            safe_message: None,
        }),
        prefetched_chapter_id: None,
    }
}

fn selection() -> SelectionContextInput {
    SelectionContextInput {
        block_id: BlockId::from("block-2"),
        source_digest: "digest-block-2".to_owned(),
        start_utf16: 4,
        end_utf16: 9,
        selected_text: "采用该假设".to_owned(),
    }
}

fn module(
    store: Arc<MemoryStore>,
    provider: Arc<dyn ReadingAssistantProviderPort>,
) -> (DefaultReadingAssistantModule, Arc<AtomicUsize>) {
    let view_calls = Arc::new(AtomicUsize::new(0));
    (
        DefaultReadingAssistantModule::new(
            Arc::new(FixedParseStore {
                document: document(),
            }),
            Arc::new(FixedTranslationModule {
                snapshot: translation(),
                view_calls: view_calls.clone(),
            }),
            store,
            Arc::new(FixedConfiguration),
            provider,
        ),
        view_calls,
    )
}

fn send_command(user_message_id: &str) -> DispatchReadingAssistantInput {
    DispatchReadingAssistantInput {
        session_id: SessionId::from("session-1"),
        document_id: DocumentId::from("document-1"),
        command: ReadingAssistantCommand::SendMessage {
            user_message_id: ReadingMessageId::from(user_message_id),
            text: "为什么需要这个假设？".to_owned(),
            selection: Some(selection()),
        },
    }
}

async fn wait_for_state(
    module: &DefaultReadingAssistantModule,
    expected: AssistantMessageState,
) -> ReadingAssistantSnapshot {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = module
                .view(&DocumentId::from("document-1"))
                .await
                .expect("snapshot should load");
            if snapshot.messages.iter().any(|message| {
                matches!(message, ReadingMessageView::Assistant { state, .. } if *state == expected)
            }) {
                break snapshot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("assistant state should settle")
}

#[tokio::test]
async fn send_validates_context_queues_before_provider_and_commits_citation() {
    let store = Arc::new(MemoryStore::default());
    let provider = Arc::new(CitationProvider::default());
    let (module, view_calls) = module(store, provider.clone());

    let queued = module
        .dispatch(send_command("reader-1"))
        .await
        .expect("message should queue");
    assert!(queued.active_assistant_message_id.is_some());
    let ready = wait_for_state(&module, AssistantMessageState::Ready).await;

    assert_eq!(view_calls.load(Ordering::SeqCst), 1);
    assert_eq!(ready.messages.len(), 2);
    assert_eq!(
        ready
            .latest_selection
            .as_ref()
            .map(|selection| selection.aligned_source.as_str()),
        Some("The model adopts this assumption.")
    );
    let ReadingMessageView::Assistant {
        text, citations, ..
    } = &ready.messages[1]
    else {
        panic!("second message should be assistant");
    };
    assert_eq!(text, "该假设限制了比较范围。");
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0].block_id.as_str(), "block-1");
    let requests = provider.requests.lock().await;
    assert!(requests[0].input_json.contains("为什么需要这个假设？"));
    assert!(
        requests[0]
            .input_json
            .contains("The model adopts this assumption.")
    );
}

#[tokio::test]
async fn duplicate_reader_id_is_rejected_without_another_provider_request() {
    let store = Arc::new(MemoryStore::default());
    let provider = Arc::new(CitationProvider::default());
    let (module, _) = module(store, provider.clone());
    module
        .dispatch(send_command("reader-1"))
        .await
        .expect("first message should queue");
    wait_for_state(&module, AssistantMessageState::Ready).await;

    assert!(
        module.dispatch(send_command("reader-1")).await.is_err(),
        "a reader message id is single-use; command id handles transport idempotency"
    );
    let mut conflicting = send_command("reader-1");
    conflicting.command = ReadingAssistantCommand::SendMessage {
        user_message_id: ReadingMessageId::from("reader-1"),
        text: "Different question".to_owned(),
        selection: Some(selection()),
    };
    assert!(
        module.dispatch(conflicting).await.is_err(),
        "same id with different content must fail"
    );
    assert_eq!(provider.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn cancel_preserves_partial_text_and_marks_terminal_state() {
    let store = Arc::new(MemoryStore::default());
    let provider = Arc::new(BlockingProvider::default());
    let (module, _) = module(store, provider.clone());
    let queued = module
        .dispatch(send_command("reader-1"))
        .await
        .expect("message should queue");
    let assistant_id = queued
        .active_assistant_message_id
        .expect("assistant id should exist");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        provider.started.notified(),
    )
    .await
    .expect("provider should start");

    let cancelled = module
        .dispatch(DispatchReadingAssistantInput {
            session_id: SessionId::from("session-1"),
            document_id: DocumentId::from("document-1"),
            command: ReadingAssistantCommand::CancelResponse {
                assistant_message_id: assistant_id,
            },
        })
        .await
        .expect("response should cancel");

    let ReadingMessageView::Assistant { state, text, .. } = &cancelled.messages[1] else {
        panic!("second message should be assistant");
    };
    assert_eq!(*state, AssistantMessageState::Cancelled);
    assert_eq!(text, "部分回答");
}

#[tokio::test]
async fn failed_clear_retains_active_sink_so_partial_text_can_still_cancel() {
    let store = Arc::new(MemoryStore::default());
    let provider = Arc::new(BlockingProvider::default());
    let (module, _) = module(store.clone(), provider.clone());
    let queued = module
        .dispatch(send_command("reader-1"))
        .await
        .expect("message should queue");
    let assistant_id = queued
        .active_assistant_message_id
        .expect("assistant id should exist");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        provider.started.notified(),
    )
    .await
    .expect("provider should start");
    store.fail_clear.store(true, Ordering::SeqCst);
    store.fail_checkpoints.store(true, Ordering::SeqCst);

    assert!(
        module
            .dispatch(DispatchReadingAssistantInput {
                session_id: SessionId::from("session-1"),
                document_id: DocumentId::from("document-1"),
                command: ReadingAssistantCommand::ClearConversation,
            })
            .await
            .is_err(),
        "failed clear should surface"
    );
    assert!(
        module
            .in_flight
            .lock()
            .await
            .contains_key(&DocumentId::from("document-1")),
        "failed clear must retain the active sink"
    );

    store.fail_checkpoints.store(false, Ordering::SeqCst);
    store.fail_clear.store(false, Ordering::SeqCst);
    let cancelled = module
        .dispatch(DispatchReadingAssistantInput {
            session_id: SessionId::from("session-1"),
            document_id: DocumentId::from("document-1"),
            command: ReadingAssistantCommand::CancelResponse {
                assistant_message_id: assistant_id,
            },
        })
        .await
        .expect("response should still cancel");
    let ReadingMessageView::Assistant { state, text, .. } = &cancelled.messages[1] else {
        panic!("second message should be assistant");
    };
    assert_eq!(*state, AssistantMessageState::Cancelled);
    assert_eq!(text, "部分回答");
}

#[tokio::test]
async fn quiet_trailing_delta_is_checkpointed_before_the_stream_finishes() {
    let store = Arc::new(MemoryStore::default());
    let provider = Arc::new(TrailingProvider::default());
    let (module, _) = module(store, provider.clone());
    module
        .dispatch(send_command("reader-1"))
        .await
        .expect("message should queue");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        provider.second_sent.notified(),
    )
    .await
    .expect("second delta should arrive");
    tokio::time::sleep(CHECKPOINT_INTERVAL + std::time::Duration::from_millis(100)).await;

    let snapshot = module
        .view(&DocumentId::from("document-1"))
        .await
        .expect("snapshot should load");
    let ReadingMessageView::Assistant { state, text, .. } = &snapshot.messages[1] else {
        panic!("second message should be assistant");
    };
    assert_eq!(*state, AssistantMessageState::Streaming);
    assert_eq!(text, "AB");
    provider.release.notify_one();
    wait_for_state(&module, AssistantMessageState::Ready).await;
}

#[tokio::test]
async fn failed_response_retries_without_duplicate_reader_message() {
    let store = Arc::new(MemoryStore::default());
    let provider = Arc::new(ScriptedReadingAssistantAdapter::new([
        Err(TranslationProviderError::new(
            TranslationProviderErrorKind::Timeout,
            "The response timed out",
        )),
        Ok(ScriptedReadingAssistantResponse {
            chunks: vec!["重试成功".to_owned()],
            finish_reason: Some("stop".to_owned()),
        }),
    ]));
    let (module, _) = module(store, provider);
    module
        .dispatch(send_command("reader-1"))
        .await
        .expect("message should queue");
    wait_for_state(&module, AssistantMessageState::Failed).await;

    module
        .dispatch(DispatchReadingAssistantInput {
            session_id: SessionId::from("session-1"),
            document_id: DocumentId::from("document-1"),
            command: ReadingAssistantCommand::RetryResponse {
                user_message_id: ReadingMessageId::from("reader-1"),
            },
        })
        .await
        .expect("retry should queue");
    let ready = wait_for_state(&module, AssistantMessageState::Ready).await;

    assert_eq!(
        ready
            .messages
            .iter()
            .filter(|message| matches!(message, ReadingMessageView::Reader { .. }))
            .count(),
        1
    );
    assert_eq!(ready.messages.len(), 3);
}

#[tokio::test]
async fn recovery_marks_interrupted_response_failed_without_resending() {
    let store = Arc::new(MemoryStore::default());
    store
        .queue_response(&QueuedReadingResponse {
            conversation_id: ConversationId::from("conversation-1"),
            document_id: DocumentId::from("document-1"),
            reader: Some(NewReaderMessage {
                id: ReadingMessageId::from("reader-1"),
                text: "Question".to_owned(),
                selection: None,
                created_at: 1,
            }),
            assistant: NewAssistantResponse {
                id: ReadingMessageId::from("assistant-1"),
                responding_to: ReadingMessageId::from("reader-1"),
                retry_of_message_id: None,
                endpoint_fingerprint: "endpoint".to_owned(),
                model_id: "model".to_owned(),
                created_at: 2,
            },
        })
        .await
        .expect("response should queue");
    let provider = Arc::new(ScriptedReadingAssistantAdapter::new(VecDeque::new()));
    let (module, _) = module(store, provider);

    assert_eq!(module.recover().await.expect("recovery should run"), 1);
    let snapshot = module
        .view(&DocumentId::from("document-1"))
        .await
        .expect("snapshot should load");
    let ReadingMessageView::Assistant {
        state,
        safe_message,
        ..
    } = &snapshot.messages[1]
    else {
        panic!("second message should be assistant");
    };
    assert_eq!(*state, AssistantMessageState::Failed);
    assert!(
        safe_message
            .as_deref()
            .is_some_and(|value| value.contains("interrupted"))
    );
}

#[test]
fn recent_history_keeps_four_complete_turns_without_orphans() {
    let mut messages = Vec::new();
    for index in 1..=5 {
        let reader_id = ReadingMessageId::new(format!("reader-{index}"));
        messages.push(ReadingMessageView::Reader {
            id: reader_id.clone(),
            text: format!("question-{index}"),
            selection_context: None,
            created_at: index,
        });
        messages.push(ReadingMessageView::Assistant {
            id: ReadingMessageId::new(format!("assistant-{index}")),
            responding_to: reader_id,
            state: AssistantMessageState::Ready,
            text: format!("answer-{index}"),
            citations: Vec::new(),
            retry_of_message_id: None,
            safe_message: None,
            created_at: index,
            updated_at: index,
        });
    }
    messages.push(ReadingMessageView::Reader {
        id: ReadingMessageId::from("reader-failed"),
        text: "failed-question".to_owned(),
        selection_context: None,
        created_at: 6,
    });
    messages.push(ReadingMessageView::Assistant {
        id: ReadingMessageId::from("assistant-failed"),
        responding_to: ReadingMessageId::from("reader-failed"),
        state: AssistantMessageState::Failed,
        text: "partial".to_owned(),
        citations: Vec::new(),
        retry_of_message_id: None,
        safe_message: Some("failed".to_owned()),
        created_at: 6,
        updated_at: 6,
    });
    let snapshot = ReadingAssistantSnapshot {
        schema_version: 1,
        conversation_id: Some(ConversationId::from("conversation-1")),
        messages,
        active_assistant_message_id: None,
        latest_selection: None,
    };

    let history = recent_history(&snapshot, None);

    assert_eq!(history.len(), 8);
    assert_eq!(history[0].role, "reader");
    assert_eq!(history[0].text, "question-2");
    assert_eq!(history[1].role, "assistant");
    assert_eq!(history[1].text, "answer-2");
    assert_eq!(history[7].text, "answer-5");
    assert!(
        !history
            .iter()
            .any(|message| message.text.contains("failed"))
    );
}

#[test]
fn request_budget_removes_history_as_complete_turns() {
    let translation = translation();
    let context = SelectionContextAssembler::new()
        .assemble(
            &document(),
            translation
                .active_chapter
                .as_ref()
                .expect("chapter translation should exist"),
            &selection(),
            ContextBudget::default(),
        )
        .expect("context should assemble");
    let mut messages = Vec::new();
    for index in 1..=6 {
        let reader_id = ReadingMessageId::new(format!("reader-{index}"));
        messages.push(ReadingMessageView::Reader {
            id: reader_id.clone(),
            text: format!("question-{index}-{}", "x".repeat(500)),
            selection_context: None,
            created_at: index,
        });
        messages.push(ReadingMessageView::Assistant {
            id: ReadingMessageId::new(format!("assistant-{index}")),
            responding_to: reader_id,
            state: AssistantMessageState::Ready,
            text: format!("answer-{index}-{}", "y".repeat(500)),
            citations: Vec::new(),
            retry_of_message_id: None,
            safe_message: None,
            created_at: index,
            updated_at: index,
        });
    }
    let snapshot = ReadingAssistantSnapshot {
        schema_version: 1,
        conversation_id: Some(ConversationId::from("conversation-1")),
        messages,
        active_assistant_message_id: None,
        latest_selection: None,
    };
    let mut configuration = configuration();
    configuration.context_window = 2_048;
    let tokenizer = tiktoken_rs::cl100k_base().ok();

    let work = prepare_provider_work(
        &configuration,
        &snapshot,
        context,
        "current question",
        None,
        tokenizer.as_ref(),
    )
    .expect("request should fit after trimming");
    let input: serde_json::Value =
        serde_json::from_str(&work.request.input_json).expect("request should be JSON");
    let history = input["recentMessages"]
        .as_array()
        .expect("history should be an array");

    assert_eq!(history.len() % 2, 0);
    assert!(history.len() <= 8);
    for turn in history.chunks(2) {
        assert_eq!(turn[0]["role"], "reader");
        assert_eq!(turn[1]["role"], "assistant");
    }
}
