use async_trait::async_trait;
use atlas_domain::{
    AtlasError, BlockId, ChapterId, DocumentId, JobId, SessionId, StructuredContent,
};
use atlas_translation::{
    CommittedTranslation, NewTranslationRecord, RecoveryTarget, StoredTranslation, TranslationJob,
    TranslationJobKind, TranslationJobState, TranslationRecordState, TranslationStore,
};
use serde_json::{Value, json};
use sqlx::{Row, Sqlite, SqlitePool, Transaction, sqlite::SqliteRow};

use crate::{AtlasDatabase, map_sqlx, to_i64, to_u64};

#[derive(Clone, Debug)]
pub struct SqliteTranslationStore {
    pool: SqlitePool,
}

impl SqliteTranslationStore {
    #[must_use]
    pub fn new(database: &AtlasDatabase) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }

    async fn save_job_in(
        transaction: &mut Transaction<'_, Sqlite>,
        job: &TranslationJob,
    ) -> Result<TranslationJobState, AtlasError> {
        let previous_state =
            sqlx::query_scalar::<_, String>("SELECT state FROM jobs WHERE id = ?1")
                .bind(job.id.as_str())
                .fetch_optional(&mut **transaction)
                .await
                .map_err(map_sqlx)?;
        let input_json = json!({
            "planDigest": job.plan_digest,
            "endpointFingerprint": job.endpoint_fingerprint,
            "modelId": job.model_id,
            "blockIds": job.block_ids,
        })
        .to_string();
        let checkpoint_json = json!({
            "completedBlockIds": job.completed_block_ids,
        })
        .to_string();
        let error_safe_json = job
            .safe_message
            .as_ref()
            .map(|message| json!({ "message": message }).to_string());
        let started_at = (job.state != TranslationJobState::Queued)
            .then_some(to_i64(job.updated_at, "translation job start time")?);
        let result_json = (job.state == TranslationJobState::Succeeded).then(|| {
            json!({
                "chapterId": job.chapter_id,
                "completedBlockIds": job.completed_block_ids,
            })
            .to_string()
        });

        sqlx::query(
            "INSERT INTO jobs (
               id, session_id, document_id, chapter_id, kind, priority, state,
               idempotency_key, input_json, checkpoint_json, result_json,
               error_code, error_safe_json, attempt_count, max_attempts, run_after,
               created_at, updated_at, started_at, completed_at
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
               ?14, 3, ?15, ?16, ?17, ?18, ?19
             )
             ON CONFLICT(id) DO UPDATE SET
               session_id = CASE WHEN jobs.state = 'cancelled' THEN jobs.session_id ELSE excluded.session_id END,
               kind = CASE WHEN jobs.state = 'cancelled' THEN jobs.kind ELSE excluded.kind END,
               priority = CASE WHEN jobs.state = 'cancelled' THEN jobs.priority ELSE excluded.priority END,
               state = CASE WHEN jobs.state = 'cancelled' THEN jobs.state ELSE excluded.state END,
               idempotency_key = CASE WHEN jobs.state = 'cancelled' THEN jobs.idempotency_key ELSE excluded.idempotency_key END,
               input_json = CASE WHEN jobs.state = 'cancelled' THEN jobs.input_json ELSE excluded.input_json END,
               checkpoint_json = CASE WHEN jobs.state = 'cancelled' THEN jobs.checkpoint_json ELSE excluded.checkpoint_json END,
               result_json = CASE WHEN jobs.state = 'cancelled' THEN jobs.result_json ELSE excluded.result_json END,
               error_code = CASE WHEN jobs.state = 'cancelled' THEN jobs.error_code ELSE excluded.error_code END,
               error_safe_json = CASE WHEN jobs.state = 'cancelled' THEN jobs.error_safe_json ELSE excluded.error_safe_json END,
               attempt_count = CASE WHEN jobs.state = 'cancelled' THEN jobs.attempt_count ELSE excluded.attempt_count END,
               run_after = CASE WHEN jobs.state = 'cancelled' THEN jobs.run_after ELSE excluded.run_after END,
               updated_at = CASE WHEN jobs.state = 'cancelled' THEN jobs.updated_at ELSE excluded.updated_at END,
               started_at = COALESCE(jobs.started_at, excluded.started_at),
               completed_at = CASE WHEN jobs.state = 'cancelled' THEN jobs.completed_at ELSE excluded.completed_at END",
        )
        .bind(job.id.as_str())
        .bind(job.session_id.as_str())
        .bind(job.document_id.as_str())
        .bind(job.chapter_id.as_str())
        .bind(job.kind.as_str())
        .bind(job.kind.priority())
        .bind(job.state.as_str())
        .bind(&job.plan_digest)
        .bind(input_json)
        .bind(checkpoint_json)
        .bind(result_json)
        .bind(&job.error_code)
        .bind(error_safe_json)
        .bind(i64::from(job.attempt_count))
        .bind(to_i64(job.updated_at, "translation job run time")?)
        .bind(to_i64(job.created_at, "translation job creation time")?)
        .bind(to_i64(job.updated_at, "translation job update time")?)
        .bind(started_at)
        .bind(
            job.completed_at
                .map(|value| to_i64(value, "translation job completion time"))
                .transpose()?,
        )
        .execute(&mut **transaction)
        .await
        .map_err(map_sqlx)?;

        let actual_state = sqlx::query_scalar::<_, String>("SELECT state FROM jobs WHERE id = ?1")
            .bind(job.id.as_str())
            .fetch_one(&mut **transaction)
            .await
            .map_err(map_sqlx)?;
        let actual_state = TranslationJobState::parse(&actual_state)
            .ok_or_else(|| AtlasError::storage("unknown translation job state"))?;
        if previous_state.as_deref() != Some(actual_state.as_str()) {
            let sequence = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(MAX(sequence), 0) + 1
                 FROM job_events
                 WHERE job_id = ?1",
            )
            .bind(job.id.as_str())
            .fetch_one(&mut **transaction)
            .await
            .map_err(map_sqlx)?;
            sqlx::query(
                "INSERT INTO job_events (
                   job_id, sequence, event_type, payload_json, created_at
                 ) VALUES (?1, ?2, 'translation_state_changed', ?3, ?4)",
            )
            .bind(job.id.as_str())
            .bind(sequence)
            .bind(
                json!({
                    "state": actual_state.as_str(),
                    "completedBlocks": job.completed_block_ids.len(),
                    "totalBlocks": job.block_ids.len(),
                    "errorCode": job.error_code,
                })
                .to_string(),
            )
            .bind(to_i64(job.updated_at, "translation event time")?)
            .execute(&mut **transaction)
            .await
            .map_err(map_sqlx)?;
        }
        Ok(actual_state)
    }

    async fn stored_row(
        &self,
        query: &'static str,
        first: &str,
        second: Option<&str>,
    ) -> Result<Option<StoredTranslation>, AtlasError> {
        let mut query = sqlx::query(query).bind(first);
        if let Some(second) = second {
            query = query.bind(second);
        }
        query
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .map(row_to_translation)
            .transpose()
    }
}

