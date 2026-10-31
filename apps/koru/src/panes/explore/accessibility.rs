// SPDX-License-Identifier: MPL-2.0

//! **What the explore column offers a screen reader, and what a request to one
//! of its controls means.**
//!
//! One sentence, and what it excludes: *this module turns the pane's already
//! prepared state into semantic nodes, and turns a request back into the same
//! call the keyboard makes.* It excludes what a control **is** — a row, a
//! fold, a mode, a popover row are [`super::ExploreView`]'s, and stay there —
//! and it excludes composition: where the column sits inside a window, which
//! surface is on top of it and what the tabs are doing belong to the shell
//! (`mainview/accessibility`). This is a child module rather than a section of
//! `explore.rs` for the reason §7.10 gives: the pane is 6,000 lines and gets a
//! call, not another embedded subsystem.
//!
//! ## Three rules it is built on
//!
//! **Geometry is never re-derived.** Every rectangle published here comes back
//! from the function drawing and hit-testing already share —
//! [`row_band`](super::ExploreView::row_band),
//! [`navigator_mode_rects`](super::ExploreView::navigator_mode_rects),
//! [`header_rects`](super::ExploreView::header_rects),
//! [`footer_rects`](super::ExploreView::footer_rects),
//! [`popover_rects`](super::ExploreView::popover_rects). A second copy of the
//! arithmetic is a control a client can find somewhere other than where it is.
//!
//! **Offscreen rows are published anyway.** `row_rects` culls to the visible
//! band because that is what drawing and hit-testing want; a screen reader
//! wants the opposite. Every row in the cache gets a node, at the rectangle it
//! *would* occupy, and [`accesskit::Action::ScrollIntoView`] on one reveals it
//! through the pane's own `scroll_into_view` — the roadmap's *"a navigation
//! request can reveal the target using existing scroll behaviour"*.
//!
//! **An action is a keystroke.** [`ExploreView::accessibility_action`] moves
//! the pane's own focus and then hands the equivalent key to
//! [`nav_key`](super::ExploreView::nav_key). There is no second open route, no
//! second fold route and no second focus state: the ring a sighted person sees
//! *is* what a client is told, because both read `ExploreView::focus`.

use accesskit::{Action, Role};
use koru_core::NodeId;
use koru_draw::geometry::Rect;
use koru_i18n::{Key, Lang, t};
use koru_workspace_host::accessibility::{
    ControlPart, Occurrence, Revision, SemanticNode, ViewSlot, WindowSlot,
};

use super::{ExploreView, NavigatorMode, PopoverKind, PopoverRow};
use crate::panes::Outcome;

/// The object every surface of this pane that stands for no model row belongs
/// to. One name, so a locator reads `.../s:explore#tree` and a person can tell
/// at a glance which pane a control came from.
const EXPLORE: &str = "explore";
/// The four projection buttons. A separate object so a mode's own keyword can
/// be the *part* — `s:explore-mode#documents` — which is what keeps the
/// identity of the Documents button the same in every language.
const MODE: &str = "explore-mode";
/// The open header popover. Its own object, so its rows sit beside the column's
/// rather than inside it.
const POPOVER: &str = "explore-popover";

/// The parts of the column that are not rows.
///
/// Keywords, chosen by the code. A part is never a label and never an index:
/// §5's *reordering, renaming, translating labels or scrolling must not change
/// the identity of a surviving control*.
pub(crate) mod parts {
    use koru_workspace_host::accessibility::ControlPart;

    /// The column itself.
    pub const COLUMN: ControlPart = ControlPart::new("column");
    /// The header's two halves.
    /// The strip of projection buttons.
    pub const MODES: ControlPart = ControlPart::new("modes");
    /// The heading band's title: the app's name.
    pub const HEADING: ControlPart = ControlPart::new("heading");
    /// The library heading's `…` and magnifier.
    pub const ACTIONS: ControlPart = ControlPart::new("actions");
    /// The tree of rows, and what stands in its place when there are none.
    pub const TREE: ControlPart = ControlPart::new("tree");
    pub const EMPTY: ControlPart = ControlPart::new("empty");
    pub const EMPTY_ACTION: ControlPart = ControlPart::new("empty-action");
    /// The pinned floor row.
    pub const TRASH: ControlPart = ControlPart::new("trash");
    pub const FLOOR_CHIP: ControlPart = ControlPart::new("floor-chip");
    /// The `…` right of the chip: the workspace's menu.
    pub const FLOOR_MENU: ControlPart = ControlPart::new("floor-menu");
    /// The rail's foot: Help, Settings and the profile square.
    pub const RAIL_SETTINGS: ControlPart = ControlPart::new("rail-settings");
    pub const RAIL_HELP: ControlPart = ControlPart::new("rail-help");
    pub const RAIL_PROFILE: ControlPart = ControlPart::new("rail-profile");
    /// The open popover, and the rows of it that name no object.
    pub const POPOVER: ControlPart = ControlPart::new("popover");
    /// The Profiles page's foot: its `+`, its devices and its sync state.
    pub const NEW_PROFILE: ControlPart = ControlPart::new("new-profile");
    pub const PROFILE_DEVICES: ControlPart = ControlPart::new("profile-devices");
    pub const PROFILE_STATUS: ControlPart = ControlPart::new("profile-status");
    /// The chip's page's heading `+`, and its import button.
    pub const NEW_WORKSPACE: ControlPart = ControlPart::new("new-workspace");
    /// A workspace's or Profile's gear: the Manage tab on it
    /// (ELEMENTS.final D2). The row's own part stays `root`.
    pub const MANAGE: ControlPart = ControlPart::new("manage");
}

