use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use atlas_domain::{
    AtlasError, BlockId, BlockKind, CanonicalBlock, CanonicalChapter, CanonicalDocument, ChapterId,
    ChapterRole, DocumentId, JobId, ParserIdentity, SessionId, StructuredContent,
};
use atlas_parse::{ParseOperation, ParseStore, PublishArtifact};
use atlas_storage::{AtlasDatabase, SqliteTranslationStore};
use atlas_translation::{
    CommittedTranslation, DefaultTranslationModule, EnsureTranslationInput, NewTranslationRecord,
    ProviderTranslationRequest, ScriptedTranslationAdapter, ScriptedTranslationResponse,
    TranslationChunkSink, TranslationCompletion, TranslationConfiguration,
    TranslationConfigurationPort, TranslationCredential, TranslationJob, TranslationJobKind,
    TranslationJobState, TranslationModule, TranslationPlanner, TranslationProviderError,
    TranslationProviderErrorKind, TranslationProviderPort, TranslationRecordState,
    TranslationStore,
};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, RwLock};
use tokio_util::sync::CancellationToken;

async fn fixture(database: &AtlasDatabase) {
    sqlx::query(
        "INSERT INTO documents (
           id, sha256, title, authors_json, page_count, file_path,
           file_size_bytes, file_mtime_ms, file_state, created_at,
           updated_at, last_opened_at
         ) VALUES (
           'document-1', 'source-sha', 'Synthetic paper', '[]', 2,
           '/tmp/synthetic.pdf', 100, 1, 'available', 1, 1, 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("document fixture should insert");
    sqlx::query(
        "INSERT INTO jobs (
           id, session_id, document_id, kind, priority, state, input_json,
           attempt_count, max_attempts, run_after, created_at, updated_at
         ) VALUES (
           'parse-job', 'session-parse', 'document-1', 'cloud_parse', 100,
           'succeeded', '{}', 1, 1, 1, 1, 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("parse job fixture should insert");
    sqlx::query(
        "INSERT INTO parse_operations (
           id, job_id, document_id, backend, parser_version,
           normalizer_version, state, data_id, retry_count, created_at, updated_at,
           completed_at
         ) VALUES (
           'parse-operation', 'parse-job', 'document-1', 'cloud_mineru',
           'parser-1', 'normalizer-1', 'succeeded', 'data-1', 0, 1, 1, 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("parse operation fixture should insert");
    sqlx::query(
        "INSERT INTO parse_artifacts (
           id, document_id, parse_operation_id, parser_name, parser_version,
           normalizer_version, canonical_schema_version, source_sha256,
           content_digest, manifest_relative_path, is_active, created_at
         ) VALUES (
           'artifact-1', 'document-1', 'parse-operation', 'Synthetic',
           'parser-1', 'normalizer-1', 1, 'source-sha', 'digest',
           'document-1/artifact-1/document.json', 1, 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("artifact fixture should insert");
    sqlx::query(
        "INSERT INTO chapters (
           id, artifact_id, document_id, order_index, depth, role,
           source_title, page_start, page_end, source_digest, created_at
         ) VALUES (
           'chapter-1', 'artifact-1', 'document-1', 0, 1, 'body',
           'Introduction', 1, 2, 'chapter-digest', 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("chapter fixture should insert");
    for (index, block_id) in ["block-1", "block-2"].into_iter().enumerate() {
        sqlx::query(
            "INSERT INTO blocks (
               id, chapter_id, order_index, kind, page_start, page_end,
               bounding_boxes_json, source_json, source_plain_text,
               source_digest, created_at
             ) VALUES (?1, 'chapter-1', ?2, 'paragraph', 1, 1, '[]', ?3, ?4, ?5, 1)",
        )
        .bind(block_id)
        .bind(i64::try_from(index).expect("fixture index should fit"))
        .bind(format!(
            r#"{{"plainText":"Source {index}","atoms":[{{"type":"text","value":"Source {index}"}}]}}"#
        ))
        .bind(format!("Source {index}"))
        .bind(format!("source-{index}"))
        .execute(database.pool())
        .await
        .expect("block fixture should insert");
    }
    sqlx::query(
        "INSERT INTO chapters (
           id, artifact_id, document_id, order_index, depth, role,
           source_title, page_start, page_end, source_digest, created_at
         ) VALUES (
           'chapter-2', 'artifact-1', 'document-1', 1, 1, 'body',
           'Method', 2, 2, 'chapter-2-digest', 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("prefetch chapter fixture should insert");
    sqlx::query(
        "INSERT INTO blocks (
           id, chapter_id, order_index, kind, page_start, page_end,
           bounding_boxes_json, source_json, source_plain_text,
           source_digest, created_at
         ) VALUES (
           'block-3', 'chapter-2', 0, 'paragraph', 2, 2, '[]',
           '{\"plainText\":\"Source 3\",\"atoms\":[{\"type\":\"text\",\"value\":\"Source 3\"}]}',
           'Source 3', 'source-3', 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("prefetch block fixture should insert");
}

fn job(id: &str, state: TranslationJobState, created_at: u64) -> TranslationJob {
    TranslationJob {
        id: JobId::from(id),
        session_id: SessionId::from("session-1"),
        document_id: DocumentId::from("document-1"),
        chapter_id: ChapterId::from("chapter-1"),
        kind: TranslationJobKind::Foreground,
        state,
        plan_digest: "plan-1".to_owned(),
        endpoint_fingerprint: "endpoint-1".to_owned(),
        model_id: "model-1".to_owned(),
        block_ids: vec![BlockId::from("block-1"), BlockId::from("block-2")],
        completed_block_ids: Vec::new(),
        attempt_count: 0,
        error_code: None,
        safe_message: None,
        created_at,
        updated_at: created_at,
        completed_at: None,
    }
}

fn records(created_at: u64) -> Vec<NewTranslationRecord> {
    ["block-1", "block-2"]
        .into_iter()
        .enumerate()
        .map(|(index, block_id)| NewTranslationRecord {
            id: format!("translation-{created_at}-{index}"),
            block_id: BlockId::from(block_id),
            request_digest: format!("request-{index}"),
            source_digest: format!("source-{index}"),
            target_locale: "zh-CN".to_owned(),
            endpoint_origin: "https://models.example/v1".to_owned(),
            provider_profile_fingerprint: "endpoint-1".to_owned(),
            model_id: "model-1".to_owned(),
            prompt_version: "academic-blocks-v1".to_owned(),
            applicable_preference_digest: String::new(),
            created_at,
        })
        .collect()
}

#[derive(Clone)]
struct CacheRaceStore {
    inner: Arc<SqliteTranslationStore>,
}

#[async_trait]
impl TranslationStore for CacheRaceStore {
    async fn translation(
        &self,
        block_id: &BlockId,
        request_digest: &str,
    ) -> Result<Option<atlas_translation::StoredTranslation>, AtlasError> {
        self.inner.translation(block_id, request_digest).await
    }

    async fn active_for_chapter(
        &self,
        chapter_id: &ChapterId,
    ) -> Result<Vec<atlas_translation::StoredTranslation>, AtlasError> {
        self.inner.active_for_chapter(chapter_id).await
    }

    async fn latest_job(
        &self,
        chapter_id: &ChapterId,
        plan_digest: Option<&str>,
    ) -> Result<Option<TranslationJob>, AtlasError> {
        self.inner.latest_job(chapter_id, plan_digest).await
    }

    async fn prepare_job(
        &self,
        job: &TranslationJob,
        records: &[NewTranslationRecord],
    ) -> Result<Vec<BlockId>, AtlasError> {
        self.inner.prepare_job(job, records).await?;
        let translations = records
            .iter()
            .map(|record| CommittedTranslation {
                block_id: record.block_id.clone(),
                target: StructuredContent::text("竞态缓存"),
                target_plain_text: "竞态缓存".to_owned(),
                validation_json: r#"{"structure":"valid"}"#.to_owned(),
            })
            .collect::<Vec<_>>();
        self.inner.commit(job, &translations).await?;
        Ok(Vec::new())
    }

    async fn save_job(&self, job: &TranslationJob) -> Result<(), AtlasError> {
        self.inner.save_job(job).await
    }

    async fn commit(
        &self,
        job: &TranslationJob,
        translations: &[CommittedTranslation],
    ) -> Result<(), AtlasError> {
        self.inner.commit(job, translations).await
    }

    async fn fail(
        &self,
        job: &TranslationJob,
        failures: &[(BlockId, String, String)],
    ) -> Result<(), AtlasError> {
        self.inner.fail(job, failures).await
    }

    async fn recoverable(&self) -> Result<Vec<atlas_translation::RecoveryTarget>, AtlasError> {
        self.inner.recoverable().await
    }

    async fn cancel_document(
        &self,
        document_id: &DocumentId,
        cancelled_at: u64,
    ) -> Result<usize, AtlasError> {
        self.inner.cancel_document(document_id, cancelled_at).await
    }

    async fn supersede_interrupted(
        &self,
        job_id: &JobId,
        superseded_at: u64,
    ) -> Result<bool, AtlasError> {
        self.inner
            .supersede_interrupted(job_id, superseded_at)
            .await
    }

    async fn latest_prefetched_chapter(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<ChapterId>, AtlasError> {
        self.inner.latest_prefetched_chapter(document_id).await
    }
}

#[tokio::test]
async fn cache_rows_and_job_checkpoints_survive_partial_failure_and_retry() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let store = SqliteTranslationStore::new(&database);
    let mut first = job("translation-job-1", TranslationJobState::Queued, 10);

    let missing = store
        .prepare_job(&first, &records(10))
        .await
        .expect("translation should queue");
    assert_eq!(missing.len(), 2);
    first.state = TranslationJobState::Running;
    first.updated_at = 11;
    first.attempt_count = 1;
    store.save_job(&first).await.expect("job should start");

    first.completed_block_ids.push(BlockId::from("block-1"));
    first.updated_at = 12;
    store
        .commit(
            &first,
            &[CommittedTranslation {
                block_id: BlockId::from("block-1"),
                target: StructuredContent::text("第一段"),
                target_plain_text: "第一段".to_owned(),
                validation_json: r#"{"structure":"valid"}"#.to_owned(),
            }],
        )
        .await
        .expect("validated block should commit");
    first.state = TranslationJobState::Failed;
    first.error_code = Some("translation_invalid".to_owned());
    first.safe_message = Some("One block failed".to_owned());
    first.updated_at = 13;
    first.completed_at = Some(13);
    store
        .fail(
            &first,
            &[(
                BlockId::from("block-2"),
                "missing_block".to_owned(),
                "The model omitted a block".to_owned(),
            )],
        )
        .await
        .expect("failed block should persist");

    let partial = store
        .active_for_chapter(&ChapterId::from("chapter-1"))
        .await
        .expect("partial chapter should load");
    assert_eq!(partial[0].state, TranslationRecordState::Ready);
    assert_eq!(partial[1].state, TranslationRecordState::Failed);

    let mut retry = job("translation-job-2", TranslationJobState::Queued, 20);
    let missing = store
        .prepare_job(&retry, &records(20))
        .await
        .expect("retry should prepare");
    assert_eq!(missing, vec![BlockId::from("block-2")]);
    retry.state = TranslationJobState::Running;
    retry.updated_at = 21;
    store.save_job(&retry).await.expect("retry should start");
    retry.completed_block_ids = retry.block_ids.clone();
    retry.updated_at = 22;
    store
        .commit(
            &retry,
            &[CommittedTranslation {
                block_id: BlockId::from("block-2"),
                target: StructuredContent::text("第二段"),
                target_plain_text: "第二段".to_owned(),
                validation_json: r#"{"structure":"valid"}"#.to_owned(),
            }],
        )
        .await
        .expect("retry should commit only the failed block");
    retry.state = TranslationJobState::Succeeded;
    retry.completed_at = Some(23);
    retry.updated_at = 23;
    store.save_job(&retry).await.expect("retry should finish");

    let reopened = SqliteTranslationStore::new(&database);
    let complete = reopened
        .active_for_chapter(&ChapterId::from("chapter-1"))
        .await
        .expect("cache should reopen");
    assert!(
        complete
            .iter()
            .all(|row| row.state == TranslationRecordState::Ready)
    );
    assert_eq!(
        complete
            .iter()
            .map(|row| row.target_plain_text.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("第一段"), Some("第二段")]
    );
    let latest = reopened
        .latest_job(&ChapterId::from("chapter-1"), Some("plan-1"))
        .await
        .expect("job should load")
        .expect("job should exist");
    assert_eq!(latest.state, TranslationJobState::Succeeded);
    assert_eq!(latest.completed_block_ids.len(), 2);
}

#[derive(Clone)]
struct FixedParseStore {
    document: CanonicalDocument,
}

#[derive(Clone)]
struct MultiParseStore {
    documents: HashMap<DocumentId, CanonicalDocument>,
}

#[async_trait]
impl ParseStore for MultiParseStore {
    async fn active_document(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<CanonicalDocument>, AtlasError> {
        Ok(self.documents.get(document_id).cloned())
    }

    async fn latest_operation(
        &self,
        _document_id: &DocumentId,
        _backend: Option<&str>,
    ) -> Result<Option<ParseOperation>, AtlasError> {
        Ok(None)
    }

    async fn recoverable_operations(&self) -> Result<Vec<ParseOperation>, AtlasError> {
        Ok(Vec::new())
    }

    async fn save_operation(&self, _operation: &ParseOperation) -> Result<(), AtlasError> {
        Err(AtlasError::internal(
            "parse writes are not used by this test",
        ))
    }

    async fn supersede_operation(
        &self,
        _operation: &ParseOperation,
        _replacement: &ParseOperation,
    ) -> Result<(), AtlasError> {
        Err(AtlasError::internal(
            "parse writes are not used by this test",
        ))
    }

    async fn publish(&self, _artifact: &PublishArtifact) -> Result<(), AtlasError> {
        Err(AtlasError::internal(
            "parse writes are not used by this test",
        ))
    }
}

#[async_trait]
impl ParseStore for FixedParseStore {
    async fn active_document(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<CanonicalDocument>, AtlasError> {
        Ok((&self.document.document_id == document_id).then(|| self.document.clone()))
    }

    async fn latest_operation(
        &self,
        _document_id: &DocumentId,
        _backend: Option<&str>,
    ) -> Result<Option<ParseOperation>, AtlasError> {
        Ok(None)
    }

    async fn recoverable_operations(&self) -> Result<Vec<ParseOperation>, AtlasError> {
        Ok(Vec::new())
    }

    async fn save_operation(&self, _operation: &ParseOperation) -> Result<(), AtlasError> {
        Err(AtlasError::internal(
            "parse writes are not used by this test",
        ))
    }

    async fn supersede_operation(
        &self,
        _operation: &ParseOperation,
        _replacement: &ParseOperation,
    ) -> Result<(), AtlasError> {
        Err(AtlasError::internal(
            "parse writes are not used by this test",
        ))
    }

    async fn publish(&self, _artifact: &PublishArtifact) -> Result<(), AtlasError> {
        Err(AtlasError::internal(
            "parse writes are not used by this test",
        ))
    }
}

struct FixedConfiguration;

fn fixed_configuration() -> TranslationConfiguration {
    TranslationConfiguration {
        profile_id: "openai_compatible".to_owned(),
        endpoint_base_url: "https://models.example/v1".to_owned(),
        endpoint_fingerprint: "endpoint-1".to_owned(),
        model_id: "model-1".to_owned(),
        context_window: 32_768,
        credential: Some(TranslationCredential::new("not-a-real-key")),
    }
}

#[async_trait]
impl TranslationConfigurationPort for FixedConfiguration {
    async fn load(&self) -> Result<Option<TranslationConfiguration>, AtlasError> {
        Ok(Some(fixed_configuration()))
    }
}

fn canonical_block(id: &str, order_index: u32, source: &str) -> CanonicalBlock {
    CanonicalBlock {
        id: BlockId::from(id),
        order_index,
        kind: BlockKind::Paragraph,
        page_start: order_index.saturating_add(1),
        page_end: order_index.saturating_add(1),
        bounding_boxes: Vec::new(),
        content: StructuredContent::text(source),
        source_digest: match id {
            "block-1" => "source-0",
            "block-2" => "source-1",
            "block-3" => "source-3",
            _ => "source-4",
        }
        .to_owned(),
    }
}

fn canonical_document() -> CanonicalDocument {
    CanonicalDocument {
        schema_version: 1,
        artifact_id: "artifact-1".to_owned(),
        document_id: DocumentId::from("document-1"),
        source_sha256: "source-sha".to_owned(),
        parser: ParserIdentity {
            name: "Synthetic".to_owned(),
            version: "1".to_owned(),
            backend: "cloud_mineru".to_owned(),
        },
        normalizer_version: "1".to_owned(),
        page_count: 2,
        title: Some("Synthetic paper".to_owned()),
        chapters: vec![
            CanonicalChapter {
                id: ChapterId::from("chapter-1"),
                order_index: 0,
                depth: 1,
                role: ChapterRole::Body,
                source_title: "Introduction".to_owned(),
                page_start: 1,
                page_end: 1,
                blocks: vec![
                    canonical_block("block-1", 0, "Source 0"),
                    canonical_block("block-2", 1, "Source 1"),
                ],
            },
            CanonicalChapter {
                id: ChapterId::from("chapter-2"),
                order_index: 1,
                depth: 1,
                role: ChapterRole::Body,
                source_title: "Method".to_owned(),
                page_start: 2,
                page_end: 2,
                blocks: vec![canonical_block("block-3", 1, "Source 3")],
            },
        ],
        assets: Vec::new(),
    }
}

fn second_document() -> CanonicalDocument {
    CanonicalDocument {
        schema_version: 1,
        artifact_id: "artifact-2".to_owned(),
        document_id: DocumentId::from("document-2"),
        source_sha256: "source-sha-2".to_owned(),
        parser: ParserIdentity {
            name: "Synthetic".to_owned(),
            version: "1".to_owned(),
            backend: "cloud_mineru".to_owned(),
        },
        normalizer_version: "1".to_owned(),
        page_count: 1,
        title: Some("Second paper".to_owned()),
        chapters: vec![CanonicalChapter {
            id: ChapterId::from("chapter-4"),
            order_index: 0,
            depth: 1,
            role: ChapterRole::Body,
            source_title: "Overview".to_owned(),
            page_start: 1,
            page_end: 1,
            blocks: vec![canonical_block("block-5", 0, "Source 5")],
        }],
        assets: Vec::new(),
    }
}

async fn second_document_fixture(database: &AtlasDatabase) {
    sqlx::query(
        "INSERT INTO documents (
           id, sha256, title, authors_json, page_count, file_path,
           file_size_bytes, file_mtime_ms, file_state, created_at,
           updated_at, last_opened_at
         ) VALUES (
           'document-2', 'source-sha-2', 'Second paper', '[]', 1,
           '/tmp/second.pdf', 100, 1, 'available', 1, 1, 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("second document should insert");
    sqlx::query(
        "INSERT INTO jobs (
           id, session_id, document_id, kind, priority, state, input_json,
           attempt_count, max_attempts, run_after, created_at, updated_at
         ) VALUES (
           'parse-job-2', 'session-parse-2', 'document-2', 'cloud_parse', 100,
           'succeeded', '{}', 1, 1, 1, 1, 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("second parse job should insert");
    sqlx::query(
        "INSERT INTO parse_operations (
           id, job_id, document_id, backend, parser_version,
           normalizer_version, state, data_id, retry_count, created_at, updated_at,
           completed_at
         ) VALUES (
           'parse-operation-2', 'parse-job-2', 'document-2', 'cloud_mineru',
           'parser-1', 'normalizer-1', 'succeeded', 'data-2', 0, 1, 1, 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("second parse operation should insert");
    sqlx::query(
        "INSERT INTO parse_artifacts (
           id, document_id, parse_operation_id, parser_name, parser_version,
           normalizer_version, canonical_schema_version, source_sha256,
           content_digest, manifest_relative_path, is_active, created_at
         ) VALUES (
           'artifact-2', 'document-2', 'parse-operation-2', 'Synthetic',
           'parser-1', 'normalizer-1', 1, 'source-sha-2', 'digest-2',
           'document-2/artifact-2/document.json', 1, 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("second artifact should insert");
    sqlx::query(
        "INSERT INTO chapters (
           id, artifact_id, document_id, order_index, depth, role,
           source_title, page_start, page_end, source_digest, created_at
         ) VALUES (
           'chapter-4', 'artifact-2', 'document-2', 0, 1, 'body',
           'Overview', 1, 1, 'chapter-4-digest', 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("second chapter should insert");
    sqlx::query(
        "INSERT INTO blocks (
           id, chapter_id, order_index, kind, page_start, page_end,
           bounding_boxes_json, source_json, source_plain_text,
           source_digest, created_at
         ) VALUES (
           'block-5', 'chapter-4', 0, 'paragraph', 1, 1, '[]',
           '{\"plainText\":\"Source 5\",\"atoms\":[{\"type\":\"text\",\"value\":\"Source 5\"}]}',
           'Source 5', 'source-4', 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("second block should insert");
}

#[tokio::test]
async fn cache_completion_between_lookup_and_prepare_marks_replacement_succeeded() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let mut document = canonical_document();
    document.chapters.truncate(1);
    let inner = Arc::new(SqliteTranslationStore::new(&database));
    let provider = ScriptedTranslationAdapter::new([]);
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore { document }),
        Arc::new(CacheRaceStore {
            inner: inner.clone(),
        }),
        Arc::new(FixedConfiguration),
        Arc::new(provider.clone()),
    );
    let input = EnsureTranslationInput {
        session_id: SessionId::from("session-1"),
        document_id: DocumentId::from("document-1"),
        focused_chapter_id: ChapterId::from("chapter-1"),
    };

    let snapshot = module
        .ensure(input)
        .await
        .expect("cache race should settle without model work");
    let latest = inner
        .latest_job(&ChapterId::from("chapter-1"), None)
        .await
        .expect("job should load")
        .expect("job should exist");

    assert_eq!(latest.state, TranslationJobState::Succeeded);
    assert_eq!(
        snapshot.active_chapter.expect("chapter should exist").state,
        atlas_domain::TranslationState::Complete
    );
    assert!(provider.requests().is_empty());
}

#[tokio::test]
async fn module_translates_the_focused_chapter_then_prefetches_exactly_one_chapter() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let provider = ScriptedTranslationAdapter::new([
        Ok(ScriptedTranslationResponse {
            chunks: vec![
                "{\"id\":\"block-1\",\"target\":\"第一段\"}\n".to_owned(),
                "{\"id\":\"block-2\",\"target\":\"第二段\"}".to_owned(),
            ],
            finish_reason: Some("stop".to_owned()),
        }),
        Ok(ScriptedTranslationResponse {
            chunks: vec!["{\"id\":\"block-3\",\"target\":\"第三段\"}".to_owned()],
            finish_reason: Some("stop".to_owned()),
        }),
    ]);
    let store = Arc::new(SqliteTranslationStore::new(&database));
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore {
            document: canonical_document(),
        }),
        store.clone(),
        Arc::new(FixedConfiguration),
        Arc::new(provider.clone()),
    );
    let input = EnsureTranslationInput {
        session_id: SessionId::from("session-1"),
        document_id: DocumentId::from("document-1"),
        focused_chapter_id: ChapterId::from("chapter-1"),
    };

    module
        .ensure(input.clone())
        .await
        .expect("foreground translation should queue");
    let complete = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = module
                .ensure(input.clone())
                .await
                .expect("translation snapshot should load");
            if snapshot.active_chapter.as_ref().is_some_and(|chapter| {
                chapter.state == atlas_domain::TranslationState::Complete && !chapter.job_active
            }) && snapshot
                .prefetched_chapter_id
                .as_ref()
                .map(ChapterId::as_str)
                == Some("chapter-2")
            {
                break snapshot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("foreground and one prefetch should finish");

    assert_eq!(
        complete
            .active_chapter
            .expect("focused chapter should exist")
            .blocks
            .iter()
            .filter(|block| block.state == atlas_domain::BlockTranslationState::Ready)
            .count(),
        2
    );
    assert_eq!(provider.requests().len(), 2);
    let prefetched = store
        .active_for_chapter(&ChapterId::from("chapter-2"))
        .await
        .expect("prefetched cache should load");
    assert_eq!(prefetched[0].target_plain_text.as_deref(), Some("第三段"));

    module
        .ensure(input)
        .await
        .expect("cache hit should remain readable");
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert_eq!(
        provider.requests().len(),
        2,
        "a cache hit must not request the model again"
    );
}

#[tokio::test]
async fn module_commits_complete_records_and_repairs_only_the_malformed_tail() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let provider = ScriptedTranslationAdapter::new([
        Ok(ScriptedTranslationResponse {
            chunks: vec![
                "{\"id\":\"block-1\",\"target\":\"第一段\"}\n{\"id\":\"block-2\"".to_owned(),
            ],
            finish_reason: Some("stop".to_owned()),
        }),
        Ok(ScriptedTranslationResponse {
            chunks: vec!["{\"id\":\"block-2\",\"target\":\"第二段\"}".to_owned()],
            finish_reason: Some("stop".to_owned()),
        }),
        Ok(ScriptedTranslationResponse {
            chunks: vec!["{\"id\":\"block-3\",\"target\":\"第三段\"}".to_owned()],
            finish_reason: Some("stop".to_owned()),
        }),
    ]);
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore {
            document: canonical_document(),
        }),
        Arc::new(SqliteTranslationStore::new(&database)),
        Arc::new(FixedConfiguration),
        Arc::new(provider.clone()),
    );
    let input = EnsureTranslationInput {
        session_id: SessionId::from("session-1"),
        document_id: DocumentId::from("document-1"),
        focused_chapter_id: ChapterId::from("chapter-1"),
    };

    module
        .ensure(input.clone())
        .await
        .expect("foreground translation should queue");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = module
                .ensure(input.clone())
                .await
                .expect("translation snapshot should load");
            if snapshot.active_chapter.as_ref().is_some_and(|chapter| {
                chapter.state == atlas_domain::TranslationState::Complete && !chapter.job_active
            }) && snapshot
                .prefetched_chapter_id
                .as_ref()
                .map(ChapterId::as_str)
                == Some("chapter-2")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("repair and prefetch should finish");

    assert_eq!(
        provider.requests().len(),
        3,
        "one malformed foreground response should consume exactly one repair request"
    );
    assert!(
        !provider.requests()[1].input_json.contains("\"block-1\""),
        "the valid first record must not be translated again"
    );
    assert!(provider.requests()[1].input_json.contains("\"block-2\""));
}

#[tokio::test]
async fn a_failed_chapter_does_not_trigger_next_chapter_prefetch() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let provider = ScriptedTranslationAdapter::new([
        Ok(ScriptedTranslationResponse {
            chunks: vec!["not JSON".to_owned()],
            finish_reason: Some("stop".to_owned()),
        }),
        Ok(ScriptedTranslationResponse {
            chunks: vec!["still not JSON".to_owned()],
            finish_reason: Some("stop".to_owned()),
        }),
    ]);
    let store = Arc::new(SqliteTranslationStore::new(&database));
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore {
            document: canonical_document(),
        }),
        store.clone(),
        Arc::new(FixedConfiguration),
        Arc::new(provider.clone()),
    );
    let input = EnsureTranslationInput {
        session_id: SessionId::from("session-1"),
        document_id: DocumentId::from("document-1"),
        focused_chapter_id: ChapterId::from("chapter-1"),
    };
    module
        .ensure(input.clone())
        .await
        .expect("translation should queue");
    let failed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = module
                .ensure(input.clone())
                .await
                .expect("snapshot should load");
            if snapshot.active_chapter.as_ref().is_some_and(|chapter| {
                chapter.state == atlas_domain::TranslationState::Failed && !chapter.job_active
            }) {
                break snapshot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("failed chapter should settle");

    assert_eq!(
        failed.active_chapter.expect("chapter should exist").state,
        atlas_domain::TranslationState::Failed
    );
    assert_eq!(provider.requests().len(), 2);
    assert!(
        store
            .latest_job(&ChapterId::from("chapter-2"), None)
            .await
            .expect("prefetch query should succeed")
            .is_none()
    );
}

#[derive(Clone, Default)]
struct RepairCancellationProvider {
    repair_started: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl TranslationProviderPort for RepairCancellationProvider {
    async fn stream(
        &self,
        _configuration: &TranslationConfiguration,
        _request: ProviderTranslationRequest,
        sink: Arc<dyn TranslationChunkSink>,
        cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => {
                sink.push("{\"id\":\"block-1\",\"target\":\"第一段\"}")
                    .await
                    .expect("partial output should write");
                Ok(TranslationCompletion {
                    finish_reason: Some("stop".to_owned()),
                })
            }
            1 => {
                self.repair_started.notify_one();
                cancellation.cancelled().await;
                Err(TranslationProviderError::new(
                    TranslationProviderErrorKind::Cancelled,
                    "Translation was cancelled",
                ))
            }
            _ => {
                sink.push("{\"id\":\"block-3\",\"target\":\"第三段\"}")
                    .await
                    .expect("new foreground output should write");
                Ok(TranslationCompletion {
                    finish_reason: Some("stop".to_owned()),
                })
            }
        }
    }
}

#[tokio::test]
async fn cancellation_during_repair_persists_cancelled_not_failed() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let provider = RepairCancellationProvider::default();
    let store = Arc::new(SqliteTranslationStore::new(&database));
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore {
            document: canonical_document(),
        }),
        store.clone(),
        Arc::new(FixedConfiguration),
        Arc::new(provider.clone()),
    );
    module
        .ensure(EnsureTranslationInput {
            session_id: SessionId::from("session-1"),
            document_id: DocumentId::from("document-1"),
            focused_chapter_id: ChapterId::from("chapter-1"),
        })
        .await
        .expect("first chapter should queue");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        provider.repair_started.notified(),
    )
    .await
    .expect("repair should start");

    let focused = EnsureTranslationInput {
        session_id: SessionId::from("session-1"),
        document_id: DocumentId::from("document-1"),
        focused_chapter_id: ChapterId::from("chapter-2"),
    };
    module
        .ensure(focused.clone())
        .await
        .expect("new chapter should preempt repair");
    wait_for_complete(&module, &focused).await;

    assert_eq!(
        store
            .latest_job(&ChapterId::from("chapter-1"), None)
            .await
            .expect("old job should load")
            .expect("old job should exist")
            .state,
        TranslationJobState::Cancelled
    );
}