#[async_trait]
impl TranslationStore for SqliteTranslationStore {
    async fn translation(
        &self,
        block_id: &BlockId,
        request_digest: &str,
    ) -> Result<Option<StoredTranslation>, AtlasError> {
        self.stored_row(
            "SELECT * FROM translations
             WHERE block_id = ?1 AND request_digest = ?2",
            block_id.as_str(),
            Some(request_digest),
        )
        .await
    }

    async fn active_for_chapter(
        &self,
        chapter_id: &ChapterId,
    ) -> Result<Vec<StoredTranslation>, AtlasError> {
        sqlx::query(
            "SELECT translations.*
             FROM translations
             JOIN blocks ON blocks.id = translations.block_id
             WHERE blocks.chapter_id = ?1 AND translations.is_active = 1
             ORDER BY blocks.order_index",
        )
        .bind(chapter_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?
        .into_iter()
        .map(row_to_translation)
        .collect()
    }

    async fn latest_job(
        &self,
        chapter_id: &ChapterId,
        plan_digest: Option<&str>,
    ) -> Result<Option<TranslationJob>, AtlasError> {
        let row = sqlx::query(
            "SELECT *
             FROM jobs
             WHERE chapter_id = ?1
               AND kind IN ('translate', 'prefetch')
               AND (?2 IS NULL OR idempotency_key = ?2)
             ORDER BY created_at DESC, rowid DESC
             LIMIT 1",
        )
        .bind(chapter_id.as_str())
        .bind(plan_digest)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        row.map(row_to_job).transpose()
    }

    async fn prepare_job(
        &self,
        job: &TranslationJob,
        records: &[NewTranslationRecord],
    ) -> Result<Vec<BlockId>, AtlasError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        let actual_state = Self::save_job_in(&mut transaction, job).await?;
        if actual_state == TranslationJobState::Cancelled {
            return Err(AtlasError::storage(
                "a cancelled translation job cannot be prepared",
            ));
        }
        let mut missing = Vec::new();
        for record in records {
            let existing_state = sqlx::query_scalar::<_, String>(
                "SELECT state
                 FROM translations
                 WHERE block_id = ?1 AND request_digest = ?2",
            )
            .bind(record.block_id.as_str())
            .bind(&record.request_digest)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_sqlx)?;

            sqlx::query(
                "UPDATE translations
                 SET is_active = 0
                 WHERE block_id = ?1 AND is_active = 1",
            )
            .bind(record.block_id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;

            if let Some(state) = existing_state {
                let ready = state == TranslationRecordState::Ready.as_str();
                sqlx::query(
                    "UPDATE translations
                     SET job_id = ?3,
                         state = CASE WHEN state = 'ready' THEN state ELSE 'queued' END,
                         error_code = CASE WHEN state = 'ready' THEN error_code ELSE NULL END,
                         error_safe_json = CASE WHEN state = 'ready' THEN error_safe_json ELSE NULL END,
                         is_active = 1,
                         updated_at = ?4
                     WHERE block_id = ?1 AND request_digest = ?2",
                )
                .bind(record.block_id.as_str())
                .bind(&record.request_digest)
                .bind(job.id.as_str())
                .bind(to_i64(job.updated_at, "translation update time")?)
                .execute(&mut *transaction)
                .await
                .map_err(map_sqlx)?;
                if !ready {
                    missing.push(record.block_id.clone());
                }
            } else {
                sqlx::query(
                    "INSERT INTO translations (
                       id, job_id, block_id, request_digest, source_digest,
                       target_locale, endpoint_origin, provider_profile_fingerprint,
                       model_id, prompt_version, translation_mode,
                       applicable_preference_digest, state, is_active, created_at,
                       updated_at
                     ) VALUES (
                       ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                       'queued', 1, ?13, ?13
                     )",
                )
                .bind(&record.id)
                .bind(job.id.as_str())
                .bind(record.block_id.as_str())
                .bind(&record.request_digest)
                .bind(&record.source_digest)
                .bind(&record.target_locale)
                .bind(&record.endpoint_origin)
                .bind(&record.provider_profile_fingerprint)
                .bind(&record.model_id)
                .bind(&record.prompt_version)
                .bind(atlas_translation::TRANSLATION_MODE)
                .bind(&record.applicable_preference_digest)
                .bind(to_i64(record.created_at, "translation creation time")?)
                .execute(&mut *transaction)
                .await
                .map_err(map_sqlx)?;
                missing.push(record.block_id.clone());
            }
        }
        transaction.commit().await.map_err(map_sqlx)?;
        Ok(missing)
    }

    async fn save_job(&self, job: &TranslationJob) -> Result<(), AtlasError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        let actual_state = Self::save_job_in(&mut transaction, job).await?;
        match actual_state {
            TranslationJobState::Running => {
                sqlx::query(
                    "UPDATE translations
                     SET state = 'translating', updated_at = ?2
                     WHERE job_id = ?1 AND is_active = 1 AND state = 'queued'",
                )
                .bind(job.id.as_str())
                .bind(to_i64(job.updated_at, "translation start time")?)
                .execute(&mut *transaction)
                .await
                .map_err(map_sqlx)?;
            }
            TranslationJobState::Cancelled => {
                sqlx::query(
                    "UPDATE translations
                     SET state = 'cancelled', updated_at = ?2
                     WHERE job_id = ?1 AND is_active = 1
                       AND state IN ('queued', 'translating')",
                )
                .bind(job.id.as_str())
                .bind(to_i64(job.updated_at, "translation cancellation time")?)
                .execute(&mut *transaction)
                .await
                .map_err(map_sqlx)?;
            }
            _ => {}
        }
        transaction.commit().await.map_err(map_sqlx)
    }

    async fn commit(
        &self,
        job: &TranslationJob,
        translations: &[CommittedTranslation],
    ) -> Result<(), AtlasError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        let state = sqlx::query_scalar::<_, String>("SELECT state FROM jobs WHERE id = ?1")
            .bind(job.id.as_str())
            .fetch_one(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
        if state == TranslationJobState::Cancelled.as_str() {
            return Err(AtlasError::storage(
                "translation was cancelled before the block could be committed",
            ));
        }
        for translation in translations {
            let target_json = serde_json::to_string(&translation.target)
                .map_err(|error| AtlasError::storage(error.to_string()))?;
            let updated = sqlx::query(
                "UPDATE translations
                 SET target_json = ?3,
                     target_plain_text = ?4,
                     state = 'ready',
                     validation_json = ?5,
                     error_code = NULL,
                     error_safe_json = NULL,
                     updated_at = ?6
                 WHERE block_id = ?1 AND job_id = ?2 AND is_active = 1",
            )
            .bind(translation.block_id.as_str())
            .bind(job.id.as_str())
            .bind(target_json)
            .bind(&translation.target_plain_text)
            .bind(&translation.validation_json)
            .bind(to_i64(job.updated_at, "translation commit time")?)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
            if updated.rows_affected() != 1 {
                return Err(AtlasError::storage(
                    "translation cache changed before a block could be committed",
                ));
            }
        }
        Self::save_job_in(&mut transaction, job).await?;
        transaction.commit().await.map_err(map_sqlx)
    }

    async fn fail(
        &self,
        job: &TranslationJob,
        failures: &[(BlockId, String, String)],
    ) -> Result<(), AtlasError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        let state = sqlx::query_scalar::<_, String>("SELECT state FROM jobs WHERE id = ?1")
            .bind(job.id.as_str())
            .fetch_one(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
        if state == TranslationJobState::Cancelled.as_str() {
            transaction.commit().await.map_err(map_sqlx)?;
            return Ok(());
        }
        for (block_id, code, message) in failures {
            sqlx::query(
                "UPDATE translations
                 SET state = 'failed',
                     error_code = ?3,
                     error_safe_json = ?4,
                     updated_at = ?5
                 WHERE block_id = ?1 AND job_id = ?2 AND is_active = 1
                   AND state != 'ready'",
            )
            .bind(block_id.as_str())
            .bind(job.id.as_str())
            .bind(code)
            .bind(json!({ "message": message }).to_string())
            .bind(to_i64(job.updated_at, "translation failure time")?)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
        }
        Self::save_job_in(&mut transaction, job).await?;
        transaction.commit().await.map_err(map_sqlx)
    }

    async fn recoverable(&self) -> Result<Vec<RecoveryTarget>, AtlasError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE jobs
             SET state = 'interrupted'
             WHERE kind IN ('translate', 'prefetch')
               AND state IN ('queued', 'running')",
        )
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE jobs
             SET state = 'cancelled',
                 error_code = 'superseded',
                 error_safe_json = '{\"message\":\"A newer translation job superseded this recovery\"}',
                 completed_at = updated_at
             WHERE kind IN ('translate', 'prefetch')
               AND state = 'interrupted'
               AND EXISTS (
                 SELECT 1
                 FROM jobs AS newer
                 WHERE newer.chapter_id = jobs.chapter_id
                   AND newer.idempotency_key = jobs.idempotency_key
                   AND newer.kind IN ('translate', 'prefetch')
                   AND (
                     newer.created_at > jobs.created_at
                     OR (
                       newer.created_at = jobs.created_at
                       AND newer.rowid > jobs.rowid
                     )
                   )
               )",
        )
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        let rows = sqlx::query(
            "SELECT id, session_id, document_id, chapter_id, kind
             FROM jobs
             WHERE kind IN ('translate', 'prefetch')
               AND state = 'interrupted'
             ORDER BY priority DESC, created_at ASC",
        )
        .fetch_all(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        transaction.commit().await.map_err(map_sqlx)?;

        rows.into_iter()
            .map(|row| {
                let kind: String = row.try_get("kind").map_err(map_sqlx)?;
                Ok(RecoveryTarget {
                    job_id: JobId::new(row.try_get::<String, _>("id").map_err(map_sqlx)?),
                    session_id: SessionId::new(
                        row.try_get::<String, _>("session_id").map_err(map_sqlx)?,
                    ),
                    document_id: DocumentId::new(
                        row.try_get::<String, _>("document_id").map_err(map_sqlx)?,
                    ),
                    chapter_id: ChapterId::new(
                        row.try_get::<String, _>("chapter_id").map_err(map_sqlx)?,
                    ),
                    kind: TranslationJobKind::parse(&kind)
                        .ok_or_else(|| AtlasError::storage("unknown translation job kind"))?,
                })
            })
            .collect()
    }

    async fn cancel_document(
        &self,
        document_id: &DocumentId,
        cancelled_at: u64,
    ) -> Result<usize, AtlasError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        let job_ids = sqlx::query_scalar::<_, String>(
            "SELECT id
             FROM jobs
             WHERE document_id = ?1
               AND kind IN ('translate', 'prefetch')
               AND state IN ('queued', 'running', 'interrupted')",
        )
        .bind(document_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        let cancelled_at = to_i64(cancelled_at, "translation cancellation time")?;
        for job_id in &job_ids {
            sqlx::query(
                "UPDATE jobs
                 SET state = 'cancelled',
                     error_code = 'cancelled',
                     error_safe_json = '{\"message\":\"Translation was cancelled\"}',
                     updated_at = ?2,
                     completed_at = ?2
                 WHERE id = ?1",
            )
            .bind(job_id)
            .bind(cancelled_at)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
            sqlx::query(
                "UPDATE translations
                 SET state = 'cancelled',
                     error_code = 'cancelled',
                     error_safe_json = '{\"message\":\"Translation was cancelled\"}',
                     updated_at = ?2
                 WHERE job_id = ?1
                   AND is_active = 1
                   AND state IN ('queued', 'translating')",
            )
            .bind(job_id)
            .bind(cancelled_at)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
            let sequence = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(MAX(sequence), 0) + 1
                 FROM job_events
                 WHERE job_id = ?1",
            )
            .bind(job_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
            sqlx::query(
                "INSERT INTO job_events (
                   job_id, sequence, event_type, payload_json, created_at
                 ) VALUES (
                   ?1, ?2, 'translation_state_changed',
                   '{\"state\":\"cancelled\",\"errorCode\":\"cancelled\"}', ?3
                 )",
            )
            .bind(job_id)
            .bind(sequence)
            .bind(cancelled_at)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
        }
        transaction.commit().await.map_err(map_sqlx)?;
        Ok(job_ids.len())
    }

    async fn supersede_interrupted(
        &self,
        job_id: &JobId,
        superseded_at: u64,
    ) -> Result<bool, AtlasError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        let superseded_at = to_i64(superseded_at, "translation supersede time")?;
        let updated = sqlx::query(
            "UPDATE jobs
             SET state = 'cancelled',
                 error_code = 'superseded',
                 error_safe_json = '{\"message\":\"A newer translation plan superseded this recovery\"}',
                 updated_at = ?2,
                 completed_at = ?2
             WHERE id = ?1 AND state = 'interrupted'",
        )
        .bind(job_id.as_str())
        .bind(superseded_at)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        if updated.rows_affected() == 1 {
            sqlx::query(
                "UPDATE translations
                 SET state = 'cancelled',
                     error_code = 'superseded',
                     error_safe_json = '{\"message\":\"A newer translation plan superseded this recovery\"}',
                     updated_at = ?2
                 WHERE job_id = ?1
                   AND state IN ('queued', 'translating')",
            )
            .bind(job_id.as_str())
            .bind(superseded_at)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
        }
        transaction.commit().await.map_err(map_sqlx)?;
        Ok(updated.rows_affected() == 1)
    }

    async fn latest_prefetched_chapter(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<ChapterId>, AtlasError> {
        sqlx::query_scalar::<_, String>(
            "SELECT chapter_id
             FROM jobs
             WHERE document_id = ?1 AND kind = 'prefetch' AND state = 'succeeded'
             ORDER BY completed_at DESC, id DESC
             LIMIT 1",
        )
        .bind(document_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)
        .map(|value| value.map(ChapterId::new))
    }
}

