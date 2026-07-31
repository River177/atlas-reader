use atlas_contracts::{AtlasError, DocumentId, ParseSnapshot, ParsedDocumentView, SessionId};
use serde::Deserialize;
use tauri::State;

use crate::app_state::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentParseInput {
    pub document_id: DocumentId,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReuploadParseInput {
    pub document_id: DocumentId,
    pub session_id: SessionId,
}

#[tauri::command]
pub async fn parse_view(
    state: State<'_, AppState>,
    input: DocumentParseInput,
) -> Result<ParsedDocumentView, AtlasError> {
    state.parse.view(&input.document_id).await
}

#[tauri::command]
pub async fn parse_retry_remote(
    state: State<'_, AppState>,
    input: DocumentParseInput,
) -> Result<ParseSnapshot, AtlasError> {
    state.parse.retry_remote_status(&input.document_id).await
}

#[tauri::command]
pub async fn parse_reupload(
    state: State<'_, AppState>,
    input: ReuploadParseInput,
) -> Result<ParseSnapshot, AtlasError> {
    state
        .parse
        .reupload(input.document_id, input.session_id.as_str().to_owned())
        .await
}