#[derive(Clone, Default)]
struct SwitchingProvider {
    first_started: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl TranslationProviderPort for SwitchingProvider {
    async fn stream(
        &self,
        _configuration: &TranslationConfiguration,
        _request: ProviderTranslationRequest,
        sink: Arc<dyn TranslationChunkSink>,
        cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            self.first_started.notify_one();
            cancellation.cancelled().await;
            return Err(TranslationProviderError::new(
                TranslationProviderErrorKind::Cancelled,
                "Translation was cancelled",
            ));
        }
        sink.push("{\"id\":\"block-3\",\"target\":\"第三段\"}")
            .await
            .expect("test sink should accept output");
        Ok(TranslationCompletion {
            finish_reason: Some("stop".to_owned()),
        })
    }
}

#[tokio::test]
async fn focusing_another_chapter_cancels_old_work_and_schedules_the_new_foreground() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let provider = SwitchingProvider::default();
    let store = Arc::new(SqliteTranslationStore::new(&database));
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore {
            document: canonical_document(),
        }),
        store.clone(),
        Arc::new(FixedConfiguration),
        Arc::new(provider.clone()),
    );
    module
        .ensure(EnsureTranslationInput {
            session_id: SessionId::from("session-1"),
            document_id: DocumentId::from("document-1"),
            focused_chapter_id: ChapterId::from("chapter-1"),
        })
        .await
        .expect("first chapter should queue");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        provider.first_started.notified(),
    )
    .await
    .expect("first chapter should reach the provider");

    let second = EnsureTranslationInput {
        session_id: SessionId::from("session-1"),
        document_id: DocumentId::from("document-1"),
        focused_chapter_id: ChapterId::from("chapter-2"),
    };
    module
        .ensure(second.clone())
        .await
        .expect("second chapter should take foreground priority");
    let complete = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = module
                .ensure(second.clone())
                .await
                .expect("second chapter snapshot should load");
            if snapshot.active_chapter.as_ref().is_some_and(|chapter| {
                chapter.state == atlas_domain::TranslationState::Complete && !chapter.job_active
            }) {
                break snapshot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("new foreground chapter should finish");

    assert_eq!(
        complete
            .active_chapter
            .expect("chapter should exist")
            .chapter_id
            .as_str(),
        "chapter-2"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        store
            .latest_job(&ChapterId::from("chapter-1"), None)
            .await
            .expect("old job should load")
            .expect("old job should exist")
            .state,
        TranslationJobState::Cancelled
    );
}

