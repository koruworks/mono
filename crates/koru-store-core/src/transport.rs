// SPDX-License-Identifier: MPL-2.0

//! **Device-local transport modes** — may this Profile or Workspace move data
//! at all, and by which transport.
//!
//! D230 settled the ladder: `Local < P2P < Cloud`, chosen **per device**, with
//! an explicit choice always beating a default and a Profile ceiling over its
//! children. The desktop choosing Local-only does not stop the phone, and the
//! phone can never re-enable the desktop. The contract is
//! the transport-mode storage amendment, approved as D249.
//!
//! ## Why this is a plain row and not a CRDT
//!
//! Every other durable fact in the store converges across replicas. This one
//! deliberately does not: two devices are *supposed* to disagree about it, so
//! merging them would destroy the product decision. It lives in `catalog.db` —
//! whose scope is already "this device" — and is never replicated, advertised
//! or serialized into a manifest, a bundle, a peer inventory or Cloud metadata.
//!
//! ## What is here and what is in the backend
//!
//! [`plan`] is the whole decision: the compare-and-set, the ladder, the
//! ceiling and the group-atomicity rules, as a pure function over the rows a
//! transaction has just read. The backend's job is only to read the affected
//! rows under one serialized transaction, call [`plan`], write what it returns
//! and commit. Keeping the policy here is what makes "a Profile downgrade with
//! an unresolved higher-mode child is refused" a test that needs no database.
//!
//! The in-memory half — closing admission *before* the durable write, and
//! keeping it closed when the write fails — is [`Fence`], held by the Store.

use std::collections::{BTreeMap, BTreeSet};

use crate::errors::{Code, Error, Result};
use crate::ids::{ProfileId, WorkspaceId};

/// The ladder, in order. `LocalOnly < P2p < Cloud` is the `Ord` derive, and
/// every ceiling comparison in this module depends on that being the
/// declaration order.
///
/// **Deliberately not `Serialize`.** These values belong in `catalog.db` and in
/// this process. A derive here is all it would take for a mode to ride along
/// inside a manifest, an inventory or a Cloud envelope, which D230 forbids.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum TransportMode {
    /// Nothing leaves or enters this device for this subject.
    LocalOnly,
    /// Paired peers only.
    P2p,
    /// Cloud, and already-authorized peers.
    Cloud,
}

impl TransportMode {
    /// The token in the `mode` column. Stable storage: renaming one orphans
    /// every row already written with it.
    pub fn as_storage(self) -> &'static str {
        match self {
            Self::LocalOnly => "local_only",
            Self::P2p => "p2p",
            Self::Cloud => "cloud",
        }
    }

    /// **An unknown token is never a default.** A row this build cannot decode
    /// is storage the Store must report, not a reason to assume Local-only and
    /// carry on — silently repairing a mode is how a person's explicit choice
    /// disappears (contract §3).
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "local_only" => Ok(Self::LocalOnly),
            "p2p" => Ok(Self::P2p),
            "cloud" => Ok(Self::Cloud),
            _ => Err(Error::new(
                Code::CorruptState,
                "a stored transport mode is not one of local_only, p2p or cloud",
            )),
        }
    }

    pub const ALL: &'static [Self] = &[Self::LocalOnly, Self::P2p, Self::Cloud];

    /// Whether this mode admits a direct peer transfer.
    pub fn allows_peer(self) -> bool {
        self >= Self::P2p
    }

    /// Whether this mode admits Cloud transfer.
    pub fn allows_cloud(self) -> bool {
        self == Self::Cloud
    }
}

/// What a mode row is about. A Profile row names itself; a Workspace row names
/// its Workspace and finds its owner through `workspaces.owner`.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum TransportSubject {
    Profile(ProfileId),
    Workspace(WorkspaceId),
}

impl TransportSubject {
    /// The `subject_kind` column.
    pub fn kind(self) -> &'static str {
        match self {
            Self::Profile(_) => "profile",
            Self::Workspace(_) => "workspace",
        }
    }

    /// The `subject_id` column.
    pub fn id(self) -> String {
        match self {
            Self::Profile(id) => id.to_storage(),
            Self::Workspace(id) => id.to_storage(),
        }
    }

    pub fn workspace(self) -> Option<WorkspaceId> {
        match self {
            Self::Workspace(id) => Some(id),
            Self::Profile(_) => None,
        }
    }
}

