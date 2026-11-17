-- ai.db 27 (M8, P8.1): the team pause. One new table; no existing table is
-- rebuilt or rewritten and no column is added. The table is section 2.1 of
-- the M8 contract (revision 4).
--
-- No column holds a task, a title, a page or any other content: every value
-- here is an identity, a digest, a counter, a flag or a time.

-- The team pause, per person and computer. This row is the durable truth.
-- The delegate holds a copy in memory for one run and is closed without it.
-- No row means never paused: revision 0, not paused.
CREATE TABLE bot_team_pause (
  user_scope_id TEXT NOT NULL,
  owner_device_id TEXT NOT NULL,
  paused INTEGER NOT NULL CHECK(paused IN (0,1)),
  -- The epoch sent to the delegate. 1 at the first change. Never decreases.
  revision INTEGER NOT NULL CHECK(revision >= 1),
  -- The request that produced this revision, and what it asked for.
  request_id TEXT NOT NULL CHECK(length(request_id) = 36),
  fingerprint TEXT NOT NULL CHECK(length(fingerprint) = 64),
  -- 'acknowledged' once any delegate run echoed this revision.
  stage TEXT NOT NULL CHECK(stage IN ('pending','acknowledged')),
  changed_at_ms INTEGER NOT NULL,
  acknowledged_at_ms INTEGER,
  PRIMARY KEY(user_scope_id, owner_device_id),
  CHECK((stage = 'acknowledged') = (acknowledged_at_ms IS NOT NULL))
);
