-- ai.db 25 (M5): a person's private connections, the items they picked, a
-- Bot's grants, and the idempotent receipts of the connection commands.
-- Four new tables; no existing table is rebuilt or rewritten. The text below
-- is section 2.1 of the M5 contract (revision 3), unchanged.
--
-- No column holds a secret. The credential is in the OS credential store,
-- under a slot named from the connection's id; `credential_slot_digest` is a
-- digest of that slot's name, never of what it holds. `account_label`,
-- `name`, `title` and `item_ref` are private metadata kept in plain text on
-- this computer only.
-- One signed-in account at one vendor. No secret column. Never deleted.
CREATE TABLE connections (
 connection_id TEXT NOT NULL PRIMARY KEY CHECK(length(connection_id)=36),
 user_scope_id TEXT NOT NULL CHECK(length(user_scope_id)=36),
 owner_device_id TEXT NOT NULL CHECK(length(owner_device_id)=36),
 provider TEXT NOT NULL CHECK(provider IN ('google_drive')),
 service_origin TEXT NOT NULL CHECK(typeof(service_origin)='text'
   AND length(CAST(service_origin AS BLOB)) BETWEEN 9 AND 256),
 name TEXT NOT NULL CHECK(typeof(name)='text'
   AND length(CAST(name AS BLOB)) BETWEEN 1 AND 256),
 account_subject_digest BLOB NOT NULL CHECK(typeof(account_subject_digest)='blob'
   AND length(account_subject_digest)=32),
 account_label TEXT NOT NULL CHECK(typeof(account_label)='text'
   AND length(CAST(account_label AS BLOB)) BETWEEN 1 AND 320),
 scopes_json TEXT NOT NULL CHECK(typeof(scopes_json)='text' AND json_valid(scopes_json)
   AND length(CAST(scopes_json AS BLOB)) BETWEEN 2 AND 4096),
 credential_slot_digest BLOB NOT NULL CHECK(typeof(credential_slot_digest)='blob'
   AND length(credential_slot_digest)=32),
 state TEXT NOT NULL CHECK(state IN ('connected','needs_sign_in','disconnecting','disconnected')),
 vendor_revoke TEXT CHECK(vendor_revoke IS NULL
   OR vendor_revoke IN ('confirmed','unconfirmed','unsupported')),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=1),
 connected_at_ms INTEGER NOT NULL CHECK(typeof(connected_at_ms)='integer'),
 refreshed_at_ms INTEGER CHECK(refreshed_at_ms IS NULL OR typeof(refreshed_at_ms)='integer'),
 disconnected_at_ms INTEGER CHECK(disconnected_at_ms IS NULL OR typeof(disconnected_at_ms)='integer'),
 created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer'),
 updated_at_ms INTEGER NOT NULL CHECK(typeof(updated_at_ms)='integer'),
 CHECK((state='disconnected' AND disconnected_at_ms IS NOT NULL AND vendor_revoke IS NOT NULL)
    OR (state<>'disconnected' AND disconnected_at_ms IS NULL AND vendor_revoke IS NULL))
);
CREATE UNIQUE INDEX connections_live_account
 ON connections(user_scope_id,owner_device_id,provider,account_subject_digest)
 WHERE state<>'disconnected';
CREATE UNIQUE INDEX connections_live_name
 ON connections(user_scope_id,owner_device_id,name) WHERE state<>'disconnected';
CREATE INDEX connections_owner
 ON connections(user_scope_id,owner_device_id,created_at_ms,connection_id);

