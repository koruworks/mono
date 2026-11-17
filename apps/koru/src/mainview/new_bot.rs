// SPDX-License-Identifier: MPL-2.0

//! **The window's half of the first-bot experience** (QUESTIONS O155): the
//! empty Bots view in the content area, the *New bot* dialog over the
//! window, and the one clock every Bot face in this window is posed from.
//!
//! - **The empty view** ([`crate::bots::empty`]) is drawn in the content
//!   area while the Bots panel is what the navigator shows and its roster
//!   has answered that the workspace has no Bot
//!   ([`MainView::bots_empty_visible`]). It takes the content area's
//!   presses and keys, so the tab drawn under it sees none; another
//!   navigator view, or a Bot, brings the tab back.
//! - **The dialog** ([`crate::bots::view::new_bot`]) is a blocker layer
//!   (`routing::Layer::NewBot`): every key and press is its while it
//!   stands. Its Create goes through the create form's own command path;
//!   when the owner answers that the Bot is made, the dialog's host is that
//!   Bot's tab and is put in the strip here.
//! - **The faces** move only while their Bot is working, and the face
//!   picked in the standing dialog moves while it stands. Which Bots are
//!   working is read from what the window already holds: the roster's
//!   activity read, and the status the chat pass gives every Bot tab's
//!   conversation. Nothing is asked for it and nothing is scanned on a
//!   timer; the one deadline is the clock's own, and there is none while
//!   neither a working Bot's face nor the dialog is on screen.

use std::time::Instant;

use accesskit::{Action, ActionData, Role};
use koru_ai_core::bots::BotId;
use koru_draw::el;
use koru_i18n::{Key as Words, t};
use koru_workspace_host::accessibility::{ControlPart, Occurrence, Revision, SemanticNode};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState};
use winit::window::CursorIcon;

use super::MainView;
use super::accessibility::{Shell, surface_of};
use super::tab::{Tab, TabView};
use crate::bots::empty::{self, EmptyView};
use crate::bots::view::new_bot::{self, NewBotDialog, Outcome};

/// What opened the dialog: where the keyboard goes back when it closes
/// with nothing made.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum Opener {
    /// The empty view's *New bot*.
    EmptyView,
    /// The navigator heading's `+`, or anything else: the keyboard was
    /// never taken from where it was, and is there again.
    #[default]
    Elsewhere,
}

/// The first-bot surfaces' state in the window.
#[derive(Default)]
pub(super) struct FirstBot {
    /// The dialog, while it stands. Boxed: it holds a whole Bot tab.
    pub(super) dialog: Option<Box<NewBotDialog>>,
    opener: Opener,
    pub(super) empty: EmptyView,
    /// **The window's one face clock.** Every face in the roster and in a
    /// Bot tab's header is posed from it, so they share one deadline.
    faces: koru_draw::face::FaceClock,
    /// The clock's deadline as last handed to the loop: a pass at or after
    /// it owes a repaint, because a step of the motion has passed.
    faces_due: Option<Instant>,
}

impl MainView {
    // ------------------------------------------------------------ the dialog

    /// **Open the *New bot* dialog** for the window's workspace: the
    /// navigator heading's `+`, the empty view's button. Any menu goes; a
    /// dialog already standing stays as it is. `false` with no workspace
    /// to create in.
    pub(super) fn open_new_bot_dialog(&mut self, opener: Opener) -> bool {
        if !crate::bots::shown() {
            return false;
        }
        if self.first_bot.dialog.is_some() {
            return true;
        }
        let Some(home) = self
            .current_workspace()
            .map(|id| koru_store_core::ids::WorkspaceId::from_uuid(id.as_uuid()))
        else {
            return false;
        };
        self.dismiss_settings_menus();
        self.nested = None;
        self.submenu = None;
        self.menu = None;
        self.launcher = None;
        self.place_chooser = None;
        self.tooltip.clear();
        self.first_bot.dialog = Some(Box::new(NewBotDialog::open(
            home,
            self.workspace_name(home),
            crate::bots::first_bot::channel_root_here(),
            self.viewport,
        )));
        self.first_bot.opener = opener;
        // The picked face moves from the dialog's first frame: the clock is
        // told the dialog stands now, not at the next pass.
        self.sync_bot_faces(Instant::now());
        true
    }

