use std::collections::HashMap;

use async_trait::async_trait;
use atlas_domain::{
    AssistantMessageState, AtlasError, CitationId, CitationTarget, ConversationId, DocumentId,
    ReadingAssistantSnapshot, ReadingMessageId, ReadingMessageView, SelectionContext,
};
use atlas_reading_assistant::{
    AssistantResponseCheckpoint, QueuedReadingResponse, ReadingAssistantStore,
    RecoverableReadingResponse,
};
use serde_json::json;
use sqlx::{Row, Sqlite, SqlitePool, Transaction, sqlite::SqliteRow};

use crate::{AtlasDatabase, map_sqlx, to_i64, to_u64};

#[derive(Clone, Debug)]
pub struct SqliteReadingAssistantStore {
    pool: SqlitePool,
}

impl SqliteReadingAssistantStore {
    #[must_use]
    pub fn new(database: &AtlasDatabase) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }

    async fn snapshot_in(
        transaction: &mut Transaction<'_, Sqlite>,
        document_id: &DocumentId,
    ) -> Result<ReadingAssistantSnapshot, AtlasError> {
        let conversation = sqlx::query(
            "SELECT id
             FROM reading_conversations
             WHERE document_id = ?1",
        )
        .bind(document_id.as_str())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        let Some(conversation) = conversation else {
            return Ok(ReadingAssistantSnapshot::default());
        };
        let conversation_id =
            ConversationId::new(conversation.try_get::<String, _>("id").map_err(map_sqlx)?);
        let rows = sqlx::query(
            "SELECT *
             FROM reading_messages
             WHERE conversation_id = ?1
             ORDER BY sequence",
        )
        .bind(conversation_id.as_str())
        .fetch_all(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        let citations = load_citations(transaction, &conversation_id).await?;
        let mut messages = Vec::with_capacity(rows.len());
        let mut active_assistant_message_id = None;
        let mut latest_selection = None;
        for row in rows {
            let message = row_to_message(row, &citations)?;
            match &message {
                ReadingMessageView::Reader {
                    selection_context: Some(selection),
                    ..
                } => latest_selection = Some(selection.clone()),
                ReadingMessageView::Assistant {
                    id,
                    state: AssistantMessageState::Queued | AssistantMessageState::Streaming,
                    ..
                } => {
                    active_assistant_message_id = Some(id.clone());
                }
                _ => {}
            }
            messages.push(message);
        }
        Ok(ReadingAssistantSnapshot {
            schema_version: atlas_domain::READING_ASSISTANT_SCHEMA_VERSION,
            conversation_id: Some(conversation_id),
            messages,
            active_assistant_message_id,
            latest_selection,
        })
    }
}

