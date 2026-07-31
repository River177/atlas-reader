use atlas_contracts::{
    AtlasError, CommandId, CommandReceipt, OpenSessionInput, OpenSessionResult, ReadingCommand,
    SessionId, SessionSnapshot,
};
use serde::Deserialize;
use tauri::State;

use crate::app_state::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchSessionInput {
    pub session_id: SessionId,
    pub command_id: CommandId,
    pub expected_revision: Option<u32>,
    pub command: ReadingCommand,
}

#[tauri::command]
pub async fn reading_session_open(
    state: State<'_, AppState>,
    input: OpenSessionInput,
) -> Result<OpenSessionResult, AtlasError> {
    state.reading_session.open(input).await
}

#[tauri::command]
pub async fn reading_session_dispatch(
    state: State<'_, AppState>,
    input: DispatchSessionInput,
) -> Result<CommandReceipt, AtlasError> {
    state
        .reading_session
        .dispatch(
            &input.session_id,
            input.command_id,
            input.expected_revision,
            input.command,
        )
        .await
}

#[tauri::command]
pub async fn reading_session_close(
    state: State<'_, AppState>,
    session_id: SessionId,
) -> Result<(), AtlasError> {
    state.reading_session.close(&session_id).await
}

#[tauri::command]
pub async fn reading_session_snapshot(
    state: State<'_, AppState>,
    session_id: SessionId,
) -> Result<SessionSnapshot, AtlasError> {
    state.reading_session.snapshot(&session_id).await
}
