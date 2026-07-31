use std::sync::Arc;

use async_trait::async_trait;
use atlas_domain::{AtlasError, ProviderKind, ProviderState, ProviderStatusSnapshot};
use atlas_parse::{CloudCredential, CloudParseConfiguration, CloudParseConfigurationPort};
use atlas_provider_settings::ProviderConfigurationSource;
use atlas_reading_session::ProviderStatusPort;
use atlas_translation::{
    TranslationConfiguration, TranslationConfigurationPort, TranslationCredential,
};

/// Runtime view of provider settings. The settings module owns validation and
/// mutation; this adapter resolves its durable profile + effective credential
/// into exactly the two read models needed by parsing and session status.
#[derive(Clone)]
pub struct ProviderRuntimeAdapter {
    source: Arc<dyn ProviderConfigurationSource>,
}

impl std::fmt::Debug for ProviderRuntimeAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderRuntimeAdapter")
            .finish_non_exhaustive()
    }
}

impl ProviderRuntimeAdapter {
    #[must_use]
    pub fn new(source: Arc<dyn ProviderConfigurationSource>) -> Self {
        Self { source }
    }

    fn endpoint(origin: &str, base_path: &str) -> String {
        format!(
            "{}{}",
            origin.trim_end_matches('/'),
            if base_path.is_empty() {
                String::new()
            } else {
                format!("/{}", base_path.trim_matches('/'))
            }
        )
    }
}

#[async_trait]
impl CloudParseConfigurationPort for ProviderRuntimeAdapter {
    async fn load(&self) -> Result<Option<CloudParseConfiguration>, AtlasError> {
        let Some(resolved) = self.source.resolve(ProviderKind::Mineru).await? else {
            return Ok(None);
        };
        let Some(secret) = resolved.secret else {
            return Ok(None);
        };
        let profile = resolved.profile;
        Ok(Some(CloudParseConfiguration {
            profile_id: ProviderKind::Mineru.as_str().to_owned(),
            endpoint_base_url: Self::endpoint(&profile.endpoint_origin, &profile.base_path),
            endpoint_fingerprint: profile.endpoint_fingerprint,
            credential: CloudCredential::new(secret.expose()),
            automatic: profile.automatic_cloud_parsing_enabled,
        }))
    }
}

#[async_trait]
impl TranslationConfigurationPort for ProviderRuntimeAdapter {
    async fn load(&self) -> Result<Option<TranslationConfiguration>, AtlasError> {
        let Some(resolved) = self.source.resolve(ProviderKind::Translation).await? else {
            return Ok(None);
        };
        let profile = resolved.profile;
        let Some(model_id) = profile.model_id.filter(|value| !value.trim().is_empty()) else {
            return Ok(None);
        };
        let credential = resolved
            .secret
            .map(|secret| TranslationCredential::new(secret.expose()));
        Ok(Some(TranslationConfiguration {
            profile_id: ProviderKind::Translation.as_str().to_owned(),
            endpoint_base_url: Self::endpoint(&profile.endpoint_origin, &profile.base_path),
            endpoint_fingerprint: profile.endpoint_fingerprint,
            model_id,
            context_window: profile.context_window_override.unwrap_or(32_768),
            credential,
        }))
    }
}

#[async_trait]
impl ProviderStatusPort for ProviderRuntimeAdapter {
    async fn snapshot(&self) -> ProviderStatusSnapshot {
        let mineru = self.source.resolve(ProviderKind::Mineru).await;
        let translation = self.source.resolve(ProviderKind::Translation).await;
        let mineru_state = match mineru {
            Ok(Some(resolved)) if resolved.secret.is_some() => ProviderState::Ready,
            Ok(Some(_) | None) => ProviderState::NotConfigured,
            Err(_) => ProviderState::Unreachable,
        };
        // An OpenAI-compatible loopback endpoint is intentionally allowed to be
        // keyless, so the profile itself is enough to call it configured.
        let translation_state = match &translation {
            Ok(Some(_)) => ProviderState::Ready,
            Ok(None) => ProviderState::NotConfigured,
            Err(_) => ProviderState::Unreachable,
        };
        ProviderStatusSnapshot {
            mineru: mineru_state,
            translation: translation_state,
            translation_model: translation
                .ok()
                .flatten()
                .and_then(|resolved| resolved.profile.model_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_provider_settings::ResolvedProviderConfiguration;

    struct UnavailableSource;

    #[async_trait]
    impl ProviderConfigurationSource for UnavailableSource {
        async fn resolve(
            &self,
            _kind: ProviderKind,
        ) -> Result<Option<ResolvedProviderConfiguration>, AtlasError> {
            Err(AtlasError::storage("secret store unavailable"))
        }
    }

    #[tokio::test]
    async fn a_secret_store_failure_is_reported_by_configuration_and_status() {
        let adapter = ProviderRuntimeAdapter::new(Arc::new(UnavailableSource));

        assert!(CloudParseConfigurationPort::load(&adapter).await.is_err());
        assert_eq!(adapter.snapshot().await.mineru, ProviderState::Unreachable);
        assert_eq!(
            adapter.snapshot().await.translation,
            ProviderState::Unreachable
        );
    }
}
