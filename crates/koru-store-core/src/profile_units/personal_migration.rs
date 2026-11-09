// SPDX-License-Identifier: MPL-2.0

//! The history-preserving migration of a Workspace's legacy personal units
//! into one personal v2 unit — `docs/features/root-catalog/TECHNICAL.final.md`
//! §5.2, step by step, as a pure function over bytes.
//!
//! A v1 unit is `(owner Profile, Workspace)`; every Store stamped its one
//! global `peer_id` on each of them, so two v1 units of one Workspace can
//! both hold `0@P` with different content. A plain Loro union would hide one
//! operation behind the other. So, before anything is imported:
//!
//! 1. every source is validated strictly as v1 against its own address, and
//!    its content child containers are checked to be the *mergeable* ones
//!    the content code writes — an ordinary child map at a shared key would
//!    shadow the other sources' map instead of merging with it;
//! 2. every original PeerID of every source gets its own deterministic
//!    namespace: the first 64 bits, big-endian, of BLAKE3 over
//!    [`PEER_DIGEST_DOMAIN`], the owner UUID, the Workspace UUID and the
//!    PeerID (big-endian). The same source migrated on two devices yields
//!    the same operation ids, so the common history deduplicates later;
//! 3. that bijection is applied through Loro's typed JSON history to change
//!    ids, dependencies, frontiers and container ids. Counters, Lamport
//!    values, keys, values, deletes and mergeable root identities are
//!    untouched. An op this transcoder does not understand is refused as a
//!    whole — a personal unit holds only map operations, so anything else is
//!    not a personal unit;
//! 4. the normalized histories are imported into one fresh candidate whose
//!    writer is a fresh unique peer, checking each import actually applied;
//! 5. `meta.schema_version = 2` is appended under that writer, so it wins the
//!    register against every legacy `1`, the result is validated as v2, and
//!    the due index is rebuilt from the merged rules.
//!
//! Nothing here touches a port. The SQLite migration calls
//! [`migrate_personal_sources`] once per Workspace inside its one
//! transaction; the Store facade calls [`admit_legacy_source`] when a v1
//! snapshot arrives after the upgrade (§5.2 step 6) — the same normalization,
//! never a second live v1 path.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use loro::{
    ContainerID, ContainerTrait, ContainerType, Frontiers, ID, IdSpan, JsonChange, JsonMapOp,
    JsonOp, JsonOpContent, JsonSchema, LoroDoc, LoroMap, LoroValue, ValueOrContainer,
};
use serde::{Deserialize, Serialize};

use super::content::{
    C_PERSONAL_ATTRIBUTES, C_REMINDER_ACTIONS, C_REMINDER_CLEARED_ACTIONS, C_REMINDERS,
};
use super::{PersonalManifest, PersonalScope, PersonalUnitRef};
use crate::errors::{Code, Error};
use crate::ids::{ProfileId, WorkspaceId};
use crate::ports::WorkspaceReminderDue;
use crate::unit_doc::{UnitAddress, entries, op_kind};
use crate::version::PERSONAL_V2_CRDT_SCHEMA_VERSION;

/// The fixed domain string of the PeerID digest (§5.2 step 2). Recorded in
/// every receipt so a reader of the receipt can recompute the mapping.
pub const PEER_DIGEST_DOMAIN: &str = "koru-personal-v2-peer";

/// One legacy `profile_personal_crdts` row: a complete v1 snapshot under the
/// `(owner, workspace)` it was filed under. Removed former Profiles included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyPersonalSource {
    pub owner: ProfileId,
    pub workspace: WorkspaceId,
    pub snapshot: Vec<u8>,
}

