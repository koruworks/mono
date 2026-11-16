-- Bot Routine controls (P4.3). Three steps in one transaction, in this order:
-- A. bot_command_receipts gains the command kind 'update_routine_access'.
-- B. bot_routine_grants holds a row only for a Routine whose access was edited.
-- C. bot_lifecycle_members gains the member kind 'routine'.
-- A CHECK list cannot be altered in place, so A and C rebuild their table the
-- way 019 and 021 do. Every existing row is copied column for column and is
-- never decoded. Migration 021's tables and columns are not read or written.

-- A. One value in the kind list, one check for that kind, one partial index.
PRAGMA defer_foreign_keys=ON;
CREATE TABLE bot_command_receipts_next (
 user_scope_id TEXT NOT NULL, request_id TEXT NOT NULL, bot_id TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 command_kind TEXT NOT NULL CHECK(command_kind IN
   ('create','update','update_access','create_chat','start','continue','pause','resume',
    'set_knowledge','ensure_memory','create_routine','routine_draft','routine_occurrence',
    'update_routine_access')),
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
    AND length(CAST(outcome_json AS BLOB))<=16384)),
 -- New in ai23. expected_revision is the expected Routine grant revision.
 CHECK(command_kind<>'update_routine_access' OR
   (stage IN ('prepared','completed','rejected') AND expected_revision IS NOT NULL
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
-- New in ai23: the restart scan for an access edit whose barrier install is not confirmed.
CREATE INDEX bot_pending_routine_access ON bot_command_receipts(created_at_ms)
 WHERE command_kind='update_routine_access' AND stage='prepared';

-- B. An edited grant, and only an edited one. A Routine with no row here is at
-- its original grant, which stays in bot_routines. No row is backfilled.
-- request_id is the update_routine_access receipt that wrote the row; each
-- later edit replaces the row and the older receipts stay.
CREATE TABLE bot_routine_grants (
 schedule_id TEXT PRIMARY KEY,
 user_scope_id TEXT NOT NULL,
 owner_device_id TEXT NOT NULL,
 bot_id TEXT NOT NULL,
 request_id TEXT NOT NULL,
 source_bot_policy_revision INTEGER NOT NULL CHECK(source_bot_policy_revision>0),
 grant_revision INTEGER NOT NULL CHECK(grant_revision>1),
 grant_json TEXT NOT NULL CHECK(typeof(grant_json)='text' AND json_valid(grant_json)
   AND length(CAST(grant_json AS BLOB)) BETWEEN 1 AND 65536),
 grant_digest BLOB NOT NULL CHECK(typeof(grant_digest)='blob' AND length(grant_digest)=32),
 updated_at_ms INTEGER NOT NULL,
 FOREIGN KEY(user_scope_id,owner_device_id,bot_id,schedule_id)
   REFERENCES bot_routines(user_scope_id,owner_device_id,bot_id,schedule_id) ON DELETE RESTRICT,
 FOREIGN KEY(user_scope_id,request_id)
   REFERENCES bot_command_receipts(user_scope_id,request_id) ON DELETE RESTRICT
);

-- C. 'routine' in kind, 'owned_routine' in reason, one line in the per-kind
-- column check, one unique index. Everything else is exactly 018's.
CREATE TABLE bot_lifecycle_members_next (
 user_scope_id TEXT NOT NULL,
 operation_id TEXT NOT NULL,
 ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
 kind TEXT NOT NULL CHECK(kind IN ('bot','chat','run_reference','owned_role','attachment_copy','routine')),
 owner TEXT NOT NULL CHECK(owner IN ('ai','delegate','store')),
 chat_id TEXT,
 run_id TEXT,
 workspace_id TEXT,
 document_id TEXT,
 attachment_id TEXT,
 reason TEXT NOT NULL CHECK(reason IN ('bot','owned_conversation','run_of_conversation','role_creation_receipt','exclusive_attachment','owned_routine')),
 prior_state TEXT NOT NULL CHECK(prior_state IN ('live','trashed','archived','absent')),
 intended TEXT NOT NULL CHECK(intended IN ('hide','restore','retain','remove')),
 stage TEXT NOT NULL CHECK(stage IN ('pending','acknowledged','retained','unavailable','conflict')),
 retained_reason TEXT CHECK(retained_reason IN ('shared_reference','already_trashed','uncaptured_descendant','native_result','ownership_unproven','path_attachment','owner_unavailable')),
 child_request_id TEXT,
 updated_at_ms INTEGER NOT NULL,
 PRIMARY KEY(user_scope_id, operation_id, ordinal),
 FOREIGN KEY(user_scope_id, operation_id) REFERENCES bot_lifecycle_intents(user_scope_id, operation_id) ON DELETE RESTRICT,
 CHECK((kind = 'bot' AND chat_id IS NULL AND run_id IS NULL AND workspace_id IS NULL AND document_id IS NULL AND attachment_id IS NULL)
    OR (kind = 'chat' AND chat_id IS NOT NULL AND run_id IS NULL AND workspace_id IS NULL AND document_id IS NULL AND attachment_id IS NULL)
    OR (kind = 'run_reference' AND run_id IS NOT NULL AND workspace_id IS NOT NULL AND attachment_id IS NULL)
    OR (kind = 'owned_role' AND chat_id IS NULL AND run_id IS NULL AND workspace_id IS NOT NULL AND document_id IS NOT NULL AND attachment_id IS NULL)
    OR (kind = 'attachment_copy' AND chat_id IS NOT NULL AND attachment_id IS NOT NULL AND run_id IS NULL AND workspace_id IS NULL AND document_id IS NULL)
    -- New in ai23. document_id is the Routine's Schedule document.
    OR (kind = 'routine' AND chat_id IS NULL AND run_id IS NULL AND workspace_id IS NOT NULL AND document_id IS NOT NULL AND attachment_id IS NULL)),
 CHECK((owner = 'ai') = (child_request_id IS NULL)),
 CHECK((stage = 'retained') = (retained_reason IS NOT NULL))
);
INSERT INTO bot_lifecycle_members_next
 (user_scope_id,operation_id,ordinal,kind,owner,chat_id,run_id,workspace_id,document_id,
  attachment_id,reason,prior_state,intended,stage,retained_reason,child_request_id,updated_at_ms)
SELECT user_scope_id,operation_id,ordinal,kind,owner,chat_id,run_id,workspace_id,document_id,
 attachment_id,reason,prior_state,intended,stage,retained_reason,child_request_id,updated_at_ms
 FROM bot_lifecycle_members;
DROP TABLE bot_lifecycle_members;
ALTER TABLE bot_lifecycle_members_next RENAME TO bot_lifecycle_members;
CREATE UNIQUE INDEX bot_lifecycle_member_bot ON bot_lifecycle_members(user_scope_id, operation_id) WHERE kind = 'bot';
CREATE UNIQUE INDEX bot_lifecycle_member_chat ON bot_lifecycle_members(user_scope_id, operation_id, chat_id) WHERE kind = 'chat';
CREATE UNIQUE INDEX bot_lifecycle_member_run ON bot_lifecycle_members(user_scope_id, operation_id, run_id) WHERE kind = 'run_reference';
CREATE UNIQUE INDEX bot_lifecycle_member_role ON bot_lifecycle_members(user_scope_id, operation_id, workspace_id, document_id) WHERE kind = 'owned_role';
CREATE UNIQUE INDEX bot_lifecycle_member_attachment ON bot_lifecycle_members(user_scope_id, operation_id, chat_id, attachment_id) WHERE kind = 'attachment_copy';
CREATE UNIQUE INDEX bot_lifecycle_member_routine ON bot_lifecycle_members(user_scope_id, operation_id, document_id) WHERE kind = 'routine';

CREATE TEMP TABLE bot_ai23_fk_guard(ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO bot_ai23_fk_guard
 SELECT NOT EXISTS(SELECT 1 FROM pragma_foreign_key_check);
DROP TABLE bot_ai23_fk_guard;
PRAGMA defer_foreign_keys=OFF;
