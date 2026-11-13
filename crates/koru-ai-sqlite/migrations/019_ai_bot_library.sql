PRAGMA defer_foreign_keys=ON;
CREATE TABLE bot_command_receipts_next (
 user_scope_id TEXT NOT NULL, request_id TEXT NOT NULL, bot_id TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 command_kind TEXT NOT NULL CHECK(command_kind IN
   ('create','update','update_access','create_chat','start','continue','pause','resume',
    'set_knowledge','ensure_memory')),
 stage TEXT NOT NULL CHECK(stage IN ('prepared','accepted','rejected','completed')),
 planned_run_id TEXT UNIQUE,
 captured_config_revision INTEGER CHECK(captured_config_revision>=1),
 captured_policy_revision INTEGER CHECK(captured_policy_revision>=1),
 submission_version INTEGER CHECK(submission_version=1),
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
 OR (command_kind NOT IN ('start','continue') AND planned_run_id IS NULL
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
    AND outcome_json IS NOT NULL AND length(CAST(outcome_json AS BLOB))<=16384))
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
 WHERE stage IN ('prepared','accepted') AND command_kind IN ('start','continue','pause','resume');
CREATE INDEX bot_pending_access ON bot_command_receipts(created_at_ms)
 WHERE command_kind='update_access' AND stage='prepared';
CREATE TEMP TABLE bot_ai19_fk_guard(ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO bot_ai19_fk_guard
 SELECT NOT EXISTS(SELECT 1 FROM pragma_foreign_key_check);
DROP TABLE bot_ai19_fk_guard;
PRAGMA defer_foreign_keys=OFF;

CREATE UNIQUE INDEX bots_library_home ON bots(bot_id,home_workspace_id);

CREATE TABLE bot_knowledge_pages (
 bot_id TEXT NOT NULL,
 workspace_id TEXT NOT NULL,
 document_id TEXT NOT NULL,
 ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 0 AND 15),
 added_at_ms INTEGER NOT NULL,
 PRIMARY KEY(bot_id,ordinal),
 UNIQUE(bot_id,workspace_id,document_id),
 FOREIGN KEY(bot_id,workspace_id)
   REFERENCES bots(bot_id,home_workspace_id) ON DELETE RESTRICT
);

CREATE TABLE bot_memory_creation_intents (
 user_scope_id TEXT NOT NULL,
 request_id TEXT NOT NULL,
 bot_id TEXT NOT NULL UNIQUE,
 owner_device_id TEXT NOT NULL,
 workspace_id TEXT NOT NULL,
 candidate_document_id TEXT NOT NULL,
 store_request_id TEXT NOT NULL UNIQUE,
 seed_format INTEGER NOT NULL CHECK(seed_format=1),
 seed_title TEXT NOT NULL CHECK(length(CAST(seed_title AS BLOB)) BETWEEN 1 AND 256),
 seed_hash BLOB NOT NULL CHECK(length(seed_hash)=32),
 state TEXT NOT NULL CHECK(state IN
   ('prepared','unconfirmed','bound','conflict','abandoned')),
 origin_edit_revision BLOB CHECK(origin_edit_revision IS NULL OR
   length(origin_edit_revision) BETWEEN 1 AND 65536),
 refusal TEXT CHECK(refusal IS NULL OR refusal IN
   ('owner_unavailable','store_unavailable','document_changed',
    'document_moved','document_trashed','lifecycle_changed','origin_mismatch')),
 created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL,
 PRIMARY KEY(user_scope_id,request_id),
 UNIQUE(workspace_id,candidate_document_id),
 UNIQUE(user_scope_id,request_id,bot_id,workspace_id,candidate_document_id),
 FOREIGN KEY(user_scope_id,request_id)
   REFERENCES bot_command_receipts(user_scope_id,request_id) ON DELETE RESTRICT,
 CHECK((state='bound')=(origin_edit_revision IS NOT NULL)),
 CHECK((state='conflict')=(refusal IS NOT NULL))
);
CREATE INDEX bot_memory_creation_pending
 ON bot_memory_creation_intents(updated_at_ms,request_id)
 WHERE state IN ('prepared','unconfirmed');

