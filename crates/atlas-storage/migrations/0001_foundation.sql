CREATE TABLE app_metadata (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE documents (
  id TEXT PRIMARY KEY,
  sha256 TEXT NOT NULL UNIQUE,
  title TEXT NOT NULL,
  authors_json TEXT NOT NULL DEFAULT '[]',
  page_count INTEGER,
  file_path TEXT NOT NULL,
  file_size_bytes INTEGER NOT NULL,
  file_mtime_ms INTEGER NOT NULL,
  file_state TEXT NOT NULL CHECK (
    file_state IN ('available', 'missing', 'changed', 'unreadable')
  ),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  last_opened_at INTEGER NOT NULL
);

CREATE INDEX documents_recent_idx
  ON documents(last_opened_at DESC);

CREATE INDEX documents_title_idx
  ON documents(title COLLATE NOCASE);

CREATE TABLE provider_profiles (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK (
    kind IN ('cloud_mineru', 'openai_compatible')
  ),
  display_name TEXT NOT NULL,
  endpoint_origin TEXT NOT NULL,
  base_path TEXT NOT NULL DEFAULT '',
  model_id TEXT,
  context_window_override INTEGER,
  secret_account TEXT NOT NULL UNIQUE,
  automatic_cloud_parsing_enabled INTEGER NOT NULL DEFAULT 0 CHECK (
    automatic_cloud_parsing_enabled IN (0, 1)
  ),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE jobs (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (
    kind IN ('cloud_parse', 'normalize', 'translate', 'prefetch', 'inline_assist')
  ),
  priority INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (
    state IN (
      'queued',
      'running',
      'waiting_remote',
      'succeeded',
      'failed',
      'cancelled',
      'status_unknown',
      'interrupted'
    )
  ),
  input_json TEXT NOT NULL,
  checkpoint_json TEXT,
  error_code TEXT,
  attempt_count INTEGER NOT NULL DEFAULT 0,
  max_attempts INTEGER NOT NULL,
  run_after INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  completed_at INTEGER
);

CREATE INDEX jobs_runnable_idx
  ON jobs(state, run_after, priority DESC);

CREATE TABLE processed_commands (
  command_id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  receipt_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);

CREATE INDEX processed_commands_expiry_idx
  ON processed_commands(expires_at);

CREATE TABLE reading_positions (
  document_id TEXT PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
  chapter_id TEXT,
  block_id TEXT,
  pdf_page INTEGER,
  pdf_scroll_offset REAL,
  view_mode TEXT NOT NULL CHECK (view_mode IN ('bilingual', 'pdf')),
  updated_at INTEGER NOT NULL
);

CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);
