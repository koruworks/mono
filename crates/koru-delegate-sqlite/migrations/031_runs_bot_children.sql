-- runs.db 12 (M6): child requests. One Bot Run asks other Bots and waits.
-- Eight parts, in order A to H. The runner has foreign keys on and runs this
-- file in one transaction that also stamps the version: a failing statement
-- rolls everything back and the version stays 11. No pragma is used.
--
-- Two tables are rebuilt with every row kept: bot_wait_runtime (the reason
-- 'child', two checks, one index, one trigger, one widened trigger) and
-- bot_decision_publications (source_schema admits 6). The publications table
-- is the only table with a foreign key to bot_wait_runtime, so it is copied
-- aside and dropped first; when its parent is then dropped no table
-- definition refers to it, and both are created again under their real names
-- before any row returns. Nothing is created as `_next` and renamed.

-- A. Copy the rows aside, in rowid order.
CREATE TEMP TABLE m6_wait_hold AS SELECT
  wait_id,run_id,decision_id,user_scope,device_id,bot_id,attempt_id,generation,
  session_id,turn_id,observed_sequence,reason,phase,revision,
  ai_cleanup_receipt_json,release_evidence_json,cleanup_evidence_json,
  created_at_ms,updated_at_ms,settled_at_ms,ai_enrollment_request_json
  FROM bot_wait_runtime ORDER BY rowid;
CREATE TEMP TABLE m6_publication_hold AS SELECT
  transition_id,run_id,wait_id,decision_id,run_sequence,decision_revision,
  workspace_id,document_id,source_schema,subject_digest,projection_kind,
  target_json,target_digest,state,base_version,result_version,prepared_delta,
  delta_digest,superseded_by_sequence,observed_version,created_at_ms,completed_at_ms
  FROM bot_decision_publications ORDER BY rowid;

-- B. Drop the child, then the parent. DROP TABLE fires no trigger.
DROP TABLE bot_decision_publications;
DROP TABLE bot_wait_runtime;

-- C. Recreate bot_wait_runtime (029 lines 64 to 93 with the column of line
--    264 folded in; changes marked M6) and put the rows back.
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
  reason TEXT NOT NULL CHECK(reason IN ('decision','capacity','native_expired','overlapping_prompt','child')),  -- M6
  phase TEXT NOT NULL CHECK(phase IN ('fenced','hold_requested','held','retired','resume_reserved','cleanup_pending','settled')),
  revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0),
  ai_cleanup_receipt_json TEXT CHECK(ai_cleanup_receipt_json IS NULL OR (typeof(ai_cleanup_receipt_json)='text' AND json_valid(ai_cleanup_receipt_json) AND length(CAST(ai_cleanup_receipt_json AS BLOB)) BETWEEN 1 AND 4096)),
  release_evidence_json TEXT CHECK(release_evidence_json IS NULL OR (typeof(release_evidence_json)='text' AND json_valid(release_evidence_json) AND length(CAST(release_evidence_json AS BLOB)) BETWEEN 1 AND 4096)),
  cleanup_evidence_json TEXT CHECK(cleanup_evidence_json IS NULL OR (typeof(cleanup_evidence_json)='text' AND json_valid(cleanup_evidence_json) AND length(CAST(cleanup_evidence_json AS BLOB)) BETWEEN 1 AND 4096)),
  created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer'),
  updated_at_ms INTEGER NOT NULL CHECK(typeof(updated_at_ms)='integer'),
  settled_at_ms INTEGER CHECK(settled_at_ms IS NULL OR typeof(settled_at_ms)='integer'),
  ai_enrollment_request_json TEXT
    CHECK(ai_enrollment_request_json IS NULL OR
      (typeof(ai_enrollment_request_json)='text' AND json_valid(ai_enrollment_request_json)
       AND length(CAST(ai_enrollment_request_json AS BLOB)) BETWEEN 1 AND 4096))
    CHECK(ai_cleanup_receipt_json IS NULL OR ai_enrollment_request_json IS NOT NULL),
  UNIQUE(run_id,wait_id),
  FOREIGN KEY(run_id,decision_id) REFERENCES bot_decisions(run_id,decision_id) ON DELETE RESTRICT,
  FOREIGN KEY(run_id,generation) REFERENCES run_attempts(run_id,generation) ON DELETE RESTRICT,
  CHECK((reason='decision' AND decision_id IS NOT NULL) OR (reason<>'decision' AND decision_id IS NULL)),
  CHECK(phase NOT IN ('held','retired') OR release_evidence_json IS NOT NULL),
  CHECK((phase='settled' AND settled_at_ms IS NOT NULL) OR (phase<>'settled' AND settled_at_ms IS NULL)),
  CHECK(reason<>'child' OR phase NOT IN ('fenced','held')),          -- M6: born hold_requested; never a same-session hold
  CHECK(reason<>'child' OR ai_enrollment_request_json IS NOT NULL)   -- M6: the intent is frozen at the ask
);
INSERT INTO bot_wait_runtime
 (wait_id,run_id,decision_id,user_scope,device_id,bot_id,attempt_id,generation,
  session_id,turn_id,observed_sequence,reason,phase,revision,
  ai_cleanup_receipt_json,release_evidence_json,cleanup_evidence_json,
  created_at_ms,updated_at_ms,settled_at_ms,ai_enrollment_request_json)
