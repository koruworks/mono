# Document sync denial contract v1

> The issuance vectors' `check.py` reads this table. The hosted Koru Cloud
> service's own contracts are not in this repository. Its provenance names — plans, `D`-numbered
> decisions, `DECISIONS.md`, `SPEC.md` — are the hosted service's planning
> records, which are not published; the rows below are the contract.

**Status: early Plan 28 contract; producer implementation and final security
closure are pending.** Published before Plan 13, as required by the amendment
table in CLOUDSYNC.DOCUMENTS.final.md. This
document does not declare the security suite passed, authorize a persisted
format, or make the build ready for release or acceptance.

The authority is Documents Part 1, the retained
MS1 Plan 28,
and DECISIONS.md, especially D43, D49, D124, D129, D145 and
D149-D153. SPEC.md supplies the amended security provenance;
its older milestone assignments are superseded by the
current rung table.

Source audit baseline: `3b32f4879aa99fcbf0619efd500c842b081f8209`, including
Plan 1. No unmerged producer source was used as
evidence. `profile-units-v1`, one of the hosted service's contracts, is a
**proposed schema and downgrade contract awaiting approval**, not an
implemented or approved format.
Its proposed refusal names below do not imply approval of its schema changes.

## Reading and implementing a row

Every `DS-Dnn` row is a producer obligation for this documents rung. Its
expected code is the single lowercase wire spelling to assert at the named
boundary. Names with no existing producer are prospective contract names;
they are not added to a code enum, API schema or status surface by publishing
this table. Each producer must implement its code and its named test together.
Before that producer merges, reconcile any existing code at the same boundary
with this table through review; do not introduce aliases or claim an unmerged
implementation is evidence. The Plan 3 implementer supplied its intended
`wrong_audience`, `wrong_region`, `workspace_not_authorized`,
`membership_epoch_mismatch`, `replica_mismatch` and `principal_not_allowed`
spellings during coordination. They are used as prospective obligations here;
its source/tests must be checked at the committed revision before they can be
mapped as evidence. Existing protocol-level `unauthorized_workspace` and
`unauthorized_profile` remain at their separate unit-protocol boundaries;
they are not aliases for the new shared authorization decision.

Fixtures start from a working authorized case and change the stated
precondition. A refused request must reach a live handler or deterministic
in-memory adapter and yield the **exact code**, not a timeout, failed dial,
empty result, or generic `is_err()`. A protocol error frame, HTTP structured
error, or typed internal fence result has one code across its adapters. Where
a session cannot receive a final frame, assert its local typed close reason
and admission refusal directly; a network disconnect alone proves nothing.
Never expose developer error prose as localized product text.

Except rows deliberately combining conditions, vary one axis at a time with
the others valid. The producer also owns its precedence table for combined
failures. For nonexistent and unauthorized object identities, verify that
refusals reveal no existence information. Denied imports must not commit;
denied reads must not export bytes. Assert both effects in addition to the
code. Inventory omission is a complementary privacy assertion, not a substitute
for the direct request/update probe.

All rows are currently **pending full producer evidence**. `E1`-`E4` below are
partial existing evidence only; a dash means no existing test mapped, not a
passed check. Owners are producer plans; Plan 28 owns final cross-plan closure.

## Credentials, admission and scope

