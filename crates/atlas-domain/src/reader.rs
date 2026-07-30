use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{DocumentSummary, ReaderSourceToken};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct ReadingPosition {
    pub page: u32,
    pub page_offset_ratio: f64,
    pub scale_value: String,
    #[ts(type = "number")]
    pub updated_at: u64,
}

impl Default for ReadingPosition {
    fn default() -> Self {
        Self {
            page: 1,
            page_offset_ratio: 0.0,
            scale_value: "page-width".to_owned(),
            updated_at: 0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct ReadingPositionUpdate {
    pub page: u32,
    pub page_offset_ratio: f64,
    pub scale_value: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct OpenedReaderDocument {
    pub document: DocumentSummary,
    pub source_token: ReaderSourceToken,
    pub position: ReadingPosition,
}