SELECT wait_id,run_id,decision_id,user_scope,device_id,bot_id,attempt_id,generation,
  session_id,turn_id,observed_sequence,reason,phase,revision,
  ai_cleanup_receipt_json,release_evidence_json,cleanup_evidence_json,
  created_at_ms,updated_at_ms,settled_at_ms,ai_enrollment_request_json
 FROM m6_wait_hold ORDER BY rowid;
CREATE UNIQUE INDEX bot_wait_runtime_open_run ON bot_wait_runtime(run_id) WHERE phase<>'settled';
CREATE INDEX bot_wait_runtime_recovery ON bot_wait_runtime(device_id,updated_at_ms,wait_id) WHERE phase<>'settled';
-- M6: the resume selector and the boot scan for child waits. At most 50 rows.
CREATE INDEX bot_wait_runtime_child_open ON bot_wait_runtime(device_id,created_at_ms,wait_id)
  WHERE reason='child' AND phase<>'settled';
-- Four triggers with the exact text of 029.
CREATE TRIGGER bot_wait_runtime_immutable BEFORE UPDATE ON bot_wait_runtime
WHEN NEW.wait_id IS NOT OLD.wait_id OR NEW.run_id IS NOT OLD.run_id OR NEW.decision_id IS NOT OLD.decision_id
 OR NEW.user_scope IS NOT OLD.user_scope OR NEW.device_id IS NOT OLD.device_id OR NEW.bot_id IS NOT OLD.bot_id
 OR NEW.attempt_id IS NOT OLD.attempt_id OR NEW.generation IS NOT OLD.generation
 OR NEW.session_id IS NOT OLD.session_id OR NEW.turn_id IS NOT OLD.turn_id OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR (OLD.ai_cleanup_receipt_json IS NOT NULL AND NEW.ai_cleanup_receipt_json IS NOT OLD.ai_cleanup_receipt_json)
 OR (OLD.cleanup_evidence_json IS NOT NULL AND NEW.cleanup_evidence_json IS NOT OLD.cleanup_evidence_json)
 OR (OLD.phase='settled' AND (NEW.phase IS NOT OLD.phase OR NEW.settled_at_ms IS NOT OLD.settled_at_ms))
