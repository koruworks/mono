// SPDX-License-Identifier: MPL-2.0

//! **A Bot's lifecycle, from the window's side** (Bots P1.8 S14).
//!
//! The Bot tab draws its ⋯ and its rows' Move to Trash, and follows what it
//! sends; **the window owns the menu and the question**, through the same
//! `ContextMenu` and `Confirm` every other menu and question in it uses, so
//! no gesture reaches the owner without being asked first. On a yes the tab
//! sends the command; on a no nothing is sent; either way the keyboard goes
//! back to the control that asked.
//!
//! **The window keeps the book** ([`crate::bots::lifecycle::Book`]): every
//! operation a tab finishes is recorded here, and handed back to every tab
//! of that Bot, so a Bot archived in one tab can be unarchived in another,
//! and the Trash and the roster can restore what this window put away.

use koru_ai_core::bots::BotId;
use koru_i18n::{Key as Words, t};

use super::MainView;
use super::context_menu::{ContextMenu, MenuAction, MenuSubject};
use super::tab::TabView;
use super::trash::ConfirmSubject;
use crate::bots::BotView;
use crate::bots::lifecycle::Ask;

impl MainView {
    /// The Bot's tab, the one in front of its group first.
    pub(super) fn bot_view(&self, bot: BotId) -> Option<&BotView> {
        let mut found = None;
        for (_, group) in self.panes.iter_groups() {
            for (index, tab) in group.tabs().iter().enumerate() {
                if let TabView::Bot(view) = &tab.view
                    && view.bot() == Some(bot)
                {
                    if index == group.active() {
                        return Some(view);
                    }
                    found = found.or(Some(view));
                }
            }
        }
        found
    }