/// Why a migration was refused. Typed so the SQL step and its tests branch on
/// the reason; every variant names the source by its owner, never content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonalMigrationRefusal {
    /// Durable bytes did not decode as a Loro snapshot.
    UndecodableSnapshot { owner: ProfileId },
    /// The snapshot's history has gaps; a stored v1 unit is always complete.
    PendingAncestors { owner: ProfileId },
    /// The v1 validator refused the snapshot against its own address.
    InvalidSource { owner: ProfileId, detail: String },
    /// The header names another `(owner, workspace)` than the row.
    WrongAddress { owner: ProfileId },
    /// The source is filed under a different Workspace than the one being
    /// migrated; sources of two Workspaces are never unioned.
    WrongWorkspace {
        owner: ProfileId,
        found: WorkspaceId,
    },
    /// A newer build wrote this unit; this build cannot migrate what it
    /// cannot validate.
    FutureSchema { owner: ProfileId, found: i64 },
    /// A content child container at a shared key is not the mergeable map
    /// the content code creates, so unioning it would hide another source's.
    NonMergeableChild { owner: ProfileId, path: String },
    /// The exported history carried a compressed peer table; the transcoder
    /// works on explicit peers only.
    PeerCompression { owner: ProfileId },
    /// An operation kind this transcoder does not understand. Nothing is
    /// partially rewritten.
    UnsupportedOp {
        owner: ProfileId,
        kind: &'static str,
    },
    /// A container id shape this transcoder does not understand.
    UnsupportedContainer { owner: ProfileId, container: String },
    /// An id referenced a peer the source's own version vector does not hold.
    UnmappedPeer { owner: ProfileId, peer: u64 },
    /// Two different inputs digest to the same 64-bit peer.
    PeerCollision { derived: u64 },
    /// A digest landed on the one PeerID Loro refuses.
    ReservedPeer { owner: ProfileId, original: u64 },
    /// One source holds two changes under one normalized id: corrupt input.
    DuplicateChangeId { owner: ProfileId, id: String },
    /// A normalized operation already exists in the target with different
    /// content; the namespace transform does not pretend to repair it.
    ConflictingHistory { owner: ProfileId, id: String },
    /// A normalized history did not fully apply to the candidate.
    ImportPending { owner: ProfileId },
    /// The merged candidate failed v2 validation.
    InvalidResult { detail: String },
    /// A Loro operation failed. Not a validation outcome.
    Crdt,
}

impl fmt::Display for PersonalMigrationRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use PersonalMigrationRefusal as R;
        match self {
            R::UndecodableSnapshot { owner } => {
                write!(f, "personal source of {owner} is not a Loro snapshot")
            }
            R::PendingAncestors { owner } => {
                write!(f, "personal source of {owner} has missing causal ancestors")
            }
            R::InvalidSource { owner, detail } => {
                write!(
                    f,
                    "personal source of {owner} failed v1 validation: {detail}"
                )
            }
            R::WrongAddress { owner } => {
                write!(
                    f,
                    "personal source of {owner} names another unit in its header"
                )
            }
            R::WrongWorkspace { owner, found } => write!(
                f,
                "personal source of {owner} belongs to workspace {found}, not the one migrated"
            ),
            R::FutureSchema { owner, found } => write!(
                f,
                "personal source of {owner} has format {found}, newer than this build"
            ),
            R::NonMergeableChild { owner, path } => write!(
                f,
                "personal source of {owner} holds a non-mergeable child at {path}"
            ),
            R::PeerCompression { owner } => {
                write!(
                    f,
                    "personal source of {owner} exported a compressed peer table"
                )
            }
            R::UnsupportedOp { owner, kind } => write!(
                f,
                "personal source of {owner} holds an unsupported {kind} operation"
            ),
            R::UnsupportedContainer { owner, container } => write!(
                f,
                "personal source of {owner} references an unsupported container {container}"
            ),
            R::UnmappedPeer { owner, peer } => write!(
                f,
                "personal source of {owner} references peer {peer} outside its history"
            ),
            R::PeerCollision { derived } => {
                write!(f, "two personal peers digest to the same id {derived}")
            }
            R::ReservedPeer { owner, original } => write!(
                f,
                "personal peer {original} of {owner} digests to a reserved id"
            ),
            R::DuplicateChangeId { owner, id } => {
                write!(f, "personal source of {owner} holds two changes at {id}")
            }
            R::ConflictingHistory { owner, id } => write!(
                f,
                "personal source of {owner} conflicts with held history at {id}"
            ),
            R::ImportPending { owner } => {
                write!(f, "normalized personal history of {owner} did not apply")
            }
            R::InvalidResult { detail } => {
                write!(f, "migrated personal unit failed v2 validation: {detail}")
            }
            R::Crdt => write!(f, "personal migration CRDT operation failed"),
        }
    }
}

