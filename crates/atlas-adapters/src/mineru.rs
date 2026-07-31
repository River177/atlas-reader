use std::{path::Path, time::Duration};

use async_trait::async_trait;
use atlas_domain::AtlasError;
use atlas_parse::{
    CancelCapability, CloudParseError, CloudParseErrorKind, CloudParseProgress, CloudParseRequest,
    CloudParseStatus, CloudParseSubmission, CloudParserPort,
};
use reqwest::{Client, Response, StatusCode, Url};
use serde::Serialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::connection_probe::same_origin_redirects;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const API_TIMEOUT: Duration = Duration::from_secs(15);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_API_BODY_BYTES: usize = 1 << 20;

#[derive(Clone, Debug)]
pub struct MineruCloudHttpAdapter {
    client: Client,
}

impl MineruCloudHttpAdapter {
    pub fn new() -> Result<Self, AtlasError> {
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(same_origin_redirects(3))
            .user_agent(concat!("AtlasReader/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                AtlasError::internal(format!("The Cloud MinerU client could not start: {error}"))
            })?;
        Ok(Self { client })
    }

    fn api_url(request: &CloudParseRequest, path: &str) -> Result<Url, CloudParseError> {
        let value = format!(
            "{}/{}",
            request.endpoint_base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let url = Url::parse(&value).map_err(|_| {
            CloudParseError::new(
                CloudParseErrorKind::Protocol,
                "The saved Cloud MinerU endpoint is invalid",
            )
        })?;
        if secure_or_loopback(&url) {
            Ok(url)
        } else {
            Err(CloudParseError::new(
                CloudParseErrorKind::Protocol,
                "Cloud MinerU must use HTTPS unless it is a loopback test endpoint",
            ))
        }
    }

    async fn api_json(response: Response, action: &str) -> Result<Value, CloudParseError> {
        let status = response.status();
        let body = read_capped(response, MAX_API_BODY_BYTES).await?;
        classify_http(status, action)?;
        serde_json::from_slice(&body).map_err(|_| {
            CloudParseError::new(
                CloudParseErrorKind::Protocol,
                format!("Cloud MinerU returned an unreadable {action} response"),
            )
        })
    }
}

#[derive(Serialize)]
struct BatchRequest<'a> {
    files: [BatchFile<'a>; 1],
    model_version: &'a str,
    enable_formula: bool,
    enable_table: bool,
    language: &'a str,
}

#[derive(Serialize)]
struct BatchFile<'a> {
    name: &'a str,
    data_id: &'a str,
    is_ocr: bool,
}

