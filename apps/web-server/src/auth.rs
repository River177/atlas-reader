use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use atlas_domain::AtlasError;
use axum::{
    Json,
    extract::{ConnectInfo, Request, State},
    http::{
        HeaderMap, Method, StatusCode,
        header::{AUTHORIZATION, HOST, ORIGIN},
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{app::WebState, error::ApiError};

#[derive(Debug)]
pub struct AuthState {
    launch_token: Mutex<Option<String>>,
    session_token: String,
    csrf_token: String,
    resource_token: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ClientId(pub String);

impl AuthState {
    pub fn new() -> Self {
        Self {
            launch_token: Mutex::new(Some(random_token())),
            session_token: random_token(),
            csrf_token: random_token(),
            resource_token: random_token(),
        }
    }

    pub async fn launch_token(&self) -> Option<String> {
        self.launch_token.lock().await.clone()
    }

    fn session_matches(&self, headers: &HeaderMap) -> bool {
        headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|value| value == self.session_token)
    }

    pub fn resource_matches(&self, value: &str) -> bool {
        value == self.resource_token
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapInput {
    launch_token: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapOutput {
    access_token: String,
    csrf_token: String,
    resource_token: String,
}

pub async fn exchange(
    State(state): State<Arc<WebState>>,
    Json(input): Json<BootstrapInput>,
) -> Result<Response, ApiError> {
    let mut launch = state.auth.launch_token.lock().await;
    if launch.as_deref() != Some(input.launch_token.as_str()) {
        return Err(ApiError(AtlasError::invalid_input(
            "Atlas launch token is invalid or expired",
        )));
    }
    launch.take();
    Ok(Json(BootstrapOutput {
        access_token: state.auth.session_token.clone(),
        csrf_token: state.auth.csrf_token.clone(),
        resource_token: state.auth.resource_token.clone(),
    })
    .into_response())
}

pub async fn session(State(state): State<Arc<WebState>>) -> Json<BootstrapOutput> {
    Json(BootstrapOutput {
        access_token: state.auth.session_token.clone(),
        csrf_token: state.auth.csrf_token.clone(),
        resource_token: state.auth.resource_token.clone(),
    })
}

pub async fn host_guard(
    State(state): State<Arc<WebState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if !loopback(peer.ip())
        || request
            .headers()
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            != Some(state.authority.as_str())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

pub async fn protected(
    State(state): State<Arc<WebState>>,
    mut request: Request,
    next: Next,
) -> Response {
    if !state.auth.session_matches(request.headers()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if request.method() != Method::GET && request.method() != Method::HEAD {
        let origin = request
            .headers()
            .get(ORIGIN)
            .and_then(|value| value.to_str().ok());
        let csrf = request
            .headers()
            .get("x-atlas-csrf")
            .and_then(|value| value.to_str().ok());
        let fetch_site = request
            .headers()
            .get("sec-fetch-site")
            .and_then(|value| value.to_str().ok());
        if origin != Some(state.origin.as_str())
            || csrf != Some(state.auth.csrf_token.as_str())
            || fetch_site.is_some_and(|value| value != "same-origin")
        {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    let Some(client_id) = request
        .headers()
        .get("x-atlas-client")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            value.len() <= 64
                && !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        .map(str::to_owned)
    else {
        return StatusCode::FORBIDDEN.into_response();
    };
    request.extensions_mut().insert(ClientId(client_id));
    next.run(request).await
}

fn random_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

pub fn loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_parser_never_accepts_a_similar_scheme() {
        let auth = AuthState::new();
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            format!("Basic {}", auth.session_token)
                .parse()
                .expect("header"),
        );
        assert!(!auth.session_matches(&headers));
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {}", auth.session_token)
                .parse()
                .expect("header"),
        );
        assert!(auth.session_matches(&headers));
    }

    #[test]
    fn only_loopback_peers_are_accepted() {
        assert!(loopback("127.0.0.1".parse().expect("loopback")));
        assert!(!loopback("192.0.2.1".parse().expect("test address")));
    }
}