#[derive(Clone, Default)]
struct AbaProvider {
    first_a_started: Arc<Notify>,
    first_a_cancelled: Arc<Notify>,
    release_first_a: Arc<Notify>,
    a_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl TranslationProviderPort for AbaProvider {
    async fn stream(
        &self,
        _configuration: &TranslationConfiguration,
        request: ProviderTranslationRequest,
        sink: Arc<dyn TranslationChunkSink>,
        cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        if request.input_json.contains("\"block-3\"") {
            sink.push("{\"id\":\"block-3\",\"target\":\"第三段\"}")
                .await
                .expect("chapter B output should write");
            return Ok(TranslationCompletion {
                finish_reason: Some("stop".to_owned()),
            });
        }
        if self.a_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.first_a_started.notify_one();
            cancellation.cancelled().await;
            self.first_a_cancelled.notify_one();
            self.release_first_a.notified().await;
            return Err(TranslationProviderError::new(
                TranslationProviderErrorKind::Cancelled,
                "Translation was cancelled",
            ));
        }
        sink.push(
            "{\"id\":\"block-1\",\"target\":\"第一段\"}\n{\"id\":\"block-2\",\"target\":\"第二段\"}",
        )
        .await
        .expect("replacement A output should write");
        Ok(TranslationCompletion {
            finish_reason: Some("stop".to_owned()),
        })
    }
}

