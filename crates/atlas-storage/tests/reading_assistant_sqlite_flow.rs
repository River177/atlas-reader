use atlas_domain::{
    AssistantMessageState, BlockId, ChapterId, CitationId, CitationTarget, ConversationId,
    DocumentId, ReadingMessageId, ReadingMessageView, SelectionContext,
};
use atlas_reading_assistant::{
    AssistantResponseCheckpoint, NewAssistantResponse, NewReaderMessage, QueuedReadingResponse,
    ReadingAssistantStore,
};
use atlas_storage::{AtlasDatabase, SqliteReadingAssistantStore};

async fn fixture(database: &AtlasDatabase) {
    sqlx::query(
        "INSERT INTO documents (
           id, sha256, title, authors_json, page_count, file_path,
           file_size_bytes, file_mtime_ms, file_state, created_at,
           updated_at, last_opened_at
         ) VALUES (
           'document-1', 'source-sha', 'Synthetic paper', '[]', 5,
           '/tmp/synthetic.pdf', 100, 1, 'available', 1, 1, 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("document fixture should insert");
    sqlx::query(
        "INSERT INTO jobs (
           id, session_id, document_id, kind, priority, state, input_json,
           attempt_count, max_attempts, run_after, created_at, updated_at
         ) VALUES (
           'parse-job', 'session-parse', 'document-1', 'cloud_parse', 100,
           'succeeded', '{}', 1, 1, 1, 1, 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("parse job fixture should insert");
    sqlx::query(
        "INSERT INTO parse_operations (
           id, job_id, document_id, backend, parser_version,
           normalizer_version, state, data_id, retry_count, created_at, updated_at,
           completed_at
         ) VALUES (
           'parse-operation', 'parse-job', 'document-1', 'cloud_mineru',
           'parser-1', 'normalizer-1', 'succeeded', 'data-1', 0, 1, 1, 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("parse operation fixture should insert");
    sqlx::query(
        "INSERT INTO parse_artifacts (
           id, document_id, parse_operation_id, parser_name, parser_version,
           normalizer_version, canonical_schema_version, source_sha256,
           content_digest, manifest_relative_path, is_active, created_at
         ) VALUES (
           'artifact-1', 'document-1', 'parse-operation', 'Synthetic',
           'parser-1', 'normalizer-1', 1, 'source-sha', 'digest',
           'document-1/artifact-1/document.json', 1, 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("parse artifact fixture should insert");
    sqlx::query(
        "INSERT INTO chapters (
           id, artifact_id, document_id, order_index, depth, role,
           source_title, page_start, page_end, source_digest, created_at
         ) VALUES (
           'chapter-1', 'artifact-1', 'document-1', 0, 1, 'body',
           'Introduction', 1, 5, 'chapter-digest', 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("chapter fixture should insert");
    sqlx::query(
        "INSERT INTO blocks (
           id, chapter_id, order_index, kind, page_start, page_end,
           bounding_boxes_json, source_json, source_plain_text,
           source_digest, created_at
         ) VALUES (
           'block-1', 'chapter-1', 0, 'paragraph', 2, 2, '[]',
           '{\"plainText\":\"this assumption\",\"atoms\":[{\"type\":\"text\",\"value\":\"this assumption\"}]}',
           'this assumption', 'source-digest', 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("block fixture should insert");
}

fn selection() -> SelectionContext {
    SelectionContext {
        block_id: BlockId::from("block-1"),
        chapter_id: ChapterId::from("chapter-1"),
        page_start: 2,
        page_end: 2,
        source_digest: "source-digest".to_owned(),
        start_utf16: 0,
        end_utf16: 4,
        selected_text: "该假设".to_owned(),
        aligned_source: "this assumption".to_owned(),
    }
}

fn queued(
    assistant_id: &str,
    reader: Option<NewReaderMessage>,
    retry_of_message_id: Option<&str>,
    created_at: u64,
) -> QueuedReadingResponse {
    QueuedReadingResponse {
        conversation_id: ConversationId::from("conversation-1"),
        document_id: DocumentId::from("document-1"),
        reader,
        assistant: NewAssistantResponse {
            id: ReadingMessageId::from(assistant_id),
            responding_to: ReadingMessageId::from("reader-1"),
            retry_of_message_id: retry_of_message_id.map(ReadingMessageId::from),
            endpoint_fingerprint: "endpoint-1".to_owned(),
            model_id: "model-1".to_owned(),
            created_at,
        },
    }
}

fn first_turn() -> QueuedReadingResponse {
    queued(
        "assistant-1",
        Some(NewReaderMessage {
            id: ReadingMessageId::from("reader-1"),
            text: "为什么需要这个假设？".to_owned(),
            selection: Some(selection()),
            created_at: 10,
        }),
        None,
        11,
    )
}

fn checkpoint(
    state: AssistantMessageState,
    text: &str,
    updated_at: u64,
) -> AssistantResponseCheckpoint {
    AssistantResponseCheckpoint {
        conversation_id: ConversationId::from("conversation-1"),
        assistant_message_id: ReadingMessageId::from("assistant-1"),
        state,
        text: text.to_owned(),
        citations: vec![CitationTarget {
            id: CitationId::from("citation-1"),
            block_id: BlockId::from("block-1"),
            chapter_id: ChapterId::from("chapter-1"),
            page: 2,
            label: "§1 · p. 2".to_owned(),
        }],
        error_code: None,
        safe_message: None,
        updated_at,
    }
}

#[tokio::test]
async fn first_turn_is_queued_atomically_and_survives_reopen() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let store = SqliteReadingAssistantStore::new(&database);

    let snapshot = store
        .queue_response(&first_turn())
        .await
        .expect("first turn should queue");

    assert_eq!(
        snapshot
            .conversation_id
            .as_ref()
            .map(ConversationId::as_str),
        Some("conversation-1")
    );
    assert_eq!(snapshot.messages.len(), 2);
    assert_eq!(
        snapshot
            .active_assistant_message_id
            .as_ref()
            .map(ReadingMessageId::as_str),
        Some("assistant-1")
    );
    assert_eq!(snapshot.latest_selection, Some(selection()));
    let reopened = SqliteReadingAssistantStore::new(&database)
        .view(&DocumentId::from("document-1"))
        .await
        .expect("conversation should reopen");
    assert_eq!(reopened, snapshot);
    assert_eq!(
        store
            .recoverable_responses()
            .await
            .expect("recovery should load")
            .len(),
        1
    );
}

#[tokio::test]
async fn checkpoints_replace_citations_and_terminal_state_fences_late_writes() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let store = SqliteReadingAssistantStore::new(&database);
    store
        .queue_response(&first_turn())
        .await
        .expect("first turn should queue");

    store
        .checkpoint_response(&checkpoint(
            AssistantMessageState::Streaming,
            "它限制了",
            20,
        ))
        .await
        .expect("stream should checkpoint");
    let ready = checkpoint(AssistantMessageState::Ready, "它限制了比较范围。", 30);
    store
        .checkpoint_response(&ready)
        .await
        .expect("response should finish");

    let snapshot = store
        .view(&DocumentId::from("document-1"))
        .await
        .expect("conversation should load");
    let ReadingMessageView::Assistant {
        state,
        text,
        citations,
        ..
    } = &snapshot.messages[1]
    else {
        panic!("second message should be the assistant");
    };
    assert_eq!(*state, AssistantMessageState::Ready);
    assert_eq!(text, "它限制了比较范围。");
    assert_eq!(citations.len(), 1);
    assert!(snapshot.active_assistant_message_id.is_none());
    assert!(
        store
            .recoverable_responses()
            .await
            .expect("recovery should load")
            .is_empty()
    );
    assert!(
        store
            .checkpoint_response(&AssistantResponseCheckpoint {
                text: "stale worker".to_owned(),
                updated_at: 40,
                ..ready
            })
            .await
            .is_err(),
        "a terminal response must fence a late worker"
    );
    assert!(
        store
            .checkpoint_response(&AssistantResponseCheckpoint {
                state: AssistantMessageState::Queued,
                ..checkpoint(AssistantMessageState::Queued, "", 50)
            })
            .await
            .is_err(),
        "checkpointing can never return a response to queued"
    );
}

#[tokio::test]
async fn citation_foreign_key_failure_rolls_back_the_message_checkpoint() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let store = SqliteReadingAssistantStore::new(&database);
    store
        .queue_response(&first_turn())
        .await
        .expect("first turn should queue");
    let invalid = AssistantResponseCheckpoint {
        citations: vec![CitationTarget {
            id: CitationId::from("citation-invalid"),
            block_id: BlockId::from("missing-block"),
            chapter_id: ChapterId::from("chapter-1"),
            page: 2,
            label: "invalid".to_owned(),
        }],
        ..checkpoint(AssistantMessageState::Streaming, "partial", 20)
    };

    assert!(
        store.checkpoint_response(&invalid).await.is_err(),
        "citation outside persisted Canonical blocks should fail"
    );
    let snapshot = store
        .view(&DocumentId::from("document-1"))
        .await
        .expect("conversation should load");
    let ReadingMessageView::Assistant {
        state,
        text,
        citations,
        ..
    } = &snapshot.messages[1]
    else {
        panic!("second message should be the assistant");
    };
    assert_eq!(*state, AssistantMessageState::Queued);
    assert!(text.is_empty());
    assert!(citations.is_empty());
}

#[tokio::test]
async fn retry_reuses_the_reader_message_without_duplicating_it() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let store = SqliteReadingAssistantStore::new(&database);
    store
        .queue_response(&first_turn())
        .await
        .expect("first turn should queue");
    store
        .checkpoint_response(&AssistantResponseCheckpoint {
            state: AssistantMessageState::Failed,
            error_code: Some("timeout".to_owned()),
            safe_message: Some("The response stopped".to_owned()),
            ..checkpoint(AssistantMessageState::Failed, "部分回答", 20)
        })
        .await
        .expect("first response should fail");

    let snapshot = store
        .queue_response(&queued("assistant-2", None, Some("assistant-1"), 30))
        .await
        .expect("retry should queue");

    assert_eq!(snapshot.messages.len(), 3);
    assert_eq!(
        snapshot
            .messages
            .iter()
            .filter(|message| matches!(message, ReadingMessageView::Reader { .. }))
            .count(),
        1
    );
    let ReadingMessageView::Assistant {
        id,
        responding_to,
        retry_of_message_id,
        state,
        ..
    } = &snapshot.messages[2]
    else {
        panic!("third message should be the retry");
    };
    assert_eq!(id.as_str(), "assistant-2");
    assert_eq!(responding_to.as_str(), "reader-1");
    assert_eq!(
        retry_of_message_id.as_ref().map(ReadingMessageId::as_str),
        Some("assistant-1")
    );
    assert_eq!(*state, AssistantMessageState::Queued);
}

#[tokio::test]
async fn queue_shape_and_retry_must_match_the_exact_reader_and_latest_attempt() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let store = SqliteReadingAssistantStore::new(&database);
    let mismatched = QueuedReadingResponse {
        conversation_id: ConversationId::from("conversation-1"),
        document_id: DocumentId::from("document-1"),
        reader: Some(NewReaderMessage {
            id: ReadingMessageId::from("reader-2"),
            text: "Mismatched".to_owned(),
            selection: None,
            created_at: 1,
        }),
        assistant: NewAssistantResponse {
            id: ReadingMessageId::from("assistant-mismatch"),
            responding_to: ReadingMessageId::from("reader-1"),
            retry_of_message_id: None,
            endpoint_fingerprint: "endpoint-1".to_owned(),
            model_id: "model-1".to_owned(),
            created_at: 2,
        },
    };
    assert_eq!(
        store
            .queue_response(&mismatched)
            .await
            .expect_err("new turn must answer its inserted reader")
            .code,
        atlas_domain::AtlasErrorCode::InvalidInput
    );
    assert_eq!(
        store
            .queue_response(&queued("assistant-orphan", None, None, 3))
            .await
            .expect_err("response without a reader or retry must fail")
            .code,
        atlas_domain::AtlasErrorCode::InvalidInput
    );

    store
        .queue_response(&first_turn())
        .await
        .expect("first turn should queue");
    store
        .checkpoint_response(&AssistantResponseCheckpoint {
            state: AssistantMessageState::Failed,
            ..checkpoint(AssistantMessageState::Failed, "first failed", 10)
        })
        .await
        .expect("first response should fail");
    store
        .queue_response(&queued("assistant-2", None, Some("assistant-1"), 20))
        .await
        .expect("latest failed response should retry");
    store
        .checkpoint_response(&AssistantResponseCheckpoint {
            assistant_message_id: ReadingMessageId::from("assistant-2"),
            state: AssistantMessageState::Ready,
            text: "second succeeded".to_owned(),
            citations: Vec::new(),
            updated_at: 30,
            ..checkpoint(AssistantMessageState::Ready, "", 30)
        })
        .await
        .expect("second response should finish");

    assert_eq!(
        store
            .queue_response(&queued("assistant-3", None, Some("assistant-1"), 40,))
            .await
            .expect_err("an older failed attempt cannot be retried after success")
            .code,
        atlas_domain::AtlasErrorCode::InvalidInput
    );
}

#[tokio::test]
async fn duplicate_terminal_message_id_is_not_misreported_as_assistant_busy() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let store = SqliteReadingAssistantStore::new(&database);
    store
        .queue_response(&first_turn())
        .await
        .expect("first turn should queue");
    store
        .checkpoint_response(&checkpoint(AssistantMessageState::Ready, "complete", 20))
        .await
        .expect("first response should finish");
    let duplicate = QueuedReadingResponse {
        conversation_id: ConversationId::from("conversation-1"),
        document_id: DocumentId::from("document-1"),
        reader: Some(NewReaderMessage {
            id: ReadingMessageId::from("reader-2"),
            text: "Second question".to_owned(),
            selection: None,
            created_at: 30,
        }),
        assistant: NewAssistantResponse {
            id: ReadingMessageId::from("assistant-1"),
            responding_to: ReadingMessageId::from("reader-2"),
            retry_of_message_id: None,
            endpoint_fingerprint: "endpoint-1".to_owned(),
            model_id: "model-1".to_owned(),
            created_at: 31,
        },
    };

    let error = store
        .queue_response(&duplicate)
        .await
        .expect_err("duplicate assistant id should fail");
    assert_ne!(error.code, atlas_domain::AtlasErrorCode::AssistantBusy);
    assert_eq!(
        store
            .view(&DocumentId::from("document-1"))
            .await
            .expect("conversation should load")
            .messages
            .len(),
        2,
        "the failed turn must roll back its reader message"
    );
}