    /// The Bot's tab, mutably, the one in front of its group first.
    pub(super) fn bot_view_mut(&mut self, bot: BotId) -> Option<&mut BotView> {
        let mut at = None;
        'groups: for (ordinal, (_, group)) in self.panes.iter_groups().enumerate() {
            for (index, tab) in group.tabs().iter().enumerate() {
                if let TabView::Bot(view) = &tab.view
                    && view.bot() == Some(bot)
                {
                    if index == group.active() {
                        at = Some((ordinal, index));
                        break 'groups;
                    }
                    at = at.or(Some((ordinal, index)));
                }
            }
        }
        let (ordinal, index) = at?;
        let (_, group) = self.panes.iter_groups_mut().nth(ordinal)?;
        match &mut group.tabs_mut().get_mut(index)?.view {
            TabView::Bot(view) => Some(view),
            _ => None,
        }
    }

    /// **The Bot tab in front asked for its menu or a question** — its ⋯,
    /// a conversation row's menu or its Delete. Opened here, at the place
    /// the tab named, in window coordinates. `true` when one opened.
    pub(super) fn drain_bot_lifecycle_asks(&mut self) -> bool {
        let view_rect = self.focused_view_rect();
        let to_window = |at: (f32, f32)| (at.0 + view_rect.x, at.1 + view_rect.y);
        let TabView::Bot(view) = &mut self.tabs_mut().active_mut().view else {
            return false;
        };
        let Some(bot) = view.bot() else {
            return false;
        };
        let actions = view.take_actions_request();
        let rows = view.lifecycle_rows();
        let row_menu = view.take_conversation_menu();
        let trash = view.take_conversation_trash();
        let mut opened = false;
        if let Some(at) = actions
            && !rows.is_empty()
        {
            let items = rows
                .iter()
                .map(|ask| match ask {
                    Ask::Archive => MenuAction::ArchiveBot,
                    Ask::Trash => MenuAction::TrashBot,
                    Ask::Unarchive | Ask::Restore => MenuAction::UnarchiveBot,
                })
                .collect();
            opened |= self.open_subject_menu(MenuSubject::Bot { bot }, items, to_window(at));
        }
        if let Some((chat, at)) = row_menu {
            let mut menu = ContextMenu::new(
                &mut self.engine,
                MenuSubject::BotConversation { bot, chat },
                to_window(at),
                vec![MenuAction::TrashBotConversation],
            );
            let enabled = self.enabled_items(&menu);
            menu.step(&enabled, 1);
            self.menu = Some(menu);
            opened = true;
        }
        if let Some(chat) = trash {
            opened |= self.ask_trash_bot_conversation(bot, chat);
        }
        opened
    }

    /// **A row of the Bot's ⋯ was chosen**: Archive and Move to Trash are
    /// asked about first; Unarchive puts the Bot back and is sent at once.
    pub(super) fn choose_bot_lifecycle(&mut self, bot: BotId, action: MenuAction) -> bool {
        let ask = match action {
            MenuAction::ArchiveBot => Ask::Archive,
            MenuAction::TrashBot => Ask::Trash,
            MenuAction::UnarchiveBot => Ask::Unarchive,
            _ => return false,
        };
        let Some(view) = self.bot_view_mut(bot) else {
            return false;
        };
        let Some((title, body, verb)) = view.confirm_words(ask) else {
            let sent = view.begin_lifecycle(ask);
            view.dialog_closed();
            return sent;
        };
        let lang = crate::format::lang();
        self.confirm = Some((
            koru_ui::Confirm::localized(title, body, verb, t(lang, Words::BotsDialogCancel))
                // Reversible, and explicitly invoked: ADR-265's second row.
                // Archive and Trash both come back, paused.
                .defaulting_to_confirm(),
            ConfirmSubject::BotLifecycle { bot, ask },
        ));
        true
    }

    /// **Ask before one conversation goes to Trash** — a row's menu or its
    /// Delete. The main conversation's question says the Bot keeps working
    /// without it.
    pub(super) fn ask_trash_bot_conversation(
        &mut self,
        bot: BotId,
        chat: koru_ai_ui::ChatId,
    ) -> bool {
        let Some(view) = self.bot_view(bot) else {
            return false;
        };
        if view.lifecycle_busy() {
            return false;
        }
        let Some((title, body, verb)) = view.conversation_trash_words(chat) else {
            return false;
        };
        let lang = crate::format::lang();
        self.confirm = Some((
            koru_ui::Confirm::localized(title, body, verb, t(lang, Words::BotsDialogCancel))
                .defaulting_to_confirm(),
            ConfirmSubject::TrashBotConversation { bot, chat },
        ));
        true
    }

    /// The person answered a Bot's question: a yes goes to its tab, which
    /// sends it; a no sends nothing.
    pub(super) fn settle_bot_confirm(&mut self, subject: &ConfirmSubject, confirmed: bool) {
        if !confirmed {
            return;
        }
        match subject {
            ConfirmSubject::BotLifecycle { bot, ask } => {
                if let Some(view) = self.bot_view_mut(*bot) {
                    view.begin_lifecycle(*ask);
                }
            }
            ConfirmSubject::TrashBotConversation { bot, chat } => {
                if let Some(view) = self.bot_view_mut(*bot) {
                    view.trash_conversation(*chat);
                }
            }
            _ => {}
        }
    }

    /// **The Trash's Bot rows** (S14): the feed reads the Bot owner's Trash
    /// filter while a Trash tab is in front of its group, and only then;
    /// its Restores are recorded in the book and said in a receipt; every
    /// Trash tab is given the rows, as it is given the bin's runs.
    pub(super) fn maintain_bot_trash(&mut self) -> bool {
        let shown = crate::bots::shown()
            && self
                .panes
                .iter_groups()
                .any(|(_, group)| matches!(group.active_tab().view, TabView::Trash(_)));
        let workspace = self
            .current_workspace()
            .map(|id| koru_store_core::ids::WorkspaceId::from_uuid(id.as_uuid()));
        self.bot_trash.set_lang(crate::format::lang());
        let mut changed = self.bot_trash.set_workspace(workspace);
        self.bot_trash
            .set_shown(shown, crate::search::gateway_root());
        changed |= self.bot_trash.maintain();
        for done in self.bot_trash.take_finished() {
            self.bot_book.record(&done);
            // The Bot's tab, wherever it is, shows what it holds now.
            if let Some(view) = self.bot_view_mut(done.bot) {
                view.lifecycle_moved_elsewhere();
            }
            self.explore.bots_mut().owe_lifecycle_refresh();
            changed = true;
        }
        if let Some(receipt) = self.bot_trash.take_receipts().pop() {
            self.toast = Some((
                koru_ui::Toast::new(receipt.words, None, std::time::Instant::now()),
                super::ToastPayload::None,
            ));
            changed = true;
        }
        if self.has_trash_tabs() {
            let rows = if crate::bots::shown() {
                self.bot_trash.rows(&self.bot_book)
            } else {
                Vec::new()
            };
            for (_, group) in self.panes.iter_groups_mut() {
                for tab in group.tabs_mut().iter_mut() {
                    if let TabView::Trash(view) = &mut tab.view {
                        changed |= view.set_bots(rows.clone());
                    }
                }
            }
        }
        changed
    }

    /// **Restore Bot rows from the Trash**: each through the feed, which
    /// names the Trash the book knows. A row with no Restore sends nothing.
    pub(super) fn restore_trashed_bots(
        &mut self,
        subjects: Vec<crate::bots::trash::BotSubject>,
    ) -> bool {
        let mut sent = false;
        for subject in subjects {
            sent |= self.bot_trash.restore(subject, &self.bot_book);
        }
        sent
    }

    /// The Bot rows among a Trash request's rows, and the rest: the
    /// active Trash tab's own split, by its rows' identities.
    pub(super) fn split_trash_bots(
        &self,
        nodes: Vec<koru_core::NodeId>,
    ) -> (Vec<crate::bots::trash::BotSubject>, Vec<koru_core::NodeId>) {
        match &self.tabs().active_tab().view {
            TabView::Trash(view) => view.split_bots(nodes),
            _ => (Vec::new(), nodes),
        }
    }

    /// How many Bot rows the Trash shows: what Empty Trash leaves, and says.
    pub(super) fn trashed_bot_rows(&self) -> usize {
        if !crate::bots::shown() {
            return 0;
        }
        self.bot_trash.rows(&self.bot_book).len()
    }

    /// **Keep the book, and hand it back**: every operation a Bot tab
    /// finished since the last pass is recorded, and each Bot tab learns
    /// what the book knows holds its Bot. Every tab, hidden ones too — it
    /// is a fold of what they already hold, and nothing is sent. `true`
    /// when anything finished.
    pub(super) fn record_bot_lifecycle(&mut self) -> bool {
        let mut finished = Vec::new();
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::Bot(view) = &mut tab.view {
                    finished.extend(view.take_lifecycle_finished());
                }
            }
        }
        let any = !finished.is_empty();
        let mut moved = Vec::new();
        for done in &finished {
            if self.bot_book.record(done) {
                moved.push(done.bot);
            }
        }
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::Bot(view) = &mut tab.view
                    && let Some(bot) = view.bot()
                {
                    let (archive, trash) = self.bot_book.sources(bot);
                    view.know_lifecycle_sources(archive, trash);
                }
            }
        }
        if any {
            // The roster's lists and the Trash's Bot rows are old now.
            self.explore.bots_mut().owe_lifecycle_refresh();
            self.bot_trash.owe_refresh();
        }
        any
    }
}

#[cfg(test)]
mod tests;
