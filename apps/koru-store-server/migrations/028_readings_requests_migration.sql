-- Readings mirror (readings.db), any stored version -> the current one (2).
--
-- Version 2 adds `read_requests` (OCR Plan 3.2): what a person asked for on
-- this computer, *Read all pages* or *Read again*, one row per workspace and
-- hash, removed when the request has been met (OCR TECHNICAL §7, "The local
-- requests table").
--
-- The mirror is derived, machine-local and never synced: every row of
-- `readings`, `reading_pages` and `reading_sources` is rebuilt from the
-- reading documents the Store holds, by the mirror's catch-up. So, as
-- 027_readings_mirror_migration.sql was, this migration is a rebuild: drop
-- every mirror-owned table and let the current schema plus the catch-up
-- repopulate it. Version-agnostic on purpose; the next bump repoints the code
-- at a new file rather than adding a step to a chain.
--
-- `read_attempts` and `read_requests` are local state rather than a
-- projection of anything synced, and both are dropped too. Coming from
-- version 1 there are no requests to lose. On a later bump, a request that
-- had not been met is lost with the rebuild: the item keeps the reading it
-- had (a capped one stays partly read and offers *Read all pages* again), so
-- nothing a person can find is lost, only the wish.
DROP TABLE IF EXISTS reading_pages;
DROP TABLE IF EXISTS readings;
DROP TABLE IF EXISTS reading_sources;
DROP TABLE IF EXISTS read_attempts;
DROP TABLE IF EXISTS read_requests;
