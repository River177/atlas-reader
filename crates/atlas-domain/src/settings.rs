use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum ProviderKind {
    Mineru,
    Translation,
}

impl ProviderKind {
    /// Stable identifier used for database rows, secret accounts, and endpoint
    /// fingerprints. It must never change for an existing installation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mineru => "cloud_mineru",
            Self::Translation => "openai_compatible",
        }
    }

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Mineru => "Cloud MinerU",
            Self::Translation => "Translation model",
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum ConnectionTestCode {
    Ok,
    NotConfigured,
    InvalidUrl,
    InsecureRemoteUrl,
    DnsFailed,
    TlsFailed,
    Unauthorized,
    RateLimited,
    ProtocolIncompatible,
    ServerError,
    Unreachable,
    Timeout,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct ConnectionTestResult {
    pub ok: bool,
    pub code: ConnectionTestCode,
    pub message: String,
}

impl ConnectionTestResult {
    #[must_use]
    pub fn passed(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            code: ConnectionTestCode::Ok,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn failed(code: ConnectionTestCode, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            code,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct PublicProviderSettings {
    pub mineru_endpoint: Option<String>,
    pub mineru_has_secret: bool,
    pub mineru_automatic_cloud_parsing_enabled: bool,
    pub translation_base_url: Option<String>,
    pub translation_model_id: Option<String>,
    pub translation_has_secret: bool,
    pub context_window_override: Option<u32>,
}

#[derive(Clone, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct MineruSettingsInput {
    pub endpoint: String,
    /// Omitted when the caller keeps the stored key. Never logged or returned.
    pub api_key: Option<String>,
    pub automatic_cloud_parsing_enabled: bool,
}

impl fmt::Debug for MineruSettingsInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MineruSettingsInput")
            .field("endpoint", &self.endpoint)
            .field("api_key", &redacted(self.api_key.as_deref()))
            .field(
                "automatic_cloud_parsing_enabled",
                &self.automatic_cloud_parsing_enabled,
            )
            .finish()
    }
}

#[derive(Clone, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct TranslationSettingsInput {
    pub base_url: String,
    /// Omitted when the caller keeps the stored key. Never logged or returned.
    pub api_key: Option<String>,
    pub model_id: String,
    pub context_window_override: Option<u32>,
}

impl fmt::Debug for TranslationSettingsInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TranslationSettingsInput")
            .field("base_url", &self.base_url)
            .field("api_key", &redacted(self.api_key.as_deref()))
            .field("model_id", &self.model_id)
            .field("context_window_override", &self.context_window_override)
            .finish()
    }
}

fn redacted(secret: Option<&str>) -> &'static str {
    if secret.is_some() {
        "<redacted>"
    } else {
        "<unset>"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_the_api_key() {
        let mineru = MineruSettingsInput {
            endpoint: "https://mineru.example.com".to_owned(),
            api_key: Some("super-secret-value".to_owned()),
            automatic_cloud_parsing_enabled: true,
        };
        let translation = TranslationSettingsInput {
            base_url: "https://models.example.com/v1".to_owned(),
            api_key: Some("super-secret-value".to_owned()),
            model_id: "gpt-4o-mini".to_owned(),
            context_window_override: Some(128_000),
        };

        let rendered = format!("{mineru:?} {translation:?}");

        assert!(!rendered.contains("super-secret-value"));
        assert!(rendered.contains("<redacted>"));
    }
}
