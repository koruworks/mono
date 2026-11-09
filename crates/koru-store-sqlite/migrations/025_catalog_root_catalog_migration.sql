-- Catalog 12 -> 13. The per-user root catalog and personal v2
-- (docs/features/root-catalog/TECHNICAL.final.md, §4.2 "Approved SQLite
-- shapes", §5.1, §5.2 step 5, §8.3).
--
-- This file is the DDL half of one step. The Rust half
-- (`catalog::root_catalog::migrate_to_root_catalog`) runs in the same
-- IMMEDIATE transaction: it seeds one root unit per retained Profile and
-- Workspace, migrates every `(Profile, Workspace)` personal history into its
-- Workspace's v2 row with normalized PeerIDs, writes the receipts, drops
-- `profile_reminder_due` and `profile_personal_crdts`, and stamps
-- `user_version = 13`. A failure anywhere rolls all of it back: the catalog
-- stays at 12 with its legacy rows intact and the next open retries.
--
-- Every row below is keyed by `user_scope_id`, the device-local validated
-- user binding kept in `store_state` (`local_user_scope`). It is a SQL key
-- and an authorization boundary, never a field of any Loro document, and it
-- is not dropped merely because the UUIDs it scopes are globally unique.
-- No table here references the former Profile of anything: personal data
-- follows the Workspace, and a Profile's removal cannot hide or strand it.
-- `writer_peer_id` columns hold a Loro PeerID as the i64 with the same bits.

-- One durable Loro snapshot per active root unit: a Profile catalog unit
-- keyed by Profile UUID or a Workspace catalog unit keyed by Workspace
-- UUID. `revision` is the compare-and-set counter of `commit_root_catalog_units`.
CREATE TABLE root_catalog_units (
  user_scope_id TEXT NOT NULL,
  unit_kind TEXT NOT NULL CHECK (unit_kind IN ('profile_catalog', 'workspace_catalog')),
  subject_id TEXT NOT NULL,
  snapshot BLOB NOT NULL CHECK (length(snapshot) > 0),
  revision INTEGER NOT NULL CHECK (typeof(revision) = 'integer' AND revision > 0),
  PRIMARY KEY (user_scope_id, unit_kind, subject_id)
);

-- A private Loro fork of one unit at `base_vv`, with its own writer, held
-- only while its edits cannot be published (§4.3). Never advertised, never
-- exported, not a second replica. Cleared when the edits are published.
CREATE TABLE root_catalog_overlays (
  user_scope_id TEXT NOT NULL,
  unit_kind TEXT NOT NULL CHECK (unit_kind IN ('profile_catalog', 'workspace_catalog')),
  subject_id TEXT NOT NULL,
  base_vv BLOB NOT NULL,
  private_snapshot BLOB NOT NULL CHECK (length(private_snapshot) > 0),
  writer_peer_id INTEGER NOT NULL,
  PRIMARY KEY (user_scope_id, unit_kind, subject_id)
);

-- Personal v2: this user's reminders and personal attributes for one
-- Workspace, independent of placement. `writer_peer_id` is the one addition
-- to the approved shape: §5.2 step 6 requires every future personal writer
-- to use a persisted per-unit identity rather than the Store-global stream,
-- and the identity has to live somewhere durable beside the bytes.
-- `indexed_version` is the version vector the due index was built from, or
-- NULL while it is rebuilding. The Workspace FK is RESTRICT on purpose: it
-- is the guard §5.3 keeps, and Plan 1.3's retirement job is the only thing
-- that removes a row before its Workspace goes.
CREATE TABLE workspace_personal_crdts (
  user_scope_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE RESTRICT,
  snapshot BLOB NOT NULL CHECK (length(snapshot) > 0),
  indexed_version BLOB,
  writer_peer_id INTEGER NOT NULL,
  PRIMARY KEY (user_scope_id, workspace_id)
);

-- The disposable reminder due index, re-keyed from the v1 owner Profile to
-- the user scope. A NULL hint means "ask the reducer", never "not due".
CREATE TABLE workspace_reminder_due (
  user_scope_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  reminder_id TEXT NOT NULL,
  due_at_ms INTEGER,
  PRIMARY KEY (user_scope_id, workspace_id, reminder_id),
  FOREIGN KEY (user_scope_id, workspace_id)
    REFERENCES workspace_personal_crdts(user_scope_id, workspace_id)
    ON DELETE CASCADE
);
CREATE INDEX workspace_reminder_due_by_time
  ON workspace_reminder_due(user_scope_id, due_at_ms, workspace_id, reminder_id);

-- One-time receipts of what this migration did: `root_seed` per Profile and
-- Workspace (the seeded placement, deletion fact and writer, or a Holder's
-- exclusion) and `personal_v2` per Workspace (the sources unioned and every
-- original -> derived PeerID). JSON text, ids only, never content.
CREATE TABLE root_catalog_migration_receipts (
  user_scope_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('root_seed', 'personal_v2')),
  subject_id TEXT NOT NULL,
  receipt TEXT NOT NULL,
  at_ms INTEGER NOT NULL,
  PRIMARY KEY (user_scope_id, kind, subject_id)
);