/// One row, as the catalog holds it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TransportModeState {
    pub subject: TransportSubject,
    /// The Profile this row is filed under. For a Profile row it is the
    /// subject itself; for a Workspace row it is `workspaces.owner`, which the
    /// Store validates on every mutation.
    pub owner: ProfileId,
    pub mode: TransportMode,
    /// `false` for a seeded or verified default; `true` once somebody on this
    /// device deliberately chose. An explicit row always wins over evidence.
    pub explicit_choice: bool,
    /// Positive, local, monotonic. The compare-and-set token — never a clock,
    /// and never compared across devices.
    pub revision: i64,
}

/// A deliberate local selection. Sets `explicit_choice`, and therefore pins the
/// value against any later default promotion.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SetTransportMode {
    pub subject: TransportSubject,
    pub expected_revision: i64,
    pub mode: TransportMode,
}

/// What the Agent has already **verified** about one subject's sharing — the
/// only input a default may be derived from (contract §3).
///
/// It is deliberately two booleans rather than an account, a subscription or a
/// peer list: the checking happens in the Agent, which is where the account and
/// the trust graph live, and re-deriving any of it here would be a second
/// authority for one question. What crosses into the Store is the verdict.
///
/// **Absence of evidence is not a command.** [`Self::default`] reads as
/// Local-only, and a Local-only verdict is never written over an explicit row
/// (`plan` refuses that) nor over a still-default row that already says
/// Local-only (that write has nothing in it). An Agent that has not finished
/// checking therefore cannot reset a choice by being slow.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SharingEvidence {
    /// Hosted-enabled sharing for this subject **and** its account context,
    /// both verified.
    ///
    /// One flag rather than two because §3 admits the Cloud default only when
    /// both hold, and two flags is two places for a caller to forget the `&&`.
    /// *Sign-in is not proof of subscription*: a caller sets this from an
    /// installed Cloud authority for this exact subject, never from the
    /// presence of an account.
    pub hosted_enabled_with_account: bool,
    /// Selected authorized peer sharing for this subject, verified.
    pub peer_selected: bool,
}

impl SharingEvidence {
    /// §3's ladder, in §3's order: **Cloud** where hosted-enabled sharing and
    /// its account context are verified; otherwise **P2P** where selected
    /// authorized peer sharing is verified; otherwise **Local-only**.
    ///
    /// The whole derivation, in one place, so there is one thing to read and
    /// one thing to pin.
    pub fn default_mode(self) -> TransportMode {
        if self.hosted_enabled_with_account {
            TransportMode::Cloud
        } else if self.peer_selected {
            TransportMode::P2p
        } else {
            TransportMode::LocalOnly
        }
    }

    /// Whether this verdict is worth an [`InitializeTransportMode`] against the
    /// row as it stands.
    ///
    /// A verdict no higher than the row already reads writes nothing: the
    /// initializer only ever *promotes* a still-default row. Lowering a default
    /// from absent evidence would make a transient offline Agent look exactly
    /// like a revocation, and §3 is explicit that absence of evidence is not a
    /// command to reset anything.
    pub fn promotes(self, current: &TransportModeState) -> bool {
        !current.explicit_choice && self.default_mode() > current.mode
    }
}

