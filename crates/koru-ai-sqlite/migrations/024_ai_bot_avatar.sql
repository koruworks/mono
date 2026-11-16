-- The face a person picked for a Bot (first-bot experience, QUESTIONS O155):
-- an index into the fixed table of 24 faces (koru_ai_core BOT_AVATAR_COUNT).
-- NULL means none was picked and the face is derived from the Bot's id, so
-- every Bot made before this column has a face and no row is rewritten.
ALTER TABLE bots ADD COLUMN avatar INTEGER
 CHECK(avatar IS NULL OR (typeof(avatar)='integer' AND avatar BETWEEN 0 AND 23));