#[tokio::test]
async fn returning_to_a_cancelled_in_flight_chapter_schedules_replacement_work() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let provider = AbaProvider::default();
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore {
            document: canonical_document(),
        }),
        Arc::new(SqliteTranslationStore::new(&database)),
        Arc::new(FixedConfiguration),
        Arc::new(provider.clone()),
    );
    let chapter_a = EnsureTranslationInput {
        session_id: SessionId::from("session-1"),
        document_id: DocumentId::from("document-1"),
        focused_chapter_id: ChapterId::from("chapter-1"),
    };
    module
        .ensure(chapter_a.clone())
        .await
        .expect("first A should queue");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        provider.first_a_started.notified(),
    )
    .await
    .expect("first A should start");
    module
        .ensure(EnsureTranslationInput {
            session_id: SessionId::from("session-1"),
            document_id: DocumentId::from("document-1"),
            focused_chapter_id: ChapterId::from("chapter-2"),
        })
        .await
        .expect("B should preempt A");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        provider.first_a_cancelled.notified(),
    )
    .await
    .expect("first A should observe cancellation");

    module
        .ensure(chapter_a.clone())
        .await
        .expect("second A intent should queue a replacement");
    provider.release_first_a.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = module
                .view(chapter_a.clone())
                .await
                .expect("A snapshot should load");
            if snapshot
                .active_chapter
                .as_ref()
                .is_some_and(|chapter| chapter.state == atlas_domain::TranslationState::Complete)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("replacement A should finish");

    assert_eq!(provider.a_calls.load(Ordering::SeqCst), 2);
}

