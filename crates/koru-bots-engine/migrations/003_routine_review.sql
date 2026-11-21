ALTER TABLE routine_occurrences ADD COLUMN review TEXT
    CHECK (review IN ('pending', 'not_required', 'handled', 'skipped'));
ALTER TABLE routine_occurrences ADD COLUMN reviewed_at_ms INTEGER;
ALTER TABLE routine_occurrences ADD COLUMN handled_by_message TEXT REFERENCES messages(id);
CREATE INDEX routine_review_pending ON routine_occurrences(routine, reply, id)
    WHERE review = 'pending';
-- The roster reads the latest published Bot words without walking older
-- Routine reference cards. Keep this in the single approved migration.
CREATE INDEX message_published_words ON messages(thread, sender, sequence DESC)
    WHERE kind = 'words' AND state = 'published';
