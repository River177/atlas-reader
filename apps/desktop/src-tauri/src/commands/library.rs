use atlas_contracts::{AtlasError, LibraryPage, LibraryQuery};
use tauri::State;

use crate::app_state::AppState;

#[tauri::command]
pub async fn library_query(
    state: State<'_, AppState>,
    input: LibraryQuery,
) -> Result<LibraryPage, AtlasError> {
    state.library.query(input).await
}
