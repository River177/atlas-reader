use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use atlas_domain::{
    AtlasError, AtlasErrorCode, ConnectionTestCode, ConnectionTestResult, MineruSettingsInput,
    ProviderKind, TranslationSettingsInput,
};
use atlas_provider_settings::{
    ConnectionProbe, DefaultProviderSettings, EnvironmentSecretOverride, InMemorySecretStore,
    ProbeRequest, ProviderConfigurationSource, ProviderProfile, ProviderSettingsModule,
    ProviderSettingsStore, ScriptedConnectionProbe, Secret, SecretStore, secret_account,
};
use tokio::sync::Notify;

#[derive(Debug, Default)]
struct InMemoryProviderSettingsStore {
    profiles: Mutex<Vec<(ProviderProfile, u64)>>,
    reject_writes: Mutex<bool>,
}

struct PausingProviderSettingsStore {
    profile: Mutex<Option<ProviderProfile>>,
    pause_next_save: AtomicBool,
    save_started: Notify,
    release_save: Notify,
}

impl PausingProviderSettingsStore {
    fn new(profile: ProviderProfile) -> Self {
        Self {
            profile: Mutex::new(Some(profile)),
            pause_next_save: AtomicBool::new(true),
            save_started: Notify::new(),
            release_save: Notify::new(),
        }
    }

    fn empty() -> Self {
        Self {
            profile: Mutex::new(None),
            pause_next_save: AtomicBool::new(true),
            save_started: Notify::new(),
            release_save: Notify::new(),
        }
    }
}

#[async_trait]
impl ProviderSettingsStore for PausingProviderSettingsStore {
    async fn load_profiles(&self) -> Result<Vec<ProviderProfile>, AtlasError> {
        Ok(self
            .profile
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .into_iter()
            .collect())
    }

    async fn save_profile(
        &self,
        profile: &ProviderProfile,
        _saved_at: u64,
    ) -> Result<(), AtlasError> {
        if self.pause_next_save.swap(false, Ordering::SeqCst) {
            self.save_started.notify_one();
            self.release_save.notified().await;
        }
        *self.profile.lock().unwrap_or_else(PoisonError::into_inner) = Some(profile.clone());
        Ok(())
    }
}

impl InMemoryProviderSettingsStore {
    fn saved_at(&self, kind: ProviderKind) -> Option<u64> {
        self.profiles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(profile, _)| profile.kind == kind)
            .map(|(_, saved_at)| *saved_at)
    }

    fn reject_writes(&self, reject: bool) {
        *self
            .reject_writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = reject;
    }
}

#[async_trait]
impl ProviderSettingsStore for InMemoryProviderSettingsStore {
    async fn load_profiles(&self) -> Result<Vec<ProviderProfile>, AtlasError> {
        Ok(self
            .profiles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(profile, _)| profile.clone())
            .collect())
    }

    async fn save_profile(
        &self,
        profile: &ProviderProfile,
        saved_at: u64,
    ) -> Result<(), AtlasError> {
        if *self
            .reject_writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            return Err(AtlasError::internal("the database is unavailable"));
        }
        let mut profiles = self.profiles.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = profiles
            .iter_mut()
            .find(|(stored, _)| stored.kind == profile.kind)
        {
            *existing = (profile.clone(), saved_at);
        } else {
            profiles.push((profile.clone(), saved_at));
        }
        Ok(())
    }
}

struct Harness {
    module: DefaultProviderSettings,
    store: Arc<InMemoryProviderSettingsStore>,
    secrets: Arc<InMemorySecretStore>,
    probe: Arc<ScriptedConnectionProbe>,
}

impl Harness {
    fn with_results(results: impl IntoIterator<Item = ConnectionTestResult>) -> Self {
        let store = Arc::new(InMemoryProviderSettingsStore::default());
        let secrets = Arc::new(InMemorySecretStore::new());
        let probe = Arc::new(ScriptedConnectionProbe::new(results));
        let module = DefaultProviderSettings::new(
            Arc::clone(&store) as Arc<dyn ProviderSettingsStore>,
            Arc::clone(&secrets) as Arc<dyn SecretStore>,
            Arc::clone(&probe) as Arc<dyn ConnectionProbe>,
        );
        Self {
            module,
            store,
            secrets,
            probe,
        }
    }

