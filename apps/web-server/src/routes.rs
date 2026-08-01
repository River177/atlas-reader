use std::{path::PathBuf, sync::Arc};

use atlas_domain::{
    CommandId, CommandReceipt, ConnectionTestResult, DocumentId, DocumentSummary, ImportPdfResult,
    LibraryPage, LibraryQuery, MineruSettingsInput, OpenSessionInput, OpenSessionResult,
    OpenedReaderDocument, ParseSnapshot, ParsedDocumentView, ProviderKind, PublicProviderSettings,
    ReaderSourceToken, ReadingCommand, ReadingPosition, ReadingPositionUpdate,
    RefreshSourcesResult, SessionId, SessionSnapshot, TranslationSettingsInput,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Extension, Multipart, Path, State},
    http::{
        HeaderValue, StatusCode,
        header::{
            CONTENT_SECURITY_POLICY, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
        },
    },
    middleware,
    response::Response,
    routing::{any, delete, get, post},
};
use serde::Deserialize;
use tower_http::services::{ServeDir, ServeFile};

use crate::{
    app::WebState,
    auth::{self, ClientId},
    error::{ApiError, internal},
    media,
};

pub fn router(state: Arc<WebState>, frontend_dir: PathBuf) -> Router {
    let protected = Router::new()
        .route("/api/bootstrap/session", get(auth::session))
        .route("/api/library/query", post(library_query))
        .route("/api/library/import", post(library_import))
        .route("/api/library/refresh", post(library_refresh))
        .route(
            "/api/library/{document_id}/relocate",
            post(library_relocate),
        )
        .route("/api/library/{document_id}", delete(library_remove))
        .route("/api/reader/open", post(reader_open))
        .route("/api/reader/position", post(reader_position))
        .route("/api/reader/close", post(reader_close))
        .route("/api/parse/{document_id}", get(parse_view))
        .route("/api/parse/{document_id}/retry", post(parse_retry))
        .route("/api/parse/{document_id}/reupload", post(parse_reupload))
        .route("/api/providers", get(provider_get))
        .route("/api/providers/mineru", post(provider_save_mineru))
        .route(
            "/api/providers/translation",
            post(provider_save_translation),
        )
        .route("/api/providers/{provider}/test", post(provider_test))
        .route("/api/providers/{provider}/secret", delete(provider_delete))
        .route("/api/heartbeat", post(heartbeat))
        .route("/api/leases/close", post(close_leases))
        .route("/api/sessions/open", post(session_open))
        .route(
            "/api/sessions/{session_id}",
            get(session_snapshot).delete(session_close),
        )
        .route(
            "/api/sessions/{session_id}/dispatch",
            post(session_dispatch),
        )
        .route("/api/{*path}", any(not_found))
        .layer(DefaultBodyLimit::max(201 * 1024 * 1024))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::protected,
        ));
    let static_files = ServeDir::new(&frontend_dir)
        .not_found_service(ServeFile::new(frontend_dir.join("index.html")));

    Router::new()
        .route("/api/bootstrap/exchange", post(auth::exchange))
        .merge(protected)
        .route("/media/pdf/{token}", get(media::pdf).head(media::pdf))
        .route(
            "/media/artifacts/{document_id}/{artifact_id}/images/{file_name}",
            get(media::asset).head(media::asset),
        )
        .route("/media/{*path}", any(not_found))
        .fallback_service(static_files)
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::host_guard,
        ))
        .with_state(state)
}

async fn not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

async fn security_headers(request: axum::extract::Request, next: middleware::Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; worker-src 'self' blob:; object-src 'none'; frame-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}

async fn library_query(
    State(state): State<Arc<WebState>>,
    Json(input): Json<LibraryQuery>,
) -> Result<Json<LibraryPage>, ApiError> {
    Ok(Json(state.library.query(input).await?))
}

async fn library_import(
    State(state): State<Arc<WebState>>,
    mut multipart: Multipart,
) -> Result<Json<ImportPdfResult>, ApiError> {
    while let Some(field) = multipart.next_field().await.map_err(internal)? {
        if field.name() == Some("file") {
            return Ok(Json(state.import_document(field).await?));
        }
    }
    Err(atlas_domain::AtlasError::invalid_input("PDF upload is missing").into())
}

async fn library_refresh(
    State(state): State<Arc<WebState>>,
) -> Result<Json<RefreshSourcesResult>, ApiError> {
    Ok(Json(state.refresh_library().await?))
}

