-- Readings mirror (readings.db), any stored version -> the current one.
--
-- The mirror is derived, machine-local and never synced (OCR TECHNICAL §7,
-- "Derived, machine-local"): every row is rebuilt from the reading documents
-- the Store holds, by the mirror's catch-up, which compares each listed
-- reading document's revision with `reading_sources` and applies the ones
-- behind. So its migration is a rebuild: drop every mirror-owned table and let
-- the current schema plus the catch-up repopulate it.
--
-- `read_attempts` is local state rather than a projection of anything synced
-- (failures that are true only on this computer, and their backoff). It is
-- dropped too: the cost is that a file which crashed the worker is offered to
-- it once more after a mirror schema change, which the attempts table then
-- records again.
--
-- **Version-agnostic on purpose**, as the projection's and the index's are: a
-- rebuild does not care where it started, so a future bump repoints the code
-- at a new file rather than adding a step to a chain.
DROP TABLE IF EXISTS reading_pages;
DROP TABLE IF EXISTS readings;
DROP TABLE IF EXISTS reading_sources;
DROP TABLE IF EXISTS read_attempts;
