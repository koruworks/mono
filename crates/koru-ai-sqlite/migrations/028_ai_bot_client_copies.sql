-- ai.db 28 (M8, P8.3): a person's Bots on their linked desktops. Ten new
-- tables: four on the computer that runs the Bots (the owner feed and the
-- receipts of what a linked desktop asked for), six on the linked desktop
-- (its read-only copy). No existing table is rebuilt or rewritten, no column
-- is added to one, and there is no trigger.
--
-- The tables are sections 2.7 and 2.8 of the M8 contract (revision 4), which
-- numbers this step 29; the step it expected before this one was not built.
-- Three differences from the contract's text, each marked below: the feed's
-- identity row also holds three counters, a marker's `removed` also covers a
-- conversation that is not shown, and one index is left out.
--
-- No column holds a message, a title, a task or a credential on the owner
-- side: every value there is an identity, a counter, a flag or a time. The
-- copy on the linked desktop holds what the owner sent, as the JSON it sent.

-- ------------------------------------------------------------ owner side

-- One identity for this database's feed.
--
-- Not in the contract's text: the three counters. A page that brings a linked
-- desktop up to date tells it how many Bots, conversations and messages it
-- should now hold. Counting the markers would read every one of them for
-- every such page, on the thread that writes chats; the counters are moved by
-- the marker write that changes them instead.
CREATE TABLE bot_client_feed_meta (
  singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
  feed_id TEXT NOT NULL CHECK(length(feed_id) = 32),
  live_bots INTEGER NOT NULL DEFAULT 0 CHECK(live_bots >= 0),
  live_chats INTEGER NOT NULL DEFAULT 0 CHECK(live_chats >= 0),
  live_messages INTEGER NOT NULL DEFAULT 0 CHECK(live_messages >= 0)
);
INSERT INTO bot_client_feed_meta(singleton, feed_id) VALUES(1, lower(hex(randomblob(16))));

-- One row per start of the AI worker. start_seq is the feed's head at that
-- start. It lets the owner tell a cursor of its own history from a cursor of
-- a history it lost to a restore.
CREATE TABLE bot_client_feed_boots (
  ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
  boot_id TEXT NOT NULL UNIQUE CHECK(length(boot_id) = 32),
  start_seq INTEGER NOT NULL CHECK(start_seq >= 0)
);

-- One marker per subject. Replaced, in the transaction of the change, by the
-- repository function that makes the change. seq is never reused.
CREATE TABLE bot_client_feed (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL CHECK(kind IN ('bot','chat','message','team_pause')),
  subject_id TEXT NOT NULL,
  bot_id TEXT,
  chat_id TEXT,
  -- The subject is gone. Also, not in the contract's text: a conversation
  -- that is private, or that one Bot opened in another and its owner has not
  -- admitted, and the messages of such a conversation. The computer that runs
  -- the Bots does not list those either.
  removed INTEGER NOT NULL DEFAULT 0 CHECK(removed IN (0,1)),
  -- The Bot was withheld when its markers were last written: its workspace
  -- is kept on this computer only, or it is not a Bot of this computer.
  -- Written on a Bot's marker and copied to its conversations' and messages'.
  withheld INTEGER NOT NULL DEFAULT 0 CHECK(withheld IN (0,1)),
  UNIQUE(kind, subject_id)
);
-- Every marker of one Bot, to rewrite them when the Bot becomes withheld or
-- stops being so. The contract's second index, on chat_id, is left out:
-- nothing reads a conversation's markers, and each index is two more page
-- writes in every marker write.
CREATE INDEX bot_client_feed_bot ON bot_client_feed(bot_id) WHERE bot_id IS NOT NULL;

-- Existing rows get a marker, so the first linked desktop can read them.
INSERT INTO bot_client_feed(kind, subject_id, bot_id)
  SELECT 'bot', bot_id, bot_id FROM bots ORDER BY created_at_ms, bot_id;
