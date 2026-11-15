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
  kind TEXT NOT NULL CHECK(kind IN ('koru_edit','native_approval')),
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
  CHECK((kind='koru_edit' AND operation_id IS NOT NULL AND native_prompt_id IS NULL AND native_deadline_at_ms IS NULL)
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
  CHECK(kind='koru_edit' OR phase NOT IN ('resume_prepared','await_retry','effect'))
);
CREATE UNIQUE INDEX bot_decisions_operation ON bot_decisions(run_id,operation_id) WHERE kind='koru_edit';
CREATE UNIQUE INDEX bot_decisions_prompt ON bot_decisions(run_id,origin_session_id,origin_turn_id,native_prompt_id) WHERE kind='native_approval';
CREATE UNIQUE INDEX bot_decisions_outstanding ON bot_decisions(run_id) WHERE phase<>'settled';
CREATE UNIQUE INDEX bot_decisions_answer ON bot_decisions(user_scope,device_id,answer_request_id) WHERE answer_request_id IS NOT NULL;
CREATE INDEX bot_decisions_deadline ON bot_decisions(device_id,deadline_at_ms,decision_id) WHERE phase<>'settled';
CREATE INDEX bot_decisions_owner_open ON bot_decisions(user_scope,device_id,decision_id) WHERE phase<>'settled';
CREATE INDEX bot_decisions_history ON bot_decisions(user_scope,device_id,run_id,created_at_ms,decision_id);

CREATE TABLE bot_wait_runtime (
  wait_id TEXT NOT NULL PRIMARY KEY CHECK(length(wait_id)=36),
  run_id TEXT NOT NULL REFERENCES bot_admissions(run_id) ON DELETE RESTRICT,
  decision_id TEXT UNIQUE,
  user_scope TEXT NOT NULL CHECK(length(user_scope)=36),
  device_id TEXT NOT NULL CHECK(length(device_id)=36),
  bot_id TEXT NOT NULL CHECK(length(bot_id)=36),
  attempt_id TEXT NOT NULL REFERENCES run_attempts(attempt_id) ON DELETE RESTRICT,
  generation INTEGER NOT NULL CHECK(typeof(generation)='integer' AND generation BETWEEN 1 AND 4294967295),
  session_id TEXT NOT NULL CHECK(length(session_id)=36),
  turn_id TEXT NOT NULL CHECK(length(turn_id)=36),
  observed_sequence INTEGER CHECK(observed_sequence IS NULL OR (typeof(observed_sequence)='integer' AND observed_sequence>=0)),
  reason TEXT NOT NULL CHECK(reason IN ('decision','capacity','native_expired','overlapping_prompt')),
  phase TEXT NOT NULL CHECK(phase IN ('fenced','hold_requested','held','retired','resume_reserved','cleanup_pending','settled')),
  revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0),
  ai_cleanup_receipt_json TEXT CHECK(ai_cleanup_receipt_json IS NULL OR (typeof(ai_cleanup_receipt_json)='text' AND json_valid(ai_cleanup_receipt_json) AND length(CAST(ai_cleanup_receipt_json AS BLOB)) BETWEEN 1 AND 4096)),
  release_evidence_json TEXT CHECK(release_evidence_json IS NULL OR (typeof(release_evidence_json)='text' AND json_valid(release_evidence_json) AND length(CAST(release_evidence_json AS BLOB)) BETWEEN 1 AND 4096)),
  cleanup_evidence_json TEXT CHECK(cleanup_evidence_json IS NULL OR (typeof(cleanup_evidence_json)='text' AND json_valid(cleanup_evidence_json) AND length(CAST(cleanup_evidence_json AS BLOB)) BETWEEN 1 AND 4096)),
  created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer'),
  updated_at_ms INTEGER NOT NULL CHECK(typeof(updated_at_ms)='integer'),
  settled_at_ms INTEGER CHECK(settled_at_ms IS NULL OR typeof(settled_at_ms)='integer'),
  UNIQUE(run_id,wait_id),
  FOREIGN KEY(run_id,decision_id) REFERENCES bot_decisions(run_id,decision_id) ON DELETE RESTRICT,
  FOREIGN KEY(run_id,generation) REFERENCES run_attempts(run_id,generation) ON DELETE RESTRICT,
  CHECK((reason='decision' AND decision_id IS NOT NULL) OR (reason<>'decision' AND decision_id IS NULL)),
  CHECK(phase NOT IN ('held','retired') OR release_evidence_json IS NOT NULL),
  CHECK((phase='settled' AND settled_at_ms IS NOT NULL) OR (phase<>'settled' AND settled_at_ms IS NULL))
);
CREATE UNIQUE INDEX bot_wait_runtime_open_run ON bot_wait_runtime(run_id) WHERE phase<>'settled';
CREATE INDEX bot_wait_runtime_recovery ON bot_wait_runtime(device_id,updated_at_ms,wait_id) WHERE phase<>'settled';

