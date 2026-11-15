# Loro

The CRDT that stores every document. `loro` 1.13.

New to Rust syntax? [RUST.md](../RUST.md) first.

---

## What a CRDT is, and what it buys here

A **conflict-free replicated data type** is a data structure that can be edited
in two places at once and merged without anybody choosing a winner. Not
last-write-wins, not "resolve conflict" — both edits are simply present
afterwards.

The trick is that you do not store *state*, you store *operations* plus enough
identity to order them. Two replicas that have seen the same set of operations
compute the same state, whatever order those operations arrived in.

What that buys Koru:

- **Two windows on one document**, with no locking or coordination.
- **Two people, offline, editing the same word** — and both edits survive.
- **A file format for free.** A snapshot is the operation log, compacted.
- **No server required.** Sync is exchanging bytes; the filesystem does it today.

And the thing worth internalising early: **collaboration is a capability, not a
requirement.** A single user never exchanges an operation and pays nothing for
the option (ADR-1).

## The containers

A Loro document is a tree of *containers*. Koru uses four:

| Container | Where | Holds |
| --- | --- | --- |
| `LoroTree` | `outline`, `library` | the node hierarchy — **movable**, which is why it was chosen |
| `LoroText` | one per node | that node's line, with marks |
| `LoroMap` | each node's meta | `id`, `kind`, `done`, `align`, and the attributes |
| `LoroDoc` | the whole thing | the document, its history, its exports |

```
LoroDoc
├── "meta"     LoroMap    name, id, schema_version
├── "outline"  LoroTree   tasks
└── "library"  LoroTree   contacts, documents
                 └── each node: LoroMap
                       ├── "text"  LoroText
                       └── "attrs" LoroMap of LoroMap
```

`crates/koru-core/src/schema.rs` is the *only* file that knows any of this.
Everything above it speaks `Node` and `NodeId`.

### Complete file-reference inspection

The forgiving display reader is not a completeness proof. Its node index can
collapse duplicate IDs, its attribute reader skips unfamiliar records, and its
embed precedence can hide a second blob slot. `schema::file_references` inspects
the raw typed containers before any such projection. It walks the `nodes` tree
and retained legacy `library`/`outline` trees, including Trash and descendants.
Malformed or duplicate identities, unknown fields or types, unsupported schema
versions and exhausted traversal budgets produce an unknown result.

Loro 1.13.9's `children()` and `children_num()` return `None` for a valid leaf
or an empty root: the child index has no entry. Check the parent's existence
separately before interpreting that absence as zero children. These calls do
not distinguish a missing parent from a leaf by themselves.

`RestoredContent::strict_blob_references` validates every raw blob slot, feeds
the canonical reference walker, and returns distinct canonical hashes bound to
the restored unit and exact head. Structural units only return an empty set
after their supported shape has been validated. Inspection does not mutate or
repair the document, confer authority, or prove a durable receipt. Its explicit
node, field, depth and value-byte budgets bound inspection; they do not bound
Loro decoding or expanded memory during causal replay.

## The movable tree

This is the feature that decided the library. Two people dragging the same
subtree to different parents is the hard case in any outliner, and
`LoroTree::mov_to` handles it — including refusing to create a cycle when each
peer moves the other's parent under their own.

```rust
// crates/koru-core/src/schema.rs
tree.mov_to(tid, under, index)?;
```

One primitive is behind reorder, indent, outdent, drag-and-drop and
wrap-into-a-group. See the [API reference](../../../reference/api/API_REFERENCE.md).

## Text, and marks

`LoroText` is insert / delete / **mark**. A mark is a key and a value applied to
a range, and Loro moves it as the text around it changes — including through a
remote edit.

```rust
text.insert_utf8(at, "hello")?;
text.mark_utf8(0..5, "b", true)?;          // bold
text.mark_utf8(0..5, "hl", "yellow")?;     // a value, not just a flag
```

That is the whole reason `**bold**` is gone: the delimiters were in the text and
every byte index had to be fixed by hand.

Reading it back gives *runs* — spans of text with the same marks:

```rust
for delta in text.to_delta() {
    let loro::TextDelta::Insert { insert, attributes } = delta else { continue };
    …
}
```

which is exactly the shape a renderer wants. `crates/koru-core/src/schema.rs`
turns them into `TextRun`s and nothing downstream parses anything.

### Traps that cost real time

- **`unmark_utf8` does not exist**, though `mark_utf8` does. Clearing a mark
  converts the byte range through `LoroText::convert_pos`.
- **`unmark` for a style that is not there is not a no-op.** It disturbs a
  neighbouring span. `insert_pill` reads what a new anchor would inherit *first*
  and clears only that.