/// Everything the pane needs from the window it is drawn in, which is nothing
/// it could work out for itself.
///
/// `area` is the column's rect **in window-logical coordinates**, and it is the
/// same one the pane is hit-tested against (`MainView::regions.sidebar`) rather
/// than the animated one it is drawn at: a control has to be findable where it
/// answers a press. The 140 ms of a slide is the only time they differ.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Context {
    pub window: WindowSlot,
    pub view: ViewSlot,
    /// The pane: the nav and its floor, after the rail.
    pub area: Rect,
    /// The rail beside it, under the lane.
    pub rail: Rect,
    /// The whole window, which the popover clamps itself inside.
    pub viewport: (f32, f32),
    pub lang: Lang,
}

impl Context {
    fn surface(&self, name: &'static str, part: ControlPart) -> Occurrence {
        Occurrence::surface(self.window, self.view, name, part)
    }

    fn model(&self, id: uuid::Uuid, part: ControlPart) -> Occurrence {
        Occurrence::model(self.window, self.view, id, part)
    }
}

/// The name of the projection now showing, which is also the tree's own name.
/// The mode's name in `lang`: the same word the apps row draws, so a screen
/// reader and the chip cannot disagree.
fn mode_label(mode: NavigatorMode, lang: Lang) -> &'static str {
    mode.label_in(lang)
}

impl ExploreView {
    /// The column, as a screen reader sees it right now.
    ///
    /// `&self` throughout: nothing here lays out, folds, scrolls or reads the
    /// Store, and the signature is what makes that true rather than a comment.
    pub(crate) fn semantics(&self, cx: &Context) -> SemanticNode {
        let mut children = Vec::new();
        children.push(self.modes_semantics(cx));
        children.extend(self.heading_semantics(cx));
        if self.showing_home() {
            children.push(self.home_nav.semantics(cx, self.rows_top(cx.area)));
        } else if self.showing_tasks() {
            children.push(self.rail.queue_semantics(cx, self.rows_top(cx.area)));
        } else if self.showing_chats() {
            // The Chats view replaces the tree, on the rail's model: a list
            // of rows that are chats, each with Open and Delete.
            children.push(chats::semantics(&self.chats, cx, self.rows_top(cx.area)));
        } else if !self.showing_socials() {
            children.push(self.tree_semantics(cx));
        }
        children.extend(self.footer_semantics(cx));
        children.extend(self.rail_foot_semantics(cx));
        children.extend(self.floor_semantics(cx));
        children.extend(self.popover_semantics(cx));
        SemanticNode::new(cx.surface(EXPLORE, parts::COLUMN), Role::Group)
            .with_label(t(cx.lang, Key::A11yNavigator))
            .with_bounds(cx.area)
            .with_children(children)
    }

    /// **Everything this column's tree is built from**, folded into the
    /// shell's revision.
    ///
    /// What is deliberately *absent* is everything that moves without changing
    /// what is published: [`scroll`](Self::scroll) — a scrolled row keeps its
    /// identity, its label and its place in the set; the popover's `progress`
    /// and `fade`; the caret turns in `turns`; every hover that is not also
    /// the keyboard's position. Scrolling moves *rectangles*, and a screen
    /// reader asks for a rectangle when it wants one rather than being told;
    /// rebuilding the tree for it is the per-frame cost §7.3 forbids.
    pub(crate) fn revise(&self, rev: &mut Revision) {
        rev.str(self.mode.token());
        if self.showing_tasks() {
            self.rail.revise_queues(rev);
        }
        if self.showing_chats() {
            chats::revise(&self.chats, rev);
        }
        rev.opt_uuid(self.focused().map(|node| node.as_uuid()));
        rev.opt_uuid(self.selected().map(|node| node.as_uuid()));
        rev.u64(self.rows.len() as u64);
        for row in &self.rows {
            rev.uuid(row.node.as_uuid());
            rev.str(&row.label);
            rev.u64(u64::from(row.depth));
            rev.bool(row.has_children);
            rev.bool(self.fold_set_ref().contains(&row.node));
        }
        match self.live() {
            Some(popover) => {
                rev.byte(1);
                rev.str(match popover.kind {
                    PopoverKind::Summary => "summary",
                    PopoverKind::Profiles => "profiles",
                    PopoverKind::LibraryActions => "library",
                });
                rev.u64(popover.hover.map_or(u64::MAX, |i| i as u64));
                // Right onto a gear moves the focus without moving the row.
                rev.bool(self.popover_part == Some(super::PopoverPart::Gear));
                for row in self.popover_rows() {
                    rev.str(&self.popover_row_label(row, Lang::En));
                }
            }
            None => {
                rev.byte(0);
            }
        }
        rev.bool(self.footer_active.is_some());
    }

