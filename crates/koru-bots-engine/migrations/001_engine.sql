CREATE TABLE bots (
    id TEXT PRIMARY KEY,
    owner TEXT NOT NULL,
    name TEXT NOT NULL,
    settings BLOB NOT NULL,
    access BLOB NOT NULL,
    role_page TEXT,
    knowledge_pages BLOB NOT NULL,
    memory_page TEXT,
    visibility TEXT NOT NULL CHECK (visibility IN ('private', 'shared')),
    paused INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0, 1)),
    lifecycle TEXT NOT NULL DEFAULT 'active' CHECK (lifecycle IN ('active', 'archived', 'trashed'))
);
CREATE TABLE team (
    owner TEXT PRIMARY KEY,
    paused INTEGER NOT NULL CHECK (paused IN (0, 1))
);
CREATE TABLE threads (
    id TEXT PRIMARY KEY,
    bot TEXT NOT NULL REFERENCES bots(id),
    kind TEXT NOT NULL CHECK (kind IN ('main', 'conversation', 'routine', 'between_bots', 'branch')),
    title TEXT NOT NULL,
    lifecycle TEXT NOT NULL CHECK (lifecycle IN ('active', 'archived', 'trashed')),
    origin_thread TEXT REFERENCES threads(id),
    origin_message TEXT,
    CHECK ((origin_thread IS NULL) = (origin_message IS NULL))
);
CREATE UNIQUE INDEX one_main_thread ON threads(bot) WHERE kind = 'main';
CREATE TABLE participants (
    thread TEXT NOT NULL REFERENCES threads(id),
    participant TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('person', 'bot')),
    read_sequence INTEGER NOT NULL DEFAULT 0 CHECK (read_sequence >= 0),
    PRIMARY KEY (thread, participant)
);
CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    thread TEXT NOT NULL REFERENCES threads(id),
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    sender TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('words', 'thread_reference', 'setup_note')),
    payload BLOB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('accepted', 'published', 'stopped')),
    accepted_at_ms INTEGER NOT NULL,
    UNIQUE (thread, sequence)
);
CREATE TABLE inbox (
    id TEXT PRIMARY KEY,
    thread TEXT NOT NULL REFERENCES threads(id),
    message TEXT NOT NULL REFERENCES messages(id),
    bot TEXT NOT NULL REFERENCES bots(id),
    accepted_at_ms INTEGER NOT NULL,
    UNIQUE (message, bot)
);
CREATE INDEX inbox_order ON inbox(bot, accepted_at_ms, id);
CREATE TABLE obligations (
    id TEXT PRIMARY KEY,
    thread TEXT NOT NULL REFERENCES threads(id),
    message TEXT NOT NULL REFERENCES messages(id),
    bot TEXT NOT NULL REFERENCES bots(id),
    state TEXT NOT NULL CHECK (state IN ('owed', 'delivered', 'answered', 'cancelled', 'expired', 'refused')),
    accepted_at_ms INTEGER NOT NULL,
    answered_by TEXT,
    UNIQUE (message, bot)
);
CREATE INDEX owed_for_bot ON obligations(bot, thread, state, accepted_at_ms, id);
CREATE TABLE turns (
    id TEXT PRIMARY KEY,
    thread TEXT NOT NULL REFERENCES threads(id),
    bot TEXT NOT NULL REFERENCES bots(id),
    session TEXT,
    grant BLOB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'needs_you', 'answered', 'stopped', 'interrupted', 'parked')),
    generation INTEGER NOT NULL CHECK (generation > 0),
    worker_incarnation TEXT,
    stopped INTEGER NOT NULL DEFAULT 0 CHECK (stopped IN (0, 1)),
    details BLOB NOT NULL,
    created_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX one_running_turn ON turns(thread, bot) WHERE state = 'running';
CREATE INDEX turn_queue ON turns(state, created_at_ms, id);
CREATE TABLE deliveries (
    id TEXT PRIMARY KEY,
    turn TEXT NOT NULL REFERENCES turns(id),
    session TEXT NOT NULL,
    history_boundary INTEGER NOT NULL CHECK (history_boundary >= 0),
    state TEXT NOT NULL CHECK (state IN ('offered', 'accepted', 'unknown', 'superseded')),
    payload BLOB NOT NULL
);
CREATE TABLE delivery_obligations (
    delivery TEXT NOT NULL REFERENCES deliveries(id),
    obligation TEXT NOT NULL REFERENCES obligations(id),
    PRIMARY KEY (delivery, obligation)
);
CREATE TABLE questions (
    id TEXT PRIMARY KEY,
    turn TEXT NOT NULL REFERENCES turns(id),
    prompt BLOB NOT NULL,
    answer BLOB,
    state TEXT NOT NULL CHECK (state IN ('open', 'answered', 'cancelled'))
);
CREATE TABLE credentials (
    digest BLOB PRIMARY KEY CHECK (length(digest) = 32),
    scope TEXT NOT NULL,
    bot TEXT NOT NULL REFERENCES bots(id),
    thread TEXT NOT NULL REFERENCES threads(id),
    session TEXT NOT NULL UNIQUE,
    worker_incarnation TEXT NOT NULL,
    grant BLOB NOT NULL,
    revoked INTEGER NOT NULL DEFAULT 0 CHECK (revoked IN (0, 1))
);
CREATE TABLE tool_calls (
    binding TEXT PRIMARY KEY,
    door_call TEXT NOT NULL,
    provider_call TEXT,
    session TEXT NOT NULL,
    turn TEXT NOT NULL REFERENCES turns(id),
    generation INTEGER NOT NULL CHECK (generation > 0),
    tool TEXT NOT NULL,
    argument_digest BLOB NOT NULL CHECK (length(argument_digest) = 32),
    details BLOB NOT NULL,
    result BLOB,
    UNIQUE (session, door_call)
);
CREATE UNIQUE INDEX provider_call_binding ON tool_calls(session, provider_call) WHERE provider_call IS NOT NULL;
CREATE TABLE operations (
    scope TEXT NOT NULL,
    bot TEXT NOT NULL REFERENCES bots(id),
    thread TEXT NOT NULL REFERENCES threads(id),
    id TEXT NOT NULL,
    argument_digest BLOB NOT NULL CHECK (length(argument_digest) = 32),
    recorded_call TEXT NOT NULL REFERENCES tool_calls(binding),
    intent BLOB NOT NULL,
    lease BLOB,
    state TEXT NOT NULL CHECK (state IN ('recorded', 'leased', 'applied', 'refused', 'unresolved')),
    outcome BLOB,
    evidence BLOB,
    recovery BLOB,
    PRIMARY KEY (scope, bot, thread, id)
);
CREATE TABLE outputs (
    id TEXT PRIMARY KEY,
    message TEXT NOT NULL REFERENCES messages(id),
    operation TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('page', 'table', 'task')),
    reference BLOB NOT NULL
);
CREATE TABLE receipts (
    sender TEXT NOT NULL,
    id TEXT NOT NULL,
    digest BLOB NOT NULL CHECK (length(digest) = 32),
    answer BLOB,
    accepted_at_ms INTEGER NOT NULL,
    PRIMARY KEY (sender, id)
);
CREATE INDEX receipt_retention ON receipts(accepted_at_ms) WHERE answer IS NOT NULL;
CREATE TABLE workers (
    bot TEXT PRIMARY KEY REFERENCES bots(id),
    incarnation TEXT NOT NULL UNIQUE,
    started_at_ms INTEGER NOT NULL,
    last_heard_at_ms INTEGER NOT NULL
);