    /// Whether the dialog stands: the blocker table's question.
    pub(super) fn new_bot_open(&self) -> bool {
        self.first_bot.dialog.is_some()
    }

    /// Act on what a gesture in the dialog asked for.
    fn settle_new_bot(&mut self, outcome: Outcome) -> bool {
        match outcome {
            Outcome::Ignored => false,
            Outcome::Changed => true,
            Outcome::Cancel => {
                // Nothing was sent: the draft goes, and the keyboard is
                // back where it was.
                self.first_bot.dialog = None;
                if self.first_bot.opener == Opener::EmptyView && self.bots_empty_visible() {
                    self.first_bot.empty.focus();
                }
                true
            }
            Outcome::OpenForm => self.new_bot_into_tab(),
        }
    }

    /// **The dialog's draft, or the Bot it made, as a tab in front**: the
    /// new Bot's tab on its main conversation once the owner made it; the
    /// create form on the same draft when the person asked for it, or
    /// closed the dialog while the owner may hold the creation.
    fn new_bot_into_tab(&mut self) -> bool {
        let Some(dialog) = self.first_bot.dialog.take() else {
            return false;
        };
        self.first_bot.empty.blur();
        self.record_jump();
        self.tabs_mut().push(Tab::bot(dialog.into_tab()));
        self.mark_tab_widths_stale();
        true
    }

    /// **The dialog's pass**, on the window's wake path: its host's own
    /// (link, catalog, the read a creation owes), the news for the roster's
    /// Unfinished group, the folder dialog it asked for, and — when the
    /// owner made the Bot — the tab. Nothing while no dialog stands.
    pub(super) fn maintain_new_bot(&mut self) -> bool {
        let Some(mut dialog) = self.first_bot.dialog.take() else {
            return false;
        };
        dialog.set_viewport(self.viewport);
        let settings = self.bot_agent_settings();
        let mut changed = dialog.pass(crate::search::gateway_root(), &settings);
        if let Some(creation) = dialog.take_creation_news() {
            changed |= self.explore.bots_mut().know_creation(creation);
        }
        if let Some(start) = dialog.take_folder_request()
            && let Some(draft) = dialog.draft_bot()
        {
            // The window's existing folder dialog, the create form's own
            // door: its answer comes back by the draft's BotId.
            self.bot_create_folder_ask = Some(draft);
            self.ai_chat_folder_ask = None;
            self.ask_pane(koru_workspace_host::PaneRequest::ChooseAiChatFolder {
                title: t(crate::format::lang(), Words::BotsFolder).to_owned(),
                start,
            });
        }
        let created = dialog.created();
        self.first_bot.dialog = Some(dialog);
        if created {
            // The roster lists the new Bot once the tab says it was made
            // (`refresh_bot_tabs`, `take_created`).
            changed |= self.new_bot_into_tab();
        }
        changed
    }

    /// When the dialog next owes a pass. `None` with no dialog, and for one
    /// nothing is on its way in.
    pub(super) fn new_bot_deadline(&self) -> Option<Instant> {
        self.first_bot
            .dialog
            .as_ref()
            .and_then(|dialog| dialog.deadline(Instant::now()))
    }

    /// The folder dialog answered for `draft`: `Some` when the draft is the
    /// standing dialog's, with whether the folder joined it.
    pub(super) fn new_bot_folder_chosen(
        &mut self,
        draft: BotId,
        chosen: &std::path::Path,
    ) -> Option<bool> {
        let dialog = self.first_bot.dialog.as_mut()?;
        (dialog.draft_bot() == Some(draft)).then(|| dialog.folder_chosen(chosen))
    }

