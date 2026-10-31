# Koru API reference

Generated from the route catalog. Do not edit.

## POST `/v1/search`

Search Store content and finalized AI chats concurrently, retaining typed source identity and partial-failure status.

Returns: A deterministic federated result page and source statuses.

## POST `/v1/store/search`

Search only Store content through the public gateway.

Returns: The Store search response.

## GET `/v1/store/status`

Read the Store's own fine-grained capability list through the public gateway.

Returns: The Store status response.

## GET `/v1/status`

What this store is and what it can do

Returns: api_version, capabilities, channel, started_at

## GET `/v1/openapi.json`

This document

Returns: an OpenAPI 3.1 document

## POST `/v1/shutdown`

Ask the store to stop; ?only_if_idle=true stops only when nothing is connected

Returns: stopping: whether it is going down

## POST `/v1/backup`

Write a consistent whole-Store backup

Returns: the destination after the backup is complete

## GET `/v1/profiles`

Every profile

Returns: an array of profiles

## POST `/v1/profiles`

Create a profile. Requires no account and no network.

Returns: the profile, with its new id

## PUT `/v1/profiles/{id}/account`

Atomically bind a Profile to an exact hosted Koru Identity

Returns: bound or already_bound; a compare-and-set mismatch never overwrites

## DELETE `/v1/profiles/{id}/account`

Atomically detach a hosted-deleted Profile for offline-only use

Returns: cleared or already_offline; local Profile content is retained

## DELETE `/v1/profiles/{id}/local`

Mark an empty profile removed in this installation's Store catalog

Returns: removed or already_removed; profile and deleted workspace data remain

## PUT `/v1/profiles/{id}/local`

Restore a locally removed Profile from retained Store data

Returns: restored or already_active; workspace tombstones remain unchanged

## GET `/v1/profiles/{id}`

One profile

Returns: a profile

## PATCH `/v1/profiles/{id}`

Rename a profile in its replicated manifest

Returns: the profile, renamed

## PATCH `/v1/profiles/{id}/appearance`

Set a profile's switcher icon and named colour

Returns: the profile with both appearance fields

## GET `/v1/profiles/{id}/personal-attributes`

One profile's personal attributes for one entity, named by ?entity=<kind>:<workspace>:<document>:<node>

Returns: the entity reference and its attributes for this profile only. An unset attribute is an absent key, never a null. The reference travels as a query parameter because it holds only ids

## PUT `/v1/profiles/{id}/personal-attributes`

Set one personal attribute on one entity, or clear it with a null value

Returns: the entity's attributes for this profile after the write. Personal state belongs to the profile in the path and never overwrites another profile's answer to the same question

## GET `/v1/profiles/{id}/documents/{document}/private-state`

One reader's device-local private view state for one document

Returns: the reader's private attributes by node id — today `koru.collapsed` for a closed toggle — and a revision that grows with each change. A reader who never changed anything gets an empty map at revision 0, and the read writes nothing. Never replicated or exported

## PATCH `/v1/profiles/{id}/documents/{document}/private-state`

Apply up to 512 explicit private view-state changes for one reader and document

Returns: the reader's private state after the one atomic save. Each change names the value it wants, so a repeat is a no-op; one invalid change rejects the batch. Needs only read access: it changes this reader's view, never the document, and it never wakes sync

## GET `/v1/profiles/{id}/reminders`

Every reminder for ?entity=<canonical entity reference>

Returns: the entity's synchronized reminder rules and action history

## POST `/v1/profiles/{id}/reminders`

Create a reminder at a client-minted id

Returns: the authoritative stored reminder; an identical retry is idempotent

## GET `/v1/profiles/{id}/reminders/{reminder}`

Fetch one reminder

Returns: the synchronized rule, deletion fact and scoped actions

## GET `/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}`

Fetch one reminder

Returns: the synchronized rule, deletion fact and scoped actions

## PUT `/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}`

Replace one whole reminder rule revision

Returns: the authoritative stored reminder; path and body ids must match

## DELETE `/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}`

Monotonically delete one reminder

Returns: the reminder with its irreversible deletion fact set

## POST `/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}/pause`

Pause one reminder as a new atomic revision

Returns: the authoritative paused reminder

## POST `/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}/resume`

Resume one reminder as a new atomic revision

Returns: the authoritative enabled reminder

## POST `/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}/snooze`

Append a snooze action

Returns: the authoritative reminder; the body action must be snoozed_until

## POST `/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}/handle`

Append a handled action

Returns: the authoritative reminder; the body action must be handled

## GET `/v1/profiles/{id}/dictionary`

One profile's personal spelling dictionary, language tag to words

Returns: every language's words for this profile only. An empty dictionary is an empty map, and it replicates between this person's devices and nobody else's