/// **Whether the person has linked this device to anything of their own**, as
/// the seeded default for a *new* subject reads it.
///
/// D258 made a person's explicit answer the only promotion path, and the
/// Profile prompt the only place to give it. A phone has no such prompt, so a
/// Profile or Workspace created there was withheld for ever — and once the
/// root catalog went behind the same gate, a Workspace made on a phone could
/// never reach the person's other devices at all. The answer is not a second
/// evidence path and not a platform check: **linking is the choice**. Joining
/// this device to an account, or pairing it with another of the person's
/// devices, is a deliberate act that says "these are mine, keep them
/// together", and a subject created afterwards starts by admitting the peer
/// path rather than refusing it.
///
/// Three things this deliberately is **not**:
///
/// - **Not the platform.** `Linked` is about what the person did, not what the
///   subject runs on. An unlinked phone seeds exactly as an unlinked desktop
///   does, and a linked desktop seeds exactly as a linked phone does.
/// - **Not Cloud.** [`Self::seeded_mode`] stops at [`TransportMode::P2p`]:
///   linking two of your own devices is consent for them to talk to each
///   other, not consent to put the data on somebody's server. Cloud still
///   comes from the Profile prompt and nothing else (D258).
/// - **Not retroactive.** It is read when a row is *created*. Rows that
///   already exist are untouched, whatever happens to the linkage afterwards —
///   promoting them would reverse D258 for data the person already has.
///
/// It lands as a default (`explicit_choice = false`), so a person who then
/// sets that subject Local-only outranks it exactly as they outrank every
/// other default.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum DeviceLinkage {
    /// Nothing has been linked: no account binding and no paired relationship.
    #[default]
    Standalone,
    /// The person has joined this device to their account or paired it with
    /// another of their devices.
    Linked,
}

impl DeviceLinkage {
    pub fn from_linked(linked: bool) -> Self {
        if linked {
            Self::Linked
        } else {
            Self::Standalone
        }
    }

    pub fn is_linked(self) -> bool {
        matches!(self, Self::Linked)
    }

    /// The mode a subject created on this device starts at.
    pub fn seeded_mode(self) -> TransportMode {
        match self {
            Self::Standalone => TransportMode::LocalOnly,
            Self::Linked => TransportMode::P2p,
        }
    }
}

/// The mode a **Workspace** created under `profile` starts at.
///
/// Two rules meet here and the order matters:
///
/// - A Profile somebody has actually spoken about hands its choice down, as it
///   has since the `COALESCE` in the catalog's workspace insert: a Workspace
///   created after the Profile prompt was answered must not sit closed.
/// - Otherwise the linkage default applies — **capped at the Profile's own
///   row**. The cap is not politeness: a child stored above its Profile is a
///   `ceiling_mismatch`, which stops the affected transport and is reported
///   rather than repaired. Seeding a Workspace P2P under a Profile row that
///   was seeded Local-only before linking would therefore not open anything;
///   it would manufacture the corruption state. The Profile row stays where it
///   is because it was created before the choice, and the Workspace is capped
///   at it.
pub fn seeded_child_mode(profile: &TransportModeState, linkage: DeviceLinkage) -> TransportMode {
    if profile.explicit_choice {
        profile.mode
    } else {
        profile.mode.min(linkage.seeded_mode())
    }
}

/// A verified default, derived from sharing evidence the Agent has already
/// checked. **Writable only through the trusted host-evidence path**, never
/// from a UI, and it can never modify a row somebody chose explicitly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InitializeTransportMode {
    pub subject: TransportSubject,
    pub expected_revision: i64,
    /// What the evidence says the default should be. Capped at the Profile
    /// ceiling; a child never raises its Profile to fit.
    pub mode: TransportMode,
}

/// One entry of a group submitted to the catalog together.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportModeRequest {
    Set(SetTransportMode),
    Initialize(InitializeTransportMode),
}

impl TransportModeRequest {
    pub fn subject(self) -> TransportSubject {
        match self {
            Self::Set(r) => r.subject,
            Self::Initialize(r) => r.subject,
        }
    }

    pub fn expected_revision(self) -> i64 {
        match self {
            Self::Set(r) => r.expected_revision,
            Self::Initialize(r) => r.expected_revision,
        }
    }

    pub fn mode(self) -> TransportMode {
        match self {
            Self::Set(r) => r.mode,
            Self::Initialize(r) => r.mode,
        }
    }

    pub fn is_explicit(self) -> bool {
        matches!(self, Self::Set(_))
    }
}