async fn library_relocate(
    State(state): State<Arc<WebState>>,
    Path(document_id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<DocumentSummary>, ApiError> {
    while let Some(field) = multipart.next_field().await.map_err(internal)? {
        if field.name() == Some("file") {
            return Ok(Json(
                state
                    .relocate_document(DocumentId::from(document_id), field)
                    .await?,
            ));
        }
    }
    Err(atlas_domain::AtlasError::invalid_input("PDF upload is missing").into())
}

async fn library_remove(
    State(state): State<Arc<WebState>>,
    Path(document_id): Path<String>,
) -> Result<(), ApiError> {
    let document_id = DocumentId::from(document_id);
    state.remove_document(document_id).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentInput {
    document_id: DocumentId,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PositionInput {
    source_token: ReaderSourceToken,
    position: ReadingPositionUpdate,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloseReaderInput {
    source_token: ReaderSourceToken,
    final_position: Option<ReadingPositionUpdate>,
}

async fn reader_open(
    State(state): State<Arc<WebState>>,
    Extension(client_id): Extension<ClientId>,
    Json(input): Json<DocumentInput>,
) -> Result<Json<OpenedReaderDocument>, ApiError> {
    Ok(Json(state.open_reader(client_id, input.document_id).await?))
}

async fn reader_position(
    State(state): State<Arc<WebState>>,
    Extension(client_id): Extension<ClientId>,
    Json(input): Json<PositionInput>,
) -> Result<Json<ReadingPosition>, ApiError> {
    state
        .touch_reader(&input.source_token, Some(&client_id))
        .await;
    Ok(Json(
        state
            .document_reader
            .save_position(&input.source_token, input.position)
            .await?,
    ))
}

async fn reader_close(
    State(state): State<Arc<WebState>>,
    Extension(client_id): Extension<ClientId>,
    Json(input): Json<CloseReaderInput>,
) -> Result<(), ApiError> {
    state
        .document_reader
        .close(&input.source_token, input.final_position)
        .await?;
    state
        .unregister_reader(&client_id, &input.source_token)
        .await;
    Ok(())
}

async fn parse_view(
    State(state): State<Arc<WebState>>,
    Path(document_id): Path<String>,
) -> Result<Json<ParsedDocumentView>, ApiError> {
    Ok(Json(
        state.parse.view(&DocumentId::from(document_id)).await?,
    ))
}

async fn parse_retry(
    State(state): State<Arc<WebState>>,
    Path(document_id): Path<String>,
) -> Result<Json<ParseSnapshot>, ApiError> {
    Ok(Json(
        state.retry_parse(&DocumentId::from(document_id)).await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReuploadInput {
    session_id: SessionId,
}

async fn parse_reupload(
    State(state): State<Arc<WebState>>,
    Path(document_id): Path<String>,
    Json(input): Json<ReuploadInput>,
) -> Result<Json<ParseSnapshot>, ApiError> {
    Ok(Json(
        state
            .reupload_parse(DocumentId::from(document_id), input.session_id)
            .await?,
    ))
}

async fn provider_get(
    State(state): State<Arc<WebState>>,
) -> Result<Json<PublicProviderSettings>, ApiError> {
    Ok(Json(state.provider_settings.get().await?))
}

async fn provider_save_mineru(
    State(state): State<Arc<WebState>>,
    Json(input): Json<MineruSettingsInput>,
) -> Result<Json<ConnectionTestResult>, ApiError> {
    Ok(Json(state.provider_settings.save_mineru(input).await?))
}

async fn provider_save_translation(
    State(state): State<Arc<WebState>>,
    Json(input): Json<TranslationSettingsInput>,
) -> Result<Json<ConnectionTestResult>, ApiError> {
    Ok(Json(state.provider_settings.save_translation(input).await?))
}

async fn provider_test(
    State(state): State<Arc<WebState>>,
    Path(provider): Path<String>,
) -> Result<Json<ConnectionTestResult>, ApiError> {
    Ok(Json(
        state
            .provider_settings
            .test(provider_kind(&provider)?)
            .await?,
    ))
}

async fn provider_delete(
    State(state): State<Arc<WebState>>,
    Path(provider): Path<String>,
) -> Result<(), ApiError> {
    state
        .provider_settings
        .delete_secret(provider_kind(&provider)?)
        .await?;
    Ok(())
}

async fn session_open(
    State(state): State<Arc<WebState>>,
    Extension(client_id): Extension<ClientId>,
    Json(input): Json<OpenSessionInput>,
) -> Result<Json<OpenSessionResult>, ApiError> {
    Ok(Json(state.open_session(client_id, input).await?))
}

async fn session_snapshot(
    State(state): State<Arc<WebState>>,
    Extension(client_id): Extension<ClientId>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionSnapshot>, ApiError> {
    let session_id = SessionId::from(session_id);
    state.touch_session(&client_id, &session_id).await;
    Ok(Json(state.reading_session.snapshot(&session_id).await?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DispatchInput {
    command_id: CommandId,
    expected_revision: Option<u32>,
    command: ReadingCommand,
}

async fn session_dispatch(
    State(state): State<Arc<WebState>>,
    Extension(client_id): Extension<ClientId>,
    Path(session_id): Path<String>,
    Json(input): Json<DispatchInput>,
) -> Result<Json<CommandReceipt>, ApiError> {
    let session_id = SessionId::from(session_id);
    state.touch_session(&client_id, &session_id).await;
    Ok(Json(
        state
            .reading_session
            .dispatch(
                &session_id,
                input.command_id,
                input.expected_revision,
                input.command,
            )
            .await?,
    ))
}

async fn session_close(
    State(state): State<Arc<WebState>>,
    Extension(client_id): Extension<ClientId>,
    Path(session_id): Path<String>,
) -> Result<(), ApiError> {
    let session_id = SessionId::from(session_id);
    state.reading_session.close(&session_id).await?;
    state.unregister_session(&client_id, &session_id).await;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeartbeatInput {
    reader_source_tokens: Vec<ReaderSourceToken>,
    session_ids: Vec<SessionId>,
}

async fn heartbeat(
    State(state): State<Arc<WebState>>,
    Extension(client_id): Extension<ClientId>,
    Json(input): Json<HeartbeatInput>,
) {
    state
        .heartbeat(&client_id, &input.reader_source_tokens, &input.session_ids)
        .await;
}

async fn close_leases(
    State(state): State<Arc<WebState>>,
    Extension(client_id): Extension<ClientId>,
    Json(input): Json<HeartbeatInput>,
) -> Result<(), ApiError> {
    state
        .close_client_leases(&client_id, input.reader_source_tokens, input.session_ids)
        .await
}

fn provider_kind(value: &str) -> Result<ProviderKind, ApiError> {
    match value {
        "mineru" => Ok(ProviderKind::Mineru),
        "translation" => Ok(ProviderKind::Translation),
        _ => Err(atlas_domain::AtlasError::invalid_input("provider is invalid").into()),
    }
}
