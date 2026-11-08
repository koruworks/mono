// SPDX-License-Identifier: MPL-2.0

//! **The `+` button's New… dialog** — the window's half of the launcher.
//!
//! The tab strip's `+`, the lane's New and Ctrl+N all open one centred grid
//! of what can be made here: a document, a task, an agent, a shell. Each
//! tile has a letter (D, T, A, S) that chooses it the moment the dialog is
//! up, and two modifiers that ask one question more:
//!
//! - **Alt** asks *where* — the place chooser opens over the focused pane,
//!   and the new thing lands in the split or the new tab it names.
//! - **Shift**, on an agent or a shell, asks *which* — the profile list
//!   opens under the tile instead of the default starting.
//!
//! The grid is KoruUI's ([`koru_ui::launcher`]): layout, hit-testing, keys,
//! drawing and the shape a screen reader hears. It answers a tile index and
//! knows nothing of what a tile makes. This file is the wiring: the tiles
//! this build offers ([`NewKind::offered`]), what each one opens
//! ([`MainView::create_new`]), and the modal plumbing — the `Launcher`
//! blocker layer, the press, the keys, the cursor.

use koru_editor::{OpenWhere, SplitSide};
use koru_i18n::{Key as I18nKey, t};
use koru_ui::launcher::{Launcher, Outcome, Tile};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key as WKey, ModifiersState};
use winit::window::CursorIcon;

use super::MainView;
use super::context_menu::{self, Anchor as MenuAnchor, ContextMenu, MenuSubject};
use super::place_chooser::PlaceTarget;
use super::tab::{Tab, TabView};
use crate::panes::explore::rail::{Bucket, RailTarget};

#[cfg(test)]
mod tests;

/// One thing the launcher makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NewKind {
    Document,
    Task,
    Chat,
    Agent,
    Shell,
    /// Never made on its own: it always asks what — files, or content in
    /// one of the import formats.
    Import,
}

impl NewKind {
    /// The tiles this build offers, in the grid's order. Tasks and agents
    /// answer to their product flags (koru-features), the doors they open by
    /// would refuse them anyway; a document and a shell are always there.
    pub(crate) fn offered() -> Vec<Self> {
        let mut kinds = vec![Self::Document];
        if koru_features::tasks() {
            kinds.push(Self::Task);
        }
        if koru_features::ai() {
            kinds.push(Self::Chat);
        }
        if koru_features::agents() {
            kinds.push(Self::Agent);
        }
        kinds.push(Self::Shell);
        kinds.push(Self::Import);
        kinds
    }

    /// The letter that chooses it, the same in every language.
    pub(crate) fn letter(self) -> char {
        match self {
            Self::Document => 'd',
            Self::Task => 't',
            Self::Chat => 'c',
            Self::Agent => 'a',
            Self::Shell => 's',
            Self::Import => 'i',
        }
    }

    fn glyph(self) -> char {
        use koru_draw::icons;
        match self {
            Self::Document => icons::DOCUMENT,
            Self::Task => icons::CHECK_SQUARE,
            Self::Chat => icons::CHATS_CIRCLE,
            Self::Agent => icons::ROBOT,
            Self::Shell => icons::TERMINAL,
            Self::Import => icons::IMPORT,
        }
    }

    fn label(self) -> I18nKey {
        match self {
            Self::Document => I18nKey::LauncherDocument,
            Self::Task => I18nKey::LauncherTask,
            Self::Chat => I18nKey::LauncherChat,
            Self::Agent => I18nKey::TerminalKindAgent,
            Self::Shell => I18nKey::TerminalKindShell,
            Self::Import => I18nKey::LauncherImport,
        }
    }

    fn tile(self, lang: koru_i18n::Lang) -> Tile {
        Tile {
            glyph: self.glyph(),
            label: t(lang, self.label()).into(),
            letter: self.letter(),
        }
    }
}

