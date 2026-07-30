//! Contract test for `HttpConnectionProbe` against a real OpenAI-compatible API.
//!
//! Skipped unless `ATLAS_LIVE_TRANSLATION=1`, per the spec's live-provider
//! boundary, so ordinary test runs stay offline and free. The credential is read
//! from the same keychain entry the application uses, so no key ever reaches the
//! repo, the environment, or the test output.
//!
//! Set `ATLAS_LIVE_TRANSLATION_URL` to point at the endpoint under test. It
//! defaults to the loopback bridge used during Phase 0, which also exercises the
//! plain-HTTP-on-loopback exception in endpoint normalization.
//!
//! Run with:
//!
//! ```text
//! ATLAS_LIVE_TRANSLATION=1 cargo test -p atlas-adapters --test live_translation -- --nocapture
//! ```
//!
//! A failure here means the endpoint changed a response shape that Atlas
//! classifies, which is exactly the drift this test exists to catch.

use std::time::Duration;

use atlas_adapters::{HttpConnectionProbe, MacOsKeychainAdapter};
use atlas_domain::{ConnectionTestCode, ProviderKind};
use atlas_provider_settings::{
    ConnectionProbe, EnvironmentSecretOverride, ProbeRequest, Secret, SecretStore, normalize,
    secret_account,
};

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:4141/v1";

fn enabled() -> bool {
    std::env::var("ATLAS_LIVE_TRANSLATION").as_deref() == Ok("1")
}

fn base_url() -> String {
    std::env::var("ATLAS_LIVE_TRANSLATION_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned())
}

fn stored_key() -> Option<Secret> {
    EnvironmentSecretOverride::new(MacOsKeychainAdapter::new())
        .get(&secret_account(ProviderKind::Translation))
        .expect("the keychain should be readable")
}

fn request(api_key: Option<Secret>) -> ProbeRequest {
    ProbeRequest {
        kind: ProviderKind::Translation,
        endpoint: normalize(ProviderKind::Translation, &base_url())
            .expect("endpoint should normalize"),
        api_key,
    }
}

fn probe() -> HttpConnectionProbe {
    HttpConnectionProbe::with_timeout(Duration::from_secs(30)).expect("probe should build")
}

#[tokio::test]
async fn the_stored_key_is_accepted_by_the_model_endpoint() {
    if !enabled() {
        eprintln!("skipping: set ATLAS_LIVE_TRANSLATION=1 to run");
        return;
    }
    let Some(api_key) = stored_key() else {
        panic!("no translation key in the keychain; save one in Atlas settings first");
    };

    let result = probe().probe(request(Some(api_key))).await;

    assert!(
        result.ok,
        "live probe failed with {:?}: {}",
        result.code, result.message
    );
    assert_eq!(result.code, ConnectionTestCode::Ok);
}

/// The endpoint answers 401 for a bad bearer token, which is the branch the
/// settings screen turns into "check your API key" rather than "check the URL".
#[tokio::test]
async fn an_invalid_key_is_reported_as_unauthorized() {
    if !enabled() {
        eprintln!("skipping: set ATLAS_LIVE_TRANSLATION=1 to run");
        return;
    }

    let result = probe()
        .probe(request(Some(Secret::new("atlas-live-test-invalid-key"))))
        .await;

    assert!(!result.ok);
    assert_eq!(
        result.code,
        ConnectionTestCode::Unauthorized,
        "expected an authorization failure, got: {}",
        result.message
    );
}

/// A reachable host that is not an OpenAI-compatible API must be reported as a
/// protocol mismatch, not as a bad key. The bridge's unauthenticated root serves
/// exactly that: a live server whose `/models` path does not exist.
#[tokio::test]
async fn a_non_openai_endpoint_is_reported_as_incompatible() {
    if !enabled() {
        eprintln!("skipping: set ATLAS_LIVE_TRANSLATION=1 to run");
        return;
    }
    let Some(api_key) = stored_key() else {
        panic!("no translation key in the keychain; save one in Atlas settings first");
    };

    let endpoint = normalize(
        ProviderKind::Translation,
        "http://127.0.0.1:4141/not-an-api",
    )
    .expect("endpoint should normalize");
    let result = probe()
        .probe(ProbeRequest {
            kind: ProviderKind::Translation,
            endpoint,
            api_key: Some(api_key),
        })
        .await;

    assert!(!result.ok);
    assert_eq!(
        result.code,
        ConnectionTestCode::ProtocolIncompatible,
        "expected a protocol mismatch, got: {}",
        result.message
    );
}
