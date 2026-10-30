// SPDX-License-Identifier: MPL-2.0

//! The right-click menu, over whatever was right-clicked.
//!
//! This is the menu's **semantics** — what a menu can be about ([`MenuSubject`]),
//! what a row does ([`MenuAction`]), and which rows a subject offers. The
//! geometry and drawing — where the menu sits, which row the pointer is over,
//! how it paints — are the shared [`koru_ui::Menu`] widget, which knows
//! nothing about tabs or workspaces. That keeps the semantics in one file and
//! the recipe in one crate: the search window's `…` menu drives the same
//! widget, and a panel recipe with two implementations drifts.
//!
//! It opens at a point — the cursor for a right-click, or the bottom-left of the
//! `…` button when that is what was pressed — and clamps inside the viewport so
//! it never runs off the window's edge.
//!
//! ## Why the items are a `Vec` fixed at open time
//!
//! This was a `const ITEMS: [TabAction; 6]` while a tab was the only subject.
//! Two things ended that. A workspace menu has dividers and a different length,
//! and several of its rows read differently depending on the workspace (*Pin to
//! Top* against *Unpin*). Both are answered by building the list once, when the
//! menu opens, from the subject as it was then.
//!
//! **Fixing it at open time is the point, not a shortcut.** The profile document
//! syncs on a poll, so the workspace list can change underneath an open menu; a
//! menu whose rows were recomputed each frame could relabel itself between the
//! press and the release. The subject is a `WorkspaceId` for the same reason —
//! never an index into a list that another window is writing to.

use std::borrow::Cow;
use std::cell::RefCell;

use koru_core::ids::ListId;
use koru_core::{NodeId, WorkspaceId};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;

use koru_session_profiles::built_in_profiles;
use koru_session_protocol::{ProfileKind, ProjectId, SessionId, SessionProfileKey};
/// Re-exported so this module's callers name the anchor the way they always
/// have; the enum itself lives with the widget.
pub use koru_ui::Anchor;
/// A stepper cell in a menu row — the widget's, so `zoom_controls` and
/// `MenuItem::controls` name one type.
pub use koru_ui::Control;
/// The tests assert the clamp against the same constant the widget clamps with.
#[cfg(test)]
pub(crate) use koru_ui::menu::EDGE_MARGIN;

/// What the menu is about.
///
/// A tab is identified by position because the strip is this window's alone and
/// nothing edits it while the menu is up. A workspace is identified by **id**,
/// because the list it lives in is a cache of a document two processes write to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MenuSubject {
    /// The caption `+` button: create or import rather than acting on a tab.
    NewTab,
    Tab(usize),
    Workspace(WorkspaceId),
    /// A right-click in the Agents navigator: a project row's menu, or —
    /// `project: None` — empty tree space and the Unassigned bucket, which
    /// offer only *New session*. The id, never a row index: the tree is a
    /// cache of a catalog the session host rewrites on its own schedule
    /// (ADR-264), and the bucket is a presentation identity no id names.
    AgentTree {
        project: Option<ProjectId>,
    },
    /// A right-click on a **session** row in the Agents navigator: koru-
    /// terminal's session menu (Open in new tab, Move to new window, Move to
    /// project ▸, Terminate). The session id, never a row index: the tree is
    /// a cache of a catalog the session host rewrites on its own schedule
    /// (ADR-264, Q-E7 P8).
    AgentSession {
        session: SessionId,
    },
    /// A right-click on **run** rows in the Agents navigator. One row, and
    /// it deletes them. Run ids rather than row indices, for the session
    /// menu's reason: the tree is a cache of a listing another process
    /// rewrites on its own schedule.
    ///
    /// **A set, like [`Nodes`](Self::Nodes)**, because the Agents column
    /// carries the library's multi-selection now (ADR-379): a right-click
    /// inside the selection is about the whole of it. Never empty.
    AgentRuns {
        runs: Vec<koru_delegate_core::RunId>,
    },
    /// A right-click on a chat row in the **Chats** navigator. One row, and
    /// it deletes the chat — through the confirmation, like the Delete key.
    /// The chat's id, never a row index: the list is a cache of an answer
    /// the AI service rewrites whenever a chat changes.
    AiChat {
        chat: koru_ai_ui::ChatId,
    },
    /// A schedule row or one of its metadata rows, captured by stable id.
    AgentSchedules {
        schedules: Vec<koru_delegate_core::ScheduleId>,
    },
    /// A library row in the explore column — its `…`, or a right-click on it.
    Nodes(Vec<NodeId>),
    /// Rows in the Files projection. File actions are deliberately separate
    /// from page move, export and trash actions.
    Files(Vec<NodeId>),
    /// A row in the Trash tab.
    Trashed(Vec<NodeId>),
    /// **A run pill in a document**, right-clicked. Its rows are what the run
    /// produced, and picking one opens it as a tab (ADR-369).
    ///
    /// The Run **document**'s id, because that is what the pill stores and
    /// what the window keys a run's resolved state by. Never a row index into
    /// the results: the list is a projection of a document another process
    /// writes, and it can grow while the menu is up — which is exactly why
    /// the titles are snapshotted at open ([`set_run_pill_results`]) and the
    /// action carries a position into that snapshot.
    RunPill {
        document: koru_core::DocumentId,
    },
    /// A row in the Tasks tab.
    ///
    /// **The document as well as the node**, because a task is a node in a
    /// document this window may not have open — which is exactly why every one
    /// of these actions has to say where to go looking, and why none of them is
    /// a bare `NodeId` the way an Explore row's is.
    Task {
        document: koru_core::DocumentId,
        node: NodeId,
    },
    /// A queue row in the tasks rail. Carries the whole target rather than the
    /// bucket alone, so the one *Open in new tab* row serves a queue and a list
    /// without the handler asking which it came from.
    RailTarget(crate::panes::explore::rail::RailTarget),
    /// A list row in the tasks rail.
    ///
    /// **By id, and for the workspace menu's reason**: the list table is a cache
    /// of a system document another window also writes, so a menu holding an
    /// index into it could act on whatever row slid into that position while it
    /// was open.
    List(ListId),
    /// The Trash tab's own `…`. One row, and it empties the bin.
    Trash,
    /// The window itself — what the caption's overflow button opens. Window
    /// destinations live here; minimize, maximize and close remain direct
    /// caption controls.
    Window,
}

