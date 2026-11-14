-- Routine admission identity and retained routing/link/policy defenses.
-- Existing payload bytes remain unchanged and are explicitly version one.
ALTER TABLE bot_admissions ADD COLUMN submission_version INTEGER NOT NULL DEFAULT 1
    CHECK(submission_version IN(1,2));

CREATE TABLE bot_routine_routes (
  schedule_id TEXT PRIMARY KEY,
  user_scope TEXT NOT NULL,
  device_id TEXT NOT NULL,
  bot_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  creation_request_id TEXT NOT NULL,
  seed_hash BLOB NOT NULL CHECK(typeof(seed_hash)='blob' AND length(seed_hash)=32),
  created_at_ms INTEGER NOT NULL,
  UNIQUE(user_scope,device_id,bot_id,schedule_id)
);
CREATE TABLE bot_routine_links (
  run_id TEXT PRIMARY KEY REFERENCES bot_admissions(run_id) ON DELETE RESTRICT,
  schedule_id TEXT NOT NULL,
  occurrence_ms INTEGER NOT NULL,
  user_scope TEXT NOT NULL,
  device_id TEXT NOT NULL,
  bot_id TEXT NOT NULL,
  definition_revision INTEGER NOT NULL CHECK(definition_revision BETWEEN 1 AND 4294967295),
  definition_digest BLOB NOT NULL CHECK(typeof(definition_digest)='blob' AND length(definition_digest)=32),
  owner_handoff TEXT CHECK(owner_handoff IS NULL OR
    (typeof(owner_handoff)='text' AND length(owner_handoff)=36)),
  grant_revision INTEGER NOT NULL CHECK(grant_revision>0),
  preparation_hash BLOB NOT NULL CHECK(typeof(preparation_hash)='blob' AND length(preparation_hash)=32),
  submission_request_id TEXT NOT NULL,
  chat_id TEXT NOT NULL,
  attempt_id TEXT NOT NULL,
  user_message_id TEXT NOT NULL,
  assistant_message_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('held','linked','terminal')),
  terminal_reason TEXT CHECK(terminal_reason IS NULL OR terminal_reason IN
    ('stopped','configuration_changed','authority_changed','already_terminal')),
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(schedule_id,occurrence_ms),
  FOREIGN KEY(schedule_id,occurrence_ms)
    REFERENCES schedule_claims(schedule_id,occurrence_ms) ON DELETE RESTRICT,
  FOREIGN KEY(user_scope,device_id,bot_id,schedule_id)
    REFERENCES bot_routine_routes(user_scope,device_id,bot_id,schedule_id) ON DELETE RESTRICT,
  CHECK((state='terminal')=(terminal_reason IS NOT NULL))
);
CREATE INDEX bot_routine_link_recovery
  ON bot_routine_links(user_scope,device_id,updated_at_ms,run_id) WHERE state='held';
CREATE INDEX bot_routine_link_subject
  ON bot_routine_links(user_scope,device_id,bot_id,schedule_id,run_id);

CREATE TABLE bot_routine_policy_barriers (
  user_scope TEXT NOT NULL,
  device_id TEXT NOT NULL,
  bot_id TEXT NOT NULL,
  schedule_id TEXT NOT NULL,
  request_id TEXT NOT NULL,
  grant_revision INTEGER NOT NULL CHECK(grant_revision>0),
  payload_digest BLOB NOT NULL CHECK(typeof(payload_digest)='blob' AND length(payload_digest)=32),
  effects_allowed INTEGER NOT NULL CHECK(effects_allowed IN(0,1)),
  state TEXT NOT NULL CHECK(state IN('pending','effective')),
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY(user_scope,device_id,bot_id,schedule_id,request_id),
  UNIQUE(user_scope,device_id,bot_id,schedule_id,grant_revision),
  FOREIGN KEY(user_scope,device_id,bot_id,schedule_id)
    REFERENCES bot_routine_routes(user_scope,device_id,bot_id,schedule_id) ON DELETE RESTRICT
);
CREATE INDEX bot_routine_policy_current ON bot_routine_policy_barriers
  (user_scope,device_id,bot_id,schedule_id,grant_revision DESC);

