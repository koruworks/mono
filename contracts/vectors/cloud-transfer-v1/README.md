# Cloud transfer v1 — production carrier vectors

Frozen before implementation from maintainer-approved TECHNICAL.final.md §§3–4.
The 32 cases in cases.json pin strict control decoding, framing header limits
and monotonic budget arithmetic. Exact JSON strings are inputs, not schematic
objects: duplicates must survive until the real decoder rejects them.

T12 is a shape-only six-byte payload descriptor. It does NOT prove a valid Loro
import. Production tests must additionally export real Loro documents and import
only after complete length/hash checking through the existing Store boundary.
No old amendment JSON, SQL, checker or golden bytes are rewritten or inherited
as production proof. The four-message handshake and its existing vectors remain.

Supplement.json freezes T33–T52 before source acceptance: decision controls,
all three Profile unit arms, strict PeerFrame and nested unknown/duplicate
properties. Both files are consumed by production Rust decoder tests.

The first production run exposed T50's authoring error: existing ProfileUnitRef
uses type, not the new Cloud transfer unit's kind. errata.json explicitly changes
that expected result to invalid while preserving the frozen input bytes.
profile-request.json adds T53–54 for the real existing shape and escaped
duplicate. Rust consumes all three input files and checks the erratum's original
expected value before applying the documented correction.

Stateful producer tests additionally cover policy generation 1 initialization,
identical replay, skipped/changed/backward policy, automatic 200000 versus
explicit 200001, zero pause, defer, stale/future start distinction, independent
directions/streams, complete/header/index/length/hash validation, bounded memory,
EOF/cancel, deadline/lifetime, policy versus durable-handoff ordering, and real
causally missing Loro ancestors. Cases here alone do not close those obligations.

Implementation/evidence: ../../../execution/PLAN10-TRANSFER.md.

sequences.json freezes T55–59 as executable directional/race sequences. Each
step is [operation, integer argument, exact result code]; start_a/start_b begin
and deliver a start from that side, accept_a/defer_a decide the A-originated
transfer at B and deliver its decision, policy_a delivers the named generation
to A, drain_b replaces B, and send_a verifies no payload remains after defer.
The Rust sequence interpreter consumes every step and asserts each result.

## The app-connection claim amendment (`claim.json`)

Frozen 2026-11-06 for the `cloud-sync-admission` amendment that lets an app
connection's upload carry a claim reference (Cloud MCP). `CloudTransferStartV1`
gains one optional property, `claim`: unpadded canonical base64url of 32
bytes, serialized only when present, never null, accepted only on an app
connection's session. A device session sending one, and a client receiving
one on a worker's start, is `sync_transfer_invalid`. Decisions and policies
never carry it. The two new refusal codes, `sync_unit_exists` and
`sync_document_claimed`, are ordinary tag-0 `PeerFrame::Error` frames.

Each case names its `receiver` (`worker_device_session`,
`worker_app_connection_session` or `client`) and two results:

- `code`, the amended contract's result;
- `today`, the result of the decoder as it stands before the amendment, which
  has no session kind and refuses every `claim`, and decodes either new code
  as the unknown-refusal fallback `handshake_malformed`.

C01 and C19 are a device's starts and are byte-identical to T12 and T38;
C03 is a connection's start with `claim` as an encoder writes it, last.
`record_hex` is the whole tag-1 or tag-0 record. The claim value is the
SHA-256 of `claim_handle_fixture`, a fixture, not a handle format.

`check_claim.py` re-derives every `code` and `today` from the amended field
table, independently of the Rust decoder, and checks the byte-identical
cases against `cases.json` and `supplement.json`. Run
`python check_claim.py` from this directory. `koru-replication`'s
`transfer::tests::claim_amendment` asserts `today` against the real decoder;
the decoder that implements the amendment asserts `code` instead.