CREATE TABLE bot_decision_resumes (
  decision_id TEXT NOT NULL PRIMARY KEY REFERENCES bot_decisions(decision_id) ON DELETE RESTRICT,
  run_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('same_session','recorded_successor')),
  origin_attempt_id TEXT NOT NULL REFERENCES run_attempts(attempt_id) ON DELETE RESTRICT,
  origin_generation INTEGER NOT NULL CHECK(typeof(origin_generation)='integer' AND origin_generation BETWEEN 1 AND 4294967295),
  origin_session_id TEXT NOT NULL CHECK(length(origin_session_id)=36),
  consumer_attempt_id TEXT NOT NULL REFERENCES run_attempts(attempt_id) ON DELETE RESTRICT,
  consumer_generation INTEGER NOT NULL CHECK(typeof(consumer_generation)='integer' AND consumer_generation BETWEEN 1 AND 4294967295),
  consumer_session_id TEXT CHECK(consumer_session_id IS NULL OR length(consumer_session_id)=36),
  session_creation_key TEXT NOT NULL CHECK(length(session_creation_key)=36),
  input_key TEXT NOT NULL UNIQUE CHECK(length(input_key)=36),
  input_fingerprint BLOB NOT NULL CHECK(typeof(input_fingerprint)='blob' AND length(input_fingerprint)=32),
  accepted_turn_id TEXT CHECK(accepted_turn_id IS NULL OR length(accepted_turn_id)=36),
  state TEXT NOT NULL CHECK(state IN ('prepared','session_bound','input_unconfirmed','input_acknowledged','closed')),
  receipt_json TEXT CHECK(receipt_json IS NULL OR (typeof(receipt_json)='text' AND json_valid(receipt_json) AND length(CAST(receipt_json AS BLOB)) BETWEEN 1 AND 65536)),
  created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer'),
  updated_at_ms INTEGER NOT NULL CHECK(typeof(updated_at_ms)='integer'),
  FOREIGN KEY(run_id,decision_id) REFERENCES bot_decisions(run_id,decision_id) ON DELETE RESTRICT,
  FOREIGN KEY(run_id,origin_generation) REFERENCES run_attempts(run_id,generation) ON DELETE RESTRICT,
  FOREIGN KEY(run_id,consumer_generation) REFERENCES run_attempts(run_id,generation) ON DELETE RESTRICT,
  CHECK((kind='same_session' AND consumer_attempt_id=origin_attempt_id AND consumer_generation=origin_generation AND consumer_session_id IS NOT NULL AND consumer_session_id=origin_session_id)
     OR (kind='recorded_successor' AND consumer_attempt_id<>origin_attempt_id AND consumer_generation=origin_generation+1)),
  CHECK(state IN ('prepared','closed') OR consumer_session_id IS NOT NULL),
  CHECK((state='input_acknowledged' AND accepted_turn_id IS NOT NULL AND receipt_json IS NOT NULL)
     OR (state='closed') OR (state<>'input_acknowledged' AND accepted_turn_id IS NULL AND receipt_json IS NULL))
);
CREATE UNIQUE INDEX bot_decision_successor_generation ON bot_decision_resumes(run_id,consumer_generation) WHERE kind='recorded_successor';
CREATE UNIQUE INDEX bot_decision_consumer_turn ON bot_decision_resumes(run_id,consumer_attempt_id,accepted_turn_id) WHERE accepted_turn_id IS NOT NULL;

