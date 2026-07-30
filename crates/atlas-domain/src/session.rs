use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{AtlasError, ChapterId, CommandId, DocumentId, JobId, SessionId};

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
    Uploading,
    Processing,
    Downloading,
    Normalizing,
    Ready,
    Degraded,
    Failed,
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
    ClearDocumentPreferences {
        #[serde(rename = "documentId")]
        #[ts(rename = "documentId")]
        document_id: DocumentId,
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
