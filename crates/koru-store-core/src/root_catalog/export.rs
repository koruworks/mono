// SPDX-License-Identifier: MPL-2.0

//! The one export boundary every root-catalog and personal unit crosses on
//! its way to a recipient — `docs/features/root-catalog/TECHNICAL.final.md`
//! §4.3 "Safe history export without a new lineage", as a pure function over
//! bytes.
//!
//! There is one authoritative history per unit. A recipient that may not
//! know a Profile exists (LocalOnly or P2P-only on this device while the
//! path is Cloud, or otherwise ineligible) must not learn its UUID from a
//! unit that is eligible: a historical `placement.current` that once named
//! it, or the `meta.owner_profile_id` a personal v2 unit inherited from its
//! v1 history. Deleting the key does not remove it from history, and a
//! reseeded or shallow copy would start a second lineage. So the history is
//! projected instead:
//!
//! 1. the authoritative snapshot is loaded and validated as the unit it
//!    claims to be — nothing pending, nothing this build cannot read;
//! 2. its complete history is exported as Loro's typed JSON, without peer
//!    compression, and every operation is walked: a map insert at
//!    `placement.current` or `meta.owner_profile_id` whose value names a
//!    Profile the [`ExportScope`] cannot see is marked by its exact
//!    `[counter, counter + 1)` span; any operation that is not a map
//!    insert or delete on a root the unit's validator allows refuses the
//!    whole export — the library's leave-unknown-ops-unchanged behavior is
//!    not relied on;
//! 3. every change message is cleared (new catalog commits carry none);
//! 4. `loro::json::redact` is applied once per marked span, never to a
//!    broader interval, so lifecycle facts and personal content around a
//!    hidden reference are untouched;
//! 5. the redacted JSON is imported into a disposable document, which must
//!    accept it completely and validate under the same validator;
//! 6. the derived document's effective state is compared with the
//!    authoritative one: current placement, deletions, restores, purged,
//!    personal content. A difference the redaction *had* to make — the
//!    current placement itself names a hidden Profile — is a [`Derived::Hold`],
//!    never an invented unassigned value; any other difference is a refusal;
//! 7. ordinary binary bytes are exported for the recipient: a snapshot, or
//!    updates since the recipient's version vector.
//!
//! What `json::redact` does to a marked op, read from the pinned
//! `loro-internal-1.13.9/src/encoding/json_schema.rs`: the op stays, with its
//! id, Lamport, dependencies, container and **key**; only its value becomes
//! `LoroValue::Null`. For `placement.current` that is the whole atomic
//! `{profile_id, move_id}` record, so a redacted historical placement reads
//! as a bare null — acceptable only there (§4.3). A reader that already held
//! the value keeps it: Loro ignores an op it already has, so this prevents
//! new disclosure and erases nothing remotely.
//!
//! Derived bytes have no writer and need no durable copy. The caller may
//! cache a derived snapshot by [`DerivedKey`] and export any recipient's
//! updates from it; nothing is cached here. The caller also owns §4.3 steps
//! 1 and 5 — capturing the recipient's authorization and the local policy
//! generation before deriving, and rechecking both before sending: the
//! [`PolicyCapture`] it hands in is echoed back unchanged in the
//! [`Publication`] for exactly that comparison. Every resend, blob route,
//! bundle-like fallback and forwarding hop goes through [`derive`]; a
//! refusal here is never answered with the raw bytes.

use std::collections::BTreeMap;
use std::fmt;

use loro::{
    ContainerID, ContainerType, ExportMode, JsonMapOp, JsonOpContent, JsonSchema, LoroDoc,
    LoroValue, ValueOrContainer, VersionRange, VersionVector,
};

use super::lifecycle::{DeletionRecord, Facts, Placement, PurgeRecord, RestoreRecord};
use super::{
    CatalogUnitRef, K_CURRENT, R_DELETIONS, R_META, R_PLACEMENT, R_PURGED, R_RESTORES, validation,
};
use crate::errors::{Code, Error};
use crate::ids::{DeletionId, ProfileId, RestoreId, WorkspaceId};
use crate::profile_units::content::{C_PERSONAL_ATTRIBUTES, C_REMINDERS};
use crate::profile_units::{PersonalScope, PersonalUnitRef};
use crate::unit_doc::{UnitAddress, non_mergeable_ancestor, op_kind};
use crate::version::Compatibility;

