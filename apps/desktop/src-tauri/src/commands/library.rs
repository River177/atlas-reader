use atlas_contracts::{
    AtlasError, DocumentId, DocumentSummary, ImportPdfResult, LibraryPage, LibraryQuery,
    RefreshSourcesResult,
};
use serde::Deserialize;
use tauri::State;

use crate::app_state::AppState;

#[tauri::command]
pub async fn library_query(
    state: State<'_, AppState>,
    input: LibraryQuery,
) -> Result<LibraryPage, AtlasError> {
    state.library.query(input).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentInput {
    pub document_id: DocumentId,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelocateDocumentInput {
    pub document_id: DocumentId,
    pub new_path: String,
}

#[tauri::command]
pub async fn library_import_pdf(
    state: State<'_, AppState>,
    path: String,
) -> Result<ImportPdfResult, AtlasError> {
    state.library.import_pdf(path).await
}

#[tauri::command]
pub async fn library_refresh_sources(
    state: State<'_, AppState>,
) -> Result<RefreshSourcesResult, AtlasError> {
    state.library.refresh_sources().await
}

#[tauri::command]
pub async fn library_relocate(
    state: State<'_, AppState>,
    input: RelocateDocumentInput,
) -> Result<DocumentSummary, AtlasError> {
    state
        .library
        .relocate(input.document_id, input.new_path)
        .await
}

#[tauri::command]
pub async fn library_remove(
    state: State<'_, AppState>,
    input: DocumentInput,
) -> Result<(), AtlasError> {
    state.library.remove(input.document_id).await
}
