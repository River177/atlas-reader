use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{BlockId, ChapterId, CitationId, ConversationId, ReadingMessageId};

pub const READING_ASSISTANT_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct SelectionContextInput {
    pub block_id: BlockId,
    pub source_digest: String,
    pub start_utf16: u32,
    pub end_utf16: u32,
    pub selected_text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct SelectionContext {
    pub block_id: BlockId,
    pub chapter_id: ChapterId,
    pub page_start: u32,
    pub page_end: u32,
    pub source_digest: String,
    pub start_utf16: u32,
    pub end_utf16: u32,
    pub selected_text: String,
    pub aligned_source: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct CitationTarget {
    pub id: CitationId,
    pub block_id: BlockId,
    pub chapter_id: ChapterId,
    pub page: u32,
    pub label: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum AssistantMessageState {
    Queued,
    Streaming,
    Ready,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "role", rename_all = "snake_case")]
#[ts(export, tag = "role", rename_all = "snake_case")]
pub enum ReadingMessageView {
    Reader {
        id: ReadingMessageId,
        text: String,
        #[serde(rename = "selectionContext")]
        #[ts(rename = "selectionContext")]
        selection_context: Option<SelectionContext>,
        #[serde(rename = "createdAt")]
        #[ts(rename = "createdAt", type = "number")]
        created_at: u64,
    },
    Assistant {
        id: ReadingMessageId,
        #[serde(rename = "respondingTo")]
        #[ts(rename = "respondingTo")]
        responding_to: ReadingMessageId,
        state: AssistantMessageState,
        text: String,
        citations: Vec<CitationTarget>,
        #[serde(rename = "retryOfMessageId")]
        #[ts(rename = "retryOfMessageId")]
        retry_of_message_id: Option<ReadingMessageId>,
        #[serde(rename = "safeMessage")]
        #[ts(rename = "safeMessage")]
        safe_message: Option<String>,
        #[serde(rename = "createdAt")]
        #[ts(rename = "createdAt", type = "number")]
        created_at: u64,
        #[serde(rename = "updatedAt")]
        #[ts(rename = "updatedAt", type = "number")]
        updated_at: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, tag = "type", rename_all = "snake_case")]
pub enum ReadingAssistantCommand {
    SendMessage {
        #[serde(rename = "userMessageId")]
        #[ts(rename = "userMessageId")]
        user_message_id: ReadingMessageId,
        text: String,
        selection: Option<SelectionContextInput>,
    },
    CancelResponse {
        #[serde(rename = "assistantMessageId")]
        #[ts(rename = "assistantMessageId")]
        assistant_message_id: ReadingMessageId,
    },
    RetryResponse {
        #[serde(rename = "userMessageId")]
        #[ts(rename = "userMessageId")]
        user_message_id: ReadingMessageId,
    },
    ClearConversation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct ReadingAssistantSnapshot {
    pub schema_version: u16,
    pub conversation_id: Option<ConversationId>,
    pub messages: Vec<ReadingMessageView>,
    pub active_assistant_message_id: Option<ReadingMessageId>,
    pub latest_selection: Option<SelectionContext>,
}

impl Default for ReadingAssistantSnapshot {
    fn default() -> Self {
        Self {
            schema_version: READING_ASSISTANT_SCHEMA_VERSION,
            conversation_id: None,
            messages: Vec::new(),
            active_assistant_message_id: None,
            latest_selection: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_message_uses_the_stable_camel_case_wire_shape() {
        let encoded = serde_json::to_value(ReadingAssistantCommand::SendMessage {
            user_message_id: ReadingMessageId::from("message-1"),
            text: "Why is this assumption necessary?".to_owned(),
            selection: Some(SelectionContextInput {
                block_id: BlockId::from("block-1"),
                source_digest: "source-digest".to_owned(),
                start_utf16: 2,
                end_utf16: 8,
                selected_text: "该假设".to_owned(),
            }),
        })
        .expect("command should encode");

        assert_eq!(encoded["type"], "send_message");
        assert_eq!(encoded["userMessageId"], "message-1");
        assert_eq!(encoded["selection"]["blockId"], "block-1");
        assert_eq!(encoded["selection"]["startUtf16"], 2);
        assert_eq!(encoded["selection"]["selectedText"], "该假设");
    }

    #[test]
    fn assistant_messages_keep_retry_and_citation_relationships() {
        let message = ReadingMessageView::Assistant {
            id: ReadingMessageId::from("assistant-2"),
            responding_to: ReadingMessageId::from("reader-1"),
            state: AssistantMessageState::Ready,
            text: "It limits the comparison class.".to_owned(),
            citations: vec![CitationTarget {
                id: CitationId::from("citation-1"),
                block_id: BlockId::from("block-1"),
                chapter_id: ChapterId::from("chapter-1"),
                page: 4,
                label: "§2 · p. 4".to_owned(),
            }],
            retry_of_message_id: Some(ReadingMessageId::from("assistant-1")),
            safe_message: None,
            created_at: 10,
            updated_at: 20,
        };
        let encoded = serde_json::to_value(&message).expect("message should encode");
        let decoded: ReadingMessageView =
            serde_json::from_value(encoded.clone()).expect("message should decode");

        assert_eq!(encoded["role"], "assistant");
        assert_eq!(encoded["respondingTo"], "reader-1");
        assert_eq!(encoded["retryOfMessageId"], "assistant-1");
        assert_eq!(encoded["citations"][0]["page"], 4);
        assert_eq!(decoded, message);
    }

    #[test]
    fn an_empty_snapshot_has_no_persisted_conversation() {
        let snapshot = ReadingAssistantSnapshot::default();
        let encoded = serde_json::to_value(&snapshot).expect("snapshot should encode");

        assert_eq!(
            snapshot,
            ReadingAssistantSnapshot {
                schema_version: READING_ASSISTANT_SCHEMA_VERSION,
                conversation_id: None,
                messages: Vec::new(),
                active_assistant_message_id: None,
                latest_selection: None,
            }
        );
        assert!(encoded["conversationId"].is_null());
        assert!(encoded["activeAssistantMessageId"].is_null());
        assert!(encoded["latestSelection"].is_null());
    }
}