- **Loro counts unicode scalars by default; parley counts UTF-8 bytes.** Always
  the `_utf8` variants. `Utf8Pos` exists so the two cannot be mixed silently.
- **`LoroText` has no embed API.** A pill needs a real anchor character
  (ADR-9).

## Expansion

When you type at the *edge* of a mark, does the mark grow to cover what you
typed? That is `ExpandType`, and it is **document configuration, not an argument
to `mark`**:

```rust
// crates/koru-core/src/schema.rs
styles.insert(mark.key().into(), StyleConfig { expand: ExpandType::After });
styles.insert(PILL_KEY.into(),  StyleConfig { expand: ExpandType::None });
doc.config_text_style(styles);
```

Formatting expands — type after a bold word and it stays bold. A pill anchor
never does, or typing beside a pill would be swallowed into it.

**It is not persisted.** `configure_styles` runs in both `create` and
`from_snapshot`, and forgetting the second makes pills start eating adjacent
text after a reload.

## Identity: `TreeID` is not an id

Loro gives every tree node a `TreeID`, which is `{peer, counter}` and meaningful
only inside one document. Koru does **not** use it as identity:

```rust
index: HashMap<NodeId, (TreeID, Realm)>
```

`NodeId` is a UUID in the node's meta map. A `TreeID` would change the moment a
node moved between documents, breaking every reference to it
(ADR-4).

Undo demonstrates why: undoing a `delete` restores the node under a **new**
`TreeID`, with its meta intact. Every reference still resolves.

## Sync: export, transport, import

```rust
let bytes = doc.export(ExportMode::updates(&their_version))?;
other.import(&bytes)?;
```

- `ExportMode::snapshot()` — the whole document, self-sufficient. The file format.
- `ExportMode::all_updates()` — every operation.
- `ExportMode::updates(&vv)` — only what is new since a version.

A **version vector** is "how much of each peer's history I have seen". Comparing
two tells you what to send.

`import` is **idempotent** — Loro discards anything already seen — so redelivery
is harmless and a reconnecting client needs no deduplication of its own.

One thing found the hard way: **the first publish of a session must send
everything.** A delta is only safe once the receiver has its causal ancestors,
and nothing local knows what a peer already has
(ADR-19).

## Undo

`UndoManager` is **local-only**: it reverts this peer's operations, never
another's. That is the only definition that works with two people editing.

```rust
undo.group_start()?;   // a burst of typing…
…
undo.group_end();      // …is one Ctrl+Z
```

Two things worth knowing before you touch it:

- **Grouping by time does not work.** `set_merge_interval` merges by wall clock,
  and `record_new_checkpoint` *cannot break out of a merge window* — so a
  structural change folds into whatever was typed a moment earlier and one
  Ctrl+Z undoes both. Leave the interval at 0; group explicitly
  (ADR-16).
- **Undo survives a concurrent remote change** since `loro-internal` 1.13.9. It
  used to decline outright on a tree-nested container — and every text node here
  is one — returning `false` and changing nothing; that was an upstream bug, and
  it is fixed. `undo` now takes back this peer's run and leaves the remote edit
  standing. One case still declines: **a node the other peer deleted.** Loro
  returns `false` and changes nothing rather than resurrecting it, which is why
  `loro` is pinned to `=1.13.9` in the root manifest — a lockfile regeneration
  changed this behaviour silently once already. Pinned by
  `undo_survives_a_concurrent_remote_change_and_removes_only_its_own_edit` and
  `undo_declines_on_a_node_the_other_peer_deleted` in
  `crates/koru-core/tests/convergence.rs`.
- **Undo can report success without removing newly inserted image tree nodes.**
  Rich and image paste therefore keep a narrow per-paste compensation: the
  inserted IDs plus complete cuttings of any blocks that paste replaced.
  Ctrl+Z inverse-deletes the inserted nodes and restores those cuttings in one
  group; Ctrl+Y undoes that group. Ordinary edits clear the hint, so it is not
  a second general-purpose history.

## Time travel

The version-history primitives are:

| | |
| --- | --- |
| `oplog_frontiers()` | a point in history, encodable to bytes |
| `checkout(&frontiers)` | move the document there — read-only, "detached" |
| `attach()` | back to the latest |
| `revert_to(&frontiers)` | create *new* operations undoing back to that point |
| `fork_at(&frontiers)` | branch it into a separate document |

`checkout` for browsing, `revert_to` for restoring — the latter converges with
peers rather than rewriting history underneath them.

## The thing that will bite you

**Convergence is not invariant preservation.**

Loro guarantees both replicas reach the same state. It guarantees nothing about
whether that state makes sense to *your* application. One peer deleting a group
while another adds a child to it converges — to something your code has to have
an opinion about.