BEGIN SELECT RAISE(ABORT,'immutable Bot wait identity'); END;
CREATE TRIGGER bot_wait_runtime_retain BEFORE DELETE ON bot_wait_runtime BEGIN SELECT RAISE(ABORT,'retained Bot wait'); END;
CREATE TRIGGER bot_wait_enrollment_request_immutable BEFORE UPDATE ON bot_wait_runtime
WHEN OLD.ai_enrollment_request_json IS NOT NULL
 AND NEW.ai_enrollment_request_json IS NOT OLD.ai_enrollment_request_json
BEGIN SELECT RAISE(ABORT,'immutable wait enrollment request'); END;
CREATE TRIGGER bot_wait_release_evidence_immutable BEFORE UPDATE ON bot_wait_runtime
WHEN OLD.release_evidence_json IS NOT NULL
 AND NEW.release_evidence_json IS NOT OLD.release_evidence_json
BEGIN SELECT RAISE(ABORT,'immutable origin release evidence'); END;
-- The phase guard gains one clause, and one trigger is new.
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
 OR (NEW.phase IN ('held','retired') AND EXISTS (              -- M6
   SELECT 1 FROM bot_child_resumes c WHERE c.wait_id=NEW.wait_id
 ))
BEGIN SELECT RAISE(ABORT,'invalid Bot wait phase'); END;

-- M6: an open request implies an open, unresumed wait. The cycle walk's
-- bound (3.7) rests on this, so SQL holds it.
CREATE TRIGGER bot_wait_runtime_child_requests_guard BEFORE UPDATE OF phase ON bot_wait_runtime
WHEN OLD.reason='child' AND NEW.phase IN ('resume_reserved','cleanup_pending','settled')
 AND NEW.phase IS NOT OLD.phase
 AND EXISTS (SELECT 1 FROM bot_child_requests q WHERE q.wait_id=OLD.wait_id AND q.state='open')
BEGIN SELECT RAISE(ABORT,'open child request'); END;

