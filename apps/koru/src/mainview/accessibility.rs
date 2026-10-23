// SPDX-License-Identifier: MPL-2.0

//! **What a Koru container window offers a screen reader, and what a request
//! to one of its controls means.**
//!
//! One sentence, and what it excludes: *this module composes the window's
//! surfaces into one semantic tree, says which control has the keyboard, and
//! routes a request back to the behaviour that already owns it.* It excludes
//! identity, projection and lifetime, which are
//! [`koru_workspace_host::accessibility`]'s; it excludes what any individual
//! control *is*, which stays with whatever draws it — the explore column
//! answers for its own rows in `panes/explore/accessibility.rs`, the caption's
//! rectangles come from `koru_ui::caption`, a tab's from
//! `koru_shell_chrome::tabs`, a menu row's from `koru_ui::menu`. Nothing here
//! computes a rectangle of its own.
//!
//! `mainview/mod.rs` is 13,000 lines and gets seven `impl View` methods that
//! call in here (§7.10) — six of them one line, and
//! `accessibility_action` three, because it ends in `drain_trash_all` the way
//! `key`, `mouse_down` and `mouse_up` do. This is where the subsystem lives.
//!
//! ## The shape of the tree
//!
//! ```text
//! Window                          koru:wN/v0/s:window#window        (the host's)
//! ├── Minimize / Maximize / Close  koru:wN/v0/s:window#minimize …    (the host's, our rects)
//! └── Group   the shell            koru:wN/vV/s:shell#root
//!     ├── Group   explore column   koru:wN/vV/s:explore#column
//!     ├── TabList tabs             koru:wN/vV/s:tabs#strip
//!     │   └── Tab                  koru:wN/vV/tab:7#root
//!     │       └── Button close     koru:wN/vV/tab:7#close
//!     ├── Button  the caption's own controls
//!     ├── Group   save status      koru:wN/vV/s:save#root           (1b.4)
//!     ├── TabPanel the active tab's region
//!     │   ├── Group   the document  koru:wN/vV/s:body#root           (1b.3)
//!     │   └── Group   the Tasks page koru:wN/vV/s:tasks#page         (2.1)
//!     └── Menu | Dialog            koru:wN/vV/menu:3#root           (only while one is up)
//! ```
//!
//! ## Three rules
//!
//! **A blocker publishes alone.** §5 requires a modal to take focus with
//! nothing below it reachable, so when [`MainView::blockers`] says something is
//! standing, the shell publishes *that* and no siblings. There is no second
//! opinion about what is on top: the answer is the same ordered table the
//! keyboard uses.
//!
//! **A transient surface carries an instance number.** A context menu's rows
//! are fixed to a subject when it opens, so a locator built from the subject or
//! a row's position would let a request queued against a closed menu land in
//! its successor. `ContextMenu::instance` is minted once per opening and never
//! reused, which is what makes the host's refusal actually fire.
//!
//! **An action is the keystroke or the press that already exists.** Every arm
//! of [`MainView::accessibility_action`] reaches a function the keyboard or the
//! pointer reaches. Nothing here is a command path of its own.

use accesskit::{Action, ActionData, Role};
use koru_core::schema::WorkspaceDoc;
use koru_draw::geometry::Rect;
use koru_i18n::{Key, Lang, t};
use koru_workspace_host::accessibility::{
    CaptionGeometry, ControlPart, ObjectKey, Occurrence, Revision, Scope, SemanticNode, ViewSlot,
    WindowSlot,
};
use koru_workspace_host::routing::Blockers;

use super::MainView;
use super::tab::TabView;
use crate::panes::explore::accessibility::Context as ExploreContext;

mod chrome;
mod commands;
pub(crate) mod document;
mod menus;
pub(crate) mod persistence;
pub(crate) mod reminders;

#[cfg(test)]
mod tests;

/// **Where the surfaces separate plans built meet.** A second test module
/// because it asks a different question from `tests.rs` — not *is this
/// projection right* but *what happens when a menu stands over a published
/// document, or a save status changes while somebody is typing into one*. See
/// its own module note.
#[cfg(test)]
mod integration;