| Row id | Credential and surface | Preconditions | Expected stable code | Exact refusal observation | Owning plan | Existing evidence |
| --- | --- | --- | --- | --- | --- | --- |
| DS-D01 | Native API access credential at Cloud sync admission | Correct signature/region/time, API audience instead of `koru-sync` | `wrong_audience` | Admission returns code before any catalog or Loro frame is handled | 6 verification, 10, 13 | E3 partial |
| DS-D02 | Sync token at native API authorization | Valid sync token supplied where native API audience is required | `wrong_audience` | Structured API error carries code; protected operation is not invoked | 3, 6 verification, 13 | E3 partial |
| DS-D03 | Sync token at regional verifier | Valid test token names EU; verifier and configured issuer serve US | `wrong_region` | Verifier/admission returns code for cross-region claims; no authority lookup in another region | 4, 6 verification, 13 | — |
| DS-D04 | Sync token at shared workspace authorization | Token authorizes workspace A; request names B; other axes current | `workspace_not_authorized` | Shared authorization returns code for both known B and unknown B; no B inventory/payload | 3, 11, 13, 19 | E1 partial |
| DS-D05 | Sync token at admission and active-session revalidation | Injected clock reaches token expiry; authority otherwise current | `credential_expired` | New admission and affected session close reason carry code at expiry; next frame cannot read/write | 6 verification, 13, 19 | E3 partial |
| DS-D06 | Native session/device evidence at sync-token issuance | Device registration is revoked before issuance transaction | `device_revoked` | Issuance returns code and no new token; repeat refresh/reissue cannot resurrect authority | 11, 13 | — |
| DS-D07 | Previously issued sync token at admission/use | Issuing device is revoked after token issuance; observation has reconciled current authority | `device_revoked` | New admission and active-session revalidation return code immediately on observed revocation | 11, 13, 19 | — |
| DS-D08 | Sync token and transport peer EndpointId | Token was issued for endpoint A, live authenticated transport is endpoint B | `endpoint_mismatch` | Application handshake returns code before first replication frame | 10, 13 | E4 partial |
| DS-D09 | Captured Cloud application handshake | Reuse signed response against a new challenge/connection | `handshake_replay` | Handshake verifier returns code; replay creates no session or scope | 10 | E4 partial |
| DS-D10 | Issued Cloud sync route and live transport | Live transport endpoint differs from the issued route's worker endpoint | `endpoint_mismatch` | Client returns code before `ClientHelloV1`; no credential is sent and no transcript is signed | 10, 19 | E4 partial |
| DS-D11 | Issued Cloud sync route and current assignment | Logical replica id differs from the requested workspace's current assignment | `replica_mismatch` | Admission returns code; no invented device identity or alternate replica fallback | 3, 10, 12, 19 | — |
| DS-D12 | Cloud service credential at bilateral device admission | Credential is valid for Cloud service, but supplied to device-to-device path | `principal_not_allowed` | Device admission returns code before device trust/session creation | 3, 10, 21 | — |
| DS-D13 | Valid sync token and cached authorization snapshot | Membership epoch advanced after snapshot/token issuance | `membership_epoch_mismatch` | Snapshot is refused at its use boundary with code; no new frames authorized by old membership | 3, 11, 13, 19 | — |
| DS-D14 | Valid sync token and cached authorization snapshot | Cloud-access epoch advanced; lifecycle otherwise allows an isolated stale-epoch probe | `cloud_access_epoch_stale` | Use/admission returns code; fresh lease or successful health read cannot bless old snapshot | 11, 13, 19 | — |
| DS-D15 | Worker's own lease ownership | Worker's lease id/epoch no longer names current live owner | `replica_fenced` | Produced by the worker's lease check alone since D231: admission returns code and the old actor cannot serve a frame | 10, 12, 19 | E2 partial |
| DS-D16 | Writer's lease id/epoch at conditional receipt commit | Reassign or expire lease between payload write and SQL receipt transaction | `replica_fenced` | SQL conditional commit refuses with code; no receipt row/version acknowledgement is committed | 12, 20 | E2 partial |
| DS-D17 | Writer's authorization snapshot at conditional receipt commit | Cloud-access epoch advances between payload persistence and receipt commit | `cloud_access_epoch_stale` | SQL conditional commit returns code and commits no receipt, even if caller skipped a precheck | 11, 20 | — |
| DS-D18 | Writer's authorization snapshot at conditional receipt commit | Membership epoch advances between payload persistence and receipt commit | `membership_epoch_mismatch` | SQL conditional commit returns code and commits no receipt under removed membership | 11, 20 | — |
| DS-D19 | Old-incarnation token/receipt attempt | Delete completed, new enable has a different incarnation, all current fixture fields except incarnation are valid | `cloud_incarnation_stale` | Admission and SQL receipt fence each return code; old payload lineage cannot advance new coverage | 11, 13, 20, 22 | — |