## PUT `/v1/profiles/{id}/reminders/{reminder}`

Replace one whole reminder rule revision

Returns: the authoritative stored reminder; path and body ids must match

## DELETE `/v1/profiles/{id}/reminders/{reminder}`

Monotonically delete one reminder

Returns: the reminder with its irreversible deletion fact set

## POST `/v1/profiles/{id}/reminders/{reminder}/pause`

Pause one reminder as a new atomic revision

Returns: the authoritative paused reminder

## POST `/v1/profiles/{id}/reminders/{reminder}/resume`

Resume one reminder as a new atomic revision

Returns: the authoritative enabled reminder

## POST `/v1/profiles/{id}/reminders/{reminder}/snooze`

Append a snooze action

Returns: the authoritative reminder; the body action must be snoozed_until

## POST `/v1/profiles/{id}/reminders/{reminder}/handle`

Append a handled action

Returns: the authoritative reminder; the body action must be handled

## DELETE `/v1/profiles/{id}/reminder-history`

Clear handled reminder history, optionally for one entity

Returns: the number of handled action ids monotonically hidden in the requested scope; active and snoozed actions remain

## GET `/v1/profiles/{id}/reminder-settings`

Fetch reminder settings

Returns: end-of-day time and default tone

## PATCH `/v1/profiles/{id}/reminder-settings`

Update reminder settings independently

Returns: the authoritative settings; absent fields are unchanged and null clears end-of-day

## POST `/v1/profiles/{id}/reminders/query`

Query active, waiting, missed, upcoming, or handled reminders

Returns: canonical joined rows; missed/upcoming require explicit caller now and canonical IANA zone

## GET `/v1/profiles/{id}/reminders/reconciliation`

Fetch the Agent's batched reminder reconciliation snapshot

Returns: device-neutral canonical rows joined to target existence, title, due, and bin state

## PUT `/v1/profiles/{id}/dictionary`

Add or remove one personal-dictionary word for one language

Returns: the whole dictionary after the write. A removed word is a deleted key, never a stored false

## DELETE `/v1/profiles/{id}/dictionary`

Clear one profile's personal spelling dictionary

Returns: the empty dictionary after every word visible to this device is deleted

## GET `/v1/workspaces`

Every workspace

Returns: an array of workspaces

## GET `/v1/profiles/{id}/workspace-usage`

Live entry counts by kind and referenced blob bytes for every workspace in one profile

Returns: one usage row per workspace, including deduplicated live file/blob bytes

## PUT `/v1/profiles/{id}/workspace-order`

Replace a profile's complete workspace order atomically

Returns: that profile's workspaces in their saved order

## DELETE `/v1/workspaces/{id}`

Soft-delete a workspace; its database and blobs remain recoverable

Returns: the workspace with deleted_at set. Takes destroy_workspaces, which no ordinary token holds: removing a workspace closes its terminals, and the Gateway's lifecycle operation is what does that first

## POST `/v1/workspaces/{id}/restore`

Restore a soft-deleted workspace

Returns: the workspace with deleted_at cleared. The same scope the delete takes: whoever decides what the trash contains decides both ways

## POST `/v1/workspaces/{id}/move`

Give a workspace to another profile

Returns: the workspace with its new owner and its position at the end of that profile's order. Its id, its documents and its history are untouched. Takes migrate_workspaces, which no ordinary token holds: a move closes every terminal the workspace owns, and the Gateway's migration operation is what does that first

## POST `/v1/workspaces`

Create a workspace and its manifest

Returns: the workspace, with its new immutable id

## GET `/v1/workspaces/{id}`

One workspace

Returns: a workspace

## PATCH `/v1/workspaces/{id}`

Rename a workspace, in the catalog and in its manifest

Returns: the workspace, renamed

## PATCH `/v1/workspaces/{id}/appearance`

How a workspace appears in a switcher: icon, colour, pinned, archived

Returns: the workspace. All four are sent every time — there is no absent-versus-null. Changing `archived` additionally takes destroy_workspaces: archiving closes the workspace's terminals and hides its projects, and it only travels here because these four fields are edited together

## GET `/v1/workspaces/{id}/documents`

The workspace's documents, without opening any of them

Returns: an array of document entries in tree order

## GET `/v1/documents/{id}/location`

Resolve a stable document id to its owning profile and workspace

Returns: the document's current location

## GET `/v1/documents/{id}/outline`

The document's headings, from the read model, without opening it

Returns: headings in reading order, and whether the document is indexed yet

## POST `/v1/workspaces/{id}/documents`

Register a new document in the workspace

Returns: the new document id

## POST `/v1/workspaces/{id}/documents/{document}/note`

The one note a file carries, creating it when it has none

Returns: the note's document id — the existing one when the file already has a note, so the call is the answer to both Add note and Open note — and whether this call created it

