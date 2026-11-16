-- runs.db 11 (M5): private operations. Four parts, in order.
--
-- A. Prove bot_decisions is empty, or stop. A row fails the CHECK, the batch
--    errors, the migration transaction rolls back and the version stays 10.
--    This step never drops a row. The opener's typed preflight
--    (`db::refuse_retained_decisions`) reports the same condition first, in
--    words; this guard is the last line of defense.
CREATE TEMP TABLE m5_decisions_empty (found INTEGER NOT NULL CHECK(found=0));
INSERT INTO m5_decisions_empty SELECT count(*) FROM bot_decisions;
DROP TABLE m5_decisions_empty;

-- B. Recreate the empty table with a wider kind set. This is migration 029
--    lines 1 to 62 with exactly four changes, each marked. `private_operation`
--    is M5's kind. `browser_origin` is admitted here for M7 (O158), so that
--    milestone needs no runs.db step; no Rust variant exists for it yet and
--    the row decoder refuses it. DROP TABLE removes the table's seven indexes
--    and its two triggers; all are recreated below, the triggers with the
--    exact text of 029.
DROP TABLE bot_decisions;
CREATE TABLE bot_decisions (
  decision_id TEXT NOT NULL PRIMARY KEY CHECK(length(decision_id)=36),
  run_id TEXT NOT NULL REFERENCES bot_admissions(run_id) ON DELETE RESTRICT,
  user_scope TEXT NOT NULL CHECK(length(user_scope)=36),
  device_id TEXT NOT NULL CHECK(length(device_id)=36),
  bot_id TEXT NOT NULL CHECK(length(bot_id)=36),
  origin_attempt_id TEXT NOT NULL REFERENCES run_attempts(attempt_id) ON DELETE RESTRICT,
  origin_generation INTEGER NOT NULL CHECK(typeof(origin_generation)='integer' AND origin_generation BETWEEN 1 AND 4294967295),
  origin_session_id TEXT NOT NULL CHECK(length(origin_session_id)=36),
  origin_turn_id TEXT NOT NULL CHECK(length(origin_turn_id)=36),
  kind TEXT NOT NULL CHECK(kind IN ('koru_edit','native_approval','private_operation','browser_origin')),  -- M5, O158
  operation_id TEXT CHECK(operation_id IS NULL OR length(operation_id)=36),
  native_prompt_id TEXT CHECK(native_prompt_id IS NULL OR length(CAST(native_prompt_id AS BLOB)) BETWEEN 1 AND 4096),
  human_profile_id TEXT NOT NULL CHECK(length(human_profile_id)=36),
  binding_version INTEGER NOT NULL CHECK(binding_version=1),
  binding_digest BLOB NOT NULL CHECK(typeof(binding_digest)='blob' AND length(binding_digest)=32),
  binding_json TEXT NOT NULL CHECK(typeof(binding_json)='text' AND json_valid(binding_json) AND length(CAST(binding_json AS BLOB)) BETWEEN 1 AND 4194304),
  created_boot_id TEXT NOT NULL CHECK(length(created_boot_id)=36),
  created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer'),
  deadline_at_ms INTEGER NOT NULL CHECK(typeof(deadline_at_ms)='integer'),
  native_deadline_at_ms INTEGER CHECK(native_deadline_at_ms IS NULL OR typeof(native_deadline_at_ms)='integer'),
  last_observed_at_ms INTEGER NOT NULL CHECK(typeof(last_observed_at_ms)='integer'),
  revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0),
  authorization TEXT NOT NULL CHECK(authorization IN ('pending','granted','denied','canceled','expired','consumed')),
  phase TEXT NOT NULL CHECK(phase IN ('publishing','holding','held','native_wait','resume_prepared','await_retry','effect','native_dispatch','native_unknown','stop_pending','settled')),
  answer_request_id TEXT CHECK(answer_request_id IS NULL OR length(answer_request_id)=36),
  answer_profile_id TEXT CHECK(answer_profile_id IS NULL OR length(answer_profile_id)=36),
  answer_choice_json TEXT CHECK(answer_choice_json IS NULL OR (typeof(answer_choice_json)='text' AND json_valid(answer_choice_json) AND length(CAST(answer_choice_json AS BLOB)) BETWEEN 1 AND 16384)),
  answer_at_ms INTEGER CHECK(answer_at_ms IS NULL OR typeof(answer_at_ms)='integer'),
  answer_proof_digest BLOB CHECK(answer_proof_digest IS NULL OR (typeof(answer_proof_digest)='blob' AND length(answer_proof_digest)=32)),
  answer_receipt_json TEXT CHECK(answer_receipt_json IS NULL OR (typeof(answer_receipt_json)='text' AND json_valid(answer_receipt_json) AND length(CAST(answer_receipt_json AS BLOB)) BETWEEN 1 AND 65536)),
  consumed_at_ms INTEGER CHECK(consumed_at_ms IS NULL OR typeof(consumed_at_ms)='integer'),
  closure_reason TEXT CHECK(closure_reason IS NULL OR closure_reason IN ('completed','denied','canceled','expired','clock_uncertain','authority_changed','unsupported','effect_not_applied','effect_unknown','native_unknown','ai_terminal')),
  settled_at_ms INTEGER CHECK(settled_at_ms IS NULL OR typeof(settled_at_ms)='integer'),
  updated_at_ms INTEGER NOT NULL CHECK(typeof(updated_at_ms)='integer'),
  UNIQUE(run_id,decision_id),
  FOREIGN KEY(run_id,origin_generation) REFERENCES run_attempts(run_id,generation) ON DELETE RESTRICT,
  CHECK((kind IN ('koru_edit','private_operation','browser_origin') AND operation_id IS NOT NULL AND native_prompt_id IS NULL AND native_deadline_at_ms IS NULL)  -- M5, O158
     OR (kind='native_approval' AND operation_id IS NULL AND native_prompt_id IS NOT NULL)),
  CHECK(deadline_at_ms>created_at_ms AND deadline_at_ms<=created_at_ms+600000),
  CHECK(native_deadline_at_ms IS NULL OR deadline_at_ms<=native_deadline_at_ms),
  CHECK((answer_request_id IS NULL AND answer_profile_id IS NULL AND answer_choice_json IS NULL AND answer_at_ms IS NULL AND answer_proof_digest IS NULL AND answer_receipt_json IS NULL)
     OR (answer_request_id IS NOT NULL AND answer_profile_id IS NOT NULL AND answer_choice_json IS NOT NULL AND answer_at_ms IS NOT NULL AND answer_proof_digest IS NOT NULL AND answer_receipt_json IS NOT NULL)),
  CHECK((authorization='consumed' AND consumed_at_ms IS NOT NULL) OR (authorization<>'consumed' AND consumed_at_ms IS NULL)),
  CHECK(authorization NOT IN ('granted','consumed','denied') OR answer_request_id IS NOT NULL),
  CHECK((phase='settled' AND closure_reason IS NOT NULL AND settled_at_ms IS NOT NULL)
     OR (phase<>'settled' AND closure_reason IS NULL AND settled_at_ms IS NULL)),
  CHECK((authorization='pending' AND phase IN ('publishing','holding','held','native_wait'))
     OR (authorization='granted' AND phase IN ('held','resume_prepared','await_retry','native_wait','native_dispatch','native_unknown'))
     OR (authorization='consumed' AND phase IN ('effect','native_dispatch','native_unknown','stop_pending','settled'))
     OR (authorization='denied' AND phase IN ('native_dispatch','native_unknown','stop_pending','settled'))
     OR (authorization IN ('canceled','expired') AND phase IN ('stop_pending','settled'))),
  CHECK(kind='native_approval' OR phase NOT IN ('native_wait','native_dispatch','native_unknown')),
  CHECK(kind IN ('koru_edit','private_operation','browser_origin') OR phase NOT IN ('resume_prepared','await_retry','effect'))  -- M5, O158
);
CREATE UNIQUE INDEX bot_decisions_operation ON bot_decisions(run_id,operation_id) WHERE operation_id IS NOT NULL;  -- M5
CREATE UNIQUE INDEX bot_decisions_prompt ON bot_decisions(run_id,origin_session_id,origin_turn_id,native_prompt_id) WHERE kind='native_approval';
CREATE UNIQUE INDEX bot_decisions_outstanding ON bot_decisions(run_id) WHERE phase<>'settled';
CREATE UNIQUE INDEX bot_decisions_answer ON bot_decisions(user_scope,device_id,answer_request_id) WHERE answer_request_id IS NOT NULL;
CREATE INDEX bot_decisions_deadline ON bot_decisions(device_id,deadline_at_ms,decision_id) WHERE phase<>'settled';
CREATE INDEX bot_decisions_owner_open ON bot_decisions(user_scope,device_id,decision_id) WHERE phase<>'settled';
CREATE INDEX bot_decisions_history ON bot_decisions(user_scope,device_id,run_id,created_at_ms,decision_id);
CREATE TRIGGER bot_decisions_immutable BEFORE UPDATE ON bot_decisions
WHEN NEW.decision_id IS NOT OLD.decision_id OR NEW.run_id IS NOT OLD.run_id
 OR NEW.user_scope IS NOT OLD.user_scope OR NEW.device_id IS NOT OLD.device_id OR NEW.bot_id IS NOT OLD.bot_id
 OR NEW.origin_attempt_id IS NOT OLD.origin_attempt_id OR NEW.origin_generation IS NOT OLD.origin_generation
 OR NEW.origin_session_id IS NOT OLD.origin_session_id OR NEW.origin_turn_id IS NOT OLD.origin_turn_id
 OR NEW.kind IS NOT OLD.kind OR NEW.operation_id IS NOT OLD.operation_id OR NEW.native_prompt_id IS NOT OLD.native_prompt_id
 OR NEW.human_profile_id IS NOT OLD.human_profile_id OR NEW.binding_version IS NOT OLD.binding_version
 OR NEW.binding_digest IS NOT OLD.binding_digest OR NEW.binding_json IS NOT OLD.binding_json
 OR NEW.created_boot_id IS NOT OLD.created_boot_id OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR NEW.deadline_at_ms IS NOT OLD.deadline_at_ms OR NEW.native_deadline_at_ms IS NOT OLD.native_deadline_at_ms
 OR (OLD.answer_request_id IS NOT NULL AND
     (NEW.answer_request_id IS NOT OLD.answer_request_id OR NEW.answer_profile_id IS NOT OLD.answer_profile_id
      OR NEW.answer_choice_json IS NOT OLD.answer_choice_json OR NEW.answer_at_ms IS NOT OLD.answer_at_ms
      OR NEW.answer_proof_digest IS NOT OLD.answer_proof_digest OR NEW.answer_receipt_json IS NOT OLD.answer_receipt_json))
 OR (OLD.consumed_at_ms IS NOT NULL AND NEW.consumed_at_ms IS NOT OLD.consumed_at_ms)
 OR (OLD.phase='settled' AND (NEW.phase IS NOT OLD.phase OR NEW.authorization IS NOT OLD.authorization
      OR NEW.closure_reason IS NOT OLD.closure_reason OR NEW.settled_at_ms IS NOT OLD.settled_at_ms))