    /// A key while the dialog stands. Always handled: it is modal.
    pub(super) fn new_bot_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        let Some(dialog) = self.first_bot.dialog.as_mut() else {
            return false;
        };
        let outcome = dialog.key(key, text, mods);
        self.settle_new_bot(outcome);
        true
    }

    /// A press while the dialog stands: it goes no further, whatever it
    /// landed on.
    pub(super) fn new_bot_mouse_down(&mut self, x: f32, y: f32) -> bool {
        let Some(dialog) = self.first_bot.dialog.as_mut() else {
            return false;
        };
        let outcome = dialog.mouse_down(x, y);
        self.settle_new_bot(outcome);
        true
    }

    pub(super) fn new_bot_mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.first_bot
            .dialog
            .as_mut()
            .is_some_and(|dialog| dialog.mouse_move(x, y))
    }

    pub(super) fn new_bot_mouse_up(&mut self) -> bool {
        self.first_bot
            .dialog
            .as_mut()
            .is_some_and(|dialog| dialog.mouse_up())
    }

    pub(super) fn new_bot_scroll(&mut self, dy: f32) -> bool {
        self.first_bot
            .dialog
            .as_mut()
            .is_some_and(|dialog| dialog.scroll_by(dy))
    }

    pub(super) fn new_bot_cursor(&self) -> Option<CursorIcon> {
        let dialog = self.first_bot.dialog.as_ref()?;
        Some(dialog.cursor(self.pointer.0, self.pointer.1))
    }

    pub(super) fn draw_new_bot(&mut self, scene: &mut Scene, root: Affine) {
        let Some(mut dialog) = self.first_bot.dialog.take() else {
            return;
        };
        dialog.set_viewport(self.viewport);
        dialog.draw(scene, root, &mut self.engine);
        self.first_bot.dialog = Some(dialog);
    }

    // ------------------------------------------- the dialog, to a screen reader

    /// The dialog in full: a dialog named *New bot*, every control under it
    /// by the element that draws it.
    pub(super) fn new_bot_semantics(&self, cx: &Shell) -> Option<SemanticNode> {
        let published = self.first_bot.dialog.as_ref()?.published();
        let root = published.nodes.first()?;
        let children = root
            .children
            .iter()
            .map(|node| element_node(cx, new_bot::SURFACE, node, Some(&published.name)))
            .collect();
        Some(
            SemanticNode::new(
                cx.surface(new_bot::SURFACE, ControlPart::new(new_bot::ROOT.part)),
                Role::Dialog,
            )
            .with_label(t(cx.lang, Words::BotsNewBot))
            .with_bounds(published.panel)
            .modal()
            .with_children(children),
        )
    }

    /// The control the dialog's keyboard is on.
    pub(super) fn new_bot_focus(&self, cx: &Shell) -> Option<Occurrence> {
        let dialog = self.first_bot.dialog.as_ref()?;
        Some(cx.surface(new_bot::SURFACE, ControlPart::new(dialog.focus().part)))
    }

    /// Whether a request names the standing dialog or one of its controls.
    pub(super) fn new_bot_is_target(&self, target: Occurrence) -> bool {
        self.first_bot.dialog.is_some() && surface_of(target) == Some(new_bot::SURFACE)
    }

    /// Do what a client asked of one of the dialog's controls.
    pub(super) fn new_bot_action(
        &mut self,
        target: Occurrence,
        action: Action,
        data: Option<&ActionData>,
    ) -> bool {
        if !self.new_bot_is_target(target) {
            return false;
        }
        let key = el::Key::surface(new_bot::SURFACE, target.part.name());
        let Some(dialog) = self.first_bot.dialog.as_mut() else {
            return false;
        };
        let outcome = match (action, data) {
            (Action::Click, _) => dialog.accessibility_action(key, true),
            (Action::Focus, _) => dialog.accessibility_action(key, false),
            (Action::SetValue, Some(ActionData::Value(text))) if key == new_bot::NAME => {
                if dialog.accessibility_set_name(text) {
                    Outcome::Changed
                } else {
                    Outcome::Ignored
                }
            }
            _ => Outcome::Ignored,
        };
        self.settle_new_bot(outcome)
    }

    /// Fold the first-bot surfaces into the chrome's fingerprint: whether
    /// each is there, and what a client would hear differently.
    pub(super) fn revise_first_bot(&self, rev: &mut Revision) {
        match &self.first_bot.dialog {
            Some(dialog) => {
                let published = dialog.published();
                rev.byte(1);
                rev.str(published.focus.part);
                rev.str(&published.name);
                fold(rev, &published.nodes);
            }
            None => {
                rev.byte(0);
            }
        }
        rev.bool(self.bots_empty_visible());
        rev.bool(self.first_bot.empty.focused());
    }

    // --------------------------------------------------------- the empty view

    /// **Whether the empty Bots view is what the content area shows**: the
    /// Bots panel is the navigator's view and is open, its roster answered
    /// that this workspace has no Bot, and the tab in front is not one the
    /// view would be wrong to cover — a Bot tab (the create form, or a
    /// Bot's conversation a link opened), settings, or the workspace
    /// manager. A roster still loading, or one the service did not answer,
    /// is never *no Bots*.
    pub(super) fn bots_empty_visible(&self) -> bool {
        use crate::bots::roster::Load;
        crate::bots::shown()
            && self.sidebar.is_open()
            && self.explore.showing_bots()
            && self.current_workspace().is_some()
            && !self.profile_wait_visible()
            && {
                let roster = self.explore.bots();
                roster.load() == Load::Ready && roster.bots().is_empty()
            }
            && !matches!(
                self.tabs().active_tab().view,
                TabView::Bot(_) | TabView::Manage(_) | TabView::Settings(_) | TabView::Feedback(_)
            )
    }

    /// Whether the empty view is what is under `(x, y)`: a gesture there is
    /// the view's, never the covered tab's.
    pub(super) fn bots_empty_covers(&self, x: f32, y: f32) -> bool {
        self.bots_empty_visible() && self.content().contains(x, y)
    }

    pub(super) fn draw_bots_empty(&mut self, scene: &mut Scene, root: Affine) {
        let area = self.content();
        self.first_bot
            .empty
            .draw(scene, root, &mut self.engine, area, crate::format::lang());
    }

    /// A press in the content area while the empty view is there.
    pub(super) fn bots_empty_mouse_down(&mut self, x: f32, y: f32) -> bool {
        if !self.bots_empty_covers(x, y) {
            return false;
        }
        let area = self.content();
        match self
            .first_bot
            .empty
            .mouse_down(area, crate::format::lang(), x, y)
        {
            empty::Outcome::NewBot => self.open_new_bot_dialog(Opener::Elsewhere),
            _ => true,
        }
    }

    pub(super) fn bots_empty_mouse_move(&mut self, x: f32, y: f32) -> bool {
        if !self.bots_empty_visible() {
            return self.first_bot.empty.blur();
        }
        let area = self.content();
        self.first_bot
            .empty
            .mouse_move(area, crate::format::lang(), x, y)
    }

    /// The pointer over the empty view's button.
    pub(super) fn bots_empty_cursor(&self) -> Option<CursorIcon> {
        self.bots_empty_covers(self.pointer.0, self.pointer.1)
            .then(|| {
                if self.first_bot.empty.hot() {
                    koru_ui::cursor::PRESS
                } else {
                    CursorIcon::Default
                }
            })
    }

    /// **A key that would otherwise reach the tab in front**, while the
    /// empty view covers it: `Some` always, so a hidden tab never takes a
    /// key, and `Some(true)` when the view used it.
    pub(super) fn bots_empty_key(&mut self, key: &Key, mods: ModifiersState) -> Option<bool> {
        if !self.bots_empty_visible() {
            return None;
        }
        Some(match self.first_bot.empty.key(key, mods) {
            empty::Outcome::NewBot => self.open_new_bot_dialog(Opener::EmptyView),
            empty::Outcome::Consumed => true,
            empty::Outcome::Ignored => false,
        })
    }

    /// The empty view under the content region: its heading, its words and
    /// its button.
    pub(super) fn bots_empty_semantics(&self, cx: &Shell) -> Option<SemanticNode> {
        if !self.bots_empty_visible() {
            return None;
        }
        let area = self.content();
        self.first_bot.empty.with_frame(area, cx.lang, |frame| {
            frame
                .semantics()
                .first()
                .map(|node| element_node(cx, empty::SURFACE, node, None))
        })
    }

    /// *New bot*, while the keyboard is visibly on it.
    pub(super) fn bots_empty_focus(&self, cx: &Shell) -> Option<Occurrence> {
        (self.bots_empty_visible() && self.first_bot.empty.focused())
            .then(|| cx.surface(empty::SURFACE, ControlPart::new(empty::NEW_BOT.part)))
    }

    /// A client's click or focus on the empty view's button.
    pub(super) fn bots_empty_action(&mut self, target: Occurrence, action: Action) -> bool {
        if !self.bots_empty_visible()
            || surface_of(target) != Some(empty::SURFACE)
            || target.part != ControlPart::new(empty::NEW_BOT.part)
        {
            return false;
        }
        match action {
            Action::Focus => {
                self.first_bot.empty.focus();
                true
            }
            Action::Click => {
                self.first_bot.empty.focus();
                self.open_new_bot_dialog(Opener::EmptyView)
            }
            _ => false,
        }
    }

    // --------------------------------------------------------------- faces

    /// **Pose every Bot face in this window**, from the one clock.
    ///
    /// A Bot is working when the roster's activity read says a reply of it
    /// is running (read only while the roster is shown), or when a Bot tab's
    /// conversation says so (the status the chat pass sets on every tab).
    /// The clock runs only while a working Bot's face is on screen: a row
    /// of the shown roster, or the header of a Bot tab in front; **or while
    /// the *New bot* dialog stands**, whose picked face (its preview and
    /// that face's tile) moves by the same clock, so the dialog adds no
    /// deadline of its own. `true` when a repaint is owed: the clock
    /// started or stopped, a step of the motion passed, or a header's pose
    /// moved.
    pub(super) fn sync_bot_faces(&mut self, now: Instant) -> bool {
        let roster_shown = self.sidebar.is_open() && self.explore.showing_bots();
        let mut working: Vec<BotId> = if roster_shown {
            self.explore.bots().running_bots().collect()
        } else {
            Vec::new()
        };
        let mut shown = false;
        for (_, group) in self.panes.iter_groups() {
            let active = group.active();
            for (index, tab) in group.iter_tabs().enumerate() {
                let TabView::Bot(view) = &tab.view else {
                    continue;
                };
                let Some(bot) = view.bot() else {
                    continue;
                };
                if view.conversation_working() && !working.contains(&bot) {
                    working.push(bot);
                }
                shown |= index == active && working.contains(&bot);
            }
        }
        shown |= roster_shown
            && self
                .explore
                .bots()
                .bots()
                .iter()
                .any(|row| working.contains(&row.id));
        // The dialog's picked face is on screen for as long as it stands.
        shown |= self.first_bot.dialog.is_some();
        let mut changed = self.first_bot.faces.set_working(shown, now);
        changed |= self.first_bot.faces.set_reduced_motion(self.reduced_motion);
        // A pass at or after the deadline last handed out: a step passed.
        if self.first_bot.faces_due.is_some_and(|due| now >= due) {
            changed = true;
        }
        self.first_bot.faces_due = self.first_bot.faces.deadline(now);
        let clock = self.first_bot.faces.clone();
        self.explore.bots_mut().set_faces(&clock, &working);
        if let Some(dialog) = self.first_bot.dialog.as_mut() {
            dialog.set_face_clock(&clock);
        }
        for (_, group) in self.panes.iter_groups_mut() {
            let active = group.active();
            for (index, tab) in group.iter_tabs_mut().enumerate() {
                let TabView::Bot(view) = &mut tab.view else {
                    continue;
                };
                // A tab behind another draws nothing: its face rests.
                let pose = match (index == active, view.bot(), view.avatar()) {
                    (true, Some(bot), Some(face)) => clock.pose(face, working.contains(&bot), now),
                    _ => koru_draw::face::FacePose::STILL,
                };
                let moved = view.set_face_pose(pose);
                changed |= moved && index == active;
            }
        }
        changed
    }

    /// **The one deadline every moving face shares**: the start of the
    /// clock's next step. `None` while neither a working Bot's face nor the
    /// *New bot* dialog is on screen, and under reduced motion.
    pub(super) fn bot_faces_deadline(&self) -> Option<Instant> {
        self.first_bot.faces.deadline(Instant::now())
    }

    /// The window's face clock, for a test to read.
    #[cfg(test)]
    pub(super) fn bot_faces(&self) -> &koru_draw::face::FaceClock {
        &self.first_bot.faces
    }
}

