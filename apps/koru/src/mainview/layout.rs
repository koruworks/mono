// SPDX-License-Identifier: MPL-2.0

//! Where things are: cutting the window up, and keeping the visible tab laid
//! out at whatever is left over.
//!
//! The arithmetic itself lives in `koru_shell_chrome::regions`; this is the part
//! that knows *which* widths to hand it and what to do once the answer changes.

use std::time::Instant;

use koru_core::schema::WorkspaceDoc;
use koru_draw::geometry::Rect;
use koru_draw::theme;
use koru_shell_chrome::regions;

use super::MainView;
use super::panes;
use super::tab::{Tab, TabView};
use super::tab_rules::KoruTabs;

/// The air between two things parked at the right-hand end of the tab strip —
/// the branch pill and the offline chip — and between the leftmost of them and
/// the tabs, so nothing ends up touching.
const CHIP_GAP: f32 = 6.0;

/// How wide the pane may be dragged.
///
/// The floor is where the tree stops being readable — a name elided to three
/// characters is not a list. The ceiling is where a *column* stops being one:
/// past this the pane is competing with the document rather than serving it,
/// and `regions::solve` would start eating into `CONTENT_MIN_W` on an ordinary
/// laptop screen. It still degrades rather than squeezing, but a limit you
/// cannot usefully reach is not a limit.
///
/// **190, not 180, because the heading's mode icons are a floor too.** Seven of
/// them at a 26 px pitch end at 188 px, and a column narrower than that clips
/// the last one — Canvases, the seventh, was the one that stopped fitting.
/// Socials made eight; `ExploreView::navigator_mode_rects` now narrows the gaps
/// and then the icons to fit, so the floor did not have to move again.
pub(crate) use koru_draw::theme::{SIDEBAR_MAX_W, SIDEBAR_MIN_W};
pub(super) const AI_MIN_W: f32 = 400.0;
pub(super) const AI_MAX_W: f32 = 900.0;

/// How wide the invisible band you can grab is. Straddles the edge, so there is
/// a target either side of the hairline rather than only inside the pane.
const GRIP: f32 = 8.0;

/// **Where a tab that holds no document lives in the explore column.**
///
/// Four tabs answer *show me where this lives* with a **selection rather
/// than a file** — a Tasks or Reminders page is a queue on the rail, a run is
/// a row of the Agents navigator, and a result is a version in the Artifacts
/// column — and all four are one gesture: switch the column to the view the thing lives in,
/// select it there, open the column if it is shut, and leave the keyboard on
/// it. They are one enum and one performer rather than four arms because four
/// arms is how a later one comes to do less than the first.
///
/// Every other tab's place *is* a file, and that is the manifest path
/// underneath [`MainView::reveal_tab_in_explore`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum ColumnPlace {
    /// The queue or list a Tasks page is showing, as the rail names it.
    Task(crate::panes::explore::rail::RailTarget),
    /// A run, whose Agents row is minted from the run's own uuid.
    Run(koru_delegate_core::RunId),
    /// One version of one result, in the Artifacts column.
    Result(koru_delegate_core::ArtifactVersionId),
    /// An AI chat, in the Chats view (AGENTUX I5): `Ctrl+Shift+E` in a chat
    /// tab shows the list with that chat selected and the keyboard on it.
    Chat(koru_ai_ui::ChatId),
}

impl ColumnPlace {
    /// The place, for the tabs that have one. `None` means *this tab's place
    /// is a document*, which is the caller's other path — and it is also what
    /// the chord asks before deciding whether the reveal is the whole gesture.
    pub(super) fn of(view: &TabView) -> Option<Self> {
        match view {
            TabView::Tasks(tasks) => Some(Self::Task(tasks.target())),
            TabView::Reminders(_) => Some(Self::Task(
                crate::panes::explore::rail::RailTarget::Reminders,
            )),
            TabView::Run(run) => run.run().map(Self::Run),
            TabView::Result(result) => result.version().map(Self::Result),
            TabView::AiChat(view) => view.chat_id().map(Self::Chat),
            _ => None,
        }
    }
}

impl MainView {
    /// Re-cut the window.
    ///
    /// Uses the panels' **layout** widths, which are final the instant a column
    /// is toggled — so the content reflows once per toggle rather than once per
    /// animation frame. The sliding is entirely in where a column is *drawn*.
    pub(super) fn resolve_regions(&mut self) {
        let sidebar_w = self.sidebar.layout_width();
        let ai_w = self.ai_panel.layout_width();
        self.regions = regions::solve(
            self.viewport.0,
            self.viewport.1,
            sidebar_w,
            ai_w.max(self.context.layout_width()),
        );
    }

    pub(super) fn content(&self) -> Rect {
        self.regions.content
    }

    /// Window space to the active (focused) view's own space.
    pub(super) fn to_leaf(&self, x: f32, y: f32) -> (f32, f32) {
        let c = self.focused_view_rect();
        (x - c.x, y - c.y)
    }

    /// Header rect of the currently focused tab group.
    pub(super) fn focused_header(&self) -> Rect {
        self.pane_header_rect(self.panes.focused_path())
            .unwrap_or_else(|| self.content())
    }

    /// The part of the title bar tabs may occupy: the caption minus its
    /// furniture — the toggle, the chips, the bell and the window buttons.
    /// This is the window's **exclusion zone** for pane lanes: a top-row pane
    /// shares the title bar, and the share it may draw and hit-test in is its
    /// lane's intersection with this band. Geometric rather than by pane
    /// identity, so a nested or right-hand pane answers for itself.
    pub(super) fn caption_lane(&self) -> Rect {
        let full = koru_shell_chrome::caption::tab_strip_with_leading(
            self.regions.caption,
            super::WINDOW_BUTTONS,
            super::CONTEXT_TOGGLE,
            super::reminder_bell(),
            self.leading(),
        );
        let taken = self
            .branch_pill()
            .map(|pill| pill.x)
            .or_else(|| self.offline_chip().map(|chip| chip.x));
        match taken {
            Some(x) => Rect::new(full.x, full.y, (x - CHIP_GAP - full.x).max(0.0), full.h),
            None => full,
        }
    }

