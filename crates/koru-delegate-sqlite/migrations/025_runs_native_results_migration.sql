-- runs.db schema 5 -> 6: native effect reconciliation evidence, not canonical Run status.
CREATE TABLE bot_policy_barriers (
  user_scope TEXT NOT NULL,
  device_id TEXT NOT NULL,
  bot_id TEXT NOT NULL,
  request_id TEXT NOT NULL,
  config_revision INTEGER NOT NULL CHECK(config_revision > 0),
  policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
  payload_digest BLOB NOT NULL CHECK(length(payload_digest) = 32),
  effects_allowed INTEGER NOT NULL CHECK(effects_allowed IN (0,1)),
  state TEXT NOT NULL CHECK(state IN ('pending','effective')),
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY(user_scope, device_id, bot_id, request_id),
  UNIQUE(user_scope, device_id, bot_id, config_revision)
);
CREATE INDEX bot_policy_barriers_current
  ON bot_policy_barriers(user_scope, device_id, bot_id, config_revision DESC);

CREATE TABLE native_result_intents (
  run_id TEXT NOT NULL REFERENCES bot_admissions(run_id) ON DELETE RESTRICT,
  operation_id TEXT NOT NULL,
  attempt_id TEXT NOT NULL REFERENCES run_attempts(attempt_id) ON DELETE RESTRICT,
  generation INTEGER NOT NULL CHECK(generation > 0 AND generation <= 4294967295),
  turn_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('create_page','edit_page')),
  request_fingerprint BLOB NOT NULL CHECK(length(request_fingerprint) = 32),
  destination_workspace_id TEXT NOT NULL,
  target_document_id TEXT NOT NULL,
  store_request_id TEXT NOT NULL UNIQUE,
  expected_revision BLOB,
  requested_title TEXT NOT NULL CHECK(length(CAST(requested_title AS BLOB)) BETWEEN 1 AND 256),
  stage TEXT NOT NULL CHECK(stage IN ('prepared','unconfirmed','verified','registered','refused')),
  lease_id TEXT UNIQUE,
  lease_user_scope TEXT,
  lease_device_id TEXT,
  lease_bot_id TEXT,
  lease_barrier_request_id TEXT,
  lease_config_revision INTEGER,
  lease_policy_revision INTEGER,
  lease_payload_digest BLOB,
  lease_state TEXT NOT NULL CHECK(lease_state IN ('none','held','settled')),
  verified_version BLOB,
  verified_title TEXT,
  confirmed_at_ms INTEGER,
  refusal TEXT CHECK(refusal IS NULL OR refusal IN ('access_denied','invalid_input','conflict','unavailable')),
  registered_at_ms INTEGER,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY(run_id, operation_id),
  FOREIGN KEY(run_id, generation) REFERENCES run_attempts(run_id, generation) ON DELETE RESTRICT,
  FOREIGN KEY(lease_user_scope, lease_device_id, lease_bot_id, lease_barrier_request_id)
    REFERENCES bot_policy_barriers(user_scope, device_id, bot_id, request_id) ON DELETE RESTRICT,
  CHECK((kind = 'create_page' AND expected_revision IS NULL) OR
        (kind = 'edit_page' AND expected_revision IS NOT NULL AND length(expected_revision) BETWEEN 1 AND 65536)),
  CHECK((lease_state = 'none' AND lease_id IS NULL AND lease_user_scope IS NULL AND
    lease_device_id IS NULL AND lease_bot_id IS NULL AND lease_barrier_request_id IS NULL AND
    lease_config_revision IS NULL AND lease_policy_revision IS NULL AND lease_payload_digest IS NULL)
    OR (lease_state IN ('held','settled') AND lease_id IS NOT NULL AND lease_user_scope IS NOT NULL AND
    lease_device_id IS NOT NULL AND lease_bot_id IS NOT NULL AND lease_barrier_request_id IS NOT NULL AND
    lease_config_revision IS NOT NULL AND lease_config_revision > 0 AND lease_policy_revision IS NOT NULL AND
    lease_policy_revision > 0 AND lease_payload_digest IS NOT NULL AND length(lease_payload_digest) = 32)),
  CHECK((stage = 'prepared' AND lease_state IN ('none','held')) OR
        (stage = 'unconfirmed' AND lease_state = 'held') OR
        (stage IN ('verified','registered') AND lease_state = 'settled') OR
        (stage = 'refused' AND lease_state IN ('none','settled'))),
  CHECK((stage IN ('verified','registered') AND verified_version IS NOT NULL AND
    length(verified_version) BETWEEN 1 AND 65536 AND verified_title IS NOT NULL AND
    length(CAST(verified_title AS BLOB)) BETWEEN 1 AND 256 AND confirmed_at_ms IS NOT NULL)
    OR (stage NOT IN ('verified','registered') AND verified_version IS NULL AND
    verified_title IS NULL AND confirmed_at_ms IS NULL)),
  CHECK((stage = 'refused' AND refusal IS NOT NULL) OR (stage <> 'refused' AND refusal IS NULL)),
  CHECK((stage = 'registered' AND registered_at_ms IS NOT NULL) OR
        (stage <> 'registered' AND registered_at_ms IS NULL))
);
CREATE UNIQUE INDEX native_result_created_identity ON native_result_intents(target_document_id)
  WHERE kind = 'create_page';
CREATE INDEX native_result_recovery ON native_result_intents(created_at_ms, run_id, operation_id)
  WHERE stage IN ('prepared','unconfirmed','verified');
CREATE INDEX native_result_held_leases ON native_result_intents(lease_user_scope, lease_device_id, lease_bot_id)
  WHERE lease_state = 'held';
CREATE INDEX native_result_created_provenance
  ON native_result_intents(destination_workspace_id, target_document_id, run_id)
  WHERE kind = 'create_page' AND stage IN ('verified','registered');

-- Pre-release runs6 amendment: stable publication identity for a confirmed
-- AI response. Legacy runs5 confirmations are repaired only by exact replay.
ALTER TABLE bot_admissions ADD COLUMN delivery_publication_event_id TEXT
  CHECK(delivery_publication_event_id IS NULL OR
    (delivery_outcome = 'confirmed'
     AND length(delivery_publication_event_id) = 36
     AND substr(delivery_publication_event_id, 9, 1) = '-'
     AND substr(delivery_publication_event_id, 14, 1) = '-'
     AND substr(delivery_publication_event_id, 19, 1) = '-'
     AND substr(delivery_publication_event_id, 24, 1) = '-'
     AND length(replace(delivery_publication_event_id, '-', '')) = 32
     AND delivery_publication_event_id NOT GLOB '*[^0-9a-f-]*'
     AND substr(delivery_publication_event_id, 15, 1) = '4'
     AND substr(delivery_publication_event_id, 20, 1) IN ('8','9','a','b')));
CREATE UNIQUE INDEX bot_delivery_publication_identity
  ON bot_admissions(delivery_publication_event_id)
  WHERE delivery_publication_event_id IS NOT NULL;
