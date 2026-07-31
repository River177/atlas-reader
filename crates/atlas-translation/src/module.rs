use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use atlas_domain::{
    AtlasError, BlockTranslationState, CanonicalBlock, CanonicalChapter, CanonicalDocument,
    ChapterId, ChapterRole, ChapterTranslationView, DocumentId, JobId, SessionId,
    TranslatedBlockView, TranslationSnapshot, TranslationState,
};
use atlas_parse::ParseStore;
use sha2::{Digest, Sha256};
use tokio::{
    sync::{Mutex, Semaphore},
    time::sleep,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    CommittedTranslation, NewTranslationRecord, OutputValidation, PROMPT_VERSION, PreparedBlock,
    TARGET_LOCALE, TranslationBatch, TranslationChunkSink, TranslationConfiguration,
    TranslationConfigurationPort, TranslationJob, TranslationJobKind, TranslationJobState,
    TranslationOutputParser, TranslationPlanner, TranslationProviderError,
    TranslationProviderErrorKind, TranslationProviderPort, TranslationRecordState,
    TranslationStore, ValidationFailure, validate_output,
};

const PROVIDER_ATTEMPTS: u32 = 3;

#[derive(Clone, Debug)]
pub struct EnsureTranslationInput {
    pub session_id: SessionId,
    pub document_id: DocumentId,
    pub focused_chapter_id: ChapterId,
}

#[derive(Clone, Debug)]
pub struct RetryTranslationInput {
    pub session_id: SessionId,
    pub document_id: DocumentId,
    pub chapter_id: ChapterId,
}

#[async_trait]
pub trait TranslationModule: Send + Sync {
    /// Returns the best committed projection immediately. A cache miss is
    /// durably queued before background model work starts.
    async fn ensure(
        &self,
        input: EnsureTranslationInput,
    ) -> Result<TranslationSnapshot, AtlasError>;

    async fn retry(&self, input: RetryTranslationInput) -> Result<TranslationSnapshot, AtlasError>;

    /// Reads committed progress without expressing new foreground intent or
    /// preempting background work.
    async fn view(&self, input: EnsureTranslationInput) -> Result<TranslationSnapshot, AtlasError>;

    async fn recover(&self) -> Result<usize, AtlasError>;

    async fn close_document(&self, document_id: &DocumentId) -> Result<(), AtlasError>;
}

type WorkKey = (DocumentId, ChapterId);

#[derive(Clone)]
struct InFlightWork {
    run_id: Uuid,
    kind: TranslationJobKind,
    configuration_fingerprint: String,
    cancellation: CancellationToken,
}

struct StartOutcome {
    snapshot: TranslationSnapshot,
    scheduled: bool,
    configured: bool,
}

#[derive(Clone)]
pub struct DefaultTranslationModule {
    parse_store: Arc<dyn ParseStore>,
    store: Arc<dyn TranslationStore>,
    configuration: Arc<dyn TranslationConfigurationPort>,
    provider: Arc<dyn TranslationProviderPort>,
    planner: Arc<TranslationPlanner>,
    in_flight: Arc<Mutex<HashMap<WorkKey, InFlightWork>>>,
    scheduling: Arc<Mutex<()>>,
    model_gate: Arc<Semaphore>,
}

impl std::fmt::Debug for DefaultTranslationModule {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DefaultTranslationModule")
            .finish_non_exhaustive()
    }
}

impl DefaultTranslationModule {
    #[must_use]
    pub fn new(
        parse_store: Arc<dyn ParseStore>,
        store: Arc<dyn TranslationStore>,
        configuration: Arc<dyn TranslationConfigurationPort>,
        provider: Arc<dyn TranslationProviderPort>,
    ) -> Self {
        Self {
            parse_store,
            store,
            configuration,
            provider,
            planner: Arc::new(TranslationPlanner::new()),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            scheduling: Arc::new(Mutex::new(())),
            model_gate: Arc::new(Semaphore::new(1)),
        }
    }