CREATE TABLE native_decision_leases (
  run_id TEXT NOT NULL,
  operation_id TEXT NOT NULL,
  decision_id TEXT NOT NULL UNIQUE,
  binding_digest BLOB NOT NULL CHECK(typeof(binding_digest)='blob' AND length(binding_digest)=32),
  consumer_attempt_id TEXT NOT NULL REFERENCES run_attempts(attempt_id) ON DELETE RESTRICT,
  consumer_generation INTEGER NOT NULL CHECK(typeof(consumer_generation)='integer' AND consumer_generation BETWEEN 1 AND 4294967295),
  consumer_turn_id TEXT NOT NULL CHECK(length(consumer_turn_id)=36),
  consumed_at_ms INTEGER NOT NULL CHECK(typeof(consumed_at_ms)='integer'),
  PRIMARY KEY(run_id,operation_id),
  FOREIGN KEY(run_id,operation_id) REFERENCES native_result_intents(run_id,operation_id) ON DELETE RESTRICT,
  FOREIGN KEY(run_id,decision_id) REFERENCES bot_decisions(run_id,decision_id) ON DELETE RESTRICT,
  FOREIGN KEY(decision_id) REFERENCES bot_decision_resumes(decision_id) ON DELETE RESTRICT,
  FOREIGN KEY(run_id,consumer_generation) REFERENCES run_attempts(run_id,generation) ON DELETE RESTRICT
);

CREATE TABLE bot_decision_native_answers (
  decision_id TEXT NOT NULL PRIMARY KEY REFERENCES bot_decisions(decision_id) ON DELETE RESTRICT,
  session_id TEXT NOT NULL CHECK(length(session_id)=36),
  turn_id TEXT NOT NULL CHECK(length(turn_id)=36),
  prompt_id TEXT NOT NULL CHECK(length(CAST(prompt_id AS BLOB)) BETWEEN 1 AND 4096),
  response_key TEXT NOT NULL UNIQUE CHECK(length(response_key)=36),
  choice_id TEXT NOT NULL CHECK(length(CAST(choice_id AS BLOB)) BETWEEN 1 AND 4096),
  choice_intent TEXT NOT NULL CHECK(choice_intent IN ('allow','allow_remembered','reject','reject_remembered')),
  native_decision TEXT NOT NULL CHECK(native_decision IN ('approve','reject')),
  state TEXT NOT NULL CHECK(state IN ('prepared','unconfirmed','acknowledged')),
  acknowledgment_json TEXT CHECK(acknowledgment_json IS NULL OR (typeof(acknowledgment_json)='text' AND json_valid(acknowledgment_json) AND length(CAST(acknowledgment_json AS BLOB)) BETWEEN 1 AND 65536)),
  created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer'),
  updated_at_ms INTEGER NOT NULL CHECK(typeof(updated_at_ms)='integer'),
  CHECK((choice_intent IN ('allow','allow_remembered') AND native_decision='approve') OR (choice_intent IN ('reject','reject_remembered') AND native_decision='reject')),
  CHECK((state='acknowledged' AND acknowledgment_json IS NOT NULL) OR (state<>'acknowledged' AND acknowledgment_json IS NULL))
);

