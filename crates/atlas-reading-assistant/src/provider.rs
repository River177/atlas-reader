use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, Mutex as StdMutex, PoisonError},
};

use async_trait::async_trait;
use atlas_domain::AtlasError;
use atlas_translation::{
    TranslationCompletion, TranslationConfiguration, TranslationProviderError,
};
use tokio_util::sync::CancellationToken;

const MARKER_PREFIX: &str = "⟦ATLAS-CITE:";
const MARKER_SUFFIX: char = '⟧';
const MAX_MARKER_BYTES: usize = 128;

#[derive(Clone, Debug)]
pub struct ReadingAssistantProviderRequest {
    pub system_prompt: String,
    pub input_json: String,
    pub max_output_tokens: u32,
    pub allowed_citation_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadingAssistantStreamEvent {
    Text(String),
    Citation(String),
    Warning(String),
}

#[async_trait]
pub trait ReadingAssistantStreamSink: Send + Sync {
    async fn push(&self, event: ReadingAssistantStreamEvent) -> Result<(), AtlasError>;
}

#[async_trait]
pub trait ReadingAssistantProviderPort: Send + Sync {
    async fn stream(
        &self,
        configuration: &TranslationConfiguration,
        request: ReadingAssistantProviderRequest,
        sink: Arc<dyn ReadingAssistantStreamSink>,
        cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError>;
}

#[derive(Debug)]
pub struct CitationMarkerParser {
    buffer: String,
    allowed: HashSet<String>,
    seen: HashSet<String>,
    discarding_marker: bool,
}

impl CitationMarkerParser {
    #[must_use]
    pub fn new(allowed: impl IntoIterator<Item = String>) -> Self {
        Self {
            buffer: String::new(),
            allowed: allowed.into_iter().collect(),
            seen: HashSet::new(),
            discarding_marker: false,
        }
    }

    pub fn push(&mut self, fragment: &str) -> Vec<ReadingAssistantStreamEvent> {
        self.buffer.push_str(fragment);
        self.drain(false)
    }

    pub fn finish(mut self) -> Vec<ReadingAssistantStreamEvent> {
        self.drain(true)
    }

    fn drain(&mut self, finish: bool) -> Vec<ReadingAssistantStreamEvent> {
        let mut events = Vec::new();
        let input = std::mem::take(&mut self.buffer);
        let mut cursor = 0;
        if self.discarding_marker {
            if let Some(end) = input.find(MARKER_SUFFIX) {
                cursor = end + MARKER_SUFFIX.len_utf8();
                self.discarding_marker = false;
            } else {
                return events;
            }
        }
        loop {
            let remaining = &input[cursor..];
            let Some(relative_start) = remaining.find(MARKER_PREFIX) else {
                let keep = trailing_prefix_bytes(remaining, MARKER_PREFIX);
                if keep == 0 {
                    push_text(&mut events, remaining.to_owned());
                } else {
                    let split = remaining.len().saturating_sub(keep);
                    push_text(&mut events, remaining[..split].to_owned());
                    if finish {
                        events.push(ReadingAssistantStreamEvent::Warning(
                            "citation_marker_invalid".to_owned(),
                        ));
                    } else {
                        self.buffer.push_str(&remaining[split..]);
                    }
                }
                break;
            };
            let start = cursor + relative_start;
            if start > cursor {
                push_text(&mut events, input[cursor..start].to_owned());
            }
            let marker_input = &input[start..];
            let Some(relative_end) = marker_input.find(MARKER_SUFFIX) else {
                if finish || marker_input.len() > MAX_MARKER_BYTES {
                    events.push(ReadingAssistantStreamEvent::Warning(
                        "citation_marker_invalid".to_owned(),
                    ));
                    self.discarding_marker = !finish;
                } else {
                    self.buffer.push_str(marker_input);
                }
                break;
            };
            let marker_end = start + relative_end + MARKER_SUFFIX.len_utf8();
            let marker = &input[start..marker_end];
            cursor = marker_end;
            let citation_id = marker
                .strip_prefix(MARKER_PREFIX)
                .and_then(|value| value.strip_suffix(MARKER_SUFFIX))
                .unwrap_or_default();
            if !self.allowed.contains(citation_id) {
                events.push(ReadingAssistantStreamEvent::Warning(
                    "citation_out_of_context".to_owned(),
                ));
            } else if !self.seen.insert(citation_id.to_owned()) {
                events.push(ReadingAssistantStreamEvent::Warning(
                    "citation_duplicate".to_owned(),
                ));
            } else {
                events.push(ReadingAssistantStreamEvent::Citation(
                    citation_id.to_owned(),
                ));
            }
            if cursor == input.len() {
                break;
            }
        }
        events
    }
}

fn push_text(events: &mut Vec<ReadingAssistantStreamEvent>, text: String) {
    if !text.is_empty() {
        events.push(ReadingAssistantStreamEvent::Text(text));
    }
}

fn trailing_prefix_bytes(value: &str, prefix: &str) -> usize {
    let mut keep = 0;
    for (index, _) in value.char_indices() {
        let suffix = &value[index..];
        if prefix.starts_with(suffix) {
            keep = suffix.len();
            break;
        }
    }
    keep
}

#[derive(Clone, Debug)]
pub struct ScriptedReadingAssistantResponse {
    pub chunks: Vec<String>,
    pub finish_reason: Option<String>,
}

#[derive(Clone, Default)]
pub struct ScriptedReadingAssistantAdapter {
    responses:
        Arc<StdMutex<VecDeque<Result<ScriptedReadingAssistantResponse, TranslationProviderError>>>>,
    requests: Arc<StdMutex<Vec<ReadingAssistantProviderRequest>>>,
}

impl std::fmt::Debug for ScriptedReadingAssistantAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScriptedReadingAssistantAdapter")
            .finish_non_exhaustive()
    }
}