#[derive(Clone)]
struct MutableConfiguration {
    value: Arc<RwLock<TranslationConfiguration>>,
}

#[async_trait]
impl TranslationConfigurationPort for MutableConfiguration {
    async fn load(&self) -> Result<Option<TranslationConfiguration>, AtlasError> {
        Ok(Some(self.value.read().await.clone()))
    }
}

#[tokio::test]
async fn switching_back_to_a_model_reactivates_its_inactive_cache() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let mut document = canonical_document();
    document.chapters.truncate(1);
    let configuration = Arc::new(RwLock::new(fixed_configuration()));
    let provider = ScriptedTranslationAdapter::new([
        Ok(ScriptedTranslationResponse {
            chunks: vec![
                "{\"id\":\"block-1\",\"target\":\"模型甲第一段\"}\n".to_owned(),
                "{\"id\":\"block-2\",\"target\":\"模型甲第二段\"}".to_owned(),
            ],
            finish_reason: Some("stop".to_owned()),
        }),
        Ok(ScriptedTranslationResponse {
            chunks: vec![
                "{\"id\":\"block-1\",\"target\":\"模型乙第一段\"}\n".to_owned(),
                "{\"id\":\"block-2\",\"target\":\"模型乙第二段\"}".to_owned(),
            ],
            finish_reason: Some("stop".to_owned()),
        }),
    ]);
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore { document }),
        Arc::new(SqliteTranslationStore::new(&database)),
        Arc::new(MutableConfiguration {
            value: configuration.clone(),
        }),
        Arc::new(provider.clone()),
    );
    let input = EnsureTranslationInput {
        session_id: SessionId::from("session-1"),
        document_id: DocumentId::from("document-1"),
        focused_chapter_id: ChapterId::from("chapter-1"),
    };

    module
        .ensure(input.clone())
        .await
        .expect("model A should queue");
    wait_for_complete(&module, &input).await;
    {
        let mut selected = configuration.write().await;
        selected.endpoint_fingerprint = "endpoint-2".to_owned();
        selected.model_id = "model-2".to_owned();
    }
    module
        .ensure(input.clone())
        .await
        .expect("model B should queue");
    wait_for_complete(&module, &input).await;
    {
        let mut selected = configuration.write().await;
        *selected = fixed_configuration();
    }
    let restored = module
        .ensure(input)
        .await
        .expect("model A cache should reactivate");

    let chapter = restored
        .active_chapter
        .expect("active chapter should exist");
    assert_eq!(chapter.state, atlas_domain::TranslationState::Complete);
    assert_eq!(
        chapter.blocks[0]
            .target
            .as_ref()
            .map(|target| target.plain_text.as_str()),
        Some("模型甲第一段")
    );
    assert_eq!(
        provider.requests().len(),
        2,
        "reactivating model A must not call the provider again"
    );
}

async fn wait_for_complete(
    module: &DefaultTranslationModule,
    input: &EnsureTranslationInput,
) -> atlas_domain::TranslationSnapshot {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = module
                .ensure(input.clone())
                .await
                .expect("translation snapshot should load");
            if snapshot.active_chapter.as_ref().is_some_and(|chapter| {
                chapter.state == atlas_domain::TranslationState::Complete && !chapter.job_active
            }) {
                break snapshot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("translation should finish")
}

#[derive(Clone, Default)]
struct PrefetchPromotionProvider {
    prefetch_started: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl TranslationProviderPort for PrefetchPromotionProvider {
    async fn stream(
        &self,
        _configuration: &TranslationConfiguration,
        _request: ProviderTranslationRequest,
        sink: Arc<dyn TranslationChunkSink>,
        cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => {
                sink.push(
                    "{\"id\":\"block-1\",\"target\":\"第一段\"}\n{\"id\":\"block-2\",\"target\":\"第二段\"}",
                )
                .await
                .expect("foreground output should write");
                Ok(TranslationCompletion {
                    finish_reason: Some("stop".to_owned()),
                })
            }
            1 => {
                self.prefetch_started.notify_one();
                cancellation.cancelled().await;
                Err(TranslationProviderError::new(
                    TranslationProviderErrorKind::Cancelled,
                    "Translation was cancelled",
                ))
            }
            _ => {
                sink.push("{\"id\":\"block-3\",\"target\":\"第三段\"}")
                    .await
                    .expect("promoted output should write");
                Ok(TranslationCompletion {
                    finish_reason: Some("stop".to_owned()),
                })
            }
        }
    }
}

#[tokio::test]
async fn focusing_an_active_prefetch_uses_a_separately_fenced_foreground_job() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let provider = PrefetchPromotionProvider::default();
    let store = Arc::new(SqliteTranslationStore::new(&database));
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore {
            document: canonical_document(),
        }),
        store.clone(),
        Arc::new(FixedConfiguration),
        Arc::new(provider.clone()),
    );
    module
        .ensure(EnsureTranslationInput {
            session_id: SessionId::from("session-1"),
            document_id: DocumentId::from("document-1"),
            focused_chapter_id: ChapterId::from("chapter-1"),
        })
        .await
        .expect("foreground should queue");
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        provider.prefetch_started.notified(),
    )
    .await
    .expect("prefetch should start");
    module
        .view(EnsureTranslationInput {
            session_id: SessionId::from("session-1"),
            document_id: DocumentId::from("document-1"),
            focused_chapter_id: ChapterId::from("chapter-1"),
        })
        .await
        .expect("snapshot view should not express foreground intent");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "read-only polling must leave the active prefetch running"
    );

    let focused = EnsureTranslationInput {
        session_id: SessionId::from("session-1"),
        document_id: DocumentId::from("document-1"),
        focused_chapter_id: ChapterId::from("chapter-2"),
    };
    module
        .ensure(focused.clone())
        .await
        .expect("prefetch should promote");
    wait_for_complete(&module, &focused).await;

    let latest = store
        .latest_job(&ChapterId::from("chapter-2"), None)
        .await
        .expect("chapter job should load")
        .expect("chapter job should exist");
    assert_eq!(latest.kind, TranslationJobKind::Foreground);
    assert_eq!(latest.state, TranslationJobState::Succeeded);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    let cancelled_prefetches = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)
         FROM jobs
         WHERE chapter_id = 'chapter-2' AND kind = 'prefetch' AND state = 'cancelled'",
    )
    .fetch_one(database.pool())
    .await
    .expect("prefetch count should load");
    assert_eq!(cancelled_prefetches, 1);
}

