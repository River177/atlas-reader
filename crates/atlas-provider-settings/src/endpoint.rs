use atlas_domain::{ConnectionTestCode, ConnectionTestResult, ProviderKind};
use sha2::{Digest, Sha256};
use url::{Host, Url};

/// Bumping this value re-fingerprints every endpoint, which invalidates the
/// binding between stored parse operations and their provider profile.
pub const ADAPTER_PROTOCOL_VERSION: &str = "1";

/// A provider endpoint reduced to the form Atlas stores and compares.
///
/// Query strings, fragments, credentials, default ports, and trailing slashes
/// are removed so that cosmetically different user input produces one identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedEndpoint {
    origin: String,
    base_path: String,
    fingerprint: String,
}

impl NormalizedEndpoint {
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Empty, or a path that starts with `/` and never ends with one.
    #[must_use]
    pub fn base_path(&self) -> &str {
        &self.base_path
    }

    /// `SHA-256(provider kind + normalized url + adapter protocol version)`.
    /// The API key is deliberately excluded so rotating a key keeps the binding.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    #[must_use]
    pub fn url(&self) -> String {
        format!("{}{}", self.origin, self.base_path)
    }

    /// Appends an adapter-defined relative path to the normalized endpoint.
    #[must_use]
    pub fn join(&self, relative: &str) -> String {
        format!("{}/{}", self.url(), relative.trim_start_matches('/'))
    }

    #[must_use]
    pub fn restore(kind: ProviderKind, origin: String, base_path: String) -> Self {
        let fingerprint = fingerprint(kind, &origin, &base_path);
        Self {
            origin,
            base_path,
            fingerprint,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EndpointError {
    InvalidUrl(String),
    InsecureRemoteUrl(String),
}

impl EndpointError {
    #[must_use]
    pub fn into_test_result(self) -> ConnectionTestResult {
        match self {
            Self::InvalidUrl(message) => {
                ConnectionTestResult::failed(ConnectionTestCode::InvalidUrl, message)
            }
            Self::InsecureRemoteUrl(message) => {
                ConnectionTestResult::failed(ConnectionTestCode::InsecureRemoteUrl, message)
            }
        }
    }
}

/// Turns raw user input into a stored endpoint identity.
///
/// A missing scheme defaults to HTTPS. Plain HTTP is accepted only for loopback
/// hosts, which keeps local model servers usable without weakening remote
/// traffic.
pub fn normalize(kind: ProviderKind, raw: &str) -> Result<NormalizedEndpoint, EndpointError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(EndpointError::InvalidUrl(
            "Enter an endpoint URL".to_owned(),
        ));
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let parsed = Url::parse(&candidate).map_err(|error| {
        EndpointError::InvalidUrl(format!("The endpoint URL is invalid: {error}"))
    })?;

    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(EndpointError::InvalidUrl(format!(
            "The endpoint must use HTTP or HTTPS, not {scheme}"
        )));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(EndpointError::InvalidUrl(
            "The endpoint URL must not embed a user name or password".to_owned(),
        ));
    }
    let Some(host) = parsed.host() else {
        return Err(EndpointError::InvalidUrl(
            "The endpoint URL is missing a host".to_owned(),
        ));
    };
    if scheme == "http" && !is_loopback(&host) {
        return Err(EndpointError::InsecureRemoteUrl(
            "Remote endpoints must use HTTPS; plain HTTP is allowed only for localhost, 127.0.0.1, and ::1"
                .to_owned(),
        ));
    }

    let host_text = match host {
        Host::Domain(domain) => domain.to_ascii_lowercase(),
        Host::Ipv4(address) => address.to_string(),
        Host::Ipv6(address) => format!("[{address}]"),
    };
    let origin = match parsed.port() {
        Some(port) => format!("{scheme}://{host_text}:{port}"),
        None => format!("{scheme}://{host_text}"),
    };
    let base_path = normalize_path(parsed.path());
    let fingerprint = fingerprint(kind, &origin, &base_path);

    Ok(NormalizedEndpoint {
        origin,
        base_path,
        fingerprint,
    })
}

fn normalize_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "/" {
        String::new()
    } else {
        trimmed.to_owned()
    }
}

