use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use atlas_adapters::OpenAiCompatibleTranslationAdapter;
use atlas_domain::AtlasError;
use atlas_translation::{
    ProviderTranslationRequest, TranslationChunkSink, TranslationConfiguration,
    TranslationCredential, TranslationProviderErrorKind, TranslationProviderPort,
};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

#[derive(Default)]
struct StringSink(Mutex<String>);

#[async_trait]
impl TranslationChunkSink for StringSink {
    async fn push(&self, content: &str) -> Result<(), AtlasError> {
        self.0.lock().await.push_str(content);
        Ok(())
    }
}

fn configuration(server: &MockServer) -> TranslationConfiguration {
    TranslationConfiguration {
        profile_id: "openai_compatible".to_owned(),
        endpoint_base_url: format!("{}/v1", server.uri()),
        endpoint_fingerprint: "endpoint-fingerprint".to_owned(),
        model_id: "test-model".to_owned(),
        context_window: 32_768,
        credential: Some(TranslationCredential::new("model-key")),
    }
}

fn request() -> ProviderTranslationRequest {
    ProviderTranslationRequest {
        system_prompt: concat!(
            "Return the exact literal record ",
            r#"{"id":"block-01","target":"译文"}"#
        )
        .to_owned(),
        input_json: r#"{"blocks":[{"id":"block-1","source":"Source"}]}"#.to_owned(),
        max_output_tokens: 2_048,
    }
}

#[tokio::test]
async fn streams_chat_completions_with_the_literal_json_lines_contract() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"{\\\"id\\\":\\\"block-1\\\",\\\"target\\\":\\\"甲\\\"}\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer model-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;
    let sink = Arc::new(StringSink::default());

    let completion = OpenAiCompatibleTranslationAdapter::with_idle_timeout(Duration::from_secs(2))
        .expect("adapter should build")
        .stream(
            &configuration(&server),
            request(),
            sink.clone(),
            CancellationToken::new(),
        )
        .await
        .expect("stream should complete");

    assert_eq!(completion.finish_reason.as_deref(), Some("stop"));
    assert_eq!(
        sink.0.lock().await.as_str(),
        r#"{"id":"block-1","target":"甲"}"#
    );
    let requests = server
        .received_requests()
        .await
        .expect("request history should be available");
    let payload: Value =
        serde_json::from_slice(&requests[0].body).expect("request body should be JSON");
    assert_eq!(payload["model"], "test-model");
    assert_eq!(payload["stream"], true);
    assert!(payload.get("response_format").is_none());
    assert!(
        payload["messages"][0]["content"]
            .as_str()
            .expect("system message should be text")
            .contains(r#"{"id":"block-01","target":"译文"}"#)
    );
}

#[tokio::test]
async fn classifies_context_length_errors_for_one_bounded_split_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"error":{"code":"context_length_exceeded","message":"maximum context length exceeded"}}"#,
        ))
        .mount(&server)
        .await;

    let error = OpenAiCompatibleTranslationAdapter::new()
        .expect("adapter should build")
        .stream(
            &configuration(&server),
            request(),
            Arc::new(StringSink::default()),
            CancellationToken::new(),
        )
        .await
        .expect_err("oversized request should fail");

    assert_eq!(error.kind, TranslationProviderErrorKind::ContextLength);
}

#[tokio::test]
async fn response_headers_are_bounded_by_the_inactivity_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(200))
                .set_body_string("data: [DONE]\n\n"),
        )
        .mount(&server)
        .await;

    let error = OpenAiCompatibleTranslationAdapter::with_idle_timeout(Duration::from_millis(20))
        .expect("adapter should build")
        .stream(
            &configuration(&server),
            request(),
            Arc::new(StringSink::default()),
            CancellationToken::new(),
        )
        .await
        .expect_err("stalled headers should time out");

    assert_eq!(error.kind, TranslationProviderErrorKind::Timeout);
}

#[tokio::test]
async fn done_terminates_a_stream_even_when_the_server_keeps_the_socket_open() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let address = listener
        .local_addr()
        .expect("listener should have an address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("client should connect");
        let mut request_bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = socket.read(&mut buffer).await.expect("request should read");
            if read == 0 {
                return;
            }
            request_bytes.extend_from_slice(&buffer[..read]);
        }
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n{:X}\r\n{}\r\n",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .expect("response should write");
        socket.flush().await.expect("response should flush");
        tokio::time::sleep(Duration::from_secs(1)).await;
    });
    let configuration = TranslationConfiguration {
        profile_id: "openai_compatible".to_owned(),
        endpoint_base_url: format!("http://{address}/v1"),
        endpoint_fingerprint: "endpoint-fingerprint".to_owned(),
        model_id: "test-model".to_owned(),
        context_window: 32_768,
        credential: None,
    };

    let completion =
        OpenAiCompatibleTranslationAdapter::with_idle_timeout(Duration::from_millis(100))
            .expect("adapter should build")
            .stream(
                &configuration,
                request(),
                Arc::new(StringSink::default()),
                CancellationToken::new(),
            )
            .await
            .expect("[DONE] should complete without waiting for EOF");

    assert_eq!(completion.finish_reason.as_deref(), Some("stop"));
    server.abort();
}