fn row_to_translation(row: SqliteRow) -> Result<StoredTranslation, AtlasError> {
    let state: String = row.try_get("state").map_err(map_sqlx)?;
    let target_json: Option<String> = row.try_get("target_json").map_err(map_sqlx)?;
    let error_safe_json: Option<String> = row.try_get("error_safe_json").map_err(map_sqlx)?;
    Ok(StoredTranslation {
        id: row.try_get("id").map_err(map_sqlx)?,
        block_id: BlockId::new(row.try_get::<String, _>("block_id").map_err(map_sqlx)?),
        request_digest: row.try_get("request_digest").map_err(map_sqlx)?,
        source_digest: row.try_get("source_digest").map_err(map_sqlx)?,
        model_id: row.try_get("model_id").map_err(map_sqlx)?,
        state: TranslationRecordState::parse(&state)
            .ok_or_else(|| AtlasError::storage("unknown translation cache state"))?,
        target: target_json
            .map(|value| {
                serde_json::from_str::<StructuredContent>(&value)
                    .map_err(|error| AtlasError::storage(error.to_string()))
            })
            .transpose()?,
        target_plain_text: row.try_get("target_plain_text").map_err(map_sqlx)?,
        error_code: row.try_get("error_code").map_err(map_sqlx)?,
        safe_message: safe_message(error_safe_json.as_deref()),
        updated_at: to_u64(
            row.try_get::<i64, _>("updated_at").map_err(map_sqlx)?,
            "translation update time",
        )?,
    })
}