    fn new() -> Self {
        Self::with_results([])
    }

    /// Composes the module the way the desktop application does, with a
    /// development override shadowing reads of the durable store.
    fn with_environment_override() -> Self {
        let store = Arc::new(InMemoryProviderSettingsStore::default());
        let secrets = Arc::new(InMemorySecretStore::new());
        let probe = Arc::new(ScriptedConnectionProbe::new([]));
        let shadowed = Arc::new(EnvironmentSecretOverride::with_lookup(
            Arc::clone(&secrets),
            shell_exported_key,
        ));
        let module = DefaultProviderSettings::new(
            Arc::clone(&store) as Arc<dyn ProviderSettingsStore>,
            shadowed as Arc<dyn SecretStore>,
            Arc::clone(&probe) as Arc<dyn ConnectionProbe>,
        );
        Self {
            module,
            store,
            secrets,
            probe,
        }
    }
}

/// Stands in for a credential the developer exported in their shell.
fn shell_exported_key(_name: &str) -> Option<String> {
    Some("shell-exported-key".to_owned())
}

fn mineru_input(endpoint: &str, api_key: Option<&str>, automatic: bool) -> MineruSettingsInput {
    MineruSettingsInput {
        endpoint: endpoint.to_owned(),
        api_key: api_key.map(str::to_owned),
        automatic_cloud_parsing_enabled: automatic,
    }
}

fn translation_input(base_url: &str, api_key: Option<&str>) -> TranslationSettingsInput {
    TranslationSettingsInput {
        base_url: base_url.to_owned(),
        api_key: api_key.map(str::to_owned),
        model_id: "gpt-4o-mini".to_owned(),
        context_window_override: Some(128_000),
    }
}

#[tokio::test]
async fn settings_start_empty() {
    let harness = Harness::new();

    let settings = harness.module.get().await.expect("get should succeed");

    assert_eq!(settings.mineru_endpoint, None);
    assert!(!settings.mineru_has_secret);
    assert!(!settings.mineru_automatic_cloud_parsing_enabled);
    assert_eq!(settings.translation_base_url, None);
    assert_eq!(settings.translation_model_id, None);
    assert!(!settings.translation_has_secret);
    assert_eq!(settings.context_window_override, None);
}

#[tokio::test]
async fn saving_mineru_stores_the_profile_and_reports_the_connection_test() {
    let harness = Harness::with_results([ConnectionTestResult::passed("Cloud MinerU reachable")]);

    let result = harness
        .module
        .save_mineru(mineru_input(
            "  MinerU.example.com/api/v4/?token=x  ",
            Some(" key-1 "),
            true,
        ))
        .await
        .expect("save should succeed");

    assert!(result.ok);
    assert_eq!(result.code, ConnectionTestCode::Ok);

    let settings = harness.module.get().await.expect("get should succeed");
    assert_eq!(
        settings.mineru_endpoint.as_deref(),
        Some("https://mineru.example.com/api/v4")
    );
    assert!(settings.mineru_has_secret);
    assert!(settings.mineru_automatic_cloud_parsing_enabled);

    let requests = harness.probe.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, ProviderKind::Mineru);
    assert_eq!(
        requests[0].endpoint.url(),
        "https://mineru.example.com/api/v4"
    );
    assert_eq!(
        requests[0].api_key.as_ref().map(Secret::expose),
        Some("key-1")
    );
}