/// The window's own furniture, as one object. A locator then reads
/// `.../s:shell#tabs` and a person can see which layer a control came from.
const SHELL: &str = "shell";
/// A tab instance. See [`ObjectKey::Instance`].
const TAB: &str = "tab";
/// One opening of one menu. See `ContextMenu::instance`.
const MENU: &str = "menu";

/// The parts of the shell that are not tabs, rows or menu items.
pub(crate) mod parts {
    use koru_workspace_host::accessibility::ControlPart;

    /// The whole container, under the window root.
    pub const ROOT: ControlPart = ControlPart::new("root");
    /// The tab strip, its `+`, and a tab's own close box.
    pub const TABS: ControlPart = ControlPart::new("tabs");
    pub const NEW_TAB: ControlPart = ControlPart::new("new-tab");
    pub const CLOSE_TAB: ControlPart = ControlPart::new("close");
    /// The caption's own three: the overflow `…`, the pane toggle, the
    /// magnifier. Minimise, maximise and close belong to the *window* and are
    /// the host's — see `koru_workspace_host::accessibility::window_commands`.
    ///
    /// The lane's Search is here too: it is window chrome, in the lane while
    /// the pane is open and in the caption's chain while it is shut.
    pub const WINDOW_MENU: ControlPart = ControlPart::new("window-menu");
    pub const TOGGLE_EXPLORE: ControlPart = ControlPart::new("toggle-explore");
    pub const SEARCH: ControlPart = ControlPart::new("search");
    /// The region the active tab draws into.
    pub const CONTENT: ControlPart = ControlPart::new("content");
    /// The Koru AI column. Named, and empty: its contents are `koru-ai-ui`'s
    /// surface, and this plan neither owns nor verifies them.
    pub const AI: ControlPart = ControlPart::new("ai");
    /// A surface that owns everything beneath it. Used only as the fall-back
    /// keyword; a standing blocker's part is the kind it *is*
    /// (`#confirm`, `#rename`) — see `menus::blocker_keyword`.
    pub const BLOCKER: ControlPart = ControlPart::new("blocker");
}

/// What the shell hands each of its parts, so nothing has to work out where it
/// is for itself.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Shell {
    pub window: WindowSlot,
    pub view: ViewSlot,
    pub viewport: (f32, f32),
    pub lang: Lang,
}

impl Shell {
    pub(crate) fn surface(&self, name: &'static str, part: ControlPart) -> Occurrence {
        Occurrence::surface(self.window, self.view, name, part)
    }

    pub(crate) fn instance(&self, kind: &'static str, id: u64, part: ControlPart) -> Occurrence {
        Occurrence::instance(self.window, self.view, kind, id, part)
    }
}

impl MainView {
    /// The window and view this container was seated in, as
    /// [`View::seat_accessibility`] told it.
    ///
    /// `view` is passed in as well and is the authority — the host hands it to
    /// every semantic call — but the window is only ever learned at seating,
    /// which is why it is stored.
    fn shell(&self, view: ViewSlot) -> Shell {
        Shell {
            window: self.accessibility_window,
            view,
            viewport: self.viewport,
            lang: koru_workspace_host::accessibility::ui_lang(),
        }
    }

    /// Remember where this container was put. See
    /// [`View::seat_accessibility`].
    pub(crate) fn seat_accessibility(&mut self, window: WindowSlot) {
        self.accessibility_window = window;
    }

