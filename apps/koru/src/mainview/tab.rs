// SPDX-License-Identifier: MPL-2.0

//! One tab, and what it shows.
//!
//! ## Why this is a different enum from [`Leaf`]
//!
//! A tab may show a welcome screen, which has no rows, no `Body` and no
//! `ViewState`. Putting that arm into `Leaf` would force `Editing::parts` to be
//! `unreachable!()` for it — a panic waiting for someone to route a keystroke
//! the wrong way. Two levels instead:
//!
//! - `TabView` implements **only** [`View`]. That is what lets a tab be
//!   something other than a list of paragraphs, and what keeps the container
//!   free of the `Editing`/`View` name collision entirely.
//! - [`Leaf`] implements both, and confines that collision to one file where it
//!   is already solved and tested.
//!
//! Adding a canvas or a calendar later is a new arm *here*, not there.

use koru_core::schema::WorkspaceDoc;
use koru_core::{NodeId, WorkspaceId};
use koru_editor::{Editing, OpenWhere};
use koru_workspace::Workspace;
use koru_workspace_host::View;

use crate::ai_chat_view::AiChatView;
use crate::leaf::Leaf;
use crate::welcome::WelcomeView;

// Both arms are large — a `Leaf` carries a `Body` and a `ViewState`, and even a
// welcome screen carries a `TextEngine` — so the enum is sized to the bigger of
// them. Boxing would save bytes on a `Vec` that holds a handful of tabs, at the
// cost of an indirection on every key, mouse and draw dispatch. Not worth it.
#[allow(clippy::large_enum_variant)]
pub enum TabView {
    Welcome(WelcomeView),
    /// Home's Today page (workspace foundation, rung 5); a singleton.
    Home(crate::panes::home::HomePage),
    /// The Trash. A fourth arm rather than something inside [`Leaf`], for the
    /// reason the module docs give: it has no rows, so `Editing::parts` would
    /// have to be `unreachable!()`.
    Trash(crate::trash::TrashView),
    /// One bucket or one list of the workspace's tasks. A fifth arm for the
    /// Trash arm's reason: it has no `Body` and no rows, so `Editing::parts`
    /// could only be `unreachable!()` down in [`Leaf`].
    Tasks(crate::tasks_view::TasksView),
    /// The All Reminders page — the bell popover's management surface hosted
    /// as an ordinary tab (`mainview/reminders_view.rs`). The rail's Reminders
    /// row re-points a Tasks tab to this arm, and a bucket press swaps it
    /// back: one tab, its content trading between the two views.
    Reminders(super::reminders_view::RemindersView),
    /// The Manage tab: Profiles, New profile, linked devices and Koru Cloud
    /// behind one nav. It replaced Manage Profiles, the Workspaces tab and the
    /// Link dialog's Linked devices tab (ADR-449-onemanagetab).
    Manage(crate::manage::ManageView),
    /// Send feedback — a form, never a document.
    Feedback(crate::feedback::FeedbackView),
    /// The Settings tab — the shared `koru-settings-ui` surface, hosted.
    ///
    /// A fifth arm rather than a flag on `Welcome`, which is what it was until
    /// SP-7: Welcome and Settings shared one struct and one set of rects, and
    /// the only thing keeping blank space on Welcome from mutating a hidden
    /// preference was an `if` in every hit test. Two arms cannot make that
    /// mistake.
    Settings(crate::settings_view::SettingsView),
    /// A version comparison — a read-only page over a computed diff. Another
    /// arm for the module docs' reason: it has no rows and no `Body`, so
    /// `Editing::parts` could only panic below.
    Diff(crate::diff::DiffView),
    /// Koru AI, hosted as a tab rather than in the right-hand pane.
    AiChat(AiChatView),
    /// Workspace social posts and scheduled publications.
    Socials(crate::socials_view::SocialsView),
    /// One delegated run: Overview, Activity and Result, and the session
    /// handover a person takes it through (P1.16). Another arm for the
    /// module docs' reason — it has no rows and no `Body`, so
    /// `Editing::parts` could only panic down in [`Leaf`].
    Run(crate::run_view::RunView),
    /// **One result of a run, as the document it is** (ADR-368). Its own
    /// arm rather than a mode of [`Run`](Self::Run): a result is not the
    /// run any more, it outlives the run's tab, and it is found in the
    /// strip by its own name. It has no rows and no `Body`, so
    /// `Editing::parts` could only panic down in [`Leaf`].
    Result(crate::result_view::ResultView),
    /// A running shell or agent session — an ordinary tab, mixing freely with
    /// document tabs (TERMINATAB Q-E4). The view wraps the extracted
    /// `koru_agents_ui::SessionSurface`; the container owns the launch, the
    /// controller lease and the backend events (`mainview/session_backend.rs`).
    Session(crate::session_tab::SessionView),
    /// **A canvas** — an ordinary node-tree document under the manifest kind
    /// `canvas`, drawn spatially rather than as rows (CANVAS P1.3). Its own arm
    /// for the module docs' reason: it is not a list of paragraphs, so
    /// `Editing::parts` could only panic down in [`Leaf`]. The tab persists as
    /// `TabRef::Document` like a page's; `MainView::document_view` is the one
    /// place that decides a document tab is this arm.
    Canvas(crate::canvas_view::CanvasView),
    Leaf(Leaf),
}

impl From<Leaf> for TabView {
    fn from(leaf: Leaf) -> Self {
        Self::Leaf(leaf)
    }
}

impl From<crate::canvas_view::CanvasView> for TabView {
    fn from(canvas: crate::canvas_view::CanvasView) -> Self {
        Self::Canvas(canvas)
    }
}