/// What a menu item does. Resolved by the container, not here.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum MenuAction {
    /// A horizontal rule. An item like any other as far as indices go — so the
    /// `enabled` slice a caller passes lines up without it having to know which
    /// rows are real — but never hovered, never chosen, and shorter.
    Divider,
    /// Non-selectable caption for a multi-select menu: "3 Documents".
    SelectionHeader(usize),

    // --------------------------------------------------------- new content
    /// Parent of the Documents flyout on the `+` menu.
    Documents,
    NewDocument,
    /// A recent document under Documents. Title comes from the snapshot
    /// [`set_plus_snapshots`] captures when the menu opens.
    ///
    /// **Also a recent canvas under Canvases**: opening a recent is opening a
    /// document by id, and the container builds the canvas's view from its
    /// kind. The title comes from [`set_plus_canvases`]'s snapshot then.
    OpenRecentDocument(koru_core::DocumentId),
    /// Parent of the Canvases flyout on the `+` menu, beside Documents.
    Canvases,
    /// Make a canvas now — the Canvases flyout's first row (CANVAS P1.3).
    NewCanvas,
    /// The last row of the Documents flyout when it lists recents: forget
    /// them. It moved here from the Welcome page, which no longer lists them.
    ClearRecents,
    /// Select the one Welcome tab, or open it. *Get started* used to be a
    /// second row for the checklist; every Welcome is the checklist now.
    OpenWelcome,
    ImportAs,
    /// One registered import format's row, carrying the format's stable id.
    /// The label comes from the format's descriptor — the registry decides
    /// what can be imported, this menu only displays it.
    ImportFormat(&'static str),

    // ------------------------------------------------------------------ tabs
    /// Pop the tab out into its own window.
    PopOut,
    /// Close the right-clicked tab.
    Close,
    /// Close every other closable tab.
    CloseOthers,
    /// Close closable tabs to the left of the right-clicked one.
    CloseLeft,
    /// Close closable tabs to the right of the right-clicked one.
    CloseRight,
    /// Close every tab. What is left is one welcome tab, the same as closing
    /// the last one by hand — never an empty window.
    CloseAll,
    PinTab,
    UnpinTab,
    ShowTabTitle,
    HideTabTitle,
    /// Switch Explore to this tab's home workspace and scroll the file into view.
    RevealInExplore,

    // ------------------------------------------------------------ workspaces
    /// Open a new terminal session running a shell profile.
    NewShell,
    /// Open a new terminal session running an agent profile.
    NewAgent,
    /// A built-in terminal profile surfaced by the `+` menu's *Shell* or
    /// *Agents* flyout. The usize is the index into
    /// [`koru_session_profiles::built_in_profiles`].
    TerminalProfile(usize),
    /// Parent of the running-session list under *Shell*.
    ActiveShells,
    /// Parent of the running-session list under *Agents*.
    ActiveAgents,
    /// A running shell or agent session listed under *Active*. Choosing it
    /// focuses the tab already showing it, or attaches a new one.
    OpenRunningSession(SessionId),
    /// Parent of the Chats flyout on the `+` menu.
    Chats,
    /// Open a new Koru AI chat as a main tab.
    NewAiChat,
    Socials,
    NewSocialPost,
    /// **Hand work to an agent** (ROADMAP D7). One row in the `+` menu and
    /// one in a task's menu, both reaching `MainView::begin_delegation` — the
    /// same single path the editor's `/delegate` and a document selection's
    /// row reach. Two variants rather than one because the two menus start
    /// the modal from different places, and a subject-less action in a
    /// subject-ful menu is how a row ends up acting on the wrong thing.
    Delegate,
    /// *Delegate…* on a task: its title becomes the objective's first line.
    TaskDelegate,
    /// **One of a run's results**, by its position in the snapshot the menu
    /// opened with. Picking it opens that version as a tab — the same
    /// primitive the run's own Result view and the Artifacts column reach.
    OpenRunResult(usize),
    /// The row a run with nothing registered shows instead of an empty menu.
    /// Never pressable: it is a statement, not an offer.
    RunHasNoResults,
    /// A recent chat under Chats. Title comes from the snapshot
    /// [`set_plus_snapshots`] captures when the menu opens.
    OpenRecentChat(koru_ai_ui::ChatId),
    /// Delete a project from the Agents tree — bookkeeping, never a process
    /// operation in disguise (ADR-264): the record and its folder setting go,
    /// nothing on disk does. Refused while the project has running sessions,
    /// with the sentence saying so where the row is.
    DeleteProject,
    /// **Delete a run** (P7a): into the workspace's Trash, from where it
    /// comes back whole. What the Trash's own empty does to it afterwards
    /// is the Trash's question, and it is asked there.
    DeleteRun,
    /// **Delete a chat** from the Chats navigator: asks first, and the chat
    /// stays with a sentence under it when its agent could not be stopped
    /// (UC13).
    DeleteAiChat,
    /// Rename a recurring schedule in its navigator row.
    ScheduleRename,
    /// Edit the complete future definition of a recurring schedule.
    ScheduleEdit,
    /// Stop future occurrences without affecting an already claimed Run.
    SchedulePause,
    /// Start again at the next future occurrence, for any paused reason.
    ScheduleResume,
    /// Permanently retire the schedule while keeping every Run it made.
    ScheduleDelete,

    // ---------------------------------------------------- a navigator session
    // Koru Terminal's session menu, spoken in this container's rows (Q-E7 P8).
    // Their labels reuse Koru Terminal's own i18n keys so the two trees say
    // the same words; nothing new lands in the i18n table.
    /// Open or focus the session's tab — the same attach clicking the row does.
    SessionOpenInNewTab,
    /// koru.exe's pop-out: a new window seated on this session, detached here.
    SessionMoveToNewWindow,
    /// Parent of the *Move to project ▸* flyout.
    SessionMoveToProject,
    /// One destination of that flyout: `AssignProject`, `None` for *No project*
    /// (out of every project, back to the Unassigned bucket). The project the
    /// session is already in is dimmed. The name is device state, so it comes
    /// from a snapshot the menu captures at open — see
    /// [`set_session_project_names`].
    SessionAssignProject(Option<ProjectId>),
    /// The explicit kill, behind Koru Terminal's confirm.
    SessionTerminate,
    /// *Install Koru MCP*, on the tab of an agent that has no per-launch form
    /// for it (ADR-472): registers the bridge in the agent's own configuration.
    InstallMcp,
    /// The same row once the bridge is registered: takes it out again.
    UninstallMcp,
    /// Parent of the *Terminal theme ▸* flyout on a session tab's menu.
    TerminalTheme,
    /// Parent of the *Terminal font ▸* flyout.
    TerminalFont,
    /// A theme for this agent's terminals: `light`, `dark`, or `None` to
    /// follow Settings (ADR-475).
    TerminalThemeIs(Option<&'static str>),
    /// A monospaced font family's token for this agent's terminals, or `None`
    /// to follow Settings.
    TerminalFontIs(Option<&'static str>),
    /// *More fonts...*: opens the picker over every installed family.
    TerminalFontMore,

    Open,
    OpenInNewWindow,
    /// Open an Explore document in a new Main tab, even if one already shows it.
    OpenInNewTab,
    /// Open an Explore document in a new window.
    OpenDocumentInNewWindow,
    Rename,
    ChangeIconAndColor,
    Pin,
    Unpin,
    Export,
    Archive,
    Unarchive,
    Delete,
    RestoreWorkspace,
    /// A step up or down the workspace's Profile's own order (maintainers' decision 6).
    MoveWorkspaceUp,
    MoveWorkspaceDown,
    /// Parent of the *Move to Profile ▸* flyout: the Profiles this workspace
    /// could go to, snapshotted as the menu opens
    /// ([`set_workspace_menu_snapshot`]).
    MoveToProfile,
    /// One destination of that flyout. The move asks its own confirmation.
    MoveWorkspaceTo(koru_core::ProfileId),
    /// Put the workspace's id on the clipboard.
    CopyWorkspaceId,

    // ------------------------------------------------------------ attributes
    /// Parent of the attributes submenu on a library document (roadmap A2).
    Attributes,
    /// Open the tag chooser over the document's root node.
    AttributeTags,
    /// Open the accent color picker over the document's root node.
    AttributeColor,
    /// Open the due date grid over the document's root node (roadmap A3).
    AttributeDue,
    /// Open the priority picker over the document's root node (roadmap A4).
    AttributePriority,

    // ----------------------------------------------------------- the trash
    /// Open the **Move to…** picker for a library document.
    MoveTo,
    /// Multi-select form of [`MoveTo`] — counts the roots in the label.
    MoveNodesTo(usize),
    /// Move a document to the Trash. **Not destructive**: it is a move, it is
    /// undoable, and the toast that follows says so. Spelling it red would put
    /// it in the same register as *Delete permanently*, which is the one thing
    /// in this menu that cannot be taken back.
    MoveToTrash,
    MoveNodesToTrash(usize),
    /// File-selection spelling of the same reversible move.
    MoveFilesToTrash(usize),
    /// Parent of the export-format submenu.
    ExportAs,
    /// One registered export format's row, carrying the format's stable id.
    ExportFormat(&'static str),
    /// Materialize a temporary copy and ask the OS-associated application to
    /// open it.
    OpenFileInApp,
    /// Copy a workspace file to an explicitly selected destination.
    SaveFileAs,
    /// Create the file's attached note and open it. Shown for one file that
    /// has none.
    AddNote,
    /// Open the note the file already has. Shown for one file that has one.
    OpenNote,
    Restore,
    DeletePermanently,
    EmptyTrash,

    // ----------------------------------------------------------- the tasks
    // A task row's menu. Prefixed, because `Open` above already means a
    // workspace and `Restore` already means a binned document — a task is a
    // fourth subject and the bare names would collide.
    /// **Open this queue or list in a tab of its own.**
    ///
    /// The one row a bucket's menu has, and the first row a list's has. Its own
    /// action rather than [`OpenInNewTab`](Self::OpenInNewTab), whose word is
    /// plural because an Explore selection can be many documents; a rail row is
    /// always one page.
    RailOpenInNewTab,
    /// **Go to where the task is written, in a tab of its own.**
    ///
    /// The Tasks page's first row, and the whole of how somebody leaves it for
    /// the document — a whole-row click used to do this and now selects
    /// instead, so this is no longer a shortcut for the click but the gesture
    /// itself, named for where it goes rather than for what it does to a
    /// window.
    ///
    /// **It was two rows and is one.** *Go to origin* and *Open in new tab*
    /// were separate actions differing only in a `bool`, and in the ordinary
    /// case — the task's document already open in another tab — they did the
    /// same thing on screen, which is a menu telling you a lie about having a
    /// choice. The one row keeps the new tab, for the double click's reason:
    /// Back does not return you to a queue and a row, so a gesture that
    /// replaced the Tasks page would be a one-way door. The tab glyph on the
    /// row is what says so without a second sentence.
    ///
    /// Its own action rather than [`Open`](Self::Open) because
    /// [`label`](Self::label) is answered by the action alone, and *Open* is
    /// already the word for a workspace and for a binned document.
    TaskGoToOrigin,
    /// **Open the page of the list the row is in.** Offered on a queue page
    /// for a row that carries a membership, beside *View in origin*: the two
    /// are the places a row leads. The list chip on the row is the same door
    /// (finding 36).
    TaskGoToList,
    /// **Make a document for a page-made task**, in *View in origin*'s place
    /// on a row that has no origin: a page named *Notes* and the task's words,
    /// at the root of the workspace, with the task as its first paragraph —
    /// then open it there. `TaskCommand::CreateHome`.
    TaskCreateDocument,
    /// Run the canonical complete command over the row.
    TaskComplete,
    /// **Put this task on Today by hand**, whatever its due says, and take it
    /// off again. Two actions rather than one that flips, on
    /// [`TaskComplete`](Self::TaskComplete)/[`TaskReopen`](Self::TaskReopen)'s
    /// precedent: the menu is built from the row's state when it opens and
    /// frozen, so a row that relabelled itself between the press and the
    /// release is the thing that precedent exists to prevent.
    TaskPinToday,
    TaskUnpinToday,
    /// …and its other direction, on a row that is already done.
    TaskReopen,
    /// **Open the node, then open its due picker over it.** The picker is the
    /// editor's, and the editor needs the document — so this is one gesture
    /// spelled as *go there and act*, the same shape an Explore row's
    /// [`AttributeDue`](Self::AttributeDue) already has.
    ///
    /// The design draws the current value at the row's right-hand end. That is
    /// deliberately not here: [`MenuAction`] is `Copy`, a date is a `String`,
    /// and widening the enum to carry one would touch every menu in the window
    /// for a hint. The value is on the row's own chip a few pixels away.
    TaskDue,
    /// The same, for priority.
    TaskPriority,
    /// **Open the membership picker over the row**, which is a different shape
    /// from the two above and has to be: there is no editor picker for
    /// `koru.list` to go to, and there could not usefully be one — a list's
    /// values live in the workspace's system document rather than in the
    /// attribute registry, so `/attr` would offer the key with nothing behind
    /// it. The Tasks view opens its own chooser where the row is
    /// (`tasks_view::menu::ListPick`) and dispatches the ordinary task command.
    TaskList,
    // ----------------------------------------------------------- the lists
    // A rail list's menu. Prefixed, like the task and window rows above, and
    // here for a reason the collision alone does not cover: [`Rename`] carries
    // **F2** in its chord column, and F2 renames the document in front of you.
    // Borrowing the row would print a shortcut that does something else.
    /// Rename the list, in a field over the row it is already on.
    ListRename,
    /// Parent of the swatch submenu. No ellipsis: it opens rows rather than a
    /// picker, which is the difference `ExportAs` already draws.
    ListColor,
    /// One accent token's row, carrying the token itself — the same
    /// `AccentColor` string the list stores, never an index into the palette.
    ListSwatch(&'static str),
    /// Delete the list. **Behind the confirm** SPEC §8 asks for, because the
    /// memberships go with it and the tasks do not — which is a sentence worth
    /// reading before it happens rather than after.
    ListDelete,

    /// Take the row off the Tasks list. **Behind the E-3 confirm**, because it
    /// clears the due date and the marker on somebody's paragraph and the
    /// paragraph itself stays — which is a sentence worth reading before it
    /// happens rather than after.
    TaskRemove,

    // --------------------------------------------------------- the window
    // Prefixed, because `Close`, `Pin` and `Unpin` above already mean a tab and
    // a workspace. A window is a third subject and the names would collide.
    /// Keep this window above the others.
    WindowAlwaysOnTop,
    /// Let it go back to the ordinary stacking order.
    WindowUnpinTop,
    /// Fill the left or right half of the monitor's work area.
    /// Opens the placement submenu. Acts on nothing itself. These four are
    /// only ever *built* behind `cfg(windows)` (the menu row below), so on
    /// other platforms they are matched but never constructed.
    #[cfg_attr(not(windows), allow(dead_code))]
    WindowArrange,
    #[cfg_attr(not(windows), allow(dead_code))]
    WindowSnapLeft,
    #[cfg_attr(not(windows), allow(dead_code))]
    WindowSnapRight,
    #[cfg_attr(not(windows), allow(dead_code))]
    WindowResetPosition,
    HeaderSettings,
    HeaderUpdates,
    /// The zoom row: a label and the three controls [`zoom_controls`] builds.
    /// Never chosen as a whole — see [`MenuAction::selectable`].
    HeaderZoom,
    /// What that row's `−`, `+` and `↺` do. **Not rows**: they are only ever
    /// reached by pressing a control, which is why nothing puts them in an
    /// item list. They are `MenuAction`s all the same, so a press still ends
    /// up in `run_menu_action` — the one place a menu choice takes effect.
    ZoomOut,
    ZoomIn,
    ZoomActualSize,
    /// Support: the Intercom chat in a window of its own, where *Help* used
    /// to sit (ADR-276).
    HeaderSupport,
    HeaderFeedback,
    HeaderShortcuts,
    HeaderMobile,
    /// The internal session recorder: opens the setup modal
    /// (the internal session-recorder design). Only ever *built* behind the
    /// internal-and-Windows gate in [`window_items`]; a public build matches
    /// it but never constructs it — the `WindowArrange` pattern, with a
    /// runtime gate rather than a `cfg` one.
    HeaderStartRecording,
    /// What the row becomes while this window is recording: stops and
    /// finalizes. Same gate, same chord — the row is built at open time, so
    /// start and stop are two actions rather than one that relabels.
    HeaderStopRecording,
}

thread_local! {
    /// The look the session tab whose menu is open has, `(theme, font)` tokens,
    /// so its flyouts can mark the current choice: snapshotted as the menu
    /// opens, for the reason [`SESSION_PROJECT_NAMES`] is.
    static TERMINAL_LOOK: RefCell<(Option<String>, Option<String>)> =
        const { RefCell::new((None, None)) };
}

/// Record the look the menu about to open belongs to.
pub fn set_terminal_look_snapshot(look: (Option<String>, Option<String>)) {
    TERMINAL_LOOK.with(|cell| *cell.borrow_mut() = look);
}

/// A row's text, with a check before the choice that is current.
fn checked(name: &str, on: bool) -> Cow<'static, str> {
    Cow::Owned(if on {
        format!("\u{2713}  {name}")
    } else {
        format!("\u{2003} {name}")
    })
}

thread_local! {
    /// The device's projects, for the *Move to project ▸* flyout's row labels.
    /// A session menu carries project **names**, which are device state — the
    /// one kind of row text [`MenuAction::label`] cannot derive from the `Copy`
    /// action alone — so the container snapshots them here when it opens the
    /// menu, the same way `label` already reaches the global `format::lang()`.
    /// Only the UI thread ever touches it, and only one such menu is open at a
    /// time.
    static SESSION_PROJECT_NAMES: RefCell<Vec<(ProjectId, String)>> =
        const { RefCell::new(Vec::new()) };
    /// Why *Terminate* is refused for the session the open menu is about, or
    /// `None` when it can be taken — Koru Terminal's own sentence, snapshotted
    /// the same way, so [`disabled_reason`](MenuAction) can show it beside the
    /// dimmed row.
    static SESSION_TERMINATE_REFUSAL: RefCell<Option<&'static str>> = const { RefCell::new(None) };
    /// Why *Delete run* is dimmed, or `None` when it is offered — the same
    /// snapshot for the same reason: `disabled_reason` has no room for
    /// state, and the state here is a listing another process rewrites
    /// while the menu stands open.
    static RUN_DELETE_REFUSAL: RefCell<Option<&'static str>> = const { RefCell::new(None) };
    /// Why schedule management is refused for the schedule whose menu is
    /// open. Unlike the other refusal snapshots this includes a catalog name,
    /// so it owns the localized sentence rather than borrowing a static row.
    static SCHEDULE_MANAGE_REFUSAL: RefCell<Option<String>> = const { RefCell::new(None) };
    /// How many runs *Delete run* is about, snapshotted for
    /// `RUN_DELETE_REFUSAL`'s reason. One is the row's plain wording; more
    /// than one counts them, so the menu says what it will do before it is
    /// pressed rather than in the dialog afterwards.
    static RUN_DELETE_COUNT: RefCell<usize> = const { RefCell::new(1) };
    /// What the run whose pill was right-clicked has produced — one title per
    /// row, snapshotted when the menu opens for `SESSION_PROJECT_NAMES`'
    /// reason and one more: a run that is still working registers results
    /// while the menu is up, and a menu that relabelled itself between the
    /// press and the release would open whatever slid into that position.
    ///
    /// **Drawn, never logged.** A result's title is somebody's writing
    /// (standing requirement 7); it reaches this table to be painted and goes
    /// nowhere else.
    static RUN_PILL_RESULTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Snapshot what a run produced, so the pill's menu can name each result.
/// Called by the container as it opens the menu.
pub fn set_run_pill_results(titles: Vec<String>) {
    RUN_PILL_RESULTS.with(|cell| *cell.borrow_mut() = titles);
}

/// The rows a run pill's menu offers: one per result, or the one sentence
/// that says there are none. **Never an empty menu** — a panel that opens
/// with nothing in it reads as a bug rather than as an answer.
pub fn run_pill_items() -> Vec<MenuAction> {
    RUN_PILL_RESULTS.with(|cell| {
        let titles = cell.borrow();
        if titles.is_empty() {
            return vec![MenuAction::RunHasNoResults];
        }
        (0..titles.len()).map(MenuAction::OpenRunResult).collect()
    })
}

/// The title of one snapshotted result, for the row that opens it.
fn run_result_label(index: usize) -> String {
    RUN_PILL_RESULTS.with(|cell| cell.borrow().get(index).cloned().unwrap_or_default())
}

thread_local! {
    static PLUS_RECENTS: RefCell<Vec<(koru_core::DocumentId, String)>> =
        const { RefCell::new(Vec::new()) };
    static PLUS_RUNNING_SHELLS: RefCell<Vec<(SessionId, String)>> =
        const { RefCell::new(Vec::new()) };
    static PLUS_RUNNING_AGENTS: RefCell<Vec<(SessionId, String)>> =
        const { RefCell::new(Vec::new()) };
    static PLUS_CHATS: RefCell<Vec<(koru_ai_ui::ChatId, String)>> =
        const { RefCell::new(Vec::new()) };
    static PLUS_CANVASES: RefCell<Vec<(koru_core::DocumentId, String)>> =
        const { RefCell::new(Vec::new()) };
}

/// How many recents the Documents and Chats flyouts offer. Matches the
/// taskbar Jump List, so the same ten documents show in both places.
pub const PLUS_RECENT_MAX: usize = crate::taskbar::RECENT_MAX;

/// Snapshot the `+` menu's recents and running sessions. Called as the menu
/// opens, so a flyout opened a moment later still names what was true then.
pub fn set_plus_snapshots(
    recents: Vec<(koru_core::DocumentId, String)>,
    running_shells: Vec<(SessionId, String)>,
    running_agents: Vec<(SessionId, String)>,
    chats: Vec<(koru_ai_ui::ChatId, String)>,
) {
    PLUS_RECENTS.with(|cell| *cell.borrow_mut() = recents);
    PLUS_RUNNING_SHELLS.with(|cell| *cell.borrow_mut() = running_shells);
    PLUS_RUNNING_AGENTS.with(|cell| *cell.borrow_mut() = running_agents);
    PLUS_CHATS.with(|cell| *cell.borrow_mut() = chats);
}

/// Snapshot the `+` menu's recent **canvases**, beside
/// [`set_plus_snapshots`]'s recent documents: the Documents flyout lists pages
/// and the Canvases flyout lists canvases, so the container splits the one
/// recents list by kind as the menu opens.
pub fn set_plus_canvases(canvases: Vec<(koru_core::DocumentId, String)>) {
    PLUS_CANVASES.with(|cell| *cell.borrow_mut() = canvases);
}

thread_local! {
    /// The Profiles the open workspace menu's *Move to Profile ▸* flyout
    /// offers, and the reason each of its dimmed rows gives — snapshotted as
    /// the menu opens, for `SESSION_PROJECT_NAMES`' reason: a row's label and
    /// its reason come from state [`MenuAction::label`] cannot reach.
    static WORKSPACE_DESTINATIONS: RefCell<Vec<(koru_core::ProfileId, String)>> =
        const { RefCell::new(Vec::new()) };
    static WORKSPACE_REFUSALS: RefCell<Vec<(MenuAction, String)>> =
        const { RefCell::new(Vec::new()) };
}

/// Snapshot what the workspace menu about to open needs: where the workspace
/// could be moved, and why each of its dimmed rows is dimmed. Called by the
/// container as it opens the menu, every time, so no other workspace's
/// answers survive into it.
pub fn set_workspace_menu_snapshot(
    destinations: Vec<(koru_core::ProfileId, String)>,
    refusals: Vec<(MenuAction, String)>,
) {
    WORKSPACE_DESTINATIONS.with(|cell| *cell.borrow_mut() = destinations);
    WORKSPACE_REFUSALS.with(|cell| *cell.borrow_mut() = refusals);
}

fn workspace_destination_name(id: koru_core::ProfileId) -> String {
    WORKSPACE_DESTINATIONS.with(|cell| {
        cell.borrow()
            .iter()
            .find(|(profile, _)| *profile == id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| "Profile".to_owned())
    })
}

/// The *Move to Profile ▸* flyout: every other Profile, in the order the
/// controller lists them.
fn workspace_destination_items() -> Vec<MenuAction> {
    WORKSPACE_DESTINATIONS.with(|cell| {
        cell.borrow()
            .iter()
            .map(|(id, _)| MenuAction::MoveWorkspaceTo(*id))
            .collect()
    })
}

/// **Only rows no other menu has.** *Rename…* is on the tab and project menus
/// too, and the snapshot outlives the workspace menu, so a workspace's reason
/// must not turn up beside somebody else's dimmed row.
fn workspace_menu_refusal(action: MenuAction) -> Option<String> {
    if matches!(action, MenuAction::Rename) {
        return None;
    }
    WORKSPACE_REFUSALS.with(|cell| {
        cell.borrow()
            .iter()
            .find(|(row, _)| *row == action)
            .map(|(_, reason)| reason.clone())
    })
}

/// Snapshot the projects the *Move to project ▸* flyout offers. Called by the
/// container as it opens a session's menu; the flyout's rows and their labels
/// read it.
pub fn set_session_project_names(names: Vec<(ProjectId, String)>) {
    SESSION_PROJECT_NAMES.with(|cell| *cell.borrow_mut() = names);
}

/// Snapshot why *Delete run* is refused for the runs the menu is about.
pub fn set_run_delete_refusal(reason: Option<&'static str>) {
    RUN_DELETE_REFUSAL.with(|cell| *cell.borrow_mut() = reason);
}

/// Snapshot how many runs *Delete run* is about, so the row can count them.
pub fn set_run_delete_count(count: usize) {
    RUN_DELETE_COUNT.with(|cell| *cell.borrow_mut() = count.max(1));
}

/// Snapshot why *Terminate* is refused for the session the menu is about.
pub fn set_session_terminate_refusal(reason: Option<&'static str>) {
    SESSION_TERMINATE_REFUSAL.with(|cell| *cell.borrow_mut() = reason);
}

/// Snapshot the owner-specific refusal shown beside a remote schedule's
/// disabled management rows. `None` means this device owns the schedule.
pub fn set_schedule_manage_refusal(reason: Option<String>) {
    SCHEDULE_MANAGE_REFUSAL.with(|cell| *cell.borrow_mut() = reason);
}

/// The snapshotted name for a project id, or a plain fallback for one the
/// snapshot does not hold (the catalog moved under an open menu).
fn session_project_name(id: ProjectId) -> String {
    SESSION_PROJECT_NAMES.with(|cell| {
        cell.borrow()
            .iter()
            .find(|(project, _)| *project == id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| "Project".to_owned())
    })
}

/// The *Move to project ▸* flyout: *No project* first — the value the wire
/// takes to move a session out of every project — then each snapshotted
/// project. The current project is dimmed by the container's enabled mask, not
/// omitted, so the check is the explanation (Koru Terminal's `destination_rows`).
fn session_project_items() -> Vec<MenuAction> {
    let mut items = vec![MenuAction::SessionAssignProject(None)];
    SESSION_PROJECT_NAMES.with(|cell| {
        for (id, _) in cell.borrow().iter() {
            items.push(MenuAction::SessionAssignProject(Some(*id)));
        }
    });
    items
}

impl MenuAction {
    /// Whether this build shows the surface the row belongs to
    /// (`koru_features`, guide/contributing/FEATURE-FLAGS.md).
    ///
    /// **Every menu passes through [`shown`]** as [`ContextMenu`] is built, and
    /// `MainView::run_menu_action` refuses a hidden action as well, so a row
    /// cannot be reached by an index that outlived a filter. Shell rows and
    /// shell sessions are never hidden; agent ones are with agents.
    pub fn shown_in_product(self) -> bool {
        use koru_features as f;
        match self {
            // Agents (shell sessions stay).
            Self::NewAgent | Self::ActiveAgents => f::agents(),
            // A project is a row of the Agents view's tree.
            Self::DeleteProject => f::agent_views(),
            Self::TerminalProfile(index) => {
                f::agents()
                    || built_in_profiles()
                        .into_iter()
                        .nth(index)
                        .is_none_or(|profile| profile.kind != ProfileKind::Agent)
            }
            // Koru AI chat and Delegate, with the runs and schedules Delegate
            // makes.
            Self::NewAiChat
            | Self::Chats
            | Self::OpenRecentChat(_)
            | Self::DeleteAiChat
            | Self::Delegate
            | Self::TaskDelegate
            | Self::OpenRunResult(_)
            | Self::RunHasNoResults
            | Self::DeleteRun
            | Self::ScheduleRename
            | Self::ScheduleEdit
            | Self::SchedulePause
            | Self::ScheduleResume
            | Self::ScheduleDelete => f::ai(),
            // The Tasks view: its rows, its rail, its lists — and the
            // priority attribute, whose only reader is the Tasks view. Due
            // stays: it still makes a task.
            Self::RailOpenInNewTab
            | Self::TaskGoToOrigin
            | Self::TaskGoToList
            | Self::TaskCreateDocument
            | Self::TaskComplete
            | Self::TaskPinToday
            | Self::TaskUnpinToday
            | Self::TaskReopen
            | Self::TaskDue
            | Self::TaskPriority
            | Self::TaskList
            | Self::TaskRemove
            | Self::ListRename
            | Self::ListColor
            | Self::ListSwatch(_)
            | Self::ListDelete
            | Self::AttributePriority => f::tasks(),
            // Canvases: the `+` flyout and its rows. A recent canvas is an
            // `OpenRecentDocument`, listed only inside this flyout.
            Self::Canvases | Self::NewCanvas => f::canvas(),
            // Socials authoring (the Agent's publisher runs regardless).
            Self::Socials | Self::NewSocialPost => f::socials(),
            _ => true,
        }
    }

    pub fn label(self) -> Cow<'static, str> {
        match self {
            MenuAction::SelectionHeader(count) => {
                return Cow::Owned(if count == 1 {
                    "1 Document".into()
                } else {
                    format!("{count} Documents")
                });
            }
            MenuAction::MoveNodesToTrash(_) => return Cow::Borrowed("Move to trash"),
            // The token as it is written in the swatch set, with its first
            // letter raised. One source, so a colour added to `ACCENT_TOKENS`
            // gets a row here without a second table to keep in step.
            MenuAction::ListSwatch(token) => return Cow::Owned(color_label(token)),
            MenuAction::TerminalTheme => return Cow::Borrowed("Terminal theme"),
            MenuAction::TerminalFont => return Cow::Borrowed("Terminal font"),
            MenuAction::TerminalFontMore => return Cow::Borrowed("More fonts\u{2026}"),
            MenuAction::TerminalThemeIs(token) => {
                let name = match token {
                    None => "Default (Settings)",
                    Some("light") => "Light",
                    Some("dark") => "Dark",
                    Some(other) => other,
                };
                let on = TERMINAL_LOOK.with(|look| look.borrow().0.as_deref() == token);
                return checked(name, on);
            }
            MenuAction::TerminalFontIs(token) => {
                let name = match token {
                    None => "Default (Settings)".to_owned(),
                    Some(token) => koru_draw::theme::FontChoice::parse(token)
                        .label()
                        .to_owned(),
                };
                let on = TERMINAL_LOOK.with(|look| look.borrow().1.as_deref() == token);
                return checked(&name, on);
            }
            MenuAction::MoveFilesToTrash(count) => {
                return Cow::Owned(format!("Move {count} to trash"));
            }
            // The result's own name, as the run registered it.
            MenuAction::OpenRunResult(index) => return Cow::Owned(run_result_label(index)),
            // The run views' own sentence for a run that registered nothing,
            // so the pill and the run say the same thing.
            MenuAction::RunHasNoResults => {
                return Cow::Borrowed(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::RunResultNone,
                ));
            }
            MenuAction::Divider => "",
            MenuAction::Documents => "Documents",
            MenuAction::NewDocument => "New",
            MenuAction::Canvases => {
                koru_i18n::t(crate::format::lang(), koru_i18n::Key::CanvasesTitle)
            }
            MenuAction::NewCanvas => koru_i18n::t(crate::format::lang(), koru_i18n::Key::CanvasNew),
            MenuAction::OpenRecentDocument(id) => {
                return Cow::Owned(plus_recent_title(id));
            }
            MenuAction::NewShell => "Shell",
            MenuAction::NewAgent => "Agents",
            MenuAction::ActiveShells | MenuAction::ActiveAgents => "Active",
            MenuAction::TerminalProfile(index) => {
                return Cow::Owned(terminal_profile_label(index));
            }
            MenuAction::Delegate | MenuAction::TaskDelegate => {
                return Cow::Borrowed(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::DelegateMenuRow,
                ));
            }
            MenuAction::OpenRunningSession(id) => {
                return Cow::Owned(plus_running_title(id));
            }
            MenuAction::Chats => "Chats",
            MenuAction::NewAiChat => "New",
            MenuAction::Socials => {
                koru_i18n::socials::t(crate::format::lang(), koru_i18n::socials::Key::Title)
            }
            MenuAction::NewSocialPost => {
                koru_i18n::socials::t(crate::format::lang(), koru_i18n::socials::Key::NewPost)
            }
            MenuAction::OpenRecentChat(id) => {
                return Cow::Owned(plus_chat_title(id));
            }
            // The i18n rows Koru Terminal's tree menu already owns; nothing
            // new lands in i18n.
            MenuAction::DeleteProject => {
                return Cow::Borrowed(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::TerminalDeleteProject,
                ));
            }
            MenuAction::DeleteAiChat => {
                return Cow::Borrowed(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::ChatsDelete,
                ));
            }
            MenuAction::DeleteRun => {
                let lang = crate::format::lang();
                let count = RUN_DELETE_COUNT.with(|cell| *cell.borrow());
                if count <= 1 {
                    return Cow::Borrowed(koru_i18n::t(lang, koru_i18n::Key::RunDelete));
                }
                // **The row says how many.** A menu raised on four selected
                // runs whose one verb read *Delete run* would be a promise
                // about one of them.
                return Cow::Owned(koru_i18n::fill(
                    koru_i18n::t(lang, koru_i18n::Key::RunDeleteMany),
                    &[&koru_i18n::number::decimal(lang, count as u64)],
                ));
            }
            // The session menu reuses Koru Terminal's i18n keys, so the two
            // trees say the same words (Q-E7 P8).
            MenuAction::SessionOpenInNewTab => {
                return Cow::Borrowed(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::TerminalOpenInNewTab,
                ));
            }
            MenuAction::SessionMoveToNewWindow => {
                return Cow::Borrowed(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::TerminalPopOut,
                ));
            }
            MenuAction::SessionMoveToProject => {
                return Cow::Borrowed(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::TerminalMoveToProject,
                ));
            }
            MenuAction::SessionAssignProject(None) => {
                return Cow::Borrowed(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::TerminalMoveOutOfProject,
                ));
            }
            MenuAction::SessionAssignProject(Some(id)) => {
                return Cow::Owned(session_project_name(id));
            }
            MenuAction::MoveWorkspaceTo(profile) => {
                return Cow::Owned(workspace_destination_name(profile));
            }
            MenuAction::SessionTerminate => {
                return Cow::Borrowed(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::TerminalTerminate,
                ));
            }
            MenuAction::OpenWelcome => "Welcome",
            MenuAction::ClearRecents => "Clear recents",
            MenuAction::ImportAs => "Import\u{2026}",
            MenuAction::ImportFormat(id) => {
                koru_workspace_host::formats::import_menu_name(id, crate::format::lang())
                    .unwrap_or(id)
            }
            MenuAction::PopOut => "Move to New Window",
            MenuAction::InstallMcp => "Install Koru MCP",
            MenuAction::UninstallMcp => "Remove Koru MCP",
            MenuAction::Close => "Close",
            MenuAction::CloseOthers => "Close Other Tabs",
            MenuAction::CloseLeft => "Close Tabs to the Left",
            MenuAction::CloseRight => "Close Tabs to the Right",
            MenuAction::CloseAll => "Close All Tabs",
            MenuAction::PinTab => "Pin tab",
            MenuAction::UnpinTab => "Unpin",
            MenuAction::ShowTabTitle => "Show title",
            MenuAction::HideTabTitle => "Hide title",
            MenuAction::RevealInExplore => "Reveal in Explore View",
            MenuAction::Open => "Open",
            MenuAction::OpenInNewWindow => "Open in New Window",
            // Multi-select puts the count in [`SelectionHeader`]; the row stays
            // short. "Tabs" is honest for one or many — each open is its own tab.
            MenuAction::OpenInNewTab => "Open in new tabs",
            MenuAction::OpenDocumentInNewWindow => "Open in a new window",
            MenuAction::Rename => "Rename…",
            MenuAction::ScheduleRename => {
                return Cow::Borrowed(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::ScheduleRename,
                ));
            }
            MenuAction::ScheduleEdit => {
                return Cow::Borrowed(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::ScheduleEdit,
                ));
            }
            MenuAction::SchedulePause => {
                return Cow::Borrowed(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::SchedulePause,
                ));
            }
            MenuAction::ScheduleResume => {
                return Cow::Borrowed(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::ScheduleResume,
                ));
            }
            MenuAction::ScheduleDelete => {
                return Cow::Borrowed(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::ScheduleDelete,
                ));
            }
            MenuAction::ChangeIconAndColor => "Change Icon & Color…",
            MenuAction::Pin => "Pin to Top",
            MenuAction::Unpin => "Unpin",
            MenuAction::Export => "Export Backup…",
            MenuAction::Archive => "Archive Workspace",
            MenuAction::Unarchive => "Unarchive Workspace",
            MenuAction::RestoreWorkspace => "Restore Workspace",
            MenuAction::Delete => "Delete Workspace…",
            MenuAction::MoveWorkspaceUp => "Move Up",
            MenuAction::MoveWorkspaceDown => "Move Down",
            MenuAction::MoveToProfile => "Move to Profile",
            MenuAction::CopyWorkspaceId => "Copy ID",
            MenuAction::Attributes => "Attributes",
            MenuAction::AttributeTags => "Tags\u{2026}",
            MenuAction::AttributeColor => "Color\u{2026}",
            MenuAction::AttributeDue => "Due\u{2026}",
            MenuAction::AttributePriority => "Priority\u{2026}",
            MenuAction::MoveTo => "Move to\u{2026}",
            MenuAction::MoveNodesTo(_) => "Move to\u{2026}",
            MenuAction::MoveToTrash => "Move to trash",
            MenuAction::ExportAs => "Export",
            MenuAction::ExportFormat(id) => koru_workspace_host::formats::export_descriptor(id)
                .map(|descriptor| descriptor.name)
                .unwrap_or(id),
            MenuAction::OpenFileInApp => "Open in app",
            MenuAction::SaveFileAs => "Save as\u{2026}",
            MenuAction::AddNote => "Add note",
            MenuAction::OpenNote => "Open note",
            MenuAction::Restore => "Restore",
            MenuAction::DeletePermanently => "Delete permanently",
            MenuAction::EmptyTrash => "Empty Trash",
            MenuAction::RailOpenInNewTab => "Open in new tab",
            MenuAction::TaskGoToOrigin => "View in origin",
            MenuAction::TaskGoToList => "Go to list",
            MenuAction::TaskCreateDocument => "Create document",
            MenuAction::TaskComplete => "Complete",
            MenuAction::TaskPinToday => "Add to Today",
            MenuAction::TaskUnpinToday => "Remove from Today",
            MenuAction::TaskReopen => "Reopen",
            MenuAction::TaskDue => "Due\u{2026}",
            MenuAction::TaskPriority => "Priority\u{2026}",
            MenuAction::TaskList => "List\u{2026}",
            MenuAction::ListRename => "Rename\u{2026}",
            MenuAction::ListColor => "Color",
            MenuAction::ListDelete => "Delete list\u{2026}",
            MenuAction::TaskRemove => "Remove from Tasks",
            MenuAction::WindowAlwaysOnTop => "Always on Top",
            MenuAction::WindowUnpinTop => "Turn Off Always on Top",
            MenuAction::WindowArrange => "Arrange Window",
            MenuAction::WindowSnapLeft => "Snap Left",
            MenuAction::WindowSnapRight => "Snap Right",
            MenuAction::WindowResetPosition => "Reset Window Position",
            MenuAction::HeaderSettings => "Settings",
            MenuAction::HeaderUpdates => "Updates",
            MenuAction::HeaderZoom => "Zoom",
            // Never drawn — the controls are glyphs — but the registry is
            // still where the words come from, so a row made of one of these
            // later would say what the View menu already says.
            MenuAction::ZoomOut => koru_keys::registry::get(koru_keys::ShortcutId::ZoomOut).label,
            MenuAction::ZoomIn => koru_keys::registry::get(koru_keys::ShortcutId::ZoomIn).label,
            MenuAction::ZoomActualSize => {
                koru_keys::registry::get(koru_keys::ShortcutId::ZoomReset).label
            }
            MenuAction::HeaderSupport => {
                koru_i18n::t(crate::format::lang(), koru_i18n::Key::SupportWindowTitle)
            }
            MenuAction::HeaderFeedback => "Feedback",
            MenuAction::HeaderShortcuts => "Keyboard shortcuts",
            MenuAction::HeaderMobile => "Get the mobile app",
            MenuAction::HeaderStartRecording => {
                koru_i18n::t(crate::format::lang(), koru_i18n::Key::RecordingMenuStart)
            }
            MenuAction::HeaderStopRecording => {
                koru_i18n::t(crate::format::lang(), koru_i18n::Key::RecordingMenuStop)
            }
        }
        .into()
    }

    /// The chord that does the same thing, set quietly at the row's right-hand
    /// end. A blank column says "no shortcut" more honestly than an invented
    /// chord — which is why `Rename` names F2 and nothing else claims one.
    ///
    /// **From the registry, not a literal.** Every one of these used to be a
    /// hardcoded `"Ctrl+…"`, and every one of them was wrong on macOS — see
    /// `koru_keys::registry`. `String` rather than `&'static str` because ⌘S
    /// and `Ctrl+S` cannot both be baked into the binary.
    ///
    /// **The section switch is asked here rather than at the two draw sites**,
    /// and that is the load-bearing half: `menu_width` measures the chord column
    /// and `draw` paints it, so gating only the paint would leave every row a
    /// gap where a chord used to be. One answer, both callers.
    ///
    /// Not every row's chord can be a [`koru_keys::Hinted`] — *Open in New
    /// Window* is a modified **click**, built per platform rather than looked
    /// up — which is why this asks `hint::shown` and formats, instead of
    /// returning something `show_hint` could take.
    pub fn shortcut(self) -> Option<String> {
        if !koru_keys::hint::shown(koru_keys::Section::Menu) {
            return None;
        }
        // The primary modifier, written the way this platform writes it, for
        // the row whose gesture is a *click* rather than a chord.
        let primary = || {
            koru_keys::label_for(koru_keys::PRIMARY, koru_keys::On::Letter(' '))
                .trim_end()
                .to_string()
        };
        match self {
            // A click with the primary modifier, not a chord — so it is built
            // rather than looked up. `⌘-click` on macOS, `Ctrl+Click` elsewhere.
            MenuAction::OpenInNewWindow => {
                let sep = if cfg!(target_os = "macos") { "" } else { "+" };
                Some(format!("{}{sep}Click", primary()))
            }
            // **Windows' own window-management chords**, which exist only there:
            // the rows carrying them are `#[cfg(windows)]` for the same reason
            // (`snap::place` is a no-op elsewhere).
            MenuAction::WindowSnapLeft => cfg!(windows).then(|| "Win+Left".to_string()),
            MenuAction::WindowSnapRight => cfg!(windows).then(|| "Win+Right".to_string()),
            _ => self.shortcut_id().map(koru_keys::label_of),
        }
    }

    /// **The registry entry whose chord does the same thing as this action**,
    /// where a registry can express it.
    ///
    /// Split out of [`shortcut`](Self::shortcut) because a chord is now asked
    /// for twice, in two different sections: the row's own quiet column, and
    /// the tooltip on a stepper control. Two lookups meant two lists, and two
    /// lists of "which chord is this?" is exactly how a menu ends up teaching a
    /// chord the app does not have.
    ///
    /// [`OpenInNewWindow`](Self::OpenInNewWindow) and the two Windows snaps are
    /// deliberately absent: a modified *click* and a shell chord are not
    /// registry entries, so they stay special cases in `shortcut` rather than
    /// pretending to be ids here.
    pub fn shortcut_id(self) -> Option<koru_keys::ShortcutId> {
        use koru_keys::ShortcutId;
        Some(match self {
            MenuAction::NewDocument => ShortcutId::NewTab,
            MenuAction::Close => ShortcutId::CloseTab,
            MenuAction::Rename | MenuAction::ScheduleRename => ShortcutId::RenameDocument,
            MenuAction::RevealInExplore => ShortcutId::ToggleExplore,
            MenuAction::MoveTo | MenuAction::MoveNodesTo(_) => ShortcutId::MoveDocument,
            MenuAction::WindowAlwaysOnTop | MenuAction::WindowUnpinTop => ShortcutId::PinWindow,
            MenuAction::HeaderSettings => ShortcutId::Settings,
            // The stepper's three. They are never rows, so nothing draws these
            // in a chord column — they are here because the controls' tooltips
            // teach the chords, and the chord a tooltip teaches must be the one
            // the app actually dispatches.
            MenuAction::ZoomOut => ShortcutId::ZoomOut,
            MenuAction::ZoomIn => ShortcutId::ZoomIn,
            MenuAction::ZoomActualSize => ShortcutId::ZoomReset,
            MenuAction::HeaderStartRecording => ShortcutId::ToggleRecording,
            MenuAction::HeaderStopRecording => ShortcutId::ToggleRecording,
            _ => return None,
        })
    }

    /// The Phosphor glyph drawn beside the label, if this item carries one.
    /// AI Chat carries the same sparkle as its other launch surfaces; Pop Out
    /// and stateful/destructive rows use their established action glyphs.
    ///
    /// Stateful window rows carry one so the current pin state remains visible.
    pub fn icon(self) -> Option<char> {
        match self {
            MenuAction::Documents | MenuAction::NewDocument => Some(koru_draw::icons::DOCUMENT),
            MenuAction::Canvases | MenuAction::NewCanvas => Some(koru_draw::icons::CANVAS),
            MenuAction::NewShell => Some(koru_draw::icons::TERMINAL),
            MenuAction::NewAgent => Some(koru_draw::icons::SPARKLE),
            // A profile row wears the launched product's mark (Q-E6 P3/P4).
            // A glyph mark — the *System* row's OS logo — answers here; a
            // vector brand mark answers through [`brand_icon`] instead, and
            // an unknown profile keeps a plain row.
            MenuAction::TerminalProfile(index) => match terminal_profile_mark(index) {
                Some(koru_draw::brand::ProfileMark::Glyph(glyph)) => Some(glyph),
                _ => None,
            },
            MenuAction::NewAiChat => Some(koru_draw::icons::SPARKLE),
            MenuAction::Socials | MenuAction::NewSocialPost => Some(koru_draw::icons::MEGAPHONE),
            MenuAction::OpenWelcome => Some(koru_draw::icons::HOUSE),
            // The Agents identity glyph, the same one the slash row and
            // the document's row wear: four gestures, one mark.
            MenuAction::Delegate | MenuAction::TaskDelegate => Some(koru_draw::icons::ROBOT),
            // A result is a document, and it opens as one.
            MenuAction::OpenRunResult(_) => Some(koru_draw::icons::FILE),
            MenuAction::Chats => Some(koru_draw::icons::SPARKLE),
            MenuAction::HeaderFeedback => Some(koru_draw::icons::USER_SOUND),
            MenuAction::HeaderSupport => Some(koru_draw::icons::HEADSET),
            MenuAction::PopOut | MenuAction::SessionMoveToNewWindow => {
                Some(koru_draw::icons::POP_OUT)
            }
            MenuAction::PinTab | MenuAction::UnpinTab => Some(koru_draw::icons::PUSH_PIN),
            MenuAction::MoveToTrash
            | MenuAction::MoveNodesToTrash(_)
            | MenuAction::MoveFilesToTrash(_)
            | MenuAction::EmptyTrash
            | MenuAction::DeletePermanently => Some(koru_draw::icons::TRASH),
            MenuAction::Restore | MenuAction::RestoreWorkspace => Some(koru_draw::icons::UNDO),
            // The tab glyph, because the row opens one — it is what carries
            // *in a tab of its own* now that the second row is gone.
            MenuAction::TaskGoToOrigin => Some(koru_draw::icons::TABS),
            MenuAction::TaskGoToList => Some(koru_draw::icons::LIST),
            MenuAction::TaskCreateDocument => Some(koru_draw::icons::DOCUMENT),
            MenuAction::ListColor => Some(koru_draw::icons::PALETTE),
            MenuAction::ListDelete | MenuAction::DeleteAiChat => Some(koru_draw::icons::TRASH),
            MenuAction::Attributes => Some(koru_draw::icons::FADERS),
            MenuAction::AttributeTags => Some(koru_draw::icons::TAG),
            MenuAction::AttributeColor => Some(koru_draw::icons::PALETTE),
            MenuAction::AttributeDue => Some(koru_draw::icons::DATE),
            MenuAction::AttributePriority => Some(koru_draw::icons::FLAG),
            MenuAction::AddNote | MenuAction::OpenNote => Some(koru_draw::icons::NOTE),
            MenuAction::WindowAlwaysOnTop | MenuAction::WindowUnpinTop => {
                Some(koru_draw::icons::PUSH_PIN)
            }
            // A window's outline: what the rows behind it all move.
            MenuAction::WindowArrange => Some(koru_draw::icons::SQUARE),
            MenuAction::WindowSnapLeft => Some(koru_draw::icons::ALIGN_LEFT),
            MenuAction::WindowSnapRight => Some(koru_draw::icons::ALIGN_RIGHT),
            MenuAction::WindowResetPosition => Some(koru_draw::icons::UNDO),
            // The record dot — the one row that starts a capture.
            MenuAction::HeaderStartRecording => Some(koru_draw::icons::RECORD),
            // …and its square while the capture is live.
            MenuAction::HeaderStopRecording => Some(koru_draw::icons::STOP),
            _ => None,
        }
    }

    /// Whether the glyph draws in Phosphor's **Fill** weight rather than the
    /// regular one.
    ///
    /// One row, and it is a state rather than a verb: *Turn Off Always on Top*
    /// only appears while the window is on top, so its pin is solid — the same
    /// signal the caption's pin gives, kept alive now that the button is a menu.
    pub fn filled_icon(self) -> bool {
        matches!(self, MenuAction::WindowUnpinTop)
    }

    /// The vector product mark a profile row wears when its product's logo is
    /// path data rather than a Phosphor glyph — Claude's starburst, OpenAI's
    /// knot and the rest. The other half of [`icon`](Self::icon)'s
    /// `TerminalProfile` arm: each row answers exactly one of the two.
    pub fn brand_icon(self) -> Option<&'static koru_draw::brand::BrandIcon> {
        match self {
            MenuAction::TerminalProfile(index) => match terminal_profile_mark(index) {
                Some(koru_draw::brand::ProfileMark::Brand(brand)) => Some(brand),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether the pointer and the keyboard may land here.
    /// Whether the pointer and the keyboard may land on the row as a whole.
    ///
    /// [`HeaderZoom`](Self::HeaderZoom) is not: it is a label and three
    /// controls, and there is no such thing as "choosing Zoom". Pressing it
    /// goes through [`zoom_controls`], and the keyboard steps past it to the
    /// next real destination.
    pub fn selectable(self) -> bool {
        !matches!(
            self,
            MenuAction::Divider | MenuAction::SelectionHeader(_) | MenuAction::HeaderZoom
        )
    }

    /// Whether choosing this is destructive enough to be spelled in red.
    ///
    /// The rows that take something away. *Archive* is not on this list: it
    /// hides a workspace and is undone by unarchiving it.
    ///
    /// [`TaskRemove`](Self::TaskRemove) is, and it is the odd one — it destroys
    /// no content at all. It is red because the *document view's* copy of the
    /// same row is (`remove_from_tasks_is_the_red_last_row_below_a_hairline`),
    /// and one gesture spelled two ways in two menus is worse than a red row
    /// that clears a due date.
    ///
    /// [`ListDelete`](Self::ListDelete) is red for the plainer reason: the list
    /// does not come back. That the tasks in it do is what its confirmation is
    /// for, not something a quiet row could say.
    pub fn destructive(self) -> bool {
        matches!(
            self,
            MenuAction::Delete
                | MenuAction::DeleteProject
                | MenuAction::DeleteRun
                | MenuAction::DeleteAiChat
                | MenuAction::DeletePermanently
                | MenuAction::EmptyTrash
                | MenuAction::SessionTerminate
                | MenuAction::TaskRemove
                | MenuAction::ListDelete
        )
    }
}

/// How the shared widget sees a row. The bodies stay on the inherent methods
/// above — they are the semantics, and every older call site already names
/// them; this impl only forwards.
impl koru_ui::MenuItem for MenuAction {
    fn label(&self) -> Cow<'_, str> {
        (*self).label()
    }

    fn icon(&self) -> Option<char> {
        (*self).icon()
    }

    fn filled_icon(&self) -> bool {
        (*self).filled_icon()
    }

    fn brand_icon(&self) -> Option<&'static koru_draw::brand::BrandIcon> {
        (*self).brand_icon()
    }

    fn shortcut(&self) -> Option<String> {
        (*self).shortcut()
    }

    fn has_submenu(&self) -> bool {
        submenu(*self).is_some()
    }

    fn divider(&self) -> bool {
        matches!(self, MenuAction::Divider)
    }

    fn height(&self) -> f32 {
        match self {
            // A touch shorter than a real row so the caption sits as a heading.
            MenuAction::SelectionHeader(_) => theme::ROW_H - 4.0,
            MenuAction::Divider => 8.0,
            _ => theme::ROW_H,
        }
    }

    fn destructive(&self) -> bool {
        (*self).destructive()
    }

    fn selectable(&self) -> bool {
        (*self).selectable()
    }

    fn controls(&self) -> Vec<koru_ui::Control> {
        row_controls(*self)
            .into_iter()
            .map(|(control, _)| control)
            .collect()
    }

    /// A dimmed row that does not explain itself is a dead end; the rows that
    /// can be dimmed here are dimmed for reasons a person cannot see from the
    /// row, so each answers with its one reason and needs no subject.
    ///
    /// A profile row has exactly one way to be dimmed: the presence probe said
    /// its executable is not on this machine (`MainView::item_is_enabled` over
    /// the cached probe). One rule for shells and agents alike, so the note
    /// reads beside whichever program the probe found missing — the same
    /// *Not installed* treatment Koru Terminal's launch menus give it.
    fn disabled_reason(&self) -> Option<Cow<'static, str>> {
        // A workspace row's reason is the one its menu worked out as it
        // opened: the controller's, or the window's own.
        if let Some(reason) = workspace_menu_refusal(*self) {
            return Some(Cow::Owned(reason));
        }
        match self {
            MenuAction::Open | MenuAction::OpenInNewWindow => {
                Some(Cow::Borrowed("This workspace is already open"))
            }
            MenuAction::Archive | MenuAction::Delete => {
                Some(Cow::Borrowed("Koru needs one workspace open"))
            }
            MenuAction::TerminalProfile(_) => Some(Cow::Borrowed(koru_i18n::t(
                crate::format::lang(),
                koru_i18n::Key::TerminalNotInstalled,
            ))),
            // An importer this build has no configuration for — the Google
            // Docs importer without an OAuth registration (C6).
            MenuAction::ImportFormat(id) => {
                koru_workspace_host::formats::import_unavailable_reason(id, crate::format::lang())
                    .map(Cow::Borrowed)
            }
            // Koru Terminal's own sentence beside the refusal, so the two
            // trees say the same thing about the same rule.
            MenuAction::DeleteProject => Some(Cow::Borrowed(koru_i18n::t(
                crate::format::lang(),
                koru_i18n::Key::TerminalDeleteProjectRunning,
            ))),
            // Koru Terminal's own sentence beside a Terminate that cannot be
            // taken (the session exited, or another window controls it),
            // snapshotted when the menu opened (Q-E7 P8).
            MenuAction::SessionTerminate => {
                SESSION_TERMINATE_REFUSAL.with(|cell| cell.borrow().map(Cow::Borrowed))
            }
            // A run that is still writing into a folder cannot be put away
            // reversibly, which is the whole of why the row is dimmed. The
            // sentence is the same one the refusal would say (P7a).
            MenuAction::DeleteRun => {
                RUN_DELETE_REFUSAL.with(|cell| cell.borrow().map(Cow::Borrowed))
            }
            MenuAction::ScheduleRename
            | MenuAction::ScheduleEdit
            | MenuAction::SchedulePause
            | MenuAction::ScheduleResume
            | MenuAction::ScheduleDelete => {
                SCHEDULE_MANAGE_REFUSAL.with(|cell| cell.borrow().clone().map(Cow::Owned))
            }
            _ => None,
        }
    }
}