## POST `/v1/workspaces/{id}/transfer`

Move a document and its explore subtree into another workspace

Returns: the document ids that moved, root first. Ids are preserved.

## GET `/v1/workspaces/{id}/manifest`

The document id a client joins to see the workspace's contents

Returns: a document id

## GET `/v1/sync`

WebSocket carrying the standard Loro Protocol

Returns: a WebSocket; the token travels on the upgrade, never in the protocol

## POST `/v1/workspaces/{id}/snapshot/{document}`

A document's whole state, for a client that has none

Returns: application/octet-stream — a Loro snapshot, ready to import

## GET `/v1/workspaces/{id}/documents/{document}/history`

A document's durable history markers, newest first

Returns: a cursor-paginated history page

## POST `/v1/workspaces/{id}/documents/{document}/history`

Capture a durable document version and append a history marker

Returns: the new history marker

## POST `/v1/workspaces/{id}/history/{entry}/label`

Rename a history marker; its immutable state is untouched

Returns: the marker with its new label

## POST `/v1/workspaces/{id}/history/{entry}/snapshot`

The checksum-verified CRDT snapshot named by a history marker

Returns: application/octet-stream — an immutable Loro snapshot

## GET `/v1/workspaces/{id}/versions/{version}/snapshot`

The checksum-verified CRDT snapshot a document version id names

Returns: application/octet-stream — an immutable Loro snapshot, with its document id, checksum and capture time on X-Koru-Document-Id, X-Koru-Checksum and X-Koru-Captured-At; `document_version_not_found` when the workspace holds no such version

## GET `/v1/workspaces/{id}/blobs`

Every blob this workspace holds

Returns: an array of blobs — hash, size, mime, filename, created_at

## PUT `/v1/workspaces/{id}/blobs`

Store bytes and get back the address they are filed under

Returns: a blob. The caller does not choose the hash: the store hashes what it receives, so storing the same bytes twice makes no second copy. The filename travels percent-encoded in X-Koru-Filename, because a filename is arbitrary Unicode and a header value is not

## GET `/v1/workspaces/{id}/blobs/{hash}`

The local or on-demand hydrated bytes behind a hash

Returns: application/octet-stream, fetched from an authorized connected peer when absent locally, then verified and persisted against the address

## POST `/v1/workspaces/{id}/blobs/{hash}`

Associate another workspace of the same Profile with one blob

Returns: the target workspace's durable blob record. No bytes are copied: one object serves every associated workspace, and the record is written before the caller publishes a CRDT reference to it

## POST `/v1/workspaces/{id}/export`

A whole workspace as one .koru bundle

Returns: application/octet-stream — manifest, a snapshot per document, then the blobs. A POST because it is work; it changes nothing

## POST `/v1/import`

Take a .koru bundle in. ?mode=restore or ?mode=duplicate, and ?owner=<profile>

Returns: the workspace. The complete bundle is buffered in memory, with no HTTP size ceiling beyond the bundle format itself. There is no default mode, deliberately: `restore` keeps the ids so the result can converge with its origin, `duplicate` mints new ones so it never will, and guessing wrong is quiet either way

## POST `/v1/attributes/query`

Which nodes carry these attributes, across workspaces, from the read model

Returns: a page of rows — each carrying workspace, document and node together, every attribute the node holds, an excerpt, and the source document's title and inherited trashed flag — plus the attribute definitions and tag tables in scope, so a filter UI builds from types. A POST because the query travels in the body: a filter can name an attribute key or a tag, and a URL is written to the access log. `group_by: "tag"` adds `groups` — a count per tag over the **whole** matching corpus rather than the page, with merged tags folded into the tag they were merged into and binned rows counted and flagged. `updating_workspaces` names any workspace the answer may be behind on rather than waiting for it

## POST `/v1/tasks/query`

What is outstanding, across workspaces, from the read model

Returns: a page of tasks — each carrying workspace, document and node together, the `task:` reference that is both its identity and the page cursor, its lifecycle, its title, the breadcrumb that says where it lives, both trash answers under their own names, and every attribute the node holds — plus the attribute definitions and tag tables in scope, so a row draws without a second request. A POST for the same reason the attribute query is one: the filter travels in the body rather than in a URL that is written to the access log. Paging is a keyset on the task reference, so `next_after` continues and `restarted` says the cursor named a row the filter no longer matches. `updating_workspaces` names any workspace the answer may be behind on rather than waiting for it

## POST `/v1/search/navigated`

Record that a search led somewhere

Returns: nothing. Machine-local and derived: it feeds recent queries and a workspace's recency, and reaches no document and no shared profile. A query earns a place in the history only by leading somewhere, which is why the text arrives here rather than on the search

## GET `/v1/ai/providers/settings`

Read non-secret AI provider settings