-- The items the person picked for this connection. item_ref is the vendor's
-- own identifier: private metadata, local only, never a secret.
CREATE TABLE connection_items (
 item_id TEXT NOT NULL PRIMARY KEY CHECK(length(item_id)=36),
 connection_id TEXT NOT NULL REFERENCES connections(connection_id) ON DELETE RESTRICT,
 item_ref TEXT NOT NULL CHECK(typeof(item_ref)='text'
   AND length(CAST(item_ref AS BLOB)) BETWEEN 1 AND 1024),
 item_ref_digest BLOB NOT NULL CHECK(typeof(item_ref_digest)='blob' AND length(item_ref_digest)=32),
 identity_digest BLOB NOT NULL CHECK(typeof(identity_digest)='blob' AND length(identity_digest)=32),
 title TEXT NOT NULL CHECK(typeof(title)='text' AND length(CAST(title AS BLOB)) BETWEEN 1 AND 512),
 state TEXT NOT NULL CHECK(state IN ('available','changed')),
 added_at_ms INTEGER NOT NULL CHECK(typeof(added_at_ms)='integer'),
 changed_at_ms INTEGER CHECK(changed_at_ms IS NULL OR typeof(changed_at_ms)='integer'),
 updated_at_ms INTEGER NOT NULL CHECK(typeof(updated_at_ms)='integer'),
 UNIQUE(connection_id,item_ref_digest),
 CHECK((state='changed' AND changed_at_ms IS NOT NULL) OR (state='available' AND changed_at_ms IS NULL))
);
CREATE INDEX connection_items_live
 ON connection_items(connection_id,added_at_ms,item_id) WHERE state='available';

-- A Bot's current grants. A row is the grant; removing access deletes it.
-- A Routine's grants live inside its grant JSON (2.4), not here.
CREATE TABLE bot_connection_grants (
 bot_id TEXT NOT NULL REFERENCES bots(bot_id) ON DELETE RESTRICT,
 connection_id TEXT NOT NULL REFERENCES connections(connection_id) ON DELETE RESTRICT,
 operation TEXT NOT NULL CHECK(operation IN ('read_item')),
 granted_at_ms INTEGER NOT NULL CHECK(typeof(granted_at_ms)='integer'),
 PRIMARY KEY(bot_id,connection_id,operation)
);
CREATE INDEX bot_connection_grants_connection ON bot_connection_grants(connection_id,bot_id);

-- Idempotent connection commands, the pattern of bot_command_receipts.
CREATE TABLE connection_command_receipts (
 user_scope_id TEXT NOT NULL CHECK(length(user_scope_id)=36),
 request_id TEXT NOT NULL CHECK(length(request_id)=36),
 owner_device_id TEXT NOT NULL CHECK(length(owner_device_id)=36),
 command_kind TEXT NOT NULL CHECK(command_kind IN ('connect','add_items','disconnect')),
 fingerprint TEXT NOT NULL CHECK(length(fingerprint)=64),
 -- The id a connect will use if it completes. Deliberately no foreign key:
 -- it names the keychain slot before any connection row exists.
 planned_connection_id TEXT CHECK(planned_connection_id IS NULL OR length(planned_connection_id)=36),
 connection_id TEXT REFERENCES connections(connection_id) ON DELETE RESTRICT,
 stage TEXT NOT NULL CHECK(stage IN ('prepared','completed','failed')),
 failure TEXT CHECK(failure IS NULL OR failure IN
   ('canceled','timed_out','interrupted','consent_denied','wrong_account','account_in_use',
    'name_in_use','vendor_rejected','vendor_unavailable','credential_store_unavailable',
    'not_configured','revision_conflict','item_limit')),
 expected_revision INTEGER CHECK(expected_revision IS NULL
   OR (typeof(expected_revision)='integer' AND expected_revision>=1)),
 result_revision INTEGER CHECK(result_revision IS NULL
   OR (typeof(result_revision)='integer' AND result_revision>=1)),
 created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer'),
 updated_at_ms INTEGER NOT NULL CHECK(typeof(updated_at_ms)='integer'),
 PRIMARY KEY(user_scope_id,request_id),
 CHECK((stage='failed' AND failure IS NOT NULL) OR (stage<>'failed' AND failure IS NULL)),
 CHECK((command_kind='connect' AND planned_connection_id IS NOT NULL)
    OR (command_kind<>'connect' AND planned_connection_id IS NULL)),
 CHECK(command_kind='connect' OR connection_id IS NOT NULL),
 CHECK(command_kind='connect' OR expected_revision IS NOT NULL),
 CHECK(stage<>'completed' OR (connection_id IS NOT NULL AND result_revision IS NOT NULL))
);
CREATE UNIQUE INDEX connection_command_open
 ON connection_command_receipts(connection_id)
 WHERE stage='prepared' AND connection_id IS NOT NULL;
