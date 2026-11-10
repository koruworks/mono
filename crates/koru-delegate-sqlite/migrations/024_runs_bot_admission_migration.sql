-- Bot queue/reservation facts only. Canonical Run status remains in the Run
-- document. The admission intent may hold one task's input, never a replayed
-- transcript. All times are epoch milliseconds; active_elapsed_ms is duration.
CREATE TABLE bot_admissions (
  run_id TEXT PRIMARY KEY REFERENCES run_intents(run_id),
  queue_sequence INTEGER NOT NULL UNIQUE CHECK(queue_sequence > 0),
  user_scope TEXT NOT NULL,
  device_id TEXT NOT NULL,
  bot_id TEXT NOT NULL,
  request_id TEXT NOT NULL,
  config_revision INTEGER NOT NULL CHECK(config_revision > 0),
  policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
  payload_json TEXT NOT NULL CHECK(json_valid(payload_json) AND length(CAST(payload_json AS BLOB)) <= 4194304),
  state TEXT NOT NULL CHECK(state IN ('queued','reserved','executing','settled','refused')),
  active_elapsed_ms INTEGER NOT NULL DEFAULT 0 CHECK(active_elapsed_ms >= 0),
  active_since_ms INTEGER,
  initial_send_key TEXT,
  initial_send_fingerprint BLOB CHECK(initial_send_fingerprint IS NULL OR length(initial_send_fingerprint) = 32),
  initial_send_attempt_id TEXT,
  initial_send_generation INTEGER CHECK(initial_send_generation > 0),
  initial_send_turn_id TEXT,
  delivery_outcome TEXT NOT NULL DEFAULT 'unconfirmed' CHECK(delivery_outcome IN ('unconfirmed','confirmed')),
  delivery_attempt_id TEXT,
  delivery_generation INTEGER CHECK(delivery_generation > 0),
  delivery_chat_id TEXT,
  delivery_ai_attempt_id TEXT,
  delivery_assistant_message_id TEXT,
  delivery_confirmed_at_ms INTEGER,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(user_scope, device_id, bot_id, request_id),
  CHECK(active_since_ms IS NULL OR state = 'executing'),
  CHECK((initial_send_key IS NULL AND initial_send_fingerprint IS NULL AND
    initial_send_attempt_id IS NULL AND initial_send_generation IS NULL AND initial_send_turn_id IS NULL)
    OR (initial_send_key IS NOT NULL AND initial_send_fingerprint IS NOT NULL AND
    initial_send_attempt_id IS NOT NULL AND initial_send_generation IS NOT NULL)),
  CHECK(delivery_outcome <> 'unconfirmed' OR
    (delivery_attempt_id IS NULL AND delivery_generation IS NULL AND delivery_chat_id IS NULL AND
     delivery_ai_attempt_id IS NULL AND delivery_assistant_message_id IS NULL AND delivery_confirmed_at_ms IS NULL)),
  CHECK(delivery_outcome <> 'confirmed' OR
    (delivery_attempt_id IS NOT NULL AND delivery_generation IS NOT NULL AND
     delivery_chat_id IS NOT NULL AND delivery_ai_attempt_id IS NOT NULL AND
     delivery_assistant_message_id IS NOT NULL AND delivery_confirmed_at_ms IS NOT NULL))
);
CREATE UNIQUE INDEX bot_admissions_execution
  ON bot_admissions(user_scope, device_id, bot_id) WHERE state IN ('reserved','executing');
CREATE INDEX bot_admissions_queue ON bot_admissions(state, queue_sequence);

-- A barrier exists even when no Run has ever been submitted. Epochs come from
-- the AI-owned Bot revision. Same-epoch payload changes conflict; old epochs
-- cannot undo a pause. Queued jobs survive pause/resume without recapturing policy.
CREATE TABLE bot_admission_barriers (
  user_scope TEXT NOT NULL,
  device_id TEXT NOT NULL,
  bot_id TEXT NOT NULL,
  epoch INTEGER NOT NULL CHECK(epoch > 0),
  paused INTEGER NOT NULL CHECK(paused IN (0,1)),
  request_id TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY(user_scope, device_id, bot_id)
);