Returns: The selected provider, model, reasoning, endpoint, and credential-presence flag. Secrets and vault labels are never returned.

## PUT `/v1/ai/providers/selection`

Select the active AI provider

Returns: The complete persisted non-secret provider settings.

## PUT `/v1/ai/providers/openai`

Update non-secret OpenAI settings

Returns: The saved OpenAI model, reasoning choice, and credential-presence flag.

## PUT `/v1/ai/providers/ollama`

Update Ollama settings

Returns: The saved Ollama endpoint, model, and reasoning choice.

## POST `/v1/ai/providers/{provider}/validate`

Validate an AI provider and selected model

Returns: A safe provider state, available model names, and selected-model capabilities when ready.

## POST `/v1/ai/providers/{provider}/models`

List the models a local agent offers

Returns: The agent's models in its own ids, each with the agent's grouping; empty when it lists none.

## POST `/v1/ai/providers/ollama/pulls`

Pull an Ollama model with bounded progress streaming

Returns: An SSE stream beginning with the accepted model followed by normalized progress and one terminal event.

## POST `/v1/ai/providers/ollama/pulls/cancel`

Cancel the active Ollama model pull

Returns: Whether an active pull was signalled for cancellation.

## POST `/v1/ai/chats`

Create an AI chat

Returns: The durable chat created with the client-minted identity.

## GET `/v1/ai/chats`

List AI chats

Returns: Durable chats ordered by most recent activity.

## GET `/v1/ai/chats/{chat}`

Read an authoritative AI chat transcript

Returns: The chat, ordered messages, finalized blocks, and pinned document references.

## PATCH `/v1/ai/chats/{chat}`

Rename an AI chat

Returns: The durable chat with its updated title and activity timestamp.

## PATCH `/v1/ai/chats/{chat}/model`

Change the model used by an AI chat

Returns: The durable chat with its updated provider, model, reasoning default, and activity timestamp.

## PATCH `/v1/ai/chats/{chat}/privacy`

Change whether agents and connected assistants can discover a chat

Returns: The durable chat with its updated privacy flag and activity timestamp.

## PATCH `/v1/ai/chats/{chat}/folder`

Choose the folder a chat's local agent works in

Returns: The durable chat with its working folder; accepted only on an agent chat with no message yet.

## POST `/v1/ai/tools/chats/list`

List non-private AI chats for a trusted tool caller

Returns: A bounded newest-first page of non-private chat summaries.

## POST `/v1/ai/tools/chats/search`

Search non-private AI chats for a trusted tool caller

Returns: A bounded relevance-ranked page of finalized response matches from non-private chats.

## POST `/v1/ai/tools/chats/read`

Read a non-private AI chat for a trusted tool caller

Returns: A byte-bounded recent page of terminal chat messages; private and missing chats share chat_not_found.

## PUT `/v1/ai/chats/{chat}/tool-policy`

Replace a chat's workspace-tool policy

Returns: The durable Manual or Auto mode and complete selected-workspace grant set. An empty set grants no workspace access.

## GET `/v1/ai/chats/{chat}/tool-policy`

Read a chat's workspace-tool policy

Returns: The durable Manual or Auto mode and complete selected-workspace grant set.

## DELETE `/v1/ai/chats/{chat}`

Delete an AI chat while retaining anonymous usage

Returns: Whether the chat existed. Content, search rows, and chat/message identifiers are removed while text-free attempt and feedback metrics survive.

## DELETE `/v1/ai/chats`

Clear all AI chat history

Returns: The number of chats deleted. Anonymous text-free usage and feedback aggregates remain until cleared separately.

## GET `/v1/ai/usage`

Read private local AI usage

Returns: Content-free local aggregates grouped by day, surface, provider, model, effort, status, and prompt version. Missing provider usage remains unavailable.

## GET `/v1/ai/chats/{chat}/usage`

Read current-chat and all private local AI usage

Returns: Current-chat totals followed by the same content-free all-usage aggregates as the local usage report.

## DELETE `/v1/ai/usage`

Clear private local AI usage and feedback

Returns: The number of text-free attempts and feedback events deleted. Chat history is unchanged.

## POST `/v1/ai/search`

Search finalized AI chat responses

Returns: Best matching finalized block per response with exact chat, message, and block identity.

## PUT `/v1/ai/messages/{message}/feedback`

Append response feedback

Returns: The retry-safe append-only feedback event.

## POST `/v1/ai/chats/{chat}/branches`

Branch an AI chat

Returns: A durable chat containing newly identified copies through the chosen response.

## POST `/v1/ai/chats/{chat}/retries`

Retry a failed AI response

Returns: A durable fresh assistant entry and attempt, streamed without replacing failed history.

## POST `/v1/ai/chats/{chat}/exports`

Store a chat prefix as a Koru document

