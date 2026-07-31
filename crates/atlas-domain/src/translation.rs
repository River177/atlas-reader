use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{BlockId, ChapterId, JobId, StructuredContent};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum TranslationState {
    NotStarted,
    NotConfigured,
    Queued,
    Translating,
    Readable,
    Complete,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum BlockTranslationState {
    Pending,
    Ready,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct TranslatedBlockView {
    pub block_id: BlockId,
    pub source_digest: String,
    pub state: BlockTranslationState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<StructuredContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safe_message: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct ChapterTranslationView {
    pub chapter_id: ChapterId,
    pub state: TranslationState,
    pub progress: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    pub job_active: bool,
    pub blocks: Vec<TranslatedBlockView>,
    pub prefetched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safe_message: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct TranslationSnapshot {
    pub target_locale: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_chapter: Option<ChapterTranslationView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefetched_chapter_id: Option<ChapterId>,
}

impl Default for TranslationSnapshot {
    fn default() -> Self {
        Self {
            target_locale: "zh-CN".to_owned(),
            model_id: None,
            active_chapter: None,
            prefetched_chapter_id: None,
        }
    }
}