DS-D03 is a hermetic claim/configuration mismatch test using test keys. It
does not require deploying EU, introducing an EU client selector, or issuing
production EU credentials. DS-D01/D02 cover retained credential classes;
they do not introduce relay tokens, object capabilities or PATs.

**Verifier precedence clarification after Plan 6.** DS-D01/D02's
`wrong_audience` assertion isolates the authenticated audience axis: the
purpose-specific key, header type and complete strict shape are otherwise
accepted. A real foreign access/enrollment credential must also be tested,
but may fail earlier at header purpose or key lookup. Never pool signing
purposes or trust an unverified payload merely to reach its audience. The
shared verifier's current exact observations are `wrong_purpose` for a
different header type and `credential_unknown_key` for a key absent from the
configured purpose. Concrete Plan 10/13 adapters must pin their wire mapping
and prove no protected effect before these full rows can close. The partial
fixtures and precedence are in PLAN06.md; they do
not claim a production sync shape, API refusal or active-session close.

## Revocation, retained reads and worker liveness

| Row id | Credential and surface | Preconditions | Expected stable code | Exact refusal observation | Owning plan | Existing evidence |
| --- | --- | --- | --- | --- | --- | --- |
| DS-D20 | Previously valid token at retained-copy read admission | Turn Off committed and worker has reconciled it | `cloud_disabled` | New read and queued next read return code immediately; retained payload is not exported | 11, 19 | — |
| DS-D21 | Previously admitted session at frame boundary | Delete committed and observed, payload removal may still be in progress | `cloud_deleting` | Read/write admission and close reason carry code immediately; deletion progress cannot retain access | 11, 19, 22 | — |
| DS-D22 | Valid token at worker startup/reconnect | No successful current authorization reconciliation yet | `authorization_unavailable` | Readiness remains false; admission returns code; opening PostgreSQL or reading a lease is insufficient | 19 | — |
| DS-D23 | Active sessions after regional authorization outage | Last successful relevant reconciliation at monotonic t0; subsequent cycles fail | `authorization_unavailable` | Independent timer fences frames and closes affected sessions by t0+30s with code; authoritative reconciliation required before readiness returns | 11, 19 | — |
| DS-D24 | Active sessions with stalled authorization consumer | Commit Turn Off; stall relevant reconciliation while lease/health SQL calls continue succeeding | `authorization_unavailable` | Successful unrelated calls do not move t0; independent timer returns code and closes sessions by t0+30s | 19 | — |
| DS-D25 | Active sessions after lost change notifications | Suppress notifications/control wakeups while authorization state changes in PostgreSQL | `cloud_disabled` | Fixed 5s reconciliation observes committed Turn Off and fences immediately with code; notification silence is not authority freshness | 11, 19 | — |
| DS-D26 | Active sessions with hung reconciliation query | Query exceeds its 3s timeout or DB task never resumes; last reconciliation was t0 | `authorization_unavailable` | Query failure cannot reset t0; independent timer closes sessions and refuses next frame by t0+30s with code | 19 | — |
| DS-D27 | Current identity with stale Cloud snapshot at use | Snapshot predates current version/epoch, despite earlier valid issue/admission | `authorization_stale` | Shared authorization use boundary returns code; refresh/read current authority before retrying any effect | 11, 19, 21 | — |

The 30 seconds is the upper bound after **lost relevant authorization
freshness**, not a grace period after an observed revocation. Only a completed
reconciliation of current authorization relevant to the sessions served resets
t0. Cadence is 5 seconds, bounded read timeout 3 seconds; a monotonic timer
independent of the database task enforces 30 seconds. D49's historical
"blackholed control stream" maps to DS-D24/DS-D26 now that the worker consumes
PostgreSQL directly. No parent-to-child forwarding channel is required.

Use an injected/paused monotonic clock for deterministic unit assertions, then
the feature-gated local-stack fault lane to verify actual session closure.
This deadline is an authorization invariant, not a machine-speed performance
budget. Matrix tests cover each affected scope (one workspace, many
workspaces, Profile identity shared by those workspaces) and verify unaffected
authorized scopes remain valid. Once the last Cloud-enabled workspace is
disabled, identity admission is denied as well (DS-D32).