#[async_trait]
impl CloudParserPort for MineruCloudHttpAdapter {
    async fn request_upload(
        &self,
        request: &CloudParseRequest,
    ) -> Result<CloudParseSubmission, CloudParseError> {
        let url = Self::api_url(request, "file-urls/batch")?;
        let body = BatchRequest {
            files: [BatchFile {
                name: &request.file_name,
                data_id: &request.data_id,
                is_ocr: false,
            }],
            model_version: &request.model_version,
            enable_formula: true,
            enable_table: true,
            language: &request.language,
        };
        let response = self
            .client
            .post(url)
            .bearer_auth(request.credential.expose())
            .header("Accept", "application/json")
            .json(&body)
            .timeout(API_TIMEOUT)
            .send()
            .await
            .map_err(|error| request_transport(error, true))?;
        let payload = Self::api_json(response, "batch allocation").await?;
        ensure_business_success(&payload, "batch allocation")?;
        let data = payload
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_error("Cloud MinerU omitted batch allocation data"))?;
        let batch_id = data
            .get("batch_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| protocol_error("Cloud MinerU omitted the batch id"))?;
        let urls = data
            .get("file_urls")
            .and_then(Value::as_array)
            .filter(|urls| urls.len() == 1)
            .ok_or_else(|| {
                protocol_error("Cloud MinerU returned the wrong number of upload URLs")
            })?;
        let upload_url = urls[0]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| protocol_error("Cloud MinerU returned an invalid upload URL"))?;
        let parsed = Url::parse(upload_url)
            .map_err(|_| protocol_error("Cloud MinerU returned an invalid upload URL"))?;
        if !secure_or_loopback(&parsed) {
            return Err(protocol_error(
                "Cloud MinerU returned an insecure upload URL",
            ));
        }
        Ok(CloudParseSubmission {
            batch_id: batch_id.to_owned(),
            data_id: request.data_id.clone(),
            upload_url: upload_url.to_owned(),
        })
    }

    async fn upload(
        &self,
        submission: &CloudParseSubmission,
        file_path: &Path,
    ) -> Result<(), CloudParseError> {
        let url = Url::parse(&submission.upload_url)
            .map_err(|_| protocol_error("The persisted upload URL is invalid"))?;
        if !secure_or_loopback(&url) {
            return Err(protocol_error("The persisted upload URL is insecure"));
        }
        let file = tokio::fs::File::open(file_path).await.map_err(|_| {
            CloudParseError::new(
                CloudParseErrorKind::Transport,
                "Atlas could not open the PDF for upload",
            )
        })?;
        let stream = ReaderStream::new(file);
        // `Body::wrap_stream` does not synthesize Content-Type. Do not replace
        // this with json/form or add that header: the OSS signature requires an
        // empty Content-Type value.
        let response = self
            .client
            .put(url)
            .body(reqwest::Body::wrap_stream(stream))
            .timeout(TRANSFER_TIMEOUT)
            .send()
            .await
            .map_err(|error| request_transport(error, true))?;
        let status = response.status();
        if status == StatusCode::OK || status == StatusCode::CREATED {
            Ok(())
        } else if status.is_server_error() {
            Err(CloudParseError::unknown_upload(
                "Atlas could not confirm whether the PDF upload completed",
            ))
        } else {
            classify_http(status, "PDF upload")
        }
    }

    async fn status(
        &self,
        request: &CloudParseRequest,
        batch_id: &str,
    ) -> Result<CloudParseStatus, CloudParseError> {
        if batch_id.trim().is_empty() {
            return Err(protocol_error("Cloud MinerU batch id is empty"));
        }
        let url = Self::api_url(request, &format!("extract-results/batch/{batch_id}"))?;
        let response = self
            .client
            .get(url)
            .bearer_auth(request.credential.expose())
            .header("Accept", "application/json")
            .timeout(API_TIMEOUT)
            .send()
            .await
            .map_err(|error| request_transport(error, false))?;
        let payload = Self::api_json(response, "status").await?;
        let code = payload.get("code").and_then(Value::as_i64);
        if code == Some(-60012) {
            return Ok(CloudParseStatus::Missing);
        }
        ensure_business_success(&payload, "status")?;
        let results = payload
            .pointer("/data/extract_result")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol_error("Cloud MinerU omitted the batch result list"))?;
        let Some(result) = results.iter().find(|result| {
            result.get("data_id").and_then(Value::as_str) == Some(request.data_id.as_str())
        }) else {
            return Ok(CloudParseStatus::Missing);
        };
        match result.get("state").and_then(Value::as_str) {
            Some("pending") => Ok(CloudParseStatus::Pending),
            Some("running") => {
                let progress = result.get("extract_progress");
                let extracted_pages =
                    u32_value(progress.and_then(|value| value.get("extracted_pages"))).unwrap_or(0);
                let total_pages =
                    u32_value(progress.and_then(|value| value.get("total_pages"))).unwrap_or(0);
                Ok(CloudParseStatus::Running(CloudParseProgress {
                    extracted_pages,
                    total_pages,
                }))
            }
            Some("done") => {
                let download_url = result
                    .get("full_zip_url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        protocol_error("Cloud MinerU omitted the completed artifact URL")
                    })?;
                let parsed = Url::parse(download_url)
                    .map_err(|_| protocol_error("Cloud MinerU returned an invalid artifact URL"))?;
                if !secure_or_loopback(&parsed) {
                    return Err(protocol_error(
                        "Cloud MinerU returned an insecure artifact URL",
                    ));
                }
                Ok(CloudParseStatus::Done {
                    download_url: download_url.to_owned(),
                })
            }
            Some("failed") => Ok(CloudParseStatus::Failed {
                safe_message: "Cloud MinerU could not parse this PDF".to_owned(),
            }),
            _ => Err(protocol_error(
                "Cloud MinerU returned an unknown parse state",
            )),
        }
    }

    async fn download(
        &self,
        download_url: &str,
        destination: &Path,
        max_bytes: u64,
    ) -> Result<u64, CloudParseError> {
        let url = Url::parse(download_url)
            .map_err(|_| protocol_error("The completed artifact URL is invalid"))?;
        if !secure_or_loopback(&url) {
            return Err(protocol_error("The completed artifact URL is insecure"));
        }
        let mut response = self
            .client
            .get(url)
            .header("Accept", "application/zip")
            .timeout(TRANSFER_TIMEOUT)
            .send()
            .await
            .map_err(|error| request_transport(error, false))?;
        classify_http(response.status(), "artifact download")?;
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes)
        {
            return Err(CloudParseError::new(
                CloudParseErrorKind::DownloadTooLarge,
                "Cloud MinerU artifact exceeds Atlas's download limit",
            ));
        }
        let mut output = tokio::fs::File::create(destination).await.map_err(|_| {
            CloudParseError::new(
                CloudParseErrorKind::Transport,
                "Atlas could not create the temporary artifact file",
            )
        })?;
        let mut written = 0_u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| request_transport(error, false))?
        {
            written = written.checked_add(chunk.len() as u64).ok_or_else(|| {
                CloudParseError::new(
                    CloudParseErrorKind::DownloadTooLarge,
                    "Cloud MinerU artifact size overflowed Atlas's limit",
                )
            })?;
            if written > max_bytes {
                drop(output);
                let _ = tokio::fs::remove_file(destination).await;
                return Err(CloudParseError::new(
                    CloudParseErrorKind::DownloadTooLarge,
                    "Cloud MinerU artifact exceeds Atlas's download limit",
                ));
            }
            output.write_all(&chunk).await.map_err(|_| {
                CloudParseError::new(
                    CloudParseErrorKind::Transport,
                    "Atlas could not write the temporary artifact file",
                )
            })?;
        }
        output.sync_all().await.map_err(|_| {
            CloudParseError::new(
                CloudParseErrorKind::Transport,
                "Atlas could not finish the temporary artifact file",
            )
        })?;
        Ok(written)
    }

    async fn cancel(
        &self,
        _request: &CloudParseRequest,
        _batch_id: &str,
    ) -> Result<CancelCapability, CloudParseError> {
        Ok(CancelCapability::Unsupported)
    }
}