/// The tab menu.
///
/// **Closing verbs first, then the one that keeps the tab.** *Move to New
/// Window* is the only row here that does not get rid of anything — it moves
/// the tab somewhere you can still see it — so it sits below a divider rather
/// than at the head of a list of four ways to close things. It led that list
/// before, which put the one constructive row where the eye lands first and the
/// most-used one (*Close*) second.
pub const TAB_ITEMS: [MenuAction; 7] = [
    MenuAction::Close,
    MenuAction::CloseOthers,
    MenuAction::CloseLeft,
    MenuAction::CloseRight,
    MenuAction::CloseAll,
    MenuAction::Divider,
    MenuAction::PopOut,
];

/// A tab over a real workspace document also owns the two document-level
/// actions that are useful without returning to Explore. Tabs with no manifest
/// document — Welcome, Manage, Trash, drafts and release notes — keep the
/// ordinary tab menu rather than showing document actions dimmed.
///
/// `pinnable` is true for tabs that can be pinned even though they have no
/// document, such as an AI chat tab.
#[cfg(test)]
pub fn tab_items(
    document: bool,
    pinnable: bool,
    pinned: bool,
    show_title: bool,
) -> Vec<MenuAction> {
    tab_items_for(document, pinnable, pinned, show_title, None, false)
}

