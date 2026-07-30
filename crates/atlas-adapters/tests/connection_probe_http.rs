use std::time::Duration;

use atlas_adapters::HttpConnectionProbe;
use atlas_domain::{ConnectionTestCode, ProviderKind};
use atlas_provider_settings::{ConnectionProbe, ProbeRequest, Secret, normalize};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, header_regex, method, path},
};

fn request(kind: ProviderKind, base_url: &str, api_key: Option<&str>) -> ProbeRequest {
    ProbeRequest {
        kind,
        endpoint: normalize(kind, base_url).expect("endpoint should normalize"),
        api_key: api_key.map(Secret::new),
    }
}

fn probe() -> HttpConnectionProbe {
    HttpConnectionProbe::with_timeout(Duration::from_secs(5)).expect("probe should build")
}

#[tokio::test]
async fn a_mineru_endpoint_is_probed_with_a_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/api/v4/extract/task/00000000-0000-4000-8000-000000000000",
        ))
        .and(header("authorization", "Bearer mineru-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"code":0,"data":{"state":"done"},"msg":"ok"}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let result = probe()
        .probe(request(
            ProviderKind::Mineru,
            &format!("{}/api/v4", server.uri()),
            Some("mineru-key"),
        ))
        .await;

    assert!(result.ok, "{}", result.message);
    assert_eq!(result.code, ConnectionTestCode::Ok);
}

#[tokio::test]
async fn a_translation_endpoint_is_probed_through_the_model_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer model-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"object":"list","data":[{"id":"gpt-4o-mini"}]}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let result = probe()
        .probe(request(
            ProviderKind::Translation,
            &format!("{}/v1", server.uri()),
            Some("model-key"),
        ))
        .await;

    assert!(result.ok, "{}", result.message);
    assert_eq!(result.code, ConnectionTestCode::Ok);
}

#[tokio::test]
async fn a_rejected_key_is_reported_as_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"invalid key"}"#))
        .mount(&server)
        .await;

    let result = probe()
        .probe(request(
            ProviderKind::Translation,
            &format!("{}/v1", server.uri()),
            Some("wrong-key"),
        ))
        .await;

    assert!(!result.ok);
    assert_eq!(result.code, ConnectionTestCode::Unauthorized);
}

#[tokio::test]
async fn a_slow_endpoint_is_reported_as_a_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(5))
                .set_body_string(r#"{"code":0}"#),
        )
        .mount(&server)
        .await;

    let result = HttpConnectionProbe::with_timeout(Duration::from_millis(250))
        .expect("probe should build")
        .probe(request(
            ProviderKind::Mineru,
            &format!("{}/api/v4", server.uri()),
            Some("mineru-key"),
        ))
        .await;

    assert!(!result.ok);
    assert_eq!(result.code, ConnectionTestCode::Timeout);
}

#[tokio::test]
async fn a_refused_connection_is_reported_as_unreachable() {
    // Port 1 is privileged and never bound by the test suite, so this exercises
    // a refused TCP connection without racing another mock server for a port.
    let result = probe()
        .probe(request(
            ProviderKind::Mineru,
            "http://127.0.0.1:1/api/v4",
            Some("mineru-key"),
        ))
        .await;

    assert!(!result.ok);
    assert_eq!(result.code, ConnectionTestCode::Unreachable);
}

#[tokio::test]
async fn an_unresolvable_host_is_reported_as_a_dns_failure() {
    let result = probe()
        .probe(request(
            ProviderKind::Mineru,
            "https://atlas-reader-endpoint-that-does-not-exist.invalid/api/v4",
            Some("mineru-key"),
        ))
        .await;

    assert!(!result.ok);
    assert_eq!(result.code, ConnectionTestCode::DnsFailed);
}

#[tokio::test]
async fn a_missing_key_short_circuits_before_any_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"code":0}"#))
        .expect(0)
        .mount(&server)
        .await;

    let result = probe()
        .probe(request(
            ProviderKind::Mineru,
            &format!("{}/api/v4", server.uri()),
            None,
        ))
        .await;

    assert!(!result.ok);
    assert_eq!(result.code, ConnectionTestCode::Unauthorized);
}

#[tokio::test]
async fn a_keyless_local_model_server_is_probed_without_authorization() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header_regex("accept", "application/json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"object":"list","data":[{"id":"qwen2.5"}]}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let result = probe()
        .probe(request(
            ProviderKind::Translation,
            &format!("{}/v1", server.uri()),
            None,
        ))
        .await;

    assert!(result.ok, "{}", result.message);
    assert_eq!(result.code, ConnectionTestCode::Ok);
    let sent = &server.received_requests().await.expect("requests recorded")[0];
    assert!(sent.headers.get("authorization").is_none());
}

#[tokio::test]
async fn a_cross_host_redirect_is_not_followed_with_the_key() {
    let destination = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"code":0,"msg":"ok"}"#))
        .expect(0)
        .mount(&destination)
        .await;

    let origin = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", destination.uri().as_str()),
        )
        .expect(1)
        .mount(&origin)
        .await;

    let result = probe()
        .probe(request(
            ProviderKind::Mineru,
            &format!("{}/api/v4", origin.uri()),
            Some("mineru-key"),
        ))
        .await;

    assert!(!result.ok);
    assert_eq!(result.code, ConnectionTestCode::ProtocolIncompatible);
    assert!(
        destination
            .received_requests()
            .await
            .expect("requests recorded")
            .is_empty()
    );
}

#[tokio::test]
async fn a_same_origin_redirect_is_still_followed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/task"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"code":0,"msg":"ok","trace_id":"t"}"#),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/api/v4/extract/task/00000000-0000-4000-8000-000000000000",
        ))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/api/v4/task"))
        .expect(1)
        .mount(&server)
        .await;

    let result = probe()
        .probe(request(
            ProviderKind::Mineru,
            &format!("{}/api/v4", server.uri()),
            Some("mineru-key"),
        ))
        .await;

    assert!(result.ok, "{}", result.message);
    let sent = server.received_requests().await.expect("requests recorded");
    assert_eq!(sent.len(), 2);
    assert!(sent[1].headers.get("authorization").is_some());
}

#[tokio::test]
async fn an_oversized_body_is_refused_instead_of_buffered() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(2 * 1024 * 1024)))
        .mount(&server)
        .await;

    let result = probe()
        .probe(request(
            ProviderKind::Mineru,
            &format!("{}/api/v4", server.uri()),
            Some("mineru-key"),
        ))
        .await;

    assert!(!result.ok);
    assert_eq!(result.code, ConnectionTestCode::ProtocolIncompatible);
    assert!(
        result.message.contains("could not read"),
        "{}",
        result.message
    );
}