    /// Everything this window offers a screen reader right now — or, in a
    /// [`Scope::Delta`], everything about it that changed.
    ///
    /// **Two publishes, and the second is what makes a large document
    /// typable.** The chrome — the column, the strip, the caption, the save
    /// status, whatever blocker is standing — is a couple of dozen nodes and is
    /// re-sent whole whenever any of it moves. The document body is O(the
    /// document) and is re-sent whole only when the client is not holding it;
    /// the rest of the time a keystroke publishes the one paragraph that
    /// changed and nothing else. Which of the two is happening is
    /// [`chrome_revision`](MainView::chrome_revision)'s answer, compared with
    /// what the client is holding.
    pub(crate) fn semantics(&self, view: ViewSlot, scope: Scope) -> Vec<SemanticNode> {
        let cx = self.shell(view);
        let chrome = self.chrome_revision();
        let whole = scope == Scope::Whole || !self.bodies.borrow().holds_chrome(chrome);
        if !whole {
            // The chrome is exactly what the client already holds, so this
            // publish is the document's difference and nothing else. The
            // paragraphs go up as loose nodes and keep the parent the client
            // has — an AccessKit update is a difference by construction, and
            // this is the one place Koru takes that.
            return self.body_delta(&cx);
        }
        // **The blocker first, and alone.** Same table, same order, same
        // answer the keyboard gets.
        let blockers = self.blockers();
        let children = if blockers.is_blocking() {
            self.bodies.borrow_mut().body_withheld(&cx);
            self.blocking_semantics(&cx, blockers)
        } else {
            self.ordinary_semantics(&cx, scope)
        };
        let mut bodies = self.bodies.borrow_mut();
        bodies.published_chrome(chrome);
        if scope == Scope::Whole {
            bodies.retirement_is_moot();
        }
        drop(bodies);
        let root = SemanticNode::new(cx.surface(SHELL, parts::ROOT), Role::Group)
            .with_bounds(Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1))
            .with_children(children);
        // **What the chrome just published, so the next chrome can say what has
        // gone.** The last thing before the tree leaves, and after the blocker
        // decision, so an open menu's set really is *the menu alone*.
        let mut surfaces = self.surfaces.borrow_mut();
        surfaces.published(std::iter::once(&root));
        // Emptied **after** the comparison, not before it, for the same reason
        // `bodies` is: a whole snapshot replaces the host's map outright, so it
        // says by omission everything this list could say — including whatever
        // the comparison just found. Clearing first would leave those entries
        // queued behind a snapshot that had already dealt with them.
        if scope == Scope::Whole {
            surfaces.retirement_is_moot();
        }
        drop(surfaces);
        vec![root]
    }

    /// Controls this view has taken away since the last publish. See
    /// [`View::accessibility_retired`](koru_workspace_host::View::accessibility_retired).
    ///
    /// **Two records, because the two halves are published differently.** The
    /// body is a difference and knows row by row what it removed; the chrome is
    /// re-sent whole and knows only by comparing one publish with the last.
    pub(crate) fn accessibility_retired(&self) -> Vec<Occurrence> {
        let mut out = self.bodies.borrow_mut().take_retired();
        out.extend(self.surfaces.borrow_mut().take_retired());
        out
    }

    fn ordinary_semantics(&self, cx: &Shell, scope: Scope) -> Vec<SemanticNode> {
        let mut out = Vec::new();
        if self.sidebar.is_open() {
            out.push(self.explore.semantics(&self.explore_context(cx)));
        }
        out.push(self.tabs_semantics(cx));
        out.extend(self.caption_semantics(cx));
        // **Before the content, because it belongs to the caption.** The chip
        // it exposes is drawn there, and somebody walking the chrome on their
        // way into the document passes "is my work saved" before they arrive
        // rather than after they have left. See `persistence.rs`.
        out.extend(self.persistence_semantics(cx));
        // **Beside the save status, and for its reason.** *Is my work saved*
        // and *when will Koru remind me about this* are the two questions a
        // person answers about the item in front of them without opening
        // anything, and somebody walking the chrome on their way into the
        // document passes both before they arrive. See `reminders.rs`.
        out.extend(self.reminder_semantics(cx));
        out.push(self.content_semantics(cx, scope));
        out.extend(commands::semantics(self, cx));
        // Named and empty, so that "the keyboard is in the AI composer" can be
        // published as somewhere that exists. Describing what is in it is
        // `koru-ai-ui`'s surface and another plan's.
        if self.ai_visible() {
            out.push(
                SemanticNode::new(cx.surface(SHELL, parts::AI), Role::Group)
                    .with_label(t(cx.lang, Key::A11yAiPane))
                    .with_bounds(self.ai_rect()),
            );
        }
        out
    }

    fn blocking_semantics(&self, cx: &Shell, blockers: Blockers) -> Vec<SemanticNode> {
        vec![self.blocker_semantics(cx, blockers)]
    }

    /// The column's own frame of reference.
    ///
    /// **`regions.sidebar`, not `sidebar_rect()`.** The settled rect is what a
    /// press is resolved against; the animated one is where the pixels are.
    /// They differ only for the 140 ms of a slide, and of the two a control
    /// must be findable where it *answers*.
    fn explore_context(&self, cx: &Shell) -> ExploreContext {
        ExploreContext {
            window: cx.window,
            view: cx.view,
            area: self.regions.sidebar,
            viewport: cx.viewport,
            lang: cx.lang,
        }
    }

    /// **Which control has the keyboard**, read from the states the focus ring
    /// is already drawn from.
    ///
    /// The order is the window's own: a blocker owns everything beneath it,
    /// then the AI composer, then the explore tree while it holds the arrows,
    /// then the active tab. That is [`MainView::editable_target`]'s order with
    /// the tree added, and it is deliberately not a second router: every arm
    /// asks a predicate that already existed.
    pub(crate) fn accessibility_focus(&self, view: ViewSlot) -> Option<Occurrence> {
        let cx = self.shell(view);
        let blockers = self.blockers();
        if blockers.is_blocking() {
            return Some(self.blocker_focus(&cx, blockers));
        }
        // **A recovery pill `F6` has landed on**, first of the unblocked arms
        // because it is first in `MainView::key` too. It is the one focus in
        // this window that belongs to no blocker: the pill floats with no
        // popover open, so `blockers()` answers `None` and nothing above would
        // ever be asked about it. The predicate is `Surface::focus` holding a
        // `Hit::Failed` — the same state its ring is painted from, never a
        // second field (§7.1).
        if self.reminder_creation.pill_focused()
            && let Some(occurrence) = self.reminder_focus(&cx)
        {
            return Some(occurrence);
        }
        // The AI composer, which sits below every chord and above the tab —
        // the same place `MainView::key` routes it and the same pair
        // `editable_target` asks.
        if self.ai_visible() && self.ai_focused {
            return Some(cx.surface(SHELL, parts::AI));
        }
        if self.sidebar.is_open()
            && let Some(occurrence) = self.explore.accessible_focus(&self.explore_context(&cx))
        {
            return Some(occurrence);
        }
        if let Some(query) = commands::focus(self, &cx) {
            return Some(query);
        }
        // The document itself, when the active tab is one and its editor is
        // what has the keyboard. Read from `ViewState::active` and
        // `Body::focus` — the two states the caret is drawn from, not a second
        // focus beside them.
        if let Some(paragraph) = self.document_focus(&cx) {
            return Some(paragraph);
        }
        // …or the Tasks page, which answers for its own panel, capture row and
        // selected row from the fields it is drawn from.
        if let Some(occurrence) = self.tasks_focus(&cx) {
            return Some(occurrence);
        }
        // …or a canvas, which answers for its dialog, its agent menu and the
        // object selected on it.
        if let Some(occurrence) = self.canvas_focus(&cx) {
            return Some(occurrence);
        }
        // …or the Manage tab, which answers for its nav.
        if let Some(occurrence) = self.manage_focus(&cx) {
            return Some(occurrence);
        }
        // Nothing else in the shell claims the keyboard, so it is the active
        // tab's region — which is where a character would go.
        Some(cx.surface(SHELL, parts::CONTENT))
    }

    /// Where this view draws the window's own three commands.
    ///
    /// Straight out of the widget that draws *and* hit-tests them, against the
    /// settled caption for the reason [`explore_context`](Self::explore_context)
    /// gives.
    pub(crate) fn window_command_bounds(&self) -> CaptionGeometry {
        chrome::window_command_bounds(self)
    }

    /// A number that changes exactly when the tree above would.
    ///
    /// **A fingerprint rather than a counter somebody has to remember to
    /// bump.** Every input to the projection is folded in, so a surface added
    /// later cannot be published without its state being watched — and nothing
    /// that moves per frame is folded in: not the scroll offset, not a
    /// popover's fade, not the caret. That asymmetry is the point.
    /// `Counters::rebuilds` only means something if scrolling and blinking
    /// leave it alone.
    ///
    /// **Only asked while a client is attached** (`PaneAccessibility::
    /// listening`), which is what lets it walk the rows: an ordinary session
    /// never runs this at all, and a screen-reader session pays one pass over
    /// state it is about to publish anyway.
    pub(crate) fn semantics_revision(&self) -> u64 {
        let mut rev = Revision::new();
        self.revise_chrome(&mut rev);
        // **The document, folded in second and separately.** The window's
        // revision decides whether to publish at all; the chrome's half decides
        // whether the publish has to carry the shell. Splitting them is what
        // makes a keystroke one paragraph rather than the whole window — see
        // `document::MainView::chrome_revision`.
        self.document_revision(&mut rev);
        rev.finish()
    }

    /// Everything the window publishes **except** the document body.
    fn revise_chrome(&self, rev: &mut Revision) {
        rev.bool(self.sidebar.is_open());
        rev.bool(self.ai_visible()).bool(self.ai_focused);
        if self.sidebar.is_open() {
            self.explore.revise(rev);
        }
        rev.u64(self.tabs().len() as u64);
        rev.u64(self.tabs().active() as u64);
        for tab in self.tabs().iter() {
            rev.u64(tab.id);
            rev.str(&tab.label);
        }
        // The ordered table the keyboard uses, so the tree and the router
        // cannot disagree about what is on top. Through the projection's own
        // keyword rather than `Debug`, because this runs on the loop and a
        // formatted string per pass is an allocation nothing needs.
        rev.str(menus::blocker_keyword(self.blockers()));
        for menu in [self.menu.as_ref()]
            .into_iter()
            .chain([self.submenu.as_ref().map(|(_, m)| m)])
            .chain([self.nested.as_ref().map(|(_, m)| m)])
        {
            match menu {
                Some(menu) => {
                    rev.u64(menu.instance);
                    rev.u64(menu.hover.map_or(u64::MAX, |i| i as u64));
                    rev.u64(menu.items().len() as u64);
                }
                None => {
                    rev.byte(0);
                }
            }
        }
        // The Tasks page, when it is the tab in front of the person. It is part
        // of the chrome's fingerprint rather than the body's because it is
        // re-sent whole with the chrome: a row ticked on it has to make the
        // window publish, and the body's half only ever answers for a document.
        self.revise_tasks(rev);
        // A canvas in front, for the Tasks page's reason: it is re-sent whole
        // with the chrome.
        self.revise_canvas(rev);
        self.revise_manage(rev);
        // What the Store has confirmed about the document in front of the
        // person, and whether anything about saving has become news.
        self.persistence_revision(rev);
        // …and what it has confirmed about that item's reminders.
        self.reminder_revision(rev);
        commands::revise(self, rev);
        // The window's own size: every published rectangle is relative to it.
        rev.f32(self.viewport.0).f32(self.viewport.1);
    }

    /// Do what a client asked, through the owner that already does it.
    pub(crate) fn accessibility_action(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: Occurrence,
        action: Action,
        data: Option<&ActionData>,
    ) -> bool {
        // **A blocker owns everything beneath it here too.** A request that
        // named a control under an open menu is refused rather than performed,
        // which is the same answer the keyboard gives.
        let blocking = self.blockers().is_blocking();
        if blocking && !menus::is_blocker_target(self, target) {
            return false;
        }
        if menus::action(self, target, action) {
            return true;
        }
        if persistence::action(self, target, action) {
            return true;
        }
        if reminders::action(self, ws, target, action) {
            return true;
        }
        if commands::action(self, ws, target, action) {
            return true;
        }
        if chrome::action(self, ws, target, action) {
            return true;
        }
        // The open document's own paragraphs, before the explore column: both
        // answer a `Model` target, and the document is the one in front of the
        // person.
        if self.document_action(target, action, data) {
            return true;
        }
        // The one document request that writes — a checklist row's box. Its own
        // route because `document_action` deliberately holds no `WorkspaceDoc`;
        // see [`MainView::document_check_action`].
        if self.document_check_action(ws, target, action) {
            return true;
        }
        // The Tasks page, which answers `Model` targets too — its rows *are*
        // task nodes. After the document for the document's reason: only one
        // of the two can be the active tab, and each refuses a target it does
        // not hold.
        if self.tasks_action(target, action) {
            return true;
        }
        // A canvas's objects answer `Model` targets too, for the same reason.
        if self.canvas_action(target, action) {
            return true;
        }
        // The Manage tab's nav, which answers `Model` targets for its Profile
        // rows — scoped by parts no other surface uses.
        if self.manage_action(target, action) {
            return true;
        }
        self.explore_action(ws, target, action)
    }

    // ------------------------------------------------------ the Tasks page
    //
    // Five short delegations, on the explore column's precedent: what the page
    // *is* lives in `tasks_view/accessibility.rs`, and this file supplies the
    // frame of reference and drains the request.

    /// The page's own frame of reference — the content region, in the window's
    /// coordinates. A tab lays out at its own origin and reports in its own
    /// coordinates, so this is the one thing it cannot work out for itself.
    fn tasks_context(&self, cx: &Shell) -> crate::tasks_view::accessibility::Context {
        crate::tasks_view::accessibility::Context {
            window: cx.window,
            view: cx.view,
            area: self.content(),
            lang: cx.lang,
        }
    }

    /// The Tasks page under the content region, when that is the active tab.
    fn tasks_semantics(&self, cx: &Shell) -> Option<SemanticNode> {
        let TabView::Tasks(page) = &self.tabs().active_tab().view else {
            return None;
        };
        Some(page.semantics(&self.tasks_context(cx)))
    }

    /// Where the keyboard is inside the Tasks page, when it is showing.
    fn tasks_focus(&self, cx: &Shell) -> Option<Occurrence> {
        let TabView::Tasks(page) = &self.tabs().active_tab().view else {
            return None;
        };
        Some(page.accessible_focus(&self.tasks_context(cx)))
    }

    /// Fold the Tasks page into the chrome's fingerprint.
    ///
    /// **Only while it is the active tab**, which is the same gate the explore
    /// column's `revise` is under: a page nobody is looking at publishes
    /// nothing, so a change in it must not make the window rebuild.
    fn revise_tasks(&self, rev: &mut Revision) {
        match &self.tabs().active_tab().view {
            TabView::Tasks(page) => {
                rev.byte(1);
                page.revise(rev);
            }
            _ => {
                rev.byte(0);
            }
        }
    }

    /// Do what a client asked of the Tasks page, and drain what it asked for.
    fn tasks_action(&mut self, target: Occurrence, action: Action) -> bool {
        let index = self.tabs().active();
        let Some(TabView::Tasks(page)) = self.tabs_mut().get_mut(index).map(|tab| &mut tab.view)
        else {
            return false;
        };
        page.accessibility_action(target, action)
    }

    // ------------------------------------------------------------ a canvas
    //
    // The Tasks page's four hooks, for a canvas tab (CANVAS P1.6): what the
    // canvas *is* lives in `canvas_view/accessibility.rs`, and this supplies
    // the frame of reference.

    /// The canvas's frame of reference: the focused pane's view rect in the
    /// window's coordinates, and the name its tab shows.
    fn canvas_context(&self, cx: &Shell) -> crate::canvas_view::accessibility::Context {
        crate::canvas_view::accessibility::Context {
            window: cx.window,
            view: cx.view,
            area: self
                .panes
                .view_rect_for(self.content(), self.panes.focused_path())
                .unwrap_or_else(|| self.content()),
            lang: cx.lang,
            name: self.tabs().active_tab().label.clone(),
        }
    }

    /// The canvas under the content region, when that is the active tab.
    fn canvas_semantics(&self, cx: &Shell) -> Option<SemanticNode> {
        let TabView::Canvas(canvas) = &self.tabs().active_tab().view else {
            return None;
        };
        Some(canvas.semantics(&self.canvas_context(cx)))
    }

    /// Where the keyboard is on the canvas, when it is showing.
    fn canvas_focus(&self, cx: &Shell) -> Option<Occurrence> {
        let TabView::Canvas(canvas) = &self.tabs().active_tab().view else {
            return None;
        };
        Some(canvas.accessible_focus(&self.canvas_context(cx)))
    }

    /// Fold the canvas into the chrome's fingerprint — only while it is the
    /// active tab.
    fn revise_canvas(&self, rev: &mut Revision) {
        match &self.tabs().active_tab().view {
            TabView::Canvas(canvas) => {
                rev.byte(1);
                canvas.revise(rev);
            }
            _ => {
                rev.byte(0);
            }
        }
    }

    // ------------------------------------------------------ the Manage tab
    //
    // The same four delegations, for the same reason: what the tab *is* lives
    // in `manage/accessibility.rs`.

    fn manage_context(&self, cx: &Shell) -> crate::manage::accessibility::Context {
        crate::manage::accessibility::Context {
            window: cx.window,
            view: cx.view,
            area: self.content(),
        }
    }

    /// The Manage tab under the content region, when that is the active tab.
    fn manage_semantics(&self, cx: &Shell) -> Option<SemanticNode> {
        let TabView::Manage(page) = &self.tabs().active_tab().view else {
            return None;
        };
        Some(page.semantics(&self.manage_context(cx)))
    }

    fn manage_focus(&self, cx: &Shell) -> Option<Occurrence> {
        let TabView::Manage(page) = &self.tabs().active_tab().view else {
            return None;
        };
        Some(page.accessible_focus(&self.manage_context(cx)))
    }

    /// Fold the Manage tab into the chrome's fingerprint, only while it is in
    /// front.
    fn revise_manage(&self, rev: &mut Revision) {
        match &self.tabs().active_tab().view {
            TabView::Manage(page) => {
                rev.byte(1);
                page.revise(rev);
            }
            _ => {
                rev.byte(0);
            }
        }
    }

    /// Do what a client asked of the canvas in front.
    fn canvas_action(&mut self, target: Occurrence, action: Action) -> bool {
        let TabView::Canvas(canvas) = self.active_view() else {
            return false;
        };
        canvas.accessibility_action(target, action)
    }

    /// Do what a client asked of the Manage tab, and drain what it asked for.
    fn manage_action(&mut self, target: Occurrence, action: Action) -> bool {
        // The request names the view it was published in; the tab's frame of
        // reference is the same one its tree was built against.
        let shell = self.shell(target.view);
        let cx = self.manage_context(&shell);
        let index = self.tabs().active();
        let Some(TabView::Manage(page)) = self.tabs_mut().get_mut(index).map(|tab| &mut tab.view)
        else {
            return false;
        };
        let done = page.accessibility_action(&cx, target, action);
        if done {
            self.drain_manage_profiles_request();
        }
        done
    }

    fn explore_action(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: Occurrence,
        action: Action,
    ) -> bool {
        if !self.sidebar.is_open() {
            return false;
        }
        let area = self.regions.sidebar;
        let viewport = self.viewport;
        let Some(outcome) = self
            .explore
            .accessibility_action(area, target, action, viewport)
        else {
            return false;
        };
        // **The same two lines the key path runs.** An expand or a collapse
        // changed which rows there are, and the container is what notices —
        // see `MainView::key`'s explore-nav arm, which this mirrors exactly
        // rather than reproducing the rebuild itself.
        self.explore_stale = true;
        self.apply(ws, outcome);
        true
    }

    /// The region the active tab draws into.
    ///
    /// A `TabPanel` named by the tab showing in it. Until Plan 1b.3 it had no
    /// children at all and the acceptance record said so; a document tab now
    /// hangs its real body under it, and since Plan 2.1 a Tasks tab hangs its
    /// page. Every other kind of tab — the welcome screen, the workspace
    /// manager, the Trash, a session — is still a named region with nothing
    /// inside, and each of those is somebody else's plan.
    fn content_semantics(&self, cx: &Shell, scope: Scope) -> SemanticNode {
        let tab = self.tabs().active_tab();
        let label = if tab.label.is_empty() {
            t(cx.lang, Key::A11yContent).to_string()
        } else {
            tab.label.clone()
        };
        let mut node = SemanticNode::new(cx.surface(SHELL, parts::CONTENT), Role::TabPanel)
            .with_label(label)
            .with_bounds(self.content());
        match self.body_semantics(cx, scope) {
            Some(body) => node = node.with_children(vec![body]),
            // Not a document: whatever the client held of a document body goes
            // with the container it hung from, so the next one is whole. The
            // Tasks page is re-sent whole with the rest of the chrome, so it
            // needs no such record — `Surfaces` names what it took away by
            // comparing one publish with the last (ADR-343-retiredsurfaces).
            None => {
                self.bodies.borrow_mut().body_withheld(cx);
                if let Some(page) = self
                    .tasks_semantics(cx)
                    .or_else(|| self.manage_semantics(cx))
                {
                    node = node.with_children(vec![page]);
                } else if let Some(canvas) = self.canvas_semantics(cx) {
                    node = node.with_children(vec![canvas]);
                }
            }
        }
        node
    }
}