/// [`tab_items`], with *Install Koru MCP* above the window verb when the tab
/// is an agent that can be given the bridge only by registering it. `mcp` is
/// `None` for any other tab, and for an agent whether it is registered already
/// (then the row removes it).
pub fn tab_items_for(
    document: bool,
    pinnable: bool,
    pinned: bool,
    show_title: bool,
    mcp: Option<bool>,
    terminal_look: bool,
) -> Vec<MenuAction> {
    let mut items = Vec::new();
    if (document || pinnable) && !pinned {
        items.push(MenuAction::PinTab);
        items.push(MenuAction::Divider);
    }
    if pinned {
        items.push(MenuAction::UnpinTab);
        items.push(if show_title {
            MenuAction::HideTabTitle
        } else {
            MenuAction::ShowTabTitle
        });
        items.push(MenuAction::Divider);
    }
    items.extend_from_slice(&TAB_ITEMS);
    // A session tab draws a terminal, so its theme and font are its own to
    // choose, per agent (ADR-475).
    if terminal_look {
        let pop = items.pop();
        items.push(MenuAction::TerminalTheme);
        items.push(MenuAction::TerminalFont);
        items.push(MenuAction::Divider);
        // Ends the process, where *Close* only lets go of it.
        items.push(MenuAction::SessionTerminate);
        items.push(MenuAction::Divider);
        items.extend(pop);
    }
    if let Some(registered) = mcp {
        let pop = items.pop();
        items.push(if registered {
            MenuAction::UninstallMcp
        } else {
            MenuAction::InstallMcp
        });
        items.push(MenuAction::Divider);
        items.extend(pop);
    }
    if document {
        let pop = items.pop();
        items.push(MenuAction::RevealInExplore);
        items.push(MenuAction::Rename);
        items.push(MenuAction::MoveToTrash);
        items.push(MenuAction::NewSocialPost);
        items.push(MenuAction::Divider);
        if let Some(pop) = pop {
            items.push(pop);
        }
    }
    items
}

