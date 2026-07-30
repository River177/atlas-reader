use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use atlas_domain::{
    AtlasError, ConnectionTestCode, ConnectionTestResult, MineruSettingsInput, ProviderKind,
    PublicProviderSettings, TranslationSettingsInput,
};
use tokio::sync::Mutex;

use crate::{
    ConnectionProbe, NormalizedEndpoint, ProbeRequest, ProviderProfile, ProviderSettingsStore,
    Secret, SecretStore, normalize, secret_account,
};

const MIN_CONTEXT_WINDOW: u32 = 1_024;
const MAX_CONTEXT_WINDOW: u32 = 8_000_000;

#[async_trait]
pub trait ProviderSettingsModule: Send + Sync {
    /// Never exposes a stored credential, only whether one exists.
    async fn get(&self) -> Result<PublicProviderSettings, AtlasError>;

    async fn save_mineru(
        &self,
        input: MineruSettingsInput,
    ) -> Result<ConnectionTestResult, AtlasError>;

    async fn save_translation(
        &self,
        input: TranslationSettingsInput,
    ) -> Result<ConnectionTestResult, AtlasError>;

    async fn test(&self, kind: ProviderKind) -> Result<ConnectionTestResult, AtlasError>;

    async fn delete_secret(&self, kind: ProviderKind) -> Result<(), AtlasError>;
}

pub struct DefaultProviderSettings {
    store: Arc<dyn ProviderSettingsStore>,
    secrets: Arc<dyn SecretStore>,
    probe: Arc<dyn ConnectionProbe>,
    /// Serializes settings writes so a save and a delete cannot interleave and
    /// leave an endpoint pointing at a credential that no longer exists.
    transitions: Mutex<()>,
}

impl std::fmt::Debug for DefaultProviderSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DefaultProviderSettings")
            .finish_non_exhaustive()
    }
}

impl DefaultProviderSettings {
    #[must_use]
    pub fn new(
        store: Arc<dyn ProviderSettingsStore>,
        secrets: Arc<dyn SecretStore>,
        probe: Arc<dyn ConnectionProbe>,
    ) -> Self {
        Self {
            store,
            secrets,
            probe,
            transitions: Mutex::new(()),
        }
    }

    async fn profile(&self, kind: ProviderKind) -> Result<Option<ProviderProfile>, AtlasError> {
        Ok(self
            .store
            .load_profiles()
            .await?
            .into_iter()
            .find(|profile| profile.kind == kind))
    }

    fn stored_secret(&self, kind: ProviderKind) -> Result<Option<Secret>, AtlasError> {
        self.secrets.get(&secret_account(kind))
    }

    /// Moves the credential and the endpoint together. If the database refuses
    /// the profile the previous credential is put back, because an endpoint left
    /// paired with a newer key would send that key to the wrong provider.
    async fn commit(
        &self,
        profile: &ProviderProfile,
        supplied_key: Option<Secret>,
    ) -> Result<(), AtlasError> {
        let saved_at = now_ms()?;
        let replaced = match supplied_key {
            None => None,
            Some(key) => {
                // Snapshots durable storage, not the effective credential: an
                // override that only shadows reads must never be written back
                // into the keychain by a rollback.
                let previous = self.secrets.stored(&profile.secret_account)?;
                self.secrets.set(&profile.secret_account, &key)?;
                Some(previous)
            }
        };

        match self.store.save_profile(profile, saved_at).await {
            Ok(()) => Ok(()),
            Err(error) => {
                if let Some(previous) = replaced {
                    self.restore_secret(&profile.secret_account, previous.as_ref());
                }
                Err(error)
            }
        }
    }

    /// Best effort by design: a failed rollback must not replace the error that
    /// made the rollback necessary.
    fn restore_secret(&self, account: &str, previous: Option<&Secret>) {
        let _ = match previous {
            Some(secret) => self.secrets.set(account, secret),
            None => self.secrets.delete(account),
        };
    }

    async fn run_probe(
        &self,
        kind: ProviderKind,
        endpoint: NormalizedEndpoint,
    ) -> ConnectionTestResult {
        let api_key = match self.stored_secret(kind) {
            Ok(secret) => secret,
            Err(error) => {
                return ConnectionTestResult::failed(
                    ConnectionTestCode::Unreachable,
                    error.message,
                );
            }
        };
        self.probe
            .probe(ProbeRequest {
                kind,
                endpoint,
                api_key,
            })
            .await
    }
}

#[async_trait]
impl ProviderSettingsModule for DefaultProviderSettings {
    async fn get(&self) -> Result<PublicProviderSettings, AtlasError> {
        let profiles = self.store.load_profiles().await?;
        let mineru = profiles
            .iter()
            .find(|profile| profile.kind == ProviderKind::Mineru);
        let translation = profiles
            .iter()
            .find(|profile| profile.kind == ProviderKind::Translation);

        Ok(PublicProviderSettings {
            mineru_endpoint: mineru.map(endpoint_url),
            mineru_has_secret: self.stored_secret(ProviderKind::Mineru)?.is_some(),
            mineru_automatic_cloud_parsing_enabled: mineru
                .is_some_and(|profile| profile.automatic_cloud_parsing_enabled),
            translation_base_url: translation.map(endpoint_url),
            translation_model_id: translation.and_then(|profile| profile.model_id.clone()),
            translation_has_secret: self.stored_secret(ProviderKind::Translation)?.is_some(),
            context_window_override: translation
                .and_then(|profile| profile.context_window_override),
        })
    }