#[async_trait]
impl ReadingAssistantStore for SqliteReadingAssistantStore {
    async fn view(&self, document_id: &DocumentId) -> Result<ReadingAssistantSnapshot, AtlasError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        let snapshot = Self::snapshot_in(&mut transaction, document_id).await?;
        transaction.commit().await.map_err(map_sqlx)?;
        Ok(snapshot)
    }

    async fn queue_response(
        &self,
        response: &QueuedReadingResponse,
    ) -> Result<ReadingAssistantSnapshot, AtlasError> {
        validate_queue_shape(response)?;
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        let created_at = response
            .reader
            .as_ref()
            .map_or(response.assistant.created_at, |reader| reader.created_at);
        sqlx::query(
            "INSERT INTO reading_conversations (id, document_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(document_id) DO NOTHING",
        )
        .bind(response.conversation_id.as_str())
        .bind(response.document_id.as_str())
        .bind(to_i64(created_at, "conversation creation time")?)
        .bind(to_i64(
            response.assistant.created_at,
            "conversation update time",
        )?)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        let stored_conversation_id = sqlx::query_scalar::<_, String>(
            "SELECT id FROM reading_conversations WHERE document_id = ?1",
        )
        .bind(response.document_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        if stored_conversation_id != response.conversation_id.as_str() {
            return Err(AtlasError::storage(
                "the document already belongs to another Reading Conversation",
            ));
        }
        if sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)
             FROM reading_messages
             WHERE conversation_id = ?1
               AND role = 'assistant'
               AND state IN ('queued', 'streaming')",
        )
        .bind(response.conversation_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_sqlx)?
            > 0
        {
            return Err(AtlasError::assistant_busy());
        }
        let mut sequence = next_sequence(&mut transaction, &response.conversation_id).await?;
        if let Some(reader) = response.reader.as_ref() {
            let selection_json = reader
                .selection
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| AtlasError::storage(error.to_string()))?;
            sqlx::query(
                "INSERT INTO reading_messages (
                   id, conversation_id, role, state, text, selection_context_json,
                   sequence, created_at, updated_at
                 ) VALUES (?1, ?2, 'reader', 'ready', ?3, ?4, ?5, ?6, ?6)",
            )
            .bind(reader.id.as_str())
            .bind(response.conversation_id.as_str())
            .bind(&reader.text)
            .bind(selection_json)
            .bind(sequence)
            .bind(to_i64(reader.created_at, "reader message creation time")?)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
            sequence = sequence.saturating_add(1);
        }
        validate_response_relationship(&mut transaction, response).await?;
        let insert = sqlx::query(
            "INSERT INTO reading_messages (
               id, conversation_id, role, state, text, responding_to_message_id,
               retry_of_message_id, endpoint_fingerprint, model_id, sequence,
               created_at, updated_at
             ) VALUES (
               ?1, ?2, 'assistant', 'queued', '', ?3, ?4, ?5, ?6, ?7, ?8, ?8
             )",
        )
        .bind(response.assistant.id.as_str())
        .bind(response.conversation_id.as_str())
        .bind(response.assistant.responding_to.as_str())
        .bind(
            response
                .assistant
                .retry_of_message_id
                .as_ref()
                .map(ReadingMessageId::as_str),
        )
        .bind(&response.assistant.endpoint_fingerprint)
        .bind(&response.assistant.model_id)
        .bind(sequence)
        .bind(to_i64(
            response.assistant.created_at,
            "assistant message creation time",
        )?)
        .execute(&mut *transaction)
        .await;
        if let Err(error) = insert {
            if error
                .as_database_error()
                .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
                && has_active_response(&mut transaction, &response.conversation_id).await?
            {
                return Err(AtlasError::assistant_busy());
            }
            return Err(map_sqlx(error));
        }
        sqlx::query(
            "UPDATE reading_conversations
             SET updated_at = ?2
             WHERE id = ?1",
        )
        .bind(response.conversation_id.as_str())
        .bind(to_i64(
            response.assistant.created_at,
            "conversation update time",
        )?)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        let snapshot = Self::snapshot_in(&mut transaction, &response.document_id).await?;
        transaction.commit().await.map_err(map_sqlx)?;
        Ok(snapshot)
    }

    async fn checkpoint_response(
        &self,
        checkpoint: &AssistantResponseCheckpoint,
    ) -> Result<(), AtlasError> {
        if checkpoint.state == AssistantMessageState::Queued {
            return Err(AtlasError::invalid_input(
                "A Reading Assistant checkpoint cannot return to queued",
            ));
        }
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        let error_safe_json = checkpoint
            .safe_message
            .as_ref()
            .map(|message| json!({ "message": message }).to_string());
        let updated = sqlx::query(
            "UPDATE reading_messages
             SET state = ?3,
                 text = ?4,
                 error_code = ?5,
                 error_safe_json = ?6,
                 updated_at = ?7
             WHERE id = ?1
               AND conversation_id = ?2
               AND role = 'assistant'
               AND state IN ('queued', 'streaming')",
        )
        .bind(checkpoint.assistant_message_id.as_str())
        .bind(checkpoint.conversation_id.as_str())
        .bind(message_state(checkpoint.state))
        .bind(&checkpoint.text)
        .bind(&checkpoint.error_code)
        .bind(error_safe_json)
        .bind(to_i64(checkpoint.updated_at, "assistant checkpoint time")?)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        if updated.rows_affected() != 1 {
            return Err(AtlasError::storage(
                "the Reading Assistant response changed before it could be checkpointed",
            ));
        }
        sqlx::query("DELETE FROM reading_citations WHERE message_id = ?1")
            .bind(checkpoint.assistant_message_id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
        for (index, citation) in checkpoint.citations.iter().enumerate() {
            sqlx::query(
                "INSERT INTO reading_citations (
                   id, message_id, chapter_id, block_id, page, label, order_index
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .bind(citation.id.as_str())
            .bind(checkpoint.assistant_message_id.as_str())
            .bind(citation.chapter_id.as_str())
            .bind(citation.block_id.as_str())
            .bind(i64::from(citation.page))
            .bind(&citation.label)
            .bind(i64::try_from(index).map_err(|_| {
                AtlasError::storage("citation order is outside the supported range")
            })?)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
        }
        sqlx::query(
            "UPDATE reading_conversations
             SET updated_at = ?2
             WHERE id = ?1",
        )
        .bind(checkpoint.conversation_id.as_str())
        .bind(to_i64(checkpoint.updated_at, "conversation update time")?)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        transaction.commit().await.map_err(map_sqlx)
    }

    async fn clear(&self, document_id: &DocumentId) -> Result<bool, AtlasError> {
        sqlx::query("DELETE FROM reading_conversations WHERE document_id = ?1")
            .bind(document_id.as_str())
            .execute(&self.pool)
            .await
            .map_err(map_sqlx)
            .map(|result| result.rows_affected() == 1)
    }

    async fn recoverable_responses(&self) -> Result<Vec<RecoverableReadingResponse>, AtlasError> {
        sqlx::query(
            "SELECT c.id AS conversation_id, c.document_id, m.id AS message_id,
                    m.responding_to_message_id
             FROM reading_messages AS m
             JOIN reading_conversations AS c ON c.id = m.conversation_id
             WHERE m.role = 'assistant' AND m.state IN ('queued', 'streaming')
             ORDER BY c.updated_at, m.sequence",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?
        .into_iter()
        .map(|row| {
            Ok(RecoverableReadingResponse {
                conversation_id: ConversationId::new(
                    row.try_get::<String, _>("conversation_id")
                        .map_err(map_sqlx)?,
                ),
                document_id: DocumentId::new(
                    row.try_get::<String, _>("document_id").map_err(map_sqlx)?,
                ),
                assistant_message_id: ReadingMessageId::new(
                    row.try_get::<String, _>("message_id").map_err(map_sqlx)?,
                ),
                responding_to: ReadingMessageId::new(
                    row.try_get::<String, _>("responding_to_message_id")
                        .map_err(map_sqlx)?,
                ),
            })
        })
        .collect()
    }
}

async fn next_sequence(
    transaction: &mut Transaction<'_, Sqlite>,
    conversation_id: &ConversationId,
) -> Result<i64, AtlasError> {
    sqlx::query_scalar(
        "SELECT COALESCE(MAX(sequence), -1) + 1
         FROM reading_messages
         WHERE conversation_id = ?1",
    )
    .bind(conversation_id.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map_err(map_sqlx)
}

fn validate_queue_shape(response: &QueuedReadingResponse) -> Result<(), AtlasError> {
    match (
        response.reader.as_ref(),
        response.assistant.retry_of_message_id.as_ref(),
    ) {
        (Some(reader), None) if reader.id == response.assistant.responding_to => Ok(()),
        (None, Some(_)) => Ok(()),
        _ => Err(AtlasError::invalid_input(
            "A Reading Assistant response must be a new turn or a retry",
        )),
    }
}

async fn validate_response_relationship(
    transaction: &mut Transaction<'_, Sqlite>,
    response: &QueuedReadingResponse,
) -> Result<(), AtlasError> {
    let reader_exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)
         FROM reading_messages
         WHERE id = ?1 AND conversation_id = ?2 AND role = 'reader'",
    )
    .bind(response.assistant.responding_to.as_str())
    .bind(response.conversation_id.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map_err(map_sqlx)?;
    if reader_exists != 1 {
        return Err(AtlasError::invalid_input(
            "The Reading Assistant response has no reader message",
        ));
    }
    if let Some(retry_id) = response.assistant.retry_of_message_id.as_ref() {
        let latest_attempt = sqlx::query(
            "SELECT id, state
             FROM reading_messages
             WHERE conversation_id = ?1
               AND role = 'assistant'
               AND responding_to_message_id = ?2
             ORDER BY sequence DESC
             LIMIT 1",
        )
        .bind(response.conversation_id.as_str())
        .bind(response.assistant.responding_to.as_str())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        let valid = latest_attempt.is_some_and(|row| {
            let id = row.try_get::<String, _>("id").ok();
            let state = row.try_get::<String, _>("state").ok();
            id.as_deref() == Some(retry_id.as_str())
                && state.is_some_and(|state| matches!(state.as_str(), "failed" | "cancelled"))
        });
        if !valid {
            return Err(AtlasError::invalid_input(
                "Only the latest failed or cancelled response can be retried",
            ));
        }
    }
    Ok(())
}

async fn has_active_response(
    transaction: &mut Transaction<'_, Sqlite>,
    conversation_id: &ConversationId,
) -> Result<bool, AtlasError> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)
         FROM reading_messages
         WHERE conversation_id = ?1
           AND role = 'assistant'
           AND state IN ('queued', 'streaming')",
    )
    .bind(conversation_id.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map_err(map_sqlx)
    .map(|count| count > 0)
}