/// A library row's menu.
///
/// **Multi-select** leads with a non-selectable count header so every row can
/// stay short ("Move to…", not "Move 4 nodes to…"), then: move, open tabs,
/// open window, export, trash. Rename is single-only — one prompt cannot name
/// several documents.
pub fn node_items(count: usize) -> Vec<MenuAction> {
    if count > 1 {
        vec![
            MenuAction::SelectionHeader(count),
            MenuAction::Divider,
            MenuAction::MoveNodesTo(count),
            MenuAction::OpenInNewTab,
            MenuAction::OpenDocumentInNewWindow,
            MenuAction::ExportAs,
            MenuAction::Divider,
            MenuAction::MoveNodesToTrash(count),
        ]
    } else {
        vec![
            MenuAction::OpenInNewTab,
            MenuAction::OpenDocumentInNewWindow,
            MenuAction::Rename,
            MenuAction::Attributes,
            MenuAction::MoveTo,
            MenuAction::ExportAs,
            MenuAction::Divider,
            MenuAction::MoveToTrash,
            MenuAction::NewSocialPost,
        ]
    }
}

/// A workspace file is opaque content, not a page. Opening and copying remain
/// file-specific; trashing reuses the same reversible manifest move as a page.
/// What the Attributes row opens: the four attribute pickers R0 ships, A3/A4
/// add Due… and Priority… here.
pub fn attribute_items() -> Vec<MenuAction> {
    vec![
        MenuAction::AttributeTags,
        MenuAction::AttributeColor,
        MenuAction::AttributeDue,
        MenuAction::AttributePriority,
    ]
}