-- D. Recreate bot_decision_publications (029 lines 158 to 192; one change
--    marked M6) and put the rows back.
CREATE TABLE bot_decision_publications (
  transition_id TEXT NOT NULL PRIMARY KEY CHECK(length(transition_id)=36),
  run_id TEXT NOT NULL REFERENCES bot_admissions(run_id) ON DELETE RESTRICT,
  wait_id TEXT NOT NULL,
  decision_id TEXT,
  run_sequence INTEGER NOT NULL CHECK(typeof(run_sequence)='integer' AND run_sequence>0),
  decision_revision INTEGER CHECK(decision_revision IS NULL OR (typeof(decision_revision)='integer' AND decision_revision>0)),
  workspace_id TEXT NOT NULL CHECK(length(workspace_id)=36),
  document_id TEXT NOT NULL CHECK(length(document_id)=36),
  source_schema INTEGER NOT NULL CHECK(source_schema IN (3,4,5,6)),  -- M6
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
INSERT INTO bot_decision_publications
 (transition_id,run_id,wait_id,decision_id,run_sequence,decision_revision,
  workspace_id,document_id,source_schema,subject_digest,projection_kind,
  target_json,target_digest,state,base_version,result_version,prepared_delta,
  delta_digest,superseded_by_sequence,observed_version,created_at_ms,completed_at_ms)
SELECT transition_id,run_id,wait_id,decision_id,run_sequence,decision_revision,
  workspace_id,document_id,source_schema,subject_digest,projection_kind,
  target_json,target_digest,state,base_version,result_version,prepared_delta,
  delta_digest,superseded_by_sequence,observed_version,created_at_ms,completed_at_ms
 FROM m6_publication_hold ORDER BY rowid;
CREATE UNIQUE INDEX bot_decision_publications_pending_source ON bot_decision_publications(run_id) WHERE state='pending' AND projection_kind='source';
CREATE UNIQUE INDEX bot_decision_publications_pending_control ON bot_decision_publications(run_id) WHERE state='pending' AND projection_kind='control';
CREATE UNIQUE INDEX bot_decision_publications_prepared ON bot_decision_publications(run_id) WHERE state='prepared';
-- Its two triggers with the exact text of 029.
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
CREATE TRIGGER bot_decision_publications_retain BEFORE DELETE ON bot_decision_publications BEGIN SELECT RAISE(ABORT,'retained Run publication'); END;

-- E. Two new tables. Ids, digests, closed tokens, numbers and times only: no
--    task text, title, path or page content has a column here.
-- One request of one ask. Never deleted.
CREATE TABLE bot_child_requests (
  child_request_id TEXT NOT NULL PRIMARY KEY CHECK(length(child_request_id)=36),
  parent_run_id TEXT NOT NULL REFERENCES bot_admissions(run_id) ON DELETE RESTRICT,
  wait_id TEXT NOT NULL CHECK(length(wait_id)=36),
  ask_key TEXT NOT NULL CHECK(length(ask_key)=36),
  ordinal INTEGER NOT NULL CHECK(typeof(ordinal)='integer' AND ordinal BETWEEN 0 AND 2),
  user_scope TEXT NOT NULL CHECK(length(user_scope)=36),
  device_id TEXT NOT NULL CHECK(length(device_id)=36),
  parent_bot_id TEXT NOT NULL CHECK(length(parent_bot_id)=36),
  recipient_bot_id TEXT NOT NULL CHECK(length(recipient_bot_id)=36),
  human_profile_id TEXT NOT NULL CHECK(length(human_profile_id)=36),
  child_run_id TEXT NOT NULL UNIQUE REFERENCES bot_admissions(run_id) ON DELETE RESTRICT,
  binding_version INTEGER NOT NULL CHECK(binding_version=1),
  binding_digest BLOB NOT NULL CHECK(typeof(binding_digest)='blob' AND length(binding_digest)=32),
  binding_json TEXT NOT NULL CHECK(typeof(binding_json)='text' AND json_valid(binding_json) AND length(CAST(binding_json AS BLOB)) BETWEEN 1 AND 65536),
  state TEXT NOT NULL CHECK(state IN ('open','settled')),
  settled_by TEXT CHECK(settled_by IS NULL OR settled_by IN ('child_ended','timed_out','parent_stopped')),
  wait_elapsed_ms INTEGER NOT NULL CHECK(typeof(wait_elapsed_ms)='integer' AND wait_elapsed_ms>=0),
  clock TEXT NOT NULL CHECK(clock IN ('running','paused','stopped')),
  clock_boot_id TEXT CHECK(clock_boot_id IS NULL OR length(clock_boot_id)=36),
  clock_since_ms INTEGER CHECK(clock_since_ms IS NULL OR typeof(clock_since_ms)='integer'),
  revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0),
  created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer'),
  updated_at_ms INTEGER NOT NULL CHECK(typeof(updated_at_ms)='integer'),
  settled_at_ms INTEGER CHECK(settled_at_ms IS NULL OR typeof(settled_at_ms)='integer'),
  UNIQUE(parent_run_id,ask_key,ordinal),
  UNIQUE(wait_id,ordinal),
  FOREIGN KEY(parent_run_id,wait_id) REFERENCES bot_wait_runtime(run_id,wait_id) ON DELETE RESTRICT,
  CHECK(parent_bot_id<>recipient_bot_id),
  CHECK(parent_run_id<>child_run_id),
  CHECK((state='open' AND settled_by IS NULL AND settled_at_ms IS NULL AND clock IN ('running','paused'))
     OR (state='settled' AND settled_by IS NOT NULL AND settled_at_ms IS NOT NULL AND clock='stopped')),
  CHECK((clock='running' AND clock_boot_id IS NOT NULL AND clock_since_ms IS NOT NULL)
     OR (clock<>'running' AND clock_boot_id IS NULL AND clock_since_ms IS NULL))
);
-- The edges of the wait graph (3.7). Open rows only.
CREATE INDEX bot_child_requests_open_parent ON bot_child_requests(device_id,parent_bot_id,child_request_id) WHERE state='open';
-- "Is every request of this wait settled?" and the per-wait read.
CREATE INDEX bot_child_requests_wait ON bot_child_requests(wait_id,state,ordinal);
CREATE INDEX bot_child_requests_history ON bot_child_requests(user_scope,device_id,parent_run_id,created_at_ms,child_request_id);