/// The legacy owner key of a personal v2 unit (§5.1): a string inherited
/// from v1 history or a redaction null, never authority.
const K_OWNER_PROFILE_ID: &str = "owner_profile_id";

/// Which unit is being exported. A Profile catalog unit names no Profile but
/// its own subject and needs no redaction; it goes through the same function
/// so that every unit kind crosses one boundary.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ExportSubject {
    ProfileCatalog(ProfileId),
    WorkspaceCatalog(WorkspaceId),
    PersonalV2(WorkspaceId),
}

impl ExportSubject {
    /// The named roots the subject's validator allows. An operation on any
    /// other container refuses the export.
    fn roots(self) -> &'static [&'static str] {
        match self {
            Self::ProfileCatalog(_) => &[R_META, R_DELETIONS, R_RESTORES, R_PURGED],
            Self::WorkspaceCatalog(_) => &[R_META, R_PLACEMENT, R_DELETIONS, R_RESTORES, R_PURGED],
            Self::PersonalV2(_) => &[R_META, C_PERSONAL_ATTRIBUTES, C_REMINDERS],
        }
    }

    /// Whether a map insert at `root.key` is the one place this subject
    /// records a reference to another Profile.
    fn is_profile_reference(self, root: &str, key: &str) -> bool {
        match self {
            Self::ProfileCatalog(_) => false,
            Self::WorkspaceCatalog(_) => root == R_PLACEMENT && key == K_CURRENT,
            Self::PersonalV2(_) => root == R_META && key == K_OWNER_PROFILE_ID,
        }
    }

    /// The subject's own validator, unchanged: the root-catalog one for a
    /// catalog unit, the personal v2 one for a personal unit.
    fn validate(self, doc: &LoroDoc) -> std::result::Result<Compatibility, String> {
        match self {
            Self::ProfileCatalog(profile) => {
                validation::validate(doc, CatalogUnitRef::Profile { profile })
                    .map_err(|refusal| refusal.to_string())
            }
            Self::WorkspaceCatalog(workspace) => {
                validation::validate(doc, CatalogUnitRef::Workspace { workspace })
                    .map_err(|refusal| refusal.to_string())
            }
            Self::PersonalV2(workspace) => PersonalScope::Workspace(PersonalUnitRef { workspace })
                .validate(doc)
                .map_err(|error| error.message().to_string()),
        }
    }
}

/// What the caller captured before deriving (§4.3 step 1) and compares
/// again before sending (step 5): the local policy generation and the
/// recipient's authorization, as an opaque token this module never reads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PolicyCapture {
    pub policy_generation: u64,
    pub authorization: String,
}

/// Who the derived bytes are for: the recipient-and-path answer to "may
/// this Profile be named?", plus the capture echoed back for the recheck.
pub struct ExportScope<'a> {
    visible: &'a dyn Fn(ProfileId) -> bool,
    capture: PolicyCapture,
}

impl<'a> ExportScope<'a> {
    /// `visible` answers for the recipient and the path together: a
    /// Profile that is LocalOnly, or P2P-only while the path is Cloud, or
    /// otherwise ineligible for this recipient, is not visible.
    pub fn new(visible: &'a dyn Fn(ProfileId) -> bool, capture: PolicyCapture) -> Self {
        Self { visible, capture }
    }

    pub fn is_profile_visible(&self, profile: ProfileId) -> bool {
        (self.visible)(profile)
    }

    pub fn capture(&self) -> &PolicyCapture {
        &self.capture
    }
}

impl fmt::Debug for ExportScope<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExportScope")
            .field("capture", &self.capture)
            .finish_non_exhaustive()
    }
}

/// The shape of the bytes in a [`Publication`].
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PayloadKind {
    /// A complete derived snapshot; the recipient held nothing.
    Snapshot,
    /// Updates since the recipient's version vector.
    Updates,
}

/// Bytes ready for one recipient, with what the caller needs to send and
/// account for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publication {
    pub bytes: Vec<u8>,
    pub kind: PayloadKind,
    /// How many operations of the *whole* history were redacted, whatever
    /// part of it the bytes carry. Zero for a Profile catalog unit.
    pub redacted_ops: usize,
    /// The recipient's version the bytes start from; empty for a snapshot.
    pub from: VersionVector,
    /// The version the bytes bring the recipient to: the authoritative
    /// unit's, since redaction preserves every operation id.
    pub to: VersionVector,
    /// The caller's [`PolicyCapture`], unchanged, to recheck before sending.
    pub capture: PolicyCapture,
}

