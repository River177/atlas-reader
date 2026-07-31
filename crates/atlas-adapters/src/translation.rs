use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use atlas_domain::{AtlasError, AtlasErrorCode};
use atlas_translation::{
    ProviderTranslationRequest, TranslationChunkSink, TranslationCompletion,
    TranslationConfiguration, TranslationProviderError, TranslationProviderErrorKind,
    TranslationProviderPort,
};
use reqwest::{
    Client, Response, StatusCode,
    header::{HeaderMap, RETRY_AFTER},
};
use serde_json::{Value, json};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::connection_probe::same_origin_redirects;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_ERROR_BODY_BYTES: usize = 1 << 20;
const MAX_STREAM_BYTES: usize = 4 << 20;

#[derive(Clone, Debug)]
pub struct OpenAiCompatibleTranslationAdapter {
    client: Client,
    idle_timeout: Duration,
}

impl OpenAiCompatibleTranslationAdapter {
    pub fn new() -> Result<Self, AtlasError> {
        Self::with_idle_timeout(STREAM_IDLE_TIMEOUT)
    }

    pub fn with_idle_timeout(idle_timeout: Duration) -> Result<Self, AtlasError> {
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT.min(idle_timeout))
            .redirect(same_origin_redirects(1))
            .user_agent(concat!("AtlasReader/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                AtlasError::internal(format!(
                    "The translation network client could not start: {error}"
                ))
            })?;
        Ok(Self {
            client,
            idle_timeout,
        })
    }

    fn url(configuration: &TranslationConfiguration) -> String {
        format!(
            "{}/chat/completions",
            configuration.endpoint_base_url.trim_end_matches('/')
        )
    }
}

#[async_trait]
impl TranslationProviderPort for OpenAiCompatibleTranslationAdapter {
    async fn stream(
        &self,
        configuration: &TranslationConfiguration,
        request: ProviderTranslationRequest,
        sink: Arc<dyn TranslationChunkSink>,
        cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        let payload = json!({
            "model": configuration.model_id,
            "stream": true,
            "messages": [
                { "role": "system", "content": request.system_prompt },
                { "role": "user", "content": request.input_json }
            ],
            "max_tokens": request.max_output_tokens
        });
        let mut builder = self
            .client
            .post(Self::url(configuration))
            .header("Accept", "text/event-stream")
            .json(&payload);
        if let Some(credential) = configuration.credential.as_ref() {
            builder = builder.bearer_auth(credential.expose());
        }

        let response = tokio::select! {
            () = cancellation.cancelled() => {
                return Err(provider_error(
                    TranslationProviderErrorKind::Cancelled,
                    "Translation was cancelled",
                ));
            }
            result = timeout(self.idle_timeout, builder.send()) => {
                result
                    .map_err(|_| provider_error(
                        TranslationProviderErrorKind::Timeout,
                        "The model endpoint did not send response headers in time",
                    ))?
                    .map_err(classify_transport)?
            }
        };
        if !response.status().is_success() {
            return Err(classify_response(response, &cancellation, self.idle_timeout).await);
        }

        consume_stream(response, sink, cancellation, self.idle_timeout).await
    }
}

async fn consume_stream(
    mut response: Response,
    sink: Arc<dyn TranslationChunkSink>,
    cancellation: CancellationToken,
    idle_timeout: Duration,
) -> Result<TranslationCompletion, TranslationProviderError> {
    let mut decoder = SseDecoder::default();
    let mut finish_reason = None;
    let mut total_bytes = 0_usize;
    loop {
        let chunk = tokio::select! {
            () = cancellation.cancelled() => {
                return Err(provider_error(
                    TranslationProviderErrorKind::Cancelled,
                    "Translation was cancelled",
                ));
            }
            result = timeout(idle_timeout, response.chunk()) => {
                result
                    .map_err(|_| provider_error(
                        TranslationProviderErrorKind::Timeout,
                        "The model sent no translation data for 60 seconds",
                    ))?
                    .map_err(classify_transport)?
            }
        };
        let Some(chunk) = chunk else {
            break;
        };
        total_bytes = total_bytes.saturating_add(chunk.len());
        if total_bytes > MAX_STREAM_BYTES {
            return Err(provider_error(
                TranslationProviderErrorKind::Protocol,
                "The model response exceeded the 4 MB safety limit",
            ));
        }
        for event in decoder.push(&chunk)? {
            if matches!(event, SseEvent::Done) {
                return Ok(TranslationCompletion { finish_reason });
            }
            apply_event(event, &sink, &mut finish_reason).await?;
        }
    }
    for event in decoder.finish()? {
        if matches!(event, SseEvent::Done) {
            break;
        }
        apply_event(event, &sink, &mut finish_reason).await?;
    }
    Ok(TranslationCompletion { finish_reason })
}

