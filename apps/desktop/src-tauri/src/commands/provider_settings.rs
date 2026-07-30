use atlas_contracts::{
    AtlasError, ConnectionTestResult, MineruSettingsInput, ProviderKind, PublicProviderSettings,
    TranslationSettingsInput,
};
use serde::Deserialize;
use tauri::State;

use crate::app_state::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderKindInput {
    pub provider: ProviderKind,
}

#[tauri::command]
pub async fn provider_settings_get(
    state: State<'_, AppState>,
) -> Result<PublicProviderSettings, AtlasError> {
    state.provider_settings.get().await
}

#[tauri::command]
pub async fn provider_settings_save_mineru(
    state: State<'_, AppState>,
    input: MineruSettingsInput,
) -> Result<ConnectionTestResult, AtlasError> {
    state.provider_settings.save_mineru(input).await
}

#[tauri::command]
pub async fn provider_settings_save_translation(
    state: State<'_, AppState>,
    input: TranslationSettingsInput,
) -> Result<ConnectionTestResult, AtlasError> {
    state.provider_settings.save_translation(input).await
}

#[tauri::command]
pub async fn provider_settings_test(
    state: State<'_, AppState>,
    input: ProviderKindInput,
) -> Result<ConnectionTestResult, AtlasError> {
    state.provider_settings.test(input.provider).await
}

#[tauri::command]
pub async fn provider_settings_delete_secret(
    state: State<'_, AppState>,
    input: ProviderKindInput,
) -> Result<(), AtlasError> {
    state.provider_settings.delete_secret(input.provider).await
}