/// Why a publication is held rather than exported. A hold is not an error
/// in the unit: redaction would have changed its *current* effective state,
/// and §4.3 forbids exporting an invented value in its place.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum HoldReason {
    /// The Workspace's current placement names a Profile the recipient may
    /// not see. Exporting would read as "unassigned", which nobody wrote.
    CurrentPlacementHidden,
    /// The derived personal unit still names a hidden Profile as its legacy
    /// owner. Unreachable when every insert was identified; kept so the
    /// boundary's last check is the state that leaves, not the walk.
    OwnerHidden,
    /// A deletion, restore or purge fact differs between the derived and
    /// the authoritative unit. Lifecycle is never redacted, so this is the
    /// boundary refusing to publish a lifecycle it cannot vouch for.
    LifecycleDiffers,
}

impl fmt::Display for HoldReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentPlacementHidden => {
                write!(
                    f,
                    "the current placement names a Profile hidden from this path"
                )
            }
            Self::OwnerHidden => {
                write!(f, "the legacy owner names a Profile hidden from this path")
            }
            Self::LifecycleDiffers => {
                write!(
                    f,
                    "the derived lifecycle differs from the authoritative unit"
                )
            }
        }
    }
}

/// A hold surfaced as an error: the publication is not permitted on this
/// path now. Local state is untouched and the hold lifts when policy does.
impl From<HoldReason> for Error {
    fn from(reason: HoldReason) -> Self {
        Error::new(Code::Forbidden, reason.to_string())
    }
}

/// What [`derive`] produced.
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Derived {
    Publish(Publication),
    Hold { reason: HoldReason },
}

impl Derived {
    pub fn publication(self) -> Option<Publication> {
        match self {
            Self::Publish(publication) => Some(publication),
            Self::Hold { .. } => None,
        }
    }
}

/// Why an export was refused. None of these is answered with raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportRefusal {
    /// The authoritative bytes did not decode as a Loro snapshot.
    UndecodableSnapshot,
    /// The authoritative bytes have missing causal ancestors; an
    /// authoritative snapshot is complete, so this is corruption.
    PendingAncestors,
    /// The authoritative snapshot fails its own unit validator.
    SourceInvalid { detail: String },
    /// A newer build wrote this unit; this one cannot judge what its
    /// history names, so it does not project it.
    FormatTooNew { found: i64, understood: i64 },
    /// The history export carried a compressed peer table; spans are
    /// addressed by real peer ids only.
    PeerCompression,
    /// An operation kind other than a map insert or delete.
    UnknownOperation {
        container: String,
        kind: &'static str,
    },
    /// A map operation on a container the subject's validator does not
    /// allow: an unknown root, a non-map root, or an ordinary (op-id) child
    /// container where only mergeable children exist.
    UnknownContainer { container: String },
    /// A historical value at a Profile-reference key that is neither a
    /// placement record, a Profile string nor a redaction null. Writers
    /// never author one; the boundary does not guess what it names.
    MalformedReference { path: String },
    /// `loro::json::redact` refused a span.
    RedactionFailed { detail: String },
    /// The redacted history did not apply completely to the disposable
    /// document, or arrived at another version than the source.
    DerivedPending,
    /// The derived document fails the subject's validator.
    DerivedInvalid { detail: String },
    /// A part of the effective state that redaction must not touch differs.
    DerivedStateDiffers { path: String },
    /// A Loro operation failed. Not a validation outcome.
    Crdt,
}

impl fmt::Display for ExportRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UndecodableSnapshot => write!(f, "authoritative unit snapshot is invalid"),
            Self::PendingAncestors => {
                write!(
                    f,
                    "authoritative unit snapshot has missing causal ancestors"
                )
            }
            Self::SourceInvalid { detail } => {
                write!(f, "authoritative unit failed validation: {detail}")
            }
            Self::FormatTooNew { found, understood } => write!(
                f,
                "unit format {found} is newer than this build's {understood}; not exported"
            ),
            Self::PeerCompression => write!(f, "history export carried a compressed peer table"),
            Self::UnknownOperation { container, kind } => {
                write!(
                    f,
                    "unsupported {kind} operation on {container} in unit history"
                )
            }
            Self::UnknownContainer { container } => {
                write!(f, "unsupported container {container} in unit history")
            }
            Self::MalformedReference { path } => {
                write!(
                    f,
                    "historical {path} is not a recognizable Profile reference"
                )
            }
            Self::RedactionFailed { detail } => write!(f, "history redaction failed: {detail}"),
            Self::DerivedPending => write!(f, "redacted history did not fully apply"),
            Self::DerivedInvalid { detail } => {
                write!(f, "derived unit failed validation: {detail}")
            }
            Self::DerivedStateDiffers { path } => {
                write!(
                    f,
                    "derived unit differs from the authoritative unit at {path}"
                )
            }
            Self::Crdt => write!(f, "export CRDT operation failed"),
        }
    }
}

