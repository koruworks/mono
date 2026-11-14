PRAGMA defer_foreign_keys=ON;
CREATE TABLE bot_command_receipts_next (
 user_scope_id TEXT NOT NULL, request_id TEXT NOT NULL, bot_id TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 command_kind TEXT NOT NULL CHECK(command_kind IN
   ('create','update','update_access','create_chat','start','continue','pause','resume',
    'set_knowledge','ensure_memory','create_routine','routine_draft','routine_occurrence')),
 stage TEXT NOT NULL CHECK(stage IN ('prepared','accepted','rejected','completed')),
 planned_run_id TEXT UNIQUE,
 captured_config_revision INTEGER CHECK(captured_config_revision>=1),
 captured_policy_revision INTEGER CHECK(captured_policy_revision>=1),
 submission_version INTEGER CHECK(submission_version IN (1,2)),
 submission_json TEXT CHECK(submission_json IS NULL OR (json_valid(submission_json)
   AND length(CAST(submission_json AS BLOB))<=4194304)),
 desired_pause INTEGER CHECK(desired_pause IN (0,1)),
 expected_revision INTEGER CHECK(expected_revision>=1),
 pause_ack_stage TEXT CHECK(pause_ack_stage IN ('pending','acknowledged')),
 outcome_json TEXT CHECK(outcome_json IS NULL OR (json_valid(outcome_json)
   AND length(CAST(outcome_json AS BLOB))<=1048576)),
 created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
 PRIMARY KEY(user_scope_id,request_id),
 CHECK((command_kind IN ('start','continue') AND planned_run_id IS NOT NULL
   AND captured_config_revision IS NOT NULL AND captured_policy_revision IS NOT NULL
   AND submission_version IS NOT NULL AND submission_version=1 AND submission_json IS NOT NULL)
 OR (command_kind='routine_occurrence' AND planned_run_id IS NOT NULL
   AND captured_config_revision IS NOT NULL AND captured_policy_revision IS NOT NULL
   AND submission_version IS NOT NULL AND submission_version=2 AND submission_json IS NOT NULL
   AND typeof(submission_json)='text' AND expected_revision IS NULL)
 OR (command_kind NOT IN ('start','continue','routine_occurrence') AND planned_run_id IS NULL
   AND captured_config_revision IS NULL AND captured_policy_revision IS NULL
   AND submission_json IS NULL AND submission_version IS NULL)),
 CHECK((command_kind IN ('pause','resume') AND desired_pause IS NOT NULL
   AND expected_revision IS NOT NULL AND pause_ack_stage IS NOT NULL)
 OR (command_kind NOT IN ('pause','resume') AND desired_pause IS NULL AND pause_ack_stage IS NULL)),
 CHECK(command_kind<>'pause' OR desired_pause=1),
 CHECK(command_kind<>'resume' OR desired_pause=0),
 CHECK(command_kind<>'set_knowledge' OR
   (stage='completed' AND expected_revision IS NOT NULL AND outcome_json IS NOT NULL
    AND length(CAST(outcome_json AS BLOB))<=16384)),
 CHECK(command_kind<>'ensure_memory' OR
   (stage IN ('prepared','completed','rejected') AND expected_revision IS NOT NULL
    AND outcome_json IS NOT NULL AND length(CAST(outcome_json AS BLOB))<=16384)),
 CHECK(command_kind<>'create_routine' OR
   (stage IN ('prepared','completed','rejected') AND expected_revision IS NOT NULL
    AND outcome_json IS NOT NULL AND typeof(outcome_json)='text'
    AND length(CAST(outcome_json AS BLOB))<=16384)),
 CHECK(command_kind<>'routine_draft' OR
   (stage IN ('prepared','rejected') AND expected_revision IS NULL
    AND outcome_json IS NOT NULL AND typeof(outcome_json)='text'
    AND length(CAST(outcome_json AS BLOB))<=16384))
);
INSERT INTO bot_command_receipts_next
 (user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,
  captured_config_revision,captured_policy_revision,submission_version,
  submission_json,desired_pause,expected_revision,pause_ack_stage,outcome_json,
  created_at_ms,updated_at_ms)