CREATE UNIQUE INDEX connection_connect_open
 ON connection_command_receipts(user_scope_id,owner_device_id)
 WHERE stage='prepared' AND command_kind='connect';

CREATE TRIGGER connections_identity BEFORE UPDATE ON connections
WHEN NEW.connection_id IS NOT OLD.connection_id OR NEW.user_scope_id IS NOT OLD.user_scope_id
 OR NEW.owner_device_id IS NOT OLD.owner_device_id OR NEW.provider IS NOT OLD.provider
 OR NEW.service_origin IS NOT OLD.service_origin OR NEW.name IS NOT OLD.name
 OR NEW.account_subject_digest IS NOT OLD.account_subject_digest
 OR NEW.credential_slot_digest IS NOT OLD.credential_slot_digest
 OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR NEW.revision<OLD.revision
 OR (OLD.state='disconnected' AND (NEW.state IS NOT OLD.state OR NEW.revision IS NOT OLD.revision
     OR NEW.account_label IS NOT OLD.account_label OR NEW.scopes_json IS NOT OLD.scopes_json
     OR NEW.vendor_revoke IS NOT OLD.vendor_revoke OR NEW.disconnected_at_ms IS NOT OLD.disconnected_at_ms))
 OR (OLD.state='disconnecting' AND NEW.state NOT IN ('disconnecting','disconnected'))
BEGIN SELECT RAISE(ABORT,'immutable connection identity'); END;
CREATE TRIGGER connections_retain BEFORE DELETE ON connections
BEGIN SELECT RAISE(ABORT,'retained connection'); END;
CREATE TRIGGER connection_items_identity BEFORE UPDATE ON connection_items
WHEN NEW.item_id IS NOT OLD.item_id OR NEW.connection_id IS NOT OLD.connection_id
 OR NEW.item_ref IS NOT OLD.item_ref OR NEW.item_ref_digest IS NOT OLD.item_ref_digest
 OR NEW.added_at_ms IS NOT OLD.added_at_ms
BEGIN SELECT RAISE(ABORT,'immutable connection item identity'); END;
CREATE TRIGGER connection_items_retain BEFORE DELETE ON connection_items
BEGIN SELECT RAISE(ABORT,'retained connection item'); END;
CREATE TRIGGER connection_command_receipts_identity BEFORE UPDATE ON connection_command_receipts
WHEN NEW.user_scope_id IS NOT OLD.user_scope_id OR NEW.request_id IS NOT OLD.request_id
 OR NEW.owner_device_id IS NOT OLD.owner_device_id OR NEW.command_kind IS NOT OLD.command_kind
 OR NEW.fingerprint IS NOT OLD.fingerprint OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR NEW.expected_revision IS NOT OLD.expected_revision
 OR NEW.planned_connection_id IS NOT OLD.planned_connection_id
 OR (OLD.connection_id IS NOT NULL AND NEW.connection_id IS NOT OLD.connection_id)
 OR (OLD.stage<>'prepared' AND (NEW.stage IS NOT OLD.stage OR NEW.failure IS NOT OLD.failure
     OR NEW.result_revision IS NOT OLD.result_revision))
BEGIN SELECT RAISE(ABORT,'immutable connection receipt'); END;
CREATE TRIGGER connection_command_receipts_retain BEFORE DELETE ON connection_command_receipts
BEGIN SELECT RAISE(ABORT,'retained connection receipt'); END;

CREATE TEMP TABLE m5_fk_guard (violations INTEGER NOT NULL CHECK(violations=0));
INSERT INTO m5_fk_guard SELECT count(*) FROM pragma_foreign_key_check;
DROP TABLE m5_fk_guard;
