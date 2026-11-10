-- runs.db 6 -> 7: settled liveness with explicitly unknown native outcomes.
CREATE TABLE native_result_intents_next (
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
  stage TEXT NOT NULL CHECK(stage IN ('prepared','unconfirmed','verified','registered','refused','settled_unknown')),
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
  effect_fenced_at_ms INTEGER,
  effect_drained_at_ms INTEGER,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  CHECK((effect_fenced_at_ms IS NULL AND effect_drained_at_ms IS NULL AND stage <> 'settled_unknown') OR
        (effect_fenced_at_ms IS NOT NULL AND effect_drained_at_ms IS NOT NULL AND lease_state = 'settled' AND stage IN ('settled_unknown','verified','registered'))),
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
        (stage = 'settled_unknown' AND lease_state = 'settled') OR
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
INSERT INTO native_result_intents_next(run_id,operation_id,attempt_id,generation,turn_id,kind,request_fingerprint,destination_workspace_id,target_document_id,store_request_id,expected_revision,requested_title,stage,lease_id,lease_user_scope,lease_device_id,lease_bot_id,lease_barrier_request_id,lease_config_revision,lease_policy_revision,lease_payload_digest,lease_state,verified_version,verified_title,confirmed_at_ms,refusal,registered_at_ms,created_at_ms,updated_at_ms) SELECT run_id,operation_id,attempt_id,generation,turn_id,kind,request_fingerprint,destination_workspace_id,target_document_id,store_request_id,expected_revision,requested_title,stage,lease_id,lease_user_scope,lease_device_id,lease_bot_id,lease_barrier_request_id,lease_config_revision,lease_policy_revision,lease_payload_digest,lease_state,verified_version,verified_title,confirmed_at_ms,refusal,registered_at_ms,created_at_ms,updated_at_ms FROM native_result_intents;
DROP TABLE native_result_intents;
ALTER TABLE native_result_intents_next RENAME TO native_result_intents;
CREATE UNIQUE INDEX native_result_created_identity ON native_result_intents(target_document_id)
  WHERE kind = 'create_page';
CREATE INDEX native_result_recovery ON native_result_intents(created_at_ms, run_id, operation_id)
  WHERE stage IN ('prepared','unconfirmed','verified');
CREATE INDEX native_result_created_provenance
  ON native_result_intents(destination_workspace_id, target_document_id, run_id)
  WHERE kind = 'create_page' AND stage IN ('verified','registered');

CREATE INDEX native_result_held_owner_recovery ON native_result_intents(lease_user_scope,lease_device_id,created_at_ms,run_id,operation_id) WHERE lease_state='held';
CREATE INDEX native_result_held_bot_recovery ON native_result_intents(lease_user_scope,lease_device_id,lease_bot_id,created_at_ms,run_id,operation_id) WHERE lease_state='held';