#[tokio::test]
async fn saving_translation_stores_model_and_context_window() {
    let harness = Harness::with_results([ConnectionTestResult::passed("Model endpoint reachable")]);

    let result = harness
        .module
        .save_translation(translation_input(
            "https://models.example.com/v1/",
            Some("key-2"),
        ))
        .await
        .expect("save should succeed");

    assert!(result.ok);

    let settings = harness.module.get().await.expect("get should succeed");
    assert_eq!(
        settings.translation_base_url.as_deref(),
        Some("https://models.example.com/v1")
    );
    assert_eq!(
        settings.translation_model_id.as_deref(),
        Some("gpt-4o-mini")
    );
    assert!(settings.translation_has_secret);
    assert_eq!(settings.context_window_override, Some(128_000));
    assert!(!settings.mineru_has_secret);
}

#[tokio::test]
async fn an_omitted_api_key_keeps_the_stored_credential() {
    let harness = Harness::new();

    harness
        .module
        .save_mineru(mineru_input(
            "https://mineru.example.com/api/v4",
            Some("key-1"),
            false,
        ))
        .await
        .expect("first save should succeed");
    harness
        .module
        .save_mineru(mineru_input(
            "https://mineru.example.com/api/v5",
            None,
            true,
        ))
        .await
        .expect("second save should succeed");

    let settings = harness.module.get().await.expect("get should succeed");
    assert_eq!(
        settings.mineru_endpoint.as_deref(),
        Some("https://mineru.example.com/api/v5")
    );
    assert!(settings.mineru_has_secret);
    assert_eq!(
        stored_key(&harness, ProviderKind::Mineru),
        Some("key-1".to_owned())
    );
}

#[tokio::test]
async fn an_explicitly_empty_api_key_is_rejected() {
    let harness = Harness::new();

    let error = harness
        .module
        .save_mineru(mineru_input(
            "https://mineru.example.com/api/v4",
            Some("   "),
            false,
        ))
        .await
        .expect_err("an empty key should be rejected");

    assert_eq!(error.code, AtlasErrorCode::InvalidInput);
    assert!(harness.probe.recorded_requests().is_empty());
}

#[tokio::test]
async fn rejected_urls_are_reported_without_being_stored() {
    let harness = Harness::new();

    let invalid = harness
        .module
        .save_mineru(mineru_input(
            "ftp://mineru.example.com",
            Some("key-1"),
            false,
        ))
        .await
        .expect("an invalid URL is a test failure, not an error");
    let insecure = harness
        .module
        .save_translation(translation_input(
            "http://models.example.com/v1",
            Some("key-2"),
        ))
        .await
        .expect("an insecure URL is a test failure, not an error");

    assert_eq!(invalid.code, ConnectionTestCode::InvalidUrl);
    assert!(!invalid.ok);
    assert_eq!(insecure.code, ConnectionTestCode::InsecureRemoteUrl);
    assert!(!insecure.ok);

    let settings = harness.module.get().await.expect("get should succeed");
    assert_eq!(settings.mineru_endpoint, None);
    assert_eq!(settings.translation_base_url, None);
    assert!(!settings.mineru_has_secret);
    assert!(!settings.translation_has_secret);
    assert!(harness.probe.recorded_requests().is_empty());
}

#[tokio::test]
async fn automatic_cloud_parsing_requires_a_stored_key() {
    let harness = Harness::new();

    let error = harness
        .module
        .save_mineru(mineru_input(
            "https://mineru.example.com/api/v4",
            None,
            true,
        ))
        .await
        .expect_err("enabling automatic parsing without a key should be rejected");

    assert_eq!(error.code, AtlasErrorCode::InvalidInput);
    let settings = harness.module.get().await.expect("get should succeed");
    assert_eq!(settings.mineru_endpoint, None);
}

#[tokio::test]
async fn deleting_the_mineru_key_disables_automatic_cloud_parsing() {
    let harness = Harness::new();

    harness
        .module
        .save_mineru(mineru_input(
            "https://mineru.example.com/api/v4",
            Some("key-1"),
            true,
        ))
        .await
        .expect("save should succeed");
    let saved_at = harness
        .store
        .saved_at(ProviderKind::Mineru)
        .expect("profile should be stored");

    harness
        .module
        .delete_secret(ProviderKind::Mineru)
        .await
        .expect("delete should succeed");

    let settings = harness.module.get().await.expect("get should succeed");
    assert!(!settings.mineru_has_secret);
    assert!(!settings.mineru_automatic_cloud_parsing_enabled);
    assert_eq!(
        settings.mineru_endpoint.as_deref(),
        Some("https://mineru.example.com/api/v4"),
        "the endpoint survives so the user only re-enters the key"
    );
    assert!(
        harness
            .store
            .saved_at(ProviderKind::Mineru)
            .expect("profile should still exist")
            >= saved_at
    );
}

