-- 013_workspace_document_private_crdts_migration.sql

-- workspace.db 12 -> 13. Adds `document_private_crdts`: one reader's private
-- view state for one document — today which toggles they closed — as a
-- complete Loro snapshot of its own, never part of the shared document and
-- never replicated. Device-local by design in this version.
--
-- Keyed to `loro_documents`, the durable identity, never to the derived
-- `documents` listing that `reindex` replaces wholesale. The cascade removes a
-- document's private rows with its storage row. Permanent deletion through the
-- manifest alone does not drop that row, so the Store also prunes rows whose
-- document the manifest no longer lists.
--
-- `owner_profile_id` is the reader's Profile. It lives in catalog.db, which a
-- foreign key cannot reach, so the Store validates it.

CREATE TABLE document_private_crdts (
  document_id      TEXT    NOT NULL REFERENCES loro_documents(id) ON DELETE CASCADE,
  owner_profile_id TEXT    NOT NULL,
  snapshot         BLOB    NOT NULL CHECK (length(snapshot) > 0),
  updated_at       INTEGER NOT NULL,
  PRIMARY KEY (document_id, owner_profile_id)
);
