-- AI schema 11 -> 12: a local agent's notices, and a pinned chat (koru-ai
-- D69 and the pinned chats, approved 2026-11-02/03: "AI store schema
-- 11 -> 12, as ONE bump").

-- A short status line a local agent said during a reply that is not part of
-- the reply (D69): Devin's "Hook failed: ..." about a hook on the person's
-- machine. Kept with the reply it came with, in the order it came, at the
-- byte length the reply's Markdown had when it came, so a reopened chat shows
-- it where it showed while the reply ran. The reply is the attempt's, as on
-- a tool row. The session host bounds the words to 500 characters before
-- they reach the worker; the check here is the ceiling, not the rule.
CREATE TABLE agent_notices (
    id             TEXT PRIMARY KEY,
    attempt_id     TEXT NOT NULL REFERENCES inference_attempts(id) ON DELETE CASCADE,
    chat_id        TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    text           TEXT NOT NULL CHECK (length(text) BETWEEN 1 AND 1000),
    text_offset    INTEGER NOT NULL CHECK (text_offset >= 0),
    created_at_ms  INTEGER NOT NULL,
    CHECK (length(id) > 0)
);

CREATE INDEX agent_notices_by_chat
    ON agent_notices(chat_id, created_at_ms, id);

-- When the person pinned the chat; null when it is not pinned, which is
-- every chat written before this schema. The pinned chats are listed oldest
-- pin first. Pinning is not activity: `updated_at_ms` does not move.
ALTER TABLE chats ADD COLUMN pinned_at_ms INTEGER
    CHECK (pinned_at_ms IS NULL OR pinned_at_ms >= 0);