#[tokio::test]
async fn deleting_the_translation_key_keeps_the_rest_of_the_profile() {
    let harness = Harness::new();

    harness
        .module
        .save_translation(translation_input(
            "https://models.example.com/v1",
            Some("key-2"),
        ))
        .await
        .expect("save should succeed");

    harness
        .module
        .delete_secret(ProviderKind::Translation)
        .await
        .expect("delete should succeed");

    let settings = harness.module.get().await.expect("get should succeed");
    assert!(!settings.translation_has_secret);
    assert_eq!(
        settings.translation_model_id.as_deref(),
        Some("gpt-4o-mini")
    );
    assert_eq!(settings.context_window_override, Some(128_000));
}

#[tokio::test]
async fn testing_an_unconfigured_provider_reports_not_configured() {
    let harness = Harness::new();

    let result = harness
        .module
        .test(ProviderKind::Translation)
        .await
        .expect("test should succeed");

    assert!(!result.ok);
    assert_eq!(result.code, ConnectionTestCode::NotConfigured);
    assert!(result.message.contains("Translation model"));
    assert!(harness.probe.recorded_requests().is_empty());
}

#[tokio::test]
async fn testing_a_saved_provider_reuses_the_stored_endpoint_and_key() {
    let harness = Harness::with_results([
        ConnectionTestResult::passed("Cloud MinerU reachable"),
        ConnectionTestResult::failed(ConnectionTestCode::Unauthorized, "The API key was rejected"),
    ]);

    harness
        .module
        .save_mineru(mineru_input(
            "https://mineru.example.com/api/v4",
            Some("key-1"),
            true,
        ))
        .await
        .expect("save should succeed");

    let result = harness
        .module
        .test(ProviderKind::Mineru)
        .await
        .expect("test should succeed");

    assert!(!result.ok);
    assert_eq!(result.code, ConnectionTestCode::Unauthorized);

    let requests = harness.probe.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].endpoint.url(), requests[0].endpoint.url());
    assert_eq!(
        requests[1].endpoint.fingerprint(),
        requests[0].endpoint.fingerprint(),
        "restoring a stored endpoint must reproduce its fingerprint"
    );
    assert_eq!(
        requests[1].api_key.as_ref().map(Secret::expose),
        Some("key-1")
    );
}

#[tokio::test]
async fn an_invalid_model_identifier_or_context_window_is_rejected() {
    let harness = Harness::new();

    let missing_model = harness
        .module
        .save_translation(TranslationSettingsInput {
            model_id: "   ".to_owned(),
            ..translation_input("https://models.example.com/v1", Some("key-2"))
        })
        .await
        .expect_err("an empty model identifier should be rejected");
    let tiny_window = harness
        .module
        .save_translation(TranslationSettingsInput {
            context_window_override: Some(16),
            ..translation_input("https://models.example.com/v1", Some("key-2"))
        })
        .await
        .expect_err("an implausible context window should be rejected");

    assert_eq!(missing_model.code, AtlasErrorCode::InvalidInput);
    assert_eq!(tiny_window.code, AtlasErrorCode::InvalidInput);
    let settings = harness.module.get().await.expect("get should succeed");
    assert_eq!(settings.translation_base_url, None);
}

