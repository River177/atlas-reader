use std::time::Duration;

use async_trait::async_trait;
use atlas_domain::{AtlasError, ConnectionTestCode, ConnectionTestResult, ProviderKind};
use atlas_provider_settings::{ConnectionProbe, ProbeRequest};
use reqwest::{
    Client, Response, StatusCode, Url,
    redirect::{self, Policy},
};
use serde_json::Value;

/// A syntactically valid identifier that no MinerU account owns. It lets the
/// probe exercise an authenticated read without creating a parse task.
const MINERU_PROBE_TASK_ID: &str = "00000000-0000-4000-8000-000000000000";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REDIRECTS: usize = 3;
/// Far more than any provider status body needs. Caps what a hostile or broken
/// endpoint can make Atlas buffer.
const MAX_BODY_BYTES: usize = 1 << 20;

/// Checks a provider endpoint over HTTPS and reports one Atlas outcome.
///
/// Provider-specific knowledge — which path to call and how to read a body that
/// reports failure with HTTP 200 — stays inside this adapter.
#[derive(Clone, Debug)]
pub struct HttpConnectionProbe {
    client: Client,
}

impl HttpConnectionProbe {
    pub fn new() -> Result<Self, AtlasError> {
        Self::with_timeout(REQUEST_TIMEOUT)
    }

    pub fn with_timeout(timeout: Duration) -> Result<Self, AtlasError> {
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT.min(timeout))
            .timeout(timeout)
            .redirect(same_origin_redirects(MAX_REDIRECTS))
            .user_agent(concat!("AtlasReader/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                AtlasError::internal(format!("The network client could not start: {error}"))
            })?;
        Ok(Self { client })
    }

    fn probe_url(request: &ProbeRequest) -> String {
        match request.kind {
            ProviderKind::Mineru => request
                .endpoint
                .join(&format!("extract/task/{MINERU_PROBE_TASK_ID}")),
            ProviderKind::Translation => request.endpoint.join("models"),
        }
    }
}

/// Follows a redirect only back to the same scheme, host, and port.
///
/// The probe carries a bearer token. Reqwest strips that header when the host or
/// port changes, but not when only the scheme changes, so an `https` endpoint
/// redirecting to `http` on the same host would put the key on the wire in the
/// clear. A cross-origin hop would also escape the loopback-only rule that
/// endpoint normalization enforces, so neither is followed.
fn same_origin_redirects(max: usize) -> Policy {
    Policy::custom(move |attempt: redirect::Attempt| {
        if follows_redirect(attempt.url(), attempt.previous(), max) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

fn follows_redirect(target: &Url, previous: &[Url], max: usize) -> bool {
    if previous.len() > max {
        return false;
    }
    let Some(source) = previous.last() else {
        return false;
    };
    target.scheme() == source.scheme()
        && target.host_str() == source.host_str()
        && target.port_or_known_default() == source.port_or_known_default()
}

#[async_trait]
impl ConnectionProbe for HttpConnectionProbe {
    async fn probe(&self, request: ProbeRequest) -> ConnectionTestResult {
        // A local OpenAI-compatible server usually wants no credential at all,
        // so only Cloud MinerU is answered before a request goes out.
        if request.api_key.is_none() && request.kind == ProviderKind::Mineru {
            return ConnectionTestResult::failed(
                ConnectionTestCode::Unauthorized,
                format!(
                    "Add a {} API key before testing the connection",
                    request.kind.display_name()
                ),
            );
        }

        let mut builder = self
            .client
            .get(Self::probe_url(&request))
            .header("Accept", "application/json");
        if let Some(api_key) = request.api_key.as_ref() {
            builder = builder.bearer_auth(api_key.expose());
        }

        match builder.send().await {
            Err(error) => classify_transport(&error),
            Ok(response) => {
                let status = response.status();
                match read_capped_body(response).await {
                    Some(body) => classify_response(request.kind, status, &body),
                    None => ConnectionTestResult::failed(
                        ConnectionTestCode::ProtocolIncompatible,
                        format!(
                            "{} answered {status} with a response Atlas could not read",
                            request.kind.display_name()
                        ),
                    ),
                }
            }
        }
    }
}

/// Returns `None` when the body exceeds the cap or ends early: either way there
/// is nothing Atlas can classify.
async fn read_capped_body(mut response: Response) -> Option<String> {
    let mut body: Vec<u8> = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > MAX_BODY_BYTES {
                    return None;
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Some(String::from_utf8_lossy(&body).into_owned()),
            Err(_) => return None,
        }
    }
}

fn classify_response(kind: ProviderKind, status: StatusCode, body: &str) -> ConnectionTestResult {
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return ConnectionTestResult::failed(
            ConnectionTestCode::Unauthorized,
            format!("{} rejected the API key", kind.display_name()),
        );
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return ConnectionTestResult::failed(
            ConnectionTestCode::RateLimited,
            format!(
                "{} is rate limiting this key; try again later",
                kind.display_name()
            ),
        );
    }
    if status.is_server_error() {
        return ConnectionTestResult::failed(
            ConnectionTestCode::ServerError,
            format!("{} returned {status}", kind.display_name()),
        );
    }

    let Ok(payload) = serde_json::from_str::<Value>(body) else {
        return ConnectionTestResult::failed(
            ConnectionTestCode::ProtocolIncompatible,
            format!(
                "{} answered {status} with a response Atlas does not understand",
                kind.display_name()
            ),
        );
    };

    match kind {
        // MinerU reports failure inside the body, so HTTP 200 is not enough.
        ProviderKind::Mineru => {
            if mineru_rejected_the_key(&payload) {
                ConnectionTestResult::failed(
                    ConnectionTestCode::Unauthorized,
                    "Cloud MinerU rejected the API key",
                )
            } else if mineru_envelope(&payload) {
                ConnectionTestResult::passed("Cloud MinerU accepted the API key")
            } else {
                ConnectionTestResult::failed(
                    ConnectionTestCode::ProtocolIncompatible,
                    format!("The endpoint answered {status} but is not a Cloud MinerU API"),
                )
            }
        }
        ProviderKind::Translation => {
            if status.is_success() && payload.get("data").is_some() {
                ConnectionTestResult::passed("The model endpoint listed its models")
            } else {
                ConnectionTestResult::failed(
                    ConnectionTestCode::ProtocolIncompatible,
                    format!(
                        "The endpoint answered {status} and does not expose an OpenAI-compatible model list"
                    ),
                )
            }
        }
    }
}

/// Every MinerU answer — success or failure — carries a `code` alongside a `msg`
/// or a `trace_id`. Requiring the pair keeps an unrelated service that happens to
/// return `{"code": ...}` from being reported as a working Cloud MinerU.
fn mineru_envelope(payload: &Value) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    object.contains_key("code")
        && (object.contains_key("msg")
            || object.contains_key("trace_id")
            || object.contains_key("data"))
}