Returns: The durable retry-safe export operation; poll it until terminal.

## GET `/v1/ai/exports/{export}`

Read an AI document export

Returns: The authoritative pending, complete, or failed export operation.

## POST `/v1/ai/chats/{chat}/turns`

Commit and stream an AI turn

Returns: An ordered SSE stream beginning with durable acceptance, followed by normalized Koru AI events. Recover a lost stream by reading the attempt and chat.

## GET `/v1/ai/attempts/{attempt}`

Read authoritative AI attempt state

Returns: The durable attempt state used to recover after stream loss.

## POST `/v1/ai/attempts/{attempt}/cancel`

Cancel an AI attempt

Returns: The current terminal state; cancelling an already-terminal attempt is a successful no-op.

## POST `/v1/ai/tool-calls/{tool}/approve`

Approve a pending workspace-tool call

Returns: The durable content-free tool activity. Repeating the same approval is an idempotent read of that state.

## POST `/v1/ai/tool-calls/{tool}/deny`

Deny a pending workspace-tool call

Returns: The durable denied activity. The provider receives a structured permission-denied result and may continue.

## GET `/v1/artifacts/families`

The artifact families of a workspace

Returns: The families the caller may read, oldest first; retired ones only with include_retired=true.

Requires: `artifacts.read` and the `artifacts` capability.

## POST `/v1/artifacts/families`

Create an artifact family

Returns: The family. The caller mints the id; a second creation under the same id is refused with artifact_family_exists rather than made twice.

Requires: `artifacts.write` and the `artifacts` capability.

## GET `/v1/artifacts/families/{family}`

A family with its versions

Returns: The family and every version under it, by ordinal.

Requires: `artifacts.read` and the `artifacts` capability.

## POST `/v1/artifacts/families/{family}/retire`

Retire a family

Returns: The family with retired_at_ms set. Its versions stay readable; it accepts no new version. Retiring twice keeps the first mark.

Requires: `artifacts.write` and the `artifacts` capability.

## POST `/v1/artifacts/families/{family}/versions`

Register an immutable version of a family

Returns: The version, and whether an earlier call with the same registration_id had already made it. Metadata only: the blob must already be in the family's workspace (PUT /v1/workspaces/{id}/blobs), or the answer is blob_not_found. The size recorded is the Store's.

Requires: `artifacts.write` and the `artifacts` capability.

## GET `/v1/artifacts/versions/{version}`

One artifact version

Returns: The version: identity, family, ordinal, blob address, size, media type, filename, producer, registration id and derived_from.

Requires: `artifacts.read` and the `artifacts` capability.

## PATCH `/v1/artifacts/versions/{version}`

Refused: a version cannot be changed (also PUT and DELETE)

Returns: 409 artifact_version_immutable, always — the same code the database raises. Register a new version, or retire the family.

Requires: `artifacts.write` and the `artifacts` capability.

## GET `/v1/artifacts/versions/{version}/provenance`

A version's provenance

Returns: The version, every ancestor reached through derived_from (nearest first), and every run that consumed or produced any of them.

Requires: `artifacts.read` and the `artifacts` capability.

## GET `/v1/artifacts/versions/{version}/content`

A version's bytes

Returns: The Store's answer for the version's blob, forwarded as it is: application/octet-stream with the blob's media type and filename headers, or the Store's own failure. Requires store.read_documents as well.

Requires: `artifacts.read` and the `artifacts` capability.

## POST `/v1/artifacts/versions/{version}/runs`

Record that a run consumed or produced a version

Returns: The link. Idempotent on (run, version, role).

Requires: `artifacts.write` and the `artifacts` capability.

## POST `/v1/artifacts/authorities`

Issue a run's authority and its first endpoint token

Returns: The run's endpoint URL (<gateway>/v1/runs/{run}/mcp, stable across attempts) and the bearer for this attempt — shown once; a later read never returns it. The grant set is the request's, frozen for the attempt (D28). A run that already has an authority is refused with authority_exists: advance its generation instead.

Requires: `artifacts.write` and the `artifacts` capability.

## GET `/v1/artifacts/authorities/{run}`

A run's authority record

Returns: The record without its token and without its working-scope path: workspace, current generation, grants, state.

Requires: `artifacts.read` and the `artifacts` capability.

## POST `/v1/artifacts/authorities/{run}/generation`

A fresh attempt: advance the run's generation under a fresh token

Returns: The same endpoint URL and a new bearer. The previous bearer is retired and answers stale_generation from then on; the grants are re-read from the record and never widened. A generation that is not strictly newer is refused with stale_generation; a revoked run with authority_revoked.

Requires: `artifacts.write` and the `artifacts` capability.

## POST `/v1/artifacts/authorities/{run}/revoke`

Revoke a run's authority