CREATE TABLE bot_decision_publications (
  transition_id TEXT NOT NULL PRIMARY KEY CHECK(length(transition_id)=36),
  run_id TEXT NOT NULL REFERENCES bot_admissions(run_id) ON DELETE RESTRICT,
  wait_id TEXT NOT NULL,
  decision_id TEXT,
  run_sequence INTEGER NOT NULL CHECK(typeof(run_sequence)='integer' AND run_sequence>0),
  decision_revision INTEGER CHECK(decision_revision IS NULL OR (typeof(decision_revision)='integer' AND decision_revision>0)),
  workspace_id TEXT NOT NULL CHECK(length(workspace_id)=36),
  document_id TEXT NOT NULL CHECK(length(document_id)=36),
  source_schema INTEGER NOT NULL CHECK(source_schema IN (3,4,5)),
  subject_digest BLOB NOT NULL CHECK(typeof(subject_digest)='blob' AND length(subject_digest)=32),
  projection_kind TEXT NOT NULL CHECK(projection_kind IN ('source','control')),
  target_json TEXT NOT NULL CHECK(typeof(target_json)='text' AND json_valid(target_json) AND length(CAST(target_json AS BLOB)) BETWEEN 1 AND 65536),
  target_digest BLOB NOT NULL CHECK(typeof(target_digest)='blob' AND length(target_digest)=32),
  state TEXT NOT NULL CHECK(state IN ('pending','prepared','applied','superseded')),
  base_version BLOB CHECK(base_version IS NULL OR (typeof(base_version)='blob' AND length(base_version) BETWEEN 1 AND 65536)),
  result_version BLOB CHECK(result_version IS NULL OR (typeof(result_version)='blob' AND length(result_version) BETWEEN 1 AND 65536)),
  prepared_delta BLOB CHECK(prepared_delta IS NULL OR (typeof(prepared_delta)='blob' AND length(prepared_delta) BETWEEN 1 AND 262144)),
  delta_digest BLOB CHECK(delta_digest IS NULL OR (typeof(delta_digest)='blob' AND length(delta_digest)=32)),
  superseded_by_sequence INTEGER CHECK(superseded_by_sequence IS NULL OR (typeof(superseded_by_sequence)='integer' AND superseded_by_sequence>run_sequence)),
  observed_version BLOB CHECK(observed_version IS NULL OR (typeof(observed_version)='blob' AND length(observed_version) BETWEEN 1 AND 65536)),
  created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer'),
  completed_at_ms INTEGER CHECK(completed_at_ms IS NULL OR typeof(completed_at_ms)='integer'),
  UNIQUE(run_id,run_sequence),
  FOREIGN KEY(run_id,wait_id) REFERENCES bot_wait_runtime(run_id,wait_id) ON DELETE RESTRICT,
  FOREIGN KEY(run_id,decision_id) REFERENCES bot_decisions(run_id,decision_id) ON DELETE RESTRICT,
  FOREIGN KEY(run_id,superseded_by_sequence) REFERENCES bot_decision_publications(run_id,run_sequence) DEFERRABLE INITIALLY DEFERRED,
  CHECK((decision_id IS NULL AND decision_revision IS NULL) OR (decision_id IS NOT NULL AND decision_revision IS NOT NULL)),
  CHECK(projection_kind<>'source' OR state<>'superseded'),
  CHECK((state IN ('pending','superseded') AND base_version IS NULL AND result_version IS NULL AND prepared_delta IS NULL AND delta_digest IS NULL)
     OR (state IN ('prepared','applied') AND base_version IS NOT NULL AND result_version IS NOT NULL AND prepared_delta IS NOT NULL AND delta_digest IS NOT NULL)),
  CHECK((state IN ('pending','prepared') AND observed_version IS NULL AND completed_at_ms IS NULL AND superseded_by_sequence IS NULL)
     OR (state='applied' AND observed_version IS NOT NULL AND completed_at_ms IS NOT NULL AND superseded_by_sequence IS NULL)
     OR (state='superseded' AND observed_version IS NULL AND completed_at_ms IS NOT NULL AND superseded_by_sequence IS NOT NULL))
);
CREATE UNIQUE INDEX bot_decision_publications_pending_source ON bot_decision_publications(run_id) WHERE state='pending' AND projection_kind='source';
CREATE UNIQUE INDEX bot_decision_publications_pending_control ON bot_decision_publications(run_id) WHERE state='pending' AND projection_kind='control';
CREATE UNIQUE INDEX bot_decision_publications_prepared ON bot_decision_publications(run_id) WHERE state='prepared';
CREATE INDEX pending_publications_open_run ON pending_publications(run_id) WHERE acknowledged_at_ms IS NULL;

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
CREATE TRIGGER bot_wait_runtime_immutable BEFORE UPDATE ON bot_wait_runtime
WHEN NEW.wait_id IS NOT OLD.wait_id OR NEW.run_id IS NOT OLD.run_id OR NEW.decision_id IS NOT OLD.decision_id
 OR NEW.user_scope IS NOT OLD.user_scope OR NEW.device_id IS NOT OLD.device_id OR NEW.bot_id IS NOT OLD.bot_id
 OR NEW.attempt_id IS NOT OLD.attempt_id OR NEW.generation IS NOT OLD.generation
 OR NEW.session_id IS NOT OLD.session_id OR NEW.turn_id IS NOT OLD.turn_id OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR (OLD.ai_cleanup_receipt_json IS NOT NULL AND NEW.ai_cleanup_receipt_json IS NOT OLD.ai_cleanup_receipt_json)
 OR (OLD.cleanup_evidence_json IS NOT NULL AND NEW.cleanup_evidence_json IS NOT OLD.cleanup_evidence_json)
 OR (OLD.phase='settled' AND (NEW.phase IS NOT OLD.phase OR NEW.settled_at_ms IS NOT OLD.settled_at_ms))
