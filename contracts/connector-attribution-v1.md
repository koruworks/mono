# Connector attribution v1 — `actor/1`

**Status: draft contract, approved to proceed on 2026-11-06; the maintainer's
review of this text is still owed.** Nothing writes it yet. It is a new
persisted convention: additive, and ignored by every reader that predates it.

Every Loro commit that an app connection makes — an AI assistant editing a
synced workspace through Koru's hosted MCP server, first — carries one compact
JSON object as the change's **commit message**. It says that an assistant made
the change, through which connection, and in which call. It travels with the
operations to every device and collaborator, and survives Cloud checkpoints,
which keep history.

## The message

```json
{"koru":"actor/1","kind":"assistant","connection":"6f1c0b52-3a43-4a8e-9a43-0d7f4c1e2b9a","name":"Claude – work","client":"https://claude.ai/oauth/client.json","verified":true,"account":"0b6c7e1d-58a2-4f60-9c1a-5d2e8f3b7a10","op":"5e9d2c41-7b3f-4a6e-8d15-2c9f0e7a4b63"}
```

A creation's seed commit adds `create`:

```json
{"koru":"actor/1","kind":"assistant","connection":"…","name":"…","client":"…","verified":false,"account":"…","op":"…","create":{"request_hash":"9f2c…64 hex…"}}
```

| Property | Type | Meaning |
| --- | --- | --- |
| `koru` | `"actor/1"` | The format and its version. A message without exactly this value is not `actor/1`. |
| `kind` | `"assistant"` | What made the change. `assistant` is the only v1 value. |
| `connection` | UUID, lowercase hyphenated v4 | The app connection the change was made through. |
| `name` | string, 1–64 characters | The connection's name **when the change was made**. A later rename does not rewrite history. |
| `client` | string | The OAuth client id of the application: the https URL of its client metadata document, or `dcr:<uuid>` for a dynamically registered client. |
| `verified` | boolean | `true` only for a metadata-document client, whose `client` URL's host is its verified domain. |
| `account` | UUID, lowercase hyphenated v4 | The Koru account the connection belongs to. |
| `op` | UUID, lowercase hyphenated v4 | The mutating tool call this change belongs to: every commit one call makes carries the same `op`. |
| `create` | object, optional | Only on a created document's seed commit. |
| `create.request_hash` | string, 64 lowercase hex characters | The tool layer's existing creation request hash: BLAKE3 of the normalized semantic `create_document` request, the idempotency key excluded (`create_request_hash` in `koru-mcp-server`). It is how a retried creation recognises its own first attempt from Cloud's history alone. |

**Encoding.** UTF-8 JSON with no insignificant whitespace, the properties in
the order above, `create` last when present. Writers produce exactly this
spelling; readers do not depend on it.

**Never in it:** document content, titles or names other than the
connection's own `name`; the agent's self-reported `clientInfo`; a claim
handle or the claim reference an upload carries; any credential.

## Which commits carry it

Every commit made under a connection's context, enumerated rather than
assumed:

- the edited document, including task changes made through an edit;
- a created document's seed commit, and the manifest entry the Store commits
  for it;
- an extraction's destination seed, its manifest entry, the moved content in
  the destination, and the source's replacement;
- any system-document commit an edit causes, such as creating a tag.

Commits Koru's own devices, local MCP, Koru AI and delegated runs make carry
no message and are unchanged by this contract.

## Reading it

- **Ignore a message whose `koru` is not exactly `actor/1`**, including a
  message that is not JSON at all. Commit messages are free text to Loro.
- **Ignore unknown properties.** A later minor addition is a new optional
  property, never a changed meaning; a changed meaning is `actor/2`.
- **It is a self-description, not provenance.** The replica that made the
  commit wrote it, and any Loro peer could write the same bytes. No reader may
  present `actor/1` as verified authorship. Authenticity needs a provenance
  the Cloud worker records per accepted change, or a signature; both are
  later work, and either will be its own contract.

## Compatibility

Builds that predate this contract ignore commit messages and see an
assistant's change as an ordinary change. No document, Store or Cloud object
schema changes, and no `schema_version` moves. Loro keeps a change's commit
message in its history-retaining snapshots and updates; Cloud checkpoints are
history-retaining by design.

## Evidence owed

- every enumerated commit carries the message, asserted by a test that walks
  every commit made under a connection context;
- no Koru path already writes commit messages;
- the message survives a Loro history-retaining snapshot, and a Cloud
  checkpoint export and import, unchanged.