## Profile units, provisioning and deletion races

| Row id | Credential and surface | Preconditions | Expected stable code | Exact refusal observation | Owning plan | Existing evidence |
| --- | --- | --- | --- | --- | --- | --- |
| DS-D28 | Workspace collaborator at personal-unit request/update | Caller has workspace W but not owner Profile P; target is Personal(P,W) | `unauthorized_profile` | Both direct read and update return code before export/import; P's unit also absent from inventory/digest | 31, 19, 21 | E1 partial |
| DS-D29 | Profile-authorized caller at personal-unit request/update | Caller has P but lacks workspace W; target is Personal(P,W) | `unauthorized_workspace` | Both direct read and update return code; no general Profile-grant fallback | 31, 19, 21 | E1 partial |
| DS-D30 | Cloud session at personal-unit request/update | Both owner/workspace authority exist, but W is local-only or disabled | `cloud_disabled` | Both direct read and update return code; no W unit, EntityRef or historical payload in Cloud inventory/provisioning | 31, 19, 21 | — |
| DS-D31 | Cloud principal at legacy-private Profile request/update | Even all-data device authority would permit this legacy unit on a device path | `legacy_profile_cloud_forbidden` | Cloud admission returns code for read and update before importing/exporting any legacy bytes or history | 31, 19, 21 | — |
| DS-D32 | Cloud session at identity-unit admission | Exact Profile authority exists but Profile has zero Cloud-enabled workspaces | `cloud_disabled` | Direct identity read/update returns code; shared identity is removed from Cloud-serving eligibility after last disable | 31, 19 | — |
| DS-D33 | Peer below unit-addressing protocol version | Old peer attempts catalog/Profile exchange | `profile_units_required` | Negotiation returns code before any changed frame is consumed; incompatibility is visible through control observation | 31, 10, 19, 21 | — |
| DS-D33b | Peer on unit-addressing protocol 5, below the root-catalog exchange | Peer attempts any data-plane frame; it would still write placement from descriptors and export personal units on the v1 address | `root_catalog_required` | Negotiation returns code before any frame is consumed; trust gossip continues; the device list says update | root-catalog 2.1 | — |
| DS-D34 | Authorized personal-unit update with inconsistent scope | Unit ref, immutable metadata or target EntityRef names another owner/workspace | `profile_unit_scope_mismatch` | Staged validation returns code before persistence, wake, acknowledgement or foreign-target application | 31 | — |
| DS-D35 | Queued unit export/import under account A | Account switch or authority revocation invalidates captured authorization before effect | `authorization_stale` | Effect boundary returns code, commits/exports nothing; no reassignment of A's owner units into B | 31, 21, 23 | — |
| DS-D36 | Durable Cloud completion at device trust admission | Cloud-provisioned device has no bilateral pairing; attempt to treat completion as device trust | `principal_not_allowed` | Trust/admission boundary returns code; completion creates no relationship or two Profile grants, including restart retry | 10, 21 | — |
| DS-D37 | New or queued write at admission after Delete barrier | Delete has closed write admission for the incarnation; request was not already admitted | `cloud_deleting` | Admission returns code before starting a storage write; queued requests cannot bypass the barrier | 20, 22 | — |
| DS-D38 | Old-incarnation write attempt after delete completion and re-enable | Retry a write naming the removed incarnation after the new dataset begins | `cloud_incarnation_stale` | Writer barrier and conditional receipt refuse with code; no old payload resurrection or new-incarnation receipt | 20, 22 | — |

DS-D28/29 must run against two owners with different values and reminders for
the same EntityRef. Neither account selection nor workspace membership alone
grants the other owner's personal unit. Source/provisioning histories are
inspected as complete retained Loro operations, including cleared/deleted
values; searching only live rows or compressed bytes does not prove privacy.
DS-D34 is scope validation, not a promise to sanitize arbitrary opaque CRDT
history. The fresh identity constructor must never import the legacy lineage.