/// **What the chrome published last time it went out**, so the next one can
/// name what has left the window.
///
/// The body keeps its own record — [`document::Bodies`] — because it is
/// published as a *difference* and knows row by row what it removed. Everything
/// else here is re-sent whole whenever any of it moves, so the honest answer is
/// a set difference between one publish and the last, and it is cheap: one pass
/// over a few dozen nodes, only while a client is attached, and only on a pass
/// that was going to rebuild the chrome anyway.
///
/// **Why it has to exist.** The host's occurrence map is what an action is
/// resolved against, and since Plan 1b.2 made an update a difference that map
/// *accumulates* — it forgets only what a view names as retired. The document
/// named its paragraphs. Nothing named a menu's rows, a closed tab's controls
/// or a column row that folded away, so the map climbed for the life of a
/// session and the host went on offering every id it had ever published. The
/// refusal §5 and ADR-336 both promise — *an action naming an id that is not in
/// the tree currently on screen is refused* — was true of the document and a
/// formality everywhere else. Found by
/// `integration::a_menu_that_closed_is_refused_by_the_host`.
#[derive(Default)]
pub(crate) struct Surfaces {
    /// Every occurrence the last whole chrome carried, the body's rows
    /// excepted.
    published: std::collections::BTreeSet<Occurrence>,
    /// What has gone since the host last drained it.
    retired: Vec<Occurrence>,
}

