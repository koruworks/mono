# Read-only Cloud document inventory

`GET /v1/account/cloud-usage/workspaces/{workspace_id}/documents` is an account
session read of one owned, currently retained Workspace incarnation. Supply
required `incarnation_id`, optional `limit` (1–100, default 100), and optional
`cursor` (at most 2048 base64url characters). The account comes from the session;
an MCP/app-connection credential and a shared member's content permission do
not grant this owner-wide inventory. The desktop gateway does not serve it.

The [account OpenAPI](koru-account.openapi.json) defines the exact closed shape.
The `koru-account-client` method `cloud_usage_documents` reads one bounded page
on the account worker, away from interactive threads. It rejects a response for
another Workspace/incarnation, too many rows or a non-advancing cursor. The
client does not fetch all pages automatically.

The census is the authoritative durable Cloud unit set, including unfiled,
unavailable and deleting retained units. It is not a document manifest listing.
Only an initializing unit without a committed head is omitted. The wire also
keeps structural units so unsupported structural evidence cannot silently vanish;
`unit_role` alone never proves a supported schema or zero bytes.

Each row names its exact Cloud unit, DocumentId when recognized, source head,
nullable authorized label, membership classification and file usage. Strict
current manifest evidence establishes `listed`, inherited `trash`, or `unfiled`.
A missing, unsupported or incomplete manifest produces `unknown` classification,
not unfiled. A null label is not proof the content is unnamed. A source copy left
by a same-ID transfer stays scoped to its source Workspace/incarnation; never
open or adopt it through a global DocumentId lookup.

Every byte count, revision and evaluation time is an exact canonical nonnegative
decimal string in signed-i64 range. Known per-unit bytes count distinct currently
mapped plaintext file references in that unit, including retained Trash. A valid
reference without a durable blob association contributes zero; publication of
that association changes usage even if the document head is unchanged. Invalid
existing mappings or incomplete evidence produce unknown bytes. Before an active
accounting generation, usage is building. Unknown and building never imply zero.

`row_totals: "non_additive"` is deliberate: two documents may reference the same
blob. Do not add row values into a Workspace/account total or promise that
removal will free that amount. Use the coherent account/Workspace read for those
totals. Recoverable deleted-page bodies and their retained file references stay
counted; ordinary file editing changes the authoritative amount after Cloud
confirmation. Inventory grants no write or cleanup authority and does not clear
recoverable bodies, redefine permanent deletion or create a purge-job format.

The cursor binds account, Workspace/incarnation, generation, account revision,
Workspace source revision, complete inventory digest, manifest proof epoch,
limit and last unit key. `400 invalid_usage_cursor` rejects malformed/foreign
bindings; `409 usage_revision_changed` requires discarding pages and restarting.
Never concatenate different epochs. A cursor grants no permission or retention
window. Keep IDs/head digests out of ordinary user-facing labels and telemetry.

Responses use `Cache-Control: private, no-store`. `401 unauthenticated` clears
account state, `404 workspace_not_found` reveals no foreign ownership, and
`503 server_unavailable` covers a disabled rollout gate or unavailable read.
An unavailable manifest can still return authoritative rows with explicit
unknown classification. The implementation must capture and revalidate current
session/owner/source evidence around bounded isolated physical inspection;
no database transaction may wait for replay or subprocess admission.