    /// **Which row the keyboard is on**, said as an occurrence.
    ///
    /// Read straight off [`focus`](Self::focused) — the same field the focus
    /// ring is drawn from — so §7.1's *visible focus and accessible focus share
    /// the same state* is structural. An open popover owns the arrows while it
    /// is up, so it answers first.
    pub(crate) fn accessible_focus(&self, cx: &Context) -> Option<Occurrence> {
        if let Some(popover) = self.live() {
            let rows = self.popover_rows();
            let hovered = popover.hover.and_then(|i| rows.get(i).copied());
            return Some(match hovered {
                // The keyboard on a workspace's or Profile's gear is on the
                // gear (ELEMENTS.final D2): its node, not the row's.
                Some(PopoverRow::Workspace(i))
                    if self.popover_part == Some(super::PopoverPart::Gear) =>
                {
                    match self.workspaces.get(i) {
                        Some(w) => cx.model(w.id.as_uuid(), parts::MANAGE),
                        None => cx.surface(POPOVER, parts::POPOVER),
                    }
                }
                Some(PopoverRow::Profile(i))
                    if self.popover_part == Some(super::PopoverPart::Gear) =>
                {
                    match self.profiles.get(i) {
                        Some(profile) => cx.model(profile.id.as_uuid(), parts::MANAGE),
                        None => cx.surface(POPOVER, parts::POPOVER),
                    }
                }
                Some(row) => self.popover_row_occurrence(cx, row),
                None => cx.surface(POPOVER, parts::POPOVER),
            });
        }
        if self.showing_tasks() {
            return self.rail.queue_focus_occurrence(cx);
        }
        if self.showing_chats() {
            return chats::focus_occurrence(&self.chats, cx);
        }
        let node = self.focused()?;
        Some(cx.model(node.as_uuid(), ControlPart::ROOT))
    }

    /// **Do what a client asked, through the key the keyboard already uses.**
    ///
    /// `None` means *this pane does not perform that action on that control*,
    /// which is the honest answer to the `Focus` Windows sends ahead of every
    /// invoke on a control that is not a row.
    pub(crate) fn accessibility_action(
        &mut self,
        area: Rect,
        target: Occurrence,
        action: Action,
        viewport: (f32, f32),
    ) -> Option<Outcome> {
        use winit::keyboard::{Key as WKey, NamedKey};

        if let Some(outcome) = self.popover_accessibility_action(target, action, area, viewport) {
            return Some(outcome);
        }
        if let Some(outcome) = self.chrome_accessibility_action(target, action, area) {
            return Some(outcome);
        }
        // Tasks replaces the document tree. A retained document-row request
        // must not fall through to hidden library content in this mode.
        if self.showing_tasks() {
            let top = self.rows_top(area);
            return self.rail.queue_action(area, top, target, action);
        }
        // Home's nav replaces the tree the same way; a press is the row's.
        if self.showing_home() {
            return match action {
                Action::Click => self.home_nav.accessibility_press(target),
                Action::Focus | Action::ScrollIntoView => Some(Outcome::Consumed),
                _ => None,
            };
        }
        // Chats replaces the tree the same way, and a request for a hidden
        // document row must not reach the library behind it either.
        if self.showing_chats() {
            let top = self.rows_top(area);
            return chats::action(&mut self.chats, area, top, target, action);
        }
        // A row. Everything below reaches `nav_key`, so there is exactly one
        // implementation of open, fold and step.
        let node = self.row_of(target)?;
        let i = self.rows.iter().position(|row| row.node == node)?;
        match action {
            // **The press sets the focus; the release opens.** Matching the
            // pointer exactly, which matters because Windows sends `Focus`
            // immediately before `Invoke` and the pair must behave like a
            // click rather than like two unrelated commands.
            Action::Focus => {
                self.focus = Some(node);
                self.scroll_into_view(area, i);
                Some(Outcome::Consumed)
            }
            Action::Click => {
                self.focus = Some(node);
                self.nav_key(area, &WKey::Named(NamedKey::Enter))
            }
            Action::Expand => {
                let row = self.rows.get(i)?;
                if !row.has_children || !self.fold_set_ref().contains(&node) {
                    return None;
                }
                self.focus = Some(node);
                self.nav_key(area, &WKey::Named(NamedKey::ArrowRight))
            }
            Action::Collapse => {
                let row = self.rows.get(i)?;
                if !row.has_children || self.fold_set_ref().contains(&node) {
                    return None;
                }
                self.focus = Some(node);
                self.nav_key(area, &WKey::Named(NamedKey::ArrowLeft))
            }
            // Reveal without opening and without focusing: the pane's own
            // scrolling, which is the only kind there is.
            Action::ScrollIntoView => {
                self.scroll_into_view(area, i);
                Some(Outcome::Consumed)
            }
            _ => None,
        }
    }