#[tokio::test]
async fn replacing_an_active_prefetch_uses_a_new_job_fence() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let provider = DelayedPrefetchCancellationProvider::default();
    let store = Arc::new(SqliteTranslationStore::new(&database));
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore {
            document: canonical_document(),
        }),
        store.clone(),
        Arc::new(FixedConfiguration),
        Arc::new(provider.clone()),
    );
    let chapter_a = EnsureTranslationInput {
        session_id: SessionId::from("session-1"),
        document_id: DocumentId::from("document-1"),
        focused_chapter_id: ChapterId::from("chapter-1"),
    };
    module
        .ensure(chapter_a.clone())
        .await
        .expect("foreground should queue");
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        provider.prefetch_started.notified(),
    )
    .await
    .expect("prefetch should start");

    module
        .ensure(chapter_a)
        .await
        .expect("renewed foreground intent should replace its prefetch");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        provider.prefetch_cancelled.notified(),
    )
    .await
    .expect("old prefetch should observe cancellation");
    provider.release_prefetch.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = module
                .view(EnsureTranslationInput {
                    session_id: SessionId::from("session-1"),
                    document_id: DocumentId::from("document-1"),
                    focused_chapter_id: ChapterId::from("chapter-2"),
                })
                .await
                .expect("prefetch snapshot should load");
            if snapshot
                .active_chapter
                .as_ref()
                .is_some_and(|chapter| chapter.state == atlas_domain::TranslationState::Complete)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("replacement prefetch should finish");

    let jobs = sqlx::query_as::<_, (String, String)>(
        "SELECT id, state
         FROM jobs
         WHERE chapter_id = 'chapter-2' AND kind = 'prefetch'
         ORDER BY rowid",
    )
    .fetch_all(database.pool())
    .await
    .expect("prefetch jobs should load");
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].1, "cancelled");
    assert_eq!(jobs[1].1, "succeeded");
    assert_ne!(jobs[0].0, jobs[1].0);
}

#[derive(Clone, Default)]
struct DelayedPrefetchCancellationProvider {
    prefetch_started: Arc<Notify>,
    prefetch_cancelled: Arc<Notify>,
    release_prefetch: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl TranslationProviderPort for DelayedPrefetchCancellationProvider {
    async fn stream(
        &self,
        _configuration: &TranslationConfiguration,
        _request: ProviderTranslationRequest,
        sink: Arc<dyn TranslationChunkSink>,
        cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => {
                sink.push(
                    "{\"id\":\"block-1\",\"target\":\"第一段\"}\n{\"id\":\"block-2\",\"target\":\"第二段\"}",
                )
                .await
                .expect("foreground output should write");
                Ok(TranslationCompletion {
                    finish_reason: Some("stop".to_owned()),
                })
            }
            1 => {
                self.prefetch_started.notify_one();
                cancellation.cancelled().await;
                self.prefetch_cancelled.notify_one();
                self.release_prefetch.notified().await;
                Err(TranslationProviderError::new(
                    TranslationProviderErrorKind::Cancelled,
                    "Translation was cancelled",
                ))
            }
            _ => {
                sink.push("{\"id\":\"block-3\",\"target\":\"第三段\"}")
                    .await
                    .expect("replacement prefetch output should write");
                Ok(TranslationCompletion {
                    finish_reason: Some("stop".to_owned()),
                })
            }
        }
    }
}

#[derive(Clone, Default)]
struct CrossDocumentPrefetchProvider {
    prefetch_started: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl TranslationProviderPort for CrossDocumentPrefetchProvider {
    async fn stream(
        &self,
        _configuration: &TranslationConfiguration,
        _request: ProviderTranslationRequest,
        sink: Arc<dyn TranslationChunkSink>,
        cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => {
                sink.push(
                    "{\"id\":\"block-1\",\"target\":\"第一段\"}\n{\"id\":\"block-2\",\"target\":\"第二段\"}",
                )
                .await
                .expect("first foreground output should write");
                Ok(TranslationCompletion {
                    finish_reason: Some("stop".to_owned()),
                })
            }
            1 => {
                self.prefetch_started.notify_one();
                cancellation.cancelled().await;
                Err(TranslationProviderError::new(
                    TranslationProviderErrorKind::Cancelled,
                    "Translation was cancelled",
                ))
            }
            _ => {
                sink.push("{\"id\":\"block-5\",\"target\":\"第五段\"}")
                    .await
                    .expect("second foreground output should write");
                Ok(TranslationCompletion {
                    finish_reason: Some("stop".to_owned()),
                })
            }
        }
    }
}

#[tokio::test]
async fn foreground_work_preempts_a_prefetch_from_another_document() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    second_document_fixture(&database).await;
    let first = canonical_document();
    let second = second_document();
    let provider = CrossDocumentPrefetchProvider::default();
    let store = Arc::new(SqliteTranslationStore::new(&database));
    let module = DefaultTranslationModule::new(
        Arc::new(MultiParseStore {
            documents: HashMap::from([
                (first.document_id.clone(), first),
                (second.document_id.clone(), second),
            ]),
        }),
        store.clone(),
        Arc::new(FixedConfiguration),
        Arc::new(provider.clone()),
    );
    module
        .ensure(EnsureTranslationInput {
            session_id: SessionId::from("session-1"),
            document_id: DocumentId::from("document-1"),
            focused_chapter_id: ChapterId::from("chapter-1"),
        })
        .await
        .expect("first document should queue");
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        provider.prefetch_started.notified(),
    )
    .await
    .expect("first document prefetch should start");

    let second_input = EnsureTranslationInput {
        session_id: SessionId::from("session-2"),
        document_id: DocumentId::from("document-2"),
        focused_chapter_id: ChapterId::from("chapter-4"),
    };
    module
        .ensure(second_input.clone())
        .await
        .expect("second document foreground should queue");
    wait_for_complete(&module, &second_input).await;

    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        store
            .latest_job(&ChapterId::from("chapter-2"), None)
            .await
            .expect("prefetch should load")
            .expect("prefetch should exist")
            .state,
        TranslationJobState::Cancelled
    );
}

#[derive(Clone, Default)]
struct PartialThenErrorProvider {
    requests: Arc<RwLock<Vec<String>>>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl TranslationProviderPort for PartialThenErrorProvider {
    async fn stream(
        &self,
        _configuration: &TranslationConfiguration,
        request: ProviderTranslationRequest,
        sink: Arc<dyn TranslationChunkSink>,
        _cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        self.requests.write().await.push(request.input_json);
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            sink.push("{\"id\":\"block-1\",\"target\":\"第一段\"}\n")
                .await
                .expect("partial output should write");
            Err(TranslationProviderError::new(
                TranslationProviderErrorKind::Timeout,
                "The stream stopped",
            ))
        } else {
            sink.push("{\"id\":\"block-2\",\"target\":\"第二段\"}")
                .await
                .expect("repair output should write");
            Ok(TranslationCompletion {
                finish_reason: Some("stop".to_owned()),
            })
        }
    }
}

#[tokio::test]
async fn complete_records_before_a_stream_error_are_committed_not_retranslated() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let mut document = canonical_document();
    document.chapters.truncate(1);
    let provider = PartialThenErrorProvider::default();
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore { document }),
        Arc::new(SqliteTranslationStore::new(&database)),
        Arc::new(FixedConfiguration),
        Arc::new(provider.clone()),
    );
    let input = EnsureTranslationInput {
        session_id: SessionId::from("session-1"),
        document_id: DocumentId::from("document-1"),
        focused_chapter_id: ChapterId::from("chapter-1"),
    };
    module
        .ensure(input.clone())
        .await
        .expect("translation should queue");
    wait_for_complete(&module, &input).await;

    let requests = provider.requests.read().await;
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("\"block-1\""));
    assert!(!requests[1].contains("\"block-1\""));
    assert!(requests[1].contains("\"block-2\""));
}

