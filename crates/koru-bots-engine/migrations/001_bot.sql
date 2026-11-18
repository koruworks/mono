CREATE TABLE turn_journal (
    sequence INTEGER PRIMARY KEY,
    turn TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('input', 'event', 'tool_call', 'engine_delivery')),
    tool_name TEXT,
    payload BLOB NOT NULL,
    recorded_at_ms INTEGER NOT NULL,
    CHECK ((kind = 'tool_call') = (tool_name IS NOT NULL))
);
CREATE INDEX turn_journal_turn ON turn_journal(turn, sequence);
CREATE TABLE agent_state (
    thread TEXT NOT NULL,
    session TEXT NOT NULL,
    provider TEXT NOT NULL,
    resume_state BLOB NOT NULL,
    last_delivery TEXT,
    saved_at_ms INTEGER NOT NULL,
    PRIMARY KEY (thread, session)
);
