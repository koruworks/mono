-- ai.db 26 (M6): one Bot asking another. One additive column on `bots` and
-- three new tables; no existing table is rebuilt or rewritten. The text below
-- is section 2.2 of the M6 contract (revision 3) with three added columns on
-- `bot_child_chats`, each marked: `wait_id`, `refusal` and `refusal_ordinal`.
--
-- No column holds a task, a title, a page or any other content: every value
-- here is an identity, a digest, a counter or a time.

-- "May ask other bots". Existing Bots get the default, on.
ALTER TABLE bots ADD COLUMN may_ask INTEGER NOT NULL DEFAULT 1 CHECK(may_ask IN (0,1));

-- One row per child request this computer's AI worker prepared. Never deleted.
-- child_request_id is also the request_id of the recipient's 'start' receipt,
-- which holds the recipient (bot_id) and the child Run (planned_run_id).
CREATE TABLE bot_child_chats (
 user_scope_id TEXT NOT NULL,
 child_request_id TEXT NOT NULL CHECK(length(child_request_id)=36),
 parent_bot_id TEXT NOT NULL REFERENCES bots(bot_id) ON DELETE RESTRICT,
 parent_chat_id TEXT NOT NULL REFERENCES bot_chats(chat_id) ON DELETE RESTRICT,
 parent_run_id TEXT NOT NULL CHECK(length(parent_run_id)=36),
 parent_assistant_message_id TEXT NOT NULL CHECK(length(parent_assistant_message_id)=36),
 ask_key TEXT NOT NULL CHECK(length(ask_key)=36),
 ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 0 AND 2),
 ask_fingerprint TEXT NOT NULL,
 -- Not in the contract's text. The parent's wait, minted with the other ids
 -- before the ask reaches the delegate. Every request binding names it, so a
 -- resend after a crash must send the same one, and until the enrollment row
 -- exists nothing else in ai.db holds it. The rows of one ask share it.
 wait_id TEXT NOT NULL CHECK(length(wait_id)=36),
 chat_id TEXT NOT NULL UNIQUE REFERENCES bot_chats(chat_id) ON DELETE RESTRICT,
 created_at_ms INTEGER NOT NULL,
 -- Not in the contract's text. The stable code a refused ask was answered
 -- with, on every row of that ask, and the item it named when it named one.
 -- The recipient's receipt keeps only a closed reason of five values, and the
 -- same key must answer the same refusal later. A code, never a message.
 refusal TEXT CHECK(refusal IS NULL OR (typeof(refusal)='text'
   AND length(CAST(refusal AS BLOB)) BETWEEN 1 AND 64)),
 refusal_ordinal INTEGER CHECK(refusal_ordinal IS NULL
   OR (refusal IS NOT NULL AND refusal_ordinal BETWEEN 0 AND 2)),
 PRIMARY KEY(user_scope_id,child_request_id),
 UNIQUE(user_scope_id,parent_run_id,ask_key,ordinal),
 FOREIGN KEY(user_scope_id,child_request_id)
   REFERENCES bot_command_receipts(user_scope_id,request_id) ON DELETE RESTRICT,
 CHECK(parent_chat_id<>chat_id)
);
CREATE INDEX bot_child_chats_parent ON bot_child_chats(parent_chat_id,created_at_ms,child_request_id);