#[tokio::test]
async fn providers_keep_separate_credentials() {
    let harness = Harness::new();

    harness
        .module
        .save_mineru(mineru_input(
            "https://mineru.example.com/api/v4",
            Some("key-mineru"),
            false,
        ))
        .await
        .expect("save should succeed");
    harness
        .module
        .save_translation(translation_input(
            "https://models.example.com/v1",
            Some("key-translation"),
        ))
        .await
        .expect("save should succeed");

    harness
        .module
        .delete_secret(ProviderKind::Mineru)
        .await
        .expect("delete should succeed");

    let settings = harness.module.get().await.expect("get should succeed");
    assert!(!settings.mineru_has_secret);
    assert!(settings.translation_has_secret);
    assert_eq!(
        stored_key(&harness, ProviderKind::Translation),
        Some("key-translation".to_owned())
    );
}

#[tokio::test]
async fn probe_requests_never_render_the_api_key() {
    let harness = Harness::new();

    harness
        .module
        .save_mineru(mineru_input(
            "https://mineru.example.com/api/v4",
            Some("super-secret-value"),
            false,
        ))
        .await
        .expect("save should succeed");

    let rendered = format!("{:?}", harness.probe.recorded_requests());

    assert!(!rendered.contains("super-secret-value"));
    assert!(rendered.contains("<redacted>"));
}

#[tokio::test]
async fn probe_requests_carry_no_key_when_none_is_stored() {
    let harness = Harness::with_results([ConnectionTestResult::failed(
        ConnectionTestCode::Unauthorized,
        "No credential was supplied",
    )]);

    let result = harness
        .module
        .save_mineru(mineru_input(
            "https://mineru.example.com/api/v4",
            None,
            false,
        ))
        .await
        .expect("save should succeed");

    assert_eq!(result.code, ConnectionTestCode::Unauthorized);
    let requests: Vec<ProbeRequest> = harness.probe.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].api_key.is_none());
}

#[tokio::test]
async fn a_rejected_profile_write_restores_the_previous_credential() {
    let harness = Harness::with_results([ConnectionTestResult::passed("connected")]);
    harness
        .module
        .save_mineru(mineru_input(
            "https://mineru.example.com/api/v4",
            Some("key-1"),
            false,
        ))
        .await
        .expect("first save should succeed");

    harness.store.reject_writes(true);
    let error = harness
        .module
        .save_mineru(mineru_input(
            "https://other.example.com/api/v4",
            Some("key-2"),
            false,
        ))
        .await
        .expect_err("the rejected write should surface");

    assert_eq!(error.code, AtlasErrorCode::Internal);
    // The endpoint never moved, so the key paired with it must not move either.
    let settings = harness.module.get().await.expect("get should succeed");
    assert_eq!(
        settings.mineru_endpoint.as_deref(),
        Some("https://mineru.example.com/api/v4")
    );
    assert_eq!(
        stored_key(&harness, ProviderKind::Mineru),
        Some("key-1".to_owned())
    );
}

#[tokio::test]
async fn a_rejected_first_profile_write_leaves_no_orphan_credential() {
    let harness = Harness::new();
    harness.store.reject_writes(true);

    harness
        .module
        .save_translation(translation_input(
            "https://models.example.com/v1",
            Some("key-1"),
        ))
        .await
        .expect_err("the rejected write should surface");

    assert_eq!(stored_key(&harness, ProviderKind::Translation), None);
    let settings = harness.module.get().await.expect("get should succeed");
    assert!(!settings.translation_has_secret);
    assert!(settings.translation_base_url.is_none());
}

#[tokio::test]
async fn a_rejected_switch_write_keeps_the_credential_deletable() {
    let harness = Harness::with_results([ConnectionTestResult::passed("connected")]);
    harness
        .module
        .save_mineru(mineru_input(
            "https://mineru.example.com/api/v4",
            Some("key-1"),
            true,
        ))
        .await
        .expect("save should succeed");

    harness.store.reject_writes(true);
    harness
        .module
        .delete_secret(ProviderKind::Mineru)
        .await
        .expect_err("the rejected write should surface");

    // Automatic parsing is still on, so the key it depends on must still be there.
    let settings = harness.module.get().await.expect("get should succeed");
    assert!(settings.mineru_automatic_cloud_parsing_enabled);
    assert!(settings.mineru_has_secret);

    harness.store.reject_writes(false);
    harness
        .module
        .delete_secret(ProviderKind::Mineru)
        .await
        .expect("delete should succeed once the store recovers");
    let settings = harness.module.get().await.expect("get should succeed");
    assert!(!settings.mineru_automatic_cloud_parsing_enabled);
    assert!(!settings.mineru_has_secret);
}