    /// A pane's tab lane, window space, ready to draw and hit-test. A pane at
    /// the top of the window shares the title bar (`PaneTree::lane_rect`), and
    /// yields whatever of it the window chrome occupies (`caption_lane`).
    /// Every other pane's lane is the top of its own rect, untouched.
    pub(super) fn lane_rect_for(&self, path: &panes::PanePath) -> Option<Rect> {
        let content = self.content();
        let (_, leaf) = self
            .panes
            .leaf_rects(content)
            .into_iter()
            .find(|(p, _)| p == path)?;
        let lane = panes::PaneTree::lane_rect(content, leaf);
        if leaf.y != content.y {
            return Some(lane);
        }
        let band = self.caption_lane();
        let x = lane.x.max(band.x);
        let right = (lane.x + lane.w).min(band.x + band.w);
        Some(Rect::new(x, lane.y, (right - x).max(0.0), lane.h))
    }

    fn pane_header_rect(&self, path: &panes::PanePath) -> Option<Rect> {
        self.lane_rect_for(path)
    }

    /// The pane leaf whose bounds contain the point, if any — and, for a point
    /// in the title bar, the top-row pane whose lane it is. A press on another
    /// pane's lane must be able to focus that pane.
    pub(super) fn pane_at(&self, x: f32, y: f32) -> Option<panes::PanePath> {
        let content = self.content();
        self.panes
            .leaf_rects(content)
            .into_iter()
            .find(|(_, rect)| rect.contains(x, y))
            .or_else(|| {
                self.panes
                    .leaf_rects(content)
                    .into_iter()
                    .find(|(_, rect)| panes::PaneTree::lane_rect(content, *rect).contains(x, y))
            })
            .map(|(path, _)| path)
    }

    /// Move focus to the pane that contains the point.
    pub(super) fn focus_pane_at(&mut self, x: f32, y: f32) -> bool {
        let Some(path) = self.pane_at(x, y) else {
            return false;
        };
        if self.panes.focused_path() == &path {
            return false;
        }
        self.active_view().clear_hover();
        self.panes.set_focus(path);
        true
    }

    /// The pane the wheel moves: the one under the pointer, falling back to the
    /// focused pane when the pointer is not over one.
    ///
    /// **The wheel follows the pointer; the keyboard does not.** The wheel
    /// carries no coordinates of its own, so a dispatch through the *focused*
    /// view kept scrolling the pane last clicked. Focusing the hovered pane
    /// would fix that and also hand it typing — with a split open, a pointer
    /// parked over the other half would take the next keystroke, which is not
    /// the convention on this platform. So the wheel gets its own target and
    /// focus stays something a click moves.
    pub(super) fn wheel_pane(&self, x: f32, y: f32) -> panes::PanePath {
        self.pane_at(x, y)
            .filter(|path| self.panes.contains_group(path))
            .unwrap_or_else(|| self.panes.focused_path().clone())
    }

    /// Hit-test the tab-strip header of whichever group is under the pointer.
    pub(super) fn pane_header_hit(
        &mut self,
        x: f32,
        y: f32,
    ) -> Option<(panes::PanePath, panes::HitKind)> {
        for (path, _) in self.panes.leaf_rects(self.content()) {
            let Some(header) = self.lane_rect_for(&path) else {
                continue;
            };
            if !header.contains(x, y) {
                continue;
            }
            let Some(group) = self.panes.group_mut(&path) else {
                continue;
            };
            group.ensure_widths(&mut self.engine);
            if let Some(kind) = group.hit(header, (x, y)) {
                return Some((path, kind));
            }
        }
        None
    }

    /// The band at the pane's right edge that resizes it, and the limits.
    ///
    /// The grip is on the **drawn** edge, because that is the one under your
    /// pointer — during a drag the settled `regions.sidebar` is a frame behind.
    pub(super) fn sidebar_grip(&self) -> Rect {
        if !self.sidebar.is_open() {
            return Rect::new(0.0, 0.0, 0.0, 0.0);
        }
        let drawn = self.sidebar_rect();
        let edge = drawn.x + drawn.w;
        Rect::new(edge - GRIP * 0.5, drawn.y, GRIP, drawn.h)
    }

    pub(super) fn ai_grip(&self) -> Rect {
        if !self.ai_panel.is_open() {
            return Rect::new(0.0, 0.0, 0.0, 0.0);
        }
        let drawn = self.ai_rect();
        let edge = drawn.x;
        Rect::new(edge - GRIP * 0.5, drawn.y, GRIP, drawn.h)
    }

    pub(super) fn set_ai_width(&mut self, w: f32) -> bool {
        let moved = self.ai_panel.set_full(w.clamp(AI_MIN_W, AI_MAX_W));
        if moved {
            self.resolve_regions();
        }
        moved
    }

    pub(super) fn commit_ai_width(&mut self) -> bool {
        if !self.ai_panel.commit_width() {
            return false;
        }
        self.relayout();
        true
    }

