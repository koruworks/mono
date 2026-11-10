CREATE TABLE document_creation_origins (
  document_id TEXT PRIMARY KEY NOT NULL REFERENCES loro_documents(id) ON DELETE CASCADE,
  scope TEXT NOT NULL,
  request_key TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  initial_update BLOB NOT NULL,
  created_at INTEGER NOT NULL
);
INSERT INTO document_creation_origins
  (document_id, scope, request_key, request_hash, initial_update, created_at)
SELECT c.document_id, c.scope, c.request_key, c.request_hash, c.initial_update, c.created_at
FROM document_creations c JOIN loro_documents d ON d.id = c.document_id
WHERE d.kind = 'document'
  AND (SELECT count(*) FROM document_creations other WHERE other.document_id = c.document_id) = 1
  AND typeof(c.scope) = 'text'
  AND typeof(c.request_key) = 'text' AND length(CAST(c.request_key AS BLOB)) BETWEEN 1 AND 128
  AND typeof(c.request_hash) = 'text' AND length(CAST(c.request_hash AS BLOB)) BETWEEN 1 AND 128
  AND typeof(c.initial_update) = 'blob' AND typeof(c.created_at) = 'integer';
