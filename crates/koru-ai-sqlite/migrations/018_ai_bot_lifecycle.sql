-- Recoverable Bot lifecycle (P1.7). Every closed vocabulary below has exactly
-- one Rust enum in koru_ai_core::bots::lifecycle; a test keeps both identical.
-- Journal rows are evidence: subject identities are not cascading foreign keys.

-- One lifecycle operation; its own idempotency receipt.
CREATE TABLE bot_lifecycle_intents (
 user_scope_id TEXT NOT NULL,
 operation_id TEXT NOT NULL,
 bot_id TEXT NOT NULL,
 owner_device_id TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('archive','trash','restore','trash_chat','restore_chat','purge_bot','purge_chat')),
 chat_id TEXT,
 fingerprint TEXT NOT NULL CHECK(length(fingerprint) = 64),
 expected_revision INTEGER NOT NULL CHECK(expected_revision >= 1),
 barrier_revision INTEGER CHECK(barrier_revision IS NULL OR barrier_revision >= 1),
 source_operation_id TEXT,
 prior_lifecycle TEXT NOT NULL CHECK(prior_lifecycle IN ('active','archived','trashed')),
 pause_request_id TEXT,
 policy_request_id TEXT,
 policy_barrier_state TEXT CHECK(policy_barrier_state IN ('pending','effective')),
 stage TEXT NOT NULL CHECK(stage IN ('captured','settling','applying','restoring','preserving_references','purging','completed','incomplete','reversed')),
 refusal TEXT CHECK(refusal IN ('owner_unavailable','work_running','person_holding','member_conflict','store_unavailable','revision_changed')),
 requested_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL,
 completed_at_ms INTEGER,
 PRIMARY KEY(user_scope_id, operation_id),
 FOREIGN KEY(user_scope_id, source_operation_id) REFERENCES bot_lifecycle_intents(user_scope_id, operation_id) ON DELETE RESTRICT,
 FOREIGN KEY(user_scope_id, pause_request_id) REFERENCES bot_command_receipts(user_scope_id, request_id) ON DELETE RESTRICT,
 CHECK((kind IN ('trash_chat','restore_chat','purge_chat')) = (chat_id IS NOT NULL)),
 CHECK((kind IN ('restore','restore_chat','purge_bot','purge_chat')) = (source_operation_id IS NOT NULL)),
 -- A whole-Bot operation on an active Bot pauses it first; an archived Bot is already paused and settled.
 CHECK((kind IN ('archive','trash') AND prior_lifecycle = 'active') = (pause_request_id IS NOT NULL)),
 CHECK((pause_request_id IS NULL) = (policy_request_id IS NULL)),
 CHECK((policy_request_id IS NULL) = (policy_barrier_state IS NULL)),
 -- 'reversed' is terminal: the completed Restore naming this operation wrote
 -- it in its own transaction. A reversed row keeps what it had been: the
 -- refusal of a parked operation, or the completion time of a completed one.
 CHECK(stage = 'reversed' OR (stage = 'incomplete') = (refusal IS NOT NULL)),
 CHECK(stage = 'reversed' OR (stage = 'completed') = (completed_at_ms IS NOT NULL)),
 CHECK(stage <> 'reversed' OR ((refusal IS NULL) <> (completed_at_ms IS NULL) AND kind IN ('archive','trash','trash_chat')))
);
-- At most one moving operation per Bot; an incomplete one is parked, still barred.
CREATE UNIQUE INDEX bot_lifecycle_running ON bot_lifecycle_intents(bot_id)
 WHERE stage NOT IN ('completed','incomplete','reversed');
-- Any unfinished operation of a Bot, including a parked incomplete one.
CREATE INDEX bot_lifecycle_open ON bot_lifecycle_intents(bot_id)
 WHERE stage NOT IN ('completed','reversed');
-- An operation is reversed or purged at most once.
CREATE UNIQUE INDEX bot_lifecycle_source ON bot_lifecycle_intents(user_scope_id, source_operation_id)
 WHERE source_operation_id IS NOT NULL;
-- The bounded restart scan.
CREATE INDEX bot_lifecycle_resume ON bot_lifecycle_intents(updated_at_ms)
 WHERE stage NOT IN ('completed','incomplete','reversed');
-- The per-conversation barrier lookup.
CREATE INDEX bot_lifecycle_chat_open ON bot_lifecycle_intents(chat_id)
 WHERE chat_id IS NOT NULL AND stage NOT IN ('completed','reversed');
-- What holds an archived or trashed Bot: its completed whole-Bot Archive or
-- Trash that no Restore reversed (a reversed one is no longer 'completed').
CREATE INDEX bot_lifecycle_held ON bot_lifecycle_intents(bot_id, completed_at_ms)
 WHERE stage = 'completed' AND chat_id IS NULL AND kind IN ('archive','trash');

