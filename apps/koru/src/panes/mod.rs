// SPDX-License-Identifier: MPL-2.0

//! Koru's pane surfaces: the explore tree, the context column, and the outcome
//! vocabulary they report in.
//!
//! These lived in a chrome crate of their own until ADR-262 split it: the neutral
//! furniture geometry went down to `koru-shell-chrome`, and these came here —
//! to their only consumer — because they are spelled in `koru-core` terms and
//! a pane that names `NodeId` and `WorkspaceId` is Koru's, not the shell's.
//! The two rules they were built under still hold: geometry lives in one
//! function shared by drawing and hit-testing, and a pane reports what
//! happened rather than reaching into the container.

pub mod context;
pub mod explore;
pub mod fold;
pub mod home;
pub mod sidebar;

use koru_core::{NodeId, ProfileId, WorkspaceId};
use koru_session_protocol::{ProjectId, SessionId};

/// Where a dragged explore row lands relative to the target row.
///
/// Reported by chrome alongside [`Outcome::MoveNodes`]; the container turns it
/// into a `(parent, index)` for `move_node`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DropZone {
    /// Immediately before the target, as its sibling.
    Before,
    /// Immediately after the target's subtree, as its sibling.
    After,
    /// As the last child of the target (documents only).
    Nest,
}

/// The resolved destination of an Explore drag.
///
/// A row target preserves the familiar before/after/nest zones. `Library` is
/// explicit because a drag can carry every visible row, leaving no other node
/// that could serve as an anchor for a top-level drop.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DropTarget {
    Library,
    Row { node: NodeId, zone: DropZone },
}

/// What a piece of chrome wants the container to do.
///
/// The same contract `koru_editor::components::Outcome` has, and for the same
/// reason. Performing any of these is the container's job: chrome knows where
/// things are, not what they mean.
#[derive(Clone, PartialEq, Debug)]
pub enum Outcome {
    /// Not ours. The press belongs to whatever is underneath — usually the
    /// active view.
    Ignored,
    /// Handled, and nothing about the container changed. Still a reason to
    /// redraw: a hover or a pressed state may have moved.
    Consumed,

    // ------------------------------------------------------------------ tabs
    SelectTab(usize),
    CloseTab(usize),
    /// A tab dragged past its neighbour. `to` is the index it should end at.
    // Unconstructed since the ADR-262 move made dead code visible; the
    // Wave-6 Outcome split deletes or revives it deliberately.
    #[allow(dead_code)]
    MoveTab {
        from: usize,
        to: usize,
    },
    /// The `+`. What kind of thing to make is asked afterwards, by the
    /// container — this crate does not know what kinds exist.
    NewTab,
    /// Open the workspace-scoped search surface.
    Search,
    /// The rail's `?`: the container opens the help popover beside the rail.
    OpenHelp,

