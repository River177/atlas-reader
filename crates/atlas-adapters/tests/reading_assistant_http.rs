use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use atlas_adapters::OpenAiCompatibleReadingAssistantAdapter;
use atlas_domain::AtlasError;
use atlas_reading_assistant::{
    ReadingAssistantProviderPort, ReadingAssistantProviderRequest, ReadingAssistantStreamEvent,
    ReadingAssistantStreamSink,
};
use atlas_translation::{TranslationConfiguration, TranslationCredential};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[derive(Default)]
struct EventSink(Mutex<Vec<ReadingAssistantStreamEvent>>);

#[async_trait]
impl ReadingAssistantStreamSink for EventSink {
    async fn push(&self, event: ReadingAssistantStreamEvent) -> Result<(), AtlasError> {
        self.0.lock().await.push(event);
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

#[tokio::test]
async fn streams_sanitized_text_and_allowlisted_citations_through_the_shared_transport() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"解释 ⟦ATLAS-\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"CITE:ctx-01⟧ 完成。\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;
    let sink = Arc::new(EventSink::default());
    let request = ReadingAssistantProviderRequest {
        system_prompt: "Answer with allowlisted citation markers.".to_owned(),
        input_json: r#"{"question":"为什么？","context":[{"id":"ctx-01"}]}"#.to_owned(),
        max_output_tokens: 2_048,
        allowed_citation_ids: vec!["ctx-01".to_owned()],
    };

    let completion =
        OpenAiCompatibleReadingAssistantAdapter::with_idle_timeout(Duration::from_secs(2))
            .expect("adapter should build")
            .stream(
                &configuration(&server),
                request,
                sink.clone(),
                CancellationToken::new(),
            )
            .await
            .expect("chat should complete");

    assert_eq!(completion.finish_reason.as_deref(), Some("stop"));
    assert_eq!(
        *sink.0.lock().await,
        vec![
            ReadingAssistantStreamEvent::Text("解释 ".to_owned()),
            ReadingAssistantStreamEvent::Citation("ctx-01".to_owned()),
            ReadingAssistantStreamEvent::Text(" 完成。".to_owned()),
        ]
    );
    let requests = server
        .received_requests()
        .await
        .expect("request history should be available");
    let payload: Value =
        serde_json::from_slice(&requests[0].body).expect("request body should be JSON");
    assert_eq!(payload["model"], "test-model");
    assert_eq!(payload["stream"], true);
    assert_eq!(
        payload["messages"][0]["content"],
        "Answer with allowlisted citation markers."
    );
    assert_eq!(
        payload["messages"][1]["content"],
        r#"{"question":"为什么？","context":[{"id":"ctx-01"}]}"#
    );
    assert!(payload.get("response_format").is_none());
}

#[tokio::test]
async fn unknown_citations_become_warnings_not_clickable_targets() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"A⟦ATLAS-CITE:ctx-evil⟧B\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let sink = Arc::new(EventSink::default());

    OpenAiCompatibleReadingAssistantAdapter::new()
        .expect("adapter should build")
        .stream(
            &TranslationConfiguration {
                credential: None,
                ..configuration(&server)
            },
            ReadingAssistantProviderRequest {
                system_prompt: "Answer safely.".to_owned(),
                input_json: "{}".to_owned(),
                max_output_tokens: 512,
                allowed_citation_ids: vec!["ctx-01".to_owned()],
            },
            sink.clone(),
            CancellationToken::new(),
        )
        .await
        .expect("chat should complete");

    assert_eq!(
        *sink.0.lock().await,
        vec![
            ReadingAssistantStreamEvent::Text("A".to_owned()),
            ReadingAssistantStreamEvent::Warning("citation_out_of_context".to_owned()),
            ReadingAssistantStreamEvent::Text("B".to_owned()),
        ]
    );
}