Returns: The record marked revoked. A state change every later call at the run's endpoint reads (authority_revoked) — no callback, nothing to refresh. Idempotent.

Requires: `artifacts.write` and the `artifacts` capability.

## GET `/v1/artifacts/authorities/{run}/workspace`

Where the run worked, as this device resolved it

Returns: The absolute working folder the run's authority record holds -- the one read that carries a path back out of this store, for the one caller entitled to it: emptying the trash, which has to remove that folder when Koru made it. Behind the write scope rather than the read one, because reading it is not what it is for. Refused with authority_not_found for a run this device never started.

Requires: `artifacts.write` and the `artifacts` capability.

## POST `/v1/artifacts/authorities/{run}/forget`

The run was deleted: forget its authority

Returns: The record, its grants and its tokens are removed -- there is nothing left for them to authorize -- and one row saying the run was deleted takes their place, so a result the person chose to keep says 'a run that was deleted' rather than printing an id that resolves to nothing. Idempotent; held_authority is false, not an error, for a run that never had one.

Requires: `artifacts.write` and the `artifacts` capability.

## GET `/v1/artifacts/runs/{run}/versions`

The versions a run has registered

Returns: Every version whose producer is the run, oldest first — what the orchestrator reads at a turn boundary to decide whether the run delivered anything (D29) and to publish delegate.artifact.registered.

Requires: `artifacts.read` and the `artifacts` capability.

## GET `/v1/artifacts/unregistered`

What a workspace's runs wrote and never registered