BEGIN SELECT RAISE(ABORT,'immutable Bot decision'); END;
CREATE TRIGGER bot_decisions_retain BEFORE DELETE ON bot_decisions BEGIN SELECT RAISE(ABORT,'retained Bot decision'); END;

-- C. The intent and the receipt of every private operation, with or without a
--    decision. Identifiers, digests, codes and sizes only: no account label,
--    title, vendor reference, link, secret or content has a column here.
CREATE TABLE bot_private_effects (
  run_id TEXT NOT NULL REFERENCES bot_admissions(run_id) ON DELETE RESTRICT,
  operation_id TEXT NOT NULL CHECK(length(operation_id)=36),
  user_scope TEXT NOT NULL CHECK(length(user_scope)=36),
  device_id TEXT NOT NULL CHECK(length(device_id)=36),
  bot_id TEXT NOT NULL CHECK(length(bot_id)=36),
  attempt_id TEXT NOT NULL REFERENCES run_attempts(attempt_id) ON DELETE RESTRICT,
  generation INTEGER NOT NULL CHECK(typeof(generation)='integer' AND generation BETWEEN 1 AND 4294967295),
  turn_id TEXT NOT NULL CHECK(length(turn_id)=36),
  dispatcher_instance TEXT NOT NULL CHECK(length(dispatcher_instance)=36),
  decision_id TEXT CHECK(decision_id IS NULL OR length(decision_id)=36),
  binding_digest BLOB CHECK(binding_digest IS NULL OR (typeof(binding_digest)='blob' AND length(binding_digest)=32)),
  connection_id TEXT NOT NULL CHECK(length(connection_id)=36),
  connection_revision INTEGER NOT NULL CHECK(typeof(connection_revision)='integer' AND connection_revision>0),
  account_digest BLOB NOT NULL CHECK(typeof(account_digest)='blob' AND length(account_digest)=32),
  item_id TEXT NOT NULL CHECK(length(item_id)=36),
  item_ref_digest BLOB NOT NULL CHECK(typeof(item_ref_digest)='blob' AND length(item_ref_digest)=32),
  operation TEXT NOT NULL CHECK(operation IN ('read_item')),
  request_fingerprint BLOB NOT NULL CHECK(typeof(request_fingerprint)='blob' AND length(request_fingerprint)=32),
  -- The Bot's barrier this effect is bound to. Always set.
  barrier_request_id TEXT NOT NULL CHECK(length(barrier_request_id)=36),
  config_revision INTEGER NOT NULL CHECK(typeof(config_revision)='integer' AND config_revision>0),
  policy_revision INTEGER NOT NULL CHECK(typeof(policy_revision)='integer' AND policy_revision>0),
  policy_digest BLOB NOT NULL CHECK(typeof(policy_digest)='blob' AND length(policy_digest)=32),
  -- The Routine's barrier, for a Routine Run only. All four set, or all four null.
  schedule_id TEXT CHECK(schedule_id IS NULL OR length(schedule_id)=36),
  routine_barrier_request_id TEXT CHECK(routine_barrier_request_id IS NULL OR length(routine_barrier_request_id)=36),
  grant_revision INTEGER CHECK(grant_revision IS NULL OR (typeof(grant_revision)='integer' AND grant_revision>0)),
  routine_policy_digest BLOB CHECK(routine_policy_digest IS NULL OR (typeof(routine_policy_digest)='blob' AND length(routine_policy_digest)=32)),
  stage TEXT NOT NULL CHECK(stage IN ('prepared','dispatched','confirmed','refused','unknown')),
  refusal TEXT CHECK(refusal IS NULL OR refusal IN
    ('not_dispatched','access_denied','revoked','wrong_account','credential_expired',
     'credential_locked','credential_missing','item_unavailable','destination_changed',
     'vendor_rejected','unavailable')),
  outcome_bytes INTEGER CHECK(outcome_bytes IS NULL OR (typeof(outcome_bytes)='integer' AND outcome_bytes BETWEEN 0 AND 4294967295)),
  withheld INTEGER CHECK(withheld IS NULL OR withheld IN (0,1)),
  dispatched_at_ms INTEGER CHECK(dispatched_at_ms IS NULL OR typeof(dispatched_at_ms)='integer'),
  resolved_at_ms INTEGER CHECK(resolved_at_ms IS NULL OR typeof(resolved_at_ms)='integer'),
  created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer'),
  updated_at_ms INTEGER NOT NULL CHECK(typeof(updated_at_ms)='integer'),
  PRIMARY KEY(run_id,operation_id),
  FOREIGN KEY(run_id,generation) REFERENCES run_attempts(run_id,generation) ON DELETE RESTRICT,
  FOREIGN KEY(run_id,decision_id) REFERENCES bot_decisions(run_id,decision_id) ON DELETE RESTRICT,
  -- A consume had an acknowledged resume, as native_decision_leases requires (029:137).
  FOREIGN KEY(decision_id) REFERENCES bot_decision_resumes(decision_id) ON DELETE RESTRICT,
  FOREIGN KEY(user_scope,device_id,bot_id,barrier_request_id)
    REFERENCES bot_policy_barriers(user_scope,device_id,bot_id,request_id) ON DELETE RESTRICT,
  FOREIGN KEY(user_scope,device_id,bot_id,schedule_id,routine_barrier_request_id)
    REFERENCES bot_routine_policy_barriers(user_scope,device_id,bot_id,schedule_id,request_id) ON DELETE RESTRICT,
  CHECK((decision_id IS NULL AND binding_digest IS NULL) OR (decision_id IS NOT NULL AND binding_digest IS NOT NULL)),
  CHECK((schedule_id IS NULL AND routine_barrier_request_id IS NULL AND grant_revision IS NULL AND routine_policy_digest IS NULL)
     OR (schedule_id IS NOT NULL AND routine_barrier_request_id IS NOT NULL AND grant_revision IS NOT NULL AND routine_policy_digest IS NOT NULL)),
  -- D6: a Routine Run never has a private decision.
  CHECK(decision_id IS NULL OR schedule_id IS NULL),
  CHECK((stage='refused' AND refusal IS NOT NULL) OR (stage<>'refused' AND refusal IS NULL)),
  CHECK((stage='confirmed' AND outcome_bytes IS NOT NULL AND withheld IS NOT NULL)
     OR (stage<>'confirmed' AND outcome_bytes IS NULL AND withheld IS NULL)),
  CHECK((stage='prepared' AND dispatched_at_ms IS NULL)
     OR (stage IN ('dispatched','confirmed','unknown') AND dispatched_at_ms IS NOT NULL)
     OR stage='refused'),
  CHECK((stage IN ('prepared','dispatched') AND resolved_at_ms IS NULL)
     OR (stage IN ('confirmed','refused','unknown') AND resolved_at_ms IS NOT NULL))
);
CREATE UNIQUE INDEX bot_private_effects_decision ON bot_private_effects(decision_id) WHERE decision_id IS NOT NULL;
CREATE UNIQUE INDEX bot_private_effects_open_run ON bot_private_effects(run_id) WHERE stage IN ('prepared','dispatched');
CREATE INDEX bot_private_effects_open ON bot_private_effects(user_scope,device_id,dispatcher_instance,run_id,operation_id) WHERE stage IN ('prepared','dispatched');
CREATE INDEX bot_private_effects_history ON bot_private_effects(user_scope,device_id,connection_id,created_at_ms,run_id,operation_id);

