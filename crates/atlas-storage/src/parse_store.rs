use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use atlas_domain::{AtlasError, CanonicalDocument, DocumentId};
use atlas_parse::{
    CLOUD_PARSER_VERSION, NORMALIZER_VERSION, ParseOperation, ParseOperationState, ParseStore,
    PublishArtifact,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

use crate::{AtlasDatabase, map_sqlx, to_i64};

#[derive(Clone, Debug)]
pub struct SqliteParseStore {
    pool: SqlitePool,
    artifact_root: PathBuf,
}

impl SqliteParseStore {
    #[must_use]
    pub fn new(database: &AtlasDatabase, artifact_root: PathBuf) -> Self {
        Self {
            pool: database.pool().clone(),
            artifact_root,
        }
    }

    async fn save_operation_in(
        transaction: &mut Transaction<'_, Sqlite>,
        operation: &ParseOperation,
    ) -> Result<(), AtlasError> {
        let previous_state =
            sqlx::query_scalar::<_, String>("SELECT state FROM parse_operations WHERE id = ?1")
                .bind(&operation.id)
                .fetch_optional(&mut **transaction)
                .await
                .map_err(map_sqlx)?;
        let input_json = json!({
            "parseOperationId": operation.id,
            "backend": operation.backend,
            "dataId": operation.data_id,
        })
        .to_string();
        let checkpoint_json = json!({
            "parseOperationId": operation.id,
            "backend": operation.backend,
            "batchId": operation.batch_id,
            "state": operation.state.as_str(),
        })
        .to_string();
        let result_json = (operation.state == ParseOperationState::Succeeded)
            .then(|| json!({ "parseOperationId": operation.id }).to_string());
        let started_at = (operation.state != ParseOperationState::Queued)
            .then_some(to_i64(operation.updated_at, "job start time")?);

        sqlx::query(
            "INSERT INTO jobs (
               id, session_id, document_id, kind, priority, state, input_json,
               checkpoint_json, error_code, attempt_count, max_attempts, run_after,
               created_at, updated_at, completed_at, remote_job_id, result_json,
               error_safe_json, started_at
             ) VALUES (
               ?1, ?2, ?3, 'cloud_parse', 100, ?4, ?5, ?6, ?7, ?8, 3, ?9,
               ?10, ?11, ?12, ?13, ?14, ?15, ?16
             )
             ON CONFLICT(id) DO UPDATE SET
               state = excluded.state,
               checkpoint_json = excluded.checkpoint_json,
               error_code = excluded.error_code,
               error_safe_json = excluded.error_safe_json,
               attempt_count = excluded.attempt_count,
               run_after = excluded.run_after,
               updated_at = excluded.updated_at,
               completed_at = excluded.completed_at,
               remote_job_id = excluded.remote_job_id,
               result_json = excluded.result_json,
               started_at = COALESCE(jobs.started_at, excluded.started_at)",
        )
        .bind(&operation.job_id)
        .bind(&operation.session_id)
        .bind(operation.document_id.as_str())
        .bind(operation.state.job_state())
        .bind(input_json)
        .bind(checkpoint_json)
        .bind(&operation.error_code)
        .bind(i64::from(operation.retry_count))
        .bind(to_i64(operation.updated_at, "job run time")?)
        .bind(to_i64(operation.created_at, "job creation time")?)
        .bind(to_i64(operation.updated_at, "job update time")?)
        .bind(
            operation
                .completed_at
                .map(|value| to_i64(value, "job completion time"))
                .transpose()?,
        )
        .bind(&operation.batch_id)
        .bind(result_json)
        .bind(&operation.error_safe_json)
        .bind(started_at)
        .execute(&mut **transaction)
        .await
        .map_err(map_sqlx)?;

        sqlx::query(
            "INSERT INTO parse_operations (
               id, job_id, document_id, provider_profile_id, backend,
               parser_version, normalizer_version, endpoint_origin,
               endpoint_fingerprint, state, progress, data_id, batch_id,
               remote_upload_url, remote_download_url, remote_status_json,
               retry_count, error_code, error_safe_json, created_at, updated_at,
               completed_at
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
               ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22
             )
             ON CONFLICT(id) DO UPDATE SET
               normalizer_version = excluded.normalizer_version,
               state = excluded.state,
               progress = excluded.progress,
               batch_id = excluded.batch_id,
               remote_upload_url = excluded.remote_upload_url,
               remote_download_url = excluded.remote_download_url,
               remote_status_json = excluded.remote_status_json,
               retry_count = excluded.retry_count,
               error_code = excluded.error_code,
               error_safe_json = excluded.error_safe_json,
               updated_at = excluded.updated_at,
               completed_at = excluded.completed_at",
        )
        .bind(&operation.id)
        .bind(&operation.job_id)
        .bind(operation.document_id.as_str())
        .bind(&operation.provider_profile_id)
        .bind(&operation.backend)
        .bind(&operation.parser_version)
        .bind(&operation.normalizer_version)
        .bind(&operation.endpoint_origin)
        .bind(&operation.endpoint_fingerprint)
        .bind(operation.state.as_str())
        .bind(operation.progress)
        .bind(&operation.data_id)
        .bind(&operation.batch_id)
        .bind(&operation.upload_url)
        .bind(&operation.download_url)
        .bind(&operation.remote_status_json)
        .bind(i64::from(operation.retry_count))
        .bind(&operation.error_code)
        .bind(&operation.error_safe_json)
        .bind(to_i64(operation.created_at, "parse creation time")?)
        .bind(to_i64(operation.updated_at, "parse update time")?)
        .bind(
            operation
                .completed_at
                .map(|value| to_i64(value, "parse completion time"))
                .transpose()?,
        )
        .execute(&mut **transaction)
        .await
        .map_err(map_sqlx)?;

        if previous_state.as_deref() != Some(operation.state.as_str()) {
            let sequence = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM job_events WHERE job_id = ?1",
            )
            .bind(&operation.job_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(map_sqlx)?;
            let payload = json!({
                "state": operation.state.as_str(),
                "progress": operation.progress,
                "errorCode": operation.error_code,
            })
            .to_string();
            sqlx::query(
                "INSERT INTO job_events (
                   job_id, sequence, event_type, payload_json, created_at
                 ) VALUES (?1, ?2, 'parse_state_changed', ?3, ?4)",
            )
            .bind(&operation.job_id)
            .bind(sequence)
            .bind(payload)
            .bind(to_i64(operation.updated_at, "job event time")?)
            .execute(&mut **transaction)
            .await
            .map_err(map_sqlx)?;
        }
        Ok(())
    }
}

