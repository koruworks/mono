ALTER TABLE native_result_intents
  ADD COLUMN effect_purpose TEXT NOT NULL DEFAULT 'result'
  CHECK(typeof(effect_purpose) = 'text' AND effect_purpose IN ('result','memory'));
ALTER TABLE native_result_intents
  ADD COLUMN memory_authority_json TEXT
  CHECK((effect_purpose = 'result' AND memory_authority_json IS NULL) OR
    (effect_purpose = 'memory' AND kind = 'edit_page' AND
     memory_authority_json IS NOT NULL AND typeof(memory_authority_json) = 'text' AND
     json_valid(memory_authority_json) AND
     length(CAST(memory_authority_json AS BLOB)) BETWEEN 1 AND 4096));

CREATE TABLE bot_memory_edit_allocations (
  run_id TEXT NOT NULL REFERENCES bot_admissions(run_id) ON DELETE RESTRICT,
  operation_id TEXT NOT NULL CHECK(length(operation_id) = 36),
  attempt_id TEXT NOT NULL REFERENCES run_attempts(attempt_id) ON DELETE RESTRICT,
  generation INTEGER NOT NULL CHECK(generation > 0 AND generation <= 4294967295),
  turn_id TEXT NOT NULL CHECK(length(turn_id) = 36),
  caller_fingerprint BLOB NOT NULL
    CHECK(typeof(caller_fingerprint) = 'blob' AND length(caller_fingerprint) = 32),
  subject_json TEXT NOT NULL CHECK(typeof(subject_json) = 'text' AND
    json_valid(subject_json) AND length(CAST(subject_json AS BLOB)) BETWEEN 1 AND 4096),
  action TEXT NOT NULL CHECK(action IN ('remember','correct','forget')),
  base_kind TEXT NOT NULL CHECK(base_kind IN ('history','original_seed')),
  requested_version TEXT CHECK(requested_version IS NULL OR length(requested_version) = 36),
  ensure_request_id TEXT UNIQUE CHECK(ensure_request_id IS NULL OR length(ensure_request_id) = 36),
  ensure_expected_revision INTEGER CHECK(ensure_expected_revision IS NULL OR ensure_expected_revision > 0),
  ensure_title TEXT CHECK(ensure_title IS NULL OR (typeof(ensure_title) = 'text' AND
    length(CAST(ensure_title AS BLOB)) BETWEEN 1 AND 256)),
  resolution_state TEXT NOT NULL CHECK(resolution_state IN ('unresolved','resolved')),
  authority_json TEXT CHECK(authority_json IS NULL OR (typeof(authority_json) = 'text' AND
    json_valid(authority_json) AND length(CAST(authority_json AS BLOB)) BETWEEN 1 AND 4096)),
  expected_version TEXT CHECK(expected_version IS NULL OR length(expected_version) = 36),
  expected_edit_revision BLOB CHECK(expected_edit_revision IS NULL OR
    (typeof(expected_edit_revision) = 'blob' AND length(expected_edit_revision) BETWEEN 1 AND 65536)),
  source_run_document_id TEXT NOT NULL CHECK(length(source_run_document_id) = 36),
  fact_node_id TEXT NOT NULL CHECK(length(fact_node_id) = 36),
  detail_node_id TEXT CHECK(detail_node_id IS NULL OR length(detail_node_id) = 36),
  source_node_id TEXT CHECK(source_node_id IS NULL OR length(source_node_id) = 36),
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY(run_id, operation_id),
  FOREIGN KEY(run_id, generation) REFERENCES run_attempts(run_id, generation) ON DELETE RESTRICT,
  CHECK((action = 'forget' AND detail_node_id IS NULL AND source_node_id IS NULL) OR
    (action IN ('remember','correct') AND detail_node_id IS NOT NULL AND source_node_id IS NOT NULL)),
  CHECK((base_kind = 'history' AND requested_version IS NOT NULL AND
    ensure_request_id IS NULL AND ensure_expected_revision IS NULL AND ensure_title IS NULL) OR
    (base_kind = 'original_seed' AND action = 'remember' AND requested_version IS NULL AND
     ensure_request_id IS NOT NULL AND ensure_expected_revision IS NOT NULL AND ensure_title IS NOT NULL)),
  CHECK((resolution_state = 'unresolved' AND authority_json IS NULL AND
    expected_version IS NULL AND expected_edit_revision IS NULL) OR
    (resolution_state = 'resolved' AND authority_json IS NOT NULL AND
     expected_version IS NOT NULL AND expected_edit_revision IS NOT NULL)),
  CHECK(resolution_state <> 'resolved' OR base_kind <> 'history' OR expected_version = requested_version)
);
CREATE INDEX bot_memory_edit_allocation_resolution
  ON bot_memory_edit_allocations(run_id, resolution_state, operation_id);