/// What one request did. **All three are successes**; only an `Err` means the
/// group was refused, and a refused group writes nothing at all.
///
/// The caller re-reads after [`Superseded`](Self::Superseded) and never
/// automatically replays an older selection over a newer one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportModeCommit {
    /// Written, and durable when the operation returns. Carries the new row.
    Committed(TransportModeState),
    /// Nothing was written because the request was already exactly true. An
    /// explicit choice of the already-committed explicit value lands here, and
    /// deliberately does **not** advance the revision.
    AlreadyCurrent(TransportModeState),
    /// Nothing was written: the caller's `expected_revision` did not match and
    /// the request was not already true, or an initializer met an explicit row.
    /// Carries the current value so the caller can decide with it.
    Superseded(TransportModeState),
}

impl TransportModeCommit {
    /// The row as it stands after the operation, whichever outcome occurred.
    pub fn state(self) -> TransportModeState {
        match self {
            Self::Committed(s) | Self::AlreadyCurrent(s) | Self::Superseded(s) => s,
        }
    }

    pub fn wrote(self) -> bool {
        matches!(self, Self::Committed(_))
    }
}

/// Every mode row a group's decision depends on: one Profile and all of its
/// Workspaces, read inside the same transaction that will write.
///
/// The children have to be *all* of them, not only the ones named in the
/// group: a Profile downgrade is refused precisely because of the child nobody
/// mentioned.
#[derive(Clone, Debug)]
pub struct TransportModeGroup {
    pub profile: TransportModeState,
    pub children: Vec<TransportModeState>,
}

/// What [`plan`] decided: the outcome per request, in request order, and the
/// rows to write. An empty `writes` with non-empty `outcomes` is the ordinary
/// idempotent case.
#[derive(Clone, Debug)]
pub struct TransportModePlan {
    pub outcomes: Vec<TransportModeCommit>,
    pub writes: Vec<TransportModeState>,
}

/// The effective answer for one subject: its own mode capped at its Profile's.
///
/// `ceiling_mismatch` is reported rather than repaired. A stored child above
/// its Profile can only come from malformed storage or a supported catalog
/// move, and contract §3 is explicit that the answer is "stop the affected
/// transport and say so", never "quietly rewrite the choice".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EffectiveTransportMode {
    pub subject: TransportSubject,
    /// What is actually permitted, after the ceiling and after the in-memory
    /// fence. This is the only value a transport check may branch on.
    pub mode: TransportMode,
    /// The Profile row's own mode, the ceiling over every child.
    pub profile_mode: TransportMode,
    /// The subject's own stored mode, before the ceiling was applied.
    pub stored_mode: TransportMode,
    pub explicit_choice: bool,
    pub revision: i64,
    /// The stored mode exceeded the Profile ceiling. Worth reporting; never
    /// worth repairing.
    pub ceiling_mismatch: bool,
    /// Admission is closed in memory — a restrictive choice has been made and
    /// its durable write has not (or has not yet) succeeded. `mode` is already
    /// [`TransportMode::LocalOnly`] when this is set.
    pub fenced: bool,
    /// The live policy generation this answer belongs to. A callback holding
    /// an older one has been invalidated and must ask again.
    pub generation: u64,
}

impl EffectiveTransportMode {
    pub fn allows_peer(&self) -> bool {
        self.mode.allows_peer()
    }

    pub fn allows_cloud(&self) -> bool {
        self.mode.allows_cloud()
    }
}

/// Which transport a caller is about to use, for the one refusal helper every
/// check point shares.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TransportPath {
    Peer,
    Cloud,
}

/// The immediate stop (contract §5), held in memory by the Store.
///
/// **The order matters and is the whole point.** A restrictive choice closes
/// admission here *before* the durable write is attempted, so a transfer in
/// flight stops rather than finishing under a mode that no longer permits it.
/// If the write then fails, the subject stays closed and the failure is
/// reported: the guard is never traded for a promise of durability that was not
/// kept. Restart reloads the last *actually committed* mode, which is why this
/// state is deliberately not persisted.
#[derive(Clone, Debug, Default)]
pub struct Fence {
    generation: u64,
    closed: BTreeSet<TransportSubject>,
}

impl Fence {
    pub fn new() -> Self {
        Self::default()
    }