CREATE TRIGGER bot_private_effects_guard BEFORE UPDATE ON bot_private_effects
WHEN NEW.run_id IS NOT OLD.run_id OR NEW.operation_id IS NOT OLD.operation_id
 OR NEW.user_scope IS NOT OLD.user_scope OR NEW.device_id IS NOT OLD.device_id OR NEW.bot_id IS NOT OLD.bot_id
 OR NEW.attempt_id IS NOT OLD.attempt_id OR NEW.generation IS NOT OLD.generation OR NEW.turn_id IS NOT OLD.turn_id
 OR NEW.dispatcher_instance IS NOT OLD.dispatcher_instance
 OR NEW.decision_id IS NOT OLD.decision_id OR NEW.binding_digest IS NOT OLD.binding_digest
 OR NEW.connection_id IS NOT OLD.connection_id OR NEW.connection_revision IS NOT OLD.connection_revision
 OR NEW.account_digest IS NOT OLD.account_digest OR NEW.item_id IS NOT OLD.item_id
 OR NEW.item_ref_digest IS NOT OLD.item_ref_digest OR NEW.operation IS NOT OLD.operation
 OR NEW.request_fingerprint IS NOT OLD.request_fingerprint
 OR NEW.barrier_request_id IS NOT OLD.barrier_request_id OR NEW.config_revision IS NOT OLD.config_revision
 OR NEW.policy_revision IS NOT OLD.policy_revision OR NEW.policy_digest IS NOT OLD.policy_digest
 OR NEW.schedule_id IS NOT OLD.schedule_id
 OR NEW.routine_barrier_request_id IS NOT OLD.routine_barrier_request_id
 OR NEW.grant_revision IS NOT OLD.grant_revision
 OR NEW.routine_policy_digest IS NOT OLD.routine_policy_digest
 OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR (OLD.dispatched_at_ms IS NOT NULL AND NEW.dispatched_at_ms IS NOT OLD.dispatched_at_ms)
 OR NOT (
      (NEW.stage=OLD.stage AND OLD.stage IN ('prepared','dispatched'))
   OR (OLD.stage='prepared' AND NEW.stage IN ('dispatched','refused'))
   OR (OLD.stage='dispatched' AND NEW.stage IN ('confirmed','refused','unknown'))
 )
BEGIN SELECT RAISE(ABORT,'invalid private effect transition'); END;
CREATE TRIGGER bot_private_effects_retain BEFORE DELETE ON bot_private_effects
BEGIN SELECT RAISE(ABORT,'retained private effect'); END;

-- D. The closing assertion, as 029 ends.
CREATE TEMP TABLE m5_fk_guard (violations INTEGER NOT NULL CHECK(violations=0));
INSERT INTO m5_fk_guard SELECT count(*) FROM pragma_foreign_key_check;
DROP TABLE m5_fk_guard;