struct SmallContextConfiguration;

#[async_trait]
impl TranslationConfigurationPort for SmallContextConfiguration {
    async fn load(&self) -> Result<Option<TranslationConfiguration>, AtlasError> {
        let mut configuration = fixed_configuration();
        configuration.context_window = 1_024;
        Ok(Some(configuration))
    }
}

#[tokio::test]
async fn one_oversized_block_does_not_fail_the_rest_of_the_chapter() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let mut document = canonical_document();
    document.chapters.truncate(1);
    document.chapters[0].blocks[1].content = StructuredContent::text("x".repeat(20_000));
    let provider = ScriptedTranslationAdapter::new([Ok(ScriptedTranslationResponse {
        chunks: vec!["{\"id\":\"block-1\",\"target\":\"第一段\"}".to_owned()],
        finish_reason: Some("stop".to_owned()),
    })]);
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore { document }),
        Arc::new(SqliteTranslationStore::new(&database)),
        Arc::new(SmallContextConfiguration),
        Arc::new(provider.clone()),
    );
    let input = EnsureTranslationInput {
        session_id: SessionId::from("session-1"),
        document_id: DocumentId::from("document-1"),
        focused_chapter_id: ChapterId::from("chapter-1"),
    };
    module
        .ensure(input.clone())
        .await
        .expect("translation should queue");
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = module
                .ensure(input.clone())
                .await
                .expect("snapshot should load");
            if snapshot
                .active_chapter
                .as_ref()
                .is_some_and(|chapter| !chapter.job_active)
            {
                break snapshot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("partial chapter should settle");

    let chapter = snapshot
        .active_chapter
        .expect("active chapter should exist");
    assert_eq!(chapter.state, atlas_domain::TranslationState::Readable);
    assert_eq!(
        chapter.blocks[0].state,
        atlas_domain::BlockTranslationState::Ready
    );
    assert_eq!(
        chapter.blocks[1].state,
        atlas_domain::BlockTranslationState::Failed
    );
    assert_eq!(provider.requests().len(), 1);
}

fn recovery_plan(
    document: &CanonicalDocument,
) -> (String, Vec<NewTranslationRecord>, Vec<BlockId>) {
    recovery_plan_for_chapter(document, 0)
}

fn recovery_plan_for_chapter(
    document: &CanonicalDocument,
    chapter_index: usize,
) -> (String, Vec<NewTranslationRecord>, Vec<BlockId>) {
    let configuration = fixed_configuration();
    let planner = TranslationPlanner::new();
    let blocks = document.chapters[chapter_index]
        .blocks
        .iter()
        .map(|block| {
            planner
                .prepare(block, &configuration)
                .expect("recovery block should prepare")
        })
        .collect::<Vec<_>>();
    let mut hasher = Sha256::new();
    for block in &blocks {
        hasher.update(block.request_digest.len().to_be_bytes());
        hasher.update(block.request_digest.as_bytes());
    }

    let records = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| NewTranslationRecord {
            id: format!("recovery-translation-{index}"),
            block_id: block.block_id.clone(),
            request_digest: block.request_digest.clone(),
            source_digest: block.source_digest.clone(),
            target_locale: "zh-CN".to_owned(),
            endpoint_origin: configuration.endpoint_base_url.clone(),
            provider_profile_fingerprint: configuration.endpoint_fingerprint.clone(),
            model_id: configuration.model_id.clone(),
            prompt_version: "academic-blocks-v1".to_owned(),
            applicable_preference_digest: String::new(),
            created_at: 30,
        })
        .collect();
    (
        hex::encode(hasher.finalize()),
        records,
        blocks.into_iter().map(|block| block.block_id).collect(),
    )
}

async fn add_third_chapter_fixture(database: &AtlasDatabase) {
    sqlx::query(
        "INSERT INTO chapters (
           id, artifact_id, document_id, order_index, depth, role,
           source_title, page_start, page_end, source_digest, created_at
         ) VALUES (
           'chapter-3', 'artifact-1', 'document-1', 2, 1, 'body',
           'Results', 3, 3, 'chapter-3-digest', 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("third chapter should insert");
    sqlx::query(
        "INSERT INTO blocks (
           id, chapter_id, order_index, kind, page_start, page_end,
           bounding_boxes_json, source_json, source_plain_text,
           source_digest, created_at
         ) VALUES (
           'block-4', 'chapter-3', 0, 'paragraph', 3, 3, '[]',
           '{\"plainText\":\"Source 4\",\"atoms\":[{\"type\":\"text\",\"value\":\"Source 4\"}]}',
           'Source 4', 'source-4', 1
         )",
    )
    .execute(database.pool())
    .await
    .expect("third block should insert");
}

#[tokio::test]
async fn recovery_resumes_the_same_job_and_consumes_the_interrupted_row() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let mut document = canonical_document();
    document.chapters.truncate(1);
    let (plan_digest, records, block_ids) = recovery_plan(&document);
    let store = Arc::new(SqliteTranslationStore::new(&database));
    let queued = TranslationJob {
        id: JobId::from("recover-same-job"),
        session_id: SessionId::from("session-recovery"),
        document_id: DocumentId::from("document-1"),
        chapter_id: ChapterId::from("chapter-1"),
        kind: TranslationJobKind::Foreground,
        state: TranslationJobState::Queued,
        plan_digest,
        endpoint_fingerprint: "endpoint-1".to_owned(),
        model_id: "model-1".to_owned(),
        block_ids,
        completed_block_ids: Vec::new(),
        attempt_count: 0,
        error_code: None,
        safe_message: None,
        created_at: 30,
        updated_at: 30,
        completed_at: None,
    };
    store
        .prepare_job(&queued, &records)
        .await
        .expect("recoverable job should persist");
    let provider = ScriptedTranslationAdapter::new([Ok(ScriptedTranslationResponse {
        chunks: vec![
            "{\"id\":\"block-1\",\"target\":\"第一段\"}\n".to_owned(),
            "{\"id\":\"block-2\",\"target\":\"第二段\"}".to_owned(),
        ],
        finish_reason: Some("stop".to_owned()),
    })]);
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore { document }),
        store.clone(),
        Arc::new(FixedConfiguration),
        Arc::new(provider),
    );

    assert_eq!(module.recover().await.expect("recovery should run"), 1);
    let recovered = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let job = store
                .latest_job(&ChapterId::from("chapter-1"), None)
                .await
                .expect("job should load")
                .expect("job should exist");
            if job.state == TranslationJobState::Succeeded {
                break job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("recovered job should finish");

    assert_eq!(recovered.id.as_str(), "recover-same-job");
    assert!(
        store
            .recoverable()
            .await
            .expect("recovery query should succeed")
            .is_empty()
    );
}

#[tokio::test]
async fn startup_recovery_leaves_prefetch_interrupted_without_spending_model_work() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    add_third_chapter_fixture(&database).await;
    let mut document = canonical_document();
    document.page_count = 3;
    document.chapters.push(CanonicalChapter {
        id: ChapterId::from("chapter-3"),
        order_index: 2,
        depth: 1,
        role: ChapterRole::Body,
        source_title: "Results".to_owned(),
        page_start: 3,
        page_end: 3,
        blocks: vec![canonical_block("block-4", 0, "Source 4")],
    });
    let (plan_digest, records, block_ids) = recovery_plan_for_chapter(&document, 1);
    let store = Arc::new(SqliteTranslationStore::new(&database));
    store
        .prepare_job(
            &TranslationJob {
                id: JobId::from("recover-prefetch"),
                session_id: SessionId::from("session-recovery"),
                document_id: DocumentId::from("document-1"),
                chapter_id: ChapterId::from("chapter-2"),
                kind: TranslationJobKind::Prefetch,
                state: TranslationJobState::Queued,
                plan_digest,
                endpoint_fingerprint: "endpoint-1".to_owned(),
                model_id: "model-1".to_owned(),
                block_ids,
                completed_block_ids: Vec::new(),
                attempt_count: 0,
                error_code: None,
                safe_message: None,
                created_at: 35,
                updated_at: 35,
                completed_at: None,
            },
            &records,
        )
        .await
        .expect("prefetch should persist");
    let provider = ScriptedTranslationAdapter::new([Ok(ScriptedTranslationResponse {
        chunks: vec!["{\"id\":\"block-3\",\"target\":\"第三段\"}".to_owned()],
        finish_reason: Some("stop".to_owned()),
    })]);
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore { document }),
        store.clone(),
        Arc::new(FixedConfiguration),
        Arc::new(provider.clone()),
    );

    assert_eq!(
        module
            .recover()
            .await
            .expect("recovery should inspect jobs"),
        0
    );
    assert_eq!(
        store
            .latest_job(&ChapterId::from("chapter-2"), None)
            .await
            .expect("prefetch should load")
            .expect("prefetch should exist")
            .state,
        TranslationJobState::Interrupted
    );
    assert!(provider.requests().is_empty());
    assert!(
        store
            .latest_job(&ChapterId::from("chapter-3"), None)
            .await
            .expect("third chapter query should succeed")
            .is_none()
    );
}

