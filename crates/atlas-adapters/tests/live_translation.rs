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

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use atlas_adapters::{HttpConnectionProbe, OpenAiCompatibleTranslationAdapter};
use atlas_domain::{
    AtlasError, BlockId, BlockKind, CanonicalBlock, ConnectionTestCode, ProviderKind,
    StructuredContent,
};
use atlas_provider_settings::{ConnectionProbe, ProbeRequest, Secret, normalize};
use atlas_translation::{
    TranslationChunkSink, TranslationConfiguration, TranslationCredential, TranslationOutputParser,
    TranslationPlanner, TranslationProviderPort, validate_output,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

mod support;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:4141/v1";

fn enabled() -> bool {
    std::env::var("ATLAS_LIVE_TRANSLATION").as_deref() == Ok("1")
}

fn base_url() -> String {
    std::env::var("ATLAS_LIVE_TRANSLATION_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned())
}

async fn stored_key() -> Option<Secret> {
    support::effective_provider_secret(ProviderKind::Translation)
        .await
        .expect("the configured credential should be readable")
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

#[derive(Default)]
struct ParserSink(Mutex<TranslationOutputParser>);

#[async_trait]
impl TranslationChunkSink for ParserSink {
    async fn push(&self, content: &str) -> Result<(), AtlasError> {
        self.0.lock().await.push(content)
    }
}

#[tokio::test]
async fn the_stored_key_is_accepted_by_the_model_endpoint() {
    if !enabled() {
        eprintln!("skipping: set ATLAS_LIVE_TRANSLATION=1 to run");
        return;
    }
    let Some(api_key) = stored_key().await else {
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

#[tokio::test]
async fn a_synthetic_block_survives_the_live_translation_protocol() {
    if !enabled() {
        eprintln!("skipping: set ATLAS_LIVE_TRANSLATION=1 to run");
        return;
    }
    let Some(api_key) = stored_key().await else {
        panic!("no translation key in the keychain; save one in Atlas settings first");
    };
    let configuration = TranslationConfiguration {
        profile_id: "openai_compatible".to_owned(),
        endpoint_base_url: base_url(),
        endpoint_fingerprint: "live-contract-test".to_owned(),
        model_id: std::env::var("ATLAS_LIVE_TRANSLATION_MODEL")
            .unwrap_or_else(|_| "gemini-3.5-flash".to_owned()),
        context_window: 32_768,
        credential: Some(TranslationCredential::new(api_key.expose())),
    };
    let block = CanonicalBlock {
        id: BlockId::from("synthetic-block-1"),
        order_index: 0,
        kind: BlockKind::Paragraph,
        page_start: 1,
        page_end: 1,
        bounding_boxes: Vec::new(),
        content: StructuredContent::text(
            "A retrieval model compares a query with a synthetic document.",
        ),
        source_digest: "synthetic-source-digest".to_owned(),
    };
    let planner = TranslationPlanner::new();
    let prepared = planner
        .prepare(&block, &configuration)
        .expect("synthetic block should prepare");
    let mut plan = planner
        .plan_batches(vec![prepared], &configuration)
        .expect("synthetic request should plan");
    assert!(plan.rejected.is_empty(), "synthetic request should fit");
    let batch = plan.batches.remove(0);
    let sink = Arc::new(ParserSink::default());

    let completion = OpenAiCompatibleTranslationAdapter::new()
        .expect("adapter should build")
        .stream(
            &configuration,
            batch.request,
            sink.clone(),
            CancellationToken::new(),
        )
        .await
        .expect("live translation should complete");
    let parser = std::mem::take(&mut *sink.0.lock().await);
    let records = parser.finish().expect("live output should be structured");
    let validation = validate_output(&batch.blocks, records, &completion);

    assert!(
        validation.failed.is_empty(),
        "live output failed validation: {:?}",
        validation.failed
    );
    assert_eq!(validation.accepted.len(), 1);
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
    let Some(api_key) = stored_key().await else {
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
