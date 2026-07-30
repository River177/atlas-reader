use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::DocumentId;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum DocumentFileState {
    Available,
    Missing,
    Changed,
    Unreadable,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum LibrarySort {
    #[default]
    Recent,
    Title,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct LibraryQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub sort: LibrarySort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    pub limit: u32,
}

impl Default for LibraryQuery {
    fn default() -> Self {
        Self {
            text: None,
            sort: LibrarySort::Recent,
            cursor: None,
            limit: 30,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct DocumentSummary {
    pub id: DocumentId,
    pub title: String,
    pub authors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u32>,
    pub file_name: String,
    pub source_state: DocumentFileState,
    #[ts(type = "number")]
    pub last_opened_at: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct ImportPdfResult {
    pub document: DocumentSummary,
    pub duplicate: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct RefreshSourcesResult {
    pub updated: Vec<DocumentSummary>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct LibraryPage {
    pub items: Vec<DocumentSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
