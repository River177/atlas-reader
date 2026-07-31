use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use atlas_domain::{AtlasError, AtlasErrorCode};
use atlas_reading_assistant::{
    CitationMarkerParser, ReadingAssistantProviderPort, ReadingAssistantProviderRequest,
    ReadingAssistantStreamEvent, ReadingAssistantStreamSink,
};
use atlas_translation::{
    ProviderTranslationRequest, TranslationChunkSink, TranslationCompletion,
    TranslationConfiguration, TranslationProviderError, TranslationProviderErrorKind,
    TranslationProviderPort,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::OpenAiCompatibleTranslationAdapter;

#[derive(Clone, Debug)]
pub struct OpenAiCompatibleReadingAssistantAdapter {
    inner: OpenAiCompatibleTranslationAdapter,
}

impl OpenAiCompatibleReadingAssistantAdapter {
    pub fn new() -> Result<Self, AtlasError> {
        Ok(Self {
            inner: OpenAiCompatibleTranslationAdapter::new()?,
        })
    }

    pub fn with_idle_timeout(idle_timeout: Duration) -> Result<Self, AtlasError> {
        Ok(Self {
            inner: OpenAiCompatibleTranslationAdapter::with_idle_timeout(idle_timeout)?,
        })
    }
}

#[async_trait]
impl ReadingAssistantProviderPort for OpenAiCompatibleReadingAssistantAdapter {
    async fn stream(
        &self,
        configuration: &TranslationConfiguration,
        request: ReadingAssistantProviderRequest,
        sink: Arc<dyn ReadingAssistantStreamSink>,
        cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        let parser_sink = Arc::new(ParserSink::new(request.allowed_citation_ids, sink));
        let result = self
            .inner
            .stream(
                configuration,
                ProviderTranslationRequest {
                    system_prompt: request.system_prompt,
                    input_json: request.input_json,
                    max_output_tokens: request.max_output_tokens,
                },
                parser_sink.clone(),
                cancellation,
            )
            .await;
        let finish = parser_sink.finish().await;
        match (result, finish) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(provider_error(error)),
            (Ok(completion), Ok(())) => Ok(completion),
        }
    }
}

struct ParserSink {
    parser: Mutex<Option<CitationMarkerParser>>,
    sink: Arc<dyn ReadingAssistantStreamSink>,
}

impl ParserSink {
    fn new(allowed_citation_ids: Vec<String>, sink: Arc<dyn ReadingAssistantStreamSink>) -> Self {
        Self {
            parser: Mutex::new(Some(CitationMarkerParser::new(allowed_citation_ids))),
            sink,
        }
    }

    async fn finish(&self) -> Result<(), AtlasError> {
        let parser = self
            .parser
            .lock()
            .await
            .take()
            .ok_or_else(|| AtlasError::internal("citation parser already finished"))?;
        self.push_events(parser.finish()).await
    }

    async fn push_events(
        &self,
        events: Vec<ReadingAssistantStreamEvent>,
    ) -> Result<(), AtlasError> {
        for event in events {
            self.sink.push(event).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl TranslationChunkSink for ParserSink {
    async fn push(&self, content: &str) -> Result<(), AtlasError> {
        let events = self
            .parser
            .lock()
            .await
            .as_mut()
            .ok_or_else(|| AtlasError::internal("citation parser already finished"))?
            .push(content);
        self.push_events(events).await
    }
}

fn provider_error(error: AtlasError) -> TranslationProviderError {
    TranslationProviderError::new(
        if error.code == AtlasErrorCode::StorageUnavailable {
            TranslationProviderErrorKind::Transport
        } else {
            TranslationProviderErrorKind::Protocol
        },
        error.message,
    )
}