async fn apply_event(
    event: SseEvent,
    sink: &Arc<dyn TranslationChunkSink>,
    finish_reason: &mut Option<String>,
) -> Result<(), TranslationProviderError> {
    match event {
        SseEvent::Done => Ok(()),
        SseEvent::Data(value) => {
            if let Some(error) = value.get("error") {
                return Err(classify_error_payload(
                    StatusCode::BAD_REQUEST,
                    error,
                    &HeaderMap::new(),
                ));
            }
            let Some(choice) = value
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
            else {
                return Err(provider_error(
                    TranslationProviderErrorKind::Protocol,
                    "The model stream did not contain an OpenAI-compatible choice",
                ));
            };
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                *finish_reason = Some(reason.to_owned());
            }
            if let Some(content) = choice
                .get("delta")
                .and_then(|delta| delta.get("content"))
                .and_then(Value::as_str)
            {
                sink.push(content).await.map_err(|error| {
                    provider_error(
                        if error.code == AtlasErrorCode::StorageUnavailable {
                            TranslationProviderErrorKind::Transport
                        } else {
                            TranslationProviderErrorKind::Protocol
                        },
                        error.message,
                    )
                })?;
            }
            Ok(())
        }
    }
}

#[derive(Debug)]
enum SseEvent {
    Data(Value),
    Done,
}

#[derive(Debug, Default)]
struct SseDecoder {
    buffer: Vec<u8>,
    data_lines: Vec<String>,
    swallow_lf: bool,
}

impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, TranslationProviderError> {
        let bytes = if self.swallow_lf {
            self.swallow_lf = false;
            bytes.strip_prefix(b"\n").unwrap_or(bytes)
        } else {
            bytes
        };
        self.buffer.extend_from_slice(bytes);
        self.drain_lines()
    }

    fn finish(mut self) -> Result<Vec<SseEvent>, TranslationProviderError> {
        let mut events = self.drain_lines()?;
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            if let Some(event) = self.process_line(&line)? {
                events.push(event);
            }
        }
        if let Some(event) = self.dispatch()? {
            events.push(event);
        }
        Ok(events)
    }

    fn drain_lines(&mut self) -> Result<Vec<SseEvent>, TranslationProviderError> {
        let mut events = Vec::new();
        while let Some(index) = self
            .buffer
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
        {
            let terminator = self.buffer[index];
            let trailing_cr = terminator == b'\r' && index + 1 == self.buffer.len();
            let terminator_len =
                if terminator == b'\r' && self.buffer.get(index + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
            let line = self.buffer[..index].to_vec();
            self.buffer.drain(..index + terminator_len);
            if trailing_cr {
                self.swallow_lf = true;
            }
            if let Some(event) = self.process_line(&line)? {
                events.push(event);
            }
        }
        Ok(events)
    }

    fn process_line(&mut self, line: &[u8]) -> Result<Option<SseEvent>, TranslationProviderError> {
        let line = std::str::from_utf8(line).map_err(|_| {
            provider_error(
                TranslationProviderErrorKind::Protocol,
                "The model stream contained invalid UTF-8",
            )
        })?;
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        if let Some(data) = line.strip_prefix("data:") {
            self.data_lines
                .push(data.strip_prefix(' ').unwrap_or(data).to_owned());
        }
        Ok(None)
    }

    fn dispatch(&mut self) -> Result<Option<SseEvent>, TranslationProviderError> {
        if self.data_lines.is_empty() {
            return Ok(None);
        }
        let data = std::mem::take(&mut self.data_lines).join("\n");
        parse_sse_data(&data).map(Some)
    }
}

fn parse_sse_data(data: &str) -> Result<SseEvent, TranslationProviderError> {
    if data.trim() == "[DONE]" {
        return Ok(SseEvent::Done);
    }
    serde_json::from_str(data).map(SseEvent::Data).map_err(|_| {
        provider_error(
            TranslationProviderErrorKind::Protocol,
            "The model returned an invalid SSE event",
        )
    })
}

async fn classify_response(
    response: Response,
    cancellation: &CancellationToken,
    idle_timeout: Duration,
) -> TranslationProviderError {
    let status = response.status();
    let headers = response.headers().clone();
    let body = match timeout(
        idle_timeout,
        read_capped_body(response, MAX_ERROR_BODY_BYTES, cancellation, idle_timeout),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(error)) => return error,
        Err(_) => {
            return provider_error(
                TranslationProviderErrorKind::Timeout,
                "The model endpoint stalled while returning an error",
            );
        }
    };
    let payload = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
    let error = payload.get("error").unwrap_or(&payload);
    classify_error_payload(status, error, &headers)
}