impl From<ExportRefusal> for Error {
    fn from(refusal: ExportRefusal) -> Self {
        use ExportRefusal as R;
        let code = match &refusal {
            R::UndecodableSnapshot
            | R::PendingAncestors
            | R::SourceInvalid { .. }
            | R::MalformedReference { .. }
            | R::DerivedInvalid { .. }
            | R::DerivedStateDiffers { .. } => Code::CorruptState,
            R::FormatTooNew { .. } => Code::SchemaTooNew,
            R::PeerCompression
            | R::UnknownOperation { .. }
            | R::UnknownContainer { .. }
            | R::RedactionFailed { .. }
            | R::DerivedPending => Code::CrdtRejected,
            R::Crdt => Code::StorageFailed,
        };
        Error::new(code, refusal.to_string())
    }
}

/// What a derived snapshot depends on, for a caller that caches one: the
/// subject, the authoritative version it was derived from, a fingerprint
/// of the scope's visibility answers (the caller's, since it owns the
/// policy) and the policy generation. Updates for any recipient version can
/// be exported from a cached derived snapshot loaded into a `LoroDoc`; the
/// bytes themselves are never durable state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DerivedKey {
    pub subject: ExportSubject,
    /// The authoritative version vector as sorted `(peer, counter)` pairs.
    pub authoritative_vv: BTreeMap<u64, i32>,
    pub scope_fingerprint: u64,
    pub generation: u64,
}

impl DerivedKey {
    pub fn new(
        subject: ExportSubject,
        authoritative_vv: &VersionVector,
        scope_fingerprint: u64,
        generation: u64,
    ) -> Self {
        Self {
            subject,
            authoritative_vv: authoritative_vv
                .iter()
                .map(|(peer, counter)| (*peer, *counter))
                .collect(),
            scope_fingerprint,
            generation,
        }
    }
}

type Result<T> = std::result::Result<T, ExportRefusal>;

/// §4.3 steps 2–4 over one authoritative snapshot, for one recipient.
/// `recipient_vv` is `None` when the recipient holds nothing of this unit.
pub fn derive(
    subject: ExportSubject,
    authoritative_snapshot: &[u8],
    scope: &ExportScope<'_>,
    recipient_vv: Option<&VersionVector>,
) -> Result<Derived> {
    let source = load(subject, authoritative_snapshot)?;
    let source_state = EffectiveState::of(subject, &source);
    // Redaction cannot change the current placement without inventing a
    // value, so a hidden current placement holds before any byte is built.
    if let Some(Placement {
        profile: Some(profile),
        ..
    }) = source_state.placement
        && !scope.is_profile_visible(profile)
    {
        return Ok(Derived::Hold {
            reason: HoldReason::CurrentPlacementHidden,
        });
    }

    let source_vv = source.oplog_vv();
    let mut json =
        source.export_json_updates_without_peer_compression(&VersionVector::default(), &source_vv);
    if json.peers.is_some() {
        return Err(ExportRefusal::PeerCompression);
    }
    let spans = identify(subject, &mut json, scope)?;
    for (peer, counter) in &spans {
        let mut range = VersionRange::new();
        range.insert(*peer, *counter, counter + 1);
        loro::json::redact(&mut json, range).map_err(|error| ExportRefusal::RedactionFailed {
            detail: error.to_string(),
        })?;
    }

    let derived = LoroDoc::new();
    let status =
        derived
            .import_json_updates(json)
            .map_err(|error| ExportRefusal::DerivedInvalid {
                detail: error.to_string(),
            })?;
    if status.pending.is_some() || derived.oplog_vv() != source_vv {
        return Err(ExportRefusal::DerivedPending);
    }
    let compatibility = subject
        .validate(&derived)
        .map_err(|detail| ExportRefusal::DerivedInvalid { detail })?;
    if !compatibility.writable() {
        return Err(ExportRefusal::DerivedInvalid {
            detail: compatibility.to_string(),
        });
    }
    let derived_state = EffectiveState::of(subject, &derived);
    if let Some(reason) = source_state.compare(&derived_state, subject, scope)? {
        return Ok(Derived::Hold { reason });
    }

    let (bytes, kind, from) = match recipient_vv {
        None => (
            derived.export(ExportMode::snapshot()),
            PayloadKind::Snapshot,
            VersionVector::default(),
        ),
        Some(vv) => (
            derived.export(ExportMode::updates(vv)),
            PayloadKind::Updates,
            vv.clone(),
        ),
    };
    Ok(Derived::Publish(Publication {
        bytes: bytes.map_err(|_| ExportRefusal::Crdt)?,
        kind,
        redacted_ops: spans.len(),
        from,
        to: source_vv,
        capture: scope.capture.clone(),
    }))
}