BEGIN SELECT RAISE(ABORT,'immutable Bot wait identity'); END;
CREATE TRIGGER bot_decision_resumes_immutable BEFORE UPDATE ON bot_decision_resumes
WHEN NEW.decision_id IS NOT OLD.decision_id OR NEW.run_id IS NOT OLD.run_id OR NEW.kind IS NOT OLD.kind
 OR NEW.origin_attempt_id IS NOT OLD.origin_attempt_id OR NEW.origin_generation IS NOT OLD.origin_generation
 OR NEW.origin_session_id IS NOT OLD.origin_session_id OR NEW.consumer_attempt_id IS NOT OLD.consumer_attempt_id
 OR NEW.consumer_generation IS NOT OLD.consumer_generation OR NEW.session_creation_key IS NOT OLD.session_creation_key
 OR NEW.input_key IS NOT OLD.input_key OR NEW.input_fingerprint IS NOT OLD.input_fingerprint OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR (OLD.consumer_session_id IS NOT NULL AND NEW.consumer_session_id IS NOT OLD.consumer_session_id)
 OR (OLD.accepted_turn_id IS NOT NULL AND (NEW.accepted_turn_id IS NOT OLD.accepted_turn_id OR NEW.receipt_json IS NOT OLD.receipt_json))
 OR (OLD.state='closed' AND NEW.state IS NOT OLD.state)
BEGIN SELECT RAISE(ABORT,'immutable Bot decision resume'); END;
CREATE TRIGGER native_decision_leases_immutable BEFORE UPDATE ON native_decision_leases
BEGIN SELECT RAISE(ABORT,'immutable Bot decision lease'); END;
CREATE TRIGGER bot_decision_native_answers_immutable BEFORE UPDATE ON bot_decision_native_answers
WHEN NEW.decision_id IS NOT OLD.decision_id OR NEW.session_id IS NOT OLD.session_id OR NEW.turn_id IS NOT OLD.turn_id
 OR NEW.prompt_id IS NOT OLD.prompt_id OR NEW.response_key IS NOT OLD.response_key
 OR NEW.choice_id IS NOT OLD.choice_id OR NEW.choice_intent IS NOT OLD.choice_intent
 OR NEW.native_decision IS NOT OLD.native_decision OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR (OLD.acknowledgment_json IS NOT NULL AND (NEW.acknowledgment_json IS NOT OLD.acknowledgment_json OR NEW.state IS NOT OLD.state))
BEGIN SELECT RAISE(ABORT,'immutable native decision answer'); END;
CREATE TRIGGER bot_decision_publications_immutable BEFORE UPDATE ON bot_decision_publications
WHEN NEW.transition_id IS NOT OLD.transition_id OR NEW.run_id IS NOT OLD.run_id OR NEW.wait_id IS NOT OLD.wait_id
 OR NEW.decision_id IS NOT OLD.decision_id OR NEW.run_sequence IS NOT OLD.run_sequence OR NEW.decision_revision IS NOT OLD.decision_revision
 OR NEW.workspace_id IS NOT OLD.workspace_id OR NEW.document_id IS NOT OLD.document_id OR NEW.source_schema IS NOT OLD.source_schema
 OR NEW.projection_kind IS NOT OLD.projection_kind
 OR NEW.subject_digest IS NOT OLD.subject_digest OR NEW.target_json IS NOT OLD.target_json OR NEW.target_digest IS NOT OLD.target_digest
 OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR (OLD.prepared_delta IS NOT NULL AND (NEW.prepared_delta IS NOT OLD.prepared_delta OR NEW.delta_digest IS NOT OLD.delta_digest
     OR NEW.base_version IS NOT OLD.base_version OR NEW.result_version IS NOT OLD.result_version))
 OR (OLD.state='prepared' AND NEW.state NOT IN ('prepared','applied'))
 OR (OLD.state IN ('applied','superseded') AND (NEW.state IS NOT OLD.state OR NEW.observed_version IS NOT OLD.observed_version
     OR NEW.completed_at_ms IS NOT OLD.completed_at_ms OR NEW.superseded_by_sequence IS NOT OLD.superseded_by_sequence))
