use async_trait::async_trait;
use atlas_domain::{AtlasError, ProviderKind};

/// A saved provider configuration. Secrets live in the secret store, so this
/// record only carries the account name that points at them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderProfile {
    pub kind: ProviderKind,
    pub endpoint_origin: String,
    pub base_path: String,
    pub endpoint_fingerprint: String,
    pub model_id: Option<String>,
    pub context_window_override: Option<u32>,
    pub automatic_cloud_parsing_enabled: bool,
    pub secret_account: String,
}

#[async_trait]
pub trait ProviderSettingsStore: Send + Sync {
    async fn load_profiles(&self) -> Result<Vec<ProviderProfile>, AtlasError>;

    async fn save_profile(
        &self,
        profile: &ProviderProfile,
        saved_at: u64,
    ) -> Result<(), AtlasError>;
}
