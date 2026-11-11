-- Search schema 5 -> 6: the words read out of a picture or a file are indexed,
-- one fragment per page.
--
-- A picture and a file used to be findable by their name alone. They are now
-- findable by the words read out of their bytes (TECHNICAL D13 of the OCR
-- design): for each live `image` or `file` node whose blob has a reading, one
-- `read` fragment per non-empty page. `fragments` gains a nullable `page`
-- column, set only for a file of more than one page, and a hit's page is read
-- from the winning row. A multi-word query therefore matches within one page,
-- because every term must already co-occur in one fragment.
--
-- Search is a derived cache of the projection, so its migration is a rebuild:
-- discard every search-owned table and let the current schema plus reconcile
-- repopulate it. User content is not stored in this database.
--
-- **Version-agnostic on purpose.** This is the only migration the search index
-- runs, from any stored version to the current one, because a rebuild does not
-- care where it started. A future bump replaces the file it points at rather
-- than adding a step to a chain.
DROP TRIGGER IF EXISTS fragments_ai;
DROP TRIGGER IF EXISTS fragments_ad;
DROP TRIGGER IF EXISTS fragments_au;
DROP TABLE IF EXISTS fragment_fts;
DROP TABLE IF EXISTS fragments;
DROP TABLE IF EXISTS attr_values;
DROP TABLE IF EXISTS documents;
DROP TABLE IF EXISTS listing;
DROP TABLE IF EXISTS workspaces;
DROP TABLE IF EXISTS recent_targets;
DROP TABLE IF EXISTS recent_queries;
DROP TABLE IF EXISTS state;