INSERT INTO bot_client_feed(kind, subject_id, bot_id, chat_id, removed)
  SELECT 'chat', bc.chat_id, bc.bot_id, bc.chat_id,
         (c.private = 1 OR EXISTS(
            SELECT 1 FROM bot_child_chats cc
              JOIN bot_command_receipts cr
                ON cr.user_scope_id = cc.user_scope_id AND cr.request_id = cc.child_request_id
             WHERE cc.chat_id = bc.chat_id AND cr.stage NOT IN ('accepted','completed')))
    FROM bot_chats bc JOIN chats c ON c.id = bc.chat_id
   ORDER BY bc.created_at_ms, bc.chat_id;
INSERT INTO bot_client_feed(kind, subject_id, bot_id, chat_id, removed)
  SELECT 'message', m.id, f.bot_id, m.chat_id, f.removed
    FROM messages m JOIN bot_client_feed f ON f.kind = 'chat' AND f.subject_id = m.chat_id
   ORDER BY m.created_at_ms, m.id;
INSERT INTO bot_client_feed(kind, subject_id)
  SELECT 'team_pause', owner_device_id FROM bot_team_pause GROUP BY owner_device_id;
UPDATE bot_client_feed_meta SET
  live_bots = (SELECT count(*) FROM bot_client_feed WHERE kind = 'bot' AND removed = 0),
  live_chats = (SELECT count(*) FROM bot_client_feed WHERE kind = 'chat' AND removed = 0),
  live_messages = (SELECT count(*) FROM bot_client_feed WHERE kind = 'message' AND removed = 0);

-- Every command a linked device sent, recorded before it is applied.
CREATE TABLE bot_client_receipts (
  peer_device_id TEXT NOT NULL,
  request_id TEXT NOT NULL CHECK(length(request_id) = 36),
  fingerprint TEXT NOT NULL CHECK(length(fingerprint) = 64),
  command_kind TEXT NOT NULL,
  -- The worker start that wrote the row (bot_client_feed_boots.boot_id). A row
  -- in 'applying' from the current start is in flight; from an earlier start
  -- it was left by a worker that stopped.
  boot_id TEXT NOT NULL CHECK(length(boot_id) = 32),
  state TEXT NOT NULL CHECK(state IN ('applying','done')),
  outcome TEXT CHECK(outcome IS NULL OR outcome IN ('completed','accepted','refused','unknown')),
  failure_code TEXT,
  feed_cursor INTEGER,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY(peer_device_id, request_id),
  CHECK((state = 'done') = (outcome IS NOT NULL))
);
CREATE INDEX bot_client_receipts_age ON bot_client_receipts(created_at_ms);

-- ----------------------------------------------------------- client side

-- All keyed by owner_device_id. None has a foreign key to a canonical table:
-- nothing that runs a Bot reads a copy.

CREATE TABLE bot_mirror_owners (
  owner_device_id TEXT PRIMARY KEY,
  feed_id TEXT,
  -- The owner start whose pages produced `cursor`.
  boot_id TEXT,
  protocol_version INTEGER,
  replica_format INTEGER,
  generation INTEGER NOT NULL DEFAULT 0 CHECK(generation >= 0),
  cursor INTEGER NOT NULL DEFAULT 0 CHECK(cursor >= 0),
  staging_generation INTEGER,
  staging_cursor INTEGER,
  withheld_bots INTEGER NOT NULL DEFAULT 0 CHECK(withheld_bots >= 0),
  state TEXT NOT NULL CHECK(state IN ('never','current','stale','unsupported','withdrawing')),
  last_confirmed_at_ms INTEGER,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  CHECK((staging_generation IS NULL) = (staging_cursor IS NULL)),
  CHECK(staging_generation IS NULL OR staging_generation > generation)
);

CREATE TABLE bot_mirror_bots (
  owner_device_id TEXT NOT NULL REFERENCES bot_mirror_owners(owner_device_id) ON DELETE CASCADE,
  generation INTEGER NOT NULL,
  bot_id TEXT NOT NULL,
  seq INTEGER NOT NULL,
  home_workspace_id TEXT NOT NULL,
  lifecycle TEXT NOT NULL CHECK(lifecycle IN ('active','archived','trashed')),
  revision INTEGER NOT NULL CHECK(revision >= 1),
  record_json TEXT NOT NULL CHECK(json_valid(record_json) AND length(CAST(record_json AS BLOB)) <= 1048576),
  PRIMARY KEY(owner_device_id, generation, bot_id)
);
CREATE INDEX bot_mirror_bots_id ON bot_mirror_bots(bot_id);