DS-D36 is accompanied by a successful Cloud completion/provisioning test:
inspect the durable repositories before completion, after completion and after
each restart repair boundary. There must be no new bilateral relationship,
Profile trust grants or trust revision caused by Cloud completion. Confirm
the device can still complete authorized Cloud document coverage. The denied
conversion probe supplies the code; absence of trust rows supplies the
separate D145 state invariant. Pairing two online devices remains its own
positive control and must continue to create the intended trust.

DS-D37 refuses new or queued admission; it does not require an already-admitted
storage call to fail. Its accompanying held-writer proof follows Plan 22's
two permitted orderings. Pause an admitted writer before payload publication;
commit Delete, close admission and revoke access. The workspace must already
read `deleting`, and the drop must not yet claim completion. Then either:

1. **Wait for the writer.** Release it, allow its storage call to settle, run
   cleanup after every old writer and repair job has settled or been fenced,
   and only then assert completion and an empty payload estate. The admitted
   write may finish while deletion remains in progress.
2. **Prevent future publication.** Prove the held writer and every old repair
   job can no longer publish to storage, run cleanup after that point, then
   permit completion before release. Release the writer and assert that the
   estate remains empty. An actor fence or refused SQL receipt alone does not
   establish that an in-flight storage call cannot publish.

Repeat the applicable ordering with process death and recovery at its
boundaries: after admission closes, before writer settlement or publication
prevention, after that point but before cleanup, and after cleanup before
completion. Persisted progress must resume the same barrier, writer and
cleanup obligations before reporting completion. Enumerate segments/snapshots
and SQL metadata for the exact incarnation, including orphan payloads; zero
advertised rows alone is insufficient. Also pause after payload persistence
before conditional receipt and repeat the corresponding fence/drop proof.

DS-D38 separately refuses an old-incarnation write attempt after completed
Delete and re-enable. In the second held-writer ordering, release may also
occur after re-enable; the old estate must remain empty and the new one
untouched. The first ordering settles writers before completion, so it must
not be forced to leave an admitted writer pending across re-enable. Replay a
stale drop against the new incarnation and assert it touches nothing. Turn
Off retains bytes and resumes the same incarnation; completed Delete removes
bytes and a later enable allocates a new incarnation. No blob/object
upload-session proof is inherited from these document-only tests.

## Cloud carrier producer obligations (TECHNICAL final, 2026-09-12)

These rows amend Plan 10's post-acceptance producer only. Error is an existing
tag-0 PeerFrame::Error followed by dataset-stream close; reset without error is
permitted after an authority fence or transport loss. No fragment is imported
before complete length/hash checking; hash checking is not causal validity.
The frozen carrier family is [cloud-transfer-v1](vectors/cloud-transfer-v1/README.md).
Source evidence is tracked in PLAN10-TRANSFER.md.

| Row id | Credential and surface | Preconditions | Expected stable code | Exact refusal observation | Owning plan | Existing evidence |
| --- | --- | --- | --- | --- | --- | --- |
| DS-D39 | Accepted Cloud stream, record/control/batch decoder | Invalid length/tag/strict shape, future start generation, changed duplicate/skipped policy, wrong decision, fragment scope/order/length/hash | `sync_transfer_invalid` | Exact code, close and zero partial import | 10, 19, 23 | Pending |
| DS-D40 | Accepted Cloud stream, monotonic transfer timer | Start JSON exceeds 10s, decision exceeds 10s, or absolute B(L)=30+ceil(L/32768) exhausted | `sync_transfer_timeout` | Exact code, discarded buffer and no ACK/import | 10, 19, 23 | Pending |
| DS-D41 | Accepted Cloud stream, serialized policy boundary | Older-generation start, local policy/path change during unfinished inbound, or new peer policy during unfinished outbound | `sync_policy_changed` | Exact code and close without importing old bytes; already handed-off valid Store commit may finish | 10, 19, 23 | Pending |
| DS-D42 | Cloud export admission | Export exceeds 16,777,216 raw bytes | `sync_update_too_large` | No manufactured checkpoint, prefix transfer or coverage | 10, 19, 23 | Pending |
| DS-D43 | Cloud transfer admission | Budget reservation fails or remaining credential/route lifetime is less than B(L)+15s | `sync_unavailable` | No partial import; refresh/rehandshake before trying again | 10, 19, 23 | Pending |

