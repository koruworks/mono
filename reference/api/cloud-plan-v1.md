# Account Cloud plan and usage reads

The hosted account API serves `GET /v1/account/cloud-plan` and
`GET /v1/account/cloud-usage/workspaces`. Both accept the same native account
Bearer token or web account session as the account read. An assistant/MCP
credential cannot call them. The account comes from the live session; neither
route accepts an account ID. The desktop gateway does not serve these routes.

The [account OpenAPI document](koru-account.openapi.json) publishes the exact
response schemas. The typed `koru-account-client` methods are `cloud_plan` and
`cloud_usage_workspaces`; call them on an account worker, away from interactive
threads. Each call has the existing bounded transport timeout and response cap.

The plan response separates access, MCP entitlement and purchase management.
An unpaid purchase can remain manageable, and complimentary access does not
invent a purchase. `local` means positively established no active Cloud access;
failed policy evidence is `unknown`. Production M1 never assigns a Free place
when a paid plan or trial ends. Byte limits use decimal bytes.

All byte counts, revisions and millisecond times are canonical nonnegative
decimal **strings** within signed SQL BIGINT range. Do not convert them through
a JavaScript Number. Nullable fields are present as null. Unknown enum values,
missing fields or invalid arithmetic are unavailable data, not a zero meter.

`usage.state = known` requires a complete active `files-plain-v0` generation and
reconciled upload reservations. Before activation usage is `building`; stale or
unreadable evidence is `unknown`. Both withhold current used/available/headroom
values. Even a zero empty account needs an active complete generation. A Local
account may retain nonzero bytes. An unbounded allowance has null total,
available and headroom values while known files and reservations remain exact.

Policy, generation, usage and reservations are read in one database snapshot,
with a database evaluation time and a final expiry check. `valid_until_millis`
is the next policy boundary, when known. Refresh at that boundary and stop
presenting an expired response as current. Responses use `Cache-Control:
private, no-store` and no ETag. A client may retain a whole earlier plan response
as explicitly stale; there is no account `last_known` field to reconstruct by
adding unrelated Workspace snapshots.

Workspace pages accept only `limit` (1–100, default 100) and optional `cursor`
(at most 2048 characters). A cursor binds the authenticated account, active
generation, account revision, catalog revision digest and last Workspace /
incarnation key. `400 invalid_usage_cursor` rejects malformed or foreign
cursors. `409 usage_revision_changed` means restart pagination; never merge
pages from different snapshots. Cursors confer no access or retention rights.

Pages include only owned retained Cloud Workspaces, including disabled and
deletion-pending content. Current committed catalog placement determines
grouping; stale or missing projections produce `unknown`, and an explicit null
placement produces `unassigned`. Both still count toward account usage. Legacy
owner Profile columns are not placement evidence. A Profile ID appears only
when its current authorization and projection are known. Display labels stay
null where no authorized label is available. Per-Workspace stale values are
diagnostic only, never evidence for a current account total.

Database failures and the disabled rollout gate return `503 server_unavailable`;
they do not fabricate Free, Local or empty usage. `free_availability` and
`feedback` are explicitly `not_implemented` in this contract.