/// A workspace file's menu.
///
/// `note` is whether the file already carries one: `Some(true)` offers *Open
/// note*, `Some(false)` *Add note*, and `None` — a multi-selection, which no
/// single note belongs to — offers neither.
pub fn file_items(count: usize, note: Option<bool>) -> Vec<MenuAction> {
    let trash = if count > 1 {
        MenuAction::MoveFilesToTrash(count)
    } else {
        MenuAction::MoveToTrash
    };
    if count > 1 {
        // A multi-selection gets neither Rename nor a note row: one prompt
        // cannot name several files, and no single note belongs to them.
        vec![
            MenuAction::OpenFileInApp,
            MenuAction::SaveFileAs,
            MenuAction::Divider,
            trash,
        ]
    } else {
        let mut items = vec![MenuAction::OpenFileInApp, MenuAction::Rename];
        items.push(MenuAction::SaveFileAs);
        match note {
            Some(true) => items.push(MenuAction::OpenNote),
            Some(false) => items.push(MenuAction::AddNote),
            None => {}
        }
        items.push(MenuAction::Divider);
        items.push(trash);
        items
    }
}

/// Formats behind *Export*. Each format is backed by its own exporter crate,
/// registered in `koru_workspace_host::formats` — the menu is a projection of it.
pub fn export_items() -> Vec<MenuAction> {
    koru_workspace_host::formats::export_menu_formats()
        .into_iter()
        .map(|descriptor| MenuAction::ExportFormat(descriptor.id))
        .collect()
}

/// *Welcome* is always offered, first: it selects the open Welcome tab or
/// brings a closed one back.
pub fn new_tab_items() -> Vec<MenuAction> {
    let mut items = vec![
        MenuAction::OpenWelcome,
        MenuAction::Documents,
        MenuAction::Canvases,
        MenuAction::NewShell,
        MenuAction::NewAgent,
        MenuAction::NewAiChat,
        MenuAction::Delegate,
        MenuAction::Chats,
        MenuAction::Socials,
    ];
    items.push(MenuAction::Divider);
    items.push(MenuAction::ImportAs);
    items
}

/// Built-in shell profiles, the lower half of the *Shell* flyout.
pub fn shell_profile_items() -> Vec<MenuAction> {
    profile_items_of_kind(ProfileKind::Shell)
}

/// Built-in agent profiles, the lower half of the *Agents* flyout.
pub fn agent_profile_items() -> Vec<MenuAction> {
    profile_items_of_kind(ProfileKind::Agent)
}

/// Documents flyout: New, then up to ten recents and *Clear recents* under
/// them — offered only when there is something to clear.
pub fn document_plus_items() -> Vec<MenuAction> {
    let mut items = vec![MenuAction::NewDocument];
    PLUS_RECENTS.with(|cell| {
        let recents = cell.borrow();
        if recents.is_empty() {
            return;
        }
        items.push(MenuAction::Divider);
        for (id, _) in recents.iter().take(PLUS_RECENT_MAX) {
            items.push(MenuAction::OpenRecentDocument(*id));
        }
        items.push(MenuAction::Divider);
        items.push(MenuAction::ClearRecents);
    });
    items
}

/// Canvases flyout: New canvas, then up to ten recent canvases — the
/// Documents flyout's shape, over the other kind.
pub fn canvas_plus_items() -> Vec<MenuAction> {
    let mut items = vec![MenuAction::NewCanvas];
    PLUS_CANVASES.with(|cell| {
        let canvases = cell.borrow();
        if canvases.is_empty() {
            return;
        }
        items.push(MenuAction::Divider);
        for (id, _) in canvases.iter().take(PLUS_RECENT_MAX) {
            items.push(MenuAction::OpenRecentDocument(*id));
        }
    });
    items
}

/// Shell flyout: *Active ▸* when any shell is running, then the profile list.
pub fn shell_plus_items() -> Vec<MenuAction> {
    PLUS_RUNNING_SHELLS.with(|cell| {
        active_then_profiles(
            !cell.borrow().is_empty(),
            MenuAction::ActiveShells,
            shell_profile_items(),
        )
    })
}

/// Agents flyout: *Active ▸* when any agent is running, then the profile list.
pub fn agent_plus_items() -> Vec<MenuAction> {
    PLUS_RUNNING_AGENTS.with(|cell| {
        active_then_profiles(
            !cell.borrow().is_empty(),
            MenuAction::ActiveAgents,
            agent_profile_items(),
        )
    })
}

fn active_then_profiles(
    has_running: bool,
    active: MenuAction,
    profiles: Vec<MenuAction>,
) -> Vec<MenuAction> {
    let mut items = Vec::new();
    if has_running {
        items.push(active);
        if !profiles.is_empty() {
            items.push(MenuAction::Divider);
        }
    }
    items.extend(profiles);
    items
}

fn running_plus_items(
    running: &'static std::thread::LocalKey<RefCell<Vec<(SessionId, String)>>>,
) -> Vec<MenuAction> {
    running.with(|cell| {
        cell.borrow()
            .iter()
            .map(|(id, _)| MenuAction::OpenRunningSession(*id))
            .collect()
    })
}

/// Chats flyout: New, then up to ten recent chats.
pub fn chat_plus_items() -> Vec<MenuAction> {
    let mut items = vec![MenuAction::NewAiChat];
    PLUS_CHATS.with(|cell| {
        let chats = cell.borrow();
        if chats.is_empty() {
            return;
        }
        items.push(MenuAction::Divider);
        for (id, _) in chats.iter().take(PLUS_RECENT_MAX) {
            items.push(MenuAction::OpenRecentChat(*id));
        }
    });
    items
}

fn plus_recent_title(id: koru_core::DocumentId) -> String {
    let named = |cell: &RefCell<Vec<(koru_core::DocumentId, String)>>| {
        cell.borrow()
            .iter()
            .find(|(document, _)| *document == id)
            .map(|(_, title)| title.clone())
    };
    // A recent canvas is named from the Canvases snapshot, and an unnamed one
    // reads *Untitled* the way its tab and its row do.
    if let Some(title) = PLUS_CANVASES.with(named) {
        return if title.is_empty() {
            koru_i18n::t(crate::format::lang(), koru_i18n::Key::DocUntitled).to_owned()
        } else {
            title
        };
    }
    PLUS_RECENTS
        .with(named)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "Document".to_owned())
}