    async fn save_mineru(
        &self,
        input: MineruSettingsInput,
    ) -> Result<ConnectionTestResult, AtlasError> {
        let _transition = self.transitions.lock().await;
        let endpoint = match normalize(ProviderKind::Mineru, &input.endpoint) {
            Ok(endpoint) => endpoint,
            Err(error) => return Ok(error.into_test_result()),
        };
        let account = secret_account(ProviderKind::Mineru);
        let supplied_key = validated_key(input.api_key.as_deref())?;
        let has_secret = supplied_key.is_some() || self.secrets.get(&account)?.is_some();
        if input.automatic_cloud_parsing_enabled && !has_secret {
            return Err(AtlasError::invalid_input(
                "Add a Cloud MinerU API key before enabling automatic cloud parsing",
            ));
        }

        self.commit(
            &ProviderProfile {
                kind: ProviderKind::Mineru,
                endpoint_origin: endpoint.origin().to_owned(),
                base_path: endpoint.base_path().to_owned(),
                endpoint_fingerprint: endpoint.fingerprint().to_owned(),
                model_id: None,
                context_window_override: None,
                automatic_cloud_parsing_enabled: input.automatic_cloud_parsing_enabled,
                secret_account: account,
            },
            supplied_key,
        )
        .await?;

        Ok(self.run_probe(ProviderKind::Mineru, endpoint).await)
    }

    async fn save_translation(
        &self,
        input: TranslationSettingsInput,
    ) -> Result<ConnectionTestResult, AtlasError> {
        let _transition = self.transitions.lock().await;
        let model_id = input.model_id.trim();
        if model_id.is_empty() {
            return Err(AtlasError::invalid_input("Enter a model identifier"));
        }
        if let Some(window) = input.context_window_override
            && !(MIN_CONTEXT_WINDOW..=MAX_CONTEXT_WINDOW).contains(&window)
        {
            return Err(AtlasError::invalid_input(format!(
                "The context window override must be between {MIN_CONTEXT_WINDOW} and {MAX_CONTEXT_WINDOW} tokens"
            )));
        }
        let endpoint = match normalize(ProviderKind::Translation, &input.base_url) {
            Ok(endpoint) => endpoint,
            Err(error) => return Ok(error.into_test_result()),
        };
        let account = secret_account(ProviderKind::Translation);
        let supplied_key = validated_key(input.api_key.as_deref())?;

        self.commit(
            &ProviderProfile {
                kind: ProviderKind::Translation,
                endpoint_origin: endpoint.origin().to_owned(),
                base_path: endpoint.base_path().to_owned(),
                endpoint_fingerprint: endpoint.fingerprint().to_owned(),
                model_id: Some(model_id.to_owned()),
                context_window_override: input.context_window_override,
                automatic_cloud_parsing_enabled: false,
                secret_account: account,
            },
            supplied_key,
        )
        .await?;

        Ok(self.run_probe(ProviderKind::Translation, endpoint).await)
    }

    async fn test(&self, kind: ProviderKind) -> Result<ConnectionTestResult, AtlasError> {
        // Held for the whole probe so a concurrent save cannot swap the stored
        // credential after the endpoint has been read, which would send the new
        // key to the previous provider.
        let _transition = self.transitions.lock().await;
        let Some(profile) = self.profile(kind).await? else {
            return Ok(ConnectionTestResult::failed(
                ConnectionTestCode::NotConfigured,
                format!("{} is not configured yet", kind.display_name()),
            ));
        };
        let endpoint = NormalizedEndpoint::restore(
            kind,
            profile.endpoint_origin.clone(),
            profile.base_path.clone(),
        );
        Ok(self.run_probe(kind, endpoint).await)
    }

    async fn delete_secret(&self, kind: ProviderKind) -> Result<(), AtlasError> {
        let _transition = self.transitions.lock().await;
        // The switch goes off before the credential does. A stored key with
        // automatic parsing off is harmless, but the reverse order would leave
        // every later import trying to reach Cloud MinerU without a key.
        if kind == ProviderKind::Mineru
            && let Some(profile) = self.profile(kind).await?
            && profile.automatic_cloud_parsing_enabled
        {
            self.store
                .save_profile(
                    &ProviderProfile {
                        automatic_cloud_parsing_enabled: false,
                        ..profile
                    },
                    now_ms()?,
                )
                .await?;
        }
        self.secrets.delete(&secret_account(kind))
    }
}

fn endpoint_url(profile: &ProviderProfile) -> String {
    format!("{}{}", profile.endpoint_origin, profile.base_path)
}

fn validated_key(api_key: Option<&str>) -> Result<Option<Secret>, AtlasError> {
    match api_key {
        None => Ok(None),
        Some(key) if key.trim().is_empty() => Err(AtlasError::invalid_input(
            "The API key must not be empty; delete the stored key instead",
        )),
        Some(key) => Ok(Some(Secret::new(key.trim()))),
    }
}

fn now_ms() -> Result<u64, AtlasError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AtlasError::internal("system clock predates the Unix epoch"))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| AtlasError::internal("system clock is outside the supported range"))
}