SELECT user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,
 captured_config_revision,captured_policy_revision,submission_version,
 submission_json,desired_pause,expected_revision,pause_ack_stage,outcome_json,
 created_at_ms,updated_at_ms FROM bot_command_receipts;
DROP TABLE bot_command_receipts;
ALTER TABLE bot_command_receipts_next RENAME TO bot_command_receipts;
CREATE INDEX bot_command_guard ON bot_command_receipts(bot_id,command_kind,stage);
CREATE INDEX bot_unresolved_coordination ON bot_command_receipts(created_at_ms)
 WHERE stage IN ('prepared','accepted') AND command_kind IN ('start','continue','pause','resume','routine_occurrence');
CREATE INDEX bot_pending_access ON bot_command_receipts(created_at_ms)
 WHERE command_kind='update_access' AND stage='prepared';
CREATE TEMP TABLE bot_ai21_fk_guard(ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO bot_ai21_fk_guard
 SELECT NOT EXISTS(SELECT 1 FROM pragma_foreign_key_check);
DROP TABLE bot_ai21_fk_guard;
PRAGMA defer_foreign_keys=OFF;

CREATE TABLE bot_routines (
 schedule_id TEXT PRIMARY KEY,
 user_scope_id TEXT NOT NULL,
 owner_device_id TEXT NOT NULL,
 bot_id TEXT NOT NULL,
 workspace_id TEXT NOT NULL,
 creation_request_id TEXT NOT NULL,
 store_request_id TEXT NOT NULL UNIQUE,
 seed_json TEXT NOT NULL CHECK(typeof(seed_json)='text' AND json_valid(seed_json)
   AND length(CAST(seed_json AS BLOB)) BETWEEN 1 AND 4194304),
 seed_hash BLOB NOT NULL CHECK(typeof(seed_hash)='blob' AND length(seed_hash)=32),
 seed_title TEXT NOT NULL CHECK(length(CAST(seed_title AS BLOB)) BETWEEN 1 AND 256),
 source_bot_policy_revision INTEGER NOT NULL CHECK(source_bot_policy_revision>0),
 grant_revision INTEGER NOT NULL CHECK(grant_revision>0),
 grant_json TEXT NOT NULL CHECK(typeof(grant_json)='text' AND json_valid(grant_json)
   AND length(CAST(grant_json AS BLOB)) BETWEEN 1 AND 65536),
 grant_digest BLOB NOT NULL CHECK(typeof(grant_digest)='blob' AND length(grant_digest)=32),
 creation_state TEXT NOT NULL CHECK(creation_state IN ('prepared','unconfirmed','bound','conflict')),
 origin_edit_revision BLOB CHECK(origin_edit_revision IS NULL OR
   (typeof(origin_edit_revision)='blob' AND length(origin_edit_revision) BETWEEN 1 AND 65536)),
 refusal TEXT CHECK(refusal IS NULL OR refusal IN
   ('origin_mismatch','document_changed','owner_changed','authority_changed')),
 created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL,
 UNIQUE(user_scope_id,creation_request_id),
 UNIQUE(user_scope_id,owner_device_id,bot_id,schedule_id),
 FOREIGN KEY(bot_id,workspace_id) REFERENCES bots(bot_id,home_workspace_id) ON DELETE RESTRICT,
 FOREIGN KEY(user_scope_id,creation_request_id)
   REFERENCES bot_command_receipts(user_scope_id,request_id) ON DELETE RESTRICT,
 CHECK((creation_state='bound')=(origin_edit_revision IS NOT NULL)),
 CHECK((creation_state='conflict')=(refusal IS NOT NULL))
);
CREATE INDEX bot_routines_owner_page
 ON bot_routines(user_scope_id,owner_device_id,bot_id,schedule_id);
CREATE INDEX bot_routines_creation_recovery
 ON bot_routines(updated_at_ms,schedule_id)
 WHERE creation_state IN ('prepared','unconfirmed');

CREATE TABLE bot_routine_preparations (
 user_scope_id TEXT NOT NULL,
 owner_device_id TEXT NOT NULL,
 bot_id TEXT NOT NULL,
 schedule_id TEXT NOT NULL,
 occurrence_ms INTEGER NOT NULL,
 definition_revision INTEGER NOT NULL CHECK(definition_revision BETWEEN 1 AND 4294967295),
 definition_digest BLOB NOT NULL CHECK(typeof(definition_digest)='blob' AND length(definition_digest)=32),
 owner_handoff TEXT CHECK(owner_handoff IS NULL OR (typeof(owner_handoff)='text' AND length(owner_handoff)=36)),
 semantic_hash BLOB NOT NULL CHECK(typeof(semantic_hash)='blob' AND length(semantic_hash)=32),
 preparation_hash BLOB NOT NULL CHECK(typeof(preparation_hash)='blob' AND length(preparation_hash)=32),
 submission_request_id TEXT NOT NULL UNIQUE,
 run_id TEXT NOT NULL UNIQUE,
 chat_id TEXT NOT NULL UNIQUE,
 attempt_id TEXT NOT NULL UNIQUE,
 user_message_id TEXT NOT NULL UNIQUE,
 assistant_message_id TEXT NOT NULL UNIQUE,
 config_revision INTEGER NOT NULL CHECK(config_revision>0),
 policy_revision INTEGER NOT NULL CHECK(policy_revision>0),
 grant_revision INTEGER NOT NULL CHECK(grant_revision>0),
 submission_json TEXT NOT NULL CHECK(typeof(submission_json)='text' AND json_valid(submission_json)
   AND length(CAST(submission_json AS BLOB)) BETWEEN 1 AND 4194304),
 capture_json TEXT NOT NULL CHECK(typeof(capture_json)='text' AND json_valid(capture_json)
   AND length(CAST(capture_json AS BLOB)) BETWEEN 1 AND 1048576),
 stage TEXT NOT NULL CHECK(stage IN ('prepared','linked','closed')),
 linked_request_id TEXT,
 linked_chat_id TEXT REFERENCES bot_chats(chat_id) ON DELETE RESTRICT,
 close_reason TEXT CHECK(close_reason IS NULL OR close_reason IN
   ('expired_unclaimed','slot_skipped','other_winner','configuration_changed','authority_changed','stopped')),
 created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL,
 PRIMARY KEY(user_scope_id,owner_device_id,schedule_id,occurrence_ms),
 FOREIGN KEY(user_scope_id,owner_device_id,bot_id,schedule_id)
   REFERENCES bot_routines(user_scope_id,owner_device_id,bot_id,schedule_id) ON DELETE RESTRICT,
 FOREIGN KEY(user_scope_id,submission_request_id)
   REFERENCES bot_command_receipts(user_scope_id,request_id) ON DELETE RESTRICT,
 FOREIGN KEY(user_scope_id,linked_request_id)
   REFERENCES bot_command_receipts(user_scope_id,request_id) ON DELETE RESTRICT,
 CHECK((stage='linked' AND linked_request_id=submission_request_id
     AND linked_request_id IS NOT NULL AND linked_chat_id=chat_id AND linked_chat_id IS NOT NULL)
   OR (stage<>'linked' AND linked_request_id IS NULL AND linked_chat_id IS NULL)),
 CHECK((stage='closed')=(close_reason IS NOT NULL))
);
CREATE INDEX bot_routine_preparation_recovery
 ON bot_routine_preparations(user_scope_id,owner_device_id,updated_at_ms,schedule_id,occurrence_ms)
 WHERE stage='prepared';
CREATE INDEX bot_routine_conversations
 ON bot_routine_preparations(bot_id,schedule_id,occurrence_ms DESC)
 WHERE stage='linked';