impl Surfaces {
    /// The chrome has just been built. Anything the last one carried and this
    /// one does not has left the window.
    fn published<'a>(&mut self, tree: impl IntoIterator<Item = &'a SemanticNode>) {
        let mut now = std::collections::BTreeSet::new();
        collect(tree, &mut now);
        self.retired
            .extend(self.published.difference(&now).copied());
        self.published = now;
    }

    fn take_retired(&mut self) -> Vec<Occurrence> {
        std::mem::take(&mut self.retired)
    }

    /// A whole snapshot empties the host's occurrence map outright, so there is
    /// nothing left to take away — and a list nobody drains is a list that
    /// grows for the life of the window.
    fn retirement_is_moot(&mut self) {
        self.retired.clear();
    }
}

/// Every occurrence a chrome publish carries, **stopping at the document
/// body**.
///
/// The paragraphs and pills under `s:body#root` are `document::Bodies`' to
/// account for, and they are not in this tree at all when the body went out as
/// a difference — so a set difference over them would retire the whole document
/// on the first keystroke. The container itself is included, because it does
/// leave the window when a blocker stands over it.
fn collect<'a>(
    nodes: impl IntoIterator<Item = &'a SemanticNode>,
    into: &mut std::collections::BTreeSet<Occurrence>,
) {
    for node in nodes {
        into.insert(node.occurrence);
        if node.occurrence.object == ObjectKey::Surface(document::BODY) {
            continue;
        }
        collect(&node.children, into);
    }
}

/// A `RefCell` for the same reason [`document::State`] is one: `View::semantics`
/// is `&self`, deliberately, and remembering what was published is mutation.
pub(crate) type SurfaceState = std::cell::RefCell<Surfaces>;

/// The object a target names, when it names one of ours.
pub(crate) fn surface_of(target: Occurrence) -> Option<&'static str> {
    match target.object {
        ObjectKey::Surface(name) => Some(name),
        _ => None,
    }
}

/// The instance a target names, when it names one.
pub(crate) fn instance_of(target: Occurrence, kind: &'static str) -> Option<u64> {
    match target.object {
        ObjectKey::Instance { kind: k, id } if k == kind => Some(id),
        _ => None,
    }
}
