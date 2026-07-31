CREATE TABLE reading_conversations (
  id TEXT PRIMARY KEY,
  document_id TEXT NOT NULL UNIQUE REFERENCES documents(id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE reading_messages (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL
    REFERENCES reading_conversations(id) ON DELETE CASCADE,
  role TEXT NOT NULL CHECK (role IN ('reader', 'assistant')),
  state TEXT NOT NULL CHECK (
    state IN ('queued', 'streaming', 'ready', 'failed', 'cancelled')
  ),
  text TEXT NOT NULL DEFAULT '',
  selection_context_json TEXT,
  responding_to_message_id TEXT REFERENCES reading_messages(id) ON DELETE CASCADE,
  retry_of_message_id TEXT REFERENCES reading_messages(id) ON DELETE SET NULL,
  endpoint_fingerprint TEXT,
  model_id TEXT,
  error_code TEXT,
  error_safe_json TEXT,
  sequence INTEGER NOT NULL CHECK (sequence >= 0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE (conversation_id, sequence),
  CHECK (
    (
      role = 'reader'
      AND state = 'ready'
      AND responding_to_message_id IS NULL
      AND retry_of_message_id IS NULL
      AND endpoint_fingerprint IS NULL
      AND model_id IS NULL
    )
    OR
    (
      role = 'assistant'
      AND responding_to_message_id IS NOT NULL
      AND endpoint_fingerprint IS NOT NULL
      AND model_id IS NOT NULL
      AND selection_context_json IS NULL
    )
  )
);

CREATE INDEX reading_messages_conversation_idx
  ON reading_messages(conversation_id, sequence);

CREATE UNIQUE INDEX reading_messages_one_active_assistant_idx
  ON reading_messages(conversation_id)
  WHERE role = 'assistant' AND state IN ('queued', 'streaming');

CREATE TABLE reading_citations (
  id TEXT PRIMARY KEY,
  message_id TEXT NOT NULL REFERENCES reading_messages(id) ON DELETE CASCADE,
  chapter_id TEXT NOT NULL REFERENCES chapters(id) ON DELETE CASCADE,
  block_id TEXT NOT NULL REFERENCES blocks(id) ON DELETE CASCADE,
  page INTEGER NOT NULL CHECK (page >= 1),
  label TEXT NOT NULL,
  order_index INTEGER NOT NULL CHECK (order_index >= 0),
  UNIQUE (message_id, order_index)
);

CREATE INDEX reading_citations_block_idx
  ON reading_citations(block_id, message_id);
