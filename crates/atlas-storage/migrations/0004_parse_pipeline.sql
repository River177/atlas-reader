ALTER TABLE jobs ADD COLUMN chapter_id TEXT REFERENCES chapters(id) ON DELETE CASCADE;
ALTER TABLE jobs ADD COLUMN idempotency_key TEXT;
ALTER TABLE jobs ADD COLUMN remote_job_id TEXT;
ALTER TABLE jobs ADD COLUMN result_json TEXT;
ALTER TABLE jobs ADD COLUMN error_safe_json TEXT;
ALTER TABLE jobs ADD COLUMN started_at INTEGER;
ALTER TABLE jobs ADD COLUMN cancellation_requested_at INTEGER;

CREATE TABLE parse_operations (
  id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL UNIQUE REFERENCES jobs(id) ON DELETE CASCADE,
  document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
  provider_profile_id TEXT REFERENCES provider_profiles(id) ON DELETE RESTRICT,
  backend TEXT NOT NULL CHECK (backend IN ('cloud_mineru', 'local_text')),
  parser_version TEXT NOT NULL,
  normalizer_version TEXT NOT NULL,
  endpoint_origin TEXT,
  endpoint_fingerprint TEXT,
  state TEXT NOT NULL CHECK (
    state IN (
      'queued',
      'uploading',
      'processing',
      'downloading',
      'normalizing',
      'succeeded',
      'failed',
      'cancelled',
      'status_unknown'
    )
  ),
  progress REAL CHECK (progress IS NULL OR (progress >= 0.0 AND progress <= 1.0)),
  data_id TEXT NOT NULL,
  batch_id TEXT,
  remote_upload_url TEXT,
  remote_download_url TEXT,
  remote_status_json TEXT,
  retry_count INTEGER NOT NULL DEFAULT 0,
  error_code TEXT,
  error_safe_json TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  completed_at INTEGER
);

CREATE INDEX parse_operations_document_idx
  ON parse_operations(document_id, created_at DESC);

CREATE INDEX parse_operations_recovery_idx
  ON parse_operations(state, updated_at);

CREATE UNIQUE INDEX parse_operations_batch_idx
  ON parse_operations(batch_id)
  WHERE batch_id IS NOT NULL;

CREATE TABLE parse_artifacts (
  id TEXT PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
  parse_operation_id TEXT NOT NULL UNIQUE
    REFERENCES parse_operations(id) ON DELETE CASCADE,
  parser_name TEXT NOT NULL,
  parser_version TEXT NOT NULL,
  normalizer_version TEXT NOT NULL,
  canonical_schema_version INTEGER NOT NULL,
  source_sha256 TEXT NOT NULL,
  content_digest TEXT NOT NULL,
  manifest_relative_path TEXT NOT NULL,
  is_active INTEGER NOT NULL CHECK (is_active IN (0, 1)),
  created_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX parse_artifacts_one_active_idx
  ON parse_artifacts(document_id)
  WHERE is_active = 1;

CREATE TABLE chapters (
  id TEXT PRIMARY KEY,
  artifact_id TEXT NOT NULL REFERENCES parse_artifacts(id) ON DELETE CASCADE,
  document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
  order_index INTEGER NOT NULL,
  depth INTEGER NOT NULL CHECK (depth >= 1),
  role TEXT NOT NULL CHECK (role IN ('front_matter', 'body', 'references')),
  source_title TEXT NOT NULL,
  page_start INTEGER NOT NULL CHECK (page_start >= 1),
  page_end INTEGER NOT NULL CHECK (page_end >= page_start),
  source_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE (artifact_id, order_index)
);

CREATE INDEX chapters_document_idx
  ON chapters(document_id, order_index);

CREATE TABLE blocks (
  row_id INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  chapter_id TEXT NOT NULL REFERENCES chapters(id) ON DELETE CASCADE,
  order_index INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK (
    kind IN (
      'heading',
      'paragraph',
      'list',
      'equation',
      'table',
      'figure',
      'caption'
    )
  ),
  page_start INTEGER NOT NULL CHECK (page_start >= 1),
  page_end INTEGER NOT NULL CHECK (page_end >= page_start),
  bounding_boxes_json TEXT NOT NULL DEFAULT '[]',
  source_json TEXT NOT NULL,
  source_plain_text TEXT NOT NULL,
  source_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE (chapter_id, order_index)
);

CREATE VIRTUAL TABLE blocks_fts USING fts5(
  source_plain_text,
  content='blocks',
  content_rowid='row_id',
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER blocks_ai AFTER INSERT ON blocks BEGIN
  INSERT INTO blocks_fts(rowid, source_plain_text)
  VALUES (new.row_id, new.source_plain_text);
END;

CREATE TRIGGER blocks_ad AFTER DELETE ON blocks BEGIN
  INSERT INTO blocks_fts(blocks_fts, rowid, source_plain_text)
  VALUES ('delete', old.row_id, old.source_plain_text);
END;

CREATE TRIGGER blocks_au AFTER UPDATE OF source_plain_text ON blocks BEGIN
  INSERT INTO blocks_fts(blocks_fts, rowid, source_plain_text)
  VALUES ('delete', old.row_id, old.source_plain_text);
  INSERT INTO blocks_fts(rowid, source_plain_text)
  VALUES (new.row_id, new.source_plain_text);
END;

CREATE TABLE job_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
  sequence INTEGER NOT NULL,
  event_type TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE (job_id, sequence)
);