impl TabView {
    /// Whether a drag this view owns is in flight.
    ///
    /// The container stops forwarding pointer moves once the pointer leaves the
    /// content rect — except during a drag. A text selection, a row reorder, a
    /// scrollbar thumb and an image resize all deliberately follow the pointer
    /// outside the band, and cutting them off at the region edge would be a
    /// regression against behaviour the editor already has.
    pub fn is_dragging(&self) -> bool {
        match self {
            // Nothing on the welcome screen is draggable, which is exactly the
            // sort of thing a typed seam should be able to say without a panic.
            Self::Welcome(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Tasks(_)
            | Self::Reminders(_)
            | Self::Trash(_)
            | Self::AiChat(_)
            | Self::Session(_)
            | Self::Run(_)
            | Self::Feedback(_) => false,
            // A result opens as a document, and a selection drawn in one
            // follows the pointer out of the band exactly as it does in the
            // library's own pages (ADR-372).
            Self::Result(r) => r.is_dragging(),
            Self::Socials(view) => view.is_dragging(),
            Self::Manage(view) => view.is_dragging(),
            Self::Diff(d) => d.is_dragging(),
            // A pan, a card move, a resize, an arrow being drawn, or a text
            // drag in the name prompt or the dialog.
            Self::Canvas(c) => c.is_dragging(),
            Self::Leaf(l) => {
                let v = l.view();
                v.dragging
                    || v.drag_row.is_some()
                    || v.drag_field
                    || v.scroll_drag.is_some()
                    || v.image_resize.is_some()
            }
        }
    }

    /// Whether this tab can be pinned even though it names no document.
    pub fn is_pinnable(&self) -> bool {
        matches!(self, Self::AiChat(_))
    }

    /// Whether this tab can leave the window for one of its own — a document
    /// can, torn off the strip or from the tab menu.
    ///
    /// **Not a canvas, yet.** A popped-out document is the shell's document
    /// window, which is the page editor: a canvas there would show as a title
    /// over nothing, and a line typed into it would become a card with no
    /// place. It pops out once the canvas surface has a window of its own.
    pub fn pops_out(&self) -> bool {
        self.root().is_some() && !matches!(self, Self::Canvas(_))
    }

    /// Drop any hover highlight, because the pointer has left this view.
    ///
    /// Needed because `hover_at` resolves a row from `y` alone and ignores `x`
    /// — so a pointer parked over the sidebar would otherwise leave a row lit
    /// underneath it.
    pub fn clear_hover(&mut self) {
        if let Self::Leaf(l) = self {
            l.view_mut().hover = None;
        }
    }

    /// Take in a document that changed **without laying out**.
    ///
    /// Take note of what changed **without doing anything about it**, for a tab
    /// that is not on screen.
    ///
    /// The drain is not optional: the Loro subscription queues into this tab's
    /// tracker whether anyone reads it or not, so a tab that never drained would
    /// accumulate every change ever made. What is deferred is the work.
    ///
    /// `reflow` was already withheld from a background tab. **Withholding the
    /// rebuild is the other half, and it is the half that mattered.** A tree
    /// change anywhere set `structure_dirty`, and every open tab answered it by
    /// re-reading its own whole document — so a peer pressing Enter cost one
    /// document walk *per open tab*, and coming back from offline delivered
    /// enough of them to wedge the window. A tab catches up in one go when it is
    /// next shown. See the performance rules in AGENTS.md.
    pub fn adopt_cheap(&mut self, ws: &WorkspaceDoc) {
        match self {
            Self::Welcome(w) => w.adopt(ws),
            Self::Manage(_) => {}
            Self::Trash(t) => koru_workspace_host::View::adopt(t, ws),
            Self::Tasks(t) => koru_workspace_host::View::adopt(t, ws),
            // The page's rows are fed presentation data, not a document:
            // nothing under it can have changed.
            Self::Reminders(_) => {}
            // Settings names no document, so nothing under it can have changed.
            Self::Settings(_) | Self::Feedback(_) | Self::Home(_) => {}
            // A diff is over two retained snapshots; the live document
            // changing does not change what was compared.
            Self::Diff(_) => {}
            // The AI chat surface reads its own state through the background
            // reader; live document changes do not alter what it shows.
            Self::AiChat(_) => {}
            Self::Socials(_) => {}
            // A session shows a process, not the document; nothing under it
            // can have changed.
            Self::Session(_) => {}
            // A run view shows a run, which is its own document read
            // through the gateway; the open workspace document is not it.
            // A result is an artifact version, which is immutable.
            Self::Run(_) | Self::Result(_) => {}
            // The Leaf arm's guard, for the Leaf arm's reason: a store swap
            // hands a background tab a stranger document, and a canvas must
            // not take in changes to a tree it is not rooted in.
            Self::Canvas(c) => {
                if ws.read(c.root()).is_ok() {
                    koru_workspace_host::View::adopt(c, ws);
                }
            }
            Self::Leaf(l) => {
                // A Store swap loads a different document. Draining that
                // stranger into this body marks structural and the next
                // catch_up wipes the cache — the empty pin.
                if let Some(root) = l.body().root
                    && ws.read(root).is_err()
                {
                    return;
                }
                l.body_mut().defer_pending(ws);
            }
        }
    }

    /// Settle whatever [`adopt_cheap`] deferred, for the tab about to be drawn.
    /// A no-op unless something actually changed.
    ///
    /// **A leaf settles through [`Editing::catch_up`], which lays it out.** This
    /// is the container's half of the editor's adopt path — the standalone
    /// document window reaches the same call from `Editing::adopt` — and it is
    /// more than a row re-read: an open picker is anchored to a Loro range, and
    /// a peer's edit can move it or delete the node under it. Going straight to
    /// `Body::catch_up` was the Q-B4 bug, where a document in a tab kept a
    /// picker hanging from a node that no longer existed. Because the editor
    /// lays out inside that call, a caller here does **not** reflow again
    /// afterwards; see `MainView::catch_up_active`
    /// ([ADR-343-overlaycatchup](../../../docs/decisions/README.md)).
    ///
    /// [`adopt_cheap`]: Self::adopt_cheap
    /// [`Editing::catch_up`]: koru_editor::Editing::catch_up
    pub fn catch_up(&mut self, ws: &WorkspaceDoc) -> bool {
        match self {
            Self::Welcome(_)
            | Self::Manage(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Diff(_)
            | Self::AiChat(_)
            | Self::Socials(_)
            | Self::Session(_)
            | Self::Run(_)
            | Self::Result(_)
            | Self::Feedback(_) => false,
            // A canvas reads its document again when its version has moved —
            // one comparison when nothing has.
            Self::Canvas(c) => c.catch_up(ws),
            // The one arm that is not `false`: the Trash reads its listing here,
            // which is what makes a background Trash tab cost a bool.
            Self::Trash(t) => t.catch_up(ws),
            // …and the second: the Tasks tab folds its answered page here, so
            // a background one costs a bool exactly as the Trash tab does.
            Self::Tasks(t) => t.catch_up(ws),
            // …and the third: the Reminders page is fed on the maintain pass
            // while it is in front, so a background one costs the same bool.
            Self::Reminders(_) => false,
            Self::Leaf(l) => {
                // The guard stays here rather than moving into the editor: a
                // single-view window has no cheaper question to ask than doing
                // the work, but the container asks it every frame.
                if !l.body().is_stale() {
                    return false;
                }
                l.catch_up(ws);
                true
            }
        }
    }

    /// The node this tab is rooted at, if any. `None` for the welcome screen and
    /// for the realm-wide outline, neither of which is a node.
    /// The document was replaced. Rebuild rather than patch — see
    /// [`koru_workspace_host::View::reseat`].
    pub fn reseat(&mut self, ws: &WorkspaceDoc) {
        match self {
            Self::Welcome(w) => koru_workspace_host::View::reseat(w, ws),
            Self::Manage(w) => koru_workspace_host::View::reseat(w, ws),
            Self::Trash(t) => koru_workspace_host::View::reseat(t, ws),
            Self::Tasks(t) => koru_workspace_host::View::reseat(t, ws),
            Self::Reminders(r) => koru_workspace_host::View::reseat(r, ws),
            Self::Settings(s) => koru_workspace_host::View::reseat(s, ws),
            Self::Home(h) => koru_workspace_host::View::reseat(h, ws),
            Self::Feedback(f) => koru_workspace_host::View::reseat(f, ws),
            Self::Diff(d) => koru_workspace_host::View::reseat(d, ws),
            Self::AiChat(a) => koru_workspace_host::View::reseat(a, ws),
            Self::Socials(a) => koru_workspace_host::View::reseat(a, ws),
            Self::Session(s) => koru_workspace_host::View::reseat(s, ws),
            Self::Run(r) => koru_workspace_host::View::reseat(r, ws),
            Self::Result(r) => koru_workspace_host::View::reseat(r, ws),
            Self::Canvas(c) => koru_workspace_host::View::reseat(c, ws),
            Self::Leaf(l) => koru_workspace_host::View::reseat(l, ws),
        }
    }

    /// The glyph the tab strip draws before this tab's label.
    ///
    /// Only for tabs that are a **place** rather than a document. A document's
    /// own page icon belongs to its row in the explore column; repeating it on
    /// the tab would make the strip a second, competing index.
    pub fn icon(&self) -> Option<char> {
        match self {
            Self::Trash(_) => Some(koru_draw::icons::TRASH),
            Self::Home(_) => Some(koru_draw::icons::HOUSE),
            Self::Tasks(_) => Some(koru_draw::icons::LIST_CHECKS),
            Self::Reminders(_) => Some(koru_draw::icons::BELL),
            Self::Feedback(_) => Some(koru_draw::icons::USER_SOUND),
            Self::Diff(_) => Some(crate::diff::tab_icon()),
            Self::AiChat(_) => Some(koru_draw::icons::SPARKLE),
            Self::Socials(_) => Some(koru_draw::icons::MEGAPHONE),
            // **A canvas wears its kind**, the way a note tab wears the file
            // glyph: the label names it, and the glyph says it is not a page.
            // Chosen by the arm, and the arm is chosen by the manifest kind
            // (`MainView::document_view`), so this is the kind's glyph.
            Self::Canvas(_) => Some(koru_draw::icons::CANVAS),
            // A session tab wears the mark of the program it runs (Q-E6
            // P3/P4). A glyph mark (the *System* shell's OS logo) comes back
            // here; a vector brand mark travels through [`brand`] instead,
            // and TERMINAL stays the fallback for a custom or unknown
            // profile.
            Self::Session(s) => Some(match session_mark(s) {
                Some(koru_draw::brand::ProfileMark::Glyph(glyph)) => glyph,
                _ => koru_draw::icons::TERMINAL,
            }),
            _ => None,
        }
    }

    /// The vector product mark a session tab wears when its launch profile is
    /// a branded product — Claude, Codex, PowerShell and the rest. Beside
    /// [`icon`](Self::icon) rather than inside it because a brand mark is
    /// path data the strip draws through `TabIcon::Brand`, not a `char` the
    /// icon font can shape. The strip asks for this first and falls back to
    /// the glyph.
    pub fn brand(&self) -> Option<&'static koru_draw::brand::BrandIcon> {
        match self {
            Self::Session(s) => match session_mark(s) {
                Some(koru_draw::brand::ProfileMark::Brand(brand)) => Some(brand),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn root(&self) -> Option<NodeId> {
        match self {
            Self::Welcome(_)
            | Self::Manage(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Trash(_)
            | Self::Tasks(_)
            | Self::Reminders(_)
            | Self::Diff(_)
            | Self::AiChat(_)
            | Self::Socials(_)
            | Self::Session(_)
            | Self::Run(_)
            | Self::Result(_)
            | Self::Feedback(_) => None,
            Self::Canvas(c) => Some(c.root()),
            Self::Leaf(l) => View::document_root(l),
        }
    }

    /// Focus `id` and scroll it into view. `false` if it is not a row here.
    ///
    /// What the explore column needs when you click something *inside* a
    /// document: the tab opens rooted at the document, and this lands on the
    /// image or line you actually pointed at.
    pub fn reveal(&mut self, id: NodeId) -> bool {
        match self {
            // Nothing on the welcome screen is a node, so there is nothing to
            // reveal — the typed seam answering for itself again.
            Self::Welcome(_)
            | Self::Manage(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Trash(_)
            | Self::Tasks(_)
            | Self::Reminders(_)
            | Self::Diff(_)
            | Self::AiChat(_)
            | Self::Socials(_)
            | Self::Session(_)
            | Self::Run(_)
            | Self::Result(_)
            | Self::Feedback(_) => false,
            // A canvas lands on the object: centred and selected.
            Self::Canvas(c) => c.reveal(id),
            Self::Leaf(l) => l.reveal(id),
        }
    }

    /// Focus a search match in the middle of its document viewport.
    pub fn reveal_centered(&mut self, id: NodeId) -> bool {
        match self {
            Self::Welcome(_)
            | Self::Manage(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Trash(_)
            | Self::Tasks(_)
            | Self::Reminders(_)
            | Self::Diff(_)
            | Self::AiChat(_)
            | Self::Socials(_)
            | Self::Session(_)
            | Self::Run(_)
            | Self::Result(_)
            | Self::Feedback(_) => false,
            // A canvas lands on the object: centred and selected.
            Self::Canvas(c) => c.reveal(id),
            Self::Leaf(l) => l.reveal_centered(id),
        }
    }

    pub fn reveal_search_match(&mut self, id: NodeId, match_text: &str) -> bool {
        match self {
            Self::Welcome(_)
            | Self::Manage(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Trash(_)
            | Self::Tasks(_)
            | Self::Reminders(_)
            | Self::Diff(_)
            | Self::AiChat(_)
            | Self::Socials(_)
            | Self::Session(_)
            | Self::Run(_)
            | Self::Result(_)
            | Self::Feedback(_) => false,
            // A canvas lands on the object: centred and selected.
            Self::Canvas(c) => c.reveal(id),
            Self::Leaf(l) => l.reveal_search_match(id, match_text),
        }
    }

    /// Put a revealed image's display name into edit mode.
    pub fn start_image_rename(&mut self, ws: &WorkspaceDoc, id: NodeId) -> bool {
        match self {
            Self::Welcome(_)
            | Self::Manage(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Trash(_)
            | Self::Tasks(_)
            | Self::Reminders(_)
            | Self::Diff(_)
            | Self::AiChat(_)
            | Self::Socials(_)
            | Self::Session(_)
            | Self::Run(_)
            | Self::Result(_)
            | Self::Canvas(_)
            | Self::Feedback(_) => false,
            Self::Leaf(l) => l.start_image_rename(ws, id),
        }
    }

    /// Put this tab's document name into the rename prompt. `false` for a
    /// welcome screen, which is not a document and has no name.
    pub fn start_rename(&mut self, ws: &WorkspaceDoc) -> bool {
        match self {
            Self::Welcome(_)
            | Self::Manage(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Trash(_)
            | Self::Tasks(_)
            | Self::Reminders(_)
            | Self::Diff(_)
            | Self::AiChat(_)
            | Self::Socials(_)
            | Self::Session(_)
            | Self::Run(_)
            | Self::Result(_)
            | Self::Feedback(_) => false,
            // The canvas answers with the same name prompt, writing its root's
            // text — so F2, the tab's Rename… and Explore's all rename it.
            Self::Canvas(c) => c.start_rename(ws),
            Self::Leaf(l) => l.start_rename(ws),
        }
    }

    /// Whether a prompt is open over this view — a rename, a `/` menu, `Ctrl+K`.
    ///
    /// **A prompt owns the keyboard.** The explore tree may hold the arrows and
    /// Enter, but not over a panel that is asking a question: Enter belongs to
    /// whatever is waiting for an answer. Without this, double-clicking an image
    /// in the tree opened its rename prompt *and left the tree focused*, so the
    /// Enter that should have committed the name opened the row instead and the
    /// rename was silently dropped.
    pub fn is_prompting(&self) -> bool {
        match self {
            Self::Welcome(_)
            | Self::Manage(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Trash(_)
            | Self::Tasks(_)
            | Self::Reminders(_)
            | Self::Diff(_)
            | Self::AiChat(_)
            | Self::Socials(_)
            | Self::Session(_)
            | Self::Run(_)
            | Self::Result(_)
            | Self::Feedback(_) => false,
            // The name prompt, the text dialog or the agent menu.
            Self::Canvas(c) => c.is_prompting(),
            Self::Leaf(l) => l.view().overlay.is_open(),
        }
    }

    /// Whether Enter would currently commit a **name** prompt of either kind —
    /// a document's title or an image's display name.
    ///
    /// Both write a name the *container* has cached: the tab's label and the
    /// explore row. Neither of those is the editor's to update, and a local edit
    /// does not come back round through `adopt` the way a peer's does — so the
    /// container has to notice the commit as it routes the key. Without it a
    /// rename showed in the document immediately and in its own tab minutes
    /// later, whenever something else happened to invalidate the cache.
    pub fn is_renaming(&self) -> bool {
        use koru_editor::trigger::Anchor;
        match self {
            Self::Welcome(_)
            | Self::Manage(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Trash(_)
            | Self::Tasks(_)
            | Self::Reminders(_)
            | Self::Diff(_)
            | Self::AiChat(_)
            | Self::Socials(_)
            | Self::Session(_)
            | Self::Run(_)
            | Self::Result(_)
            | Self::Feedback(_) => false,
            Self::Canvas(c) => c.is_renaming(),
            Self::Leaf(l) => l.view().overlay.trigger().is_some_and(|trigger| {
                matches!(trigger.anchor, Anchor::Rename(_) | Anchor::RenameImage(_))
            }),
        }
    }

    /// The document whose title the caret is in, if it is in one.
    ///
    /// Distinct from [`is_renaming`](Self::is_renaming), which asks about the
    /// *prompt*: a title is edited in place now, so there is no overlay to look
    /// at and the question is about the body's focus instead.
    pub fn editing_title(&self) -> Option<NodeId> {
        match self {
            Self::Welcome(_)
            | Self::Manage(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Trash(_)
            | Self::Tasks(_)
            | Self::Reminders(_)
            | Self::Diff(_)
            | Self::AiChat(_)
            | Self::Socials(_)
            | Self::Session(_)
            | Self::Run(_)
            | Self::Result(_)
            | Self::Canvas(_)
            | Self::Feedback(_) => None,
            Self::Leaf(l) => l.editing_title().then(|| View::document_root(l)).flatten(),
        }
    }

    /// Whether the active overlay will write replicated document-icon
    /// metadata when an emoji is chosen.
    pub fn is_setting_page_icon(&self) -> bool {
        use koru_editor::trigger::Anchor;
        match self {
            Self::Welcome(_)
            | Self::Manage(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Trash(_)
            | Self::Tasks(_)
            | Self::Reminders(_)
            | Self::Diff(_)
            | Self::AiChat(_)
            | Self::Socials(_)
            | Self::Session(_)
            | Self::Run(_)
            | Self::Result(_)
            | Self::Canvas(_)
            | Self::Feedback(_) => false,
            Self::Leaf(l) => l
                .view()
                .overlay
                .trigger()
                .is_some_and(|trigger| matches!(trigger.anchor, Anchor::PageIcon(_))),
        }
    }

    /// A node this tab asked to open, and where. Cleared by the asking.
    ///
    /// The container asks for the disposition because it is the side that owns a
    /// tab set and can honour it; a welcome tab has no pills, so it can only ask
    /// for the plain thing.
    pub fn take_wants_open(&mut self) -> Option<(NodeId, OpenWhere)> {
        match self {
            // The welcome opens a *document* by stable id now, which the
            // container routes through the store — a different channel from the
            // editor's node opens, so nothing comes through here.
            Self::Welcome(_) => None,
            // Neither Manage nor the Trash opens a node through this
            // channel: their asks go out as their own `Request` types.
            Self::Manage(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Trash(_)
            | Self::Diff(_)
            | Self::Tasks(_)
            | Self::Reminders(_)
            | Self::AiChat(_)
            | Self::Socials(_)
            | Self::Session(_)
            | Self::Run(_)
            | Self::Result(_)
            | Self::Canvas(_)
            | Self::Feedback(_) => None,
            Self::Leaf(l) => l.take_wants_open_where(),
        }
    }

    /// Where this tab is scrolled to, so a session can put it back.
    pub fn scroll(&self) -> f32 {
        match self {
            Self::Welcome(_)
            | Self::Manage(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Trash(_)
            | Self::Tasks(_)
            | Self::Reminders(_)
            | Self::Diff(_)
            | Self::AiChat(_)
            | Self::Socials(_)
            | Self::Session(_)
            | Self::Run(_)
            | Self::Result(_)
            | Self::Canvas(_)
            | Self::Feedback(_) => 0.0,
            Self::Leaf(l) => l.view().scroll,
        }
    }

    pub fn set_scroll(&mut self, scroll: f32) {
        if let Self::Leaf(l) = self {
            l.view_mut().scroll = scroll;
        }
    }

    /// Put the caret (and a text selection, if any) back after `reveal`.
    ///
    /// `reveal` always places at 0, so history has to overwrite that. Queued
    /// through `place_caret` / `place_selection_pair` and applied by `reflow`.
    pub fn restore_insertion(&mut self, caret: usize, selection: Option<(usize, usize)>) {
        let Self::Leaf(l) = self else {
            return;
        };
        match selection {
            Some((from, to)) => Editing::place_selection_pair(l, (from, to)),
            None => Editing::place_caret(l, caret),
        }
        Editing::reflow(l);
    }

    /// Whether this tab has motion in flight — a smooth scroll. The shell's
    /// fast-wake loop runs while any visible view reports this.
    pub fn is_animating(&self) -> bool {
        match self {
            Self::Welcome(_)
            | Self::Manage(_)
            | Self::Settings(_)
            | Self::Home(_)
            | Self::Trash(_)
            | Self::Tasks(_)
            | Self::Reminders(_)
            | Self::Feedback(_)
            // The run views animate nothing: they draw what a run is, and a
            // run changes when the worker says so, not on a timer.
            | Self::Run(_)
            | Self::Result(_)
            | Self::Diff(_) => false,
            Self::AiChat(a) => View::animating(a),
            Self::Socials(a) => View::animating(a),
            Self::Session(s) => View::animating(s),
            Self::Canvas(c) => View::animating(c),
            Self::Leaf(l) => View::animating(l),
        }
    }

    pub fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        match self {
            Self::Welcome(_)
            | Self::Trash(_)
            | Self::Diff(_)
            // A run view wakes on what its worker sends, so it asks for no
            // deadline of its own — an idle run costs no wake.
            | Self::Run(_)
            | Self::Result(_)
            // Home's panel is driven by the frame, as the Tasks page's is.
            | Self::Home(_)
            | Self::Tasks(_) => None,
            // **Only while a transport-mode write it started is unanswered.**
            // This view sat in the group above until B10 gave it background
            // work, and nothing moved it: the prompt wrote its answer into a
            // channel that was never drained, so a person said "Move
            // everything", saw no error, and the Store was never told.
            Self::Manage(m) => View::maintenance_deadline(m),
            // The snooze chooser's minute tick, asked only of the tab in
            // front — the deadline list reads the active tab alone.
            Self::Reminders(r) => View::maintenance_deadline(r),
            // The Terminal section's link, and the deadline that turns silence
            // from the authority into the read-only state.
            Self::Settings(s) => View::maintenance_deadline(s),
            Self::AiChat(a) => View::maintenance_deadline(a),
            Self::Socials(a) => View::maintenance_deadline(a),
            // The caret's quiet interval — and `None` for every inactive
            // session tab, which is what keeps a hidden one costing no wake.
            Self::Session(s) => View::maintenance_deadline(s),
            Self::Feedback(f) => View::maintenance_deadline(f),
            Self::Canvas(c) => View::maintenance_deadline(c),
            Self::Leaf(l) => View::maintenance_deadline(l),
        }
    }

    pub fn maintain(&mut self, ws: &mut WorkspaceDoc, now: std::time::Instant) -> bool {
        match self {
            Self::Welcome(_)
            | Self::Trash(_)
            | Self::Diff(_)
            | Self::Run(_)
            | Self::Result(_)
            | Self::Home(_)
            | Self::Tasks(_) => false,
            // The other half of the deadline above: without this the answer
            // is put on the wire and its reply never collected.
            Self::Manage(m) => View::maintain(m, ws, now),
            // The page's geometry rebuild — it runs here, on the active tab's
            // ordinary maintain, after the container's feeding block.
            Self::Reminders(r) => View::maintain(r, ws, now),
            Self::Settings(s) => View::maintain(s, ws, now),
            Self::AiChat(a) => View::maintain(a, ws, now),
            Self::Socials(a) => View::maintain(a, ws, now),
            Self::Session(s) => View::maintain(s, ws, now),
            Self::Feedback(f) => View::maintain(f, ws, now),
            Self::Canvas(c) => View::maintain(c, ws, now),
            Self::Leaf(l) => View::maintain(l, ws, now),
        }
    }
}

/// What a document is called before anybody has called it anything.
pub const DRAFT_NAME: &str = "Untitled";

/// A document that does not exist yet.
///
/// **`+` opens a page you can type on, and nothing is written to the workspace
/// until you do.** Somebody who opens a tab, looks at it and changes their mind
/// should not have left an empty *Untitled* behind in the library — and with a
/// CRDT they would have left it in everybody else's library too, which is the
/// part that makes "create it and tidy up later" the wrong answer rather than
/// merely an untidy one.
///
/// **It is a real editor over a private in-memory workspace**, not a mock-up of
/// one. So the caret, the toolbar, the margins, the key map and every future
/// change to any of them are the document view's, with no second implementation
/// to drift — and the moment of promotion is invisible because there is nothing
/// to swap visually. The private `Workspace` is the whole trick: `Loro` is happy
/// to hold a document nobody ever saves.
///
/// It stays permanently empty. The first keystroke that would type is
/// intercepted *before* the leaf sees it ([`MainView::promote_draft`]), so there
/// is never any content here to migrate — which is what keeps this from being a
/// second copy of the document model.
///
/// [`MainView::promote_draft`]: super::MainView::promote_draft
pub struct Draft {
    /// Not the window's workspace. Never saved, never synced, thrown away whole
    /// when the tab is promoted or closed.
    pub workspace: Workspace,
    /// Library document that was open when this draft was created — the new
    /// page becomes its next sibling on promote. `None` when there was no
    /// anchor (welcome, empty library).
    pub library_anchor: Option<NodeId>,
}

/// This build's release notes, rendered as a page rather than fetched as one.
///
/// **The same trick as [`Draft`], for a different reason.** A `Loro` document
/// nobody ever saves is a perfectly good document, so the notes are parsed into
/// a private in-memory workspace and shown by the ordinary document view — which
/// is why they arrive in Koru's own typography, with real headings and real
/// bullets, and why there is no second renderer to keep in step with the first.
///
/// The alternative was a hand-drawn panel. The agent's own window was considered
/// and lost for reasons written up in the ADR; inside `apps/koru` it would still
/// have meant a Markdown-to-scene renderer that this crate already contains.
///
/// **Read-only, in three layers**, because two of them are not enough:
/// the workspace is private, so nothing here can reach the person's content
/// (structural, not a policy); `whats_new` is not `draft`, so nothing promotes
/// it; and `ViewState::read_only` stops the page accepting edits it would
/// silently discard.
pub struct WhatsNewTab {
    /// Not the window's workspace. Never saved, never synced, thrown away whole
    /// when the tab is closed.
    pub workspace: Workspace,
    /// Which private page this is. The release notes were the only one; the
    /// Sync settings placeholder is built the same way (a read-only page
    /// over a private workspace), and the session file spells each.
    pub page: PrivatePage,
}

/// The pages a tab can host over a private workspace.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PrivatePage {
    /// This build's release notes.
    WhatsNew,
    /// The who/where popover's sync control: a page that says only its name
    /// for now (QUESTIONS.md E31).
    SyncSettings,
}

/// One tab: what it shows, whether it can be closed, and whether it has been
/// laid out at the size it is currently displayed at.
pub struct Tab {
    /// Identity of this tab *instance*, not of the document it shows. Two tabs
    /// on the same document are different places in Back/Forward.
    pub id: u64,
    pub view: TabView,
    /// Which **document** this tab is over, when it is over one.
    ///
    /// **Not derivable from the view.** A tab's `root()` is a `NodeId` inside
    /// its document, and a tab whose body was rebuilt against a *different*
    /// document reports that one's root instead — which is how clicking a
    /// document sometimes reused a tab rather than opening one, and why the
    /// reuse test has to compare documents rather than nodes.
    ///
    /// `None` for the welcome screen, the Manage tab, the Trash tab and
    /// a draft, none of which is over a document in the workspace.
    pub document: Option<koru_core::DocumentId>,
    /// What the tab is called. Resolved from the document each frame rather than
    /// stored, so renaming in the tab retitles it — this is only the cache.
    /// A leading emoji has been stripped into [`icon`] so the strip does not
    /// draw it twice.
    pub label: String,
    /// Leading title emoji for the icon slot, when the document name starts with
    /// one. Welcome / workspaces / drafts leave this empty.
    pub icon: Option<String>,
    /// The content size this tab was last laid out at. `None` means stale.
    ///
    /// This is what keeps a resize O(1) rather than O(tabs): a live drag
    /// reflows the tab you can see and marks the rest stale, and each of those
    /// reflows once, when it is next shown. A stale tab that adopts lays out at
    /// its old width — wasted work at worst, never wrong output, because it is
    /// corrected before it is drawn.
    pub sized: Option<(f32, f32)>,
    /// Set while this tab's document has not been created yet. See [`Draft`].
    ///
    /// **A field rather than a `TabView` arm**, and that is the load-bearing
    /// choice: a draft *is* a `Leaf`, so all forty-odd `if let TabView::Leaf(l)`
    /// sites keep working unchanged. What differs is only which document the
    /// leaf is talked to through, and that is one substitution at each of the
    /// handful of places the container hands a view the workspace —
    /// [`Tab::doc`]. A third arm would have made every one of those forty a
    /// place to forget the new case, silently.
    ///
    /// **Not the only tab with a private workspace any more** — see
    /// [`whats_new`](Self::whats_new). This one means *empty and waiting to
    /// become real*, which is why `promote_draft` and the `Escape`-closes-it
    /// branch both key on it; a page that will never be promoted must not.
    pub draft: Option<Draft>,
    /// Set while this tab is showing this build's release notes. See
    /// [`WhatsNewTab`].
    ///
    /// **A second field rather than a second kind of [`Draft`]**, and the
    /// difference is what a keystroke means. A draft is *empty and waiting to
    /// become real*: `MainView::promote_draft` turns the first character into a
    /// document in the person's library, and `Escape` throws it away. Release
    /// notes are the opposite on both counts — full, and never anybody's
    /// document. Sharing the field would mean auditing every `draft.is_some()`
    /// in the container for whether it meant "private workspace" or "about to be
    /// promoted", and the failure of getting one wrong is a page called *What's
    /// new* appearing in somebody's library.
    ///
    /// The cost of a second field is that the workspace substitution has two
    /// sources; [`Tab::private`] is the one place that knows, so the three
    /// public accessors cannot disagree.
    pub whats_new: Option<WhatsNewTab>,
    /// Home workspace of the document this tab shows, when it shows one.
    ///
    /// Set for every real document tab (Main and Pinned). Selecting the tab
    /// opens the document in place without switching Explore; Reveal /
    /// Ctrl+Shift+E uses this to move the active workspace.
    pub home: Option<koru_core::WorkspaceId>,
    /// Home workspace when this tab is pinned. `None` is not pinned.
    ///
    /// Pin membership is this field; the document's home is [`home`](Self::home)
    /// (kept in step when pinning).
    pub pinned: Option<koru_core::WorkspaceId>,
    /// When pinned, whether the strip shows the title as well as the icon.
    /// Copied from the Welcome default at pin time; the tab menu overrides it.
    pub show_title: bool,
}

/// Split a document name into the tab's icon (leading emoji) and label.
pub fn tab_title(name: &str) -> (Option<String>, String) {
    let (emoji, rest) = koru_shell_chrome::split_leading_emoji(name);
    (emoji.map(str::to_owned), rest.to_owned())
}

fn mint_tab_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Tab {
    /// What a window opens on, and what is left when the last tab closes.
    ///
    /// There is no permanent tab any more. The outline used to be one, because
    /// closing it would have left the `outline/` realm with no way back — but
    /// the outline is becoming a `TaskGroup` node *inside* a document rather
    /// than a view of its own, so there is nothing left that only a fixed tab
    /// could reach.
    pub fn welcome() -> Self {
        Self {
            id: mint_tab_id(),
            view: TabView::Welcome(WelcomeView::new()),
            document: None,
            label: "Welcome".to_string(),
            icon: None,
            sized: None,
            draft: None,
            whats_new: None,
            home: None,
            pinned: None,
            show_title: false,
        }
    }

    pub fn first_minutes(profile: impl Into<String>, workspace: impl Into<String>) -> Self {
        let mut tab = Self::welcome();
        tab.view = TabView::Welcome(WelcomeView::first_minutes(profile, workspace));
        tab
    }

    pub fn settings() -> Self {
        let mut tab = Self::welcome();
        tab.view = TabView::Settings(crate::settings_view::SettingsView::new());
        tab.label = "Settings".to_string();
        tab
    }

    /// The Home tab: Today. A singleton, like Settings; `open_home_tab`
    /// reuses the one that is open.
    pub fn home() -> Self {
        let mut tab = Self::welcome();
        tab.view = TabView::Home(crate::panes::home::HomePage::new());
        tab.label =
            koru_i18n::t(crate::format::lang(), koru_i18n::Key::A11yTaskBucketToday).to_owned();
        tab
    }

    pub fn feedback() -> Self {
        let mut tab = Self::welcome();
        tab.view = TabView::Feedback(crate::feedback::FeedbackView::new());
        tab.label = "Feedback".to_string();
        tab
    }

    /// The Feedback tab opened on *Something isn't working* — the help
    /// popover's *Report a problem*.
    pub fn report_problem() -> Self {
        let mut tab = Self::welcome();
        tab.view = TabView::Feedback(crate::feedback::FeedbackView::with_kind(
            crate::feedback::Kind::Bug,
        ));
        tab.label = "Feedback".to_string();
        tab
    }

    /// The Trash tab. Its glyph comes from [`TabView::icon`], not from a
    /// leading emoji — it is a place, and places have no title to take one from.
    pub fn trash() -> Self {
        Self {
            id: mint_tab_id(),
            view: TabView::Trash(crate::trash::TrashView::new()),
            document: None,
            label: "Trash".to_string(),
            icon: None,
            sized: None,
            draft: None,
            whats_new: None,
            home: None,
            pinned: None,
            show_title: false,
        }
    }

    /// The Tasks tab, on `target`. Its glyph comes from [`TabView::icon`] for
    /// the Trash tab's reason: it is a place, and places have no title to take
    /// a leading emoji from — except a list, which brings its swatch, and that
    /// is [`Tab::tasks_label`]'s other half.
    ///
    /// **One per window at most.** The rail has one selection, so a second
    /// Tasks tab would be a second answer to *which bucket am I looking at* —
    /// the container re-points the existing one instead. Which is exactly why
    /// the label cannot be the word *Tasks*: one tab shows five queues and
    /// every list in turn, so a fixed name says nothing about where you are,
    /// and the strip is the one place a background window's answer is visible.
    pub fn tasks(target: crate::panes::explore::rail::RailTarget) -> Self {
        // The Reminders row is a rail target like any other, but its page is
        // a different view — `Tab::tasks` is the session restore's door too,
        // so the swap happens here and every caller stays honest.
        if target == crate::panes::explore::rail::RailTarget::Reminders {
            return Self::reminders();
        }
        let view = crate::tasks_view::TasksView::new(target);
        let label = view.title();
        Self {
            id: mint_tab_id(),
            view: TabView::Tasks(view),
            document: None,
            label,
            icon: None,
            sized: None,
            draft: None,
            whats_new: None,
            home: None,
            pinned: None,
            show_title: false,
        }
    }

    /// The All Reminders page. A fixed label for a fixed place — the Tasks
    /// tab's reason in reverse: one page, so one word says everything.
    pub fn reminders() -> Self {
        let label = crate::panes::explore::rail::reminders_label(crate::format::lang());
        Self {
            id: mint_tab_id(),
            view: TabView::Reminders(super::reminders_view::RemindersView::new()),
            document: None,
            label: label.to_string(),
            icon: None,
            sized: None,
            draft: None,
            whats_new: None,
            home: None,
            pinned: None,
            show_title: false,
        }
    }

    /// The version-diff tab. One per window at most, like the Manage
    /// tab — a new comparison replaces what the existing tab shows.
    /// Deliberately never captured into the session: a diff of two history
    /// entries is a working artifact, and restoring it would need a re-fetch
    /// that can fail at startup.
    pub fn diff(view: crate::diff::DiffView) -> Self {
        Self {
            id: mint_tab_id(),
            view: TabView::Diff(view),
            document: None,
            label: koru_i18n::t(crate::format::lang(), koru_i18n::Key::DiffTabTitle).to_string(),
            icon: None,
            sized: None,
            draft: None,
            whats_new: None,
            home: None,
            pinned: None,
            show_title: false,
        }
    }

    /// The singleton Manage tab, on whichever page the caller then shows.
    pub fn manage() -> Self {
        Self {
            id: mint_tab_id(),
            view: TabView::Manage(crate::manage::ManageView::new()),
            document: None,
            label: "Manage".to_string(),
            icon: None,
            sized: None,
            draft: None,
            whats_new: None,
            home: None,
            pinned: None,
            show_title: false,
        }
    }

    /// A Koru AI chat tab. Starts a fresh chat; workspace context is attached
    /// when the tab is drawn, but no active document is pre-attached.
    pub fn ai_chat() -> Self {
        Self::ai_chat_view(AiChatView::new())
    }

    pub fn socials(surface: koru_socials_ui::SharedSurface) -> Self {
        let mut tab = Self::welcome();
        tab.view = TabView::Socials(crate::socials_view::SocialsView::with_surface(surface));
        tab.label =
            koru_i18n::socials::t(crate::format::lang(), koru_i18n::socials::Key::Title).to_owned();
        tab
    }

    /// A Koru AI tab opened on its provider setup.
    pub fn ai_setup() -> Self {
        Self::ai_chat_view(AiChatView::setup())
    }

    /// A Koru AI chat tab seated on an existing conversation.
    pub fn ai_chat_existing(id: koru_ai_ui::ChatId) -> Self {
        Self::ai_chat_view(AiChatView::existing(id))
    }

    fn ai_chat_view(view: AiChatView) -> Self {
        let (icon, label) = view
            .title()
            .map(tab_title)
            .unwrap_or_else(|| (None, "AI Chat".to_string()));
        Self {
            id: mint_tab_id(),
            view: TabView::AiChat(view),
            document: None,
            label,
            icon,
            sized: None,
            draft: None,
            whats_new: None,
            home: None,
            pinned: None,
            show_title: false,
        }
    }

    /// A session tab: a shell or agent running as an ordinary tab. `label` is
    /// the profile's display name ("PowerShell"); its glyph comes from
    /// [`TabView::icon`] — a session is a place, and places take no emoji.
    /// One delegated run's views (P1.16). Its label is the run's derived
    /// two-word name (ADR-360) — never its objective, which is a paragraph
    /// and a tab strip is not where a paragraph goes, and never the view
    /// showing, which told a person nothing about which run they had open.
    pub fn run(view: crate::run_view::RunView, label: &str) -> Self {
        let mut tab = Self::welcome();
        tab.view = TabView::Run(view);
        tab.label = label.to_owned();
        tab
    }

    /// One result of a run, as its own tab (ADR-368). Its label is what the
    /// result is called — the name a person recognises, and the same name
    /// the document is headed by.
    pub fn result(view: crate::result_view::ResultView, label: &str) -> Self {
        let mut tab = Self::welcome();
        tab.view = TabView::Result(view);
        tab.label = label.to_owned();
        tab
    }

    pub fn session(view: crate::session_tab::SessionView, label: &str) -> Self {
        let mut tab = Self::welcome();
        tab.view = TabView::Session(view);
        tab.label = label.to_owned();
        tab
    }

    /// `name` is the document's full title; a leading emoji is peeled into
    /// [`icon`](Self::icon) and the rest becomes the label.
    pub fn document(leaf: Leaf, name: &str) -> Self {
        Self::document_of(leaf, name, None)
    }

    /// The same, saying which document it is over.
    ///
    /// Separate rather than a fourth argument everywhere, because half the
    /// callers — the tests, and the restore path before it knows — have no
    /// `DocumentId` to give and `None` is honest for them.
    pub fn document_of(leaf: Leaf, name: &str, document: Option<koru_core::DocumentId>) -> Self {
        Self::document_at(leaf, name, document, None)
    }

    /// A document tab that knows its home workspace.
    ///
    /// `view` is whatever the document is looked at through — a page's
    /// [`Leaf`] or a canvas's view — which `MainView::document_view` decides
    /// from the document's manifest kind.
    pub fn document_at(
        view: impl Into<TabView>,
        name: &str,
        document: Option<koru_core::DocumentId>,
        home: Option<koru_core::WorkspaceId>,
    ) -> Self {
        let (icon, label) = tab_title(name);
        Self {
            id: mint_tab_id(),
            view: view.into(),
            document,
            label,
            icon,
            sized: None,
            draft: None,
            whats_new: None,
            home,
            pinned: None,
            show_title: false,
        }
    }

    /// Workspace this document tab belongs to (pin home, else recorded home).
    pub fn home_workspace(&self) -> Option<koru_core::WorkspaceId> {
        self.home.or(self.pinned)
    }

    /// Whether the strip draws an icon slot for this tab.
    ///
    /// Place tabs (Trash, Tasks, Feedback, Diff) wear a Phosphor glyph from
    /// [`TabView::icon`]; documents wear a leading emoji in [`icon`]. Pinned
    /// tabs always reserve the slot. Width and draw both ask this so an icon
    /// cannot steal space from a label measured without it.
    ///
    /// **Two branches found this the same week and fixed it the same way.**
    /// `dev` wrote it for the Feedback tab; the Tasks branch wrote it as
    /// `draws_icon` because *Upcoming* on the Tasks tab was reading *Upco* —
    /// the label losing exactly the icon slot. One name survives the merge and
    /// it is this one; what the other carried and this did not is the gap
    /// below.
    ///
    /// **One gap remains, and it is the same shape.** The strip also draws a
    /// bin glyph for a tab whose root is in the trash, and answering that needs
    /// the workspace, which measuring does not have. It only ever *adds* an
    /// icon where this says there is none, so it under-measures rather than
    /// over-measures — the same clipping, on a case nobody has reported.
    pub fn strip_shows_icon(&self) -> bool {
        self.is_pinned() || self.icon.is_some() || self.view.icon().is_some()
    }

    /// Take the label from the Tasks view this tab holds, if it holds one.
    ///
    /// **The view is the authority and the label is the cache**, which is the
    /// contract [`label`](Self::label) already states for a document tab. Two
    /// things move it: pressing another queue, and renaming or recolouring the
    /// list it is showing — and the second is why this is a refresh rather
    /// than a line in `open_tasks_tab`, because nobody presses anything when a
    /// peer renames a list.
    pub fn refresh_tasks_label(&mut self) -> bool {
        self.refresh_tasks_label_for(crate::format::lang())
    }

    /// The same refresh under an explicit language, so locale adoption can be
    /// proven without mutating the process-global language in a parallel test.
    pub(super) fn refresh_tasks_label_for(&mut self, lang: koru_i18n::Lang) -> bool {
        let label = match &self.view {
            TabView::Tasks(view) => view.title(),
            TabView::Reminders(_) => crate::panes::explore::rail::reminders_label(lang).to_owned(),
            _ => return false,
        };
        let changed = self.label != label;
        self.label = label;
        changed
    }

    /// Apply a fresh document name to the cached label and icon.
    pub fn set_name(&mut self, name: &str) {
        let (icon, label) = tab_title(name);
        self.icon = icon;
        self.label = label;
    }

    /// This build's release notes, as a page. See [`WhatsNewTab`].
    ///
    /// `notes` is the Markdown that shipped beside the executable, which is
    /// absent on any build nobody published — the greeting stands alone then,
    /// and the tab is still worth opening because the greeting is the part that
    /// says an update happened.
    ///
    /// `None` only if Loro will not make a document at all. The caller opens
    /// nothing in that case: a missing notice is invisible, where a tab that
    /// failed halfway would be a mystery.
    pub fn whats_new(version: &str, notes: Option<&str>) -> Option<Self> {
        let title = format!("What's new in Koru {version}");
        Self::private_page(
            PrivatePage::WhatsNew,
            &title,
            &crate::whatsnew::blocks(version, notes),
        )
    }

    /// The Sync settings tab: its name as the title and nothing under it yet
    /// (the CTO's artboard; QUESTIONS.md E31). A singleton, like Settings.
    pub fn sync_settings() -> Option<Self> {
        let title = koru_i18n::t(crate::format::lang(), koru_i18n::Key::SyncSettingsTab);
        Self::private_page(PrivatePage::SyncSettings, title, &[])
    }

    /// A read-only page over a workspace of its own, titled and filled.
    ///
    /// `None` only if Loro will not make a document at all. The caller opens
    /// nothing in that case: a missing notice is invisible, where a tab that
    /// failed halfway would be a mystery.
    fn private_page(
        page: PrivatePage,
        title: &str,
        blocks: &[koru_workspace_host::RichBlock],
    ) -> Option<Self> {
        let doc = WorkspaceDoc::create(WorkspaceId::new(), title).ok()?;
        let mut workspace = Workspace::new(doc);
        // The title goes in row 0 the way every document's does, so the page has
        // a real heading and the tab has a real name, with no second mechanism.
        let root = koru_workspace::create_document(&mut workspace.ws, title)?;
        let mut leaf = Leaf::document(&workspace.ws, root);
        // **Filled before it is made read-only.** The guard refuses writes from
        // the key map, and `paste_rich` is not one — but ordering it this way
        // means the flag is true for the whole of the tab's visible life, rather
        // than for all of it except the frame nobody sees.
        if !blocks.is_empty() {
            View::paste_rich(&mut leaf, &mut workspace.ws, None, blocks);
        }
        // **Drop the empty paragraph `create_document` seeds.** Every new
        // document gets one so there is a line to type into; this page is never
        // typed into, and the blocks are *appended* after it — so left alone it
        // is a blank line between the title and the first thing the page has to
        // say. Removed rather than pasted over, because `paste_rich` takes an
        // anchor to insert *after* and has no notion of replacing.
        if let Ok(children) = workspace.ws.children(koru_core::schema::Parent::Node(root))
            && let Some(&first) = children.first()
            && children.len() > 1
            && workspace
                .ws
                .read(first)
                .ok()
                .and_then(|n| n.content.as_text().map(str::is_empty))
                .unwrap_or(false)
        {
            // `delete`, not `delete_permanently`: the latter refuses anything
            // that is not already in the bin (`NotInTrash`). A move is all that
            // is wanted here — the whole workspace is thrown away when the tab
            // closes, so where the node goes is immaterial; what matters is
            // that `children` stops returning it.
            let _ = workspace.ws.delete(first);
            leaf = Leaf::document(&workspace.ws, root);
        }
        leaf.set_read_only(true);
        Some(Self {
            id: mint_tab_id(),
            view: TabView::Leaf(leaf),
            // Not a document in the person's workspace, so nothing in the
            // explore column or the tab-reuse check may match it.
            document: None,
            label: title.to_owned(),
            icon: None,
            sized: None,
            draft: None,
            whats_new: Some(WhatsNewTab { workspace, page }),
            home: None,
            pinned: None,
            show_title: false,
        })
    }

    /// An empty document that is not in the workspace yet. See [`Draft`].
    ///
    /// `library_anchor` is the real-workspace document the new page should sit
    /// under in Explore (next sibling). Capture it **before** switching to the
    /// draft — the draft's own root is not in the library.
    ///
    /// `None` only if Loro cannot make a document at all, which is the same
    /// failure `+` would hit anyway — the caller falls back to a welcome tab
    /// rather than opening a tab that cannot be typed in.
    pub fn draft(library_anchor: Option<NodeId>) -> Option<Self> {
        let doc = WorkspaceDoc::create(WorkspaceId::new(), DRAFT_NAME).ok()?;
        let mut workspace = Workspace::new(doc);
        // An empty title, not a prefilled one: the row draws its "Title"
        // placeholder, and the label below and every other display fallback
        // still read *Untitled* until somebody names it.
        let root = koru_workspace::create_document(&mut workspace.ws, "")?;
        let mut leaf = Leaf::document(&workspace.ws, root);
        // A brand-new page is the one place the caret belongs in the title:
        // naming the thing is the first thing you do with it. Opening an
        // *existing* document still lands in the body — see
        // `BuiltInDocument::open`.
        leaf.focus_title();
        Some(Self {
            id: mint_tab_id(),
            view: TabView::Leaf(leaf),
            document: None,
            label: DRAFT_NAME.to_string(),
            icon: None,
            sized: None,
            whats_new: None,
            draft: Some(Draft {
                workspace,
                library_anchor,
            }),
            home: None,
            pinned: None,
            show_title: false,
        })
    }

    pub fn is_pinned(&self) -> bool {
        self.pinned.is_some()
    }

    /// A slot Explore (and SameTab on a pin) may overwrite.
    ///
    /// Pins keep their document; a draft is unsaved private workspace; and
    /// **Welcome is a dedicated tab**, never consumed by what is opened from
    /// it — opening anything while it is active opens a new tab beside it,
    /// the same way a pin does. So is **Socials**, whose editor can hold an
    /// unsaved post. Trash, Manage, What's new and ordinary document tabs are
    /// reusable.
    pub fn is_reusable(&self) -> bool {
        !self.is_pinned()
            && self.draft.is_none()
            && !matches!(self.view, TabView::Welcome(_) | TabView::Socials(_))
    }

    /// **The workspace this tab's view must be talked to through.**
    ///
    /// The window's, for every ordinary tab; its own private one for a draft.
    /// Every place the container hands the active view a document goes through
    /// here, so a draft cannot be handed the real workspace by accident — which
    /// would write into it the very thing a draft exists not to write.
    /// They come out **together** because every caller needs both at once, and
    /// splitting the borrow inside one method is what lets it: `view` and
    /// `draft` are disjoint fields, so the compiler is happy to hand out one
    /// reference to each — which it would not be if the caller asked twice.
    pub fn doc<'a>(
        &'a mut self,
        ws: &'a mut WorkspaceDoc,
    ) -> (&'a mut TabView, &'a mut WorkspaceDoc) {
        let doc = match Self::private_mut(&mut self.draft, &mut self.whats_new) {
            Some(w) => &mut w.ws,
            None => ws,
        };
        (&mut self.view, doc)
    }

    /// The private workspace this tab is talked to through, if it has one.
    ///
    /// **One place that knows there is more than one kind.** Two tabs own a
    /// workspace of their own — a draft and the release notes — and three
    /// accessors need to substitute it. Written out three times, adding a third
    /// kind later would be three places to forget, and forgetting one hands that
    /// view the *window's* workspace: the exact failure a private workspace
    /// exists to make impossible, arriving down the one path nobody exercised.
    fn private<'a>(
        draft: &'a Option<Draft>,
        notes: &'a Option<WhatsNewTab>,
    ) -> Option<&'a Workspace> {
        match (draft, notes) {
            (Some(d), _) => Some(&d.workspace),
            (_, Some(n)) => Some(&n.workspace),
            _ => None,
        }
    }

    /// [`Self::private`], mutably. Split because the borrow checker will not
    /// hand out `&mut self` and a reborrowed field at once.
    fn private_mut<'a>(
        draft: &'a mut Option<Draft>,
        notes: &'a mut Option<WhatsNewTab>,
    ) -> Option<&'a mut Workspace> {
        match (draft, notes) {
            (Some(d), _) => Some(&mut d.workspace),
            (_, Some(n)) => Some(&mut n.workspace),
            _ => None,
        }
    }

    /// The read-only half of [`Tab::doc`].
    pub fn doc_ref<'a>(&'a mut self, ws: &'a WorkspaceDoc) -> (&'a mut TabView, &'a WorkspaceDoc) {
        let doc = match Self::private(&self.draft, &self.whats_new) {
            Some(w) => &w.ws,
            None => ws,
        };
        (&mut self.view, doc)
    }

    /// The whole `Workspace`, which is what drawing needs — it reaches the blob
    /// store for images as well as the document.
    pub fn workspace<'a>(&'a mut self, ws: &'a Workspace) -> (&'a mut TabView, &'a Workspace) {
        let w = match Self::private(&self.draft, &self.whats_new) {
            Some(w) => w,
            None => ws,
        };
        (&mut self.view, w)
    }

    /// Lay out at `size` if it has not been already. Returns whether it ran.
    pub fn ensure_sized(&mut self, w: f32, h: f32) -> bool {
        if self.sized == Some((w, h)) {
            return false;
        }
        self.sized = Some((w, h));
        match &mut self.view {
            TabView::Welcome(v) => v.set_viewport(w, h),
            TabView::Manage(v) => View::set_viewport(v, w, h),
            TabView::Trash(v) => v.set_viewport(w, h),
            TabView::Tasks(v) => v.set_viewport(w, h),
            TabView::Reminders(v) => v.set_viewport(w, h),
            TabView::Diff(v) => View::set_viewport(v, w, h),
            TabView::Settings(v) => View::set_viewport(v, w, h),
            TabView::Home(v) => View::set_viewport(v, w, h),
            TabView::AiChat(a) => View::set_viewport(a, w, h),
            TabView::Socials(a) => View::set_viewport(a, w, h),
            TabView::Session(s) => View::set_viewport(s, w, h),
            TabView::Run(r) => View::set_viewport(r, w, h),
            TabView::Result(r) => View::set_viewport(r, w, h),
            TabView::Feedback(v) => View::set_viewport(v, w, h),
            TabView::Canvas(c) => View::set_viewport(c, w, h),
            TabView::Leaf(l) => Leaf::set_viewport(l, w, h),
        }
        true
    }

    /// Mark this tab as needing layout before it is next shown.
    pub fn invalidate_size(&mut self) {
        self.sized = None;
    }
}

/// The mark for a session tab's launch profile: the shared
/// `koru_draw::brand::for_profile` projection over the session's frozen
/// wire key — the same mapping the `+` menu's profile rows draw from, so the
/// row you clicked and the tab it opened cannot wear different faces.
///
/// `None` — and thus the TERMINAL fallback — for a session with no recorded
/// profile, a custom profile, or a built-in this build predates.
fn session_mark(view: &crate::session_tab::SessionView) -> Option<koru_draw::brand::ProfileMark> {
    match view.profile_key.as_ref()? {
        koru_session_protocol::SessionProfileKey::BuiltIn(key) => {
            koru_draw::brand::for_profile(key.as_str())
        }
        koru_session_protocol::SessionProfileKey::Custom(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_title_peels_a_leading_emoji() {
        let (icon, label) = tab_title("🚀 Ship");
        assert_eq!(icon.as_deref(), Some("🚀"));
        assert_eq!(label, "Ship");
    }

    #[test]
    fn tab_title_leaves_plain_names_alone() {
        let (icon, label) = tab_title("Notes");
        assert_eq!(icon, None);
        assert_eq!(label, "Notes");
    }

    #[test]
    fn neither_welcome_nor_a_draft_is_reusable() {
        assert!(!Tab::welcome().is_reusable(), "Welcome is a dedicated tab");
        assert!(!Tab::draft(None).expect("draft").is_reusable());
    }
}
