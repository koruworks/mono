-- AI owns the transcript; these rows fence replay and durable delivery only.
CREATE TABLE bot_chat_runs (
 ai_attempt_id TEXT NOT NULL PRIMARY KEY REFERENCES inference_attempts(id) ON DELETE RESTRICT,
 user_scope_id TEXT NOT NULL, owner_device_id TEXT NOT NULL, submission_id TEXT NOT NULL,
 bot_id TEXT NOT NULL REFERENCES bots(bot_id) ON DELETE RESTRICT,
 chat_id TEXT NOT NULL REFERENCES bot_chats(chat_id) ON DELETE RESTRICT,
 assistant_message_id TEXT NOT NULL UNIQUE REFERENCES messages(id) ON DELETE RESTRICT,
 run_id TEXT NOT NULL UNIQUE, record_workspace_id TEXT NOT NULL,
 continued_from_run_id TEXT CHECK(continued_from_run_id IS NULL OR continued_from_run_id <> run_id),
 created_at_ms INTEGER NOT NULL,
 delivery_state TEXT NOT NULL DEFAULT 'none' CHECK(delivery_state IN ('none','pending','acknowledged','unconfirmed')),
 delivery_confirmed_at_ms INTEGER, delivery_acknowledged_at_ms INTEGER,
 UNIQUE(user_scope_id,submission_id),
 FOREIGN KEY(user_scope_id,submission_id) REFERENCES bot_command_receipts(user_scope_id,request_id) ON DELETE RESTRICT,
 CHECK((delivery_state='none' AND delivery_confirmed_at_ms IS NULL AND delivery_acknowledged_at_ms IS NULL)
    OR (delivery_state IN ('pending','unconfirmed') AND delivery_confirmed_at_ms IS NOT NULL AND delivery_acknowledged_at_ms IS NULL)
    OR (delivery_state='acknowledged' AND delivery_confirmed_at_ms IS NOT NULL AND delivery_acknowledged_at_ms IS NOT NULL))
);
CREATE INDEX bot_pending_delivery ON bot_chat_runs(user_scope_id,owner_device_id,created_at_ms,ai_attempt_id)
 WHERE delivery_state IN ('pending','unconfirmed');
CREATE TABLE bot_run_replay (
 ai_attempt_id TEXT NOT NULL PRIMARY KEY REFERENCES bot_chat_runs(ai_attempt_id) ON DELETE RESTRICT,
 run_attempt_id TEXT, generation INTEGER CHECK(generation IS NULL OR generation > 0), session_id TEXT,
 turn_id TEXT, last_sequence INTEGER CHECK(last_sequence IS NULL OR last_sequence >= 0),
 stop_reason TEXT CHECK(stop_reason IS NULL OR stop_reason IN ('replay_gap','execution_changed','ai_restart','owner_unavailable')),
 updated_at_ms INTEGER NOT NULL,
 CHECK((run_attempt_id IS NULL AND generation IS NULL AND session_id IS NULL AND turn_id IS NULL AND last_sequence IS NULL)
    OR (run_attempt_id IS NOT NULL AND generation IS NOT NULL AND session_id IS NOT NULL))
);
ALTER TABLE tool_invocations ADD COLUMN bot_source_kind TEXT
 CHECK(bot_source_kind IS NULL OR bot_source_kind IN ('tool','approval','questions'));
ALTER TABLE tool_invocations ADD COLUMN bot_source_id TEXT
 CHECK((bot_source_kind IS NULL AND bot_source_id IS NULL)
    OR (bot_source_kind IS NOT NULL AND bot_source_id IS NOT NULL AND length(CAST(bot_source_id AS BLOB)) BETWEEN 1 AND 4096));
CREATE UNIQUE INDEX bot_tool_source ON tool_invocations(attempt_id,bot_source_kind,bot_source_id)
 WHERE bot_source_kind IS NOT NULL;
-- Pre-release ai17 amendment: bounded per-Bot access/pause admission guards.
CREATE INDEX bot_command_guard
ON bot_command_receipts(bot_id, command_kind, stage);