fn plus_running_title(id: SessionId) -> String {
    let from_shells = PLUS_RUNNING_SHELLS.with(|cell| {
        cell.borrow()
            .iter()
            .find(|(session, _)| *session == id)
            .map(|(_, name)| name.clone())
    });
    from_shells
        .or_else(|| {
            PLUS_RUNNING_AGENTS.with(|cell| {
                cell.borrow()
                    .iter()
                    .find(|(session, _)| *session == id)
                    .map(|(_, name)| name.clone())
            })
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Session".to_owned())
}

fn plus_chat_title(id: koru_ai_ui::ChatId) -> String {
    PLUS_CHATS.with(|cell| {
        cell.borrow()
            .iter()
            .find(|(chat, _)| *chat == id)
            .map(|(_, title)| title.clone())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "Chat".to_owned())
    })
}

/// The `+` menu's name for a built-in profile row.
///
/// **The shared projection, not the wire fallback.** Koru Terminal rendered
/// these rows through `koru_session_profiles::display_name`, which is where
/// "Codex CLI" reads "Codex" (owner, 2026-09-02); this menu calls the same
/// function, so the projection stays the one place a profile is named. The two non-brand words — the platform shell and an unnamed custom
/// profile — pass through the i18n rows Koru Terminal already owns; nothing
/// new lands in i18n.
///
/// Owned rather than borrowed because the catalogue is built per ask; the
/// alternative was a second copy of the mapping with `'static` names, which
/// is the drift this function exists to end.
fn terminal_profile_label(index: usize) -> String {
    let lang = crate::format::lang();
    built_in_profiles()
        .into_iter()
        .nth(index)
        .map(|profile| {
            koru_session_profiles::display_name(
                &profile,
                koru_i18n::t(lang, koru_i18n::Key::TerminalPlatformShell),
                koru_i18n::t(lang, koru_i18n::Key::TerminalCustomProfile),
            )
            .to_owned()
        })
        // An index the catalogue does not know answers a plain word rather
        // than panicking a draw; nothing constructs one today.
        .unwrap_or_else(|| "Terminal".to_owned())
}

/// The `+` menu's mark for a built-in profile row: the shared
/// `koru_draw::brand::for_profile` projection over the profile's frozen wire
/// key — the same mapping the session tab draws from, so the row you click
/// and the tab it opens cannot wear different faces. `None` for an index the
/// catalogue does not know, which keeps a plain row rather than panicking a
/// draw.
fn terminal_profile_mark(index: usize) -> Option<koru_draw::brand::ProfileMark> {
    let profile = built_in_profiles().into_iter().nth(index)?;
    match &profile.id {
        SessionProfileKey::BuiltIn(key) => koru_draw::brand::for_profile(key.as_str()),
        SessionProfileKey::Custom(_) => None,
    }
}

fn profile_items_of_kind(kind: ProfileKind) -> Vec<MenuAction> {
    built_in_profiles()
        .into_iter()
        .enumerate()
        .filter(|(_, profile)| {
            profile.kind == kind
                && profile.label != koru_session_profiles::ProfileLabel::CodexEmbedded
        })
        .map(|(index, _)| MenuAction::TerminalProfile(index))
        .collect()
}

/// The Agents tree's right-click menu — Koru Terminal's project menu, spoken
/// in this container's rows.
///
/// *Shell ▸* and *Agents ▸* are **the `+` menu's own rows**, each opening
/// the same flyout (*Active ▸* when anything of that kind is running, then
/// `built_in_profiles()` partitioned by kind), so one catalogue stands behind
/// every launch surface in the window.
/// A project row adds Rename… and Delete project; the bucket and empty space
/// offer only the launches — the bucket cannot be renamed or deleted
/// (ADR-264), and a menu should not list what cannot be chosen there.
pub fn agent_tree_items(project: bool) -> Vec<MenuAction> {
    let mut items = vec![MenuAction::NewShell, MenuAction::NewAgent];
    if project {
        items.push(MenuAction::Divider);
        items.push(MenuAction::Rename);
        items.push(MenuAction::Divider);
        items.push(MenuAction::DeleteProject);
    }
    items
}

/// **A run row's menu.** One row, because a run has one verb here: the
/// others a person might want — open it, see what it produced — are the
/// row's own click and the run's page. The same one row for a selection of
/// several: what changes is the label's count, not the shape of the menu.
pub fn agent_run_items() -> Vec<MenuAction> {
    vec![MenuAction::DeleteRun]
}

/// A chat row's menu in the Chats navigator: its one verb.
pub fn ai_chat_items() -> Vec<MenuAction> {
    vec![MenuAction::DeleteAiChat]
}

/// The first schedule-management increment adds rename. The following
/// vertical increments append edit, lifecycle, and retirement in their final
/// order.
pub fn agent_schedule_items(states: &[koru_delegate_schedule::ScheduleState]) -> Vec<MenuAction> {
    let mut items = Vec::new();
    if states.len() == 1 {
        items.extend([MenuAction::ScheduleRename, MenuAction::ScheduleEdit]);
    }
    if states
        .iter()
        .all(|state| matches!(state, koru_delegate_schedule::ScheduleState::Active { .. }))
    {
        items.push(MenuAction::SchedulePause);
    } else if states
        .iter()
        .all(|state| matches!(state, koru_delegate_schedule::ScheduleState::Paused { .. }))
    {
        items.push(MenuAction::ScheduleResume);
    }
    if !states.is_empty()
        && states
            .iter()
            .all(|state| !matches!(state, koru_delegate_schedule::ScheduleState::Retired))
    {
        items.push(MenuAction::Divider);
        items.push(MenuAction::ScheduleDelete);
    }
    items
}

/// A session row's menu (Q-E7 P8): Koru Terminal's four items in its order —
/// Open in new tab, Move to new window, Move to project ▸, Terminate. No
/// divider, matching Koru Terminal's `session_rows`; the project flyout hangs
/// off *Move to project ▸* through [`submenu`].
pub fn session_items() -> Vec<MenuAction> {
    vec![
        MenuAction::SessionOpenInNewTab,
        MenuAction::SessionMoveToNewWindow,
        MenuAction::SessionMoveToProject,
        MenuAction::SessionTerminate,
    ]
}

/// Formats behind *Import…* / *Import content…*. A projection of the shell's
/// format registry (`koru_workspace_host::formats`): registering an importer with a
/// `menu_name` lists it here, and nothing else has to change.
pub fn import_items() -> Vec<MenuAction> {
    koru_workspace_host::formats::import_menu_formats()
        .into_iter()
        .map(|descriptor| MenuAction::ImportFormat(descriptor.id))
        .collect()
}

/// A trashed item's menu: put it back, or really remove it.
///
/// **There is no "Restore to…".** Restore puts a document back where it came
/// from, and picking a destination needs a folder chooser over the library tree
/// that does not exist yet. A row that opened nothing would teach nothing; see
/// `docs/deferred/`.
pub fn trashed_items() -> Vec<MenuAction> {
    vec![
        MenuAction::Restore,
        MenuAction::Divider,
        MenuAction::DeletePermanently,
    ]
}

/// The Trash tab's own `…`.
pub fn trash_items() -> Vec<MenuAction> {
    vec![MenuAction::EmptyTrash]
}

/// A rail list's menu: the three things you can do to a list.
///
/// **Three rows, and no fourth.** SPEC §8 gives a list a name, a swatch and an
/// existence, and that is all there is to manage — reordering is the rail's own
/// gesture when it grows one, and *Open* is what the row already does on a plain
/// click. The divider is what keeps the red row from sitting flush against the
/// two that change a label.
pub fn list_items() -> Vec<MenuAction> {
    vec![
        MenuAction::RailOpenInNewTab,
        MenuAction::Divider,
        MenuAction::ListRename,
        MenuAction::ListColor,
        MenuAction::Divider,
        MenuAction::ListDelete,
    ]
}

/// A queue row's menu: one row, because a computed queue has nothing to rename,
/// recolour or delete — and *Open in new tab* is still a thing you can do to it.
pub fn rail_bucket_items() -> Vec<MenuAction> {
    vec![MenuAction::RailOpenInNewTab]
}

/// The swatches behind *Color*, one row per token in the shared accent set.
///
/// **Named rather than painted**, which is the one place this menu falls short
/// of the design's swatch strip: `koru_ui::Menu` draws a row's glyph in the
/// row's own ink, so a circle here would come out grey nine times. The colour
/// itself is a few pixels away on the row the menu opened over, and inventing a
/// tenth `Palette` field to tint a menu glyph is the trade the standing rule
/// refuses.
pub fn list_color_items() -> Vec<MenuAction> {
    koru_draw::theme::ACCENT_TOKENS
        .into_iter()
        .map(MenuAction::ListSwatch)
        .collect()
}

/// `"teal"` → `"Teal"`. ASCII, because the tokens are.
fn color_label(token: &str) -> String {
    let mut chars = token.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The workspace menu, built against the workspace as it is now.
///
/// Grouped by what the groups cost you: getting there, changing how it looks,
/// getting at its files, and the two that change whether it is in your life.
pub fn workspace_items(pinned: bool, archived: bool, deleted: bool) -> Vec<MenuAction> {
    let mut items = vec![
        MenuAction::Open,
        MenuAction::OpenInNewWindow,
        MenuAction::Divider,
        MenuAction::Rename,
        MenuAction::ChangeIconAndColor,
        if pinned {
            MenuAction::Unpin
        } else {
            MenuAction::Pin
        },
        MenuAction::MoveWorkspaceUp,
        MenuAction::MoveWorkspaceDown,
        MenuAction::Divider,
        MenuAction::MoveToProfile,
        MenuAction::CopyWorkspaceId,
        MenuAction::Export,
        MenuAction::Divider,
    ];
    if deleted {
        items.push(MenuAction::RestoreWorkspace);
    } else {
        items.push(if archived {
            MenuAction::Unarchive
        } else {
            MenuAction::Archive
        });
        items.push(MenuAction::Delete);
    }
    items
}

/// The caption overflow menu: window state first, application destinations
/// after it. Minimize, maximize and close remain direct caption controls.
///
/// The first row reads off the window's state rather than naming both halves —
/// the `Pin`/`Unpin` precedent, and for the reason this module's header gives: the
/// list is fixed when the menu opens, so a row cannot relabel itself between the
/// press and the release.
///
/// `recording` is whether *this* window is mid-lifecycle (countdown, starting,
/// recording, stopping): the row then reads **Stop recording**. Another window
/// holding the process's one recording is a different answer — the row still
/// reads Start, and [`MainView::item_is_enabled`](crate::mainview::MainView)
/// dims it.
pub fn window_items(pinned: bool, _maximized: bool, recording: bool) -> Vec<MenuAction> {
    let mut items = vec![
        if pinned {
            MenuAction::WindowUnpinTop
        } else {
            MenuAction::WindowAlwaysOnTop
        },
        MenuAction::HeaderSettings,
        MenuAction::Divider,
        MenuAction::HeaderZoom,
        MenuAction::Divider,
    ];
    if OVERFLOW_HIDDEN_ROWS_SHOWN {
        items.push(MenuAction::HeaderSupport);
    }
    items.push(MenuAction::HeaderFeedback);
    if OVERFLOW_HIDDEN_ROWS_SHOWN {
        items.extend([
            MenuAction::HeaderShortcuts,
            MenuAction::HeaderMobile,
            MenuAction::Divider,
            MenuAction::HeaderUpdates,
        ]);
        // The session recorder is an internal Windows tool
        // (the internal session-recorder design): a public build never shows the row.
        // `cfg!` rather than `#[cfg]` so the construction type-checks on every
        // platform and the variant is never dead code.
        if cfg!(windows) && koru_channel::ReleaseChannel::this_build_is_internal() {
            items.push(MenuAction::Divider);
            items.push(if recording {
                MenuAction::HeaderStopRecording
            } else {
                MenuAction::HeaderStartRecording
            });
        }
    }
    items
}

/// **Hidden for now: Feedback is the overflow's only destination.** Support,
/// Keyboard shortcuts, Get the mobile app, Updates and the session recorder's
/// row are left out of [`window_items`]. Each is still built and handled, so
/// turning this on puts every row back where it was. The recorder's chord (F9)
/// is unaffected.
pub(crate) const OVERFLOW_HIDDEN_ROWS_SHOWN: bool = false;

/// **The zoom row's controls, and what pressing each one does** — one list, so
/// the glyph and the action behind it cannot drift apart.
///
/// The readout in the middle answers `None`, which is what makes it a readout
/// rather than a fourth button.
///
/// **The one place a menu reads live state.** Everything else in this module is
/// fixed when the menu opens, for the reason the header gives — a row must not
/// relabel itself between the press and the release. A stepper is the exception
/// that proves it: the number *is* the feedback, and pressing `−` with nothing
/// changing on screen would be worse than any relabel. The panel still cannot
/// resize under the pointer, because [`koru_ui::Control`]'s widths are
/// constants rather than measurements — `100%` and `90%` occupy the same cell.
///
/// **The factor is a parameter, not a read of the global**, so every answer
/// here — which glyph, which end of the range, what the readout says — is a
/// pure function of one float that a test can name. `row_controls` is the one
/// line that reaches for `theme::zoom()`.
pub fn zoom_controls(zoom: f32) -> Vec<(Control, Option<MenuAction>)> {
    vec![
        (
            Control::Button {
                glyph: koru_draw::icons::MINUS,
                enabled: theme::can_zoom_out(zoom),
            },
            Some(MenuAction::ZoomOut),
        ),
        (Control::Readout(theme::zoom_percent(zoom)), None),
        (
            Control::Button {
                glyph: koru_draw::icons::PLUS,
                enabled: theme::can_zoom_in(zoom),
            },
            Some(MenuAction::ZoomIn),
        ),
        (
            Control::Button {
                glyph: koru_draw::icons::RESET,
                // Dim at 100%: there is nothing to come back from, and a live
                // control that does nothing teaches the wrong thing about the
                // two beside it.
                enabled: theme::clamp_zoom(zoom) != 1.0,
            },
            Some(MenuAction::ZoomActualSize),
        ),
    ]
}

/// What pressing control `control` of `row` does, or `None` for a readout and
/// for any row that has no controls at all.
pub fn control_action(row: MenuAction, control: usize) -> Option<MenuAction> {
    row_controls(row).into_iter().nth(control)?.1
}

/// **What a stepper control says when you rest on it**: what it does, and the
/// chord that does the same thing.
///
/// A glyph is a good button and a poor label — `−` says *which way* and nothing
/// about how far, and `↺` says *back* without saying back to what. The chip
/// answers both, and teaches the chord on the way, which is the whole reason a
/// menu carries chords at all.
///
/// **`Section::Tooltip`, not `Section::Menu`.** Whether a chord appears in a
/// hover chip and whether it appears in a row's quiet right-hand column are two
/// separate settings, and this is a chip. The words and the chord both come
/// from the registry through the action, so neither can drift from what the
/// keyboard actually does.
///
/// `None` for a readout, and for any row with no controls.
pub fn control_tooltip(row: MenuAction, control: usize) -> Option<(String, Option<String>)> {
    let action = control_action(row, control)?;
    Some((
        action.label().into_owned(),
        action
            .shortcut_id()
            .and_then(|id| koru_keys::show_hint(id, koru_keys::Section::Tooltip)),
    ))
}

/// The controls a row carries, paired with their actions. One row has any
/// today; the shape is a lookup rather than an `if` so a second stepper is a
/// new arm rather than a new mechanism.
fn row_controls(row: MenuAction) -> Vec<(Control, Option<MenuAction>)> {
    match row {
        MenuAction::HeaderZoom => zoom_controls(theme::zoom()),
        _ => Vec::new(),
    }
}

/// What *Arrange Window* opens.
///
/// The two halves, then the way back out of them.
#[cfg(windows)]
pub fn arrange_items() -> Vec<MenuAction> {
    vec![
        MenuAction::WindowSnapLeft,
        MenuAction::WindowSnapRight,
        MenuAction::Divider,
        MenuAction::WindowResetPosition,
    ]
}

/// Whether choosing this row opens a submenu rather than doing something.
///
/// One row today. It carries a `›` at its right-hand end instead of a chord,
/// which is the only visual difference between "this acts" and "there is more
/// behind this".
pub fn submenu(self_: MenuAction) -> Option<Vec<MenuAction>> {
    if self_ == MenuAction::TerminalTheme {
        return Some(vec![
            MenuAction::TerminalThemeIs(None),
            MenuAction::Divider,
            MenuAction::TerminalThemeIs(Some("light")),
            MenuAction::TerminalThemeIs(Some("dark")),
        ]);
    }
    if self_ == MenuAction::TerminalFont {
        let mut items = vec![MenuAction::TerminalFontIs(None), MenuAction::Divider];
        items.extend(
            koru_settings::FontFamily::ALL
                .iter()
                .copied()
                .filter(|family| family.is_mono())
                .map(|family| MenuAction::TerminalFontIs(Some(family.token()))),
        );
        items.push(MenuAction::Divider);
        items.push(MenuAction::TerminalFontMore);
        return Some(items);
    }
    if self_ == MenuAction::ImportAs {
        return Some(import_items());
    }
    if self_ == MenuAction::ExportAs {
        return Some(export_items());
    }
    if self_ == MenuAction::Attributes {
        return Some(attribute_items());
    }
    if self_ == MenuAction::ListColor {
        return Some(list_color_items());
    }
    if self_ == MenuAction::Documents {
        return Some(document_plus_items());
    }
    if self_ == MenuAction::Canvases {
        return Some(canvas_plus_items());
    }
    if self_ == MenuAction::NewShell {
        return Some(shell_plus_items());
    }
    if self_ == MenuAction::NewAgent {
        return Some(agent_plus_items());
    }
    if self_ == MenuAction::ActiveShells {
        let items = running_plus_items(&PLUS_RUNNING_SHELLS);
        return (!items.is_empty()).then_some(items);
    }
    if self_ == MenuAction::ActiveAgents {
        let items = running_plus_items(&PLUS_RUNNING_AGENTS);
        return (!items.is_empty()).then_some(items);
    }
    if self_ == MenuAction::Chats {
        return Some(chat_plus_items());
    }
    if self_ == MenuAction::SessionMoveToProject {
        return Some(session_project_items());
    }
    if self_ == MenuAction::MoveToProfile {
        let items = workspace_destination_items();
        return (!items.is_empty()).then_some(items);
    }
    #[cfg(windows)]
    if self_ == MenuAction::WindowArrange {
        return Some(arrange_items());
    }
    let _ = self_;
    None
}

/// `items` minus every row this build hides
/// ([`MenuAction::shown_in_product`]), with the dividers tidied so a hidden
/// group leaves no doubled, leading or trailing rule behind. Identity-based:
/// rows are filtered before the menu exists, so every index the menu hands
/// out afterwards names a row it is showing.
pub fn shown(items: Vec<MenuAction>) -> Vec<MenuAction> {
    // Untouched when nothing is hidden, so a menu built with every surface on
    // is exactly the menu its builder wrote.
    if items.iter().all(|item| item.shown_in_product()) {
        return items;
    }
    let mut out: Vec<MenuAction> = Vec::with_capacity(items.len());
    for item in items {
        if !item.shown_in_product() {
            continue;
        }
        if item == MenuAction::Divider && out.last().is_none_or(|last| *last == MenuAction::Divider)
        {
            continue;
        }
        out.push(item);
    }
    while out.last() == Some(&MenuAction::Divider) {
        out.pop();
    }
    out
}

/// The menu, as the container holds it: the subject it acts on, plus the
/// shared widget that places and paints the rows.
///
/// **`Deref` to the widget is deliberate.** Every call site keeps reading
/// `menu.hover`, `menu.items()`, `menu.panel(..)` and `menu.draw(..)` exactly
/// as it did when the geometry lived here — only the subject is this
/// wrapper's own.
#[derive(Clone)]
pub struct ContextMenu {
    /// What the menu acts on — whatever was under the cursor when it opened.
    pub subject: MenuSubject,
    /// **This opening of this menu**, numbered once and never reused.
    ///
    /// The rows are fixed when a menu opens and the subject with them, so two
    /// openings of "the tab menu" are two different lists over two different
    /// tabs. A client that queued a request against the first must not have it
    /// land on the second — §5's *never route a stale action to a new menu
    /// that reused an ID* — and an identity built from the subject, or from a
    /// row's position alone, would do precisely that. Minted here, in the one
    /// place every menu is constructed, the way `Tab::id` is.
    ///
    /// **A block of numbers, not one.** `instance + 1 + i` is row `i`'s own
    /// id, reserved with the menu, so no two live rows anywhere can share one.
    /// Rows never move within an opening — they are fixed when it opens — so
    /// an offset inside the block is a stable identity for exactly as long as
    /// the identity is allowed to exist, which is what *transient surfaces
    /// have an instance lifetime* means. A row is therefore addressed by
    /// reading the tree rather than by a locator built in advance, the same as
    /// any menu on any platform.
    ///
    /// Accessibility and optional UI inspection share it; nothing persists it.
    pub instance: u64,
    inner: koru_ui::Menu<MenuAction>,
}

/// Reserve `rows + 1` ids for one opening of a menu: the menu, then a row each.
///
/// Monotonic and never reused: a number that could come round again is the bug
/// this exists to prevent. The counter only moves forward, and a process would
/// have to open menus for longer than the universe has existed to wrap it.
fn mint_menu_instance(rows: usize) -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(rows as u64 + 1, Ordering::Relaxed)
}

impl std::ops::Deref for ContextMenu {
    type Target = koru_ui::Menu<MenuAction>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl std::ops::DerefMut for ContextMenu {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl ContextMenu {
    /// Reuse this opening's native projection IDs in read-only inspection.
    /// Flyout ownership comes from the host's actual parent row, never from
    /// proximity of the independently clamped panels.
    pub fn draw_inspected(
        &self,
        scene: &mut vello::Scene,
        root: vello::kurbo::Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        enabled: &[bool],
        owner_row: Option<u64>,
    ) {
        let row_occurrence = |i| Some(self.row_instance(i));
        self.inner.draw_with_inspection(
            scene,
            root,
            engine,
            viewport,
            enabled,
            koru_ui::menu::KoruMenuInspection {
                occurrence: Some(self.instance),
                row_occurrence: Some(&row_occurrence),
                owner_row,
                // The existing host does not retain a trigger occurrence for
                // every opening. A subject or anchor is not that identity.
                trigger: None,
            },
        );
    }

    pub fn new(
        engine: &mut TextEngine,
        subject: MenuSubject,
        anchor: (f32, f32),
        items: Vec<MenuAction>,
    ) -> Self {
        Self::aligned(engine, subject, anchor, items, Anchor::Cursor)
    }

    /// ...with the corner said out loud. What a `…` button uses.
    pub fn aligned(
        engine: &mut TextEngine,
        subject: MenuSubject,
        anchor: (f32, f32),
        items: Vec<MenuAction>,
        align: Anchor,
    ) -> Self {
        let items = shown(items);
        Self {
            subject,
            instance: mint_menu_instance(items.len()),
            inner: koru_ui::Menu::aligned(engine, anchor, items, align),
        }
    }

    pub fn for_tab(
        engine: &mut TextEngine,
        tab: usize,
        document: bool,
        pinnable: bool,
        pinned: bool,
        show_title: bool,
        mcp: Option<bool>,
        terminal_look: bool,
        anchor: (f32, f32),
    ) -> Self {
        Self::new(
            engine,
            MenuSubject::Tab(tab),
            anchor,
            tab_items_for(document, pinnable, pinned, show_title, mcp, terminal_look),
        )
    }

    /// A flyout beside its parent row — see [`koru_ui::Menu::as_submenu`].
    pub fn as_submenu(
        engine: &mut TextEngine,
        subject: MenuSubject,
        parent: Rect,
        items: Vec<MenuAction>,
        viewport: (f32, f32),
    ) -> Self {
        let items = shown(items);
        Self {
            subject,
            instance: mint_menu_instance(items.len()),
            inner: koru_ui::Menu::as_submenu(engine, parent, items, viewport),
        }
    }

    /// The same menu at a width somebody else measured — see
    /// [`koru_ui::Menu::at_width`].
    #[cfg(test)]
    pub fn at_width(
        subject: MenuSubject,
        anchor: (f32, f32),
        items: Vec<MenuAction>,
        align: Anchor,
        width: f32,
    ) -> Self {
        let items = shown(items);
        Self {
            subject,
            instance: mint_menu_instance(items.len()),
            inner: koru_ui::Menu::at_width(anchor, items, align, width),
        }
    }

    /// Where this menu was anchored. Tests only.
    #[cfg(test)]
    pub fn anchor_for_test(&self) -> (f32, f32) {
        self.inner.anchor_for_test()
    }

    /// The accessibility id of row `i` of this opening. See
    /// [`instance`](ContextMenu::instance).
    pub fn row_instance(&self, i: usize) -> u64 {
        self.instance + 1 + i as u64
    }

    /// …and the row an id names, if it is one of this opening's.
    pub fn row_of_instance(&self, id: u64) -> Option<usize> {
        let offset = id.checked_sub(self.instance + 1)?;
        let i = usize::try_from(offset).ok()?;
        (i < self.items().len()).then_some(i)
    }
}

#[cfg(test)]
mod tests;