fn classify_error_payload(
    status: StatusCode,
    payload: &Value,
    headers: &HeaderMap,
) -> TranslationProviderError {
    let message = payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let code = payload
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let detail = format!("{code} {message}").to_ascii_lowercase();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return provider_error(
            TranslationProviderErrorKind::Unauthorized,
            "The model endpoint rejected the API key",
        );
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        let retry_after = retry_after_seconds(headers);
        return TranslationProviderError {
            kind: TranslationProviderErrorKind::RateLimited,
            safe_message: "The model endpoint is rate limiting Atlas".to_owned(),
            retry_after_seconds: retry_after,
        };
    }
    if detail.contains("context_length")
        || detail.contains("context length")
        || detail.contains("too many tokens")
        || detail.contains("maximum context")
    {
        return provider_error(
            TranslationProviderErrorKind::ContextLength,
            "The translation batch exceeded the model context window",
        );
    }
    if detail.contains("unsupported_api_for_model") {
        return provider_error(
            TranslationProviderErrorKind::Protocol,
            "The selected model does not support chat completions",
        );
    }
    if matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    ) {
        return provider_error(
            TranslationProviderErrorKind::Transport,
            "The model endpoint is temporarily unavailable",
        );
    }
    provider_error(
        if status.is_server_error() {
            TranslationProviderErrorKind::Remote
        } else {
            TranslationProviderErrorKind::Protocol
        },
        format!("The model endpoint returned {status}"),
    )
}

fn retry_after_seconds(headers: &HeaderMap) -> Option<u64> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?;
    value
        .parse::<u64>()
        .ok()
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()?
                .duration_since(SystemTime::now())
                .ok()
                .map(|duration| duration.as_secs().max(1))
        })
        .map(|seconds| seconds.min(60))
}

fn classify_transport(error: reqwest::Error) -> TranslationProviderError {
    provider_error(
        if error.is_timeout() {
            TranslationProviderErrorKind::Timeout
        } else {
            TranslationProviderErrorKind::Transport
        },
        if error.is_timeout() {
            "The model endpoint did not answer in time"
        } else {
            "Atlas could not reach the model endpoint"
        },
    )
}

async fn read_capped_body(
    mut response: Response,
    limit: usize,
    cancellation: &CancellationToken,
    idle_timeout: Duration,
) -> Result<Vec<u8>, TranslationProviderError> {
    let mut body = Vec::new();
    loop {
        let chunk = tokio::select! {
            () = cancellation.cancelled() => {
                return Err(provider_error(
                    TranslationProviderErrorKind::Cancelled,
                    "Translation was cancelled",
                ));
            }
            result = timeout(idle_timeout, response.chunk()) => {
                result
                    .map_err(|_| provider_error(
                        TranslationProviderErrorKind::Timeout,
                        "The model endpoint stalled while returning an error",
                    ))?
                    .map_err(classify_transport)?
            }
        };
        let Some(chunk) = chunk else {
            break;
        };
        if body.len().saturating_add(chunk.len()) > limit {
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn provider_error(
    kind: TranslationProviderErrorKind,
    message: impl Into<String>,
) -> TranslationProviderError {
    TranslationProviderError::new(kind, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_decoder_handles_lines_split_across_byte_chunks() {
        let mut decoder = SseDecoder::default();
        assert!(
            decoder
                .push(br#"data: {"choices":[{"delta":{"content":"{\"id\""#)
                .expect("partial event should buffer")
                .is_empty()
        );
        let events = decoder
            .push(br#":\"a\"}"},"finish_reason":null}]}"#)
            .expect("continued event should buffer");
        assert!(events.is_empty());
        let events = decoder.push(b"\n\n").expect("blank line should flush");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn sse_decoder_joins_multiple_data_fields_at_the_event_boundary() {
        let mut decoder = SseDecoder::default();
        let events = decoder
            .push(
                b"data: {\"choices\":\ndata: [{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            )
            .expect("multi-line event should decode");

        assert_eq!(events.len(), 1);
    }

    #[test]
    fn sse_decoder_accepts_cr_only_line_endings() {
        let mut decoder = SseDecoder::default();
        let events = decoder
            .push(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\r\r",
            )
            .expect("CR-only event should decode");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn sse_decoder_handles_crlf_split_across_chunks() {
        let mut decoder = SseDecoder::default();
        assert!(
            decoder
                .push(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\r",
                )
                .expect("data line should decode")
                .is_empty()
        );
        let events = decoder
            .push(b"\n\r\n")
            .expect("split CRLF event should decode");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn context_errors_are_distinct_from_generic_bad_requests() {
        let error = classify_error_payload(
            StatusCode::BAD_REQUEST,
            &json!({"message":"maximum context length exceeded"}),
            &HeaderMap::new(),
        );
        assert_eq!(error.kind, TranslationProviderErrorKind::ContextLength);
    }

    #[test]
    fn retry_after_accepts_an_http_date() {
        let mut headers = HeaderMap::new();
        headers.insert(
            RETRY_AFTER,
            httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(30))
                .parse()
                .expect("HTTP date should become a header"),
        );

        let error = classify_error_payload(StatusCode::TOO_MANY_REQUESTS, &Value::Null, &headers);

        assert!(
            error
                .retry_after_seconds
                .is_some_and(|seconds| (1..=30).contains(&seconds))
        );
    }
}