impl From<PersonalMigrationRefusal> for Error {
    fn from(refusal: PersonalMigrationRefusal) -> Self {
        use PersonalMigrationRefusal as R;
        let code = match &refusal {
            R::UndecodableSnapshot { .. }
            | R::PendingAncestors { .. }
            | R::InvalidSource { .. }
            | R::NonMergeableChild { .. }
            | R::DuplicateChangeId { .. }
            | R::InvalidResult { .. } => Code::CorruptState,
            R::WrongAddress { .. } | R::WrongWorkspace { .. } => Code::ProfileUnitScopeMismatch,
            R::FutureSchema { .. } => Code::SchemaTooNew,
            R::PeerCompression { .. }
            | R::UnsupportedOp { .. }
            | R::UnsupportedContainer { .. }
            | R::UnmappedPeer { .. }
            | R::PeerCollision { .. }
            | R::ReservedPeer { .. }
            | R::ConflictingHistory { .. }
            | R::ImportPending { .. } => Code::CrdtRejected,
            R::Crdt => Code::StorageFailed,
        };
        Error::new(code, refusal.to_string())
    }
}

type Result<T> = std::result::Result<T, PersonalMigrationRefusal>;

/// One `(original peer → derived peer)` of the receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerMapping {
    pub original: u64,
    pub derived: u64,
}

/// What one source contributed: its address, its size and the id mapping
/// that was applied to it. Ids only; no content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceReceipt {
    pub owner: ProfileId,
    pub workspace: WorkspaceId,
    pub change_count: usize,
    pub op_count: usize,
    /// `counter@peer` of the source's frontiers before normalization.
    pub original_frontiers: Vec<String>,
    /// The same frontiers after normalization.
    pub normalized_frontiers: Vec<String>,
    pub peers: Vec<PeerMapping>,
}

/// The receipt of one Workspace's migration, stored by SQL as JSON text in
/// `root_catalog_migration_receipts` under `kind = 'personal_v2'`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonalMigrationReceipt {
    pub workspace: WorkspaceId,
    pub digest_domain: String,
    pub writer_peer_id: u64,
    pub sources: Vec<SourceReceipt>,
}

/// The v2 unit one Workspace's sources became, ready for its
/// `workspace_personal_crdts` row and its `workspace_reminder_due` rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigratedPersonalUnit {
    pub snapshot: Vec<u8>,
    pub writer_peer_id: u64,
    pub due_rows: Vec<WorkspaceReminderDue>,
    /// The encoded version vector the due rows were built from.
    pub indexed_version: Vec<u8>,
    pub receipt: PersonalMigrationReceipt,
}

/// The §5.2 step 2 digest: the first 64 bits, big-endian, of BLAKE3 over
/// the domain, the owner UUID (16 bytes), the Workspace UUID (16 bytes) and
/// the original PeerID (8 bytes, big-endian).
pub fn derive_peer(owner: ProfileId, workspace: WorkspaceId, original: u64) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PEER_DIGEST_DOMAIN.as_bytes());
    hasher.update(owner.as_uuid().as_bytes());
    hasher.update(workspace.as_uuid().as_bytes());
    hasher.update(&original.to_be_bytes());
    let digest = hasher.finalize();
    let mut eight = [0u8; 8];
    eight.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_be_bytes(eight)
}

/// One source after steps 1–3: its history in Loro's typed JSON with every
/// id in its own namespace, and the receipt of what was done to it.
pub struct NormalizedSource {
    pub owner: ProfileId,
    pub json: JsonSchema,
    pub receipt: SourceReceipt,
}