/// Where a launcher's follow-up — a list, the place chooser — opens.
#[derive(Clone, Copy, Debug, PartialEq)]
enum AskAt {
    /// Hung from the pointer: the choice was a press.
    Pointer((f32, f32)),
    /// Centred on the point: the choice came from the keyboard.
    Centre((f32, f32)),
}

impl AskAt {
    fn point(self) -> (f32, f32) {
        match self {
            Self::Pointer(at) | Self::Centre(at) => at,
        }
    }

    fn anchor(self) -> MenuAnchor {
        match self {
            Self::Pointer(_) => MenuAnchor::Cursor,
            Self::Centre(_) => MenuAnchor::Centre,
        }
    }
}

/// One opening of the launcher: the grid, and the kind behind each tile.
#[derive(Clone, Debug)]
pub(crate) struct LauncherAsk {
    pub launcher: Launcher,
    pub kinds: Vec<NewKind>,
}

impl MainView {
    /// Open the launcher. Any menu, chooser or tooltip goes: it is the
    /// newest thing asked for.
    pub(crate) fn open_launcher(&mut self) -> bool {
        self.dismiss_settings_menus();
        self.nested = None;
        self.submenu = None;
        self.menu = None;
        self.place_chooser = None;
        self.tooltip.clear();
        // Freshen the presence probe that dims missing programs, off the loop
        // thread, so a default chosen a moment from now is one that is there.
        self.profile_probe.refresh(self.agent.wake_proxy());
        let lang = crate::format::lang();
        let kinds = NewKind::offered();
        let tiles = kinds.iter().map(|kind| kind.tile(lang)).collect();
        self.launcher = Some(LauncherAsk {
            launcher: Launcher::open(t(lang, I18nKey::LauncherTitle), tiles)
                .with_hint(t(lang, I18nKey::LauncherHint)),
            kinds,
        });
        true
    }

    /// Close the launcher with nothing made.
    pub(crate) fn dismiss_launcher(&mut self) -> bool {
        self.launcher.take().is_some()
    }

    /// Choose tile `index`: Alt asks where, Shift on a session asks which,
    /// and otherwise the kind is made in a new tab.
    ///
    /// **What asks next sits where the choice came from**: chosen by a
    /// press, the list or the place chooser opens at the pointer; chosen
    /// from the keyboard (or a screen reader), in the window's middle,
    /// where the launcher was.
    pub(crate) fn choose_launcher_tile(
        &mut self,
        index: usize,
        alt: bool,
        shift: bool,
        by_pointer: bool,
    ) -> bool {
        let Some(ask) = self.launcher.take() else {
            return false;
        };
        let Some(&kind) = ask.kinds.get(index) else {
            return true;
        };
        let at = self.next_ask_at(by_pointer);
        koru_ui::keys::debug::write(format_args!(
            "[Koru][input] view launcher chose={kind:?} alt={alt} shift={shift} pointer={by_pointer}"
        ));
        // Import has no default and no place: every modifier, and none,
        // asks what to import.
        if kind == NewKind::Import {
            return self.open_import_list(at);
        }
        if alt {
            return self.open_place_chooser(PlaceTarget::New(kind), at.point());
        }
        if shift && matches!(kind, NewKind::Agent | NewKind::Shell) {
            return self.open_profile_list(kind, at);
        }
        if shift && kind == NewKind::Task {
            return self.open_task_destinations(at);
        }
        self.create_new(kind, OpenWhere::NewTab)
    }

    /// The Tasks pages a new task can be written on, under the Task tile:
    /// the queues, then the person's lists.
    fn open_task_destinations(&mut self, at: AskAt) -> bool {
        context_menu::set_task_list_names(
            self.list_table
                .iter()
                .filter(|entry| entry.deleted_ms.is_none())
                .map(|entry| (entry.id, entry.name.clone()))
                .collect(),
        );
        let mut menu = ContextMenu::aligned(
            &mut self.engine,
            MenuSubject::NewTab,
            at.point(),
            context_menu::task_destination_items(),
            at.anchor(),
        );
        let enabled = self.enabled_items(&menu);
        menu.step(&enabled, 1);
        self.menu = Some(menu);
        true
    }

