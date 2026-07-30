use atlas_contracts::{
    AtlasError, DocumentId, OpenedReaderDocument, ReaderSourceToken, ReadingPosition,
    ReadingPositionUpdate,
};
use serde::Deserialize;
use tauri::State;

use crate::app_state::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenReaderInput {
    pub document_id: DocumentId,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavePositionInput {
    pub source_token: ReaderSourceToken,
    pub position: ReadingPositionUpdate,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseReaderInput {
    pub source_token: ReaderSourceToken,
    pub final_position: Option<ReadingPositionUpdate>,
}

#[tauri::command]
pub async fn reader_open(
    state: State<'_, AppState>,
    input: OpenReaderInput,
) -> Result<OpenedReaderDocument, AtlasError> {
    state.document_reader.open(input.document_id).await
}

#[tauri::command]
pub async fn reader_save_position(
    state: State<'_, AppState>,
    input: SavePositionInput,
) -> Result<ReadingPosition, AtlasError> {
    state
        .document_reader
        .save_position(&input.source_token, input.position)
        .await
}

#[tauri::command]
pub async fn reader_close(
    state: State<'_, AppState>,
    input: CloseReaderInput,
) -> Result<(), AtlasError> {
    state
        .document_reader
        .close(&input.source_token, input.final_position)
        .await
}