#[tokio::test]
async fn active_response_rejects_another_turn_without_inserting_its_reader_message() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let store = SqliteReadingAssistantStore::new(&database);
    store
        .queue_response(&first_turn())
        .await
        .expect("first turn should queue");
    let second = QueuedReadingResponse {
        conversation_id: ConversationId::from("conversation-1"),
        document_id: DocumentId::from("document-1"),
        reader: Some(NewReaderMessage {
            id: ReadingMessageId::from("reader-2"),
            text: "Another question".to_owned(),
            selection: None,
            created_at: 20,
        }),
        assistant: NewAssistantResponse {
            id: ReadingMessageId::from("assistant-2"),
            responding_to: ReadingMessageId::from("reader-2"),
            retry_of_message_id: None,
            endpoint_fingerprint: "endpoint-1".to_owned(),
            model_id: "model-1".to_owned(),
            created_at: 21,
        },
    };

    let error = store
        .queue_response(&second)
        .await
        .expect_err("a second active response should be rejected");
    assert_eq!(error.code, atlas_domain::AtlasErrorCode::AssistantBusy);
    assert_eq!(
        store
            .view(&DocumentId::from("document-1"))
            .await
            .expect("conversation should load")
            .messages
            .len(),
        2
    );
}

#[tokio::test]
async fn clear_removes_messages_and_citations_but_not_the_document() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let store = SqliteReadingAssistantStore::new(&database);
    store
        .queue_response(&first_turn())
        .await
        .expect("first turn should queue");
    store
        .checkpoint_response(&checkpoint(AssistantMessageState::Ready, "完整回答", 20))
        .await
        .expect("response should finish");

    assert!(
        store
            .clear(&DocumentId::from("document-1"))
            .await
            .expect("clear should succeed")
    );
    assert_eq!(
        store
            .view(&DocumentId::from("document-1"))
            .await
            .expect("empty view should load"),
        atlas_domain::ReadingAssistantSnapshot::default()
    );
    let document_count =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM documents WHERE id = 'document-1'")
            .fetch_one(database.pool())
            .await
            .expect("document count should load");
    assert_eq!(document_count, 1);
}