    /// What to import: files, then each content format.
    fn open_import_list(&mut self, at: AskAt) -> bool {
        let mut menu = ContextMenu::aligned(
            &mut self.engine,
            MenuSubject::NewTab,
            at.point(),
            context_menu::import_menu_items(),
            at.anchor(),
        );
        let enabled = self.enabled_items(&menu);
        menu.step(&enabled, 1);
        self.menu = Some(menu);
        true
    }

    /// The profiles of `kind`, under the tile that asked — the `+` menu's
    /// old flyout, on its own. A row opens its session in a new tab.
    ///
    /// **Agents end in *Manage…*** (product decision, 2026-11-05, kept when
    /// the launcher replaced the `+` menu): only installed agents are listed,
    /// so the way to install another is the list's last row, and a machine
    /// with none still gets a row that does something.
    fn open_profile_list(&mut self, kind: NewKind, at: AskAt) -> bool {
        let items = match kind {
            NewKind::Agent => {
                let mut items = context_menu::agent_profile_items();
                if !items.is_empty() {
                    items.push(context_menu::MenuAction::Divider);
                }
                items.push(context_menu::MenuAction::ManageAgents);
                items
            }
            _ => context_menu::shell_profile_items(),
        };
        let mut menu = ContextMenu::aligned(
            &mut self.engine,
            MenuSubject::NewTab,
            at.point(),
            items,
            at.anchor(),
        );
        let enabled = self.enabled_items(&menu);
        menu.step(&enabled, 1);
        self.menu = Some(menu);
        true
    }

    /// Make `kind` where `where_` says: a new tab, or a split of the focused
    /// pane. Every other disposition is a new tab — a new thing has nothing
    /// to reuse.
    pub(crate) fn create_new(&mut self, kind: NewKind, where_: OpenWhere) -> bool {
        let split = match where_ {
            OpenWhere::Split(side) => Some(side),
            _ => None,
        };
        match kind {
            NewKind::Document => self.new_document_tab(split),
            NewKind::Task => self.new_task(split),
            NewKind::Chat => self.new_chat(split),
            NewKind::Import => self.open_import_list(self.next_ask_at(false)),
            NewKind::Agent | NewKind::Shell => match self.default_profile(kind) {
                Some(index) => self.open_session_tab_where(index, None, split),
                // Nothing of that kind is installed: the list says so — a
                // shell row by row, dimmed; for agents, *Manage…* alone,
                // where one is installed — rather than nothing happening.
                None => self.open_profile_list(kind, self.next_ask_at(false)),
            },
        }
    }

    /// A draft document, in a new tab or a split.
    fn new_document_tab(&mut self, split: Option<SplitSide>) -> bool {
        let Some(side) = split else {
            return self.new_tab();
        };
        let anchor = self.library_create_anchor_hint();
        self.record_jump();
        self.split_with(Tab::draft(anchor).unwrap_or_else(Tab::welcome), side);
        true
    }

    /// A new Koru AI chat, in a new tab or a split.
    fn new_chat(&mut self, split: Option<SplitSide>) -> bool {
        let Some(side) = split else {
            return self.open_ai_chat_tab();
        };
        if !koru_features::ai() {
            return false;
        }
        self.record_jump();
        self.split_with(Tab::ai_chat(), side);
        true
    }

    /// The Tasks page somebody was last on (Today when there is none), with
    /// the caret in its *Add a task* row.
    fn new_task(&mut self, split: Option<SplitSide>) -> bool {
        let target = match self.tasks_selection() {
            Some(target @ (RailTarget::Bucket(_) | RailTarget::List(_))) => target,
            _ => RailTarget::Bucket(Bucket::Today),
        };
        self.new_task_in(target, split)
    }

