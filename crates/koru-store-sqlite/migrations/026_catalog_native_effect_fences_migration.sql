-- catalog.db 13 -> 14: local native operation liveness, never result proof.
-- No content foreign keys: deny fences survive target/workspace purge.
CREATE TABLE native_effect_fences (
  user_scope_id TEXT NOT NULL,
  scope TEXT NOT NULL CHECK(length(CAST(scope AS BLOB)) BETWEEN 1 AND 256),
  request_key TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  document_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('create_document','edit_document')),
  request_fingerprint BLOB NOT NULL CHECK(length(request_fingerprint)=32),
  fenced_at_ms INTEGER NOT NULL,
  drained_at_ms INTEGER,
  PRIMARY KEY(user_scope_id, scope, request_key),
  CHECK(substr(request_key,15,1)='4' AND substr(request_key,20,1) IN ('8','9','a','b')),
  CHECK(length(user_scope_id) = 36 AND substr(user_scope_id,9,1)='-' AND substr(user_scope_id,14,1)='-' AND substr(user_scope_id,19,1)='-' AND substr(user_scope_id,24,1)='-' AND length(replace(user_scope_id,'-',''))=32 AND user_scope_id NOT GLOB '*[^0-9a-f-]*' AND user_scope_id<>'00000000-0000-0000-0000-000000000000'),
  CHECK(length(request_key) = 36 AND substr(request_key,9,1)='-' AND substr(request_key,14,1)='-' AND substr(request_key,19,1)='-' AND substr(request_key,24,1)='-' AND length(replace(request_key,'-',''))=32 AND request_key NOT GLOB '*[^0-9a-f-]*' AND request_key<>'00000000-0000-0000-0000-000000000000'),
  CHECK(length(workspace_id) = 36 AND substr(workspace_id,9,1)='-' AND substr(workspace_id,14,1)='-' AND substr(workspace_id,19,1)='-' AND substr(workspace_id,24,1)='-' AND length(replace(workspace_id,'-',''))=32 AND workspace_id NOT GLOB '*[^0-9a-f-]*' AND workspace_id<>'00000000-0000-0000-0000-000000000000'),
  CHECK(length(document_id) = 36 AND substr(document_id,9,1)='-' AND substr(document_id,14,1)='-' AND substr(document_id,19,1)='-' AND substr(document_id,24,1)='-' AND length(replace(document_id,'-',''))=32 AND document_id NOT GLOB '*[^0-9a-f-]*' AND document_id<>'00000000-0000-0000-0000-000000000000')
);
