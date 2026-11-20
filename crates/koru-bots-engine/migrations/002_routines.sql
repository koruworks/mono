-- Routines are engine-owned values, never Store Schedule or Run documents.
CREATE TABLE routines (
    id TEXT PRIMARY KEY,
    bot TEXT NOT NULL REFERENCES bots(id),
    thread TEXT NOT NULL UNIQUE REFERENCES threads(id),
    created_by TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    grant_revision INTEGER NOT NULL CHECK(grant_revision > 0),
    source_policy_revision INTEGER NOT NULL CHECK(source_policy_revision > 0),
    name TEXT NOT NULL,
    objective TEXT NOT NULL,
    recurrence BLOB NOT NULL,
    access BLOB NOT NULL,
    initial_access BLOB NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0,1)),
    first_enabled_at_ms INTEGER,
    effective_at_ms INTEGER NOT NULL,
    observed_through_ms INTEGER,
    next_due_ms INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX routines_bot ON routines(bot,id);
CREATE UNIQUE INDEX routine_creation ON routines(bot,created_by);
CREATE INDEX routines_due ON routines(next_due_ms,id) WHERE enabled=1;
CREATE TABLE routine_occurrences (
    id TEXT PRIMARY KEY,
    routine TEXT NOT NULL REFERENCES routines(id),
    definition_revision INTEGER NOT NULL CHECK(definition_revision > 0),
    occurrence_ms INTEGER NOT NULL,
    manual INTEGER NOT NULL CHECK(manual IN (0,1)),
    state TEXT NOT NULL CHECK(state IN ('queued','running','answered','missed','skipped','cancelled','failed')),
    reason TEXT,
    message TEXT UNIQUE REFERENCES messages(id),
    reply TEXT REFERENCES messages(id),
    card TEXT UNIQUE REFERENCES messages(id),
    deadline_ms INTEGER,
    recorded_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX routine_due_once ON routine_occurrences(routine,occurrence_ms) WHERE manual=0;
CREATE INDEX routine_history ON routine_occurrences(routine,occurrence_ms DESC,id);
CREATE INDEX routine_waiting ON routine_occurrences(deadline_ms,id) WHERE state='queued';
CREATE INDEX routine_pending ON routine_occurrences(state,id,message) WHERE state IN ('queued','running');
