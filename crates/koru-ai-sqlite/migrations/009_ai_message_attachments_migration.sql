-- AI schema 8 -> 9: pictures and files sent to a local agent with a message
-- (chat-attachments TECHNICAL, approved 2026-10-31: "ok"; ADR-479).

-- One row per stored attachment. The bytes live beside ai.db, under
-- attachments/<chat_id>/<id>/, written by the AI service before the row and
-- removed after it. A row with no message is staged: stored, not yet sent;
-- the send binds it to the user message in the turn's own transaction, and
-- from then on it never changes.
CREATE TABLE message_attachments (
    id                  TEXT PRIMARY KEY,
    chat_id             TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    message_id          TEXT REFERENCES messages(id) ON DELETE CASCADE,
    ordinal             INTEGER,
    kind                TEXT NOT NULL CHECK (kind IN ('image', 'file')),
    file_name           TEXT NOT NULL CHECK (length(file_name) BETWEEN 1 AND 255),
    media_type          TEXT NOT NULL CHECK (length(media_type) BETWEEN 3 AND 255),
    byte_count          INTEGER NOT NULL CHECK (byte_count > 0),
    width               INTEGER CHECK (width IS NULL OR width > 0),
    height              INTEGER CHECK (height IS NULL OR height > 0),
    has_sent_copy       INTEGER NOT NULL DEFAULT 0 CHECK (has_sent_copy IN (0, 1)),
    source              TEXT NOT NULL CHECK (
        source IN ('clipboard', 'drop', 'file_picker', 'workspace')
    ),
    -- Provenance of an @-linked file: where the copy came from. Not a live link.
    origin_workspace_id TEXT,
    origin_document_id  TEXT,
    content_blake3      TEXT NOT NULL CHECK (length(content_blake3) = 64),
    created_at_ms       INTEGER NOT NULL,
    bound_at_ms         INTEGER,
    CHECK (length(id) > 0),
    CHECK ((message_id IS NULL) = (ordinal IS NULL)),
    CHECK ((message_id IS NULL) = (bound_at_ms IS NULL)),
    CHECK ((width IS NULL) = (height IS NULL)),
    CHECK (kind = 'file' OR width IS NOT NULL),
    CHECK (ordinal IS NULL OR ordinal >= 0),
    CHECK ((source = 'workspace') = (origin_document_id IS NOT NULL)),
    CHECK ((origin_workspace_id IS NULL) = (origin_document_id IS NULL)),
    UNIQUE (message_id, ordinal)
);
CREATE INDEX message_attachments_by_chat ON message_attachments(chat_id);
CREATE INDEX message_attachments_staged ON message_attachments(created_at_ms)
    WHERE message_id IS NULL;

CREATE TRIGGER message_attachments_bound_are_immutable
BEFORE UPDATE ON message_attachments
WHEN OLD.message_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'a sent attachment is immutable');
END;