    /// `target`'s Tasks page, in front or in a split, with the caret in its
    /// *Add a task* row.
    pub(crate) fn new_task_in(&mut self, target: RailTarget, split: Option<SplitSide>) -> bool {
        let opened = match split {
            Some(side) => self.open_tasks_page_where(target, OpenWhere::Split(side)),
            None => self.open_tasks_tab(target, false),
        };
        if opened && let TabView::Tasks(view) = &mut self.tabs_mut().active_mut().view {
            view.begin_capture();
        }
        opened
    }

    /// The profile a plain choice starts: the first of `kind` in the
    /// person's order (Agent providers) whose program is there.
    fn default_profile(&self, kind: NewKind) -> Option<usize> {
        let items = match kind {
            NewKind::Agent => context_menu::agent_profile_items(),
            _ => context_menu::shell_profile_items(),
        };
        items.into_iter().find_map(|item| match item {
            context_menu::MenuAction::TerminalProfile(index)
                if self.profile_probe.row_enabled(index) =>
            {
                Some(index)
            }
            _ => None,
        })
    }

    /// The agent profiles on their own, centred: the tray's *Agents ▸ New*.
    pub(super) fn open_agent_profiles(&mut self) -> bool {
        self.open_profile_list(NewKind::Agent, self.next_ask_at(false))
    }

    /// Where a follow-up ask opens: at the pointer, or centred.
    fn next_ask_at(&self, by_pointer: bool) -> AskAt {
        if by_pointer {
            AskAt::Pointer(self.pointer)
        } else {
            AskAt::Centre((self.viewport.0 * 0.5, self.viewport.1 * 0.5))
        }
    }

    /// Act on what the launcher answered.
    fn settle_launcher(&mut self, outcome: Outcome, by_pointer: bool) -> bool {
        match outcome {
            Outcome::Handled => true,
            Outcome::Moved(index) => self.set_launcher_highlight(index),
            Outcome::Chose { index, alt, shift } => {
                self.choose_launcher_tile(index, alt, shift, by_pointer)
            }
            Outcome::Dismissed => self.dismiss_launcher(),
        }
    }

    /// Move the highlight from the keyboard or a screen reader's focus.
    pub(crate) fn set_launcher_highlight(&mut self, index: usize) -> bool {
        match self.launcher.as_mut() {
            Some(ask) => {
                ask.launcher.set_highlight(index);
                true
            }
            None => false,
        }
    }

    /// A press while the launcher stands: on a tile it chooses it, with the
    /// modifiers held; anywhere else it closes the launcher. Either way the
    /// press goes no further. `None` when no launcher is up.
    pub(super) fn launcher_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        let outcome = self
            .launcher
            .as_ref()?
            .launcher
            .mouse_down(self.viewport, x, y, self.mods);
        Some(self.settle_launcher(outcome, true))
    }

    /// The pointer moved while the launcher stands: the tile under it is the
    /// highlight. `None` when no launcher is up.
    pub(super) fn launcher_mouse_move(&mut self, x: f32, y: f32) -> Option<bool> {
        let viewport = self.viewport;
        Some(self.launcher.as_mut()?.launcher.mouse_move(viewport, x, y))
    }

    /// A key while the launcher stands. Always handled: it is modal.
    pub(super) fn launcher_key(&mut self, key: &WKey, mods: ModifiersState) -> bool {
        let Some(ask) = self.launcher.as_mut() else {
            return false;
        };
        let outcome = ask.launcher.key(key, mods);
        self.settle_launcher(outcome, false);
        true
    }

    /// The pointer over the launcher: pressable over a tile, the arrow
    /// elsewhere.
    pub(super) fn launcher_cursor(&self) -> Option<CursorIcon> {
        let ask = self.launcher.as_ref()?;
        Some(
            ask.launcher
                .cursor(self.viewport, self.pointer.0, self.pointer.1),
        )
    }

    pub(super) fn draw_launcher(&mut self, scene: &mut Scene, root: Affine) {
        let Some(ask) = self.launcher.as_ref() else {
            return;
        };
        let launcher = ask.launcher.clone();
        launcher.draw(scene, root, &mut self.engine, self.viewport);
    }
}