/// MinerU authorization failures use the `A02xx` code family.
fn mineru_rejected_the_key(payload: &Value) -> bool {
    let code_matches = payload
        .get("code")
        .and_then(Value::as_str)
        .is_some_and(|code| code.starts_with("A02"));
    let message_matches = payload
        .get("msg")
        .and_then(Value::as_str)
        .is_some_and(|message| {
            let lowered = message.to_ascii_lowercase();
            lowered.contains("token") || lowered.contains("unauthorized")
        });
    code_matches || message_matches
}

fn classify_transport(error: &reqwest::Error) -> ConnectionTestResult {
    if error.is_timeout() {
        return ConnectionTestResult::failed(
            ConnectionTestCode::Timeout,
            "The endpoint did not answer in time",
        );
    }

    let detail = error_chain(error);
    if detail.contains("dns")
        || detail.contains("failed to lookup address")
        || detail.contains("name or service not known")
        || detail.contains("nodename nor servname")
    {
        return ConnectionTestResult::failed(
            ConnectionTestCode::DnsFailed,
            "The endpoint host name could not be resolved",
        );
    }
    if detail.contains("certificate")
        || detail.contains("tls")
        || detail.contains("ssl")
        || detail.contains("handshake")
    {
        return ConnectionTestResult::failed(
            ConnectionTestCode::TlsFailed,
            "The endpoint presented a certificate Atlas could not verify",
        );
    }

    ConnectionTestResult::failed(
        ConnectionTestCode::Unreachable,
        "Atlas could not reach the endpoint",
    )
}