async fn read_capped(mut response: Response, max: usize) -> Result<Vec<u8>, CloudParseError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| request_transport(error, false))?
    {
        if body.len().saturating_add(chunk.len()) > max {
            return Err(protocol_error(
                "Cloud MinerU response exceeded Atlas's limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn ensure_business_success(payload: &Value, action: &str) -> Result<(), CloudParseError> {
    if gateway_rejected_key(payload) {
        return Err(CloudParseError::new(
            CloudParseErrorKind::Unauthorized,
            "Cloud MinerU rejected the API key",
        ));
    }
    match payload.get("code").and_then(Value::as_i64) {
        Some(0) => Ok(()),
        Some(_) => Err(CloudParseError::new(
            CloudParseErrorKind::Remote,
            format!("Cloud MinerU rejected the {action} request"),
        )),
        None => Err(protocol_error(
            "Cloud MinerU returned an unrecognized response envelope",
        )),
    }
}

fn gateway_rejected_key(payload: &Value) -> bool {
    payload.get("success").and_then(Value::as_bool) == Some(false)
        && payload
            .get("msgCode")
            .and_then(Value::as_str)
            .is_some_and(|code| code.starts_with("A02"))
}

fn classify_http(status: StatusCode, action: &str) -> Result<(), CloudParseError> {
    if status.is_success() {
        return Ok(());
    }
    let (kind, message) = if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        (
            CloudParseErrorKind::Unauthorized,
            "Cloud MinerU rejected the API key".to_owned(),
        )
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        (
            CloudParseErrorKind::RateLimited,
            "Cloud MinerU is rate limiting this key".to_owned(),
        )
    } else if status.is_server_error() {
        (
            CloudParseErrorKind::Remote,
            format!("Cloud MinerU could not complete {action}"),
        )
    } else {
        (
            CloudParseErrorKind::Protocol,
            format!("Cloud MinerU rejected {action}"),
        )
    };
    Err(CloudParseError::new(kind, message))
}

fn request_transport(error: reqwest::Error, upload_outcome_unknown: bool) -> CloudParseError {
    if upload_outcome_unknown {
        return CloudParseError::unknown_upload(
            "Atlas lost the Cloud MinerU response before it could confirm the upload",
        );
    }
    CloudParseError::new(
        if error.is_timeout() {
            CloudParseErrorKind::Timeout
        } else {
            CloudParseErrorKind::Transport
        },
        if error.is_timeout() {
            "Cloud MinerU timed out"
        } else {
            "Cloud MinerU is unreachable"
        },
    )
}

fn protocol_error(message: impl Into<String>) -> CloudParseError {
    CloudParseError::new(CloudParseErrorKind::Protocol, message)
}

fn secure_or_loopback(url: &Url) -> bool {
    url.scheme() == "https"
        || (url.scheme() == "http"
            && url
                .host_str()
                .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1")))
}

fn u32_value(value: Option<&Value>) -> Option<u32> {
    u32::try_from(value?.as_u64()?).ok()
}