async fn load_citations(
    transaction: &mut Transaction<'_, Sqlite>,
    conversation_id: &ConversationId,
) -> Result<HashMap<String, Vec<CitationTarget>>, AtlasError> {
    let rows = sqlx::query(
        "SELECT c.*
         FROM reading_citations AS c
         JOIN reading_messages AS m ON m.id = c.message_id
         WHERE m.conversation_id = ?1
         ORDER BY m.sequence, c.order_index",
    )
    .bind(conversation_id.as_str())
    .fetch_all(&mut **transaction)
    .await
    .map_err(map_sqlx)?;
    let mut citations = HashMap::<String, Vec<CitationTarget>>::new();
    for row in rows {
        let message_id: String = row.try_get("message_id").map_err(map_sqlx)?;
        citations
            .entry(message_id)
            .or_default()
            .push(CitationTarget {
                id: CitationId::new(row.try_get::<String, _>("id").map_err(map_sqlx)?),
                block_id: atlas_domain::BlockId::new(
                    row.try_get::<String, _>("block_id").map_err(map_sqlx)?,
                ),
                chapter_id: atlas_domain::ChapterId::new(
                    row.try_get::<String, _>("chapter_id").map_err(map_sqlx)?,
                ),
                page: u32::try_from(row.try_get::<i64, _>("page").map_err(map_sqlx)?)
                    .map_err(|_| AtlasError::storage("citation page is outside u32 range"))?,
                label: row.try_get("label").map_err(map_sqlx)?,
            });
    }
    Ok(citations)
}

