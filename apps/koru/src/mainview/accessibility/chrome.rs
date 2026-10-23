// SPDX-License-Identifier: MPL-2.0

//! **The caption and the tab strip**, as a screen reader sees them and as a
//! request reaches them.
//!
//! Every rectangle here is the one already shared by drawing and hit-testing:
//! [`koru_ui::caption::controls`] for the window's four caption squares,
//! [`koru_shell_chrome::tabs::layout`] and [`koru_shell_chrome::tabs::new_tab`]
//! for the strip. Nothing recomputes a position.
//!
//! ## Which rect, of the two the shell keeps
//!
//! `regions.caption` is the **settled** caption; `drawn_caption()` is where the
//! pixels are during the 140 ms a column takes to slide. The two differ by up
//! to a column's width, and `hit_at` uses the settled one — so a control must be
//! published there, or a client's click-through would miss by exactly the
//! amount the pane is animating.
//!
//! ## Identity is the tab, not its position
//!
//! `Tab::id` is minted once per tab instance and never reused, which is what
//! §5's *reordering, renaming, translating labels or scrolling must not change
//! the identity of a surviving control* needs. It is also what makes two tabs
//! over one document two occurrences. A flat index would fail both.

use accesskit::{Action, Role};
use koru_core::schema::WorkspaceDoc;
use koru_i18n::{Key, t};
use koru_shell_chrome::tabs;
use koru_workspace_host::accessibility::{CaptionGeometry, Occurrence, SemanticNode};

use super::{MENU, SHELL, Shell, TAB, instance_of, parts, surface_of};
use crate::mainview::MainView;
use crate::panes::Outcome;

/// Where this view draws minimise, maximise and close.
///
/// The overflow `…` is **not** here: it opens a container menu, so it is one of
/// the view's own controls and appears in the view's subtree instead. What the
/// host owns is what the host performs.
pub(super) fn window_command_bounds(view: &MainView) -> CaptionGeometry {
    use koru_ui::caption::Action as Caption;
    if !crate::mainview::WINDOW_BUTTONS {
        return CaptionGeometry::default();
    }
    let mut geometry = CaptionGeometry::default();
    for control in koru_ui::caption::controls(view.regions.caption) {
        match control.action {
            Caption::Minimise => geometry.minimize = Some(control.rect),
            Caption::ToggleMaximise => geometry.maximize = Some(control.rect),
            Caption::Close => geometry.close = Some(control.rect),
            Caption::WindowMenu => {}
        }
    }
    geometry
}

impl MainView {
    /// The tab strip of the focused pane group.
    ///
    /// **The focused group only.** A split window draws a strip per group and
    /// this plan publishes one; the others stay unreachable to a client, which
    /// is recorded in `docs/features/accessibility/README.md` rather than papered
    /// over. Splitting is a pointer gesture and A1 is the ordinary launch.
    pub(super) fn tabs_semantics(&self, cx: &Shell) -> SemanticNode {
        let strip = self.strip();
        let widths = self.tab_widths();
        let scroll = self.tab_scroll();
        let rects = tabs::layout(strip, widths, scroll);
        let strip_tabs = self.tabs();
        let total = strip_tabs.len();
        let active = strip_tabs.active();
        let mut children: Vec<SemanticNode> = Vec::with_capacity(total + 1);
        for (i, tab) in strip_tabs.iter().enumerate() {
            // `widths` can be stale for a frame after a tab opens — it is
            // remeasured with a `TextEngine` this borrow cannot reach. A tab
            // with no rectangle yet is still a tab, and still selectable.
            let rect = rects.get(i);
            let mut node = SemanticNode::new(cx.instance(TAB, tab.id, parts::ROOT), Role::Tab)
                .with_label(tab_label(tab, cx))
                .with_actions([Action::Click, Action::Focus])
                // A tab is what selection is *for*, and "not selected" is
                // worth saying about one -- so both values are published. It
                // costs the tab its Invoke pattern (a node carrying `selected`
                // is not invocable), which is correct here: a UIA client
                // selects a tab through `SelectionItem`, and the adapter turns
                // that back into the `Click` above.
                .with_selected(i == active)
                .with_position(i);
            if let Some(rect) = rect {
                node = node.with_bounds(rect.body);
                // Zero-sized when the tab is too narrow to draw one, which is
                // also when it cannot be pressed.
                if rect.close.w > 0.0 {
                    node = node.with_children(vec![
                        SemanticNode::new(cx.instance(TAB, tab.id, parts::CLOSE_TAB), Role::Button)
                            .with_label(t(cx.lang, Key::A11yCloseTab))
                            .with_bounds(rect.close)
                            .with_action(Action::Click),
                    ]);
                }
            }
            children.push(node);
        }
        children.push(
            SemanticNode::new(cx.surface(SHELL, parts::NEW_TAB), Role::Button)
                .with_label(t(cx.lang, Key::A11yNewTab))
                .with_bounds(tabs::new_tab(strip, widths, scroll))
                .with_action(Action::Click),
        );
        SemanticNode::new(cx.surface(SHELL, parts::TABS), Role::TabList)
            .with_label(t(cx.lang, Key::A11yTabs))
            .with_bounds(strip)
            // On the container: an item's total is read from its selection
            // container, not from the item.
            .with_size_of_set(total)
            .with_children(children)
    }

