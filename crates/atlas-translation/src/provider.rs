use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex as StdMutex},
};

use async_trait::async_trait;
use atlas_domain::AtlasError;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct TranslationCredential(Arc<str>);

impl TranslationCredential {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(Arc::from(value.into()))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TranslationCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TranslationCredential(<redacted>)")
    }
}

#[derive(Clone, Debug)]
pub struct TranslationConfiguration {
    pub profile_id: String,
    pub endpoint_base_url: String,
    pub endpoint_fingerprint: String,
    pub model_id: String,
    pub context_window: u32,
    pub credential: Option<TranslationCredential>,
}

#[async_trait]
pub trait TranslationConfigurationPort: Send + Sync {
    async fn load(&self) -> Result<Option<TranslationConfiguration>, AtlasError>;
}

#[derive(Clone, Debug)]
pub struct ProviderTranslationRequest {
    pub system_prompt: String,
    pub input_json: String,
    pub max_output_tokens: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranslationCompletion {
    pub finish_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranslationProviderErrorKind {
    Unauthorized,
    RateLimited,
    Timeout,
    Transport,
    Protocol,
    ContextLength,
    Remote,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranslationProviderError {
    pub kind: TranslationProviderErrorKind,
    pub safe_message: String,
    pub retry_after_seconds: Option<u64>,
}

impl TranslationProviderError {
    #[must_use]
    pub fn new(kind: TranslationProviderErrorKind, safe_message: impl Into<String>) -> Self {
        Self {
            kind,
            safe_message: safe_message.into(),
            retry_after_seconds: None,
        }
    }

    #[must_use]
    pub fn with_retry_after(mut self, seconds: u64) -> Self {
        self.retry_after_seconds = Some(seconds);
        self
    }
}

#[async_trait]
pub trait TranslationChunkSink: Send + Sync {
    async fn push(&self, content: &str) -> Result<(), AtlasError>;
}

#[async_trait]
pub trait TranslationProviderPort: Send + Sync {
    async fn stream(
        &self,
        configuration: &TranslationConfiguration,
        request: ProviderTranslationRequest,
        sink: Arc<dyn TranslationChunkSink>,
        cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError>;
}

#[derive(Clone, Debug)]
pub struct ScriptedTranslationResponse {
    pub chunks: Vec<String>,
    pub finish_reason: Option<String>,
}

#[derive(Clone, Default)]
pub struct ScriptedTranslationAdapter {
    responses:
        Arc<StdMutex<VecDeque<Result<ScriptedTranslationResponse, TranslationProviderError>>>>,
    requests: Arc<StdMutex<Vec<ProviderTranslationRequest>>>,
}

impl fmt::Debug for ScriptedTranslationAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScriptedTranslationAdapter")
            .finish_non_exhaustive()
    }
}

impl ScriptedTranslationAdapter {
    #[must_use]
    pub fn new(
        responses: impl IntoIterator<
            Item = Result<ScriptedTranslationResponse, TranslationProviderError>,
        >,
    ) -> Self {
        Self {
            responses: Arc::new(StdMutex::new(responses.into_iter().collect())),
            requests: Arc::new(StdMutex::new(Vec::new())),
        }
    }

    #[must_use]
    pub fn requests(&self) -> Vec<ProviderTranslationRequest> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl TranslationProviderPort for ScriptedTranslationAdapter {
    async fn stream(
        &self,
        _configuration: &TranslationConfiguration,
        request: ProviderTranslationRequest,
        sink: Arc<dyn TranslationChunkSink>,
        cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request);
        let response = self
            .responses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .unwrap_or_else(|| {
                Err(TranslationProviderError::new(
                    TranslationProviderErrorKind::Protocol,
                    "The scripted provider has no response",
                ))
            })?;
        for chunk in response.chunks {
            if cancellation.is_cancelled() {
                return Err(TranslationProviderError::new(
                    TranslationProviderErrorKind::Cancelled,
                    "Translation was cancelled",
                ));
            }
            sink.push(&chunk).await.map_err(|error| {
                TranslationProviderError::new(TranslationProviderErrorKind::Protocol, error.message)
            })?;
        }
        Ok(TranslationCompletion {
            finish_reason: response.finish_reason,
        })
    }
}