fn error_chain(error: &reqwest::Error) -> String {
    let mut detail = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        detail.push(' ');
        detail.push_str(&cause.to_string());
        source = cause.source();
    }
    detail.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_statuses_map_to_unauthorized() {
        for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
            let result = classify_response(ProviderKind::Mineru, status, "");
            assert_eq!(result.code, ConnectionTestCode::Unauthorized);
            assert!(!result.ok);
        }
    }

    #[test]
    fn rate_limits_and_server_errors_are_distinguished() {
        assert_eq!(
            classify_response(ProviderKind::Translation, StatusCode::TOO_MANY_REQUESTS, "").code,
            ConnectionTestCode::RateLimited
        );
        assert_eq!(
            classify_response(
                ProviderKind::Translation,
                StatusCode::INTERNAL_SERVER_ERROR,
                ""
            )
            .code,
            ConnectionTestCode::ServerError
        );
        assert_eq!(
            classify_response(ProviderKind::Mineru, StatusCode::BAD_GATEWAY, "").code,
            ConnectionTestCode::ServerError
        );
    }

    #[test]
    fn a_non_json_success_is_protocol_incompatible() {
        let result = classify_response(
            ProviderKind::Translation,
            StatusCode::OK,
            "<html>hello</html>",
        );

        assert_eq!(result.code, ConnectionTestCode::ProtocolIncompatible);
    }

    #[test]
    fn mineru_reports_authorization_failures_inside_a_200_body() {
        let result = classify_response(
            ProviderKind::Mineru,
            StatusCode::OK,
            r#"{"code":"A0202","msg":"token error","trace_id":"t"}"#,
        );

        assert_eq!(result.code, ConnectionTestCode::Unauthorized);
        assert!(!result.ok);
    }

    #[test]
    fn mineru_accepts_a_structured_body_for_a_missing_task() {
        for body in [
            r#"{"code":0,"data":{"task_id":"x","state":"done"},"msg":"ok"}"#,
            r#"{"code":-60012,"msg":"task not found","trace_id":"t"}"#,
        ] {
            let result = classify_response(ProviderKind::Mineru, StatusCode::OK, body);
            assert!(result.ok, "{body} should pass");
            assert_eq!(result.code, ConnectionTestCode::Ok);
        }
    }

    #[test]
    fn a_json_body_without_a_mineru_envelope_is_protocol_incompatible() {
        for body in [
            r#"{"hello":"world"}"#,
            // A bare status code is what a generic gateway or CDN returns.
            r#"{"code":404}"#,
            r#"{"code":"NOT_FOUND","error":"no such route"}"#,
            r#"["code"]"#,
        ] {
            let result = classify_response(ProviderKind::Mineru, StatusCode::OK, body);
            assert_eq!(
                result.code,
                ConnectionTestCode::ProtocolIncompatible,
                "{body} should not pass as Cloud MinerU"
            );
        }
    }

    #[test]
    fn mineru_passes_when_it_answers_a_missing_task_with_a_client_error() {
        let result = classify_response(
            ProviderKind::Mineru,
            StatusCode::NOT_FOUND,
            r#"{"code":-60012,"msg":"task not found","trace_id":"t"}"#,
        );

        assert!(result.ok);
        assert_eq!(result.code, ConnectionTestCode::Ok);
    }

    #[test]
    fn redirects_are_followed_only_inside_one_origin() {
        let source: Url = "https://mineru.example.com/api/v4".parse().expect("url");

        assert!(follows_redirect(
            &"https://mineru.example.com/api/v4/x".parse().expect("url"),
            std::slice::from_ref(&source),
            MAX_REDIRECTS,
        ));
        // A scheme downgrade keeps the same host, so reqwest would not strip the
        // bearer header on its own.
        assert!(!follows_redirect(
            &"http://mineru.example.com/api/v4/x".parse().expect("url"),
            std::slice::from_ref(&source),
            MAX_REDIRECTS,
        ));
        assert!(!follows_redirect(
            &"https://evil.example.com/api/v4/x".parse().expect("url"),
            std::slice::from_ref(&source),
            MAX_REDIRECTS,
        ));
        assert!(!follows_redirect(
            &"https://mineru.example.com:8443/x".parse().expect("url"),
            std::slice::from_ref(&source),
            MAX_REDIRECTS,
        ));
    }

    #[test]
    fn redirect_chains_stop_at_the_limit() {
        let hop: Url = "https://mineru.example.com/a".parse().expect("url");
        let target: Url = "https://mineru.example.com/b".parse().expect("url");

        assert!(follows_redirect(&target, &[hop.clone(), hop.clone()], 2));
        assert!(!follows_redirect(
            &target,
            &[hop.clone(), hop.clone(), hop],
            2
        ));
        assert!(!follows_redirect(&target, &[], 2));
    }

    #[test]
    fn an_openai_compatible_model_list_passes() {
        let result = classify_response(
            ProviderKind::Translation,
            StatusCode::OK,
            r#"{"object":"list","data":[{"id":"gpt-4o-mini"}]}"#,
        );

        assert!(result.ok);
        assert_eq!(result.code, ConnectionTestCode::Ok);
    }

    #[test]
    fn a_missing_model_list_is_protocol_incompatible() {
        for (status, body) in [
            (StatusCode::NOT_FOUND, r#"{"error":"not found"}"#),
            (StatusCode::OK, r#"{"object":"list"}"#),
            (StatusCode::BAD_REQUEST, r#"{"error":"bad request"}"#),
        ] {
            let result = classify_response(ProviderKind::Translation, status, body);
            assert_eq!(result.code, ConnectionTestCode::ProtocolIncompatible);
        }
    }

    #[test]
    fn probe_urls_follow_each_provider_protocol() {
        use atlas_provider_settings::{NormalizedEndpoint, normalize};

        let mineru = ProbeRequest {
            kind: ProviderKind::Mineru,
            endpoint: normalize(ProviderKind::Mineru, "https://mineru.example.com/api/v4")
                .expect("endpoint should normalize"),
            api_key: None,
        };
        let translation = ProbeRequest {
            kind: ProviderKind::Translation,
            endpoint: NormalizedEndpoint::restore(
                ProviderKind::Translation,
                "https://models.example.com".to_owned(),
                "/v1".to_owned(),
            ),
            api_key: None,
        };

        assert_eq!(
            HttpConnectionProbe::probe_url(&mineru),
            format!("https://mineru.example.com/api/v4/extract/task/{MINERU_PROBE_TASK_ID}")
        );
        assert_eq!(
            HttpConnectionProbe::probe_url(&translation),
            "https://models.example.com/v1/models"
        );
    }
}