fn row_to_message(
    row: SqliteRow,
    citations: &HashMap<String, Vec<CitationTarget>>,
) -> Result<ReadingMessageView, AtlasError> {
    let id: String = row.try_get("id").map_err(map_sqlx)?;
    let role: String = row.try_get("role").map_err(map_sqlx)?;
    let created_at = to_u64(
        row.try_get::<i64, _>("created_at").map_err(map_sqlx)?,
        "reading message creation time",
    )?;
    match role.as_str() {
        "reader" => {
            let selection_context_json: Option<String> =
                row.try_get("selection_context_json").map_err(map_sqlx)?;
            let selection_context = selection_context_json
                .map(|value| {
                    serde_json::from_str::<SelectionContext>(&value)
                        .map_err(|error| AtlasError::storage(error.to_string()))
                })
                .transpose()?;
            Ok(ReadingMessageView::Reader {
                id: ReadingMessageId::new(id),
                text: row.try_get("text").map_err(map_sqlx)?,
                selection_context,
                created_at,
            })
        }
        "assistant" => {
            let state: String = row.try_get("state").map_err(map_sqlx)?;
            let error_safe_json: Option<String> =
                row.try_get("error_safe_json").map_err(map_sqlx)?;
            Ok(ReadingMessageView::Assistant {
                id: ReadingMessageId::new(id.clone()),
                responding_to: ReadingMessageId::new(
                    row.try_get::<String, _>("responding_to_message_id")
                        .map_err(map_sqlx)?,
                ),
                state: parse_message_state(&state)?,
                text: row.try_get("text").map_err(map_sqlx)?,
                citations: citations.get(&id).cloned().unwrap_or_default(),
                retry_of_message_id: row
                    .try_get::<Option<String>, _>("retry_of_message_id")
                    .map_err(map_sqlx)?
                    .map(ReadingMessageId::new),
                safe_message: safe_message(error_safe_json.as_deref()),
                created_at,
                updated_at: to_u64(
                    row.try_get::<i64, _>("updated_at").map_err(map_sqlx)?,
                    "reading message update time",
                )?,
            })
        }
        _ => Err(AtlasError::storage(
            "unknown Reading Assistant message role",
        )),
    }
}

fn message_state(state: AssistantMessageState) -> &'static str {
    match state {
        AssistantMessageState::Queued => "queued",
        AssistantMessageState::Streaming => "streaming",
        AssistantMessageState::Ready => "ready",
        AssistantMessageState::Failed => "failed",
        AssistantMessageState::Cancelled => "cancelled",
    }
}

fn parse_message_state(state: &str) -> Result<AssistantMessageState, AtlasError> {
    match state {
        "queued" => Ok(AssistantMessageState::Queued),
        "streaming" => Ok(AssistantMessageState::Streaming),
        "ready" => Ok(AssistantMessageState::Ready),
        "failed" => Ok(AssistantMessageState::Failed),
        "cancelled" => Ok(AssistantMessageState::Cancelled),
        _ => Err(AtlasError::storage(
            "unknown Reading Assistant message state",
        )),
    }
}

fn safe_message(error_safe_json: Option<&str>) -> Option<String> {
    error_safe_json
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .and_then(|value| value.get("message")?.as_str().map(str::to_owned))
}