Returns: Every run of the workspace with an authority on this device, newest first: how many versions it has registered (zero is D29's finished-having-registered-nothing), whether its .artifacts/<RunId>/ folder is still there, and the files inside it that are not results yet. A link is never followed and never listed; the scan stops at 8 levels and 500 files, and says so with truncated.

Requires: `artifacts.read` and the `artifacts` capability.

## POST `/v1/artifacts/runs/{run}/register`

Register a file the run left in its results folder (D19)

Returns: The version and its family. The path is resolved under the same register path rule the run's own tools obey — canonical, inside <run-workspace>/.artifacts/<RunId>/, no traversal, no symlink or junction, unchanged between listing and read — and refused with the same stable codes. The workspace, the family's identity and the provenance all come from the run's durable authority record, never from the request; a revoked run still answers, because revocation withdraws the agent's reach and not the person's. Absent a title, the file's last path segment names the result. Registering the same bytes twice is the same version; the same path with new bytes is the next one.

Requires: `artifacts.write` and the `artifacts` capability.

## POST `/v1/runs`

Delegate a piece of work to an agent

Returns: 202 with the run's id, its Run document and its status. The client mints the run id, so a retried request is the same run. The request is durable — intent, captured context and Run document — before this answers; the start follows on the worker.

Requires: `runs.write` and the `delegate` capability.

## POST `/v2/runs`

Preserve model selection: Delegate a piece of work to an agent

Returns: 202 with the run's id, its Run document and its status. The client mints the run id, so a retried request is the same run. The request is durable — intent, captured context and Run document — before this answers; the start follows on the worker.

Requires: `runs.write` and the `delegate` capability.

## GET `/v1/runs`

The runs of a workspace

Returns: Every run registered in the workspace, oldest first, as its header — objective, agent, scope, status, executing device, results, economics, rating.

Requires: `runs.read` and the `delegate` capability.

## GET `/v1/runs/{run}`

One run, with a window of its history

Returns: The header, what the answering device holds, the current state of every context pin, and a window of the timeline by index, oldest first (`from`, `len` up to 500). `workspace` names the workspace a run this device did not create is registered in — how a second device reads a run that reached it by sync.

Requires: `runs.read` and the `delegate` capability.

## POST `/v1/runs/{run}/cancel`

End the run

Returns: The header after the cancellation was asked for. A run mid-turn stays `running` until the provider answers the cancellation; the stream carries the end. Never a takeover — that is the session's own handover verb (D36).

Requires: `runs.write` and the `delegate` capability.

## POST `/v2/runs/{run}/resume`

Preserve model selection: Continue an interrupted run as a fresh attempt

Returns: The header after the fresh attempt began. Refused with `not_interrupted` unless the run is `interrupted`, with `attempt_live` when the earlier attempt's session is still running, with `source_in_trash` or `source_unavailable` when a source the run was given no longer resolves for anything new (D48), and with `prior_attempt_unknown` when nothing proved the earlier attempt stopped — that one needs `{"confirm": true}` from a person. The body is optional and every field defaults.

Requires: `runs.write` and the `delegate` capability.

## POST `/v1/runs/{run}/rate`

Rate a finished run

Returns: The header carrying the rating. One gesture — good or not good — with an optional note that lives in the run's own content and reaches no event and no metric (D44). Refused with `wrong_status` while the run is still working.

Requires: `runs.write` and the `delegate` capability.

## POST `/v1/runs/{run}/trash`

Put a run in the trash

Returns: The header and the Run document. **This is half of the gesture**: a run is a document of the workspace's manifest and putting it in the trash is that document's move into the workspace's bin, which is the caller's to make -- reversible, synced, and exactly what happens to every other document in Koru. What this does is the half only the worker can: refuse a run that is still working with `wrong_status` (cancel it first -- trashing something still writing into a folder is the one case where the reversible operation is not), and remember what the person said should happen to the run's results when the trash is emptied. Nothing on disk and nothing in the artifact store changes. The body is optional and its one field defaults to `keep`.

Requires: `runs.write` and the `delegate` capability.

## POST `/v1/runs/{run}/restore`

Take a run back out of the trash

Returns: The header and the Run document, which the caller moves back out of the workspace's bin. Nothing was deleted, so the run comes back whole; what this forgets is the decision about its results.

Requires: `runs.write` and the `delegate` capability.

## POST `/v1/runs/{run}/purge`

The trash was emptied: delete the run

Returns: Everything this device holds about the run -- its ledger rows, its authority record, and its working folder **when Koru made that folder**. A run scoped to a project ran inside the person's own repository, and that directory is left exactly alone: `folder` says `kept`, `removed` or `absent`, because a trash that quietly did less than it offered teaches somebody that it did more. `artifacts_retired` is how many result families went, which is none unless the person asked for them to go with the run. Refused with `run_folder_not_removed` when the filesystem would not remove a folder Koru made -- nothing is changed and the run stays in the trash. The Run document is the caller's to remove once this has answered.

Requires: `runs.write` and the `delegate` capability.

## GET `/v1/runs/{run}/stream`

Follow a run's history and status

Returns: text/event-stream of `run_stream_item` events: every history entry after `after` with its index, then live entries as they are written, every status change, and `closed` when the run ended. `workspace` names the workspace a run this device did not create is registered in.

Requires: `runs.read` and the `delegate` capability.

## POST `/v1/schedules/pause`

Pause a schedule

Returns: 200 with the canonical paused schedule projection. A claimed Run is not cancelled.

Requires: `runs.write` and the `delegate` capability.

## POST `/v1/schedules/resume`

Resume a schedule

Returns: 200 with the canonical active schedule projection. Elapsed paused occurrences are not caught up.

Requires: `runs.write` and the `delegate` capability.

## POST `/v1/schedules/retire`

Delete a schedule

Returns: 200 with the canonical retired schedule projection. Existing Runs and their provenance remain unchanged.

Requires: `runs.write` and the `delegate` capability.

## POST `/v1/schedules/edit`

Edit a schedule

Returns: 200 with the canonical schedule projection after one durable revision-checked replacement of its editable definition and name.

Requires: `runs.write` and the `delegate` capability.

## POST `/v1/schedules`

Create repeating delegated work

Returns: 201 with the canonical durable Schedule document projection. The client supplies the ScheduleId, workspace, reusable delegation template and recurrence rule; the worker stamps owner, revision, lifecycle and times.

Requires: `runs.write` and the `delegate` capability.

## POST `/v1/schedules/rename`

Rename a schedule

Returns: 200 with the canonical schedule projection after its durable owner-authorized name change.

Requires: `runs.write` and the `delegate` capability.

## GET `/v1/schedules`

The schedules of a workspace

Returns: Canonical definitions, reduced ownership/lifecycle state, truthful next-firing projections, and firing history. A firing carries a RunId only when it actually fired.

Requires: `runs.read` and the `delegate` capability.

## GET `/v1/schedules/stream`

Watch a schedule list for changes

Returns: A bounded text/event-stream of content-free schedule generations. Each item means to read the workspace list again; no objective, context, path, document id or schedule id travels in the event.

Requires: `runs.read` and the `delegate` capability.

## GET `/v1/settings/{section}`

Read one settings section

Returns: The typed section: global, ai, or terminal. Secrets are never returned.

Requires: `settings.read`.

## PATCH `/v1/settings/{section}`

Change named fields of one settings section

Returns: The section as it stands after the change, read back from the authority.

Requires: `settings.write`.

## POST `/v1/workspaces/{id}/lifecycle/confirmation`

Compute what an action would close, and issue the token that proves it was shown

Returns: The consequence — the terminals this action would close, named — and a single-use token bound to this workspace and this action. The caller presents the consequence, and sends the token back in the lifecycle request if the person accepts. The same scope as the operation itself: computing what would be destroyed is not a smaller permission than destroying it.

Requires: `workspace.destroy`.

## POST `/v1/workspaces/{id}/lifecycle`

Archive, soft-delete or restore a workspace, closing its terminals first

Returns: What the operation did: the sessions it closed, whether they all closed, and whether the catalog mutation committed. A refusal carrying workspace_lifecycle_incomplete with sessions_closed: true and mutation_applied: false is a partial failure — the terminals are gone and the catalog is unchanged — and the same request again is the retry, which needs a fresh confirmation because the first one was spent. Asking for a state the workspace is already in succeeds and closes nothing.

Requires: `workspace.destroy`.

## POST `/v1/workspaces/{id}/migration/confirmation`

Compute what moving a workspace to a profile would close, and issue the token that proves it was shown

Returns: Both ends of the move — the profile that owns the workspace and the one it would go to — the terminals the move would close, named, and a single-use token bound to this workspace and this destination. A token issued for one destination does not spend against another: a person accepted a move to a particular profile, not a move. The same scope as the operation itself.

Requires: `workspace.migrate`.

## POST `/v1/workspaces/{id}/migration`

Give a workspace to another profile, closing its terminals first

Returns: What the operation did: both ends, the sessions it closed, and whether the reassignment committed. A refusal carrying workspace_migration_incomplete with sessions_closed: true and mutation_applied: false is a partial failure — the terminals are gone and the workspace has not moved — and the same request again is the retry, which needs a fresh confirmation because the first one was spent. Moving a workspace to the profile that already owns it succeeds and closes nothing.

Requires: `workspace.migrate`.

## POST `/v1/events`

Publish a batch of events; idempotent by event_id

## GET `/v1/events`

Read one page of the journal after a cursor

## GET `/v1/events/activity`

One subject's activity, bounded and paged; never a journal scan

## GET `/v1/events/stream`

Server-sent events from a cursor; id = seq, Last-Event-ID resumes

## POST `/v1/sessions`

Create a rooted process session

Requires: `sessions.control` and the `sessions` capability.

## GET `/v1/sessions`

List session metadata

Requires: `sessions.metadata.read` and the `sessions` capability.

## GET `/v1/sessions/{session_id}`

Get one session's metadata

Requires: `sessions.metadata.read` and the `sessions` capability.

## POST `/v1/sessions/{session_id}/controller`

Acquire the exclusive controller lease

Requires: `sessions.control` and the `sessions` capability.

## DELETE `/v1/sessions/{session_id}/controller`

Release the controller lease without ending the process

Requires: `sessions.control` and the `sessions` capability.

## POST `/v1/sessions/{session_id}/snapshot`

Read a bounded terminal snapshot

Requires: `sessions.output.read` and the `sessions` capability.

## POST `/v1/sessions/{session_id}/scrollback`

Read one bounded scrollback page

Requires: `sessions.output.read` and the `sessions` capability.

## POST `/v1/sessions/{session_id}/input/bytes`

Write base64-encoded bytes to the process

Requires: `sessions.control` and the `sessions` capability.

## POST `/v1/sessions/{session_id}/input/text`

Write text to the process

Requires: `sessions.control` and the `sessions` capability.

## POST `/v1/sessions/{session_id}/input/key`

Send one terminal key

Requires: `sessions.control` and the `sessions` capability.

## POST `/v1/sessions/{session_id}/input/paste`

Paste text using terminal paste semantics

Requires: `sessions.control` and the `sessions` capability.

## POST `/v1/sessions/{session_id}/attachments`

Upload and attach verified image bytes

Requires: `sessions.control`, `sessions.attachments.write` and the `sessions` capability.

## GET `/v1/sessions/{session_id}/attachments`

List session-owned attachments

Requires: `sessions.attachments.write` and the `sessions` capability.

## DELETE `/v1/sessions/{session_id}/attachments/{attachment_id}`

Remove one pending attachment

Requires: `sessions.control`, `sessions.attachments.write` and the `sessions` capability.

## POST `/v1/sessions/{session_id}/pending-input`

Deliver one logical text-and-attachment input

Requires: `sessions.control`, `sessions.attachments.write` and the `sessions` capability.

## POST `/v1/sessions/{session_id}/interrupt`

Interrupt the active process

Requires: `sessions.control` and the `sessions` capability.

## PUT `/v1/sessions/{session_id}/size`

Set the process terminal size

Requires: `sessions.control` and the `sessions` capability.

## DELETE `/v1/sessions/{session_id}`

Terminate the complete process tree

Requires: `sessions.control`, `sessions.terminate` and the `sessions` capability.

## GET `/v1/sessions/{session_id}/subscribe`

Subscribe to ordered output and lifecycle events

Requires: `sessions.output.read` and the `sessions` capability.

## GET `/docs`

Offline HTTP API documentation.

## GET `/docs/asyncapi`

Offline streaming API documentation.

## GET `/v1/asyncapi.json`

Generated AsyncAPI 3.0 contract.