CREATE TABLE native_routine_leases (
  run_id TEXT NOT NULL,
  operation_id TEXT NOT NULL,
  user_scope TEXT NOT NULL,
  device_id TEXT NOT NULL,
  bot_id TEXT NOT NULL,
  schedule_id TEXT NOT NULL,
  barrier_request_id TEXT NOT NULL,
  grant_revision INTEGER NOT NULL CHECK(grant_revision>0),
  payload_digest BLOB NOT NULL CHECK(typeof(payload_digest)='blob' AND length(payload_digest)=32),
  PRIMARY KEY(run_id,operation_id),
  FOREIGN KEY(run_id,operation_id)
    REFERENCES native_result_intents(run_id,operation_id) ON DELETE RESTRICT,
  FOREIGN KEY(run_id) REFERENCES bot_routine_links(run_id) ON DELETE RESTRICT,
  FOREIGN KEY(user_scope,device_id,bot_id,schedule_id,barrier_request_id)
    REFERENCES bot_routine_policy_barriers
      (user_scope,device_id,bot_id,schedule_id,request_id) ON DELETE RESTRICT
);
CREATE INDEX native_routine_lease_subject ON native_routine_leases
  (user_scope,device_id,bot_id,schedule_id,run_id,operation_id);

CREATE TRIGGER bot_routine_dispatch_gate
BEFORE UPDATE OF state ON bot_admissions
WHEN NEW.submission_version=2 AND NEW.state IN ('reserved','executing')
 AND NOT EXISTS(SELECT 1 FROM bot_routine_links l WHERE l.run_id=NEW.run_id
   AND l.state='linked' AND l.user_scope=NEW.user_scope
   AND l.device_id=NEW.device_id AND l.bot_id=NEW.bot_id
   AND l.submission_request_id=NEW.request_id)
BEGIN SELECT RAISE(ABORT,'routine_link_required'); END;

CREATE TRIGGER bot_routine_initial_dispatch_gate
BEFORE INSERT ON bot_admissions
WHEN NEW.submission_version=2 AND NEW.state IN ('reserved','executing')
BEGIN SELECT RAISE(ABORT,'routine_must_be_claimed_queued'); END;

CREATE TRIGGER native_routine_lease_identity
BEFORE INSERT ON native_routine_leases
WHEN NOT EXISTS(
  SELECT 1 FROM bot_admissions a
  JOIN bot_routine_links l ON l.run_id=a.run_id
  JOIN bot_routine_policy_barriers p
    ON p.user_scope=NEW.user_scope AND p.device_id=NEW.device_id
   AND p.bot_id=NEW.bot_id AND p.schedule_id=NEW.schedule_id
   AND p.request_id=NEW.barrier_request_id
  JOIN native_result_intents n ON n.run_id=a.run_id
   AND n.operation_id=NEW.operation_id
  WHERE a.run_id=NEW.run_id AND a.submission_version=2
    AND l.state='linked' AND l.user_scope=NEW.user_scope
    AND l.device_id=NEW.device_id AND l.bot_id=NEW.bot_id
    AND l.schedule_id=NEW.schedule_id
    AND p.grant_revision=NEW.grant_revision AND p.payload_digest=NEW.payload_digest
    AND p.state='effective' AND p.effects_allowed=1
    AND n.stage='prepared' AND n.lease_state='none')
BEGIN SELECT RAISE(ABORT,'routine_lease_identity'); END;

CREATE TRIGGER native_routine_lease_immutable
BEFORE UPDATE ON native_routine_leases
BEGIN SELECT RAISE(ABORT,'routine_lease_immutable'); END;

CREATE TRIGGER native_routine_lease_retained
BEFORE DELETE ON native_routine_leases
WHEN EXISTS(SELECT 1 FROM native_result_intents n
  WHERE n.run_id=OLD.run_id AND n.operation_id=OLD.operation_id
    AND n.lease_state='held')
BEGIN SELECT RAISE(ABORT,'routine_lease_held'); END;

CREATE TRIGGER native_routine_acquire_gate
BEFORE UPDATE OF lease_state ON native_result_intents
WHEN NEW.lease_state='held'
 AND EXISTS(SELECT 1 FROM bot_admissions a
   WHERE a.run_id=NEW.run_id AND a.submission_version=2)
 AND NOT EXISTS(SELECT 1 FROM native_routine_leases r
   WHERE r.run_id=NEW.run_id AND r.operation_id=NEW.operation_id
     AND r.user_scope=NEW.lease_user_scope AND r.device_id=NEW.lease_device_id
     AND r.bot_id=NEW.lease_bot_id)
BEGIN SELECT RAISE(ABORT,'routine_lease_required'); END;

CREATE TRIGGER native_routine_initial_lease_gate
BEFORE INSERT ON native_result_intents
WHEN NEW.lease_state='held'
 AND EXISTS(SELECT 1 FROM bot_admissions a
   WHERE a.run_id=NEW.run_id AND a.submission_version=2)
BEGIN SELECT RAISE(ABORT,'routine_lease_requires_prepared_intent'); END;