    fn modes_semantics(&self, cx: &Context) -> SemanticNode {
        let rects = self.navigator_mode_rects(cx.rail);
        let total = rects.len();
        let children = rects
            .iter()
            .enumerate()
            .map(|(i, (mode, rect))| {
                SemanticNode::new(
                    // The mode's stored token as the part: a keyword the code
                    // chose, already asserted stable because a session file
                    // holds it.
                    cx.surface(MODE, ControlPart::new(mode.token())),
                    Role::Tab,
                )
                .with_label(mode_label(*mode, cx.lang))
                .with_bounds(*rect)
                .with_action(Action::Click)
                // A `Tab` in a `TabList` is what selection is *for*: a client
                // reaches it through `SelectionItem`, which the adapter turns
                // straight back into the `Click` above. "Not selected" is
                // worth saying about a projection button, so both values are
                // published.
                .with_selected(*mode == self.mode)
                .with_position(i)
            })
            .collect();
        // **A container needs a rectangle too.** The first thing anyone does
        // with a UIA inspector is hover, and a node with no bounds has a
        // zero-size one — present in the tree and unreachable with a pointer or
        // with spatial navigation. The strip's band is the span of the buttons
        // in it; nothing is re-derived.
        let band =
            rects
                .iter()
                .map(|(_, rect)| *rect)
                .fold(Rect::new(0.0, 0.0, 0.0, 0.0), |acc, rect| {
                    if acc.w <= 0.0 {
                        return rect;
                    }
                    let x = acc.x.min(rect.x);
                    let y = acc.y.min(rect.y);
                    let right = (acc.x + acc.w).max(rect.x + rect.w);
                    let bottom = (acc.y + acc.h).max(rect.y + rect.h);
                    Rect::new(x, y, right - x, bottom - y)
                });
        SemanticNode::new(cx.surface(EXPLORE, parts::MODES), Role::TabList)
            .with_label(t(cx.lang, Key::A11yNavigatorViews))
            .with_bounds(band)
            // On the container, because that is where an item's total is read
            // from -- see `SemanticNode::size_of_set`.
            .with_size_of_set(total)
            .with_children(children)
    }

    /// The library heading's two buttons. `…` is absent in a mode that offers
    /// no actions, and its rect says so by being zero-wide — the same idiom the
    /// pane's own hit test reads.
    fn heading_semantics(&self, cx: &Context) -> Vec<SemanticNode> {
        let mut out = Vec::new();
        // The band's title says which app the rows below belong to, as the
        // drawn heading does (the CTO's note).
        let title = self.heading_title_rect(cx.area);
        if title.w > 0.0 {
            out.push(
                SemanticNode::new(cx.surface(EXPLORE, parts::HEADING), Role::Heading)
                    .with_label(self.mode.label_in(cx.lang))
                    .with_bounds(title),
            );
        }
        let actions = self.actions_button(cx.area);
        if actions.w > 0.0 {
            // The Chats heading's button is *New chat* (the CTO's note).
            let label = if self.mode == NavigatorMode::Chats {
                Key::ChatsNew
            } else {
                Key::A11yMoreActions
            };
            out.push(
                SemanticNode::new(cx.surface(EXPLORE, parts::ACTIONS), Role::Button)
                    .with_label(t(cx.lang, label))
                    .with_bounds(actions)
                    .with_action(Action::Click),
            );
        }
        out
    }

    // --------------------------------------------------------------- the tree