-- Three recipients per parent Run, for good (decision 6).
CREATE TRIGGER bot_child_requests_limit BEFORE INSERT ON bot_child_requests
WHEN (SELECT count(*) FROM bot_child_requests WHERE parent_run_id=NEW.parent_run_id)>=3
BEGIN SELECT RAISE(ABORT,'child request limit'); END;
-- Depth one: a child Run never asks, and a parent Run is never a child.
CREATE TRIGGER bot_child_requests_depth BEFORE INSERT ON bot_child_requests
WHEN EXISTS(SELECT 1 FROM bot_child_requests WHERE child_run_id=NEW.parent_run_id)
  OR EXISTS(SELECT 1 FROM bot_child_requests WHERE parent_run_id=NEW.child_run_id)
BEGIN SELECT RAISE(ABORT,'child request depth'); END;
CREATE TRIGGER bot_child_requests_guard BEFORE UPDATE ON bot_child_requests
WHEN NEW.child_request_id IS NOT OLD.child_request_id OR NEW.parent_run_id IS NOT OLD.parent_run_id
 OR NEW.wait_id IS NOT OLD.wait_id OR NEW.ask_key IS NOT OLD.ask_key OR NEW.ordinal IS NOT OLD.ordinal
 OR NEW.user_scope IS NOT OLD.user_scope OR NEW.device_id IS NOT OLD.device_id
 OR NEW.parent_bot_id IS NOT OLD.parent_bot_id OR NEW.recipient_bot_id IS NOT OLD.recipient_bot_id
 OR NEW.human_profile_id IS NOT OLD.human_profile_id OR NEW.child_run_id IS NOT OLD.child_run_id
 OR NEW.binding_version IS NOT OLD.binding_version OR NEW.binding_digest IS NOT OLD.binding_digest
 OR NEW.binding_json IS NOT OLD.binding_json OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR NEW.wait_elapsed_ms<OLD.wait_elapsed_ms
 OR NEW.revision<=OLD.revision
 OR OLD.state='settled'
BEGIN SELECT RAISE(ABORT,'invalid child request transition'); END;
CREATE TRIGGER bot_child_requests_retain BEFORE DELETE ON bot_child_requests
BEGIN SELECT RAISE(ABORT,'retained child request'); END;