/// Fold what a client would hear of `nodes` into `rev`: each node's part,
/// its name, and whether it is selected or inert.
fn fold(rev: &mut Revision, nodes: &[el::SemNode]) {
    for node in nodes {
        rev.str(node.key.map_or("", |key| key.part));
        match &node.name {
            Some(el::Name::Key(key)) => {
                rev.u64(*key as u64);
            }
            Some(el::Name::Text(text)) => {
                rev.str(text);
            }
            None => {
                rev.byte(0);
            }
        }
        rev.byte(match node.selected {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        });
        rev.bool(node.disabled);
        fold(rev, &node.children);
    }
}

/// **One element of a first-bot surface as the window's node**: published
/// by its element's own part on `surface`, so no two can share a node id;
/// placed where the frame placed it. `name` is the text of a field, which
/// the element does not hold.
fn element_node(
    cx: &Shell,
    surface: &'static str,
    node: &el::SemNode,
    name: Option<&str>,
) -> SemanticNode {
    let part = node.key.map_or("root", |key| key.part);
    let role = match node.role {
        el::Role::Button => Role::Button,
        el::Role::Heading => Role::Heading,
        el::Role::TextInput => Role::TextInput,
        el::Role::Image => Role::Image,
        // A sentence: a group with nothing under it is words to read.
        el::Role::Group if node.children.is_empty() => Role::Label,
        _ => Role::Group,
    };
    let mut out = SemanticNode::new(cx.surface(surface, ControlPart::new(part)), role)
        .with_bounds(node.bounds);
    out = match &node.name {
        Some(el::Name::Key(key)) => out.with_label(t(cx.lang, *key)),
        Some(el::Name::Text(text)) => out.with_label(text.clone()),
        None => out,
    };
    // A picked face, the chosen place and the chosen agent are pressed
    // buttons: `selected` on a button would take Invoke from it
    // (`accesskit_consumer::Node::is_invocable`), as the Bot tab's own
    // publisher found.
    if role == Role::Button
        && let Some(on) = node.selected
    {
        out = out.with_toggled(on);
    }
    if node.disabled {
        out = out.disabled(true);
    } else {
        out = out.with_actions(node.actions.iter().map(|action| match action {
            el::Action::Click => Action::Click,
            el::Action::Focus => Action::Focus,
        }));
    }
    if role == Role::TextInput {
        out = out.with_value(name.unwrap_or_default());
        if !node.disabled {
            out = out.with_action(Action::SetValue);
        }
    }
    out.with_children(
        node.children
            .iter()
            .map(|child| element_node(cx, surface, child, name))
            .collect(),
    )
}

#[cfg(test)]
mod tests;