CREATE TABLE bot_mirror_chats (
  owner_device_id TEXT NOT NULL REFERENCES bot_mirror_owners(owner_device_id) ON DELETE CASCADE,
  generation INTEGER NOT NULL,
  chat_id TEXT NOT NULL,
  bot_id TEXT NOT NULL,
  seq INTEGER NOT NULL,
  purpose TEXT NOT NULL CHECK(purpose IN ('main','additional')),
  trashed INTEGER NOT NULL CHECK(trashed IN (0,1)),
  updated_at_ms INTEGER NOT NULL,
  record_json TEXT NOT NULL CHECK(json_valid(record_json) AND length(CAST(record_json AS BLOB)) <= 1048576),
  PRIMARY KEY(owner_device_id, generation, chat_id)
);
CREATE INDEX bot_mirror_chats_bot ON bot_mirror_chats(owner_device_id, generation, bot_id, updated_at_ms);

CREATE TABLE bot_mirror_messages (
  owner_device_id TEXT NOT NULL REFERENCES bot_mirror_owners(owner_device_id) ON DELETE CASCADE,
  generation INTEGER NOT NULL,
  message_id TEXT NOT NULL,
  chat_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
  seq INTEGER NOT NULL,
  record_json TEXT NOT NULL CHECK(json_valid(record_json) AND length(CAST(record_json AS BLOB)) <= 1048576),
  PRIMARY KEY(owner_device_id, generation, message_id)
);
CREATE INDEX bot_mirror_messages_chat ON bot_mirror_messages(owner_device_id, generation, chat_id, ordinal);

CREATE TABLE bot_mirror_team (
  owner_device_id TEXT NOT NULL REFERENCES bot_mirror_owners(owner_device_id) ON DELETE CASCADE,
  generation INTEGER NOT NULL,
  seq INTEGER NOT NULL,
  record_json TEXT NOT NULL CHECK(json_valid(record_json) AND length(CAST(record_json AS BLOB)) <= 65536),
  PRIMARY KEY(owner_device_id, generation)
);

-- Commands this device sent. Not a queue: a row exists only once the command
-- is being written to an open stream.
CREATE TABLE bot_mirror_commands (
  owner_device_id TEXT NOT NULL,
  request_id TEXT NOT NULL CHECK(length(request_id) = 36),
  fingerprint TEXT NOT NULL CHECK(length(fingerprint) = 64),
  command_kind TEXT NOT NULL,
  bot_id TEXT,
  state TEXT NOT NULL CHECK(state IN ('sent','receipted','unconfirmed','expired')),
  -- The exact command JSON, to send again unchanged. Cleared with the receipt.
  command_json TEXT CHECK(command_json IS NULL OR (json_valid(command_json) AND length(CAST(command_json AS BLOB)) <= 1048576)),
  outcome TEXT CHECK(outcome IS NULL OR outcome IN ('completed','accepted','refused','unknown')),
  failure_code TEXT,
  feed_cursor INTEGER,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY(owner_device_id, request_id),
  CHECK((state = 'receipted') = (outcome IS NOT NULL))
);
CREATE INDEX bot_mirror_commands_open ON bot_mirror_commands(owner_device_id, state)
  WHERE state IN ('sent','unconfirmed');

-- The step is refused whole, and the version does not move, unless every
-- existing Bot, Bot conversation and message of one has exactly one marker
-- and no foreign key is violated.
CREATE TEMP TABLE bot_ai28_guard(ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO bot_ai28_guard
 SELECT (SELECT count(*) FROM bot_client_feed WHERE kind = 'bot') = (SELECT count(*) FROM bots)
    AND (SELECT count(*) FROM bot_client_feed WHERE kind = 'chat') = (SELECT count(*) FROM bot_chats)
    AND (SELECT count(*) FROM bot_client_feed WHERE kind = 'message')
        = (SELECT count(*) FROM messages m JOIN bot_chats bc ON bc.chat_id = m.chat_id)
    AND NOT EXISTS(SELECT 1 FROM pragma_foreign_key_check);
DROP TABLE bot_ai28_guard;
