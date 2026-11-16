-- Derived authored-text revisions. The durable authority is the reading CRDT.
CREATE TABLE IF NOT EXISTS reading_corrections (
    workspace_id TEXT NOT NULL,
    parent_document_id TEXT NOT NULL,
    hash TEXT NOT NULL,
    revisions TEXT NOT NULL,
    PRIMARY KEY (workspace_id, parent_document_id, hash)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS reading_corrections_hash
    ON reading_corrections(workspace_id, hash);
-- Revisit snapshots even if an older build last advanced the mirror revision.
DELETE FROM reading_sources;