An advertised zero ceiling pauses all content and cannot be bypassed by manual
accept. Restricted ceiling is 200,000, unrestricted 16,777,216. Authority
refusals preserve their existing more specific codes. Policy initialization is
per receiving direction/accepted stream and cannot be inferred from an older
connection, account or previously closed stream.

## App connection producer obligations (Cloud MCP, 2026-11-06)

These rows amend the Cloud carrier for **app connections**: an approved
third-party application, such as an AI assistant, whose in-memory replica
syncs under its own credential purpose, `koru:sync:app-connection:v1`. The
frozen vectors are [cloud-transfer-v1's `claim.json`](vectors/cloud-transfer-v1/README.md#the-app-connection-claim-amendment-claimjson).
`CloudTransferStartV1` gains an optional `claim` reference, the SHA-256 of a
document claim's handle, accepted only on an app connection's session.

The two new codes are produced **only for uploads under an app connection's
credential**; a device upload is never checked by either rule, and a device
never receives either code. Each is an existing tag-0 `PeerFrame::Error`
followed by dataset-stream close, like the carrier refusals above, and each is
defined to leave the unit's head unmoved: nothing it refused is ever
published, which is what lets the connection report the effect `not_saved`.

| Row id | Credential and surface | Preconditions | Expected stable code | Exact refusal observation | Owning plan | Existing evidence |
| --- | --- | --- | --- | --- | --- | --- |
| DS-D44 | App connection upload at the worker, before `reserve` | The upload's blob is a snapshot from an empty base (a creation) and the document unit resolves `active` | `sync_unit_exists` | Exact code before `reserve`; no object row, no receipt, unit head unmoved; an `absent` or `initializing` unit is still initialized | Cloud MCP 2.2 | Pending |
| DS-D45 | App connection upload of a document unit, before `reserve` and in the publish transaction under the claim lock | A live claim (unexpired by the database clock, holder not revoked) exists for the document, and the upload's `claim` is absent, differs from the claim's hash, or the claim belongs to another connection | `sync_document_claimed` | Exact code; before `reserve` nothing is written; at publish the transaction rolls back and its `pending` object row is never published; unit head unmoved | Cloud MCP 3.3 | Pending |
| DS-D46 | Cloud transfer decoder on a session that is not an app connection's, or a client receiving a worker's start | A `transfer_start` carries `claim` | `sync_transfer_invalid` | Exact code and close before any payload is admitted; the same start without `claim` is byte-identical to before | Cloud MCP 3.2 | Pending; `claim.json` C04 and C05 |

With no live claim, an upload's `claim` is ignored. A claim reference is a
hash, not a bearer: no tool accepts it, it is never logged, and it is never
written into attribution.

## Existing test evidence: partial, inspected, not run for this publication

Repository-relative source paths below are exact existing locations at the
audited commit. They establish useful fixtures or narrower assertions only.
No row above is fully mapped to a passing producer test by this publication.

| Evidence id | Existing test id and source | Actual assertion and remaining gap |
| --- | --- | --- |
| E1 | `koru-replication::protocol::tests::cross_profile_inventory_blob_and_loro_frames_reveal_nothing`; `scoped_unknown_and_unrelated_workspace_probes_are_indistinguishable`; `profile_scope_exposes_ids_and_workspace_data_but_no_profile_metadata`, all in `crates/koru-replication/src/protocol/tests.rs` | Existing device-scoped inventory/Loro probes assert `unauthorized_workspace`, unknown/known probes are indistinguishable, and Profile metadata probe asserts `profile_metadata_not_authorized`. No Cloud sync token, new personal units or owner-plus-workspace Cloud producer exists in these tests. The neighboring blob fixture is existing device coverage, not document-rung object-plane evidence. |
| E2 | `fencing_a_replica_stops_its_frames_from_reaching_a_receipt` and `every_refusal_has_its_own_stable_code`, in the hosted replica host's tests; `HostError::code` in its source (hosted Koru Cloud code, not in this repository) | Actor fence test asserts typed `HostError::Fenced` for in-flight/queued/future work; source maps that error to `replica_fenced`. This is not a SQL conditional receipt test, lease expiration test, worker authority timer test or proof of no stale payload publication. Future row test must assert the exact code at its named boundary. |
| E3 | `koru-account-session::lib_tests::device_attestations_have_a_distinct_type_audience_and_verifier`; `attestation_issue_skew_is_bounded_and_never_extends_expiry`; `attestation_signing_rejects_wrong_audience_and_clock_boundaries`, in `crates/koru-account-session/src/lib_tests.rs` | Existing device-attestation purpose/audience and time validation; no `koru-sync` issuance/verification adapter or the table's exact new denial codes. |
| E4 | `koru-pairing::tests::a_join_half_cannot_be_replayed_against_another_invitation`; `enrollment_rejects_attested_device_key_and_endpoint_substitution`, in `crates/koru-pairing/src/tests.rs` | Existing device-pairing transcript and EndpointMismatch assertions. They are not Cloud service handshakes, Cloud replay code evidence, or proof that Cloud provisioning creates no trust. |

## Deferred security families and historical provenance

Deferred entries have **no document-rung code or producer**. Their identifiers
record gaps for the later security completeness check; they are not executable
denial rows. Recheck the current rung table when those plans become final.

| Gap id | Family / historical rule | Named owner |
| --- | --- | --- |
| DS-G01 | Object capability audience/workspace/operation/range, upload replay/bounds and object delete races | Rung 2, **blob storage infrastructure and syncing**, pending its own approved plan (D154/D155) |
| DS-G02 | Relay token audience, EndpointId theft, expiry, revoked-no-reissue, enforcement and active-session closure | Rung 10, **commercial readiness / relay authentication**; retained shared verification fragment alone does not add relay enforcement |
| DS-G03 | Every unwrap/cross-hierarchy rule, tampered envelope and ciphertext-only object plane | Rung 5, **encryption**; provider-specific verification continues in B2/Cloudflare rungs 7/8 |
| DS-G04 | Entitlement boundary, capped live authority, quota | Rung 10, **commercial readiness**; Plan 13's unbounded cap input and synthetic bound test remain required without a production entitlement code |
| DS-G05 | Retention, purge/reseed and old-trust-epoch reseed | **After launch: purge saga and retention clock**, with its explicit future plan; immediate document Delete is DS-D21/37/38 |
| DS-G06 | PAT audience/scope/revocation and MCP access | **After launch: MCP and API access**, `CLOUDSYNC.API-ACCESS.rev1.md`; historical MS5, not a currently numbered rung |
| DS-G07 | EU PII routing, independent EU credentials/configuration and no cross-region failover | Rung 6, **EU**; provider cross-region structure extends in rungs 7/8; DS-D03 already rejects wrong-region claims in the US-only rung |
| DS-G08 | Acknowledged document survives total host-disk loss | Rung 7, **B2**, per D150; rung 1 proves process/container replacement on the same volume under Plan 20 |
| DS-G09 | Outside account is refused by the service | **Public launch review (rung 11)** must settle the admission policy before distribution expands; D116 is currently an operator-held installer restriction, not a code guarantee or a passing denial test |

The complete source examples in historical
§65/§66 map as follows.
This records provenance; it does not implement the historical note.

| Provenance id | Historical item | Current resolution |
| --- | --- | --- |
| DS-P65-1 | Relay token at API | DS-G02 |
| DS-P65-2 | API PAT at Relay | DS-G06 and DS-G02 |
| DS-P65-3 | Object capability at Sync | DS-G01 |
| DS-P65-4 | Sync token A at workspace B | DS-D04 |
| DS-P65-5 | EU token at US service | DS-D03; live second-region deployment DS-G07 |
| DS-P65-6 | Enforce region and audience through claims and service configuration | DS-D01/02/03; later credential families DS-G01/02/06 |
| DS-P66-1 | EU email never reaches US Auth/API | DS-G07 |
| DS-P66-2 | Stolen Relay token without matching EndpointId | DS-G02 |
| DS-P66-3 | Revoked device cannot get fresh Relay or Sync token | Sync DS-D06; Relay DS-G02 |
| DS-P66-4 | Relay token at API | DS-G02 |
| DS-P66-5 | US API token at EU API | DS-G07; US-side wrong-region verifier obligation DS-D03 |
| DS-P66-6 | Object capability A at workspace B | DS-G01 |
| DS-P66-7 | EU object request cannot reach US B2 | DS-G07 with provider closure in rung 7 |
| DS-P66-8 | Sync server destroyed after durable acknowledgement | Plan 20 same-volume replacement proof; host-disk-loss part DS-G08 (D150) |
| DS-P66-9 | Cloudflare/B2 sees ciphertext only | DS-G03 with provider closure in rungs 7/8 |
| DS-P66-10 | EU Gateway/B2 sees ciphertext only | DS-G03 with regional/provider closure in rungs 6/7 |

## Producer handoff and final mechanical closure

Each producer lands its row tests beside the covered module and records the
row id in the test case. The test must observe the exact boundary/code above,
not repeat this table as a mock result. Append its fully qualified test name,
source path, verification lane and immutable implementation commit to the
final evidence map when integrated. A named future test is a pending obligation,
not evidence. Default tests remain hermetic (no disk, live Store, browser or
telemetry); filesystem/SQL faults use the feature-gated integration lane,
isolated roots and `koru_account_test_*` with `koru_disposable=1`.

Plan 28's later executable completeness check must:

1. Parse every `DS-D`, `DS-G` and `DS-P` id exactly once from these tables;
   reject duplicates, dangling resolutions and missing historical items.
2. Require every document-rung denial to resolve to a named implemented test
   in the test inventory with a recorded lane/result. Reject a mapping to a
   nonexistent test, an unmapped row, or a pending producer as security
   closure. Each gap must resolve to its named future rung/workstream with
   its reason. Do not treat an unrelated existing test as a row's producer.
3. Require every historical item to resolve through those rows/gaps or to
   Plan 20's named durability test. Follow every branch of a compound
   resolution; testing Sync does not close a deferred Relay obligation.
4. Check expected codes against the producer's actual stable-code output and
   their unchanged adapter spelling. Test IDs alone do not prove this; the
   executed test must assert the code at the real refusal boundary.
5. Carry deliberately failing fixtures for a missing mapping, nonexistent
   test, orphan historical item and invalid code spelling. CI must prove
   the check rejects each fixture. The final `SECURITY.md` links results and
   remaining gaps. This publication implements none of that checker.

The separate product-claim/content detector also remains Plan 28 work: bound
the built artifacts, the hosted deployment's configuration, operations documentation and each surface's
eight-language message catalogues; record the exact encryption/E2E/
zero-knowledge/backup term list and translations; allowlist individual TLS,
internal disclaimer and local-backup statements with reasons; inject a failing
claim fixture. Scan logs, metrics, crash artifacts and telemetry using seeded
content fixtures and a defined capture path set. A prose assertion of absence
or an unbounded grep cannot close either detector.

## What remains to verify

Only source/document inspection was performed for this contract. No suite,
local service, deployed service, app or database was run or changed. The early
publication is ready for producer review; full Plan 28, release and acceptance
remain pending.

Downstream automation must implement the rows, prove exact codes, verify
authority outage/stall and SQL/writer races, and close the complete map plus
claim/content detectors. Windows/Android release checks must then verify the
localized old-peer refusal and Cloud-only provisioning with every other
device off. Inspect the received unit inventory and decoded retained history,
not just the opened document. Inspect trust records around exact completion
retry/restart and confirm the Cloud path created none; pair online explicitly
as the positive control for later LAN convergence.

Operators must record the image/build identifiers and isolated fixture roots;
exercise actual worker control outage with unrelated SQL still succeeding,
observe session closure within the 30-second bound and recovery only after
reconciliation; replace the container on its persistent volume and confirm
acknowledged document vectors; Turn Off and confirm retained bytes with denied
reads, then race Delete against a held writer and confirm zero document payload
for the deleted incarnation. Preserve credentials and content outside reports.
These observations belong in the delivery lead's eventual acceptance/runbook
artifacts and do not substitute for automated producer proofs.