    /// The live policy generation. A2's session fence, not the persisted
    /// revision — it counts invalidations within one process lifetime.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Close admission for these subjects now, and invalidate every policy
    /// answer handed out so far.
    pub fn close(&mut self, subjects: impl IntoIterator<Item = TransportSubject>) {
        let before = self.closed.len();
        self.closed.extend(subjects);
        // Even a no-op close bumps: the caller is about to attempt a write, and
        // an answer cached across it must not be reused.
        if self.closed.len() != before {
            self.invalidate();
        }
    }

    /// A durable commit landed, so the row is authority again for these
    /// subjects.
    pub fn reopen(&mut self, subjects: impl IntoIterator<Item = TransportSubject>) {
        for subject in subjects {
            self.closed.remove(&subject);
        }
        self.invalidate();
    }

    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Whether this subject, or the Profile above it, is closed.
    pub fn is_closed(&self, subject: TransportSubject, owner: ProfileId) -> bool {
        self.closed.contains(&subject) || self.closed.contains(&TransportSubject::Profile(owner))
    }

    pub fn closed(&self) -> impl Iterator<Item = TransportSubject> + '_ {
        self.closed.iter().copied()
    }
}

/// Apply a group of requests to the rows it names — the whole of contract §3
/// and §4, with no I/O in sight.
///
/// Refusing returns `Err` and the caller writes nothing: "an unrelated invalid
/// mutation cannot half-change modes" means the group is one decision.
pub fn plan(
    group: &TransportModeGroup,
    requests: &[TransportModeRequest],
) -> Result<TransportModePlan> {
    if requests.is_empty() {
        return Err(Error::new(
            Code::InvalidRequest,
            "a transport mode group names no subject",
        ));
    }
    let owner = group.profile.owner;
    let mut seen = BTreeSet::new();
    for request in requests {
        if !seen.insert(request.subject()) {
            return Err(Error::new(
                Code::InvalidRequest,
                "a transport mode group names one subject twice",
            ));
        }
    }

    // Index what the transaction read. A request naming something not in here
    // is the backend's bug, not the caller's — the backend resolves subjects
    // before planning.
    let mut rows: BTreeMap<TransportSubject, TransportModeState> = BTreeMap::new();
    rows.insert(group.profile.subject, group.profile);
    for child in &group.children {
        if child.owner != owner {
            return Err(Error::new(
                Code::InvalidRequest,
                "a transport mode group mixes Profiles",
            ));
        }
        rows.insert(child.subject, *child);
    }

    let profile_before = group.profile.mode;
    let mut writes = Vec::new();
    // What every subject reads as once this group has been applied.
    let mut after: BTreeMap<TransportSubject, TransportModeState> = rows.clone();

    // Two passes: the Profile first, because a child's cap is the Profile's
    // mode *after* the group. Doing it in request order would make the answer
    // depend on how a caller happened to order its list.
    let ordered: Vec<&TransportModeRequest> = requests
        .iter()
        .filter(|r| matches!(r.subject(), TransportSubject::Profile(_)))
        .chain(
            requests
                .iter()
                .filter(|r| matches!(r.subject(), TransportSubject::Workspace(_))),
        )
        .collect();

    let mut by_subject: BTreeMap<TransportSubject, TransportModeCommit> = BTreeMap::new();
    for request in ordered {
        let subject = request.subject();
        let current = *rows.get(&subject).ok_or_else(|| {
            Error::new(
                Code::CorruptState,
                "a transport mode row named by this group is missing",
            )
        })?;

        // The ceiling a child's *default* is capped at. Only Initialize is
        // capped; an explicit selection above the ceiling is refused below,
        // because silently downgrading somebody's deliberate choice would be
        // the same lie as silently raising the Profile.
        let ceiling = match subject {
            TransportSubject::Profile(_) => TransportMode::Cloud,
            TransportSubject::Workspace(_) => after
                .get(&group.profile.subject)
                .map(|state| state.mode)
                .unwrap_or(profile_before),
        };

        let outcome = decide(current, *request, ceiling)?;
        if let TransportModeCommit::Committed(state) = outcome {
            writes.push(state);
            after.insert(subject, state);
        }
        by_subject.insert(subject, outcome);
    }

    let profile_after = after[&group.profile.subject].mode;

    // The ceiling, on the state the group would leave behind. Only a selection
    // that actually took effect can breach it: a Set whose revision was stale
    // wrote nothing and asked for nothing, so the row it left behind is a
    // pre-existing inconsistency for the downgrade rule below to catch.
    for request in requests {
        let subject = request.subject();
        if subject == group.profile.subject {
            continue;
        }
        let took_effect = matches!(
            by_subject.get(&subject),
            Some(TransportModeCommit::Committed(_) | TransportModeCommit::AlreadyCurrent(_))
        );
        let state = after[&subject];
        if request.is_explicit() && took_effect && state.mode > profile_after {
            return Err(Error::new(
                Code::TransportModeCeiling,
                "a Workspace cannot be set above its Profile's transport mode; \
                 raise the Profile explicitly first",
            ));
        }
    }

    // A Profile downgrade commits only with every higher-mode child explicitly
    // resolved in the same group. The child nobody mentioned is exactly the one
    // this exists for.
    if profile_after < profile_before {
        let mut unresolved: Vec<TransportSubject> = Vec::new();
        for child in &group.children {
            let state = after[&child.subject];
            let resolved = matches!(
                by_subject.get(&child.subject),
                Some(TransportModeCommit::Committed(_) | TransportModeCommit::AlreadyCurrent(_))
            );
            if state.mode > profile_after && !resolved {
                unresolved.push(child.subject);
            }
        }
        if !unresolved.is_empty() {
            return Err(Error::new(
                Code::TransportModeChildren,
                format!(
                    "{} Workspace(s) under this Profile are above the requested mode and \
                     need an explicit choice in the same group",
                    unresolved.len()
                ),
            ));
        }
    }

    // Back into the caller's order. The decision is made Profile-first, but
    // answering in that order would hand a caller outcomes that do not line up
    // with the list it sent.
    let outcomes = requests
        .iter()
        .map(|request| by_subject[&request.subject()])
        .collect();
    Ok(TransportModePlan { outcomes, writes })
}