fn stored_key(harness: &Harness, kind: ProviderKind) -> Option<String> {
    let account = harness
        .store
        .profiles
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(profile, _)| profile.kind == kind)
        .map(|(profile, _)| profile.secret_account.clone())
        .unwrap_or_else(|| secret_account(kind));
    harness
        .secrets
        .get(&account)
        .expect("secret lookup should succeed")
        .map(|secret| secret.expose().to_owned())
}

/// A rollback must restore what was durably stored, never a value that only
/// shadowed it. Restoring the shadow would write the developer's shell-exported
/// credential into the keychain, which no save was ever asked to do.
#[tokio::test]
async fn a_rejected_write_never_persists_an_environment_override() {
    let harness = Harness::with_environment_override();
    harness.store.reject_writes(true);

    harness
        .module
        .save_translation(translation_input(
            "https://models.example.com/v1",
            Some("typed-key"),
        ))
        .await
        .expect_err("the rejected write should surface");

    assert_eq!(
        stored_key(&harness, ProviderKind::Translation),
        None,
        "the shadowed value must not become a stored credential"
    );
}

/// The same rollback must not overwrite a real stored credential with the
/// shadowing one.
#[tokio::test]
async fn a_rejected_write_restores_the_stored_key_not_the_override() {
    let harness = Harness::with_environment_override();
    harness
        .secrets
        .set(
            &secret_account(ProviderKind::Translation),
            &Secret::new("keychain-key"),
        )
        .expect("seeding the keychain should succeed");
    harness.store.reject_writes(true);

    harness
        .module
        .save_translation(translation_input(
            "https://models.example.com/v1",
            Some("typed-key"),
        ))
        .await
        .expect_err("the rejected write should surface");

    assert_eq!(
        stored_key(&harness, ProviderKind::Translation),
        Some("keychain-key".to_owned()),
        "the rollback must restore the durable credential"
    );
}