/// Steps 1–3 for one source. Pure; refuses rather than partially rewrites.
pub fn normalize_legacy_source(
    source: &LegacyPersonalSource,
    workspace: WorkspaceId,
) -> Result<NormalizedSource> {
    use PersonalMigrationRefusal as R;
    let owner = source.owner;
    if source.workspace != workspace {
        return Err(R::WrongWorkspace {
            owner,
            found: source.workspace,
        });
    }
    // Step 1a: decode on a probe first, so the typed reasons stay apart —
    // the manifest loader folds "undecodable" and "pending" into one code.
    let probe = LoroDoc::new();
    let status = probe
        .import(&source.snapshot)
        .map_err(|_| R::UndecodableSnapshot { owner })?;
    if status.pending.is_some() {
        return Err(R::PendingAncestors { owner });
    }
    // Step 1b: strictly v1, against the address the header itself names.
    let manifest =
        PersonalManifest::from_snapshot(&source.snapshot).map_err(|error| R::InvalidSource {
            owner,
            detail: error.message().to_string(),
        })?;
    if manifest.owner() != Some(owner) || manifest.workspace() != workspace {
        return Err(R::WrongAddress { owner });
    }
    if let crate::version::Compatibility::TooNew { found, .. } = manifest.compatibility() {
        return Err(R::FutureSchema { owner, found });
    }
    let doc = &manifest.inner.doc;
    check_mergeable_children(doc).map_err(|path| R::NonMergeableChild { owner, path })?;

    // Step 2: one derived namespace per original peer of this source.
    let mut peers = BTreeMap::new();
    for (peer, counter) in doc.oplog_vv().iter() {
        if *counter <= 0 {
            continue;
        }
        let derived = derive_peer(owner, workspace, *peer);
        if derived == u64::MAX {
            return Err(R::ReservedPeer {
                owner,
                original: *peer,
            });
        }
        peers.insert(*peer, derived);
    }
    let mut seen = BTreeSet::new();
    for derived in peers.values() {
        if !seen.insert(*derived) {
            return Err(R::PeerCollision { derived: *derived });
        }
    }

    // Step 3: the bijection through Loro's own schema.
    let exported = doc.export_json_updates_without_peer_compression(
        &loro::VersionVector::default(),
        &doc.oplog_vv(),
    );
    let original_frontiers = ids_to_strings(doc.oplog_frontiers().iter());
    let (json, op_count) = transcode(exported, &peers, owner)?;
    check_unique_change_ids(&json, owner)?;
    let normalized_frontiers = ids_to_strings(json.start_version.iter().chain(frontier_ids(&json)));
    let receipt = SourceReceipt {
        owner,
        workspace,
        change_count: json.changes.len(),
        op_count,
        original_frontiers,
        normalized_frontiers,
        peers: peers
            .iter()
            .map(|(original, derived)| PeerMapping {
                original: *original,
                derived: *derived,
            })
            .collect(),
    };
    Ok(NormalizedSource {
        owner,
        json,
        receipt,
    })
}

/// Steps 1–5 for every source of one Workspace, in canonical Profile order.
/// `writer_peer` is a fresh unique peer minted the way the Store mints its
/// own; it becomes the unit's persisted writer identity.
pub fn migrate_personal_sources(
    workspace: WorkspaceId,
    mut sources: Vec<LegacyPersonalSource>,
    writer_peer: u64,
) -> Result<MigratedPersonalUnit> {
    sources.sort_by_key(|source| source.owner);
    let mut normalized = Vec::with_capacity(sources.len());
    let mut derived_peers: BTreeSet<u64> = BTreeSet::new();
    for source in &sources {
        let source = normalize_legacy_source(source, workspace)?;
        // A collision between two *different* inputs anywhere in the batch
        // refuses the whole migration; one owner's own mapping is checked
        // inside `normalize_legacy_source`.
        for mapping in &source.receipt.peers {
            if !derived_peers.insert(mapping.derived) {
                return Err(PersonalMigrationRefusal::PeerCollision {
                    derived: mapping.derived,
                });
            }
        }
        normalized.push(source);
    }
    let candidate = assemble(workspace, &normalized, writer_peer)?;
    let (snapshot, due_rows, indexed_version) = finish(&candidate)?;
    Ok(MigratedPersonalUnit {
        snapshot,
        writer_peer_id: writer_peer,
        due_rows,
        indexed_version,
        receipt: PersonalMigrationReceipt {
            workspace,
            digest_domain: PEER_DIGEST_DOMAIN.to_string(),
            writer_peer_id: writer_peer,
            sources: normalized
                .into_iter()
                .map(|source| source.receipt)
                .collect(),
        },
    })
}