/// Step 1: the authoritative snapshot as a complete, validated document this
/// build can read in full.
fn load(subject: ExportSubject, bytes: &[u8]) -> Result<LoroDoc> {
    let doc = LoroDoc::new();
    let status = doc
        .import(bytes)
        .map_err(|_| ExportRefusal::UndecodableSnapshot)?;
    if status.pending.is_some() {
        return Err(ExportRefusal::PendingAncestors);
    }
    match subject
        .validate(&doc)
        .map_err(|detail| ExportRefusal::SourceInvalid { detail })?
    {
        Compatibility::Writable => Ok(doc),
        Compatibility::TooNew { found, understood } => {
            Err(ExportRefusal::FormatTooNew { found, understood })
        }
    }
}

/// Steps 2 and 3: clear every change message and answer the exact
/// `(peer, counter)` of every map insert whose value names a hidden Profile.
/// Refuses any operation or container the subject's validator would not
/// allow, instead of leaving it for the library to pass through unchanged.
fn identify(
    subject: ExportSubject,
    json: &mut JsonSchema,
    scope: &ExportScope<'_>,
) -> Result<Vec<(u64, i32)>> {
    let mut spans = Vec::new();
    for change in json.changes.iter_mut() {
        change.msg = None;
        for op in &change.ops {
            let (key, value) = match &op.content {
                JsonOpContent::Map(JsonMapOp::Insert { key, value }) => (key, Some(value)),
                JsonOpContent::Map(JsonMapOp::Delete { key }) => (key, None),
                other => {
                    return Err(ExportRefusal::UnknownOperation {
                        container: op.container.to_string(),
                        kind: op_kind(other),
                    });
                }
            };
            let ancestor = non_mergeable_ancestor(&op.container);
            let root = match &ancestor {
                ContainerID::Root {
                    name,
                    container_type: ContainerType::Map,
                } if subject.roots().contains(&name.as_str()) => name.to_string(),
                _ => {
                    return Err(ExportRefusal::UnknownContainer {
                        container: op.container.to_string(),
                    });
                }
            };
            // A reference lives on the root itself, never in a mergeable
            // child: those hold personal content and are left alone.
            if ancestor != op.container || !subject.is_profile_reference(&root, key) {
                continue;
            }
            let Some(value) = value else { continue };
            let path = format!("{root}.{key}");
            if let Some(profile) = referenced_profile(subject, value, &path)?
                && !scope.is_profile_visible(profile)
            {
                spans.push((change.id.peer, op.counter));
            }
        }
    }
    Ok(spans)
}

/// The Profile a historical value at a reference key names, if any. A null
/// is a value redacted on an earlier hop (every forwarding hop uses this
/// boundary) or an explicitly unassigned placement, and names nobody.
fn referenced_profile(
    subject: ExportSubject,
    value: &LoroValue,
    path: &str,
) -> Result<Option<ProfileId>> {
    match (subject, value) {
        (_, LoroValue::Null) => Ok(None),
        (ExportSubject::WorkspaceCatalog(_), LoroValue::Map(_)) => {
            Placement::from_loro(value, path)
                .map(|placement| placement.profile)
                .map_err(|_| ExportRefusal::MalformedReference {
                    path: path.to_string(),
                })
        }
        (ExportSubject::PersonalV2(_), LoroValue::String(text)) => ProfileId::parse(text)
            .map(Some)
            .ok_or_else(|| ExportRefusal::MalformedReference {
                path: path.to_string(),
            }),
        _ => Err(ExportRefusal::MalformedReference {
            path: path.to_string(),
        }),
    }
}

