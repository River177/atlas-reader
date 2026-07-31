CREATE TABLE translations (
  id TEXT PRIMARY KEY,
  job_id TEXT REFERENCES jobs(id) ON DELETE SET NULL,
  block_id TEXT NOT NULL REFERENCES blocks(id) ON DELETE CASCADE,
  request_digest TEXT NOT NULL,
  source_digest TEXT NOT NULL,
  target_locale TEXT NOT NULL,
  endpoint_origin TEXT NOT NULL,
  provider_profile_fingerprint TEXT NOT NULL,
  model_id TEXT NOT NULL,
  prompt_version TEXT NOT NULL,
  translation_mode TEXT NOT NULL,
  applicable_preference_digest TEXT NOT NULL,
  target_json TEXT,
  target_plain_text TEXT,
  state TEXT NOT NULL CHECK (
    state IN ('queued', 'translating', 'ready', 'stale', 'failed', 'cancelled')
  ),
  validation_json TEXT,
  error_code TEXT,
  error_safe_json TEXT,
  is_active INTEGER NOT NULL CHECK (is_active IN (0, 1)),
  user_confirmed INTEGER NOT NULL DEFAULT 0 CHECK (user_confirmed IN (0, 1)),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE (block_id, request_digest)
);

CREATE UNIQUE INDEX translations_one_active_idx
  ON translations(block_id)
  WHERE is_active = 1;

CREATE INDEX translations_job_idx
  ON translations(job_id, state);

CREATE INDEX translations_cache_idx
  ON translations(block_id, request_digest, state);