    // ---------------------------------------------------------------- panels
    /// A row in the explore tree, or a recent on the welcome screen.
    OpenNode(NodeId),
    /// A file row's note glyph: open the note it names.
    ///
    /// **Deliberately not [`Outcome::OpenNode`].** In the Files projection that
    /// already means *Save as* for the row's own document — and the id here is
    /// the note's, not the row's. The same reasoning that keeps
    /// [`Outcome::DownloadFile`] distinct.
    OpenNote(NodeId),
    /// Alt+click on a document or canvas row (the CTO's note): open it in a
    /// new tab **beside** the active one, without activating it — the way a
    /// browser's middle-click does. A double-click opened a tab of its own
    /// for a day and felt wrong: the first click had already opened the
    /// document, so the second looked like nothing or like a duplicate.
    OpenNodeInBackgroundTab(NodeId),
    /// A document dragged to a new place in the explore tree. `nodes` is the
    /// carried set in document order (one entry for a plain drag); `target`
    /// is the row it was dropped on and `zone` says where relative to it.
    /// Resolving that to `(parent, index)` and calling `move_node` is the
    /// container's job — this crate cannot mutate the document.
    MoveNodes {
        nodes: Vec<NodeId>,
        target: DropTarget,
    },
    /// A double-clicked image row in Explore: open it and edit its display name.
    RenameImage(NodeId),
    /// A session row in the Agents navigator was chosen. The container focuses
    /// the tab already showing that session, or opens a new session tab
    /// **bound to the existing session** — an attach, never a launch. The `+`
    /// menu stays the only place a session is created (Q-E4).
    OpenSession(SessionId),
    /// A **run** row in the Agents navigator was chosen. The container
    /// focuses the tab already showing that run, or opens one on it. A run
    /// is a durable record rather than a live process, so this is always an
    /// open and never a start — delegating is the modal's (P1.14).
    OpenRun(koru_delegate_core::RunId),
    /// The Agents navigator's *Projects* heading `+`: create a project. The
    /// container asks for a name and sends the typed create verb — the pane
    /// holds no project truth (ADR-264).
    NewAgentProject,
    /// A chat row in the **Chats** navigator was chosen. The container
    /// focuses the tab already showing that chat, or opens one on it
    /// (`open_ai_chat_tab_for`).
    OpenAiChat(koru_chats_ui::ChatId),
    OpenSocials,
    /// The Chats navigator's *New chat* row: a fresh AI chat tab.
    NewAiChat,
    /// The Delete key on a chat row. **Nothing is deleted yet**: the
    /// container asks through its confirmation and, on yes, the Chats
    /// surface sends the delete itself.
    DeleteAiChats(Vec<koru_chats_ui::ChatId>),
    /// A right-click on a chat row. The pane reports which chat; the
    /// container builds the menu, whose one verb is *Delete chat*.
    AiChatMenu {
        /// The picked chats, in list order — one, or a multi-selection (the
        /// CTO's note: delete several as several documents).
        chats: Vec<koru_chats_ui::ChatId>,
        at: (f32, f32),
    },
    /// A right-click in the Agents navigator. `Some` is a project row's menu
    /// (New session ▸, Rename…, Delete project); `None` is empty tree space or
    /// the Unassigned bucket, which offer only *New session* — the bucket is a
    /// presentation identity, never renamed or deleted (ADR-264).
    ///
    /// **The pane reports which row; it never builds the menu** — the same
    /// boundary [`Outcome::NodeMenu`] keeps.
    AgentTreeMenu {
        project: Option<ProjectId>,
        at: (f32, f32),
    },
    /// A right-click on a **session** row in the Agents navigator. The
    /// container builds Koru Terminal's session menu (Open in new tab, Move
    /// to new window, Move to project ▸, Terminate) over this session (Q-E7
    /// P8) — the pane reports which session, it never builds the menu, the
    /// same boundary [`Outcome::AgentTreeMenu`] keeps.
    AgentSessionMenu {
        session: SessionId,
        at: (f32, f32),
    },
    /// A right-click on **run** rows in the Agents navigator. Its one verb
    /// is Delete, and whether a run may be deleted — and what its results
    /// should do — is the container's to know, so the pane reports which
    /// runs and nothing else.
    ///
    /// **The multi-selection, in row order**, on [`Outcome::NodeMenu`]'s own
    /// rule: a right-click inside the selection is about the whole of it, and
    /// one outside makes that row the selection first. Never empty — the
    /// clicked row is in it either way.
    AgentRunMenu {
        runs: Vec<koru_delegate_core::RunId>,
        at: (f32, f32),
    },
    /// A right-click on a schedule parent or one of its metadata rows. Stable
    /// schedule identity prevents a watch-driven reorder from retargeting the
    /// menu; fired Run children deliberately never produce this outcome.
    AgentScheduleMenu {
        schedules: Vec<koru_delegate_core::ScheduleId>,
        at: (f32, f32),
    },
    /// **Runs dropped onto the pinned Trash row.** The Agents answer to
    /// [`Outcome::TrashNodes`] — the same gesture on rows that are not
    /// documents, so it reports runs rather than nodes.
    TrashRuns(Vec<koru_delegate_core::RunId>),
    /// A session row dropped on a project group in the Agents navigator.
    /// `None` is the drop on the Unassigned bucket — the same value the wire's
    /// `AssignProject` takes to move a session out of every project. The
    /// container sends the verb; the tree reflects the catalog that answers.
    MoveSessionToProject {
        session: SessionId,
        project: Option<ProjectId>,
    },
    /// A file row listed on this device but held on another one was clicked:
    /// fetch its bytes from a peer.
    ///
    /// Deliberately not [`Outcome::OpenNode`]. In the Files projection that
    /// already means *Save as*, and `Enter` is bound to it — so borrowing it
    /// here would silently change what the keyboard does.
    DownloadFile(NodeId),
    /// A double-clicked file whose bytes are already here: open it through the
    /// operating system's associated application.
    OpenFile(NodeId),
    /// A right-click on a workspace row of the who/where popover (the CTO's
    /// note): the container opens the workspace menu — the one a Manage
    /// row's `•••` opens — at the pointer, with the page still under it.
    ///
    /// **The pane reports which workspace; it never builds the menu.** What
    /// the workspace may have done to it is the container's to know.
    WorkspaceMenu {
        workspace: WorkspaceId,
        at: (f32, f32),
    },
    /// A Documents row's `+` (the CTO's note, Notion's *Add a page inside*):
    /// the container asks for a name and has the shell make a document as
    /// this row's last child.
    NewChildDocument(NodeId),
    /// A library row wants its menu — the `…` was pressed, or it was
    /// right-clicked.
    ///
    /// **The pane reports which node; it never builds the menu.** Whether a
    /// document can be binned, and what binning it means, are the container's to
    /// know. Same boundary [`Outcome::WorkspaceMenu`] keeps.
    NodeMenu {
        /// The row under the pointer, or the complete selection when that row
        /// is already selected. Kept in tree order so the container can apply
        /// one plural action without reconstructing selection state.
        nodes: Vec<NodeId>,
        at: (f32, f32),
        /// Whether `at` is a button's bottom-right rather than a cursor, so the
        /// menu hangs back under the button rather than out past the column.
        from_button: bool,
    },
    /// Empty space in the library wants the workspace-level import menu at the
    /// pointer. The pane identifies the gesture; the container owns formats.
    LibraryMenu {
        at: (f32, f32),
    },
    /// Rows dropped onto the pinned Trash row. The drag's answer to
    /// [`Outcome::MoveNodes`] — same gesture, different landing.
    ///
    /// Plural because a drag carries whatever was selected, and the container
    /// bins them one at a time so each still gets its own confirmation.
    TrashNodes(Vec<NodeId>),
    /// The pinned Trash row was pressed. Never raised while the bin is empty:
    /// a row that opens an empty list is a promise the list cannot keep.
    OpenTrash,
    /// A press on the explore column's **tasks rail**: a bucket, a list, or the
    /// making of one.
    ///
    /// One arm rather than four, because they are one surface's vocabulary and
    /// the container routes them together — and because the rail's own module
    /// is where a fifth belongs when the rail grows one.
    Tasks(explore::rail::TasksRailRequest),
    /// The empty column's "Create document" button. A column that only explains
    /// why it is empty leaves you to find the way out somewhere else.
    NewDocument,
    /// The empty Canvases section's *New canvas* button: make a canvas now.
    NewCanvas,
    /// The Files projection's empty-state button. The container owns the OS
    /// picker and persistence, so chrome only reports the intent.
    ImportFiles,
    /// A row of the context column's **Tasks** section: go to that task's row
    /// in the document the column is about.
    ///
    /// Deliberately not [`Outcome::OpenNode`], which means *open this document*
    /// and is answered by opening a tab. This names a node **inside the document
    /// already in front of you**, and the answer is a navigation within it —
    /// the same distinction the Tasks tab's own row makes.
    RevealTask(NodeId),
    /// The History section's explicit snapshot boundary.
    SaveVersion,
    /// Two opaque history marker keys selected in the History section.
    CompareVersions(String, String),
    /// One selected history marker, compared against the document as it is
    /// now. Comparing is a question, not an event: it saves nothing.
    CompareWithCurrent(String),
    /// The Cloud section's *Copy details*: put what the column is showing on
    /// the clipboard, so it can be pasted into a message instead of being
    /// transcribed out of a log. The pane owns the text — it is exactly what
    /// is on the screen — and the container owns the clipboard, the boundary
    /// every other copy in this app keeps.
    CopyCloudDetails,
    ToggleSidebar,
    ToggleContext,

