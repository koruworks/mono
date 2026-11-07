// SPDX-License-Identifier: MPL-2.0

//! **Workspace management without a picture**: the controller behind the
//! Manage tab's *Workspaces* on a Profile's page, and the link it talks
//! through.
//!
//! Charter: *rows, states, and asynchronous typed API use; it performs no
//! mutation itself, owns no Store or session semantics, and names no
//! `WorkspaceDoc`.*
//!
//! ## What it does for itself, and what it asks the host for
//!
//! This is the one surface entitled to call the destructive half of the API, so
//! the split is not the usual one. It reads the catalog and it performs the
//! lifecycle operation **itself**, through `koru-api-client`, on its own
//! thread. What it asks the host for is anything that needs a *window*: opening
//! a workspace, opening a second one, a file dialog, a name field, the
//! clipboard. Each of those is the host's, which is exactly why they are
//! [`Request`]s and the API calls are not.
//!
//! ## Rows come from the including-deleted projection
//!
//! Archived (S17) and soft-deleted (S18) workspaces are what a manager exists
//! to show, and the ordinary listing hides both — so this reads
//! `?include_deleted=true` and groups what comes back. **The
//! missing-workspace area (S19) is deliberately not here**: a workspace absent
//! from the including-deleted catalog is a fact about somebody's *projects*,
//! and the project tree is the Terminal's (SP-11). Nothing here precludes it —
//! the catalog this holds is the same authority that reconciliation needs.
//!
//! ## Disabled, never hidden
//!
//! **Recorded decision, since the plan is silent.** A window that the Agent did
//! not vouch for holds no `workspace.destroy` (ADR-267), and its Archive and
//! Delete rows are drawn **dimmed with the reason on screen** rather than left
//! out. Three arguments, in order of weight:
//!
//! - ADR-265's rule for modals is that behaviour must be a fact of the visible
//!   design and never of an invisible trigger. A surface whose *shape* changed
//!   with how the process was launched is the same failure one level up: two
//!   identical-looking windows, one of which quietly has fewer actions.
//! - A missing control teaches nothing. Somebody who cannot find Archive
//!   concludes the app cannot archive; somebody who sees it greyed with *Koru
//!   could not confirm that this window is part of this Koru installation*
//!   knows the action exists and why this window does not have it.
//! - Hiding is also the more expensive answer to get right: the same rows are
//!   reachable from a context menu the host builds, and a surface that hid them
//!   would have to teach every host to hide them too.
//!
//! The same idiom carries the other refusals a person can act on: the
//! move-to-profile row, whose four reasons — nothing answering, this window's
//! own `workspace.migrate` grant, a workspace in the trash, and no other
//! profile to send it to — outrank each other in that order. A typed
//! documents-open refusal remains for compatibility with an older gateway;
//! current confirmed soft-delete disconnects those Store rooms (ADR-267).
//! That refusal never appears on a move: a move touches no room, so the Store
//! has no `workspace_in_use` to answer with (ADR-268).
//!
//! ## A controller, and no drawing
//!
//! The state machine — catalog, authorizations, the confirmation's stages,
//! notices, the host-menu intent, and the reason each control is dimmed — is
//! [`WorkspacesController`], which draws nothing. It drew in this crate once,
//! as `WorkspacesSurface`, the list-and-details manager of the Workspaces
//! tab; that tab and its drawing were deleted when the Manage tab replaced it
//! (ADR-449-onemanagetab). The one drawing now is a Profile page's
//! *Workspaces* in `apps/koru/src/manage/workspaces.rs`, which asks this
//! controller what a press means and why it is refused, so the drawing and
//! the workspace menu cannot disagree.

pub mod action;
pub mod controller;
pub mod links;
mod pending;
pub mod state;

#[cfg(test)]
mod dependency;
#[cfg(test)]
mod tests;

pub use action::{Action, Request};
pub use controller::WorkspacesController;
pub use links::{MANAGER_SCOPES, WorkspaceLinks};
pub use pending::CommandId;
pub use state::{Authority, Authorization, Availability, Group, Notice, ProfileRow, Row};

/// The classifications [`links`] hands back, worded here.
///
/// **Stable tokens, not sentences.** The worker thread does not know which
/// language a window is drawing in and must not: it says which refusal this is
/// and the surface says it in the person's own words.
pub(crate) const REFUSAL_PARTIAL: &str = "partial";
pub(crate) const REFUSAL_LISTING_PENDING: &str = "listing_pending";
pub(crate) const REFUSAL_DOCUMENTS_OPEN: &str = "documents-open";
pub(crate) const REFUSAL_NOT_PERMITTED: &str = "not-permitted";
pub(crate) const REFUSAL_UNAVAILABLE: &str = "unavailable";
/// The move's own three. They are separate tokens rather than reuses because
/// each says something the lifecycle's version does not: which scope was
/// refused, that the *move* did not happen rather than a catalog write, and
/// that a destination went away.
pub(crate) const REFUSAL_MOVE_PARTIAL: &str = "move-partial";
pub(crate) const REFUSAL_MOVE_NOT_PERMITTED: &str = "move-not-permitted";
pub(crate) const REFUSAL_MOVE_NO_SUCH_PROFILE: &str = "move-no-such-profile";
/// Not a refusal at all: the move committed and the session host was not
/// answering, so its projects have not caught up. Worded as something a person
/// can finish rather than as a failure, and it offers the same retry.
pub(crate) const NOTICE_MOVE_PROJECTS_PENDING: &str = "move-projects-pending";