    /// Set the pane's width, clamped. Returns whether it moved.
    ///
    /// **Chrome geometry only.** The content keeps the width it was laid out
    /// against until [`commit_sidebar_width`](Self::commit_sidebar_width), which
    /// is what stops a drag paying `reflow` per mouse-move.
    pub(super) fn set_sidebar_width(&mut self, w: f32) -> bool {
        let moved = self.sidebar.set_full(w.clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W));
        if moved {
            self.resolve_regions();
        }
        moved
    }

    /// Let the content catch up, on release. One reflow for the whole drag.
    pub(super) fn commit_sidebar_width(&mut self) -> bool {
        if !self.sidebar.commit_width() {
            return false;
        }
        self.relayout();
        true
    }

    /// Where the explore column is *drawn*, which is not quite where it lives
    /// while it is arriving or leaving.
    pub(super) fn sidebar_rect(&self) -> Rect {
        let r = self.regions.sidebar;
        Rect::new(
            -self.sidebar.slide_offset(),
            r.y,
            self.sidebar.drawn_width(),
            r.h,
        )
    }

    /// The AI pane occupies the same right-hand slot as Context. The two are
    /// mutually exclusive, and both enter from the right with a grip on their
    /// left edge.
    pub(super) fn ai_rect(&self) -> Rect {
        let r = self.regions.context;
        let w = self.ai_panel.drawn_width();
        Rect::new(
            self.viewport.0 - w + self.ai_panel.slide_offset(),
            r.y,
            w,
            r.h,
        )
    }

    /// The area the Explore popover uses as its anchor.
    ///
    /// With the pane open this is its ordinary settled region. With it shut,
    /// the region is deliberately zero-width so the document reclaims all of
    /// the room; the workspace shortcut must not change that layout merely to
    /// give its floating panel an anchor. In that state, use the pane's
    /// remembered width as virtual geometry. Nothing draws or hit-tests the
    /// pane itself there — only the popover uses this rectangle.
    pub(super) fn explore_popover_area(&self) -> Rect {
        if self.regions.sidebar.w > 0.0 {
            self.regions.sidebar
        } else {
            Rect::new(0.0, 0.0, self.sidebar.full(), self.viewport.1)
        }
    }

    /// Where the caption is *drawn*, which is not quite where it lives while the
    /// pane is arriving or leaving.
    ///
    /// The exact mirror of [`sidebar_rect`]: the caption's left edge is the
    /// pane's *drawn* right edge, so the logo, the tabs and the strip travel
    /// with the column rather than teleporting the instant it settles.
    ///
    /// Hit-testing deliberately stays on `regions.caption`, which is settled,
    /// except for the reminder bell's draw/hit/anchor contract.
    /// For the 140 ms of a slide the logo is therefore drawn up to a column's
    /// width from where it is clickable — the same trade the two columns have
    /// always made, for the same duration.
    ///
    /// [`sidebar_rect`]: Self::sidebar_rect
    pub(super) fn drawn_caption(&self) -> Rect {
        let sb = self.sidebar_rect();
        let x = (sb.x + sb.w).max(0.0);
        let cap = self.regions.caption;
        Rect::new(x, cap.y, (self.viewport.0 - x).max(0.0), cap.h)
    }

    /// The same for the context column, which slides in from the right — so its
    /// offset is added rather than subtracted.
    pub(super) fn context_rect(&self) -> Rect {
        let r = self.regions.context;
        let w = self.context.drawn_width();
        Rect::new(
            self.viewport.0 - w + self.context.slide_offset(),
            r.y,
            w,
            r.h,
        )
    }

    /// Lay every visible group's active tab out at its view rect.
    pub(super) fn size_visible_groups(&mut self) {
        let content = self.content();
        let rects = self.panes.leaf_rects(content);
        for (path, rect) in rects {
            let view_rect = panes::PaneTree::view_rect_in(content, rect);
            if let Some(group) = self.panes.group_mut(&path) {
                group.active_mut().ensure_sized(view_rect.w, view_rect.h);
            }
        }
    }

    pub(super) fn active_view(&mut self) -> &mut TabView {
        self.tabs_mut().active_view()
    }

    /// Re-cut the window and lay every visible group's active tab out at its new
    /// width.
    ///
    /// **Once.** The other tabs are marked stale and catch up when they are next
    /// shown, which is what keeps a toggle or a resize O(1) reflows rather than
    /// O(tabs) — `reflow` is parley over every row, and a live resize drag would
    /// otherwise pay it once per tab per frame.
    pub(super) fn relayout(&mut self) {
        #[cfg(test)]
        {
            self.relayouts += 1;
        }
        self.resolve_regions();
        let content = self.content();
        let rects = self.panes.leaf_rects(content);
        for (path, rect) in rects {
            let view_rect = panes::PaneTree::view_rect_in(content, rect);
            if let Some(group) = self.panes.group_mut(&path) {
                let active = group.active();
                for (i, t) in group.iter_tabs_mut().enumerate() {
                    if i != active {
                        t.invalidate_size();
                    }
                }
                group.active_mut().ensure_sized(view_rect.w, view_rect.h);
            }
        }
        // Strip width may have changed (sidebar, window resize) — keep the
        // focused group's active tab fully visible rather than only clamping max scroll.
        let active = self.tabs().active();
        if !self.tab_widths().is_empty() {
            self.ensure_tab_visible(active);
        }
    }

    /// `Ctrl+Shift+E`: **focus the tree, or toggle the pane.**
    ///
    /// Three states rather than two, because "open" and "has the keyboard" are
    /// different things once the tree can be navigated:
    ///
    /// | | |
    /// | --- | --- |
    /// | shut | open it, and land the keyboard on the row you are in |
    /// | open, keyboard in the document | **take the keyboard**, and land it there too |
    /// | open, keyboard in the tree | shut it, and give the keyboard back |
    ///
    /// The middle row is the one that matters: typing in a document with the
    /// pane already open and reaching for `Ctrl+Shift+E` means *put me back in the
    /// tree*, not *hide the tree*. Shutting it was the only thing that could
    /// happen before, which made the chord useless from exactly the place you
    /// would reach for it. Pressed twice it is still a toggle, so the muscle
    /// memory survives.
    ///
    /// **The chord only.** Clicking the logo is unambiguously *hide the pane* —
    /// there is no "put me back" reading of a click on the thing that shuts it
    /// — so that stays [`toggle_sidebar`].
    ///
    /// Falls through to a toggle when there is nothing to focus, or an empty
    /// library would give you a pane that opens and never shuts.
    ///
    /// [`toggle_sidebar`]: Self::toggle_sidebar
    pub(super) fn focus_or_toggle_sidebar(&mut self, ws: &WorkspaceDoc) -> bool {
        let shutting = self.sidebar.is_open() && self.explore.has_focus();
        if !shutting {
            // **A task under the caret is revealed as a task.** The chord means
            // *show me where this lives in the column*, and for a task row the
            // column's answer is the Tasks rail's queue with the row selected
            // — the same path the chip menu's *View in Tasks* takes, which is
            // why that row teaches this chord.
            //
            // With the Tasks view hidden (koru-features) a task row is an
            // ordinary row here, and the chord toggles the column as it does
            // everywhere else.
            let focused_task = match &self.tabs().active_tab().view {
                TabView::Leaf(leaf) if koru_features::tasks() => koru_editor::Editing::body(leaf)
                    .focused()
                    .filter(|row| row.task.is_some())
                    .map(|row| row.id),
                _ => None,
            };
            if let Some(node) = focused_task {
                let revealed = self.reveal_task_in_tasks_view(ws, node);
                // The column opens, as it does for every other reveal: the
                // page came forward, and the rail beside it is where the
                // queue is named.
                if revealed && !self.sidebar.is_open() {
                    let _ = self.sidebar.toggle(Instant::now());
                    self.relayout();
                }
                return revealed;
            }
            // Task and reminder pages are represented by their queue in the
            // rail. Reveal that queue and give it the keyboard.
            if matches!(
                self.tabs().active_tab().view,
                TabView::Tasks(_) | TabView::Reminders(_)
            ) {
                let revealed = self.reveal_tab_in_explore(self.tabs().active());
                if revealed {
                    self.explore.focus_tree(self.regions.sidebar);
                }
                return revealed;
            }
            // **A tab whose place in the column is a selection reveals, and
            // that is the whole gesture.** There is nothing left for the
            // fall-through to do, and two ways it would do harm: `focus_tree`
            // focuses the *document* tree, which is not what is on screen in
            // any of these four modes; and the reveal has already put the
            // keyboard in the column, so the fall-through would find it
            // focused and toggle the pane shut on the press that opened it.
            if ColumnPlace::of(&self.tabs().active_tab().view).is_some() {
                return self.reveal_tab_in_explore(self.tabs().active());
            }
            let _ = self.reveal_tab_in_explore(self.tabs().active());
        }
        if self.sidebar.is_open()
            && !self.explore.has_focus()
            && self.explore.focus_tree(self.regions.sidebar)
        {
            return true;
        }
        self.toggle_sidebar()
    }

    /// Switch to the tab's home workspace (if needed), open Explore, and scroll
    /// the document into view. Shared by Ctrl+Shift+E and the tab menu.
    ///
    /// **Reveal is deferred** through [`MainView::pending_explore_reveal`]: a
    /// workspace switch is a pane request, so the tree rebuilds only after the
    /// shell swaps; selecting against the *old* tree is wiped on rebuild.
    pub(super) fn reveal_tab_in_explore(&mut self, i: usize) -> bool {
        let Some(tab) = self.tabs().get(i) else {
            return false;
        };
        // **A tab whose place in the column is a selection is revealed there**
        // — and refusing them was the whole behaviour this replaced: the one
        // shortcut for *show me where this tab lives in the column* did
        // nothing on exactly the tabs whose place is not a file. See
        // [`ColumnPlace`] for the four of them and why they are one thing.
        if let Some(place) = ColumnPlace::of(&tab.view) {
            return self.reveal_in_column(place);
        }
        let Some(document) = tab.document else {
            return false;
        };
        let home = tab.home_workspace();
        // **A canvas lives in the Canvases section**, and a page in Documents:
        // revealing either in the other would select a row that is not there.
        let canvas = matches!(tab.view, TabView::Canvas(_));
        if canvas {
            self.explore.show_canvases();
        } else if self.explore.navigator_mode() == crate::panes::explore::NavigatorMode::Canvases {
            self.explore.show_documents();
        }
        // Manifest ids are the same UUID as the document id; Explore selects by
        // that node id.
        let node = koru_core::NodeId::from_uuid(document.as_uuid());
        let _ = home; // may be wrong (Ctrl+click background open stamps current)
        self.pending_explore_reveal = Some(node);
        // Open the pane immediately so the switch has somewhere visible to land.
        if !self.sidebar.is_open() {
            let _ = self.sidebar.toggle(Instant::now());
            self.relayout();
        }
        // **Always locate.** `Tab::home` can be the window's workspace at the
        // moment a background tab was minted (Ctrl+click a cross-workspace
        // pill), not where the file lives. Navigate resolves the real home and
        // switches when needed; reveal applies once that workspace's tree is
        // rebuilt.
        self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document,
                node: None,
            },
            match_text: None,
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        });
        true
    }

    /// **Perform a [`ColumnPlace`]**: the view, the selection, the column, and
    /// the keyboard — in that order, because the pane's rect is not its new one
    /// until the window has been re-cut and every landing row is scrolled to
    /// against that rect.
    ///
    /// Always `true`: the column came forward showing the view the thing lives
    /// in, which is the half of the gesture that never fails. The selection can
    /// fail — a catalog that has not answered yet holds no row for the version
    /// — and the keyboard then lands on the column's first row rather than
    /// nowhere, so the pane still answers the arrows.
    pub(super) fn reveal_in_column(&mut self, place: ColumnPlace) -> bool {
        match place {
            ColumnPlace::Task(target) => {
                self.explore.show_tasks();
                self.explore.rail_mut().set_selected(Some(target));
            }
            ColumnPlace::Run(run) => {
                // The Agents row's node id is the run's own uuid — the same
                // one `rebuild_agent_rows` mints.
                let node = koru_core::NodeId::from_uuid(run.as_uuid());
                self.explore.show_agents();
                self.explore.set_selected(Some(node));
                self.explore.reveal(node);
            }
            // The artifacts column and the Chats view select *and* scroll in
            // one call, which is why that happens after the re-cut rather
            // than here.
            ColumnPlace::Result(_) => {}
            ColumnPlace::Chat(_) => self.explore.show_chats(),
        }
        if !self.sidebar.is_open() {
            let _ = self.sidebar.toggle(Instant::now());
        }
        self.relayout();
        self.explore_stale = true;
        let landed = match place {
            ColumnPlace::Result(version) => {
                self.explore.reveal_artifact(version, self.regions.sidebar)
            }
            // A chat the list has not answered with yet is remembered, and
            // the keyboard lands on it when it arrives — so it is not a miss
            // to paper over with the first row.
            ColumnPlace::Chat(chat) => {
                self.explore.reveal_chat(chat, self.regions.sidebar);
                true
            }
            // The rail owns its own rows, so `focus_tree` is a no-op on a Tasks
            // column and the selection above is the whole of it.
            ColumnPlace::Task(_) | ColumnPlace::Run(_) => false,
        };
        if !landed {
            self.explore.focus_tree(self.regions.sidebar);
        }
        true
    }

    /// Open Explore, select and scroll to [`Self::pending_explore_reveal`].
    pub(super) fn apply_pending_explore_reveal(&mut self) {
        let Some(node) = self.pending_explore_reveal.take() else {
            return;
        };
        if !self.sidebar.is_open() {
            let _ = self.sidebar.toggle(Instant::now());
            self.relayout();
        }
        self.explore.set_selected(Some(node));
        self.explore.reveal(node);
        self.explore_stale = true;
        self.explore.focus_tree(self.regions.sidebar);
        // Correct home after a successful reveal (overwrites a wrong stamp).
        let home = self.current_workspace();
        if let Some(tab) = self.tabs_mut().iter_mut().find(|t| {
            t.document
                .is_some_and(|d| koru_core::NodeId::from_uuid(d.as_uuid()) == node)
        }) {
            tab.home = home;
        }
    }

    /// Open or shut the pane. What the logo and the toggle button do.
    pub(super) fn toggle_sidebar(&mut self) -> bool {
        let now = Instant::now();
        // **The fold** (`theme::motion::pane_fold`): the panel's own slide,
        // reversed from where it is on interrupt, with the contents' fade
        // and the chain's ride clocked from here. Under reduced motion both
        // states land at once — the panel has no zero-duration path, so the
        // host takes the other door it already has.
        let open = if self.reduced_motion {
            let open = !self.sidebar.is_open();
            self.sidebar.set_immediately(open);
            self.fold = None;
            open
        } else {
            let open = self.sidebar.toggle(now);
            self.fold = Some((now, open));
            open
        };
        super::chrome_debug::chrome_debug!(
            "pane {} (width {}{})",
            if open { "opening" } else { "closing" },
            self.sidebar.full(),
            if self.reduced_motion {
                ", reduced motion"
            } else {
                ""
            }
        );
        // Closing the pane closes what was hanging off it: a popover left
        // floating over the content with nothing to anchor it to is a panel
        // from nowhere.
        self.explore.close_popover();
        self.explore_stale = true;
        // Before focusing, not after: the pane's rect is not its new one until
        // the window has been re-cut, and the landing row is scrolled to.
        self.relayout();
        // A hidden list must not keep the keyboard.
        if open {
            self.explore.focus_tree(self.regions.sidebar);
        } else {
            self.explore.clear_focus();
        }
        true
    }

    /// The fold's clock stops when the panel's slide does. Separate from the
    /// tick so a test can drive the panel by hand and still see it settle.
    pub(super) fn settle_fold(&mut self) {
        if self.fold.is_some() && !self.sidebar.animating() {
            super::chrome_debug::chrome_debug!(
                "pane fold settled {}",
                if self.sidebar.is_open() {
                    "open"
                } else {
                    "shut"
                }
            );
            self.fold = None;
        }
    }

    /// How opaque the pane's contents are this frame: 1 at rest, fading out
    /// over a collapse's first 90 ms and in over an expand's last 90 ms.
    pub(super) fn fold_content_alpha(&self) -> f32 {
        match self.fold {
            Some((started, opening)) if self.sidebar.animating() => {
                theme::motion::pane_fold().content_alpha(started.elapsed(), opening)
            }
            _ => 1.0,
        }
    }

    /// Whether the caption's leading chain is drawn this frame: while the
    /// pane is shut, and through the fold either way — it rides the drawn
    /// pane's edge out to the corner and back (frame 9's blue note).
    pub(super) fn chain_drawn(&self) -> bool {
        !self.sidebar.is_open() || (self.fold.is_some() && self.sidebar.animating())
    }

    /// Show the workspaces switcher without changing the Explore pane.
    ///
    /// When the pane is shut, [`explore_popover_area`](Self::explore_popover_area)
    /// gives the floating panel virtual anchor geometry while the document
    /// keeps all of its width. The popover has the arrows, and `popover_key` is
    /// asked before `nav_key` on every press.
    pub(super) fn open_workspaces_popover(&mut self) -> bool {
        super::chrome_debug::chrome_debug!("who/where popover opened on workspaces");
        self.explore
            .open_popover(crate::panes::explore::PopoverKind::Workspaces)
    }

    /// Show the profiles switcher without changing the Explore pane.
    pub(super) fn open_profiles_popover(&mut self) -> bool {
        super::chrome_debug::chrome_debug!("who/where popover opened on profiles");
        self.explore
            .open_popover(crate::panes::explore::PopoverKind::Profiles)
    }

    pub(super) fn toggle_context(&mut self) -> bool {
        let now = Instant::now();
        let opening = !self.context.is_open();
        if opening {
            self.ai_panel.set_immediately(false);
            self.ai_focused = false;
        }
        self.context.toggle(now);
        self.relayout();
        true
    }

    pub(super) fn toggle_ai(&mut self) -> bool {
        // The one door the pane opens by; a build that hides Koru AI has no
        // pane to open (koru-features).
        if !koru_features::ai() {
            return false;
        }
        let now = Instant::now();
        let opening = !self.ai_panel.is_open();
        if opening {
            self.context.set_immediately(false);
            // A reopened pane should re-offer the current document even if the
            // user removed its pill while the pane was previously open.
            self.ai_chat.clear_context_document_dismissal();
        }
        let open = self.ai_panel.toggle(now);
        self.ai_focused = open;
        if open {
            self.ai_chat.activate();
        }
        self.relayout();
        true
    }

    /// Open Koru AI as a new main tab, leaving the right-hand pane alone.
    ///
    /// Used by the caption `+` menu's *Chats ▸ New* row. The tab starts a
    /// fresh empty chat and attaches workspace context when it is drawn.
    pub(super) fn open_ai_chat_tab(&mut self) -> bool {
        if !koru_features::ai() {
            return false;
        }
        self.tabs_mut().push(Tab::ai_chat());
        true
    }

    pub(super) fn socials_navigation_deadline(&self) -> Option<Instant> {
        if !self.sidebar.is_open() || !self.explore.showing_socials() {
            return None;
        }
        let Some(surface) = self.explore.socials() else {
            return Some(Instant::now());
        };
        if matches!(self.tabs().active_tab().view, TabView::Socials(_)) {
            return None;
        }
        surface.borrow().maintenance_deadline()
    }

    pub(super) fn maintain_socials_navigation(&mut self, now: Instant) -> bool {
        if !self.sidebar.is_open() || !self.explore.showing_socials() {
            return false;
        }
        let mut changed = false;
        if self.explore.socials().is_none() {
            changed |= self.open_socials_tab();
        }
        let surface = self.explore.ensure_socials(crate::search::gateway_root());
        surface.borrow_mut().set_context_workspace(
            self.current_workspace()
                .map(|id| koru_store_core::WorkspaceId::from_uuid(id.as_uuid())),
        );
        if !matches!(self.tabs().active_tab().view, TabView::Socials(_)) {
            changed |= surface.borrow_mut().maintain(now);
        }
        changed
    }

    /// Focus the existing Socials surface or open it in this tab group.
    ///
    /// **The one door a Socials tab opens by** — the `+` row, Explore's
    /// Socials mode and its chord, *New social post* — so `ENABLE_SOCIALS`
    /// is answered here once (koru-features). The Agent's publisher is not
    /// a door and runs either way (ADR-450).
    pub(super) fn open_socials_tab(&mut self) -> bool {
        if !koru_features::socials() {
            return false;
        }
        let surface = self.explore.ensure_socials(crate::search::gateway_root());
        surface.borrow_mut().set_context_workspace(
            self.current_workspace()
                .map(|id| koru_store_core::WorkspaceId::from_uuid(id.as_uuid())),
        );
        self.explore.show_socials();
        if !self.sidebar.is_open() {
            self.toggle_sidebar();
        }
        if self.sidebar.full() < 280. {
            self.set_sidebar_width(280.);
            self.commit_sidebar_width();
        }
        let showing = self.panes.iter_groups().find_map(|(path, group)| {
            group
                .tabs()
                .iter()
                .position(|tab| matches!(tab.view, TabView::Socials(_)))
                .map(|index| (path, index))
        });
        if let Some((path, index)) = showing {
            self.panes.set_focus(path);
            let _ = self.select(index);
        } else {
            self.tabs_mut().push(Tab::socials(surface));
        }
        true
    }

    /// Only ordinary text documents can seed a social post.
    pub(super) fn social_source_title(&self, document: koru_core::DocumentId) -> Option<String> {
        self.entries
            .as_ref()?
            .iter()
            .find(|entry| entry.id == document && entry.kind == koru_core::explore::KIND_DOCUMENT)
            .map(|entry| entry.title.clone())
    }

    pub(super) fn social_post_from_document(&mut self, document: koru_core::DocumentId) -> bool {
        if !koru_features::socials() {
            return false;
        }
        let Some(title) = self.social_source_title(document) else {
            return false;
        };
        let Some(workspace) = self.current_workspace() else {
            return false;
        };
        self.open_socials_tab();
        if let TabView::Socials(view) = self.tabs_mut().active_view() {
            view.import_document(
                koru_store_core::WorkspaceId::from_uuid(workspace.as_uuid()),
                koru_store_core::DocumentId::from_uuid(document.as_uuid()),
                title,
            );
        }
        true
    }

    /// Refuse window and workspace changes while a Socials composer holds edits.
    pub(super) fn socials_window_close_veto(&mut self) -> bool {
        let blocked = self.panes.iter_groups_mut().find_map(|(path, group)| {
            group
                .tabs_mut()
                .iter_mut()
                .enumerate()
                .find_map(|(index, tab)| {
                    if let TabView::Socials(view) = &mut tab.view {
                        view.request_close().then_some((path.clone(), index))
                    } else {
                        None
                    }
                })
        });
        if let Some((path, index)) = blocked {
            self.panes.set_focus(path);
            self.select(index);
            true
        } else {
            false
        }
    }

    /// Keep unsaved Socials edits visible when a tab-close action is refused.
    pub(super) fn socials_close_veto(&mut self, targets: impl Fn(usize) -> bool) -> bool {
        let blocked = self
            .tabs_mut()
            .iter_mut()
            .enumerate()
            .find_map(|(index, tab)| {
                if targets(index)
                    && !tab.is_pinned()
                    && let TabView::Socials(view) = &mut tab.view
                {
                    view.request_close().then_some(index)
                } else {
                    None
                }
            });
        if let Some(index) = blocked {
            self.select(index);
            true
        } else {
            false
        }
    }

    /// Open a recent chat as a tab, focusing one already showing it.
    pub(super) fn open_ai_chat_tab_for(&mut self, id: koru_ai_ui::ChatId) -> bool {
        if !koru_features::ai() {
            return false;
        }
        let showing = self.panes.iter_groups().find_map(|(path, group)| {
            group
                .tabs()
                .iter()
                .position(
                    |tab| matches!(&tab.view, TabView::AiChat(view) if view.chat_id() == Some(id)),
                )
                .map(|index| (path, index))
        });
        if let Some((path, index)) = showing {
            self.panes.set_focus(path);
            let _ = self.select(index);
            return true;
        }
        self.tabs_mut().push(Tab::ai_chat_existing(id));
        true
    }

    /// Open a recent document, focusing a tab that already shows it.
    pub(super) fn open_recent_document(&mut self, document: koru_core::DocumentId) -> bool {
        if self.focus_showing_document(document) {
            return true;
        }
        self.request_store_document_here(koru_core::NodeId::from_uuid(document.as_uuid()), true)
    }

    /// The band the tabs run in. Shared by drawing and hit-testing, like every
    /// other piece of chrome geometry.
    ///
    /// The focused group's tab-strip header. Every tab-strip geometry call site
    /// now operates on the focused pane's header rather than the window caption.
    pub(super) fn strip(&self) -> Rect {
        self.focused_header()
    }

    /// The window-caption tab strip, independent of the per-pane header. Kept
    /// for tests that verify caption geometry.
    #[cfg(test)]
    pub(super) fn caption_strip(&self) -> Rect {
        let full = koru_shell_chrome::caption::tab_strip_with_leading(
            self.regions.caption,
            super::WINDOW_BUTTONS,
            super::CONTEXT_TOGGLE,
            super::reminder_bell(),
            self.leading(),
        );
        let taken = self
            .branch_pill()
            .map(|pill| pill.x)
            .or_else(|| self.offline_chip().map(|chip| chip.x));
        match taken {
            Some(x) => Rect::new(full.x, full.y, (x - CHIP_GAP - full.x).max(0.0), full.h),
            None => full,
        }
    }

    /// Where the branch pill sits, when this build came off a branch worth
    /// naming at all.
    ///
    /// **Left of the offline chip when both are up.** The chip is the one that
    /// says something is wrong, so it keeps the end of the band; a pill that
    /// jumped position depending on whether the store was reachable would be
    /// worse than either order.
    ///
    /// `None` on `dev` and `main`, and in every released build — see
    /// [`koru_shell_chrome::branch::worth_showing`].
    pub(super) fn branch_pill(&self) -> Option<Rect> {
        self.branch_pill_for(crate::build_stamp::caption_branch(), self.branch_pill_w)
    }

    /// Split from [`branch_pill`](Self::branch_pill) so both arms are reachable
    /// in a test. This suite runs on whatever branch somebody is on — usually
    /// `dev`, where there is no pill at all — and the arrangement worth pinning
    /// is the one with a pill *and* an offline chip up, which no test could
    /// otherwise reach without a rebuild on another branch.
    pub(super) fn branch_pill_for(&self, name: Option<&str>, w: f32) -> Option<Rect> {
        name?;
        if w <= 0.0 {
            return None;
        }
        let full = koru_shell_chrome::caption::tab_strip_with_leading(
            self.regions.caption,
            super::WINDOW_BUTTONS,
            super::CONTEXT_TOGGLE,
            super::reminder_bell(),
            self.leading(),
        );
        let band = match self.offline_chip() {
            Some(chip) => Rect::new(
                full.x,
                full.y,
                (chip.x - CHIP_GAP - full.x).max(0.0),
                full.h,
            ),
            None => full,
        };
        (band.w > 0.0).then(|| koru_shell_chrome::branch::pill(band, w))
    }

    /// Where the offline chip sits, if there is anything to say.
    ///
    /// At the right-hand end of the tab strip, immediately before the buttons —
    /// so it is beside the window controls a person already looks at, and it
    /// pushes the tabs rather than covering them.
    ///
    /// `None` while the store is reachable, which is the overwhelming majority
    /// of the time and costs the strip nothing.
    pub(super) fn offline_chip(&self) -> Option<Rect> {
        self.store_trouble.as_ref()?;
        let full = koru_shell_chrome::caption::tab_strip_with_leading(
            self.regions.caption,
            super::WINDOW_BUTTONS,
            super::CONTEXT_TOGGLE,
            super::reminder_bell(),
            self.leading(),
        );
        Some(koru_shell_chrome::status_chip::chip(full))
    }

    /// Remeasure tab preferred widths when labels or fonts changed.
    ///
    /// Only when widths actually change is the active tab scrolled into view —
    /// hit-testing calls this often, and must not yank the strip back while the
    /// user is browsing tabs with the wheel.
    pub(super) fn ensure_tab_widths(&mut self) {
        if !self.tab_widths_stale() && self.tab_widths().len() == self.tabs().len() {
            return;
        }
        self.remeasure_tab_widths();
        self.ensure_tab_visible(self.tabs().active());
    }

    pub(super) fn clamp_tab_scroll(&mut self) {
        let strip = self.strip();
        self.set_tab_scroll(koru_shell_chrome::tabs::clamp_scroll(
            strip,
            &self.tab_widths(),
            self.tab_scroll(),
        ));
    }

    /// Scroll the strip so tab `i` is fully visible: left edge in view if it
    /// was off to the left, right edge if it was off to the right.
    pub(super) fn ensure_tab_visible(&mut self, i: usize) {
        if self.tab_widths_stale() || self.tab_widths().len() != self.tabs().len() {
            self.remeasure_tab_widths();
        }
        let strip = self.strip();
        self.set_tab_scroll(koru_shell_chrome::tabs::ensure_visible(
            strip,
            &self.tab_widths(),
            self.tab_scroll(),
            i,
        ));
    }

    fn remeasure_tab_widths(&mut self) {
        let pin_count = self.tabs().pinned_count();
        // Borrowed before `self.tabs()`: the accessor borrows all of `self`,
        // and the map closure below still needs the note set.
        let note_documents = &self.note_documents;
        let tab_data: Vec<(bool, bool, bool, String)> = self
            .tabs()
            .iter()
            .map(|t| {
                let pinned = t.is_pinned();
                // A note tab draws the file glyph even with no leading emoji
                // (see the icon chain in `draw`), so its width has to agree.
                let has_icon =
                    t.strip_shows_icon() || t.document.is_some_and(|d| note_documents.contains(&d));
                (pinned, has_icon, t.show_title, t.label.clone())
            })
            .collect();
        let widths: Vec<f32> = tab_data
            .into_iter()
            .enumerate()
            .map(|(i, (pinned, has_icon, show_title, label))| {
                let label_ref = if pinned && !show_title { "" } else { &label };
                let w = koru_shell_chrome::tabs::preferred_width(
                    &mut self.engine,
                    label_ref,
                    pinned || has_icon,
                    !pinned,
                );
                if pin_count > 0 && i + 1 == pin_count {
                    w + theme::TAB_PIN_GAP
                } else {
                    w
                }
            })
            .collect();
        *self.tab_widths_mut() = widths;
        self.branch_pill_w = match crate::build_stamp::caption_branch() {
            Some(name) => koru_shell_chrome::branch::width(&mut self.engine, name),
            None => 0.0,
        };
        self.set_tab_widths_stale(false);
        self.clamp_tab_scroll();
    }

    pub(super) fn mark_tab_widths_stale(&mut self) {
        self.set_tab_widths_stale(true);
    }

    /// Resolve a window-space point to a chrome hit. Every pane has its own
    /// lane — inside the content rect for most, in the shared title bar for
    /// the top row — so the shared chrome resolver answers only the caption's
    /// furniture (it is called with empty tab widths) and the lanes answer for
    /// themselves below.
    pub(super) fn hit_at(&mut self, x: f32, y: f32) -> koru_shell_chrome::regions::Hit {
        // Ask the shared chrome resolver with empty tab widths so the old caption
        // strip never answers with tab hits. Any tab-like answer there is just the
        // drag window area.
        let hit = koru_shell_chrome::regions::hit(
            &self.regions,
            &[],
            0.0,
            super::WINDOW_BUTTONS,
            super::CONTEXT_TOGGLE,
            self.leading(),
            super::reminder_bell(),
            false,
            x,
            y,
        );
        let hit = match hit {
            // The bell opts out of the caption's historical 140 ms settled-hit
            // trade: its popover anchor, paint and hit rectangle are one drawn
            // geometry authority even while Explore is sliding.
            koru_shell_chrome::regions::Hit::HostAction
                if !self.reminder_bell_rect().contains(x, y) =>
            {
                koru_shell_chrome::regions::Hit::Caption
            }
            koru_shell_chrome::regions::Hit::Tab(_)
            | koru_shell_chrome::regions::Hit::TabClose(_)
            | koru_shell_chrome::regions::Hit::NewTab
            | koru_shell_chrome::regions::Hit::TabScroll => {
                koru_shell_chrome::regions::Hit::Caption
            }
            other => other,
        };
        // Pane lanes are asked for the content area and for the caption's drag
        // area alike: a top-row pane's lane *is* the title bar there, and the
        // caption's own furniture (window buttons, toggle, bell) has already
        // answered for itself above. What no lane claims stays `Caption` — the
        // window's drag area — or `Content`.
        use koru_shell_chrome::regions::Hit;
        if hit != Hit::Content && hit != Hit::Caption {
            return hit;
        }
        if let Some((path, kind)) = self.pane_header_hit(x, y) {
            return match kind {
                panes::HitKind::Tab(i) => Hit::Tab(i),
                panes::HitKind::TabClose(i)
                    if self
                        .group_at(&path)
                        .tabs()
                        .get(i)
                        .is_some_and(|t| t.is_pinned()) =>
                {
                    Hit::Tab(i)
                }
                panes::HitKind::TabClose(i) => Hit::TabClose(i),
                panes::HitKind::NewTab => Hit::NewTab,
                panes::HitKind::TabScroll => Hit::TabScroll,
                panes::HitKind::Strip => Hit::Caption,
            };
        }
        hit
    }

    /// Darker thumb while the pointer is on it (or dragging) — same cue as the
    /// document scrollbar. Lighter when the strip shows the bar but the pointer
    /// is only over a tab label.
    pub(super) fn tab_scrollbar_active_at(&self, x: f32, y: f32) -> bool {
        if self.tab_scroll_drag().is_some() {
            return true;
        }
        let strip = self.strip();
        if strip.w <= 0.0 || !strip.contains(x, y) {
            return false;
        }
        koru_shell_chrome::tabs::scroll_thumb(strip, &self.tab_widths(), self.tab_scroll())
            .is_some_and(|t| t.contains(x, y))
    }

    /// Advance everything that is mid-slide — both columns and the tab strip.
    /// Called from `draw`, because the frame the shell just asked for *is* the
    /// tick: `animating` is why it asked.
    pub(super) fn advance_panels(&mut self, ws: &WorkspaceDoc) {
        let now = Instant::now();
        self.sidebar.advance(now);
        self.settle_fold();
        self.ai_panel.advance(now);
        self.context.advance(now);
        self.slides_mut().advance(now);
        // The pane's popover keeps its own fade, because it is the pane's
        // furniture — but it is ticked from here so every animation in the
        // window runs off one `Instant` and cannot drift apart across a frame.
        // The same goes for the row slides, the drag's auto-scroll and its
        // spring-open (which needs the document to read the opened children).
        self.explore.advance_popover(now);
        self.reminder_shell.advance(now);
        self.explore.advance_drag(ws, now);
    }

    /// How far the wheel should move a tree row. Lines are what a mouse sends;
    /// a trackpad sends pixels already.
    pub(super) fn wheel_lines(delta: winit::event::MouseScrollDelta) -> f32 {
        match delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => y * theme::NAV_ROW_H,
            winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32,
        }
    }
}

#[cfg(test)]
mod tests;
