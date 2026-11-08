# Hosted tools v1 — effects, outcomes and the hosted catalog

**Status: draft contract, approved to proceed on 2026-11-06; the maintainer's
review of this text is still owed.** Implemented by `koru-mcp-server` (Cloud MCP
Plan 2.1) and the hosted MCP server (Plan 2.3). It fixes what
`koru-mcp-server`'s mutating tools answer when a save cannot be confirmed,
and what the tool catalog adds when Koru's hosted MCP server
(`mcp.koru.do`) serves it. The local catalog (`koru-mcp serve --stdio`) keeps
every input schema it has today.

The tools keep one implementation. The hosted server composes them over an
in-memory replica of a Cloud workspace, where a write is local first and
durable only when Cloud acknowledges it, so it has to say which writes Cloud
holds. Everything below is the vocabulary for saying that.

## 1. Effects

**An effect is one submission: one persist of one unit.** A mutating call
makes its effects in this order, and stops at the first that does not end
`saved`:

| Tool and path | Effects, in submission order |
| --- | --- |
| `edit_document` | `system` (the workspace system document: tags or definitions the batch creates), only when the batch creates any; then `document` (the body, task changes and any id stamps) |
| `create_document` | `document` (the seed, id stamps included); then `manifest` (its entry) |
| `extract_to_document` into a **new** destination | `destination_seed`, `manifest`, `destination` (the moved content), `source` (the replacement) |
| `extract_to_document` into an **existing** destination | `destination` (id stamps and the moved content), `source` |

Every check runs before the first effect: readability, `base_revision`,
preconditions, the validation fork, capabilities and claims (§4). A call
refused by any of them has no effect, locally as well as hosted. Id stamps a
document still needs ride in that unit's next effect; they are never a write
of their own.

**Each effect ends in exactly one state:**

