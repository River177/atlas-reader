use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, PoisonError},
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

    /// The credential to use for `account`, which a decorator may substitute.
    fn get(&self, account: &str) -> Result<Option<Secret>, AtlasError>;

    fn delete(&self, account: &str) -> Result<(), AtlasError>;

    /// The credential durably held for `account`, ignoring any read-time
    /// substitution.
    ///
    /// Rollback needs this rather than [`SecretStore::get`]. Restoring what
    /// `get` returned would write a substituted value into durable storage,
    /// turning a read-only override into a permanent write. Stores that never
    /// substitute answer both questions identically.
    fn stored(&self, account: &str) -> Result<Option<Secret>, AtlasError> {
        self.get(account)
    }
}

/// Lets a shared store be used wherever an owned one is expected, so a caller
/// can keep a handle to the store it wraps.
impl<S: SecretStore + ?Sized> SecretStore for Arc<S> {
    fn set(&self, account: &str, secret: &Secret) -> Result<(), AtlasError> {
        (**self).set(account, secret)
    }

    fn get(&self, account: &str) -> Result<Option<Secret>, AtlasError> {
        (**self).get(account)
    }

    fn delete(&self, account: &str) -> Result<(), AtlasError> {
        (**self).delete(account)
    }

    fn stored(&self, account: &str) -> Result<Option<Secret>, AtlasError> {
        (**self).stored(account)
    }
}

/// Test adapter. Keeps credentials in process memory only.
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    secrets: Mutex<HashMap<String, Secret>>,
}

impl InMemorySecretStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Secret>> {
        self.secrets.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SecretStore for InMemorySecretStore {
    fn set(&self, account: &str, secret: &Secret) -> Result<(), AtlasError> {
        self.lock().insert(account.to_owned(), secret.clone());
        Ok(())
    }

    fn get(&self, account: &str) -> Result<Option<Secret>, AtlasError> {
        Ok(self.lock().get(account).cloned())
    }

    fn delete(&self, account: &str) -> Result<(), AtlasError> {
        self.lock().remove(account);
        Ok(())
    }
}

/// Environment variable that shadows the stored entry for `account`.
///
/// `atlas.cloud_mineru` becomes `ATLAS_CLOUD_MINERU`. The `atlas.` prefix is
/// required and the remainder must be lowercase, which keeps the mapping
/// injective and confines it to Atlas's own namespace: no account can address
/// `PATH`, `HOME`, or any other variable Atlas does not own, and no two
/// accounts can name the same variable.
#[must_use]
pub fn secret_env_var(account: &str) -> Option<String> {
    let suffix = account.strip_prefix("atlas.")?;
    let usable = !suffix.is_empty()
        && suffix.starts_with(|character: char| character.is_ascii_lowercase())
        && suffix.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        });
    usable.then(|| format!("ATLAS_{}", suffix.to_ascii_uppercase()))
}

/// Where an override value comes from. Injected so tests never have to mutate
/// the process environment, which no test can do safely while others run.
type EnvLookup = fn(&str) -> Option<String>;

fn read_environment(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn no_environment(_name: &str) -> Option<String> {
    None
}

/// The lookup a build uses. Release builds must not consult the environment at
/// all, so a shipped Atlas can only read credentials from the keychain.
fn build_lookup() -> EnvLookup {
    if cfg!(debug_assertions) {
        read_environment
    } else {
        no_environment
    }
}

/// Lets an environment variable stand in for a stored credential.
///
/// Development builds are ad-hoc signed, and the linker embeds a fresh build
/// hash in the signing identity on every rebuild. macOS binds keychain access
/// control to that identity, so each `cargo build` produces what the keychain
/// sees as a brand new application and prompts for authorization again.
/// Choosing "Always Allow" cannot help, because the binary it authorizes is
/// replaced by the next build. Reading the credential from the environment
/// avoids touching the keychain at all, which is why development and tests stay
/// prompt-free without any credential entering the repository.
///
/// Release builds ignore the environment completely. A signed Atlas reads
/// credentials only from the keychain, so no environment variable can introduce
/// one, and a signed application keeps a stable identity across launches and
/// upgrades — the prompt is a development artifact, not something users meet.
///
/// The override shadows reads only. `set` and `delete` always reach the inner
/// store, so a value written while an override is active does not change what
/// `get` returns until the variable is unset.
#[derive(Clone, Debug)]
pub struct EnvironmentSecretOverride<S> {
    inner: S,
    lookup: EnvLookup,
}

impl<S: SecretStore> EnvironmentSecretOverride<S> {
    /// Consults the environment in debug builds and ignores it in release
    /// builds.
    #[must_use]
    pub fn new(inner: S) -> Self {
        Self::with_lookup(inner, build_lookup())
    }

    #[must_use]
    pub fn with_lookup(inner: S, lookup: EnvLookup) -> Self {
        Self { inner, lookup }
    }

    fn override_for(&self, account: &str) -> Option<Secret> {
        let value = (self.lookup)(&secret_env_var(account)?)?;
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| Secret::new(trimmed))
    }
}