So every invariant is either structural (depth is derived from the tree, so an
illegal depth cannot be represented) or re-checked after `import`.
`orphan_policy_is_pinned_down` in `crates/koru-core/tests/convergence.rs` exists
for exactly this class of bug, which is easy to miss *because* the CRDT is
correct.

## Where to look

An empty named root container is not necessarily represented by an operation.
In the frozen Cloud Profile Private fixture, the separately captured snapshot
declares an empty `reminders` map that reconstruction from the base plus both
segments does not declare, despite equal VVs. Tests comparing exported content
must use the actual captured chain as the expected state, not assume a separate
same-VV snapshot has an identical root-container inventory. The Cloud export
tests compare both VV and deep content after replaying that exact chain.

Cloud causal admission must inspect `ImportStatus.pending`, not only `Ok`.
An import may both apply an independent subset and retain missing-ancestor ops.
The Cloud Store boundary preflights on a disposable fork and rejects the whole
input before live/journal mutation. On success it imports into the original
object: replacing that object with the fork could discard pending device ops.
`cloud_preflight_preserves_older_device_pending_operations` pins this distinction;
the explicit SQLite Cloud disk lane verifies refusal, ancestor arrival, retry
and reopen for both documents and workspace manifests.

The pinned 1.13.9 public API has no query for parked missing-ancestor imports on
an existing `LoroDoc`. `get_pending_txn_len()` counts local uncommitted operations,
not pending imported dependencies. An attached document with equal `oplog_vv()`
and `state_vv()` proves the coherently applied state, not that all parked device
updates are absent. A fork can omit those pending updates, so inspecting it
cannot establish synchronization completeness. Read-only projections must state
this limitation; import/claim authority must retain its separate causal checks.

### Export length is known after allocation

The pinned Rust `LoroDoc::export(&self, ExportMode)` returns
`Result<Vec<u8>, LoroEncodeError>`; its public export API does not accept a
bounded writer or an output-length limit. Verified against generated local
rustdoc/source for 1.13.9, not the JavaScript API. Reserving a Cloud payload
allowance before calling it bounds admitted retained payloads, not the codec's
working heap or an oversized result's transient allocation. A post-export
length check is the 16 MiB protocol gate, not a memory-allocation proof.
The client retains actual Vec capacity (including any excess over its
pre-reservation) through the Store reply and upload guard. Production capacity
measurement remains required; no streaming codec or alternative CRDT format
was invented.

### Version-vector bytes are commitments, not semantic identities

With the workspace-pinned `loro`/`loro-internal` 1.13.9,
`VersionVector::decode(original).encode()` can produce different bytes for an
equal multi-peer vector. A reproduced example is:

```text
original:  03bb1702a31f02c2a9bd9e88e48bff1c0c
reencoded: 03bb1702c2a9bd9e88e48bff1c0ca31f02
```

Both decode to the same vector. The Plan 20 artifact's real Loro probe
reproduced this for Profile Identity and Personal vectors across twelve fresh
processes; it does not attribute the observed ordering to randomized hashing.
The first narrower Manifest-only sample did not reproduce the difference.

Use decoded equality/inclusion for causal proof. A SHA-256 receipt commits the
exact captured encoding bytes, not a universal semantic version hash. Retain
the previous receipt's exact result bytes as the next segment's predecessor;
capture a new result encoding once for the intent/header/receipt. Do not
re-encode a semantically equal live vector and assume its digest must match,
and do not normalize stored payload or envelope bytes to force equality.
This library finding is not production Cloud durability or capacity evidence.

- `crates/koru-core/src/schema.rs` — every Loro call in the project
- `crates/koru-core/tests/convergence.rs` — what merging actually guarantees
- `cargo run -p koru-core --example two_peers` — watch it happen
- [loro.dev](https://loro.dev) — upstream documentation

## Encoded version vectors are not canonical

`VersionVector::encode()` preserves an internal peer ordering that can differ
between a document and its fork. In a two-peer in-memory probe, all 1,000 forks
had semantically equal vectors but different encoded bytes. Concrete encodings
`[2, 2, 2, 1, 2]` and `[2, 1, 2, 2, 2]` decode to equal vectors.

Compare decoded `VersionVector` values (or causal inclusion when that is the
question), never raw encoded bytes. Automatic history previously compared the
commit vector's bytes with the captured fork's bytes; an unchanged page could
therefore be captured on every subsequent idle tick. Its schedule now retains
typed vectors, decoded at the commit boundary. Storage and API encodings, history
intervals, and timestamps remain unchanged. The fixed byte-pair regression lives
beside `StoreSource::automatic_history` in `apps/koru-store-server/src/projection.rs`.