| State | Meaning |
| --- | --- |
| `saved` | Durably acknowledged. Locally: the Store acknowledged the persist. Hosted: Cloud's acknowledgement covers the unit's frontier after it. |
| `not_saved` | **Proven** to have no effect in Cloud. It was refused before submission (admission, a claim, validation, the replica's generation fence), or the Cloud worker refused it with one of the two refusals defined to leave the unit's head unmoved: `sync_unit_exists` and `sync_document_claimed`. |
| `unknown` | Submitted, then not acknowledged within 15 seconds, or the session dropped after submission. Any refusal other than the two above, after submission, is also `unknown`: there is no general "no durable effect" guarantee. |

`not_saved` is a statement about Cloud. A local host reports `unknown` only
when its Store did not acknowledge within its wait (Cloud MCP Q2.1-2).

An effect is reported as:

```json
{ "effect": "document", "document_id": "<uuid>", "state": "saved" }
```

`effect` is one of the names in the table above; `document_id` is the unit's
document (the workspace's system document or manifest for `system` and
`manifest`).

## 2. What a mutating call answers

| Effects | Answer |
| --- | --- |
| All `saved` | Success, with the tool's result as today. |
| All `not_saved` | The refusal's own code where it has one (`revision_conflict`, `precondition_failed` and the other validation codes; `document_claimed`; `authorization_revoked`, `cloud_subscription_ended`, `authorization_unavailable`), otherwise `edit_not_saved`. Nothing was saved and a retry is safe once the cause is gone. |
| A `saved` prefix, the rest `not_saved` | `partially_saved` |
| Any `unknown` | `save_status_unknown` |

The three envelope codes are tool errors (`{ code, message, details }` under
`isError`), and their `details` always carry:

```json
{ "operation_id": "<uuid>", "effects": [ { "effect": "…", "document_id": "…", "state": "…" } ] }
```

listing every effect the call planned, the saved ones included.

**`partially_saved`.** A refusal stops the sequence, so a mixed outcome is
always a saved prefix. Its `message` is the tool's recovery sentence, and its
`details` add the tool's recovery fields. A blanket rollback is never claimed.

| Saved prefix | State of the workspace | Message, and added details |
| --- | --- | --- |
| edit: `system` | the named tags exist, unused; the document is unchanged | "The tags were created; the edit was not saved. Retrying the edit is safe: existing tags are reused." |
| create: `document` | the document exists and is not in the workspace's list | "The document was saved but is not listed yet. Retry with the same `idempotency_key` to finish." Details add `idempotency_key`. |
| extract, new: `destination_seed` | an empty, unlisted page; the source unchanged | extraction's existing recovery fields: `created_document_id`, `target_document_id`, `source_document_id`, `source_unchanged: true`, `recovery_required: true` |
| extract, new: `destination_seed`, `manifest` | an empty, listed page; the source unchanged | the same fields: move the content again into `target_document_id`, or delete the page |
| extract: … `destination` | the content is in both documents | the same fields, `source_unchanged: true`: retry the source replacement with an edit, or remove the copy |

**`save_status_unknown`.** Its `message` is exactly:

> Koru could not confirm whether this change was saved. Read the documents
> listed before retrying; if your change is there, do not repeat it.
> `create_document` with the same `idempotency_key` is always safe to retry.

**`operation_id`** is a UUID minted for every mutating call. It is written
into every commit the call makes (`op` in
[connector-attribution-v1](connector-attribution-v1.md)), so the operation can
be found in Cloud's history after the replica that made it is gone. There is
no tool that looks one up; the assistant checks content, as the message says.

## 3. What the hosted server changes

- **`create_document` always has a key.** When the caller sends no
  `idempotency_key`, the hosted server mints a v4 UUID and returns it as
  `idempotency_key` in every answer, success included, so a partial or unknown
  outcome can always be finished by a retry. The local catalog is unchanged.
- **No history captures.** The `mcp_before`/`mcp_after` captures are
  switched off: history is device-local and a replica is in memory. Hosted
  answers carry no `history` field.
- **No task-log delivery** (`tasks.deliver`): a replica has nowhere durable to
  keep it.
- **The catalog is the capability set.** Rung 1 serves
  `get_documentation`, `list_workspaces`, `list_documents`, `search_documents`,
  `read_document`, `create_document`, `edit_document`, `create_image_upload`,
  `add_image`, `replace_image`, `extract_to_document`,
  `list_tasks`, `claim_document` and `release_document`. Capabilities are
  checked before scope and before any backend call, and the published catalog
  is built from the same set, so any other tool called by name answers
  `tool_not_available` with no backend work.

### Image uploads

`create_image_upload`, `add_image` and `replace_image` are in this catalog.
They do not accept image bytes. `create_image_upload` returns `upload_id`,
`expires_at` and `upload.url` / `upload.headers.authorization`
(`Upload <token>`, ten minutes). The uploader `PUT`s the raw file to that
URL. `add_image` inserts the completed upload; `replace_image` replaces one
existing image's bytes. Both take `claim_ids` like `edit_document`.

The byte route is not implemented by an in-process replica in this
repository. Until the hosted worker mounts `PUT /v1/image-uploads/{upload_id}`
and implements blob storage for that route, `create_image_upload` answers
`image_upload_unavailable` and the place tools answer
`image_upload_unavailable` or `claims_unavailable`. Neither writes a
document. A caller-supplied hash is not a reference. The server does not
fetch URLs.

Place refusals that must not mutate the document: `upload_not_found`,
`upload_not_complete`, `upload_expired`, `upload_conflict`,
`upload_workspace_mismatch`, `permission_denied` (another principal),
`revision_conflict`, `duplicate_node_id`, `document_claimed`,
`claims_unavailable`, `invalid_request` (not an image, too large, bad
caption). A retry whose `node_id` already shows that upload's hash answers
success with `replayed: true` and does not insert a second block.

`mcp.tool.called`'s `tool` vocabulary includes the three image tools. Its
`code` vocabulary is capped at 32 reviewed tokens, so the image-upload
refusals above that are not already in that list are recorded as `other`.
The tool result still carries the specific code.

## 4. Claims

A claim is an exclusive, server-held, expiring lease on one document for one
hosted agent. Other hosted agents' changes to that document are refused while
it lives — on other connections, and on the same connection without the
handle. People on their devices, local MCP, Koru AI and delegated runs are
never blocked by a claim and never see one refuse them.

### `claim_document`

Requires Read & edit. Input:

| Property | Type | |
| --- | --- | --- |
| `workspace_id` | UUID | required |
| `document_id` | UUID | required |
| `claim_id` | string | optional: an existing handle, to **renew** that claim |
| `purpose` | string, 1–120 characters | optional, self-declared, shown as such |
| `agent_label` | string, 1–64 characters | optional, self-declared, shown as such |
| `duration_seconds` | integer, 60–900 | optional, default 300 |

Result: `{ "claim_id": "<handle>", "expires_at": <ms>, "holder": <holder> }`.
`claim_id` is an opaque, server-minted handle; it is the bearer of the claim,
and the server keeps only its SHA-256. `expires_at` is Unix milliseconds,
like every other time in tool results. Renewing keeps the handle.

`holder` is `{ "connection_name", "client", "verified", "agent_label" }`:
the holding connection's name, its client (the verified domain, or the
client's own name when unverified), whether it is verified, and the
self-declared label, `null` when none was given.

### `release_document`

Input `{ "claim_id": "<handle>" }`; result `{ "claim_id": "<handle>" }`.
Idempotent: releasing a claim that is already released or expired answers the
same.

### Handles on mutating calls

The hosted `edit_document`, `extract_to_document`, `add_image` and
`replace_image` gain an optional `claim_ids: string[]`, at most 2 items. Each handle names its own document on
the server, so an extraction whose source and destination are claimed
separately presents both. A handle for a document the call does not change is
ignored.

The documents a call changes are its **targets**: `edit_document`'s document;
`add_image`'s and `replace_image`'s document;
`extract_to_document`'s source and, when it extracts into one, its existing
destination. A new document is never a target. Before the first effect, every
target under a live claim needs that claim's handle; the server checks again
as Cloud accepts each effect, so a claim taken while a call runs stops the
effects not yet accepted, and the saved prefix is reported as §2 says.

### Refusals

| Code | When | `details` |
| --- | --- | --- |
| `document_claimed` | a target is under another live claim, or the handle is missing or wrong; `claim_document` on a document someone else holds | `{ "document_id", "holder": <holder>, "purpose", "expires_at", "retry_after_seconds" }` |
| `claim_not_found` | `claim_document` renewing a handle the server does not hold | — |
| `claim_expired` | renewing a handle whose claim has expired | — |
| `claim_limit_reached` | the connection already holds 16 live claims | — |
| `workspace_not_authorized` | the document is outside the connection's allowed workspaces | as today |

`document_id` is the claimed document: the target the refused call would have
changed, or the one `claim_document` asked for, so an extraction says whether
its source or its destination is claimed (an additive erratum, Cloud MCP
Q3.4-4).

`document_claimed`'s message tells the assistant to wait until `expires_at`
or retry later, and not to work around the claim.

### Claims shown on reads

The hosted `read_document` result, and each entry of the hosted
`list_documents` result, add `claim: { "holder": <holder>, "purpose",
"expires_at" }` for a document under a live claim, and omit it otherwise.

### The boundary, stated in the tool description

A claim stops every hosted agent's change that Cloud accepts after the claim
is granted. It does not stop people on devices, local MCP, Koru AI or
delegated runs; the CRDT keeps all of those safe to merge.

## 5. Other hosted answers

Listed so this contract is whole; each is the hosted server's, never the
local one's.

| Code or field | When |
| --- | --- |
| `workspace_loading` (`details: { documents_settled, documents_total }`) | the replica is still loading after 30 seconds; a retry is safe |
| `cloud_unavailable` | the replica's Cloud session is down or has lost liveness; nothing is served from memory as current |
| `document_newer_than_host` | the document's `schema_version` is newer than the hosted server's; it is read-only |
| `idempotency_conflict` | the same `idempotency_key` with a different request; nothing is created |
| `idempotency_key_retired` | the key's document was deleted; use a new key |
| `list_workspaces` entry `name`, `profile_id`, `profile_name`, `name_status` | `name_status` is `loading` while the names have not arrived (with `name: null`), `unavailable` when Cloud cannot serve them, absent otherwise |