impl<S: SecretStore> SecretStore for EnvironmentSecretOverride<S> {
    fn set(&self, account: &str, secret: &Secret) -> Result<(), AtlasError> {
        self.inner.set(account, secret)
    }

    fn get(&self, account: &str) -> Result<Option<Secret>, AtlasError> {
        match self.override_for(account) {
            Some(secret) => Ok(Some(secret)),
            None => self.inner.get(account),
        }
    }

    fn delete(&self, account: &str) -> Result<(), AtlasError> {
        self.inner.delete(account)
    }

    fn stored(&self, account: &str) -> Result<Option<Secret>, AtlasError> {
        self.inner.stored(account)
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

    #[test]
    fn provider_accounts_map_to_readable_environment_variables() {
        assert_eq!(
            secret_env_var(&secret_account(ProviderKind::Mineru)).as_deref(),
            Some("ATLAS_CLOUD_MINERU")
        );
        assert_eq!(
            secret_env_var(&secret_account(ProviderKind::Translation)).as_deref(),
            Some("ATLAS_OPENAI_COMPATIBLE")
        );
    }

    /// An account that cannot produce a well-formed variable name must not be
    /// looked up at all, rather than being coerced into some nearby name.
    /// The mapping must stay inside Atlas's own namespace. An account that
    /// could name `PATH` would turn that variable's contents into a bearer
    /// token sent to the configured provider.
    #[test]
    fn no_account_can_name_a_variable_atlas_does_not_own() {
        for account in [
            "path",
            "home",
            "user",
            "shell",
            "PATH",
            "atlas",
            "atlasx.key",
            ".atlas.key",
            "",
        ] {
            assert_eq!(secret_env_var(account), None, "account {account:?}");
        }
        assert!(
            secret_env_var("atlas.anything")
                .expect("an atlas account should map")
                .starts_with("ATLAS_")
        );
    }

    /// Two accounts that named the same variable would let one provider's
    /// override silently answer for another.
    #[test]
    fn distinct_accounts_never_collide_on_one_variable() {
        for account in [
            "atlas.Cloud_Mineru",
            "atlas.CLOUD_MINERU",
            "atlas.cloud.mineru",
            "atlas.cloud-mineru",
            "atlas._cloud_mineru",
            "atlas.9cloud",
        ] {
            assert_eq!(secret_env_var(account), None, "account {account:?}");
        }
        assert_eq!(
            secret_env_var("atlas.cloud_mineru").as_deref(),
            Some("ATLAS_CLOUD_MINERU")
        );
    }

    /// Inverting the release gate must not leave the suite green.
    #[test]
    fn only_debug_builds_consult_the_environment() {
        let observed = build_lookup()("PATH").is_some();

        assert_eq!(
            observed,
            cfg!(debug_assertions),
            "a release build must ignore the environment entirely"
        );
    }

    #[test]
    fn the_default_constructor_uses_the_build_lookup() {
        let store = EnvironmentSecretOverride::new(InMemorySecretStore::new());

        assert!(
            std::ptr::fn_addr_eq(store.lookup, build_lookup()),
            "new() must route through the release gate"
        );
    }

    /// The in-memory store is used throughout the test suite, so its Debug
    /// output must redact just as the keychain adapter's does.
    #[test]
    fn the_in_memory_store_never_prints_a_credential() {
        let store = InMemorySecretStore::new();
        store
            .set("atlas.cloud_mineru", &Secret::new("super-secret-value"))
            .expect("set should succeed");

        let rendered = format!("{store:?}");

        assert!(!rendered.contains("super-secret-value"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    /// `stored` must report durable state so a rollback cannot write an
    /// override back into the keychain.
    #[test]
    fn stored_reports_the_inner_value_even_while_shadowed() {
        let store = EnvironmentSecretOverride::with_lookup(stored("atlas.shadowed"), env_key);

        assert_eq!(
            read(&store, "atlas.shadowed"),
            Some("env-key".to_owned()),
            "reads are shadowed"
        );
        assert_eq!(
            store
                .stored("atlas.shadowed")
                .expect("stored should succeed")
                .map(|secret| secret.expose().to_owned()),
            Some("stored-key".to_owned()),
            "durable state is not shadowed"
        );
    }

    fn env_key(name: &str) -> Option<String> {
        (name == "ATLAS_SHADOWED").then(|| "env-key".to_owned())
    }

    fn blank_env_key(name: &str) -> Option<String> {
        (name == "ATLAS_SHADOWED").then(|| "   ".to_owned())
    }

    fn stored(account: &str) -> InMemorySecretStore {
        let store = InMemorySecretStore::new();
        store
            .set(account, &Secret::new("stored-key"))
            .expect("set should succeed");
        store
    }

    fn read(store: &impl SecretStore, account: &str) -> Option<String> {
        store
            .get(account)
            .expect("get should succeed")
            .map(|secret| secret.expose().to_owned())
    }

    #[test]
    fn an_environment_value_shadows_the_stored_credential() {
        let store = EnvironmentSecretOverride::with_lookup(stored("atlas.shadowed"), env_key);

        assert_eq!(read(&store, "atlas.shadowed"), Some("env-key".to_owned()));
    }

    /// The release configuration must reach the keychain even when a matching
    /// variable is present in the environment.
    #[test]
    fn a_release_build_ignores_the_environment() {
        let store =
            EnvironmentSecretOverride::with_lookup(stored("atlas.shadowed"), no_environment);

        assert_eq!(
            read(&store, "atlas.shadowed"),
            Some("stored-key".to_owned())
        );
    }

    /// A variable that is present but blank means "not configured", so it must
    /// not shadow a real credential with an empty one.
    #[test]
    fn a_blank_environment_value_falls_through_to_the_store() {
        let store = EnvironmentSecretOverride::with_lookup(stored("atlas.shadowed"), blank_env_key);

        assert_eq!(
            read(&store, "atlas.shadowed"),
            Some("stored-key".to_owned())
        );
    }

    #[test]
    fn an_unmatched_account_falls_through_to_the_store() {
        let store = EnvironmentSecretOverride::with_lookup(stored("atlas.other"), env_key);

        assert_eq!(read(&store, "atlas.other"), Some("stored-key".to_owned()));
    }

    #[test]
    fn writes_and_deletes_always_reach_the_inner_store() {
        let inner = Arc::new(InMemorySecretStore::new());
        let store = EnvironmentSecretOverride::with_lookup(inner.clone(), env_key);

        store
            .set("atlas.shadowed", &Secret::new("stored-key"))
            .expect("set should succeed");
        assert_eq!(
            read(&inner, "atlas.shadowed"),
            Some("stored-key".to_owned()),
            "the write must land in the keychain, not be swallowed by the override"
        );

        store
            .delete("atlas.shadowed")
            .expect("delete should succeed");
        assert_eq!(
            read(&inner, "atlas.shadowed"),
            None,
            "the delete must land in the keychain"
        );
        assert_eq!(
            read(&store, "atlas.shadowed"),
            Some("env-key".to_owned()),
            "the override still shadows reads until the variable is unset"
        );
    }
}
