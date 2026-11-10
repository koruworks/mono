// SPDX-License-Identifier: MPL-2.0

//! **What a press means, and what the host is asked to do about it.**
//!
//! Two vocabularies and not one, because they answer different questions.
//! [`Action`] is *what somebody asked for* — the controller's intent
//! vocabulary, produced by whichever drawing laid out the control and consumed
//! by `WorkspacesController::perform`; public, so a second drawing of the same
//! workspaces speaks it too. [`Request`] is *what only the host can do* —
//! drained by the host, and deliberately small.
//!
//! The split is sharper here than in `koru-settings-ui`, and it is the packet's
//! central boundary: this surface calls the typed client for the catalog and
//! for the lifecycle operation **itself**, because it is the one surface
//! entitled to. What it cannot do is anything that needs a window — opening a
//! workspace, opening a second window, a file dialog, a name field, the
//! clipboard — and every one of those is a [`Request`].

use koru_store_core::ids::{ProfileId, WorkspaceId};

/// One intent, named by what it does.
///
/// Each carries the workspace (and, for a move, the destination) it is about,
/// so a drawing never has to tell the controller *which* row was pressed
/// separately from *what* was pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    // ------------------------------------------------------------ selection
    Select(WorkspaceId),
    Open(WorkspaceId),
    OpenInNewWindow(WorkspaceId),

    // ------------------------------ what the host performs (see [`Request`])
    Create,
    Rename(WorkspaceId),
    Appearance(WorkspaceId),
    Import,
    Export(WorkspaceId),
    CopyId(WorkspaceId),

    // ------------------------- what this surface performs, behind `workspace.migrate`
    /// **Open the destination picker.** The row that was drawn refusing until
    /// SP-15a; it now opens a list of the other profiles rather than raising a
    /// request, because choosing from a list is not something that needs a
    /// window — it is a section of this column.
    Migrate(WorkspaceId),
    /// One destination, chosen. This is where the operation begins.
    MigrateTo(WorkspaceId, ProfileId),
    /// Close the picker without choosing. A person who opened it to look at
    /// their profiles has not asked to move anything.
    CancelMigrate,

    // --------------------------------- what this surface performs, non-destructively
    /// `store.manage_workspaces`, which every ordinary window holds.
    TogglePinned(WorkspaceId),
    /// **Reorder, by a step rather than by a drag.**
    ///
    /// The manager this replaces reordered by dragging a grip, which worked
    /// because its rows were a fixed-height table with a lane down the side.
    /// This surface is a measured column, and a drag across a scrolling column
    /// is a different piece of machinery from the one that was there. Two
    /// steps keep the *capability* whole — the route takes the complete order
    /// either way — and they are reachable from a keyboard, which the drag
    /// never was. *Recommendation, recorded:* restore the drag when SP-10's
    /// shell gives this surface a table again, and keep the steps beside it.
    MoveUp(WorkspaceId),
    MoveDown(WorkspaceId),

    // ------------------------- what this surface performs, behind `workspace.destroy`
    /// `true` archives, `false` brings it back out of the archive. One control
    /// with two labels, as the manager this replaces had.
    Archive(WorkspaceId, bool),
    SoftDelete(WorkspaceId),
    Restore(WorkspaceId),

    // ------------------- recovery, behind `workspace.destroy` (root catalog §6.3)
    /// **Restore a deleted Profile by its deletion batch**: the Profile and
    /// the workspaces trashed by exactly that deletion come back; earlier
    /// Trash and later moves stand. From an Unassigned row (its former
    /// Profile) or a *Deleted profiles* row.
    RestoreProfile(ProfileId, koru_api_client::DeletionId),

    // ------------------------------------------------------------ the surface's own
    ShowArchived,
    ShowTrash,
    /// S24's affordance: run the same operation again.
    Retry,
    DismissNotice,
}

/// **What the surface cannot do for itself.**
///
/// Everything here needs a window, a file, or the system — none of which a
/// surface hosted by three different applications may reach, because the way to
/// reach them is different in each. A host drains these with
/// `take_requests` and performs them in its own vocabulary.
///
/// **Reordering is not here**, and neither is pinning: both are ordinary
/// catalog writes this surface makes through the typed client, like the
/// destructive three. What separates the two lists is a window, not a
/// permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Switch this window to the workspace. The host owns what "switch" means
    /// — a tab, a document, a session — and this surface owns none of it.
    Open(WorkspaceId),
    /// Open it in a second window. Only a host can make a window.
    OpenInNewWindow(WorkspaceId),
    /// Ask for a name and create it. The name field is a modal the host draws,
    /// and its Enter follows ADR-265 row one.
    Create,
    /// Ask for a new name. Same reason.
    Rename(WorkspaceId),
    /// Open the icon and colour picker. Same reason.
    ChangeAppearance(WorkspaceId),
    /// Choose a file and import it. A file dialog is the host's.
    Import,
    /// Choose a destination and write the backup. Same reason.
    Export(WorkspaceId),
    /// Put the workspace id on the clipboard. The clipboard is the host's.
    CopyId(WorkspaceId),
    // **`MigrateToProfile` was here, and is gone.** It was a seam waiting for a
    // route: owner reassignment did not exist, so the row was drawn refusing
    // and the request named the host as the thing that would one day perform
    // it. SP-15a built the route, and moving a workspace turned out to need no
    // window at all — a destination picker is a section of this column, and the
    // operation is a typed client call this surface is entitled to make, like
    // the destructive three. So it left this list, which is the list of things
    // only a host can do.
}