BEGIN SELECT RAISE(ABORT,'immutable Bot decision publication'); END;
CREATE TRIGGER bot_decisions_retain BEFORE DELETE ON bot_decisions BEGIN SELECT RAISE(ABORT,'retained Bot decision'); END;
CREATE TRIGGER bot_wait_runtime_retain BEFORE DELETE ON bot_wait_runtime BEGIN SELECT RAISE(ABORT,'retained Bot wait'); END;
CREATE TRIGGER bot_decision_resumes_retain BEFORE DELETE ON bot_decision_resumes BEGIN SELECT RAISE(ABORT,'retained Bot resume'); END;
CREATE TRIGGER native_decision_leases_retain BEFORE DELETE ON native_decision_leases BEGIN SELECT RAISE(ABORT,'retained decision lease'); END;
CREATE TRIGGER bot_decision_native_answers_retain BEFORE DELETE ON bot_decision_native_answers BEGIN SELECT RAISE(ABORT,'retained native answer'); END;
CREATE TRIGGER bot_decision_publications_retain BEFORE DELETE ON bot_decision_publications BEGIN SELECT RAISE(ABORT,'retained Run publication'); END;

ALTER TABLE bot_wait_runtime ADD COLUMN ai_enrollment_request_json TEXT
  CHECK(ai_enrollment_request_json IS NULL OR
    (typeof(ai_enrollment_request_json)='text' AND json_valid(ai_enrollment_request_json)
     AND length(CAST(ai_enrollment_request_json AS BLOB)) BETWEEN 1 AND 4096))
  CHECK(ai_cleanup_receipt_json IS NULL OR ai_enrollment_request_json IS NOT NULL);
CREATE TRIGGER bot_wait_enrollment_request_immutable BEFORE UPDATE ON bot_wait_runtime
WHEN OLD.ai_enrollment_request_json IS NOT NULL
 AND NEW.ai_enrollment_request_json IS NOT OLD.ai_enrollment_request_json
BEGIN SELECT RAISE(ABORT,'immutable wait enrollment request'); END;

CREATE TRIGGER bot_wait_release_evidence_immutable BEFORE UPDATE ON bot_wait_runtime
WHEN OLD.release_evidence_json IS NOT NULL
 AND NEW.release_evidence_json IS NOT OLD.release_evidence_json
BEGIN SELECT RAISE(ABORT,'immutable origin release evidence'); END;
CREATE TRIGGER bot_wait_runtime_phase_guard BEFORE UPDATE ON bot_wait_runtime
WHEN NOT (
  NEW.phase=OLD.phase
  OR (OLD.phase='fenced' AND NEW.phase IN ('hold_requested','cleanup_pending','settled'))
  OR (OLD.phase='hold_requested' AND NEW.phase IN ('held','retired','cleanup_pending'))
  OR (OLD.phase IN ('held','retired') AND NEW.phase IN ('resume_reserved','cleanup_pending'))
  OR (OLD.phase='resume_reserved' AND NEW.phase IN ('cleanup_pending','settled'))
  OR (OLD.phase='cleanup_pending' AND NEW.phase='settled')
)
 OR (NEW.phase IN ('held','retired') AND EXISTS (
   SELECT 1 FROM bot_decision_resumes r WHERE r.decision_id=NEW.decision_id
 ))
BEGIN SELECT RAISE(ABORT,'invalid Bot wait phase'); END;

CREATE TEMP TABLE p42_fk_guard (violations INTEGER NOT NULL CHECK(violations=0));
INSERT INTO p42_fk_guard SELECT count(*) FROM pragma_foreign_key_check;
DROP TABLE p42_fk_guard;