-- The AI worker's enrollment of a child wait. The decision table
-- (022: bot_decision_wait_enrollments) requires a decision id, so it cannot hold this.
CREATE TABLE bot_child_wait_enrollments (
  wait_id TEXT NOT NULL PRIMARY KEY CHECK(length(wait_id)=36),
  run_id TEXT NOT NULL CHECK(length(run_id)=36),
  ai_attempt_id TEXT NOT NULL REFERENCES bot_chat_runs(ai_attempt_id) ON DELETE RESTRICT,
  wait_revision INTEGER NOT NULL CHECK(typeof(wait_revision)='integer' AND wait_revision>0),
  origin_attempt_id TEXT NOT NULL CHECK(length(origin_attempt_id)=36),
  origin_generation INTEGER NOT NULL CHECK(typeof(origin_generation)='integer' AND origin_generation BETWEEN 1 AND 4294967295),
  origin_session_id TEXT NOT NULL CHECK(length(origin_session_id)=36),
  origin_turn_id TEXT NOT NULL CHECK(length(origin_turn_id)=36),
  receipt_digest BLOB NOT NULL CHECK(typeof(receipt_digest)='blob' AND length(receipt_digest)=32),
  receipt_json TEXT NOT NULL CHECK(typeof(receipt_json)='text' AND json_valid(receipt_json) AND length(CAST(receipt_json AS BLOB)) BETWEEN 1 AND 4096),
  created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer')
);
CREATE INDEX bot_child_wait_enrollments_run ON bot_child_wait_enrollments(run_id,wait_id);
CREATE TRIGGER bot_child_wait_enrollments_immutable BEFORE UPDATE ON bot_child_wait_enrollments
BEGIN SELECT RAISE(ABORT,'immutable AI child wait enrollment'); END;
CREATE TRIGGER bot_child_wait_enrollments_retain BEFORE DELETE ON bot_child_wait_enrollments
BEGIN SELECT RAISE(ABORT,'retained AI child wait enrollment'); END;

-- The replay rollover to the parent's successor. Mirrors
-- bot_decision_replay_rollovers (022), keyed by the wait.
CREATE TABLE bot_child_replay_rollovers (
  wait_id TEXT NOT NULL PRIMARY KEY REFERENCES bot_child_wait_enrollments(wait_id) ON DELETE RESTRICT,
  ai_attempt_id TEXT NOT NULL REFERENCES bot_chat_runs(ai_attempt_id) ON DELETE RESTRICT,
  old_attempt_id TEXT NOT NULL CHECK(length(old_attempt_id)=36),
  old_generation INTEGER NOT NULL CHECK(typeof(old_generation)='integer' AND old_generation BETWEEN 1 AND 4294967295),
  old_session_id TEXT NOT NULL CHECK(length(old_session_id)=36),
  old_turn_id TEXT NOT NULL CHECK(length(old_turn_id)=36),
  old_last_sequence INTEGER CHECK(old_last_sequence IS NULL OR (typeof(old_last_sequence)='integer' AND old_last_sequence>=0)),
  consumer_attempt_id TEXT NOT NULL CHECK(length(consumer_attempt_id)=36),
  consumer_generation INTEGER NOT NULL CHECK(typeof(consumer_generation)='integer' AND consumer_generation BETWEEN 1 AND 4294967295),
  consumer_session_id TEXT NOT NULL CHECK(length(consumer_session_id)=36),
  consumer_turn_id TEXT NOT NULL CHECK(length(consumer_turn_id)=36),
  installed_after_sequence INTEGER CHECK(installed_after_sequence IS NULL OR (typeof(installed_after_sequence)='integer' AND installed_after_sequence>=0)),
  resume_receipt_json TEXT NOT NULL CHECK(typeof(resume_receipt_json)='text' AND json_valid(resume_receipt_json) AND length(CAST(resume_receipt_json AS BLOB)) BETWEEN 1 AND 65536),
  created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer'),
  UNIQUE(ai_attempt_id,consumer_attempt_id,consumer_generation,consumer_session_id,consumer_turn_id)
);
CREATE TRIGGER bot_child_replay_rollovers_immutable BEFORE UPDATE ON bot_child_replay_rollovers
BEGIN SELECT RAISE(ABORT,'immutable child replay rollover'); END;
CREATE TRIGGER bot_child_replay_rollovers_retain BEFORE DELETE ON bot_child_replay_rollovers
BEGIN SELECT RAISE(ABORT,'retained child replay rollover'); END;

CREATE TEMP TABLE bot_ai26_fk_guard(ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO bot_ai26_fk_guard
 SELECT NOT EXISTS(SELECT 1 FROM pragma_foreign_key_check);
DROP TABLE bot_ai26_fk_guard;