#[derive(Clone)]
struct ToggleConfiguration {
    available: Arc<AtomicBool>,
}

#[async_trait]
impl TranslationConfigurationPort for ToggleConfiguration {
    async fn load(&self) -> Result<Option<TranslationConfiguration>, AtlasError> {
        Ok(self
            .available
            .load(Ordering::SeqCst)
            .then(fixed_configuration))
    }
}

#[tokio::test]
async fn unavailable_configuration_defers_recovery_until_a_later_ensure() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let mut document = canonical_document();
    document.chapters.truncate(1);
    let (plan_digest, records, block_ids) = recovery_plan(&document);
    let store = Arc::new(SqliteTranslationStore::new(&database));
    let queued = TranslationJob {
        id: JobId::from("deferred-recovery-job"),
        session_id: SessionId::from("session-recovery"),
        document_id: DocumentId::from("document-1"),
        chapter_id: ChapterId::from("chapter-1"),
        kind: TranslationJobKind::Foreground,
        state: TranslationJobState::Queued,
        plan_digest,
        endpoint_fingerprint: "endpoint-1".to_owned(),
        model_id: "model-1".to_owned(),
        block_ids,
        completed_block_ids: Vec::new(),
        attempt_count: 0,
        error_code: None,
        safe_message: None,
        created_at: 40,
        updated_at: 40,
        completed_at: None,
    };
    store
        .prepare_job(&queued, &records)
        .await
        .expect("recoverable job should persist");
    let available = Arc::new(AtomicBool::new(false));
    let provider = ScriptedTranslationAdapter::new([Ok(ScriptedTranslationResponse {
        chunks: vec![
            "{\"id\":\"block-1\",\"target\":\"第一段\"}\n".to_owned(),
            "{\"id\":\"block-2\",\"target\":\"第二段\"}".to_owned(),
        ],
        finish_reason: Some("stop".to_owned()),
    })]);
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore { document }),
        store.clone(),
        Arc::new(ToggleConfiguration {
            available: available.clone(),
        }),
        Arc::new(provider),
    );

    assert_eq!(
        module
            .recover()
            .await
            .expect("deferred recovery should not fail"),
        0
    );
    assert_eq!(
        store
            .latest_job(&ChapterId::from("chapter-1"), None)
            .await
            .expect("job should load")
            .expect("job should exist")
            .state,
        TranslationJobState::Interrupted
    );

    available.store(true, Ordering::SeqCst);
    let input = EnsureTranslationInput {
        session_id: SessionId::from("session-recovery"),
        document_id: DocumentId::from("document-1"),
        focused_chapter_id: ChapterId::from("chapter-1"),
    };
    module
        .ensure(input.clone())
        .await
        .expect("later ensure should claim recovery");
    let recovered = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let job = store
                .latest_job(&ChapterId::from("chapter-1"), None)
                .await
                .expect("job should load")
                .expect("job should exist");
            if job.state == TranslationJobState::Succeeded {
                break job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("deferred job should finish");

    assert_eq!(recovered.id.as_str(), "deferred-recovery-job");
}

#[tokio::test]
async fn recovery_supersedes_an_incompatible_old_plan() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let mut document = canonical_document();
    document.chapters.truncate(1);
    let (_, records, block_ids) = recovery_plan(&document);
    let store = Arc::new(SqliteTranslationStore::new(&database));
    store
        .prepare_job(
            &TranslationJob {
                id: JobId::from("obsolete-recovery-job"),
                session_id: SessionId::from("session-recovery"),
                document_id: DocumentId::from("document-1"),
                chapter_id: ChapterId::from("chapter-1"),
                kind: TranslationJobKind::Foreground,
                state: TranslationJobState::Queued,
                plan_digest: "obsolete-plan".to_owned(),
                endpoint_fingerprint: "old-endpoint".to_owned(),
                model_id: "old-model".to_owned(),
                block_ids,
                completed_block_ids: Vec::new(),
                attempt_count: 0,
                error_code: None,
                safe_message: None,
                created_at: 50,
                updated_at: 50,
                completed_at: None,
            },
            &records,
        )
        .await
        .expect("old job should persist");
    let provider = ScriptedTranslationAdapter::new([Ok(ScriptedTranslationResponse {
        chunks: vec![
            "{\"id\":\"block-1\",\"target\":\"第一段\"}\n".to_owned(),
            "{\"id\":\"block-2\",\"target\":\"第二段\"}".to_owned(),
        ],
        finish_reason: Some("stop".to_owned()),
    })]);
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore { document }),
        store.clone(),
        Arc::new(FixedConfiguration),
        Arc::new(provider),
    );

    assert_eq!(module.recover().await.expect("recovery should run"), 1);
    let old_state = sqlx::query_scalar::<_, String>("SELECT state FROM jobs WHERE id = ?1")
        .bind("obsolete-recovery-job")
        .fetch_one(database.pool())
        .await
        .expect("old state should load");
    assert_eq!(old_state, "cancelled");
    let replacement = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let job = store
                .latest_job(&ChapterId::from("chapter-1"), None)
                .await
                .expect("replacement should load")
                .expect("replacement should exist");
            if job.state == TranslationJobState::Succeeded {
                break job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("replacement should finish");

    assert_ne!(replacement.id.as_str(), "obsolete-recovery-job");
    assert!(
        store
            .recoverable()
            .await
            .expect("recovery query should succeed")
            .is_empty()
    );
}

#[derive(Clone, Default)]
struct BlockingProvider {
    started: Arc<Notify>,
}

#[async_trait]
impl TranslationProviderPort for BlockingProvider {
    async fn stream(
        &self,
        _configuration: &TranslationConfiguration,
        _request: ProviderTranslationRequest,
        _sink: Arc<dyn TranslationChunkSink>,
        cancellation: CancellationToken,
    ) -> Result<TranslationCompletion, TranslationProviderError> {
        self.started.notify_one();
        cancellation.cancelled().await;
        Err(TranslationProviderError::new(
            TranslationProviderErrorKind::Cancelled,
            "Translation was cancelled",
        ))
    }
}

#[tokio::test]
async fn closing_a_document_persists_cancellation_instead_of_failure() {
    let database = AtlasDatabase::open_in_memory()
        .await
        .expect("database should open");
    fixture(&database).await;
    let provider = BlockingProvider::default();
    let store = Arc::new(SqliteTranslationStore::new(&database));
    let module = DefaultTranslationModule::new(
        Arc::new(FixedParseStore {
            document: canonical_document(),
        }),
        store.clone(),
        Arc::new(FixedConfiguration),
        Arc::new(provider.clone()),
    );
    module
        .ensure(EnsureTranslationInput {
            session_id: SessionId::from("session-1"),
            document_id: DocumentId::from("document-1"),
            focused_chapter_id: ChapterId::from("chapter-1"),
        })
        .await
        .expect("foreground translation should queue");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        provider.started.notified(),
    )
    .await
    .expect("provider should start");

    module
        .close_document(&DocumentId::from("document-1"))
        .await
        .expect("cancellation should persist");
    let cancelled = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let latest = store
                .latest_job(&ChapterId::from("chapter-1"), None)
                .await
                .expect("job should load")
                .expect("job should exist");
            if latest.state == TranslationJobState::Cancelled {
                break latest;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancellation should persist");

    assert_eq!(cancelled.error_code.as_deref(), Some("cancelled"));
}
