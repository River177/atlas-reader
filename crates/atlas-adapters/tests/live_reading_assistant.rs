//! Live contract test for the OpenAI-compatible Reading Assistant stream.
//!
//! Skipped unless `ATLAS_LIVE_READING_CHAT=1`, so normal test runs remain
//! offline. The request contains only synthetic paper text, and the streamed
//! response is asserted in memory without being printed or stored as a fixture.
//!
//! Run with:
//!
//! ```text
//! ATLAS_LIVE_READING_CHAT=1 cargo test -p atlas-adapters \
//!   --test live_reading_assistant -- --nocapture
//! ```

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use atlas_adapters::OpenAiCompatibleReadingAssistantAdapter;
use atlas_domain::{AtlasError, ProviderKind};
use atlas_reading_assistant::{
    ReadingAssistantProviderPort, ReadingAssistantProviderRequest, ReadingAssistantStreamEvent,
    ReadingAssistantStreamSink,
};
use atlas_translation::{TranslationConfiguration, TranslationCredential};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

mod support;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:4141/v1";
const CITATION_ID: &str = "ctx-synthetic-1";

fn enabled() -> bool {
    std::env::var("ATLAS_LIVE_READING_CHAT").as_deref() == Ok("1")
}

fn base_url() -> String {
    std::env::var("ATLAS_LIVE_READING_CHAT_URL")
        .or_else(|_| std::env::var("ATLAS_LIVE_TRANSLATION_URL"))
        .unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned())
}

fn model_id() -> String {
    std::env::var("ATLAS_LIVE_READING_CHAT_MODEL")
        .or_else(|_| std::env::var("ATLAS_LIVE_TRANSLATION_MODEL"))
        .unwrap_or_else(|_| "gemini-3.5-flash".to_owned())
}

#[derive(Default)]
struct EventSink(Mutex<Vec<ReadingAssistantStreamEvent>>);

#[async_trait]
impl ReadingAssistantStreamSink for EventSink {
    async fn push(&self, event: ReadingAssistantStreamEvent) -> Result<(), AtlasError> {
        self.0.lock().await.push(event);
        Ok(())
    }
}

#[tokio::test]
async fn synthetic_selection_streams_text_and_an_allowlisted_citation() {
    if !enabled() {
        eprintln!("skipping: set ATLAS_LIVE_READING_CHAT=1 to run");
        return;
    }
    let credential = support::effective_provider_secret(ProviderKind::Translation)
        .await
        .expect("the configured credential should be readable")
        .map(|secret| TranslationCredential::new(secret.expose()));
    let configuration = TranslationConfiguration {
        profile_id: "openai_compatible".to_owned(),
        endpoint_base_url: base_url(),
        endpoint_fingerprint: "live-reading-chat-contract-test".to_owned(),
        model_id: model_id(),
        context_window: 32_768,
        credential,
    };
    let request = ReadingAssistantProviderRequest {
        system_prompt: concat!(
            "Answer the synthetic question in one short Chinese sentence. ",
            "Include this exact citation marker once: ",
            "⟦ATLAS-CITE:ctx-synthetic-1⟧. ",
            "Do not emit any other citation marker."
        )
        .to_owned(),
        input_json: concat!(
            r#"{"question":"为什么这个对照实验必要？","selection":{"translated":"该实验只改变检索策略。","#,
            r#""source":"The experiment changes only the retrieval strategy."},"#,
            r#""context":[{"id":"ctx-synthetic-1","text":"All other variables remain fixed.","page":1}]}"#
        )
        .to_owned(),
        max_output_tokens: 2_048,
        allowed_citation_ids: vec![CITATION_ID.to_owned()],
    };
    let sink = Arc::new(EventSink::default());

    let completion =
        OpenAiCompatibleReadingAssistantAdapter::with_idle_timeout(Duration::from_secs(60))
            .expect("adapter should build")
            .stream(
                &configuration,
                request,
                sink.clone(),
                CancellationToken::new(),
            )
            .await
            .expect("live Reading Assistant response should complete");
    let events = sink.0.lock().await;
    let text = events
        .iter()
        .filter_map(|event| match event {
            ReadingAssistantStreamEvent::Text(value) => Some(value.as_str()),
            ReadingAssistantStreamEvent::Citation(_) | ReadingAssistantStreamEvent::Warning(_) => {
                None
            }
        })
        .collect::<String>();
    let citations = events
        .iter()
        .filter_map(|event| match event {
            ReadingAssistantStreamEvent::Citation(id) => Some(id.as_str()),
            ReadingAssistantStreamEvent::Text(_) | ReadingAssistantStreamEvent::Warning(_) => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(completion.finish_reason.as_deref(), Some("stop"));
    assert!(
        !text.trim().is_empty(),
        "the live response should stream text"
    );
    assert!(
        !text.contains("ATLAS-CITE"),
        "citation markers must not leak into rendered text"
    );
    assert_eq!(citations, vec![CITATION_ID]);
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, ReadingAssistantStreamEvent::Warning(_))),
        "the live response must not produce citation warnings"
    );
}