/// The current `meta.owner_profile_id` of a personal unit, as the three
/// things the v2 validator admits there.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LegacyOwner {
    Absent,
    Redacted,
    Profile(String),
}

/// What redaction must leave alone, read the same way from the source and
/// the derived document. Meta is compared without the legacy owner, which
/// is the one meta value redaction may null.
#[derive(Debug, Clone, PartialEq)]
struct EffectiveState {
    meta: BTreeMap<String, LoroValue>,
    owner: LegacyOwner,
    placement: Option<Placement>,
    deletions: BTreeMap<DeletionId, DeletionRecord>,
    restores: BTreeMap<RestoreId, RestoreRecord>,
    purged: BTreeMap<DeletionId, PurgeRecord>,
    /// Deep values of the subject's non-meta roots that hold content: the
    /// personal containers. Catalog units carry theirs as the facts above.
    content: BTreeMap<&'static str, LoroValue>,
}

impl EffectiveState {
    fn of(subject: ExportSubject, doc: &LoroDoc) -> Self {
        let meta_map = doc.get_map(R_META);
        let mut meta: BTreeMap<String, LoroValue> = match meta_map.get_deep_value() {
            LoroValue::Map(values) => values
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            _ => BTreeMap::new(),
        };
        let owner = match meta_map.get(K_OWNER_PROFILE_ID) {
            None => LegacyOwner::Absent,
            Some(ValueOrContainer::Value(LoroValue::String(text))) => {
                LegacyOwner::Profile(text.to_string())
            }
            Some(_) => LegacyOwner::Redacted,
        };
        meta.remove(K_OWNER_PROFILE_ID);
        // Only the subject's own roots are read: `get_map` on a name the
        // document never held registers an empty root, and this document
        // may be the one about to be exported.
        let facts = Facts { doc };
        let (placement, deletions, restores, purged, content) = match subject {
            ExportSubject::PersonalV2(_) => (
                None,
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                [C_PERSONAL_ATTRIBUTES, C_REMINDERS]
                    .into_iter()
                    .map(|root| (root, doc.get_map(root).get_deep_value()))
                    .collect(),
            ),
            ExportSubject::ProfileCatalog(_) => (
                None,
                facts.deletions(),
                facts.restores(),
                facts.purged(),
                BTreeMap::new(),
            ),
            ExportSubject::WorkspaceCatalog(_) => (
                facts.placement(),
                facts.deletions(),
                facts.restores(),
                facts.purged(),
                BTreeMap::new(),
            ),
        };
        Self {
            meta,
            owner,
            placement,
            deletions,
            restores,
            purged,
            content,
        }
    }

    /// Step 4's comparison: `Ok(None)` when the derived state is the
    /// permitted source state, `Ok(Some(hold))` when redaction had to
    /// change current authority, `Err` for a difference it must never make.
    fn compare(
        &self,
        derived: &Self,
        subject: ExportSubject,
        scope: &ExportScope<'_>,
    ) -> Result<Option<HoldReason>> {
        if derived.placement != self.placement {
            return Ok(Some(HoldReason::CurrentPlacementHidden));
        }
        if derived.deletions != self.deletions
            || derived.restores != self.restores
            || derived.purged != self.purged
        {
            return Ok(Some(HoldReason::LifecycleDiffers));
        }
        if derived.meta != self.meta {
            return Err(ExportRefusal::DerivedStateDiffers {
                path: R_META.to_string(),
            });
        }
        match (&self.owner, &derived.owner) {
            (before, after) if before == after => {
                if let (ExportSubject::PersonalV2(_), LegacyOwner::Profile(text)) = (subject, after)
                    && ProfileId::parse(text)
                        .is_some_and(|profile| !scope.is_profile_visible(profile))
                {
                    return Ok(Some(HoldReason::OwnerHidden));
                }
            }
            // The one permitted change: a hidden legacy owner became null.
            (LegacyOwner::Profile(_), LegacyOwner::Redacted) => {}
            _ => {
                return Err(ExportRefusal::DerivedStateDiffers {
                    path: format!("{R_META}.{K_OWNER_PROFILE_ID}"),
                });
            }
        }
        for (root, value) in &self.content {
            if derived.content.get(root) != Some(value) {
                return Err(ExportRefusal::DerivedStateDiffers {
                    path: (*root).to_string(),
                });
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
