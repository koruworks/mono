CREATE TABLE bot_decision_invocations (
  decision_id TEXT NOT NULL PRIMARY KEY CHECK(length(decision_id)=36),
  run_id TEXT NOT NULL CHECK(length(run_id)=36),
  ai_attempt_id TEXT NOT NULL REFERENCES bot_chat_runs(ai_attempt_id) ON DELETE RESTRICT,
  chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE RESTRICT,
  assistant_message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE RESTRICT,
  invocation_id TEXT NOT NULL UNIQUE REFERENCES tool_invocations(id) ON DELETE RESTRICT,
  decision_revision INTEGER NOT NULL CHECK(typeof(decision_revision)='integer' AND decision_revision>0),
  event_id TEXT NOT NULL CHECK(length(event_id)=36),
  created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer'),
  updated_at_ms INTEGER NOT NULL CHECK(typeof(updated_at_ms)='integer')
);
CREATE INDEX bot_decision_invocations_run ON bot_decision_invocations(run_id,decision_id);

CREATE TABLE bot_decision_replay_rollovers (
  decision_id TEXT NOT NULL PRIMARY KEY REFERENCES bot_decision_invocations(decision_id) ON DELETE RESTRICT,
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
CREATE TRIGGER bot_decision_invocations_immutable BEFORE UPDATE ON bot_decision_invocations
WHEN NEW.decision_id IS NOT OLD.decision_id OR NEW.run_id IS NOT OLD.run_id OR NEW.ai_attempt_id IS NOT OLD.ai_attempt_id
 OR NEW.chat_id IS NOT OLD.chat_id OR NEW.assistant_message_id IS NOT OLD.assistant_message_id
 OR NEW.invocation_id IS NOT OLD.invocation_id OR NEW.created_at_ms IS NOT OLD.created_at_ms
BEGIN SELECT RAISE(ABORT,'immutable decision invocation'); END;
CREATE TRIGGER bot_decision_replay_rollovers_immutable BEFORE UPDATE ON bot_decision_replay_rollovers
BEGIN SELECT RAISE(ABORT,'immutable decision replay rollover'); END;
CREATE TRIGGER bot_decision_invocations_retain BEFORE DELETE ON bot_decision_invocations
BEGIN SELECT RAISE(ABORT,'retained decision invocation'); END;
CREATE TRIGGER bot_decision_replay_rollovers_retain BEFORE DELETE ON bot_decision_replay_rollovers
BEGIN SELECT RAISE(ABORT,'retained decision replay rollover'); END;

CREATE TABLE bot_decision_wait_enrollments (
  wait_id TEXT NOT NULL PRIMARY KEY CHECK(length(wait_id)=36),
  decision_id TEXT NOT NULL UNIQUE CHECK(length(decision_id)=36),
  run_id TEXT NOT NULL CHECK(length(run_id)=36),
  ai_attempt_id TEXT NOT NULL REFERENCES bot_chat_runs(ai_attempt_id) ON DELETE RESTRICT,
  decision_revision INTEGER NOT NULL CHECK(typeof(decision_revision)='integer' AND decision_revision>0),
  wait_revision INTEGER NOT NULL CHECK(typeof(wait_revision)='integer' AND wait_revision>0),
  origin_attempt_id TEXT NOT NULL CHECK(length(origin_attempt_id)=36),
  origin_generation INTEGER NOT NULL CHECK(typeof(origin_generation)='integer' AND origin_generation BETWEEN 1 AND 4294967295),
  origin_session_id TEXT NOT NULL CHECK(length(origin_session_id)=36),
  origin_turn_id TEXT NOT NULL CHECK(length(origin_turn_id)=36),
  receipt_digest BLOB NOT NULL CHECK(typeof(receipt_digest)='blob' AND length(receipt_digest)=32),
  receipt_json TEXT NOT NULL CHECK(typeof(receipt_json)='text' AND json_valid(receipt_json) AND length(CAST(receipt_json AS BLOB)) BETWEEN 1 AND 4096),
  created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer')
);
CREATE INDEX bot_decision_wait_enrollments_run ON bot_decision_wait_enrollments(run_id,wait_id);
CREATE TRIGGER bot_decision_wait_enrollments_immutable BEFORE UPDATE ON bot_decision_wait_enrollments
BEGIN SELECT RAISE(ABORT,'immutable AI wait enrollment'); END;
CREATE TRIGGER bot_decision_wait_enrollments_retain BEFORE DELETE ON bot_decision_wait_enrollments
BEGIN SELECT RAISE(ABORT,'retained AI wait enrollment'); END;

CREATE TEMP TABLE p42_fk_guard (violations INTEGER NOT NULL CHECK(violations=0));
INSERT INTO p42_fk_guard SELECT count(*) FROM pragma_foreign_key_check;
DROP TABLE p42_fk_guard;