fn is_loopback(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => {
            let lowered = domain.to_ascii_lowercase();
            lowered == "localhost" || lowered.ends_with(".localhost")
        }
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    }
}

fn fingerprint(kind: ProviderKind, origin: &str, base_path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_str().as_bytes());
    hasher.update(b"\n");
    hasher.update(origin.as_bytes());
    hasher.update(base_path.as_bytes());
    hasher.update(b"\n");
    hasher.update(ADAPTER_PROTOCOL_VERSION.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosmetic_differences_normalize_to_one_identity() {
        let variants = [
            "https://MinerU.example.com/api/v4/",
            "https://mineru.example.com:443/api/v4",
            "  https://mineru.example.com/api/v4?token=x#frag  ",
            "MinerU.example.com/api/v4//",
        ];

        let normalized: Vec<_> = variants
            .iter()
            .map(|raw| normalize(ProviderKind::Mineru, raw).expect("endpoint should normalize"))
            .collect();

        for endpoint in &normalized {
            assert_eq!(endpoint.origin(), "https://mineru.example.com");
            assert_eq!(endpoint.base_path(), "/api/v4");
            assert_eq!(endpoint.url(), "https://mineru.example.com/api/v4");
            assert_eq!(endpoint.fingerprint(), normalized[0].fingerprint());
        }
    }

    #[test]
    fn loopback_may_use_http_but_remote_hosts_may_not() {
        for raw in [
            "http://localhost:8000/v1",
            "http://127.0.0.1:1234/v1",
            "http://[::1]:1234/v1",
        ] {
            let endpoint =
                normalize(ProviderKind::Translation, raw).expect("loopback HTTP should normalize");
            assert!(endpoint.origin().starts_with("http://"));
        }

        assert_eq!(
            normalize(ProviderKind::Translation, "http://models.example.com/v1"),
            Err(EndpointError::InsecureRemoteUrl(
                "Remote endpoints must use HTTPS; plain HTTP is allowed only for localhost, 127.0.0.1, and ::1"
                    .to_owned()
            ))
        );
    }

    #[test]
    fn malformed_and_unsafe_urls_are_rejected() {
        for raw in [
            "",
            "   ",
            "ftp://mineru.example.com",
            "https://user:pass@mineru.example.com",
            "https://",
        ] {
            assert!(
                matches!(
                    normalize(ProviderKind::Mineru, raw),
                    Err(EndpointError::InvalidUrl(_))
                ),
                "{raw} should be rejected"
            );
        }
    }

    #[test]
    fn fingerprints_separate_providers_paths_and_ports() {
        let mineru = normalize(ProviderKind::Mineru, "https://a.example.com/api")
            .expect("endpoint should normalize");
        let translation = normalize(ProviderKind::Translation, "https://a.example.com/api")
            .expect("endpoint should normalize");
        let other_path = normalize(ProviderKind::Mineru, "https://a.example.com/apiv2")
            .expect("endpoint should normalize");
        let other_port = normalize(ProviderKind::Mineru, "https://a.example.com:8443/api")
            .expect("endpoint should normalize");

        assert_ne!(mineru.fingerprint(), translation.fingerprint());
        assert_ne!(mineru.fingerprint(), other_path.fingerprint());
        assert_ne!(mineru.fingerprint(), other_port.fingerprint());
        assert_eq!(
            mineru.fingerprint(),
            NormalizedEndpoint::restore(
                ProviderKind::Mineru,
                mineru.origin().to_owned(),
                mineru.base_path().to_owned(),
            )
            .fingerprint()
        );
    }

    #[test]
    fn join_builds_adapter_paths_without_duplicate_separators() {
        let endpoint = normalize(ProviderKind::Mineru, "https://mineru.example.com/api/v4")
            .expect("endpoint should normalize");

        assert_eq!(
            endpoint.join("extract/task"),
            "https://mineru.example.com/api/v4/extract/task"
        );
        assert_eq!(
            endpoint.join("/extract/task"),
            "https://mineru.example.com/api/v4/extract/task"
        );
    }
}