/// §5.2 step 6: a v1 snapshot that arrives after this device migrated. The
/// source goes through the same normalization, its normalized operations are
/// compared against the held history (an id the unit already holds must
/// carry the same operation), and the result is staged into `existing` as
/// one validated candidate. `fresh_writer` authors the candidate's own v2
/// header; it must not be `existing`'s writer, whose counters are taken.
pub fn admit_legacy_source(
    existing: &PersonalManifest,
    source: &LegacyPersonalSource,
    fresh_writer: u64,
) -> crate::errors::Result<(PersonalManifest, SourceReceipt)> {
    let workspace = existing.workspace();
    let normalized = normalize_legacy_source(source, workspace)?;
    detect_conflicts(&existing.inner.doc, &normalized)?;
    let candidate = assemble(workspace, std::slice::from_ref(&normalized), fresh_writer)?;
    let (snapshot, _, _) = finish(&candidate)?;
    let merged = existing.staged_import(&[snapshot])?;
    Ok((merged, normalized.receipt))
}

/// Refuse a normalized history whose operations the held document already
/// has under the same ids with different content. Loro would silently keep
/// the held ones; §5.2 step 4 says a corrupt origin is refused, not hidden.
pub fn detect_conflicts(held: &LoroDoc, incoming: &NormalizedSource) -> Result<()> {
    let held_vv = held.oplog_vv();
    for change in &incoming.json.changes {
        let peer = change.id.peer;
        let Some(have) = held_vv.get(&peer).copied() else {
            continue;
        };
        let start = change.id.counter;
        let end = start + change.op_len() as i32;
        let overlap_end = end.min(have);
        if overlap_end <= start {
            continue;
        }
        let held_changes = held.export_json_in_id_span(IdSpan::new(peer, start, overlap_end));
        let held_ops = ops_by_counter(&held_changes, start, overlap_end);
        let incoming_ops = ops_by_counter(std::slice::from_ref(change), start, overlap_end);
        if held_ops != incoming_ops {
            return Err(PersonalMigrationRefusal::ConflictingHistory {
                owner: incoming.owner,
                id: change.id.to_string(),
            });
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ steps

/// Step 1's mergeable-identity check: every child container the content
/// code creates with `ensure_mergeable_map` must carry the id that call
/// derives from its parent and key. Answers the offending path.
fn check_mergeable_children(doc: &LoroDoc) -> std::result::Result<(), String> {
    let attributes = doc.get_map(C_PERSONAL_ATTRIBUTES);
    for (key, value) in entries(&attributes) {
        expect_mergeable(&attributes, &key, value, C_PERSONAL_ATTRIBUTES)?;
    }
    let reminders = doc.get_map(C_REMINDERS);
    for (key, value) in entries(&reminders) {
        let child = expect_mergeable(&reminders, &key, value, C_REMINDERS)?;
        if key == C_REMINDER_ACTIONS || key == C_REMINDER_CLEARED_ACTIONS {
            let path = format!("{C_REMINDERS}.{key}");
            for (reminder, value) in entries(&child) {
                expect_mergeable(&child, &reminder, value, &path)?;
            }
        }
    }
    Ok(())
}

fn expect_mergeable(
    parent: &LoroMap,
    key: &str,
    value: ValueOrContainer,
    parent_path: &str,
) -> std::result::Result<LoroMap, String> {
    let path = format!("{parent_path}.{key}");
    let ValueOrContainer::Container(container) = value else {
        return Err(path);
    };
    if container.id() != ContainerID::new_mergeable(&parent.id(), key, ContainerType::Map) {
        return Err(path);
    }
    container.into_map().map_err(|_| path)
}

fn ids_to_strings<'a>(ids: impl Iterator<Item = ID> + 'a) -> Vec<String> {
    let mut out: Vec<String> = ids.map(|id| id.to_string()).collect();
    out.sort();
    out.dedup();
    out
}

/// The heads of a normalized history: every change id that no other change
/// in it depends on. Sufficient for a receipt; Loro recomputes its own.
fn frontier_ids(json: &JsonSchema) -> impl Iterator<Item = ID> + '_ {
    let depended: BTreeSet<(u64, i32)> = json
        .changes
        .iter()
        .flat_map(|change| change.deps.iter().map(|dep| (dep.peer, dep.counter)))
        .collect();
    json.changes.iter().filter_map(move |change| {
        let last = ID::new(
            change.id.peer,
            change.id.counter + change.op_len().max(1) as i32 - 1,
        );
        (!depended.contains(&(last.peer, last.counter))).then_some(last)
    })
}

/// Step 3. Every id-bearing field of the supported schema is rewritten
/// through `peers`; everything else is copied. Answers the op count too.
fn transcode(
    json: JsonSchema,
    peers: &BTreeMap<u64, u64>,
    owner: ProfileId,
) -> Result<(JsonSchema, usize)> {
    use PersonalMigrationRefusal as R;
    if json.peers.is_some() {
        return Err(R::PeerCompression { owner });
    }
    let map_peer = |peer: u64| -> Result<u64> {
        peers
            .get(&peer)
            .copied()
            .ok_or(R::UnmappedPeer { owner, peer })
    };
    let map_id = |id: &ID| -> Result<ID> { Ok(ID::new(map_peer(id.peer)?, id.counter)) };
    let mut start_version = Frontiers::default();
    for id in json.start_version.iter() {
        start_version.push(map_id(&id)?);
    }
    let mut op_count = 0;
    let mut changes = Vec::with_capacity(json.changes.len());
    for change in json.changes {
        let mut ops = Vec::with_capacity(change.ops.len());
        for op in change.ops {
            let container = transcode_container(op.container, &map_peer, owner)?;
            let content = match op.content {
                JsonOpContent::Map(JsonMapOp::Insert { key, value }) => {
                    JsonOpContent::Map(JsonMapOp::Insert {
                        key,
                        value: transcode_value(value, &map_peer, owner)?,
                    })
                }
                JsonOpContent::Map(JsonMapOp::Delete { key }) => {
                    JsonOpContent::Map(JsonMapOp::Delete { key })
                }
                other => {
                    return Err(R::UnsupportedOp {
                        owner,
                        kind: op_kind(&other),
                    });
                }
            };
            op_count += 1;
            ops.push(JsonOp {
                content,
                container,
                counter: op.counter,
            });
        }
        let mut deps = Vec::with_capacity(change.deps.len());
        for dep in &change.deps {
            deps.push(map_id(dep)?);
        }
        changes.push(JsonChange {
            id: map_id(&change.id)?,
            timestamp: change.timestamp,
            deps,
            lamport: change.lamport,
            msg: change.msg,
            ops,
        });
    }
    Ok((
        JsonSchema {
            schema_version: json.schema_version,
            start_version,
            peers: None,
            changes,
        },
        op_count,
    ))
}

/// A root container is a name and stays; an ordinary container is an op id
/// and is mapped; a mergeable container encodes its nearest non-mergeable
/// ancestor, which is mapped recursively when it is an ordinary container.
fn transcode_container(
    container: ContainerID,
    map_peer: &impl Fn(u64) -> Result<u64>,
    owner: ProfileId,
) -> Result<ContainerID> {
    match container {
        ContainerID::Normal {
            peer,
            counter,
            container_type,
        } => Ok(ContainerID::Normal {
            peer: map_peer(peer)?,
            counter,
            container_type,
        }),
        root @ ContainerID::Root { .. } => match root.parse_mergeable() {
            Some((parent, key, kind)) => {
                if parent.container_type() != ContainerType::Map {
                    return Err(PersonalMigrationRefusal::UnsupportedContainer {
                        owner,
                        container: root.to_string(),
                    });
                }
                let parent = transcode_container(parent, map_peer, owner)?;
                Ok(ContainerID::new_mergeable(&parent, &key, kind))
            }
            None => Ok(root),
        },
    }
}

/// A map value is atomic in a personal unit; a container reference inside
/// one is still mapped so no id form is left half-rewritten.
fn transcode_value(
    value: LoroValue,
    map_peer: &impl Fn(u64) -> Result<u64>,
    owner: ProfileId,
) -> Result<LoroValue> {
    Ok(match value {
        LoroValue::Container(container) => {
            LoroValue::Container(transcode_container(container, map_peer, owner)?)
        }
        LoroValue::List(list) => {
            let mut out = Vec::with_capacity(list.len());
            for item in list.iter() {
                out.push(transcode_value(item.clone(), map_peer, owner)?);
            }
            LoroValue::from(out)
        }
        LoroValue::Map(map) => {
            let mut out: std::collections::HashMap<String, LoroValue> =
                std::collections::HashMap::with_capacity(map.len());
            for (key, item) in map.iter() {
                out.insert(key.clone(), transcode_value(item.clone(), map_peer, owner)?);
            }
            LoroValue::from(out)
        }
        other => other,
    })
}

/// Within one source every normalized change must own a distinct counter
/// range of its peer. A Loro snapshot cannot hold two changes at one id, so
/// a duplicate here is corrupt input, refused rather than deduplicated.
fn check_unique_change_ids(json: &JsonSchema, owner: ProfileId) -> Result<()> {
    let mut spans: BTreeMap<u64, Vec<(i32, i32)>> = BTreeMap::new();
    for change in &json.changes {
        let start = change.id.counter;
        let end = start + change.op_len().max(1) as i32;
        spans.entry(change.id.peer).or_default().push((start, end));
    }
    for (peer, ranges) in spans.iter_mut() {
        ranges.sort();
        for pair in ranges.windows(2) {
            if pair[1].0 < pair[0].1 {
                return Err(PersonalMigrationRefusal::DuplicateChangeId {
                    owner,
                    id: ID::new(*peer, pair[1].0).to_string(),
                });
            }
        }
    }
    Ok(())
}

/// Step 4: one fresh candidate under the new writer, every normalized
/// history imported and checked to have applied.
fn assemble(
    workspace: WorkspaceId,
    sources: &[NormalizedSource],
    writer_peer: u64,
) -> Result<PersonalManifest> {
    let candidate = PersonalManifest::create_v2(workspace, writer_peer)
        .map_err(|_| PersonalMigrationRefusal::Crdt)?;
    for source in sources {
        let status = candidate
            .inner
            .doc
            .import_json_updates(source.json.clone())
            .map_err(|_| PersonalMigrationRefusal::ImportPending {
                owner: source.owner,
            })?;
        if status.pending.is_some() {
            return Err(PersonalMigrationRefusal::ImportPending {
                owner: source.owner,
            });
        }
    }
    Ok(candidate)
}

/// Step 5: the v2 stamp under the writer, validation as v2, the due index.
fn finish(candidate: &PersonalManifest) -> Result<(Vec<u8>, Vec<WorkspaceReminderDue>, Vec<u8>)> {
    let doc = &candidate.inner.doc;
    // Appended after every import so its Lamport exceeds every legacy `1`
    // and the register reads 2 on every replica that merges this history.
    doc.get_map("meta")
        .insert("schema_version", PERSONAL_V2_CRDT_SCHEMA_VERSION)
        .map_err(|_| PersonalMigrationRefusal::Crdt)?;
    doc.commit();
    let scope = PersonalScope::Workspace(PersonalUnitRef {
        workspace: candidate.workspace(),
    });
    match scope.validate(doc) {
        Ok(compatibility) if compatibility.writable() => {}
        Ok(compatibility) => {
            return Err(PersonalMigrationRefusal::InvalidResult {
                detail: compatibility.to_string(),
            });
        }
        Err(error) => {
            return Err(PersonalMigrationRefusal::InvalidResult {
                detail: error.message().to_string(),
            });
        }
    }
    let workspace = candidate.workspace();
    let due_rows = candidate
        .reminder_due_hints()
        .into_iter()
        .map(|(reminder, due_at_ms)| WorkspaceReminderDue {
            workspace,
            reminder,
            due_at_ms,
        })
        .collect();
    let snapshot = candidate
        .export_snapshot()
        .map_err(|_| PersonalMigrationRefusal::Crdt)?;
    Ok((snapshot, due_rows, candidate.version().encode()))
}

/// The operations of `changes` whose counters fall in `[start, end)`, keyed
/// by counter, as comparable JSON. Container and content only: that is the
/// payload, and a sliced change's Lamport and deps are not.
fn ops_by_counter(
    changes: &[JsonChange],
    start: i32,
    end: i32,
) -> BTreeMap<i32, (String, serde_json::Value)> {
    let mut out = BTreeMap::new();
    for change in changes {
        for op in &change.ops {
            if op.counter >= start && op.counter < end {
                out.insert(
                    op.counter,
                    (
                        op.container.to_string(),
                        serde_json::to_value(&op.content).unwrap_or(serde_json::Value::Null),
                    ),
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