    /// The rows, or the explanation standing in for them.
    ///
    /// **Every row, not the visible ones.** See the module note: culling is for
    /// drawing.
    fn tree_semantics(&self, cx: &Context) -> SemanticNode {
        // The band the rows scroll inside, from the pane's own two edges — so
        // a hover between two rows lands on the tree rather than on nothing.
        let top = self.rows_top(cx.area);
        let band = Rect::new(
            cx.area.x,
            top,
            cx.area.w,
            (self.rows_bottom(cx.area) - top).max(0.0),
        );
        let tree = SemanticNode::new(cx.surface(EXPLORE, parts::TREE), Role::Tree)
            .with_label(mode_label(self.mode, cx.lang))
            .with_bounds(band);
        if self.rows.is_empty() {
            return tree.with_children(self.empty_semantics(cx));
        }
        let total = self.rows.len();
        let children = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let occurrence = cx.model(row.node.as_uuid(), ControlPart::ROOT);
                let mut node = SemanticNode::new(occurrence, Role::TreeItem)
                    .with_label(row.label.clone())
                    .with_bounds(self.row_band(cx.area, i))
                    // `depth` is zero at the top of the column; a reader counts
                    // from one.
                    .with_level(row.depth as usize + 1)
                    .with_position(i)
                    .with_actions([Action::Click, Action::Focus, Action::ScrollIntoView]);
                // **`Some(true)` on the open row, and nothing on the rest.**
                // `selected` here means *this is the document the active tab
                // shows*, and AccessKit's own guidance is to omit the flag
                // wherever "not selected" would be noise -- which it is for a
                // library row. It is also load-bearing: a node carrying
                // `selected` loses its Invoke pattern, so publishing `false`
                // everywhere would leave a client unable to open *any* row.
                // The one row that does carry it is the one already open,
                // which is the one nothing needs to open.
                if self.selected == Some(row.node) {
                    node = node.with_selected(true);
                }
                if row.has_children {
                    // The fold actions are routed and tested. They are not
                    // reachable from UIA today -- 0.32.1 has no ExpandCollapse
                    // pattern -- and `expanded` is deliberately *not* set,
                    // because setting it would take Invoke away and give
                    // nothing back. Left advertised so that the day the
                    // adapter grows the pattern, the routing is already here.
                    node = node.with_action(if self.fold_set_ref().contains(&row.node) {
                        Action::Expand
                    } else {
                        Action::Collapse
                    });
                }
                node
            })
            .collect();
        tree.with_size_of_set(total).with_children(children)
    }

    /// Why the column is empty, and the way out of it.
    ///
    /// The pane already draws both; what is added here is that the sentence is
    /// localized. The drawn copy is still English — existing debt this plan is
    /// not authorised to respell — so the two can differ in a non-English
    /// session. Recorded in `docs/features/accessibility/README.md`.
    fn empty_semantics(&self, cx: &Context) -> Vec<SemanticNode> {
        let (hint, action) = self.empty_accessible_names(cx.lang);
        let mut out = vec![
            SemanticNode::new(cx.surface(EXPLORE, parts::EMPTY), Role::Label)
                .with_label(hint)
                .with_bounds(self.heading_band(cx.area)),
        ];
        // The button is only there once a frame has measured it, which is also
        // exactly when it can be pressed.
        if let (Some(action), Some(rect)) = (action, self.empty_cta) {
            out.push(
                SemanticNode::new(cx.surface(EXPLORE, parts::EMPTY_ACTION), Role::Button)
                    .with_label(t(cx.lang, action))
                    .with_bounds(rect)
                    .with_action(Action::Click),
            );
        }
        out
    }

    /// The localized twin of [`empty_state`](Self::empty_state), which decides
    /// the same three cases in the same order.
    fn empty_accessible_names(&self, lang: Lang) -> (&'static str, Option<Key>) {
        match self.mode {
            NavigatorMode::Home => (t(lang, Key::HomeTitle), None),
            NavigatorMode::Tasks | NavigatorMode::Agents => (t(lang, Key::A11yEmptySessions), None),
            NavigatorMode::Artifacts => (t(lang, Key::ArtifactsEmpty), None),
            NavigatorMode::Chats => (t(lang, Key::ChatsEmpty), None),
            NavigatorMode::Socials => (
                koru_i18n::socials::t(lang, koru_i18n::socials::Key::Title),
                None,
            ),
            _ if self.current.is_none() => {
                (t(lang, Key::A11yEmptyWorkspace), Some(Key::CreateWorkspace))
            }
            NavigatorMode::Files => (t(lang, Key::A11yEmptyFiles), Some(Key::A11yAddFiles)),
            NavigatorMode::Canvases => (t(lang, Key::CanvasesEmpty), Some(Key::CanvasNew)),
            NavigatorMode::Documents => (
                t(lang, Key::A11yEmptyDocuments),
                Some(Key::A11yCreateDocument),
            ),
        }
    }

    fn footer_semantics(&self, cx: &Context) -> Vec<SemanticNode> {
        self.footer_rects(cx.area)
            .into_iter()
            .map(|(row, rect)| {
                let super::FooterRow::Trash = row;
                SemanticNode::new(cx.surface(EXPLORE, parts::TRASH), Role::Button)
                    .with_label(t(cx.lang, Key::WorkspacesTrash))
                    .with_bounds(rect)
                    .with_action(Action::Click)
            })
            .collect()
    }

    /// The occurrence of a chrome control `F6` has the keyboard on, for the
    /// window's `accessibility_focus`: a chip by its mode's token, the rest by
    /// their parts.
    pub(crate) fn chrome_focus_occurrence(
        &self,
        cx: &Context,
        focus: super::ExploreChromeFocus,
    ) -> Option<Occurrence> {
        use super::ExploreChromeFocus;
        Some(match focus {
            ExploreChromeFocus::App(i) => {
                let mode = *NavigatorMode::visible().get(i)?;
                cx.surface(MODE, ControlPart::new(mode.token()))
            }
            ExploreChromeFocus::RailSettings => cx.surface(EXPLORE, parts::RAIL_SETTINGS),
            ExploreChromeFocus::RailHelp => cx.surface(EXPLORE, parts::RAIL_HELP),
            ExploreChromeFocus::RailProfile => cx.surface(EXPLORE, parts::RAIL_PROFILE),
            ExploreChromeFocus::FloorChip => cx.surface(EXPLORE, parts::FLOOR_CHIP),
        })
    }

    /// The rail's foot: Help, Settings and the profile square, each a
    /// button with its name.
    fn rail_foot_semantics(&self, cx: &Context) -> Vec<SemanticNode> {
        let r = self.apps_rail_rects(cx.rail);
        if r.band.w <= 0.0 {
            return Vec::new();
        }
        let person = self
            .profile()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| t(cx.lang, Key::TerminalSwitchProfile).to_string());
        vec![
            SemanticNode::new(cx.surface(EXPLORE, parts::RAIL_HELP), Role::Button)
                .with_label(t(cx.lang, Key::Help))
                .with_bounds(r.help)
                .with_action(Action::Click),
            SemanticNode::new(cx.surface(EXPLORE, parts::RAIL_SETTINGS), Role::Button)
                .with_label(t(cx.lang, Key::SettingsTab))
                .with_bounds(r.settings)
                .with_action(Action::Click),
            SemanticNode::new(cx.surface(EXPLORE, parts::RAIL_PROFILE), Role::Button)
                .with_label(format!("{person}, {}", t(cx.lang, Key::FloorChipAction)))
                .with_bounds(r.profile)
                .with_action(Action::Click),
        ]
    }

    /// The floor: the who/where chip, named by where and who, with its
    /// action as the description.
    fn floor_semantics(&self, cx: &Context) -> Vec<SemanticNode> {
        let r = crate::panes::sidebar::floor::solve(cx.area);
        if r.chip.w <= 0.0 {
            return Vec::new();
        }
        let workspace = self
            .current
            .and_then(|id| self.workspaces.iter().find(|w| w.id == id))
            .map(|w| w.name.clone())
            .unwrap_or_else(|| t(cx.lang, Key::TerminalSwitchWorkspace).to_string());
        let person = self
            .profile()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| t(cx.lang, Key::TerminalSwitchProfile).to_string());
        vec![
            SemanticNode::new(cx.surface(EXPLORE, parts::FLOOR_CHIP), Role::Button)
                // The name says where and who; the Click action a client
                // already has is what pressing does (`FloorChipAction` is
                // the tooltip's sentence).
                .with_label(format!("{workspace}, {person}"))
                .with_bounds(r.chip)
                .with_action(Action::Click),
            // The `…` beside it: the workspace's menu (the CTO's artboard).
            SemanticNode::new(cx.surface(EXPLORE, parts::FLOOR_MENU), Role::Button)
                .with_label(t(cx.lang, Key::A11yMoreActions))
                .with_bounds(r.dots)
                .with_action(Action::Click),
        ]
    }

    // ------------------------------------------------------------ the popover

    /// The open header list, if one is taking input.
    ///
    /// [`live`](Self::live) rather than `popover.is_some()`, which is the same
    /// distinction every other caller makes: a popover fading out must not
    /// answer for anything, and publishing it would offer a client a control
    /// that is on its way off the screen.
    fn popover_semantics(&self, cx: &Context) -> Option<SemanticNode> {
        let popover = self.live()?;
        // The switcher's pages publish their element frames: the Menu, its
        // heading, each row's primary beside its gear (P4, P5), and the
        // Profiles page's three foot items.
        if popover.kind != PopoverKind::LibraryActions {
            return self
                .with_page(cx.area, cx.viewport, |frame| {
                    self.element_semantics(cx, frame)
                })
                .and_then(|mut roots| roots.pop());
        }
        let panel = self.popover_panel(cx.area, cx.viewport)?;
        let rects = self.popover_rects(cx.area, cx.viewport);
        let mut children = Vec::with_capacity(rects.len());
        for (row, rect) in rects.iter().filter(|(row, _)| row.selectable()) {
            children.push(
                SemanticNode::new(self.popover_row_occurrence(cx, *row), Role::MenuItem)
                    .with_label(self.popover_row_label(*row, cx.lang))
                    .with_bounds(*rect)
                    .with_actions([Action::Click, Action::Focus]),
            );
        }
        let size = children.len();
        for (index, child) in children.iter_mut().enumerate() {
            *child = std::mem::replace(
                child,
                SemanticNode::new(cx.surface(POPOVER, parts::POPOVER), Role::MenuItem),
            )
            .with_position(index);
        }
        Some(
            SemanticNode::new(cx.surface(POPOVER, parts::POPOVER), Role::Menu)
                .with_size_of_set(size)
                .with_label(t(cx.lang, Key::A11yMoreActions))
                .with_bounds(panel)
                .with_children(children),
        )
    }

    /// **A popover row's identity is the thing it names.**
    ///
    /// A workspace row *is* that workspace, so it carries the workspace's own
    /// id and survives the list being reordered, renamed or reopened. The fixed
    /// rows carry a keyword. Neither is a position, and neither is a label.
    fn popover_row_occurrence(&self, cx: &Context, row: PopoverRow) -> Occurrence {
        match row {
            PopoverRow::Workspace(i) => match self.workspaces.get(i) {
                Some(w) => cx.model(w.id.as_uuid(), ControlPart::ROOT),
                None => cx.surface(POPOVER, parts::POPOVER),
            },
            PopoverRow::Profile(i) => match self.profiles.get(i) {
                Some(p) => cx.model(p.id.as_uuid(), ControlPart::ROOT),
                None => cx.surface(POPOVER, parts::POPOVER),
            },
            // A fixed row's buttons publish themselves; the row is what
            // Enter presses on it.
            PopoverRow::WorkspacesHeading => cx.surface(POPOVER, parts::NEW_WORKSPACE),
            PopoverRow::ProfilesHeading => cx.surface(POPOVER, parts::POPOVER),
            PopoverRow::ProfilesFooter => cx.surface(POPOVER, parts::NEW_PROFILE),
            PopoverRow::LibraryAction(action) => {
                cx.surface(POPOVER, ControlPart::new(super::actions::token(action)))
            }
        }
    }

    /// Which of a fixed row's buttons an accessibility target names.
    fn head_button_of(part: ControlPart) -> Option<super::HeadButton> {
        if part == parts::NEW_PROFILE {
            Some(super::HeadButton::New)
        } else if part == parts::PROFILE_DEVICES {
            Some(super::HeadButton::Devices)
        } else if part == parts::PROFILE_STATUS {
            Some(super::HeadButton::Status)
        } else {
            None
        }
    }

    fn popover_row_label(&self, row: PopoverRow, lang: Lang) -> String {
        match row {
            PopoverRow::Workspace(i) => self
                .workspaces
                .get(i)
                .map(|w| w.name.clone())
                .unwrap_or_default(),
            PopoverRow::Profile(i) => self
                .profiles
                .get(i)
                .map(|p| p.name.clone())
                .unwrap_or_default(),
            PopoverRow::WorkspacesHeading => t(lang, Key::TerminalNewWorkspace).to_string(),
            PopoverRow::ProfilesHeading => t(lang, Key::WorkspacesProfiles).to_string(),
            PopoverRow::ProfilesFooter => t(lang, Key::NewProfile).to_string(),
            PopoverRow::LibraryAction(action) => {
                super::actions::label(action, self.mode).to_string()
            }
        }
    }

    /// Route a request at an open popover through the same two keys its own
    /// keyboard handler answers: move the highlight, then `Enter`.
    fn popover_accessibility_action(
        &mut self,
        target: Occurrence,
        action: Action,
        _area: Rect,
        viewport: (f32, f32),
    ) -> Option<Outcome> {
        use winit::keyboard::{Key as WKey, NamedKey};
        let _ = (_area, viewport);
        self.live()?;
        let rows = self.popover_rows();
        let index = rows
            .iter()
            .position(|row| row.selectable() && self.popover_row_matches(*row, target))?;
        // **A request for the gear acts on the gear** (ELEMENTS.final D2):
        // Focus lights it without choosing anything, Click opens Manage on
        // that row, whatever the keyboard was on.
        if target.part == parts::MANAGE
            && matches!(
                rows[index],
                PopoverRow::Workspace(_) | PopoverRow::Profile(_)
            )
        {
            return match action {
                Action::Focus => {
                    self.popover.as_mut()?.hover = Some(index);
                    self.popover_part = Some(super::PopoverPart::Gear);
                    Some(Outcome::Consumed)
                }
                Action::Click => {
                    self.popover.as_mut()?.hover = Some(index);
                    Some(self.press(rows[index], super::PopoverPart::Gear))
                }
                _ => None,
            };
        }
        let popover = self.popover.as_mut()?;
        match action {
            // The highlight the arrows move — the popover's own keyboard
            // state, not a second one.
            Action::Focus => {
                popover.hover = Some(index);
                // The row itself, not its gear: the keyboard's lit part.
                self.popover_part = None;
                Some(Outcome::Consumed)
            }
            // Enter, except that a fixed row's other buttons are not what
            // Enter presses there: each is pressed as itself.
            Action::Click => {
                popover.hover = Some(index);
                if let Some(button) = Self::head_button_of(target.part)
                    && matches!(rows[index], PopoverRow::ProfilesFooter)
                {
                    return Some(self.press(rows[index], super::PopoverPart::Button(button)));
                }
                self.popover_key(&WKey::Named(NamedKey::Enter))
            }
            _ => None,
        }
    }

    fn popover_row_matches(&self, row: PopoverRow, target: Occurrence) -> bool {
        match row {
            PopoverRow::Workspace(i) => self
                .workspaces
                .get(i)
                .is_some_and(|w| target.object == object_of(w.id.as_uuid())),
            PopoverRow::Profile(i) => self
                .profiles
                .get(i)
                .is_some_and(|p| target.object == object_of(p.id.as_uuid())),
            PopoverRow::WorkspacesHeading => target.part == parts::NEW_WORKSPACE,
            PopoverRow::ProfilesHeading => false,
            PopoverRow::ProfilesFooter => {
                target.part == parts::NEW_PROFILE
                    || target.part == parts::PROFILE_DEVICES
                    || target.part == parts::PROFILE_STATUS
            }
            PopoverRow::LibraryAction(action) => {
                target.part.name() == super::actions::token(action)
            }
        }
    }

    // ------------------------------------------------------------ the chrome

    /// The column's own buttons. Each one reaches the outcome its press
    /// already produces; nothing here is a second command.
    fn chrome_accessibility_action(
        &mut self,
        target: Occurrence,
        action: Action,
        area: Rect,
    ) -> Option<Outcome> {
        if action != Action::Click {
            return None;
        }
        let surface = match target.object {
            koru_workspace_host::accessibility::ObjectKey::Surface(name) => name,
            _ => return None,
        };
        match surface {
            MODE => {
                let (mode, _) = self
                    .navigator_mode_rects(self.rail_of(area))
                    .into_iter()
                    .find(|(mode, _)| mode.token() == target.part.name())?;
                // The same call the press makes, two lines below the popover
                // toggles in `mouse_down_with_modifiers`.
                self.set_navigator_mode(mode);
                Some(if mode == NavigatorMode::Socials {
                    Outcome::OpenSocials
                } else {
                    Outcome::Consumed
                })
            }
            EXPLORE => match target.part {
                p if p == parts::FLOOR_CHIP => {
                    self.toggle_popover(PopoverKind::Summary);
                    Some(Outcome::Consumed)
                }
                p if p == parts::FLOOR_MENU => Some(self.floor_menu(area)),
                p if p == parts::RAIL_SETTINGS => Some(Outcome::OpenSettings),
                p if p == parts::RAIL_HELP => Some(Outcome::OpenHelp),
                p if p == parts::RAIL_PROFILE => {
                    self.toggle_profile_popover();
                    Some(Outcome::Consumed)
                }
                p if p == parts::ACTIONS => {
                    self.toggle_popover(PopoverKind::LibraryActions);
                    Some(Outcome::Consumed)
                }
                p if p == parts::TRASH => Some(Outcome::OpenTrash),
                p if p == parts::EMPTY_ACTION => Some(self.empty_state().2),
                _ => None,
            },
            _ => None,
        }
    }

    /// The row a request is about, when it is about one.
    fn row_of(&self, target: Occurrence) -> Option<NodeId> {
        match target.object {
            koru_workspace_host::accessibility::ObjectKey::Model(id) => {
                let node = NodeId::from_uuid(id);
                self.rows.iter().any(|row| row.node == node).then_some(node)
            }
            _ => None,
        }
    }
}

