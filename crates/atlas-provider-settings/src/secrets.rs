use std::{
    collections::HashMap,
    fmt,
    sync::{Mutex, PoisonError},
};

use atlas_domain::{AtlasError, ProviderKind};

/// A credential that never reaches logs, panics, or error messages.
#[derive(Clone, Eq, PartialEq)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(<redacted>)")
    }
}

/// Keychain account for a provider. Stable across upgrades and installations.
#[must_use]
pub fn secret_account(kind: ProviderKind) -> String {
    format!("atlas.{}", kind.as_str())
}

pub trait SecretStore: Send + Sync {
    fn set(&self, account: &str, secret: &Secret) -> Result<(), AtlasError>;
    fn get(&self, account: &str) -> Result<Option<Secret>, AtlasError>;
    fn delete(&self, account: &str) -> Result<(), AtlasError>;
}

/// Test adapter. Keeps credentials in process memory only.
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    secrets: Mutex<HashMap<String, String>>,
}

impl InMemorySecretStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.secrets.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SecretStore for InMemorySecretStore {
    fn set(&self, account: &str, secret: &Secret) -> Result<(), AtlasError> {
        self.lock()
            .insert(account.to_owned(), secret.expose().to_owned());
        Ok(())
    }

    fn get(&self, account: &str) -> Result<Option<Secret>, AtlasError> {
        Ok(self.lock().get(account).map(Secret::new))
    }

    fn delete(&self, account: &str) -> Result<(), AtlasError> {
        self.lock().remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_hides_the_credential() {
        let secret = Secret::new("super-secret-value");

        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert_eq!(secret.expose(), "super-secret-value");
    }

    #[test]
    fn accounts_are_distinct_and_stable_per_provider() {
        assert_eq!(secret_account(ProviderKind::Mineru), "atlas.cloud_mineru");
        assert_eq!(
            secret_account(ProviderKind::Translation),
            "atlas.openai_compatible"
        );
    }

    #[test]
    fn in_memory_store_round_trips_and_deletes() {
        let store = InMemorySecretStore::new();
        let account = secret_account(ProviderKind::Mineru);

        assert!(store.get(&account).expect("get should succeed").is_none());
        store
            .set(&account, &Secret::new("key-1"))
            .expect("set should succeed");
        assert_eq!(
            store
                .get(&account)
                .expect("get should succeed")
                .map(|secret| secret.expose().to_owned()),
            Some("key-1".to_owned())
        );
        store.delete(&account).expect("delete should succeed");
        assert!(store.get(&account).expect("get should succeed").is_none());
    }
}