/// The compare-and-set for one row, before any ceiling or group rule applies.
fn decide(
    current: TransportModeState,
    request: TransportModeRequest,
    ceiling: TransportMode,
) -> Result<TransportModeCommit> {
    let (proposed_mode, proposed_explicit) = match request {
        TransportModeRequest::Set(set) => (set.mode, true),
        TransportModeRequest::Initialize(init) => {
            // An explicit row always wins: evidence never overwrites a choice,
            // however late the evidence arrives and whatever revision it
            // expected. The caller re-reads and stops.
            if current.explicit_choice {
                return Ok(TransportModeCommit::Superseded(current));
            }
            (init.mode.min(ceiling), false)
        }
    };
    let identical = current.mode == proposed_mode && current.explicit_choice == proposed_explicit;

    if request.expected_revision() != current.revision {
        // Success without writing, but only when the requested *explicit*
        // state already equals the current state. Anything else is stale and
        // the caller is handed the current value instead.
        return Ok(if request.is_explicit() && identical {
            TransportModeCommit::AlreadyCurrent(current)
        } else {
            TransportModeCommit::Superseded(current)
        });
    }
    if identical {
        // The exact duplicate of a committed choice. Deliberately does not
        // advance the revision: a retry must cost bytes, never correctness.
        return Ok(TransportModeCommit::AlreadyCurrent(current));
    }
    match current.revision.checked_add(1) {
        Some(revision) => Ok(TransportModeCommit::Committed(TransportModeState {
            mode: proposed_mode,
            explicit_choice: proposed_explicit,
            revision,
            ..current
        })),
        // **Revisions never wrap.** Refusing the whole group rather than
        // reusing or resetting a revision is what keeps compare-and-set
        // meaningful, and the refusal leaves the caller's fence closed — which
        // is what "holds affected transport admission closed until repaired"
        // means in practice.
        None => Err(Error::new(
            Code::CorruptState,
            "this subject's transport mode revision is exhausted; \
             its transport stays closed until the row is repaired",
        )),
    }
}

#[cfg(test)]
mod tests;
