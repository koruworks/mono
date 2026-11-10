-- AI schema 13 -> 14: path references alongside copied chat attachments.
-- Approved for path rows only by Composer v2 TECHNICAL §5.2 (2026-11-06).
-- Keep the sent/staged shape, both indexes, and the bound-row trigger.
CREATE TABLE message_attachments_new (
    id                  TEXT PRIMARY KEY,
    chat_id             TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    message_id          TEXT REFERENCES messages(id) ON DELETE CASCADE,
    ordinal             INTEGER,
    kind                TEXT NOT NULL CHECK (kind IN ('image', 'file', 'folder')),
    file_name           TEXT NOT NULL CHECK (length(file_name) BETWEEN 1 AND 255),
    media_type          TEXT NOT NULL CHECK (length(media_type) BETWEEN 3 AND 255),
    byte_count          INTEGER NOT NULL CHECK (byte_count >= 0),
    width               INTEGER CHECK (width IS NULL OR width > 0),
    height              INTEGER CHECK (height IS NULL OR height > 0),
    has_sent_copy       INTEGER NOT NULL DEFAULT 0 CHECK (has_sent_copy IN (0, 1)),
    source              TEXT NOT NULL CHECK (
        source IN ('clipboard', 'drop', 'file_picker', 'workspace', 'path')
    ),
    origin_workspace_id TEXT,
    origin_document_id  TEXT,
    reference_path      TEXT,
    content_blake3      TEXT NOT NULL CHECK (length(content_blake3) = 64),
    created_at_ms       INTEGER NOT NULL,
    bound_at_ms         INTEGER,
    CHECK (length(id) > 0),
    CHECK ((message_id IS NULL) = (ordinal IS NULL)),
    CHECK ((message_id IS NULL) = (bound_at_ms IS NULL)),
    CHECK ((width IS NULL) = (height IS NULL)),
    CHECK (kind IN ('file', 'folder') OR width IS NOT NULL),
    CHECK (ordinal IS NULL OR ordinal >= 0),
    CHECK ((source = 'workspace') = (origin_document_id IS NOT NULL)),
    CHECK ((origin_workspace_id IS NULL) = (origin_document_id IS NULL)),
    CHECK ((source = 'path') = (reference_path IS NOT NULL)),
    CHECK (reference_path IS NULL OR length(reference_path) BETWEEN 1 AND 4096),
    CHECK ((source = 'path') = (byte_count = 0)),
    CHECK (kind != 'folder' OR source = 'path'),
    CHECK (source != 'path' OR kind != 'image'),
    UNIQUE (message_id, ordinal)
);

INSERT INTO message_attachments_new (
    id, chat_id, message_id, ordinal, kind, file_name, media_type,
    byte_count, width, height, has_sent_copy, source, origin_workspace_id,
    origin_document_id, reference_path, content_blake3, created_at_ms, bound_at_ms
)
SELECT id, chat_id, message_id, ordinal, kind, file_name, media_type,
       byte_count, width, height, has_sent_copy, source, origin_workspace_id,
       origin_document_id, NULL, content_blake3, created_at_ms, bound_at_ms
  FROM message_attachments;

DROP TABLE message_attachments;
ALTER TABLE message_attachments_new RENAME TO message_attachments;

CREATE INDEX message_attachments_by_chat ON message_attachments(chat_id);
CREATE INDEX message_attachments_staged ON message_attachments(created_at_ms)
    WHERE message_id IS NULL;

CREATE TRIGGER message_attachments_bound_are_immutable
BEFORE UPDATE ON message_attachments
WHEN OLD.message_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'a sent attachment is immutable');
END;