impl ScriptedReadingAssistantAdapter {
    #[must_use]
    pub fn new(
        responses: impl IntoIterator<
            Item = Result<ScriptedReadingAssistantResponse, TranslationProviderError>,
        >,
    ) -> Self {
        Self {
            responses: Arc::new(StdMutex::new(responses.into_iter().collect())),
            requests: Arc::new(StdMutex::new(Vec::new())),
        }
    }

    #[must_use]
    pub fn requests(&self) -> Vec<ReadingAssistantProviderRequest> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl ReadingAssistantProviderPort for ScriptedReadingAssistantAdapter {
    async fn stream(
        &self,
        _configuration: &TranslationConfiguration,
        request: ReadingAssistantProviderRequest,
        sink: Arc<dyn ReadingAssistantStreamSink>,
        cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());
        let response = self
            .responses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or_else(|| {
                Err(TranslationProviderError::new(
                    atlas_translation::TranslationProviderErrorKind::Protocol,
                    "The scripted Reading Assistant provider has no response",
                ))
            })?;
        let mut parser = CitationMarkerParser::new(request.allowed_citation_ids);
        let mut stream_error = None;
        'chunks: for chunk in response.chunks {
            if cancellation.is_cancelled() {
                stream_error = Some(TranslationProviderError::new(
                    atlas_translation::TranslationProviderErrorKind::Cancelled,
                    "Reading Assistant response was cancelled",
                ));
                break;
            }
            for event in parser.push(&chunk) {
                if let Err(error) = sink.push(event).await {
                    stream_error = Some(TranslationProviderError::new(
                        atlas_translation::TranslationProviderErrorKind::Protocol,
                        error.message,
                    ));
                    break 'chunks;
                }
            }
        }
        if cancellation.is_cancelled() && stream_error.is_none() {
            stream_error = Some(TranslationProviderError::new(
                atlas_translation::TranslationProviderErrorKind::Cancelled,
                "Reading Assistant response was cancelled",
            ));
        }
        let mut finish_error = None;
        for event in parser.finish() {
            if let Err(error) = sink.push(event).await {
                finish_error = Some(TranslationProviderError::new(
                    atlas_translation::TranslationProviderErrorKind::Protocol,
                    error.message,
                ));
                break;
            }
        }
        if let Some(error) = stream_error.or(finish_error) {
            return Err(error);
        }
        Ok(TranslationCompletion {
            finish_reason: response.finish_reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<ReadingAssistantStreamEvent>>);

    #[async_trait]
    impl ReadingAssistantStreamSink for RecordingSink {
        async fn push(&self, event: ReadingAssistantStreamEvent) -> Result<(), AtlasError> {
            self.0.lock().await.push(event);
            Ok(())
        }
    }

    #[test]
    fn parser_handles_split_markers_and_keeps_plain_text_order() {
        let mut parser = CitationMarkerParser::new(["ctx-01".to_owned()]);
        let mut events = parser.push("结论见 ⟦ATLAS-");
        events.extend(parser.push("CITE:ctx-01⟧，因此成立。"));
        events.extend(parser.finish());

        assert_eq!(
            events,
            vec![
                ReadingAssistantStreamEvent::Text("结论见 ".to_owned()),
                ReadingAssistantStreamEvent::Citation("ctx-01".to_owned()),
                ReadingAssistantStreamEvent::Text("，因此成立。".to_owned()),
            ]
        );
    }

    #[test]
    fn parser_rejects_unknown_duplicate_and_incomplete_markers() {
        let mut parser = CitationMarkerParser::new(["ctx-01".to_owned()]);
        let mut events = parser.push(concat!(
            "A⟦ATLAS-CITE:ctx-unknown⟧",
            "B⟦ATLAS-CITE:ctx-01⟧",
            "C⟦ATLAS-CITE:ctx-01⟧",
            "D⟦ATLAS-CITE:"
        ));
        events.extend(parser.finish());

        assert!(events.contains(&ReadingAssistantStreamEvent::Warning(
            "citation_out_of_context".to_owned()
        )));
        assert!(events.contains(&ReadingAssistantStreamEvent::Warning(
            "citation_duplicate".to_owned()
        )));
        assert!(events.contains(&ReadingAssistantStreamEvent::Warning(
            "citation_marker_invalid".to_owned()
        )));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ReadingAssistantStreamEvent::Citation(_)))
                .count(),
            1
        );
        assert!(!events.iter().any(|event| {
            matches!(event, ReadingAssistantStreamEvent::Text(text) if text.contains("ATLAS"))
        }));
    }

    #[test]
    fn oversized_marker_is_discarded_through_its_suffix() {
        let mut parser = CitationMarkerParser::new(["ctx-01".to_owned()]);
        let events = parser.push(&format!("{MARKER_PREFIX}{}", "x".repeat(256)));
        assert_eq!(
            events,
            vec![ReadingAssistantStreamEvent::Warning(
                "citation_marker_invalid".to_owned()
            )]
        );

        let mut events = parser.push("still-marker⟧safe text");
        events.extend(parser.finish());
        assert_eq!(
            events,
            vec![ReadingAssistantStreamEvent::Text("safe text".to_owned())]
        );
    }

    #[tokio::test]
    async fn scripted_adapter_honours_pre_cancelled_empty_responses() {
        let adapter =
            ScriptedReadingAssistantAdapter::new([Ok(ScriptedReadingAssistantResponse {
                chunks: Vec::new(),
                finish_reason: Some("stop".to_owned()),
            })]);
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = adapter
            .stream(
                &TranslationConfiguration {
                    profile_id: "test".to_owned(),
                    endpoint_base_url: "http://127.0.0.1:1/v1".to_owned(),
                    endpoint_fingerprint: "endpoint".to_owned(),
                    model_id: "model".to_owned(),
                    context_window: 32_768,
                    credential: None,
                },
                ReadingAssistantProviderRequest {
                    system_prompt: "test".to_owned(),
                    input_json: "{}".to_owned(),
                    max_output_tokens: 128,
                    allowed_citation_ids: Vec::new(),
                },
                Arc::new(RecordingSink::default()),
                cancellation,
            )
            .await
            .expect_err("pre-cancelled response should not succeed");

        assert_eq!(
            error.kind,
            atlas_translation::TranslationProviderErrorKind::Cancelled
        );
    }
}
