//! Contract test for `HttpConnectionProbe` against the real Cloud MinerU API.
//!
//! Skipped unless `ATLAS_LIVE_MINERU=1`, per the spec's live-provider boundary,
//! so ordinary test runs stay offline and free. The credential is read from the
//! same keychain entry the application uses, or from `ATLAS_CLOUD_MINERU` when
//! that is exported, so no key ever reaches the repository or the test output.
//! The environment override exists because development builds are ad-hoc signed
//! and the keychain re-prompts for every rebuild; see the README.
//!
//! Run with:
//!
//! ```text
//! ATLAS_LIVE_MINERU=1 cargo test -p atlas-adapters --test live_mineru -- --nocapture
//! ```
//!
//! A failure here means Cloud MinerU changed a response shape that Atlas
//! classifies, which is exactly the drift this test exists to catch.

use std::time::Duration;

use atlas_adapters::HttpConnectionProbe;
use atlas_domain::{ConnectionTestCode, ProviderKind};
use atlas_provider_settings::{ConnectionProbe, ProbeRequest, Secret, normalize};

mod support;

const BASE_URL: &str = "https://mineru.net/api/v4";

fn enabled() -> bool {
    std::env::var("ATLAS_LIVE_MINERU").as_deref() == Ok("1")
}

async fn stored_key() -> Option<Secret> {
    support::effective_provider_secret(ProviderKind::Mineru)
        .await
        .expect("the configured credential should be readable")
}

fn request(api_key: Option<Secret>) -> ProbeRequest {
    ProbeRequest {
        kind: ProviderKind::Mineru,
        endpoint: normalize(ProviderKind::Mineru, BASE_URL).expect("endpoint should normalize"),
        api_key,
    }
}

fn probe() -> HttpConnectionProbe {
    HttpConnectionProbe::with_timeout(Duration::from_secs(30)).expect("probe should build")
}

#[tokio::test]
async fn the_stored_key_is_accepted_by_cloud_mineru() {
    if !enabled() {
        eprintln!("skipping: set ATLAS_LIVE_MINERU=1 to run");
        return;
    }
    let Some(api_key) = stored_key().await else {
        panic!("no Cloud MinerU key in the keychain; save one in Atlas settings first");
    };

    let result = probe().probe(request(Some(api_key))).await;

    assert!(
        result.ok,
        "live probe failed with {:?}: {}",
        result.code, result.message
    );
    assert_eq!(result.code, ConnectionTestCode::Ok);
}

#[tokio::test]
async fn an_invalid_key_is_reported_as_unauthorized() {
    if !enabled() {
        eprintln!("skipping: set ATLAS_LIVE_MINERU=1 to run");
        return;
    }

    let result = probe()
        .probe(request(Some(Secret::new("sk-atlas-live-test-invalid-key"))))
        .await;

    assert!(!result.ok);
    assert_eq!(
        result.code,
        ConnectionTestCode::Unauthorized,
        "expected an authorization failure, got: {}",
        result.message
    );
}