    /// The caption's own controls — the ones the *container* performs.
    pub(super) fn caption_semantics(&self, cx: &Shell) -> Vec<SemanticNode> {
        let caption = self.regions.caption;
        // The lane's controls, wherever they are this frame: in the pane's
        // top band while it is open, in the caption's leading chain while it
        // is shut. One geometry function either way (`lane_controls`).
        let lane = self.lane_controls();
        let mut out = vec![
            SemanticNode::new(cx.surface(SHELL, parts::TOGGLE_EXPLORE), Role::Button)
                .with_label(t(cx.lang, Key::A11yToggleNavigator))
                .with_bounds(lane.toggle)
                .with_action(Action::Click),
        ];
        if lane.search.w > 0.0 {
            out.push(
                SemanticNode::new(cx.surface(SHELL, parts::SEARCH), Role::Button)
                    .with_label(t(cx.lang, Key::SearchOrTypeACommand))
                    .with_bounds(lane.search)
                    .with_action(Action::Click),
            );
        }
        if lane.new.w > 0.0
            && let Some(create) = self.explore.navigator_mode().primary_create()
        {
            out.push(
                SemanticNode::new(cx.surface(SHELL, parts::NEW), Role::Button)
                    .with_label(create.label_in(cx.lang))
                    .with_bounds(lane.new)
                    .with_action(Action::Click),
            );
        }
        let _ = caption;
        if crate::mainview::WINDOW_BUTTONS
            && let Some(control) = koru_ui::caption::controls(caption)
                .into_iter()
                .find(|control| control.action == koru_ui::caption::Action::WindowMenu)
        {
            out.push(
                SemanticNode::new(cx.surface(SHELL, parts::WINDOW_MENU), Role::Button)
                    .with_label(t(cx.lang, Key::A11yWindowMenu))
                    // No `expanded`: it would cost the button its Invoke
                    // pattern and there is no ExpandCollapse pattern on
                    // Windows to replace it. See `guide/contributing/deps/ACCESSKIT.md`.
                    .with_bounds(control.rect)
                    .with_action(Action::Click),
            );
        }
        out
    }
}

/// What a tab is called, with the leading emoji the strip drew separately put
/// back — a screen reader has no icon slot, and a tab that reads as its bare
/// title loses the mark a person chose to tell it apart by.
fn tab_label(tab: &crate::mainview::tab::Tab, cx: &Shell) -> String {
    match (&tab.icon, tab.label.is_empty()) {
        (Some(icon), false) => format!("{icon} {}", tab.label),
        (Some(icon), true) => icon.clone(),
        (None, false) => tab.label.clone(),
        (None, true) => t(cx.lang, Key::A11yContent).to_string(),
    }
}

/// Route a request at the caption or the strip.
///
/// Each arm reaches the [`Outcome`] the press already produces, and hands it to
/// the container's own `apply` — there is no second dispatcher.
pub(super) fn action(
    view: &mut MainView,
    ws: &mut WorkspaceDoc,
    target: Occurrence,
    action: Action,
) -> bool {
    if let Some(id) = instance_of(target, TAB) {
        return tab_action(view, ws, target, action, id);
    }
    if instance_of(target, MENU).is_some() {
        return false;
    }
    if action != Action::Click || surface_of(target) != Some(SHELL) {
        return false;
    }
    let outcome = match target.part {
        p if p == parts::NEW_TAB => Outcome::NewTab,
        p if p == parts::TOGGLE_EXPLORE => {
            // Ctrl+Shift+E's own three-state answer — open and focus, take the
            // keyboard, or shut — rather than a fourth interpretation of what
            // pressing the toggle means.
            view.focus_or_toggle_sidebar(ws);
            return true;
        }
        p if p == parts::SEARCH => Outcome::Search,
        p if p == parts::NEW => {
            return view.run_primary_create();
        }
        p if p == parts::WINDOW_MENU => {
            let Some(button) = view.window_menu_button() else {
                return false;
            };
            let rect = button.rect();
            view.open_window_menu(rect);
            return true;
        }
        _ => return false,
    };
    view.apply(ws, outcome);
    true
}

fn tab_action(
    view: &mut MainView,
    ws: &mut WorkspaceDoc,
    target: Occurrence,
    action: Action,
    id: u64,
) -> bool {
    // **By instance, resolved to a position at the moment of consequence.**
    // The strip addresses a tab by index and the index moves; the id does not.
    let Some(index) = view.tabs().iter().position(|tab| tab.id == id) else {
        return false;
    };
    let outcome = match (action, target.part) {
        (Action::Click, p) if p == parts::CLOSE_TAB => Outcome::CloseTab(index),
        // Selecting is what both a click and a focus mean for a tab: a tab
        // strip has one selection and it *is* where the keyboard goes.
        (Action::Click | Action::Focus, p) if p == parts::ROOT => Outcome::SelectTab(index),
        _ => return false,
    };
    view.apply(ws, outcome);
    true
}