fn row_to_job(row: SqliteRow) -> Result<TranslationJob, AtlasError> {
    let state: String = row.try_get("state").map_err(map_sqlx)?;
    let kind: String = row.try_get("kind").map_err(map_sqlx)?;
    let input_json: String = row.try_get("input_json").map_err(map_sqlx)?;
    let checkpoint_json: Option<String> = row.try_get("checkpoint_json").map_err(map_sqlx)?;
    let input: Value = serde_json::from_str(&input_json)
        .map_err(|error| AtlasError::storage(error.to_string()))?;
    let checkpoint = checkpoint_json
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
        .map_err(|error| AtlasError::storage(error.to_string()))?
        .unwrap_or_else(|| json!({}));
    let block_ids = string_ids(&input, "blockIds")?
        .into_iter()
        .map(BlockId::new)
        .collect();
    let completed_block_ids = string_ids(&checkpoint, "completedBlockIds")?
        .into_iter()
        .map(BlockId::new)
        .collect();
    let error_safe_json: Option<String> = row.try_get("error_safe_json").map_err(map_sqlx)?;
    Ok(TranslationJob {
        id: JobId::new(row.try_get::<String, _>("id").map_err(map_sqlx)?),
        session_id: SessionId::new(row.try_get::<String, _>("session_id").map_err(map_sqlx)?),
        document_id: DocumentId::new(row.try_get::<String, _>("document_id").map_err(map_sqlx)?),
        chapter_id: ChapterId::new(row.try_get::<String, _>("chapter_id").map_err(map_sqlx)?),
        kind: TranslationJobKind::parse(&kind)
            .ok_or_else(|| AtlasError::storage("unknown translation job kind"))?,
        state: TranslationJobState::parse(&state)
            .ok_or_else(|| AtlasError::storage("unknown translation job state"))?,
        plan_digest: required_string(&input, "planDigest")?,
        endpoint_fingerprint: required_string(&input, "endpointFingerprint")?,
        model_id: required_string(&input, "modelId")?,
        block_ids,
        completed_block_ids,
        attempt_count: u32::try_from(row.try_get::<i64, _>("attempt_count").map_err(map_sqlx)?)
            .map_err(|_| AtlasError::storage("translation attempt count is outside u32 range"))?,
        error_code: row.try_get("error_code").map_err(map_sqlx)?,
        safe_message: safe_message(error_safe_json.as_deref()),
        created_at: to_u64(
            row.try_get::<i64, _>("created_at").map_err(map_sqlx)?,
            "translation job creation time",
        )?,
        updated_at: to_u64(
            row.try_get::<i64, _>("updated_at").map_err(map_sqlx)?,
            "translation job update time",
        )?,
        completed_at: row
            .try_get::<Option<i64>, _>("completed_at")
            .map_err(map_sqlx)?
            .map(|value| to_u64(value, "translation job completion time"))
            .transpose()?,
    })
}

fn required_string(value: &Value, field: &str) -> Result<String, AtlasError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| AtlasError::storage(format!("translation job is missing {field}")))
}

fn string_ids(value: &Value, field: &str) -> Result<Vec<String>, AtlasError> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| AtlasError::storage(format!("translation job is missing {field}")))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| AtlasError::storage(format!("translation job has invalid {field}")))
        })
        .collect()
}

fn safe_message(encoded: Option<&str>) -> Option<String> {
    encoded
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .and_then(|value| value.get("message")?.as_str().map(str::to_owned))
}