CREATE TABLE bot_memory_bindings (
 bot_id TEXT PRIMARY KEY,
 user_scope_id TEXT NOT NULL,
 workspace_id TEXT NOT NULL,
 document_id TEXT NOT NULL,
 creation_request_id TEXT NOT NULL,
 bound_at_ms INTEGER NOT NULL,
 UNIQUE(workspace_id,document_id),
 FOREIGN KEY(bot_id,workspace_id)
   REFERENCES bots(bot_id,home_workspace_id) ON DELETE RESTRICT,
 FOREIGN KEY(user_scope_id,creation_request_id,bot_id,workspace_id,document_id)
   REFERENCES bot_memory_creation_intents
   (user_scope_id,request_id,bot_id,workspace_id,candidate_document_id)
   ON DELETE RESTRICT
);

CREATE TABLE bot_library_captures (
 user_scope_id TEXT NOT NULL,
 submission_request_id TEXT NOT NULL,
 bot_id TEXT NOT NULL,
 capture_version INTEGER NOT NULL CHECK(capture_version=1),
 config_revision INTEGER NOT NULL CHECK(config_revision>=1),
 knowledge_count INTEGER NOT NULL CHECK(knowledge_count BETWEEN 0 AND 16),
 memory_state TEXT NOT NULL CHECK(memory_state IN ('absent','captured')),
 memory_workspace_id TEXT,
 memory_document_id TEXT,
 memory_history_version_id TEXT,
 memory_binding_request_id TEXT,
 index_digest BLOB NOT NULL CHECK(length(index_digest)=32),
 indexed_count INTEGER NOT NULL CHECK(indexed_count BETWEEN 0 AND 256),
 omitted_count INTEGER NOT NULL CHECK(omitted_count>=0),
 invalid_count INTEGER NOT NULL CHECK(invalid_count>=0),
 created_at_ms INTEGER NOT NULL,
 PRIMARY KEY(user_scope_id,submission_request_id),
 FOREIGN KEY(user_scope_id,submission_request_id)
   REFERENCES bot_command_receipts(user_scope_id,request_id) ON DELETE RESTRICT,
 CHECK((memory_state='absent' AND memory_workspace_id IS NULL
   AND memory_document_id IS NULL AND memory_history_version_id IS NULL
   AND memory_binding_request_id IS NULL AND indexed_count=0
   AND omitted_count=0 AND invalid_count=0)
 OR (memory_state='captured' AND memory_workspace_id IS NOT NULL
   AND memory_document_id IS NOT NULL AND memory_binding_request_id IS NOT NULL
   AND memory_history_version_id IS NOT NULL))
);

CREATE TABLE bot_knowledge_captures (
 user_scope_id TEXT NOT NULL,
 submission_request_id TEXT NOT NULL,
 ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 0 AND 15),
 workspace_id TEXT NOT NULL,
 document_id TEXT NOT NULL,
 history_version_id TEXT NOT NULL,
 PRIMARY KEY(user_scope_id,submission_request_id,ordinal),
 UNIQUE(user_scope_id,submission_request_id,workspace_id,document_id),
 FOREIGN KEY(user_scope_id,submission_request_id)
   REFERENCES bot_library_captures(user_scope_id,submission_request_id)
   ON DELETE RESTRICT
);

CREATE TABLE bot_memory_index_nodes (
 user_scope_id TEXT NOT NULL,
 submission_request_id TEXT NOT NULL,
 ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 0 AND 255),
 node_id TEXT NOT NULL,
 PRIMARY KEY(user_scope_id,submission_request_id,ordinal),
 UNIQUE(user_scope_id,submission_request_id,node_id),
 FOREIGN KEY(user_scope_id,submission_request_id)
   REFERENCES bot_library_captures(user_scope_id,submission_request_id)
   ON DELETE RESTRICT
);
