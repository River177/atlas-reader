use async_trait::async_trait;
use atlas_domain::{ProviderState, ProviderStatusSnapshot};
use atlas_reading_session::ProviderStatusPort;

#[derive(Clone, Debug, Default)]
pub struct UnconfiguredProviderStatusAdapter;

#[async_trait]
impl ProviderStatusPort for UnconfiguredProviderStatusAdapter {
    async fn snapshot(&self) -> ProviderStatusSnapshot {
        ProviderStatusSnapshot {
            mineru: ProviderState::NotConfigured,
            translation: ProviderState::NotConfigured,
            translation_model: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StaticProviderStatusAdapter {
    snapshot: ProviderStatusSnapshot,
}

impl StaticProviderStatusAdapter {
    #[must_use]
    pub fn new(snapshot: ProviderStatusSnapshot) -> Self {
        Self { snapshot }
    }
}

#[async_trait]
impl ProviderStatusPort for StaticProviderStatusAdapter {
    async fn snapshot(&self) -> ProviderStatusSnapshot {
        self.snapshot.clone()
    }
}