#[tokio::test]
async fn runtime_resolution_never_pairs_a_new_key_with_the_previous_endpoint() {
    let account = secret_account(ProviderKind::Translation);
    let store = Arc::new(PausingProviderSettingsStore::new(ProviderProfile {
        kind: ProviderKind::Translation,
        endpoint_origin: "https://old.example.com".to_owned(),
        base_path: "/v1".to_owned(),
        endpoint_fingerprint: "old-fingerprint".to_owned(),
        model_id: Some("old-model".to_owned()),
        context_window_override: None,
        automatic_cloud_parsing_enabled: false,
        secret_account: account.clone(),
    }));
    let secrets = Arc::new(InMemorySecretStore::new());
    secrets
        .set(&account, &Secret::new("old-key"))
        .expect("old key should seed");
    let module = Arc::new(DefaultProviderSettings::new(
        store.clone() as Arc<dyn ProviderSettingsStore>,
        secrets as Arc<dyn SecretStore>,
        Arc::new(ScriptedConnectionProbe::new([])),
    ));
    let saving = {
        let module = module.clone();
        tokio::spawn(async move {
            module
                .save_translation(TranslationSettingsInput {
                    base_url: "https://new.example.com/v1".to_owned(),
                    api_key: Some("new-key".to_owned()),
                    model_id: "new-model".to_owned(),
                    context_window_override: None,
                })
                .await
        })
    };
    store.save_started.notified().await;
    let resolving = {
        let module = module.clone();
        tokio::spawn(async move {
            module
                .resolve(ProviderKind::Translation)
                .await
                .expect("runtime resolution should succeed")
                .expect("translation should remain configured")
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(
        !resolving.is_finished(),
        "runtime reads must wait while the key and endpoint transition"
    );

    store.release_save.notify_one();
    saving
        .await
        .expect("save task should join")
        .expect("save should succeed");
    let resolved = resolving.await.expect("resolve task should join");

    assert_eq!(resolved.profile.endpoint_origin, "https://new.example.com");
    assert_eq!(
        resolved.secret.as_ref().map(Secret::expose),
        Some("new-key")
    );
}

#[tokio::test]
async fn cancelling_a_save_keeps_the_previous_endpoint_and_key_paired() {
    let account = secret_account(ProviderKind::Translation);
    let store = Arc::new(PausingProviderSettingsStore::new(ProviderProfile {
        kind: ProviderKind::Translation,
        endpoint_origin: "https://old.example.com".to_owned(),
        base_path: "/v1".to_owned(),
        endpoint_fingerprint: "old-fingerprint".to_owned(),
        model_id: Some("old-model".to_owned()),
        context_window_override: None,
        automatic_cloud_parsing_enabled: false,
        secret_account: account.clone(),
    }));
    let secrets = Arc::new(InMemorySecretStore::new());
    secrets
        .set(&account, &Secret::new("old-key"))
        .expect("old key should seed");
    let module = Arc::new(DefaultProviderSettings::new(
        store.clone() as Arc<dyn ProviderSettingsStore>,
        secrets as Arc<dyn SecretStore>,
        Arc::new(ScriptedConnectionProbe::new([])),
    ));
    let saving = {
        let module = module.clone();
        tokio::spawn(async move {
            module
                .save_translation(TranslationSettingsInput {
                    base_url: "https://new.example.com/v1".to_owned(),
                    api_key: Some("new-key".to_owned()),
                    model_id: "new-model".to_owned(),
                    context_window_override: None,
                })
                .await
        })
    };
    store.save_started.notified().await;
    saving.abort();
    assert!(
        saving
            .await
            .expect_err("save should be cancelled")
            .is_cancelled()
    );

    let resolved = module
        .resolve(ProviderKind::Translation)
        .await
        .expect("runtime resolution should succeed")
        .expect("old translation profile should remain");
    assert_eq!(resolved.profile.endpoint_origin, "https://old.example.com");
    assert_eq!(
        resolved.secret.as_ref().map(Secret::expose),
        Some("old-key")
    );
}

#[tokio::test]
async fn cancelling_the_first_save_cannot_attach_its_key_to_a_later_endpoint() {
    let store = Arc::new(PausingProviderSettingsStore::empty());
    let secrets = Arc::new(InMemorySecretStore::new());
    let module = Arc::new(DefaultProviderSettings::new(
        store.clone() as Arc<dyn ProviderSettingsStore>,
        secrets.clone() as Arc<dyn SecretStore>,
        Arc::new(ScriptedConnectionProbe::new([])),
    ));
    let saving = {
        let module = module.clone();
        tokio::spawn(async move {
            module
                .save_translation(TranslationSettingsInput {
                    base_url: "https://abandoned.example.com/v1".to_owned(),
                    api_key: Some("orphaned-key".to_owned()),
                    model_id: "model-a".to_owned(),
                    context_window_override: None,
                })
                .await
        })
    };
    store.save_started.notified().await;
    saving.abort();
    assert!(
        saving
            .await
            .expect_err("save should be cancelled")
            .is_cancelled()
    );
    assert!(
        secrets
            .get(&secret_account(ProviderKind::Translation))
            .expect("stable account should be readable")
            .is_none(),
        "a cancelled first save must not populate the fallback account"
    );

    module
        .save_translation(TranslationSettingsInput {
            base_url: "https://later.example.com/v1".to_owned(),
            api_key: None,
            model_id: "model-b".to_owned(),
            context_window_override: None,
        })
        .await
        .expect("a later keyless profile should save");
    let resolved = module
        .resolve(ProviderKind::Translation)
        .await
        .expect("runtime resolution should succeed")
        .expect("later profile should exist");

    assert_eq!(
        resolved.profile.endpoint_origin,
        "https://later.example.com"
    );
    assert!(resolved.secret.is_none());
}