    async fn start(
        &self,
        input: EnsureTranslationInput,
        force: bool,
        kind: TranslationJobKind,
    ) -> Result<StartOutcome, AtlasError> {
        let document = self
            .parse_store
            .active_document(&input.document_id)
            .await?
            .ok_or_else(|| AtlasError::not_found("parsed document is not ready"))?;
        let chapter = find_chapter(&document, &input.focused_chapter_id)?.clone();
        let configuration = match self.configuration.load().await {
            Ok(configuration) => configuration,
            Err(error) => {
                let mut snapshot = self.snapshot_for(&document, &chapter, None, false).await?;
                if let Some(active_chapter) = snapshot.active_chapter.as_mut() {
                    active_chapter.safe_message = Some(error.message);
                }
                return Ok(StartOutcome {
                    snapshot,
                    scheduled: false,
                    configured: false,
                });
            }
        };
        let Some(configuration) = configuration else {
            return Ok(StartOutcome {
                snapshot: self.snapshot_for(&document, &chapter, None, false).await?,
                scheduled: false,
                configured: false,
            });
        };

        let _scheduling = self.scheduling.lock().await;
        let key = (input.document_id.clone(), chapter.id.clone());
        let configuration_fingerprint = runtime_configuration_fingerprint(&configuration);
        let replace_existing = {
            let in_flight = self.in_flight.lock().await;
            if let Some(existing) = in_flight.get(&key)
                && !force
                && !existing.cancellation.is_cancelled()
                && existing.configuration_fingerprint == configuration_fingerprint
                && !(kind == TranslationJobKind::Foreground
                    && existing.kind == TranslationJobKind::Prefetch)
            {
                drop(in_flight);
                return Ok(StartOutcome {
                    snapshot: self
                        .snapshot_for(&document, &chapter, Some(&configuration), false)
                        .await?,
                    scheduled: false,
                    configured: true,
                });
            }
            let replace_existing = in_flight.contains_key(&key);
            if let Some(existing) = in_flight.get(&key) {
                existing.cancellation.cancel();
            }
            if kind == TranslationJobKind::Foreground {
                for ((document_id, _), existing) in in_flight.iter() {
                    if document_id == &input.document_id
                        || existing.kind == TranslationJobKind::Prefetch
                    {
                        existing.cancellation.cancel();
                    }
                }
            }
            replace_existing
        };

        let prepared = match self
            .prepare_chapter(
                &document,
                &chapter,
                &input.session_id,
                kind,
                &configuration,
                force || replace_existing,
            )
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                return Err(error);
            }
        };

        let ready_for_prefetch =
            if kind == TranslationJobKind::Foreground && prepared.is_none() && !force {
                self.snapshot_for(&document, &chapter, Some(&configuration), false)
                    .await?
                    .active_chapter
                    .is_some_and(|chapter| chapter.state == TranslationState::Complete)
            } else {
                false
            };
        let next = if ready_for_prefetch {
            self.prepare_prefetch(
                &document,
                &chapter,
                &input.session_id,
                &configuration,
                replace_existing,
            )
            .await?
        } else {
            None
        };
        let scheduled = prepared.is_some() || next.is_some();
        if scheduled {
            let cancellation = CancellationToken::new();
            let run_id = Uuid::new_v4();
            let active_kind = if prepared.is_none() && next.is_some() {
                TranslationJobKind::Prefetch
            } else {
                kind
            };
            self.in_flight.lock().await.insert(
                key.clone(),
                InFlightWork {
                    run_id,
                    kind: active_kind,
                    configuration_fingerprint,
                    cancellation: cancellation.clone(),
                },
            );
            let module = self.clone();
            let session_id = input.session_id.clone();
            let focused_chapter_id = chapter.id.clone();
            let task_document = document.clone();
            let task_configuration = configuration.clone();
            tokio::spawn(async move {
                if let Some(work) = prepared {
                    let succeeded = module
                        .execute_work(work, &task_configuration, cancellation.clone())
                        .await;
                    let prefetch = if succeeded && kind == TranslationJobKind::Foreground {
                        let _scheduling = module.scheduling.lock().await;
                        if cancellation.is_cancelled() {
                            None
                        } else if let Ok(chapter) =
                            find_chapter(&task_document, &focused_chapter_id)
                        {
                            let prefetch = module
                                .prepare_prefetch(
                                    &task_document,
                                    chapter,
                                    &session_id,
                                    &task_configuration,
                                    false,
                                )
                                .await
                                .ok()
                                .flatten();
                            if prefetch.is_some()
                                && let Some(active) = module.in_flight.lock().await.get_mut(&key)
                                && active.run_id == run_id
                            {
                                active.kind = TranslationJobKind::Prefetch;
                            }
                            prefetch
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(prefetch) = prefetch {
                        module
                            .execute_work(prefetch, &task_configuration, cancellation.clone())
                            .await;
                    }
                } else if let Some(work) = next {
                    module
                        .execute_work(work, &task_configuration, cancellation.clone())
                        .await;
                }
                let mut in_flight = module.in_flight.lock().await;
                if in_flight
                    .get(&key)
                    .is_some_and(|active| active.run_id == run_id)
                {
                    in_flight.remove(&key);
                }
            });
        }

        Ok(StartOutcome {
            snapshot: self
                .snapshot_for(&document, &chapter, Some(&configuration), false)
                .await?,
            scheduled,
            configured: true,
        })
    }

    async fn prepare_prefetch(
        &self,
        document: &CanonicalDocument,
        chapter: &CanonicalChapter,
        session_id: &SessionId,
        configuration: &TranslationConfiguration,
        force: bool,
    ) -> Result<Option<PreparedWork>, AtlasError> {
        let Some(next) = next_body_chapter(document, chapter) else {
            return Ok(None);
        };
        self.prepare_chapter(
            document,
            next,
            session_id,
            TranslationJobKind::Prefetch,
            configuration,
            force,
        )
        .await
    }

    async fn prepare_chapter(
        &self,
        document: &CanonicalDocument,
        chapter: &CanonicalChapter,
        session_id: &SessionId,
        kind: TranslationJobKind,
        configuration: &TranslationConfiguration,
        force: bool,
    ) -> Result<Option<PreparedWork>, AtlasError> {
        let blocks = chapter
            .blocks
            .iter()
            .filter(|block| block.is_translatable())
            .map(|block| self.planner.prepare(block, configuration))
            .collect::<Result<Vec<_>, _>>()?;
        if blocks.is_empty() {
            return Ok(None);
        }
        let plan_digest = plan_digest(&blocks);
        let latest = self
            .store
            .latest_job(&chapter.id, Some(&plan_digest))
            .await?;
        let succeeded = latest
            .as_ref()
            .filter(|job| job.state == TranslationJobState::Succeeded)
            .cloned();
        if !force && let Some(latest) = latest.as_ref() {
            let foreground_supersedes_prefetch = kind == TranslationJobKind::Foreground
                && latest.kind == TranslationJobKind::Prefetch;
            if !matches!(
                latest.state,
                TranslationJobState::Interrupted
                    | TranslationJobState::Queued
                    | TranslationJobState::Running
            ) && (!latest.state.is_terminal() || latest.state == TranslationJobState::Failed)
                && !foreground_supersedes_prefetch
            {
                return Ok(None);
            }
            if latest.state == TranslationJobState::Succeeded
                && self.all_active_cached(chapter, &blocks).await?
            {
                return Ok(None);
            }
        }
        let now = now_ms()?;
        let recovered = (!force)
            .then(|| {
                latest
                    .clone()
                    .filter(|job| {
                        matches!(
                            job.state,
                            TranslationJobState::Interrupted
                                | TranslationJobState::Queued
                                | TranslationJobState::Running
                        ) && !(kind == TranslationJobKind::Foreground
                            && job.kind == TranslationJobKind::Prefetch
                            && matches!(
                                job.state,
                                TranslationJobState::Queued | TranslationJobState::Running
                            ))
                    })
                    .map(|mut job| {
                        job.session_id = session_id.clone();
                        job.kind = kind;
                        job.state = TranslationJobState::Queued;
                        job.endpoint_fingerprint = configuration.endpoint_fingerprint.clone();
                        job.model_id = configuration.model_id.clone();
                        job.block_ids = blocks.iter().map(|block| block.block_id.clone()).collect();
                        job.error_code = None;
                        job.safe_message = None;
                        job.updated_at = now;
                        job.completed_at = None;
                        job
                    })
            })
            .flatten();
        if self.all_cached(&blocks).await? {
            let mut job = recovered.or(succeeded).unwrap_or_else(|| {
                new_job(
                    document,
                    chapter,
                    session_id,
                    kind,
                    configuration,
                    &blocks,
                    plan_digest.clone(),
                    now,
                )
            });
            job.session_id = session_id.clone();
            job.kind = kind;
            job.state = TranslationJobState::Succeeded;
            job.completed_block_ids = job.block_ids.clone();
            job.error_code = None;
            job.safe_message = None;
            job.updated_at = now;
            job.completed_at = Some(now);
            let records = new_records(&blocks, configuration, now);
            self.store.prepare_job(&job, &records).await?;
            return Ok(None);
        }

        let mut job = recovered.unwrap_or_else(|| {
            new_job(
                document,
                chapter,
                session_id,
                kind,
                configuration,
                &blocks,
                plan_digest,
                now,
            )
        });
        let records = new_records(&blocks, configuration, now);
        let missing = self.store.prepare_job(&job, &records).await?;
        let missing_set = missing.into_iter().collect::<HashSet<_>>();
        let missing_blocks = blocks
            .into_iter()
            .filter(|block| missing_set.contains(&block.block_id))
            .collect::<Vec<_>>();
        if missing_blocks.is_empty() {
            job.state = TranslationJobState::Succeeded;
            job.completed_block_ids = job.block_ids.clone();
            job.error_code = None;
            job.safe_message = None;
            job.updated_at = now;
            job.completed_at = Some(now);
            self.store.save_job(&job).await?;
            return Ok(None);
        }
        Ok(Some(PreparedWork {
            job,
            blocks: missing_blocks,
        }))
    }

    async fn all_cached(&self, blocks: &[PreparedBlock]) -> Result<bool, AtlasError> {
        for block in blocks {
            let cached = self
                .store
                .translation(&block.block_id, &block.request_digest)
                .await?;
            if !cached.is_some_and(|value| value.state == TranslationRecordState::Ready) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn all_active_cached(
        &self,
        chapter: &CanonicalChapter,
        blocks: &[PreparedBlock],
    ) -> Result<bool, AtlasError> {
        let active = self
            .store
            .active_for_chapter(&chapter.id)
            .await?
            .into_iter()
            .map(|translation| (translation.block_id.clone(), translation))
            .collect::<HashMap<_, _>>();
        Ok(blocks.iter().all(|block| {
            active.get(&block.block_id).is_some_and(|translation| {
                translation.request_digest == block.request_digest
                    && translation.state == TranslationRecordState::Ready
            })
        }))
    }

    async fn execute_work(
        &self,
        mut work: PreparedWork,
        configuration: &TranslationConfiguration,
        cancellation: CancellationToken,
    ) -> bool {
        let _model_permit = tokio::select! {
            () = cancellation.cancelled() => {
                self.cancel_work(&mut work.job).await;
                return false;
            }
            permit = self.model_gate.acquire() => match permit {
                Ok(permit) => permit,
                Err(_) => {
                    self.fail_all(
                        &mut work.job,
                        &work.blocks,
                        "translation_unavailable",
                        "The translation scheduler is unavailable",
                    )
                    .await;
                    return false;
                }
            }
        };
        work.job.state = TranslationJobState::Running;
        work.job.attempt_count = work.job.attempt_count.saturating_add(1);
        if let Ok(now) = now_ms() {
            work.job.updated_at = now;
        }
        if self.store.save_job(&work.job).await.is_err() {
            return false;
        }
        let plan = match self
            .planner
            .plan_batches(work.blocks.clone(), configuration)
        {
            Ok(plan) => plan,
            Err(error) => {
                self.fail_all(
                    &mut work.job,
                    &work.blocks,
                    "request_too_large",
                    &error.message,
                )
                .await;
                return false;
            }
        };

        let mut failures = plan
            .rejected
            .iter()
            .map(|block| ValidationFailure {
                block_id: block.block_id.clone(),
                code: "request_too_large".to_owned(),
                safe_message: "The source block exceeds the model request budget".to_owned(),
            })
            .collect::<Vec<_>>();
        let mut queue = plan
            .batches
            .into_iter()
            .map(|batch| (batch, false))
            .collect::<VecDeque<_>>();
        while let Some((batch, split_once)) = queue.pop_front() {
            if cancellation.is_cancelled() {
                self.cancel_work(&mut work.job).await;
                return false;
            }
            match self
                .run_batch(configuration, batch.clone(), cancellation.clone())
                .await
            {
                Ok(validation) => {
                    if !validation.accepted.is_empty() {
                        let committed = validation
                            .accepted
                            .into_iter()
                            .map(|value| CommittedTranslation {
                                block_id: value.block_id,
                                target: value.target,
                                target_plain_text: value.target_plain_text,
                                validation_json: value.validation_json,
                            })
                            .collect::<Vec<_>>();
                        for translation in &committed {
                            if !work.job.completed_block_ids.contains(&translation.block_id) {
                                work.job
                                    .completed_block_ids
                                    .push(translation.block_id.clone());
                            }
                        }
                        if let Ok(now) = now_ms() {
                            work.job.updated_at = now;
                        }
                        if self.store.commit(&work.job, &committed).await.is_err() {
                            return false;
                        }
                    }
                    if !validation.failed.is_empty() {
                        let repair_blocks = blocks_for_failures(&batch.blocks, &validation.failed);
                        match self
                            .repair(configuration, repair_blocks.clone(), cancellation.clone())
                            .await
                        {
                            Ok(repaired) => {
                                if !repaired.accepted.is_empty() {
                                    let committed = repaired
                                        .accepted
                                        .into_iter()
                                        .map(|value| CommittedTranslation {
                                            block_id: value.block_id,
                                            target: value.target,
                                            target_plain_text: value.target_plain_text,
                                            validation_json: value.validation_json,
                                        })
                                        .collect::<Vec<_>>();
                                    for translation in &committed {
                                        if !work
                                            .job
                                            .completed_block_ids
                                            .contains(&translation.block_id)
                                        {
                                            work.job
                                                .completed_block_ids
                                                .push(translation.block_id.clone());
                                        }
                                    }
                                    if let Ok(now) = now_ms() {
                                        work.job.updated_at = now;
                                    }
                                    if self.store.commit(&work.job, &committed).await.is_err() {
                                        return false;
                                    }
                                }
                                failures.extend(repaired.failed);
                            }
                            Err(error) if error.kind == TranslationProviderErrorKind::Cancelled => {
                                self.cancel_work(&mut work.job).await;
                                return false;
                            }
                            Err(error) => failures.extend(repair_blocks.iter().map(|block| {
                                ValidationFailure {
                                    block_id: block.block_id.clone(),
                                    code: provider_error_code(error.kind).to_owned(),
                                    safe_message: error.safe_message.clone(),
                                }
                            })),
                        }
                    }
                }
                Err(error)
                    if error.kind == TranslationProviderErrorKind::ContextLength
                        && !split_once
                        && batch.blocks.len() > 1 =>
                {
                    let middle = batch.blocks.len() / 2;
                    let left = batch.blocks[..middle].to_vec();
                    let right = batch.blocks[middle..].to_vec();
                    for half in [right, left] {
                        match self.planner.plan_batches(half.clone(), configuration) {
                            Ok(split) => {
                                failures.extend(split.rejected.iter().map(|block| {
                                    ValidationFailure {
                                        block_id: block.block_id.clone(),
                                        code: "request_too_large".to_owned(),
                                        safe_message:
                                            "The source block exceeds the model request budget"
                                                .to_owned(),
                                    }
                                }));
                                for child in split.batches.into_iter().rev() {
                                    queue.push_front((child, true));
                                }
                            }
                            Err(error) => {
                                failures.extend(half.iter().map(|block| ValidationFailure {
                                    block_id: block.block_id.clone(),
                                    code: "request_too_large".to_owned(),
                                    safe_message: error.message.clone(),
                                }))
                            }
                        }
                    }
                }
                Err(error) if error.kind == TranslationProviderErrorKind::Cancelled => {
                    self.cancel_work(&mut work.job).await;
                    return false;
                }
                Err(error) => failures.extend(batch.blocks.iter().map(|block| ValidationFailure {
                    block_id: block.block_id.clone(),
                    code: provider_error_code(error.kind).to_owned(),
                    safe_message: error.safe_message.clone(),
                })),
            }
        }

        if failures.is_empty() {
            work.job.state = TranslationJobState::Succeeded;
            work.job.error_code = None;
            work.job.safe_message = None;
            if let Ok(now) = now_ms() {
                work.job.updated_at = now;
                work.job.completed_at = Some(now);
            }
            self.store.save_job(&work.job).await.is_ok()
        } else {
            work.job.state = TranslationJobState::Failed;
            work.job.error_code = Some("translation_invalid".to_owned());
            work.job.safe_message = Some("Some blocks could not be translated safely".to_owned());
            if let Ok(now) = now_ms() {
                work.job.updated_at = now;
                work.job.completed_at = Some(now);
            }
            let stored_failures = failures
                .into_iter()
                .map(|failure| (failure.block_id, failure.code, failure.safe_message))
                .collect::<Vec<_>>();
            let _ = self.store.fail(&work.job, &stored_failures).await;
            false
        }
    }

    async fn run_batch(
        &self,
        configuration: &TranslationConfiguration,
        batch: TranslationBatch,
        cancellation: CancellationToken,
    ) -> Result<OutputValidation, TranslationProviderError> {
        let collector = Arc::new(OutputCollector::default());
        let mut attempt = 0;
        loop {
            attempt += 1;
            collector.clear().await;
            let result = self
                .provider
                .stream(
                    configuration,
                    batch.request.clone(),
                    collector.clone(),
                    cancellation.clone(),
                )
                .await;
            match result {
                Ok(completion) => {
                    return Ok(match collector.finish().await {
                        Ok(records) => validate_output(&batch.blocks, records, &completion),
                        Err(error) => OutputValidation {
                            accepted: Vec::new(),
                            failed: batch
                                .blocks
                                .iter()
                                .map(|block| ValidationFailure {
                                    block_id: block.block_id.clone(),
                                    code: "invalid_json".to_owned(),
                                    safe_message: error.message.clone(),
                                })
                                .collect(),
                        },
                    });
                }
                Err(error) => {
                    let partial = collector.finish().await.ok().map(|records| {
                        validate_output(
                            &batch.blocks,
                            records,
                            &crate::TranslationCompletion {
                                finish_reason: None,
                            },
                        )
                    });
                    if let Some(partial) = partial
                        && !partial.accepted.is_empty()
                    {
                        return Ok(partial);
                    }
                    if attempt < PROVIDER_ATTEMPTS
                        && matches!(
                            error.kind,
                            TranslationProviderErrorKind::Transport
                                | TranslationProviderErrorKind::Timeout
                                | TranslationProviderErrorKind::RateLimited
                        )
                    {
                        let delay = if error.kind == TranslationProviderErrorKind::RateLimited {
                            Duration::from_secs(error.retry_after_seconds.unwrap_or(1).min(60))
                        } else if attempt == 1 {
                            Duration::from_secs(1)
                        } else {
                            Duration::from_secs(4)
                        };
                        tokio::select! {
                            () = cancellation.cancelled() => {
                                return Err(TranslationProviderError::new(
                                    TranslationProviderErrorKind::Cancelled,
                                    "Translation was cancelled",
                                ));
                            }
                            () = sleep(delay) => {}
                        }
                        continue;
                    }
                    return Err(error);
                }
            }
        }
    }

    async fn repair(
        &self,
        configuration: &TranslationConfiguration,
        blocks: Vec<PreparedBlock>,
        cancellation: CancellationToken,
    ) -> Result<OutputValidation, TranslationProviderError> {
        if blocks.is_empty() {
            return Ok(OutputValidation::default());
        }
        let plan = self
            .planner
            .plan_batches(blocks, configuration)
            .map_err(|error| {
                TranslationProviderError::new(TranslationProviderErrorKind::Protocol, error.message)
            })?;
        let mut combined = OutputValidation::default();
        combined
            .failed
            .extend(plan.rejected.iter().map(|block| ValidationFailure {
                block_id: block.block_id.clone(),
                code: "request_too_large".to_owned(),
                safe_message: "The source block exceeds the model request budget".to_owned(),
            }));
        for batch in plan.batches {
            let validation = self
                .run_batch(configuration, batch, cancellation.clone())
                .await?;
            combined.accepted.extend(validation.accepted);
            combined.failed.extend(validation.failed);
        }
        Ok(combined)
    }

    async fn fail_all(
        &self,
        job: &mut TranslationJob,
        blocks: &[PreparedBlock],
        code: &str,
        message: &str,
    ) {
        job.state = TranslationJobState::Failed;
        job.error_code = Some(code.to_owned());
        job.safe_message = Some(message.to_owned());
        if let Ok(now) = now_ms() {
            job.updated_at = now;
            job.completed_at = Some(now);
        }
        let failures = blocks
            .iter()
            .map(|block| (block.block_id.clone(), code.to_owned(), message.to_owned()))
            .collect::<Vec<_>>();
        let _ = self.store.fail(job, &failures).await;
    }

    async fn cancel_work(&self, job: &mut TranslationJob) {
        job.state = TranslationJobState::Cancelled;
        job.error_code = Some("cancelled".to_owned());
        job.safe_message = Some("Translation was cancelled".to_owned());
        if let Ok(now) = now_ms() {
            job.updated_at = now;
            job.completed_at = Some(now);
        }
        let _ = self.store.save_job(job).await;
    }

    async fn snapshot_for(
        &self,
        document: &CanonicalDocument,
        chapter: &CanonicalChapter,
        configuration: Option<&TranslationConfiguration>,
        prefetched: bool,
    ) -> Result<TranslationSnapshot, AtlasError> {
        let stored = self.store.active_for_chapter(&chapter.id).await?;
        let stored_by_block = stored
            .into_iter()
            .map(|translation| (translation.block_id.clone(), translation))
            .collect::<HashMap<_, _>>();
        let plan_digest = if let Some(configuration) = configuration {
            let prepared = chapter
                .blocks
                .iter()
                .filter(|block| block.is_translatable())
                .map(|block| self.planner.prepare(block, configuration))
                .collect::<Result<Vec<_>, _>>()?;
            Some(plan_digest(&prepared))
        } else {
            None
        };
        let latest = self
            .store
            .latest_job(&chapter.id, plan_digest.as_deref())
            .await?;
        let expected_digests = if let Some(configuration) = configuration {
            chapter
                .blocks
                .iter()
                .filter(|block| block.is_translatable())
                .map(|block| {
                    self.planner
                        .prepare(block, configuration)
                        .map(|prepared| (block.id.clone(), prepared.request_digest))
                })
                .collect::<Result<HashMap<_, _>, _>>()?
        } else {
            HashMap::new()
        };
        let translatable_count = chapter
            .blocks
            .iter()
            .filter(|block| block.is_translatable())
            .count();
        let mut ready = 0;
        let blocks = chapter
            .blocks
            .iter()
            .map(|block| {
                if !block.is_translatable() {
                    return TranslatedBlockView {
                        block_id: block.id.clone(),
                        source_digest: block.source_digest.clone(),
                        state: BlockTranslationState::Skipped,
                        target: None,
                        safe_message: None,
                    };
                }
                let translation = stored_by_block.get(&block.id);
                let matches_runtime = configuration.is_none_or(|_| {
                    translation.is_some_and(|value| {
                        expected_digests
                            .get(&block.id)
                            .is_some_and(|digest| digest == &value.request_digest)
                    })
                });
                let state = match translation.filter(|_| matches_runtime) {
                    Some(value) if value.state == TranslationRecordState::Ready => {
                        ready += 1;
                        BlockTranslationState::Ready
                    }
                    Some(value) if value.state == TranslationRecordState::Failed => {
                        BlockTranslationState::Failed
                    }
                    _ => BlockTranslationState::Pending,
                };
                TranslatedBlockView {
                    block_id: block.id.clone(),
                    source_digest: block.source_digest.clone(),
                    state,
                    target: translation
                        .filter(|value| {
                            matches_runtime && value.state == TranslationRecordState::Ready
                        })
                        .and_then(|value| value.target.clone()),
                    safe_message: translation
                        .filter(|_| matches_runtime)
                        .and_then(|value| value.safe_message.clone()),
                }
            })
            .collect::<Vec<_>>();
        let progress = if translatable_count == 0 {
            1.0
        } else {
            ready as f64 / translatable_count as f64
        };
        let state = if ready == translatable_count {
            TranslationState::Complete
        } else if ready > 0 {
            TranslationState::Readable
        } else if configuration.is_none() {
            TranslationState::NotConfigured
        } else {
            match latest.as_ref().map(|job| job.state) {
                Some(TranslationJobState::Queued) => TranslationState::Queued,
                Some(TranslationJobState::Running | TranslationJobState::Interrupted) => {
                    TranslationState::Translating
                }
                Some(TranslationJobState::Failed | TranslationJobState::Cancelled) => {
                    TranslationState::Failed
                }
                _ => TranslationState::NotStarted,
            }
        };
        let safe_message = latest.as_ref().and_then(|job| job.safe_message.clone());
        let job_id = latest.as_ref().map(|job| job.id.clone());
        let job_active = latest.as_ref().is_some_and(|job| !job.state.is_terminal());
        let prefetched = prefetched
            || latest
                .as_ref()
                .is_some_and(|job| job.kind == TranslationJobKind::Prefetch);
        Ok(TranslationSnapshot {
            target_locale: TARGET_LOCALE.to_owned(),
            model_id: configuration.map(|value| value.model_id.clone()),
            active_chapter: Some(ChapterTranslationView {
                chapter_id: chapter.id.clone(),
                state,
                progress,
                job_id,
                job_active,
                blocks,
                prefetched,
                safe_message,
            }),
            prefetched_chapter_id: self
                .store
                .latest_prefetched_chapter(&document.document_id)
                .await?,
        })
    }
}

#[async_trait]
impl TranslationModule for DefaultTranslationModule {
    async fn ensure(
        &self,
        input: EnsureTranslationInput,
    ) -> Result<TranslationSnapshot, AtlasError> {
        self.start(input, false, TranslationJobKind::Foreground)
            .await
            .map(|outcome| outcome.snapshot)
    }

    async fn retry(&self, input: RetryTranslationInput) -> Result<TranslationSnapshot, AtlasError> {
        self.start(
            EnsureTranslationInput {
                session_id: input.session_id,
                document_id: input.document_id,
                focused_chapter_id: input.chapter_id,
            },
            true,
            TranslationJobKind::Foreground,
        )
        .await
        .map(|outcome| outcome.snapshot)
    }

    async fn view(&self, input: EnsureTranslationInput) -> Result<TranslationSnapshot, AtlasError> {
        let document = self
            .parse_store
            .active_document(&input.document_id)
            .await?
            .ok_or_else(|| AtlasError::not_found("parsed document is not ready"))?;
        let chapter = find_chapter(&document, &input.focused_chapter_id)?;
        match self.configuration.load().await {
            Ok(configuration) => {
                self.snapshot_for(&document, chapter, configuration.as_ref(), false)
                    .await
            }
            Err(error) => {
                let mut snapshot = self.snapshot_for(&document, chapter, None, false).await?;
                if let Some(active_chapter) = snapshot.active_chapter.as_mut() {
                    active_chapter.safe_message = Some(error.message);
                }
                Ok(snapshot)
            }
        }
    }

    async fn recover(&self) -> Result<usize, AtlasError> {
        let targets = self.store.recoverable().await?;
        let mut started = 0;
        for target in targets {
            if target.kind == TranslationJobKind::Prefetch {
                continue;
            }
            if let Ok(outcome) = self
                .start(
                    EnsureTranslationInput {
                        session_id: target.session_id,
                        document_id: target.document_id,
                        focused_chapter_id: target.chapter_id,
                    },
                    false,
                    target.kind,
                )
                .await
            {
                if outcome.configured {
                    self.store
                        .supersede_interrupted(&target.job_id, now_ms()?)
                        .await?;
                }
                if outcome.scheduled {
                    started += 1;
                }
            }
        }
        Ok(started)
    }

    async fn close_document(&self, document_id: &DocumentId) -> Result<(), AtlasError> {
        let _scheduling = self.scheduling.lock().await;
        self.store.cancel_document(document_id, now_ms()?).await?;
        for ((active_document_id, _), work) in self.in_flight.lock().await.iter() {
            if active_document_id == document_id {
                work.cancellation.cancel();
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct PreparedWork {
    job: TranslationJob,
    blocks: Vec<PreparedBlock>,
}

#[derive(Default)]
struct OutputCollector {
    parser: Mutex<TranslationOutputParser>,
}

impl OutputCollector {
    async fn clear(&self) {
        *self.parser.lock().await = TranslationOutputParser::new();
    }

    async fn finish(&self) -> Result<Vec<crate::OutputRecord>, AtlasError> {
        let parser = std::mem::take(&mut *self.parser.lock().await);
        parser.finish()
    }
}

#[async_trait]
impl TranslationChunkSink for OutputCollector {
    async fn push(&self, content: &str) -> Result<(), AtlasError> {
        self.parser.lock().await.push(content)
    }
}

fn find_chapter<'a>(
    document: &'a CanonicalDocument,
    chapter_id: &ChapterId,
) -> Result<&'a CanonicalChapter, AtlasError> {
    document
        .chapters
        .iter()
        .find(|chapter| &chapter.id == chapter_id)
        .ok_or_else(|| AtlasError::not_found("chapter was not found"))
}

fn next_body_chapter<'a>(
    document: &'a CanonicalDocument,
    current: &CanonicalChapter,
) -> Option<&'a CanonicalChapter> {
    document
        .chapters
        .iter()
        .filter(|chapter| chapter.order_index > current.order_index)
        .find(|chapter| {
            chapter.role == ChapterRole::Body
                && chapter.blocks.iter().any(CanonicalBlock::is_translatable)
        })
}

fn plan_digest(blocks: &[PreparedBlock]) -> String {
    let mut hasher = Sha256::new();
    for block in blocks {
        hasher.update(block.request_digest.len().to_be_bytes());
        hasher.update(block.request_digest.as_bytes());
    }
    hex::encode(hasher.finalize())
}

#[allow(clippy::too_many_arguments)]
fn new_job(
    document: &CanonicalDocument,
    chapter: &CanonicalChapter,
    session_id: &SessionId,
    kind: TranslationJobKind,
    configuration: &TranslationConfiguration,
    blocks: &[PreparedBlock],
    plan_digest: String,
    now: u64,
) -> TranslationJob {
    TranslationJob {
        id: JobId::new(Uuid::new_v4().to_string()),
        session_id: session_id.clone(),
        document_id: document.document_id.clone(),
        chapter_id: chapter.id.clone(),
        kind,
        state: TranslationJobState::Queued,
        plan_digest,
        endpoint_fingerprint: configuration.endpoint_fingerprint.clone(),
        model_id: configuration.model_id.clone(),
        block_ids: blocks.iter().map(|block| block.block_id.clone()).collect(),
        completed_block_ids: Vec::new(),
        attempt_count: 0,
        error_code: None,
        safe_message: None,
        created_at: now,
        updated_at: now,
        completed_at: None,
    }
}

fn new_records(
    blocks: &[PreparedBlock],
    configuration: &TranslationConfiguration,
    now: u64,
) -> Vec<NewTranslationRecord> {
    blocks
        .iter()
        .map(|block| NewTranslationRecord {
            id: Uuid::new_v4().to_string(),
            block_id: block.block_id.clone(),
            request_digest: block.request_digest.clone(),
            source_digest: block.source_digest.clone(),
            target_locale: TARGET_LOCALE.to_owned(),
            endpoint_origin: configuration.endpoint_base_url.clone(),
            provider_profile_fingerprint: configuration.endpoint_fingerprint.clone(),
            model_id: configuration.model_id.clone(),
            prompt_version: PROMPT_VERSION.to_owned(),
            applicable_preference_digest: String::new(),
            created_at: now,
        })
        .collect()
}

fn blocks_for_failures(
    blocks: &[PreparedBlock],
    failures: &[ValidationFailure],
) -> Vec<PreparedBlock> {
    let failed = failures
        .iter()
        .map(|failure| &failure.block_id)
        .collect::<HashSet<_>>();
    blocks
        .iter()
        .filter(|block| failed.contains(&block.block_id))
        .cloned()
        .collect()
}

fn provider_error_code(kind: TranslationProviderErrorKind) -> &'static str {
    match kind {
        TranslationProviderErrorKind::Unauthorized => "unauthorized",
        TranslationProviderErrorKind::RateLimited => "rate_limited",
        TranslationProviderErrorKind::Timeout => "timeout",
        TranslationProviderErrorKind::Transport => "unreachable",
        TranslationProviderErrorKind::Protocol => "protocol_incompatible",
        TranslationProviderErrorKind::ContextLength => "context_length",
        TranslationProviderErrorKind::Remote => "remote_failed",
        TranslationProviderErrorKind::Cancelled => "cancelled",
    }
}

fn runtime_configuration_fingerprint(configuration: &TranslationConfiguration) -> String {
    format!(
        "{}\0{}",
        configuration.endpoint_fingerprint, configuration.model_id
    )
}

fn now_ms() -> Result<u64, AtlasError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AtlasError::internal("system clock predates the Unix epoch"))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| AtlasError::internal("system time does not fit in storage"))
}
