use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    AtlasError, CanonicalDocument, ChapterId, CommandId, DocumentId, JobId,
    ReadingAssistantCommand, ReadingAssistantSnapshot, SessionId, TranslationSnapshot,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum SessionLifecycle {
    Opening,
    Parsing,
    Ready,
    Degraded,
    Blocked,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum ParseState {
    NotStarted,
    Queued,
    Uploading,
    Processing,
    Downloading,
    Normalizing,
    Ready,
    Degraded,
    Failed,
    StatusUnknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum ParseBackend {
    CloudMineru,
    LocalText,
}

impl ParseBackend {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CloudMineru => "cloud_mineru",
            Self::LocalText => "local_text",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cloud_mineru" => Some(Self::CloudMineru),
            "local_text" => Some(Self::LocalText),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct ParseSnapshot {
    pub state: ParseState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<ParseBackend>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_operation_id: Option<String>,
    pub automatic_cloud_parsing_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safe_message: Option<String>,
}

impl Default for ParseSnapshot {
    fn default() -> Self {
        Self {
            state: ParseState::NotStarted,
            backend: None,
            progress: None,
            parse_operation_id: None,
            automatic_cloud_parsing_enabled: false,
            safe_message: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct ParsedDocumentView {
    pub parse: ParseSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<CanonicalDocument>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum ProviderState {
    NotConfigured,
    Ready,
    Unreachable,
    Unauthorized,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct ProviderStatusSnapshot {
    pub mineru: ProviderState,
    pub translation: ProviderState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation_model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub schema_version: u16,
    pub session_id: SessionId,
    pub document_id: DocumentId,
    pub revision: u32,
    pub lifecycle: SessionLifecycle,
    pub parse_state: ParseState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_chapter_id: Option<ChapterId>,
    pub active_job_ids: Vec<JobId>,
    pub provider_status: ProviderStatusSnapshot,
    pub translation: TranslationSnapshot,
    pub reading_assistant: ReadingAssistantSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct OpenSessionInput {
    pub document_id: DocumentId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_chapter_id: Option<ChapterId>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct OpenSessionResult {
    pub session_id: SessionId,
    pub restored: bool,
    pub snapshot: SessionSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, tag = "type", rename_all = "snake_case")]
pub enum ReadingCommand {
    FocusChapter {
        #[serde(rename = "chapterId")]
        #[ts(rename = "chapterId")]
        chapter_id: ChapterId,
    },
    RetryTranslation {
        #[serde(rename = "chapterId")]
        #[ts(rename = "chapterId")]
        chapter_id: ChapterId,
    },
    ReadingAssistant {
        command: ReadingAssistantCommand,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum CommandStatus {
    Accepted,
    Duplicate,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct CommandReceipt {
    pub command_id: CommandId,
    pub status: CommandStatus,
    pub revision: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejection: Option<AtlasError>,
}

#[cfg(test)]
mod tests {
    use crate::{BlockId, ReadingMessageId, SelectionContextInput};

    use super::*;

    #[test]
    fn reading_assistant_stays_nested_behind_one_session_command_variant() {
        let encoded = serde_json::to_value(ReadingCommand::ReadingAssistant {
            command: ReadingAssistantCommand::SendMessage {
                user_message_id: ReadingMessageId::from("reader-1"),
                text: "Explain this claim".to_owned(),
                selection: Some(SelectionContextInput {
                    block_id: BlockId::from("block-1"),
                    source_digest: "digest".to_owned(),
                    start_utf16: 0,
                    end_utf16: 4,
                    selected_text: "该结论".to_owned(),
                }),
            },
        })
        .expect("command should encode");

        assert_eq!(encoded["type"], "reading_assistant");
        assert_eq!(encoded["command"]["type"], "send_message");
        assert_eq!(encoded["command"]["userMessageId"], "reader-1");
        assert_eq!(encoded["command"]["selection"]["endUtf16"], 4);
    }
}