-- The parent's resume after a child wait. Mirrors bot_decision_resumes
-- (029:95-123), keyed by the wait, always a recorded successor.
CREATE TABLE bot_child_resumes (
  wait_id TEXT NOT NULL PRIMARY KEY REFERENCES bot_wait_runtime(wait_id) ON DELETE RESTRICT,
  run_id TEXT NOT NULL,
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
  FOREIGN KEY(run_id,wait_id) REFERENCES bot_wait_runtime(run_id,wait_id) ON DELETE RESTRICT,
  FOREIGN KEY(run_id,origin_generation) REFERENCES run_attempts(run_id,generation) ON DELETE RESTRICT,
  FOREIGN KEY(run_id,consumer_generation) REFERENCES run_attempts(run_id,generation) ON DELETE RESTRICT,
  CHECK(consumer_attempt_id<>origin_attempt_id AND consumer_generation=origin_generation+1),
  CHECK(state IN ('prepared','closed') OR consumer_session_id IS NOT NULL),
  CHECK((state='input_acknowledged' AND accepted_turn_id IS NOT NULL AND receipt_json IS NOT NULL)
     OR (state='closed') OR (state<>'input_acknowledged' AND accepted_turn_id IS NULL AND receipt_json IS NULL))
);
CREATE UNIQUE INDEX bot_child_successor_generation ON bot_child_resumes(run_id,consumer_generation);
CREATE UNIQUE INDEX bot_child_consumer_turn ON bot_child_resumes(run_id,consumer_attempt_id,accepted_turn_id) WHERE accepted_turn_id IS NOT NULL;
CREATE TRIGGER bot_child_resumes_immutable BEFORE UPDATE ON bot_child_resumes
WHEN NEW.wait_id IS NOT OLD.wait_id OR NEW.run_id IS NOT OLD.run_id
 OR NEW.origin_attempt_id IS NOT OLD.origin_attempt_id OR NEW.origin_generation IS NOT OLD.origin_generation
 OR NEW.origin_session_id IS NOT OLD.origin_session_id OR NEW.consumer_attempt_id IS NOT OLD.consumer_attempt_id
 OR NEW.consumer_generation IS NOT OLD.consumer_generation OR NEW.session_creation_key IS NOT OLD.session_creation_key
 OR NEW.input_key IS NOT OLD.input_key OR NEW.input_fingerprint IS NOT OLD.input_fingerprint OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR (OLD.consumer_session_id IS NOT NULL AND NEW.consumer_session_id IS NOT OLD.consumer_session_id)
 OR (OLD.accepted_turn_id IS NOT NULL AND (NEW.accepted_turn_id IS NOT OLD.accepted_turn_id OR NEW.receipt_json IS NOT OLD.receipt_json))
 OR (OLD.state='closed' AND NEW.state IS NOT OLD.state)
BEGIN SELECT RAISE(ABORT,'immutable Bot child resume'); END;
CREATE TRIGGER bot_child_resumes_retain BEFORE DELETE ON bot_child_resumes
BEGIN SELECT RAISE(ABORT,'retained Bot child resume'); END;

-- F. The Store version id of a saved result. Existing rows read as null.
ALTER TABLE native_result_intents ADD COLUMN verified_version_id TEXT
  CHECK(verified_version_id IS NULL OR
    (typeof(verified_version_id)='text' AND length(verified_version_id)=36
     AND stage IN ('verified','registered')));

-- G. Prove the rows survived, column for column.
CREATE TEMP TABLE m6_rows_guard (ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO m6_rows_guard SELECT
  (SELECT count(*) FROM bot_wait_runtime)=(SELECT count(*) FROM m6_wait_hold);
INSERT INTO m6_rows_guard SELECT
  (SELECT count(*) FROM bot_decision_publications)=(SELECT count(*) FROM m6_publication_hold);
INSERT INTO m6_rows_guard SELECT NOT EXISTS(
  SELECT * FROM m6_wait_hold EXCEPT SELECT
    wait_id,run_id,decision_id,user_scope,device_id,bot_id,attempt_id,generation,
    session_id,turn_id,observed_sequence,reason,phase,revision,
    ai_cleanup_receipt_json,release_evidence_json,cleanup_evidence_json,
    created_at_ms,updated_at_ms,settled_at_ms,ai_enrollment_request_json
  FROM bot_wait_runtime);
INSERT INTO m6_rows_guard SELECT NOT EXISTS(
  SELECT * FROM m6_publication_hold EXCEPT SELECT
    transition_id,run_id,wait_id,decision_id,run_sequence,decision_revision,
    workspace_id,document_id,source_schema,subject_digest,projection_kind,
    target_json,target_digest,state,base_version,result_version,prepared_delta,
    delta_digest,superseded_by_sequence,observed_version,created_at_ms,completed_at_ms
  FROM bot_decision_publications);
DROP TABLE m6_rows_guard;
DROP TABLE m6_wait_hold;
DROP TABLE m6_publication_hold;

-- H. The closing assertion, as 029 ends.
CREATE TEMP TABLE m6_fk_guard (violations INTEGER NOT NULL CHECK(violations=0));
INSERT INTO m6_fk_guard SELECT count(*) FROM pragma_foreign_key_check;
DROP TABLE m6_fk_guard;
