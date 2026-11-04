-- AI schema 12 -> 13: the profile a chat belongs to (koru-ai D120, approved
-- 2026-11-03: "AI store 12 -> 13").

-- The profile of the window the chat was started in; a branch's source's.
-- The Chats list, a chat's search hits and the workspaces it may read are
-- that profile's. The Store's profile id, as the Store spells it.
--
-- **Null on every chat written before this schema**, because this database
-- cannot say which profile that is: it holds workspace and document ids,
-- and only the Store knows who owns them. The AI service places each such
-- chat as soon as the Store answers it (koru_ai_core::place_chat): the
-- profile owning the workspace the chat last read, else the one it was last
-- granted, else the first profile. It never writes null, and never moves a
-- chat that has a profile.
ALTER TABLE chats ADD COLUMN profile_id TEXT
    CHECK (profile_id IS NULL OR length(profile_id) > 0);

-- One profile's chats, newest activity first: the Chats list.
CREATE INDEX chats_by_profile
    ON chats(profile_id, updated_at_ms DESC, id);
