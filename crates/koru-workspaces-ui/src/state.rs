// SPDX-License-Identifier: MPL-2.0

//! **What the surface is told, and what it can tell about itself.**
//!
//! The rows, the four groups they fall into, the three things that can be true
//! about a management call, and the two states rev 4 owes this surface by name:
//! S9's quiet unavailability and S24's partial failure.

use koru_api_wire::{WorkspaceLifecycleAction, WorkspaceLifecycleOutcome};
use koru_store_core::ids::{ProfileId, WorkspaceId};

#[cfg(test)]
mod tests;

/// Whether the gateway is answering.
///
/// **Three, not two.** A surface that has never heard is not the same as one
/// whose gateway has gone away: the first draws nothing and says it is looking,
/// the second draws the last catalog it had and says it is stale (S9's idiom).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Availability {
    /// Nothing has arrived yet. The first fetch is in flight.
    #[default]
    Loading,
    Live,
    /// The gateway stopped answering. Rows stay visible and read-only.
    Down,
}

impl Availability {
    pub fn is_down(self) -> bool {
        self == Self::Down
    }

    pub fn is_loading(self) -> bool {
        self == Self::Loading
    }
}

/// **Whether this window may remove a workspace.**
///
/// `workspace.destroy` is in neither the default nor the allowed scope set: a
/// caller must name it *and* be vouched for by the Agent (ADR-267), which
/// vouches for a process running the `koru` beside it, however it was started.
/// A window that is not — a `koru` from another installation or build tree, or
/// one whose executable the operating system would not report — gets a
/// session without it.
///
/// **The rows are still drawn.** See `lib.rs` for why disabled beats hidden.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Authorization {
    /// Not yet asked. Destructive rows are inert meanwhile — offering an action
    /// and then discovering it is refused is worse than a moment's dimming.
    #[default]
    Unknown,
    Vouched,
    /// Asked, and the answer was no.
    Refused,
}

impl Authorization {
    pub fn may_destroy(self) -> bool {
        self == Self::Vouched
    }

    /// The same three-valued answer, read about whichever scope this copy was
    /// probed for. The surface holds two: `workspace.destroy` and
    /// `workspace.migrate` are separate permissions a window can hold one of
    /// (ADR-268), so one answer standing for both would offer a row that is
    /// going to refuse.
    pub fn is_granted(self) -> bool {
        self == Self::Vouched
    }
}

/// One profile a workspace could be moved to.
///
/// **A name and an id, and nothing else.** The picker draws a list of
/// destinations; a profile's icon, colour and purpose belong to the banner that
/// presents personas, and carrying them here would be this surface growing a
/// second opinion about what a profile looks like.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileRow {
    pub id: ProfileId,
    pub name: String,
}

/// Which group a row sits in, and therefore which heading it is under.
///
/// **Derived from the catalog row, never stored.** Archive and soft-deletion
/// are two independent flags on the workspace; this is the one place they are
/// turned into a place in a list, so two parts of the surface cannot disagree
/// about where a workspace belongs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Pinned,
    Active,
    /// S17. Its projects are hidden and its records are untouched.
    Archived,
    /// S18's other half: in the trash, restorable.
    Deleted,
}

impl Group {
    /// **Deleted outranks archived.** A workspace can be both, and the trash is
    /// the more consequential fact: somebody looking for it will look there.
    pub(crate) fn of(row: &Row) -> Self {
        match (row.deleted, row.archived, row.pinned) {
            (true, _, _) => Self::Deleted,
            (_, true, _) => Self::Archived,
            (_, _, true) => Self::Pinned,
            _ => Self::Active,
        }
    }
}

/// What committed storage proved about a row. Unknown is never navigable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Authority {
    #[default]
    Unknown,
    Verified,
    Pending,
    Uncertain {
        cause: String,
        recovery: Option<koru_store_core::Recovery>,
    },
}

/// One workspace, as this surface draws it.
///
/// **A projection of the catalog row and nothing else.** No `WorkspaceDoc`, no
/// document count that would need a second call, no session state — the
/// terminals a workspace owns are named by the consequence when they matter,
/// which is the only moment they do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub id: WorkspaceId,
    pub owner: ProfileId,
    pub name: String,
    /// An emoji or a `blob:<hash>` — a token the store never interprets and
    /// neither does this.
    pub icon: Option<String>,
    /// A colour *name*, never a hex value (ADR-82).
    pub color: Option<String>,
    pub pinned: bool,
    pub archived: bool,
    pub deleted: bool,
    pub position: i64,
    /// When this workspace was last opened, in epoch milliseconds UTC, as the
    /// catalog records it — `None` for one never opened. For *display* (a
    /// "last opened" line, a most-recent sort); never for deciding anything
    /// between replicas, which is what version vectors are for.
    pub last_opened_at: Option<i64>,
    pub authority: Authority,
    pub listing_differs: bool,
}

impl Row {
    pub fn verified_live(&self) -> bool {
        self.authority == Authority::Verified && !self.archived && !self.deleted
    }

