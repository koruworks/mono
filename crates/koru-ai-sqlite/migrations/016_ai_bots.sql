-- Engine-owned Bot configuration. No credentials, Run status or Routine grants.
CREATE TABLE bots (
 bot_id TEXT PRIMARY KEY, user_scope_id TEXT NOT NULL, home_workspace_id TEXT NOT NULL,
 owner_device_id TEXT NOT NULL, name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 256),
 role_workspace_id TEXT NOT NULL, role_document_id TEXT NOT NULL,
 role_owned INTEGER NOT NULL DEFAULT 0 CHECK(role_owned IN (0,1)),
 role_creation_request_id TEXT,
 working_folder TEXT NOT NULL, provider TEXT NOT NULL, model TEXT NOT NULL,
 reasoning TEXT NOT NULL, mode TEXT, fast INTEGER CHECK(fast IN (0,1)),
 revision INTEGER NOT NULL CHECK(revision >= 1), policy_revision INTEGER NOT NULL CHECK(policy_revision >= 1),
 access TEXT NOT NULL CHECK(access IN ('default','workspace')),
 lifecycle TEXT NOT NULL DEFAULT 'active' CHECK(lifecycle IN ('active','archived','trashed')),
 paused INTEGER NOT NULL DEFAULT 0 CHECK(paused IN (0,1)),
 folder_lock_request_id TEXT, folder_frozen_at_ms INTEGER,
 created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
 FOREIGN KEY(user_scope_id,role_creation_request_id) REFERENCES bot_command_receipts(user_scope_id,request_id),
 CHECK((role_owned=0 AND role_creation_request_id IS NULL) OR (role_owned=1 AND role_creation_request_id IS NOT NULL))
);
CREATE INDEX bots_home ON bots(user_scope_id, home_workspace_id);
CREATE TABLE bot_selected_pages (
 bot_id TEXT NOT NULL REFERENCES bots(bot_id) ON DELETE RESTRICT,
 workspace_id TEXT NOT NULL, document_id TEXT NOT NULL, selected_at_ms INTEGER NOT NULL,
 PRIMARY KEY(bot_id, workspace_id, document_id)
);
CREATE TABLE bot_chats (
 chat_id TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE RESTRICT,
 bot_id TEXT NOT NULL REFERENCES bots(bot_id) ON DELETE RESTRICT,
 purpose TEXT NOT NULL CHECK(purpose IN ('main','additional')), created_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX bot_main_chat ON bot_chats(bot_id) WHERE purpose = 'main';
CREATE TABLE bot_command_receipts (
 user_scope_id TEXT NOT NULL, request_id TEXT NOT NULL, bot_id TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 command_kind TEXT NOT NULL CHECK(command_kind IN ('create','update','update_access','create_chat','start','continue','pause','resume')),
 stage TEXT NOT NULL CHECK(stage IN ('prepared','accepted','rejected','completed')),
 planned_run_id TEXT UNIQUE,
 captured_config_revision INTEGER CHECK(captured_config_revision >= 1),
 captured_policy_revision INTEGER CHECK(captured_policy_revision >= 1),
 submission_version INTEGER CHECK(submission_version = 1),
 submission_json TEXT CHECK(submission_json IS NULL OR (json_valid(submission_json) AND length(CAST(submission_json AS BLOB)) <= 4194304)),
 desired_pause INTEGER CHECK(desired_pause IN (0,1)),
 expected_revision INTEGER CHECK(expected_revision >= 1),
 pause_ack_stage TEXT CHECK(pause_ack_stage IN ('pending','acknowledged')),
 outcome_json TEXT CHECK(outcome_json IS NULL OR (json_valid(outcome_json) AND length(CAST(outcome_json AS BLOB)) <= 1048576)),
 created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
 PRIMARY KEY(user_scope_id, request_id),
 CHECK ((command_kind IN ('start','continue') AND planned_run_id IS NOT NULL
   AND captured_config_revision IS NOT NULL AND captured_policy_revision IS NOT NULL
   AND submission_version IS NOT NULL AND submission_version=1 AND submission_json IS NOT NULL)
   OR (command_kind NOT IN ('start','continue') AND planned_run_id IS NULL
   AND captured_config_revision IS NULL AND captured_policy_revision IS NULL
   AND submission_json IS NULL AND submission_version IS NULL)),
 CHECK ((command_kind IN ('pause','resume') AND desired_pause IS NOT NULL
   AND expected_revision IS NOT NULL AND pause_ack_stage IS NOT NULL)
   OR (command_kind NOT IN ('pause','resume') AND desired_pause IS NULL AND pause_ack_stage IS NULL)),
 CHECK(command_kind <> 'pause' OR desired_pause=1),
 CHECK(command_kind <> 'resume' OR desired_pause=0)
);