#[async_trait]
impl ParseStore for SqliteParseStore {
    async fn active_document(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<CanonicalDocument>, AtlasError> {
        let row = sqlx::query(
            "SELECT manifest_relative_path, content_digest
             FROM parse_artifacts
             WHERE document_id = ?1 AND is_active = 1",
        )
        .bind(document_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let relative_path: String = row.try_get("manifest_relative_path").map_err(map_sqlx)?;
        let expected_digest: String = row.try_get("content_digest").map_err(map_sqlx)?;
        let relative = Path::new(&relative_path);
        if !safe_relative_path(relative) {
            return Err(AtlasError::storage(
                "stored parse manifest path is outside the artifact cache",
            ));
        }
        let bytes = tokio::fs::read(self.artifact_root.join(relative))
            .await
            .map_err(|error| {
                AtlasError::storage(format!("parse manifest is unavailable: {error}"))
            })?;
        let actual_digest = hex::encode(Sha256::digest(&bytes));
        if actual_digest != expected_digest {
            return Err(AtlasError::storage(
                "parse manifest does not match its stored digest",
            ));
        }
        let document: CanonicalDocument = serde_json::from_slice(&bytes)
            .map_err(|error| AtlasError::storage(format!("parse manifest is invalid: {error}")))?;
        if &document.document_id != document_id {
            return Err(AtlasError::storage(
                "parse manifest belongs to a different document",
            ));
        }
        if document.parser.backend != "cloud_mineru"
            || document.parser.version != CLOUD_PARSER_VERSION
            || document.normalizer_version != NORMALIZER_VERSION
        {
            return Ok(None);
        }
        Ok(Some(document))
    }

    async fn latest_operation(
        &self,
        document_id: &DocumentId,
        backend: Option<&str>,
    ) -> Result<Option<ParseOperation>, AtlasError> {
        let row = sqlx::query(
            "SELECT parse_operations.*, jobs.session_id
             FROM parse_operations
             JOIN jobs ON jobs.id = parse_operations.job_id
             WHERE parse_operations.document_id = ?1
               AND (?2 IS NULL OR parse_operations.backend = ?2)
             ORDER BY parse_operations.created_at DESC, parse_operations.id DESC
             LIMIT 1",
        )
        .bind(document_id.as_str())
        .bind(backend)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        row.map(|row| row_to_operation(&row)).transpose()
    }

    async fn recoverable_operations(&self) -> Result<Vec<ParseOperation>, AtlasError> {
        let rows = sqlx::query(
            "SELECT parse_operations.*, jobs.session_id
             FROM parse_operations
             JOIN jobs ON jobs.id = parse_operations.job_id
             WHERE parse_operations.state IN (
               'queued', 'uploading', 'processing', 'downloading',
               'normalizing', 'status_unknown'
             )
             ORDER BY parse_operations.created_at ASC, parse_operations.id ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        rows.iter().map(row_to_operation).collect()
    }

    async fn save_operation(&self, operation: &ParseOperation) -> Result<(), AtlasError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        Self::save_operation_in(&mut transaction, operation).await?;
        transaction.commit().await.map_err(map_sqlx)
    }

    async fn supersede_operation(
        &self,
        operation: &ParseOperation,
        replacement: &ParseOperation,
    ) -> Result<(), AtlasError> {
        if operation.document_id != replacement.document_id
            || operation.state != ParseOperationState::Cancelled
            || replacement.state != ParseOperationState::Queued
        {
            return Err(AtlasError::invalid_input(
                "a re-upload must atomically cancel one operation and queue its replacement",
            ));
        }
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        let persisted_state = sqlx::query_scalar::<_, String>(
            "SELECT state FROM parse_operations WHERE id = ?1 AND document_id = ?2",
        )
        .bind(&operation.id)
        .bind(operation.document_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        if persisted_state.as_deref() != Some(ParseOperationState::StatusUnknown.as_str()) {
            return Err(AtlasError::invalid_input(
                "the unknown parse operation is no longer eligible for re-upload",
            ));
        }
        Self::save_operation_in(&mut transaction, operation).await?;
        Self::save_operation_in(&mut transaction, replacement).await?;
        transaction.commit().await.map_err(map_sqlx)
    }

    async fn publish(&self, artifact: &PublishArtifact) -> Result<(), AtlasError> {
        if artifact.operation.document_id != artifact.document.document_id
            || artifact.operation.id.is_empty()
            || artifact.id.is_empty()
            || artifact.operation.state != ParseOperationState::Succeeded
        {
            return Err(AtlasError::invalid_input(
                "parse artifact and operation do not describe the same completed document",
            ));
        }
        if !safe_relative_path(Path::new(&artifact.manifest_relative_path)) {
            return Err(AtlasError::invalid_input(
                "parse manifest path must stay inside the artifact cache",
            ));
        }
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE parse_artifacts SET is_active = 0
             WHERE document_id = ?1 AND is_active = 1",
        )
        .bind(artifact.document.document_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "INSERT INTO parse_artifacts (
               id, document_id, parse_operation_id, parser_name, parser_version,
               normalizer_version, canonical_schema_version, source_sha256,
               content_digest, manifest_relative_path, is_active, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11)",
        )
        .bind(&artifact.id)
        .bind(artifact.document.document_id.as_str())
        .bind(&artifact.operation.id)
        .bind(&artifact.document.parser.name)
        .bind(&artifact.document.parser.version)
        .bind(&artifact.document.normalizer_version)
        .bind(i64::from(artifact.document.schema_version))
        .bind(&artifact.document.source_sha256)
        .bind(&artifact.content_digest)
        .bind(&artifact.manifest_relative_path)
        .bind(to_i64(artifact.created_at, "artifact creation time")?)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;

        for chapter in &artifact.document.chapters {
            let mut chapter_hasher = Sha256::new();
            chapter_hasher.update(chapter.source_title.as_bytes());
            for block in &chapter.blocks {
                chapter_hasher.update([0]);
                chapter_hasher.update(block.source_digest.as_bytes());
            }
            let chapter_digest = hex::encode(chapter_hasher.finalize());
            sqlx::query(
                "INSERT INTO chapters (
                   id, artifact_id, document_id, order_index, depth, role,
                   source_title, page_start, page_end, source_digest, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )
            .bind(chapter.id.as_str())
            .bind(&artifact.id)
            .bind(artifact.document.document_id.as_str())
            .bind(i64::from(chapter.order_index))
            .bind(i64::from(chapter.depth))
            .bind(chapter.role.as_str())
            .bind(&chapter.source_title)
            .bind(i64::from(chapter.page_start))
            .bind(i64::from(chapter.page_end))
            .bind(chapter_digest)
            .bind(to_i64(artifact.created_at, "chapter creation time")?)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;

            for block in &chapter.blocks {
                let bounding_boxes_json = serde_json::to_string(&block.bounding_boxes)
                    .map_err(|error| AtlasError::storage(error.to_string()))?;
                let source_json = serde_json::to_string(&block.content)
                    .map_err(|error| AtlasError::storage(error.to_string()))?;
                sqlx::query(
                    "INSERT INTO blocks (
                       id, chapter_id, order_index, kind, page_start, page_end,
                       bounding_boxes_json, source_json, source_plain_text,
                       source_digest, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                )
                .bind(block.id.as_str())
                .bind(chapter.id.as_str())
                .bind(i64::from(block.order_index))
                .bind(block.kind.as_str())
                .bind(i64::from(block.page_start))
                .bind(i64::from(block.page_end))
                .bind(bounding_boxes_json)
                .bind(source_json)
                .bind(&block.content.plain_text)
                .bind(&block.source_digest)
                .bind(to_i64(artifact.created_at, "block creation time")?)
                .execute(&mut *transaction)
                .await
                .map_err(map_sqlx)?;
            }
        }
        Self::save_operation_in(&mut transaction, &artifact.operation).await?;
        transaction.commit().await.map_err(map_sqlx)
    }
}

fn row_to_operation(row: &sqlx::sqlite::SqliteRow) -> Result<ParseOperation, AtlasError> {
    let state_text: String = row.try_get("state").map_err(map_sqlx)?;
    let state = ParseOperationState::parse(&state_text)
        .ok_or_else(|| AtlasError::storage(format!("unknown parse state {state_text}")))?;
    Ok(ParseOperation {
        id: row.try_get("id").map_err(map_sqlx)?,
        job_id: row.try_get("job_id").map_err(map_sqlx)?,
        session_id: row.try_get("session_id").map_err(map_sqlx)?,
        document_id: DocumentId::new(row.try_get::<String, _>("document_id").map_err(map_sqlx)?),
        provider_profile_id: row.try_get("provider_profile_id").map_err(map_sqlx)?,
        backend: row.try_get("backend").map_err(map_sqlx)?,
        parser_version: row.try_get("parser_version").map_err(map_sqlx)?,
        normalizer_version: row.try_get("normalizer_version").map_err(map_sqlx)?,
        endpoint_origin: row.try_get("endpoint_origin").map_err(map_sqlx)?,
        endpoint_fingerprint: row.try_get("endpoint_fingerprint").map_err(map_sqlx)?,
        state,
        progress: row.try_get("progress").map_err(map_sqlx)?,
        data_id: row.try_get("data_id").map_err(map_sqlx)?,
        batch_id: row.try_get("batch_id").map_err(map_sqlx)?,
        upload_url: row.try_get("remote_upload_url").map_err(map_sqlx)?,
        download_url: row.try_get("remote_download_url").map_err(map_sqlx)?,
        remote_status_json: row.try_get("remote_status_json").map_err(map_sqlx)?,
        retry_count: u32_from_i64(row.try_get("retry_count").map_err(map_sqlx)?, "retry count")?,
        error_code: row.try_get("error_code").map_err(map_sqlx)?,
        error_safe_json: row.try_get("error_safe_json").map_err(map_sqlx)?,
        created_at: u64_from_i64(
            row.try_get("created_at").map_err(map_sqlx)?,
            "creation time",
        )?,
        updated_at: u64_from_i64(row.try_get("updated_at").map_err(map_sqlx)?, "update time")?,
        completed_at: row
            .try_get::<Option<i64>, _>("completed_at")
            .map_err(map_sqlx)?
            .map(|value| u64_from_i64(value, "completion time"))
            .transpose()?,
    })
}

fn safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn u64_from_i64(value: i64, label: &str) -> Result<u64, AtlasError> {
    u64::try_from(value)
        .map_err(|_| AtlasError::storage(format!("stored {label} cannot be negative")))
}

fn u32_from_i64(value: i64, label: &str) -> Result<u32, AtlasError> {
    u32::try_from(value)
        .map_err(|_| AtlasError::storage(format!("stored {label} is outside the supported range")))
}