    pub fn navigation_ready(&self) -> bool {
        self.verified_live() && !self.listing_differs
    }

    pub fn authority_reason(&self, lang: koru_i18n::Lang) -> Option<String> {
        use koru_i18n::{Key, t};
        let key = match &self.authority {
            Authority::Unknown => Key::ProfileWaitUnknown,
            Authority::Pending => Key::ProfileWaitPending,
            Authority::Uncertain { .. } => Key::ProfileWaitUncertain,
            Authority::Verified if self.listing_differs => Key::ProfileWaitListing,
            Authority::Verified => return None,
        };
        Some(t(lang, key).into())
    }

    pub(crate) fn group(&self) -> Group {
        Group::of(self)
    }
}

/// **What an action would do, as the gateway computed it.**
///
/// Held while the confirmation is on screen; the token goes back with the
/// acceptance. It is the gateway's answer rather than this surface's guess,
/// which is what makes the acceptance provably about it (ADR-267).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consequence {
    pub(crate) workspace: WorkspaceId,
    pub(crate) action: WorkspaceLifecycleAction,
    pub(crate) confirmation: String,
    /// The terminals that will close, named. Empty is a real answer and a
    /// different sentence: *nothing is open in it*.
    pub(crate) terminals: Vec<String>,
}

/// **What a move would do, as the gateway computed it.**
///
/// The migration's half of [`Consequence`]. It names both ends, because the
/// dialog does: a person deciding whether to move a workspace is deciding where
/// to, and the destination is what the token is bound to as well as the
/// workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveConsequence {
    pub(crate) workspace: WorkspaceId,
    pub(crate) to: ProfileId,
    /// The destination's name, resolved when the picker was drawn. The title
    /// says where it is going, and a person reading "Move Notes to Home?" is
    /// reading the answer to the question they just asked.
    pub(crate) to_name: String,
    pub(crate) confirmation: String,
    pub(crate) terminals: Vec<String>,
}

/// **The operation a notice offers to run again.**
///
/// Two shapes, because there are two operations and a retry has to name which.
/// A single `(workspace, action)` pair could not say "move it to Home", and a
/// retry that guessed would be a retry of something the person did not ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Retry {
    Lifecycle(WorkspaceId, WorkspaceLifecycleAction),
    Migration(WorkspaceId, ProfileId),
}

/// **What a management call left behind.**
///
/// Three kinds, because a person can do three different things about them:
/// nothing (it worked), read it (it refused for a reason that is not going to
/// change by pressing again), or press retry.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub(crate) text: String,
    /// The operation to run again, when running it again is the right move.
    /// S24 is the case this exists for: the terminals are closed, the catalog
    /// is unchanged, and the same operation against a workspace nothing moved
    /// is what finishes it.
    ///
    /// A move has a second case the lifecycle does not have: the workspace
    /// moved and its projects did not catch up, because the session host was
    /// not answering. That is not a failure — but running the move again is
    /// exactly what repairs them, so it is offered the same way.
    pub(crate) retry: Option<Retry>,
}

impl Notice {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether this notice offers a retry (S24's affordance).
    pub fn is_retryable(&self) -> bool {
        self.retry.is_some()
    }
}

/// What came back from a management call, in the surface's own vocabulary.
#[derive(Debug)]
pub(crate) enum Answer {
    /// Only this command's terminal answer may remove its pending projection.
    Settled(crate::CommandId, Box<Answer>),
    ProfileCreated(koru_store_core::errors::Result<koru_api_client::CreatedProfile>),
    ProfileKept(koru_api_client::WorkspaceResult<()>),
    WorkspaceEnsured {
        attempt: u64,
        result: koru_store_core::errors::Result<koru_api_client::EnsuredWorkspace>,
    },
    Catalog(Vec<Row>),
    CatalogState {
        rows: Vec<Row>,
        removals:
            std::collections::BTreeMap<ProfileId, koru_store_core::profile_removal::RemovalMark>,
    },
    /// The profiles a workspace could be moved to.
    Profiles(Vec<ProfileRow>),
    /// The authorization probe's answer, for `workspace.destroy`.
    Authorization(Authorization),
    /// The same probe, for `workspace.migrate`. Two probes because two scopes.
    MigrationAuthorization(Authorization),
    /// A consequence to present.
    Consequence(Consequence),
    /// A move's consequence to present.
    MoveConsequence(MoveConsequence),
    /// The move finished. `projects_rescoped: None` is not a failure — it means
    /// the session host was not asked — and the surface says so rather than
    /// reporting a clean move.
    Moved(koru_api_wire::WorkspaceMigrationOutcome),
    /// The operation finished. The catalog is refetched either way — a
    /// committed fact is on its way through `/v1/events` too, and asking twice
    /// is cheaper than a surface that is briefly wrong about the trash.
    Completed(WorkspaceLifecycleOutcome),
    /// Something refused, and this is what a person should be told about it.
    Refused(Notice),
    /// The gateway stopped answering.
    Unavailable,
}