-- Captured membership. One null group per kind; uniqueness per kind below.
-- A Run reference's document is the delegate's to name and may be recorded
-- after capture; ai.db does not hold it.
CREATE TABLE bot_lifecycle_members (
 user_scope_id TEXT NOT NULL,
 operation_id TEXT NOT NULL,
 ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
 kind TEXT NOT NULL CHECK(kind IN ('bot','chat','run_reference','owned_role','attachment_copy')),
 owner TEXT NOT NULL CHECK(owner IN ('ai','delegate','store')),
 chat_id TEXT,
 run_id TEXT,
 workspace_id TEXT,
 document_id TEXT,
 attachment_id TEXT,
 reason TEXT NOT NULL CHECK(reason IN ('bot','owned_conversation','run_of_conversation','role_creation_receipt','exclusive_attachment')),
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
    OR (kind = 'attachment_copy' AND chat_id IS NOT NULL AND attachment_id IS NOT NULL AND run_id IS NULL AND workspace_id IS NULL AND document_id IS NULL)),
 CHECK((owner = 'ai') = (child_request_id IS NULL)),
 CHECK((stage = 'retained') = (retained_reason IS NOT NULL))
);
CREATE UNIQUE INDEX bot_lifecycle_member_bot ON bot_lifecycle_members(user_scope_id, operation_id) WHERE kind = 'bot';
CREATE UNIQUE INDEX bot_lifecycle_member_chat ON bot_lifecycle_members(user_scope_id, operation_id, chat_id) WHERE kind = 'chat';
CREATE UNIQUE INDEX bot_lifecycle_member_run ON bot_lifecycle_members(user_scope_id, operation_id, run_id) WHERE kind = 'run_reference';
CREATE UNIQUE INDEX bot_lifecycle_member_role ON bot_lifecycle_members(user_scope_id, operation_id, workspace_id, document_id) WHERE kind = 'owned_role';
CREATE UNIQUE INDEX bot_lifecycle_member_attachment ON bot_lifecycle_members(user_scope_id, operation_id, chat_id, attachment_id) WHERE kind = 'attachment_copy';

-- A conversation in Trash: the row's presence is the state. bot_chats is not rebuilt.
CREATE TABLE bot_chat_trash (
 chat_id TEXT PRIMARY KEY REFERENCES bot_chats(chat_id) ON DELETE RESTRICT,
 user_scope_id TEXT NOT NULL,
 operation_id TEXT NOT NULL,
 trashed_at_ms INTEGER NOT NULL,
 FOREIGN KEY(user_scope_id, operation_id) REFERENCES bot_lifecycle_intents(user_scope_id, operation_id) ON DELETE RESTRICT
);
-- An owner's conversations in Trash, in Chat ID order: a Trash page is a
-- range of the caller's own rows.
CREATE INDEX bot_chat_trash_owner ON bot_chat_trash(user_scope_id, chat_id);

-- Content-free receipts that outlive an explicit purge.
CREATE TABLE bot_purge_tombstones (
 user_scope_id TEXT NOT NULL,
 subject_kind TEXT NOT NULL CHECK(subject_kind IN ('bot','chat')),
 subject_id TEXT NOT NULL,
 bot_id TEXT NOT NULL,
 owner_device_id TEXT NOT NULL,
 operation_id TEXT NOT NULL,
 chat_purpose TEXT CHECK(chat_purpose IN ('main','additional')),
 purged_at_ms INTEGER NOT NULL,
 PRIMARY KEY(user_scope_id, subject_kind, subject_id),
 FOREIGN KEY(user_scope_id, operation_id) REFERENCES bot_lifecycle_intents(user_scope_id, operation_id) ON DELETE RESTRICT,
 CHECK((subject_kind = 'chat') = (chat_purpose IS NOT NULL))
);
CREATE INDEX bot_tombstoned_main ON bot_purge_tombstones(bot_id)
 WHERE subject_kind = 'chat' AND chat_purpose = 'main';
CREATE INDEX bot_tombstoned_chat ON bot_purge_tombstones(subject_id)
 WHERE subject_kind = 'chat';

-- Inert history a surviving copy keeps after its source is purged; never a Run binding.
CREATE TABLE bot_historical_run_refs (
 chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
 message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
 source_chat_id TEXT NOT NULL,
 source_message_id TEXT NOT NULL,
 bot_id TEXT NOT NULL,
 run_id TEXT NOT NULL,
 record_workspace_id TEXT NOT NULL,
 recorded_at_ms INTEGER NOT NULL,
 PRIMARY KEY(chat_id, message_id, run_id)
);
CREATE INDEX bot_historical_run_message ON bot_historical_run_refs(message_id);

-- Unresolved ai16 coordination, so recovery pages read only open receipts.
-- Each predicate is exactly its query's; SQLite does not prove IN-list subsets.
CREATE INDEX bot_unresolved_coordination ON bot_command_receipts(created_at_ms)
 WHERE stage IN ('prepared','accepted') AND command_kind IN ('start','continue','pause','resume');
CREATE INDEX bot_pending_access ON bot_command_receipts(created_at_ms)
 WHERE command_kind = 'update_access' AND stage = 'prepared';