    // ------------------------------------------------ workspaces and profiles
    //
    // The pane knows the *rows*. What a workspace is, what switching one costs,
    // and where a profile's folder lives are all the container's business.
    /// Show this workspace in **this** window, replacing what is open.
    SwitchWorkspace(WorkspaceId),
    /// A Profile row on the who/where popover's Profiles page: switch this
    /// window to that Profile, landing on the workspace it last had open.
    SwitchProfile(ProfileId),
    /// The "New workspace" row. What it is called is asked afterwards, by the
    /// container — the same contract [`Outcome::NewTab`] has.
    NewWorkspace,
    /// "New profile" — the Profile manager's create form.
    NewProfile,
    /// "Subscribe to Koru Cloud" — the website's Pricing page, the same
    /// destination the account card's Subscribe button opens.
    SubscribeCloud,
    /// Import documents into the open workspace.
    ImportContent,
    /// The gear on a workspace row: the Manage tab on the open Profile's
    /// page, scrolled to its Workspaces.
    ManageWorkspace(WorkspaceId),
    /// The gear on a Profile row: the Manage tab on that Profile's page.
    ManageProfile(ProfileId),
    /// The who/where popover's heading import button: the container hangs
    /// the import formats beside the page, which stays (the CTO's note:
    /// the heading's ⋯ became a `+` and an import button).
    WorkspacesImport,
    /// Home's Today row: the Home tab, a singleton.
    OpenHome,
    /// A recent document, from Home's nav.
    OpenRecentDocument(koru_core::DocumentId),
    /// The rail's settings square: the Settings tab.
    OpenSettings,

    // ---------------------------------------------------------------- window
    //
    // Only reachable once the caption row is the window's own rather than the
    // OS's. Each needs the `Window` handle, which is the shell's to hold.
    /// A press on empty caption: begin an OS window drag.
    DragWindow,
    Minimise,
    ToggleMaximise,
    CloseWindow,
    /// Keep this window above the others. Per-window: only the window whose
    /// button was pressed changes.
    // Unconstructed since the ADR-262 move made dead code visible; the
    // Wave-6 Outcome split deletes or revives it deliberately.
    #[allow(dead_code)]
    TogglePin,
    /// Ctrl and the ✕: put the app in the notification area instead of quitting
    /// it. The shell decides whether that is possible — with no tray to go to
    /// it closes as usual, because an app with no window and no icon is one
    /// nobody can reach.
    HideToTray,
}

impl Outcome {
    /// Whether this press was ours at all. A press that was not falls through
    /// to the view underneath.
    #[allow(dead_code)] // See the note on `MoveTab`.
    pub fn handled(&self) -> bool {
        !matches!(self, Self::Ignored)
    }
}