impl ExploreView {
    /// An element frame's accessibility tree as this window's nodes: each
    /// element key becomes the occurrence it names — a model's part, or a
    /// surface's — and a Menu's items carry their place in the set.
    // Read by the element pages' tests until P4 publishes from it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn element_semantics(
        &self,
        cx: &Context,
        frame: &koru_draw::el::Frame,
    ) -> Vec<SemanticNode> {
        frame
            .semantics()
            .iter()
            .map(|node| element_node(cx, node))
            .collect()
    }
}

// Read by the element pages' tests until P4 publishes from it.
#[cfg_attr(not(test), allow(dead_code))]
fn element_occurrence(cx: &Context, key: koru_draw::el::Key) -> Occurrence {
    let part = ControlPart::new(key.part);
    match key.object {
        koru_draw::el::Object::Surface(name) => cx.surface(name, part),
        koru_draw::el::Object::Model(id) => cx.model(uuid::Uuid::from_u128(id), part),
    }
}

// Read by the element pages' tests until P4 publishes from it.
#[cfg_attr(not(test), allow(dead_code))]
fn element_node(cx: &Context, node: &koru_draw::el::SemNode) -> SemanticNode {
    use koru_draw::el;
    let role = match node.role {
        el::Role::Menu => Role::Menu,
        el::Role::MenuItem => Role::MenuItem,
        el::Role::Heading => Role::Heading,
        el::Role::Button => Role::Button,
        el::Role::Group => Role::Group,
        el::Role::List => Role::List,
        el::Role::ListItem => Role::ListItem,
        el::Role::Image => Role::Image,
    };
    let occurrence = match node.key {
        Some(key) => element_occurrence(cx, key),
        None => cx.surface(POPOVER, parts::POPOVER),
    };
    let mut out = SemanticNode::new(occurrence, role).with_bounds(node.bounds);
    match &node.name {
        Some(el::Name::Key(key)) => out = out.with_label(t(cx.lang, *key)),
        Some(el::Name::Text(content)) => out = out.with_label(content.clone()),
        None => {}
    }
    if let Some(selected) = node.selected {
        out = out.with_selected(selected);
    }
    out = out.with_actions(node.actions.iter().map(|action| match action {
        el::Action::Click => Action::Click,
        el::Action::Focus => Action::Focus,
    }));
    let items = node
        .children
        .iter()
        .filter(|child| child.role == el::Role::MenuItem)
        .count();
    let mut position = 0;
    let children = node
        .children
        .iter()
        .map(|child| {
            let built = element_node(cx, child);
            if node.role == el::Role::Menu && child.role == el::Role::MenuItem {
                position += 1;
                built.with_position(position - 1)
            } else {
                built
            }
        })
        .collect();
    if node.role == el::Role::Menu {
        out = out.with_size_of_set(items);
    }
    out.with_children(children)
}

fn object_of(id: uuid::Uuid) -> koru_workspace_host::accessibility::ObjectKey {
    koru_workspace_host::accessibility::ObjectKey::Model(id)
}

mod chats;

#[cfg(test)]
mod tests;
