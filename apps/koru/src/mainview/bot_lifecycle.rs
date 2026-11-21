// SPDX-License-Identifier: MPL-2.0

//! **A Bot's lifecycle, from the window's side** (Bots P1.8 S14).
//!
//! The navigator offers each Bot and conversation's lifecycle actions; the
//! Bot tab follows what it sends. **The window owns the menu and the question**, through the same
//! `ContextMenu` and `Confirm` every other menu and question in it uses, so
//! no gesture reaches the owner without being asked first. On a yes the tab
//! sends the command; on a no nothing is sent; either way the keyboard goes
//! back to the control that asked.
//!
//! **What a Restore names is the owner's to say**: a Bot's reads and the
//! Archived and Trash filters name the operation that holds it
//! (`Bot.held_by`), and the Trash's conversation listing names what hid each
//! one (`trashed_by`), so an Unarchive or a Restore works from any surface
//! and after a restart. The window keeps no record of its own: an operation
//! a tab finishes only tells the Bot's other tabs, the roster and the Trash
//! to read again.
//!
//! **Cancel is the default when the question will stop a reply running
//! now** (QUESTIONS O46): the Bot or the conversation comes back, the reply
//! does not. What is running is what the window already knows when it asks
//! — the asking tab's own conversation and the roster's activity read while
//! fresh (`bots::stops`) — and when it does not know, Cancel too. Only a
//! known idle keeps Enter confirming (ADR-265's row for a reversible,
//! explicitly invoked action).

use koru_ai_core::bots::BotId;
use koru_i18n::{Key as Words, t};

use super::MainView;
use super::context_menu::{ContextMenu, MenuAction, MenuSubject};
use super::tab::TabView;
use super::trash::ConfirmSubject;
use crate::bots::BotView;
use crate::bots::lifecycle::Ask;
use crate::bots::stops::{self, Facts, Scope, WorkNow};

impl MainView {



    /// **The tab that asked**, by its own identity (`Tab::id`), while it is
    /// still `bot`'s: a menu or a question is answered to the tab it came
    /// from, never to whichever of the Bot's tabs is in front. `None` when
    /// that tab closed or moved to another Bot meanwhile.
    pub(super) fn asking_bot_tab(&self, tab: u64, bot: BotId) -> Option<&BotView> {
        self.panes.iter_groups().find_map(|(_, group)| {
            group.tabs().iter().find_map(|open| match &open.view {
                TabView::Bot(view) if open.id == tab && view.bot() == Some(bot) => Some(view),
                _ => None,
            })
        })
    }

    /// The tab that asked, mutably ([`Self::asking_bot_tab`]).
    pub(super) fn asking_bot_tab_mut(&mut self, tab: u64, bot: BotId) -> Option<&mut BotView> {
        self.panes.iter_groups_mut().find_map(|(_, group)| {
            group
                .tabs_mut()
                .iter_mut()
                .find_map(|open| match &mut open.view {
                    TabView::Bot(view) if open.id == tab && view.bot() == Some(bot) => Some(view),
                    _ => None,
                })
        })
    }

    /// **Every tab of `bot`** is told it changed elsewhere — the Trash
    /// restored it, the roster unarchived it — so none keeps showing what
    /// it held before. `true` when any tab was told.
    pub(super) fn bot_moved_elsewhere(&mut self, bot: BotId) -> bool {
        let mut told = false;
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::Bot(view) = &mut tab.view
                    && view.bot() == Some(bot)
                {
                    view.lifecycle_moved_elsewhere();
                    told = true;
                }
            }
        }
        told
    }

    /// **A Restore or an Unarchive made outside the Bot's tab finished** —
    /// from the Trash, or the roster's Archived filter: every tab of `bot`
    /// shows what it said of the Bot's Routines, as the tab's own would.
    pub(super) fn bot_routines_restored(
        &mut self,
        bot: BotId,
        restored: &crate::bots::routines::control::Restored,
    ) {
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::Bot(view) = &mut tab.view
                    && view.bot() == Some(bot)
                {
                    view.routines_restored_elsewhere(restored.clone());
                }
            }
        }
    }

    /// **A Retry or an Abandon from the roster's Unfinished group answered
    /// for `request`**: every create tab holding that creation reads it
    /// again and follows its stage — made (it becomes the Bot's tab),
    /// abandoned (its draft again), or still unfinished. `true` when one
    /// was told.
    pub(super) fn creation_moved_elsewhere(
        &mut self,
        request: koru_ai_core::bots::BotRequestId,
    ) -> bool {
        let mut told = false;
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::Bot(view) = &mut tab.view {
                    told |= view.creation_moved_elsewhere(request);
                }
            }
        }
        told
    }

    /// **The Bot tab in front asked for its menu or a question** — its ⋯,
    /// a conversation row's menu or its Delete. Opened here, at the place
    /// the tab named, in window coordinates. `true` when one opened.
    pub(super) fn drain_bot_lifecycle_asks(&mut self) -> bool {
        let view_rect = self.focused_view_rect();
        let to_window = |at: (f32, f32)| (at.0 + view_rect.x, at.1 + view_rect.y);
        let asking = self.tabs_mut().active_mut();
        let tab = asking.id;
        let TabView::Bot(view) = &mut asking.view else {
            return false;
        };
        let Some(bot) = view.bot() else {
            return false;
        };
        let actions = view.take_actions_request();
        let rows = view.lifecycle_rows();
        let row_menu = view
            .take_conversation_menu()
            .map(|(chat, at)| (chat, at, view.can_trash_conversation(chat)));
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
            opened |= self.open_subject_menu(MenuSubject::Bot { bot, tab }, items, to_window(at));
        }
        if let Some((chat, at, can_trash)) = row_menu {
            // **Rename, as the Chats list offers it** for an ordinary chat —
            // the same entry — and the conversation's Trash.
            let mut menu = ContextMenu::new(
                &mut self.engine,
                MenuSubject::BotConversation { bot, chat, tab },
                to_window(at),
                if can_trash {
                    vec![MenuAction::RenameAiChat, MenuAction::TrashBotConversation]
                } else {
                    vec![MenuAction::RenameAiChat]
                },
            );
            let enabled = self.enabled_items(&menu);
            menu.step(&enabled, 1);
            self.menu = Some(menu);
            opened = true;
        }
        if let Some(chat) = trash {
            opened |= self.ask_trash_bot_conversation(bot, tab, chat);
        }
        opened
    }

    /// **A row of the Bot's ⋯ was chosen**: Archive and Move to Trash are
    /// asked about first; Unarchive puts the Bot back and is sent at once.
    pub(super) fn choose_bot_lifecycle(
        &mut self,
        bot: BotId,
        tab: u64,
        action: MenuAction,
    ) -> bool {
        let ask = match action {
            MenuAction::ArchiveBot => Ask::Archive,
            MenuAction::TrashBot => Ask::Trash,
            MenuAction::UnarchiveBot => Ask::Unarchive,
            _ => return false,
        };
        let work = self.bot_work_now(tab, Scope::Bot(bot));
        let Some(view) = self.asking_bot_tab_mut(tab, bot) else {
            return self.say_asking_tab_gone();
        };
        let Some((title, body, verb)) = view.confirm_words(ask, work) else {
            let sent = view.begin_lifecycle(ask);
            view.dialog_closed();
            return sent;
        };
        let lead = self
            .asking_bot_tab(tab, bot)
            .and_then(|view| view.stops_words(work, false));
        self.confirm = Some((
            bot_question(title, body, verb, lead, work),
            ConfirmSubject::BotLifecycle { bot, tab, ask },
        ));
        true
    }

    /// **What an Archive or a Trash of `scope` would stop**, from what the
    /// window knows now and nothing it would have to ask: the asking tab's
    /// own conversation and the roster's activity read while fresh
    /// (`BotsRoster::activity_now`).
    pub(super) fn bot_work_now(&self, tab: u64, scope: Scope) -> WorkNow {
        let bot = match scope {
            Scope::Bot(bot) | Scope::Conversation(bot, _) => bot,
        };
        let facts = Facts {
            own: self
                .asking_bot_tab(tab, bot)
                .and_then(|view| view.own_work()),
            activity: self.explore.bots().activity_now(std::time::Instant::now()),
        };
        stops::work_now(scope, facts)
    }

    /// **Ask before one conversation goes to Trash** — a row's menu or its
    /// Delete. The main conversation's question says the Bot keeps working
    /// without it.
    pub(super) fn ask_trash_bot_conversation(
        &mut self,
        bot: BotId,
        tab: u64,
        chat: koru_ai_ui::ChatId,
    ) -> bool {
        let Some(view) = self.asking_bot_tab(tab, bot) else {
            return self.say_asking_tab_gone();
        };
        if view.lifecycle_busy() {
            return false;
        }
        // A row's Delete opens this question: Delete then Enter must not
        // stop a reply running in that conversation.
        let work = self.bot_work_now(tab, Scope::Conversation(bot, chat));
        let Some(view) = self.asking_bot_tab(tab, bot) else {
            return self.say_asking_tab_gone();
        };
        let Some((title, body, verb)) = view.conversation_trash_words(chat, work) else {
            return false;
        };
        let lead = view.stops_words(work, true);
        self.confirm = Some((
            bot_question(title, body, verb, lead, work),
            ConfirmSubject::TrashBotConversation { bot, tab, chat },
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
            ConfirmSubject::BotLifecycle { bot, tab, ask } => {
                match self.asking_bot_tab_mut(*tab, *bot) {
                    Some(view) => {
                        view.begin_lifecycle(*ask);
                    }
                    None => {
                        self.say_asking_tab_gone();
                    }
                }
            }
            ConfirmSubject::TrashBotConversation { bot, tab, chat } => {
                match self.asking_bot_tab_mut(*tab, *bot) {
                    Some(view) => {
                        view.trash_conversation(*chat);
                    }
                    None => {
                        self.say_asking_tab_gone();
                    }
                }
            }
            _ => {}
        }
    }

    /// **The tab a question was asked for is gone** — closed, or moved to
    /// another Bot — before the person answered it: nothing is sent, and
    /// that is said, never dropped in silence. `true`: the toast changed.
    pub(super) fn say_asking_tab_gone(&mut self) -> bool {
        let lang = crate::format::lang();
        self.toast = Some((
            koru_ui::Toast::new(
                t(lang, Words::BotsAskingTabGone).to_owned(),
                None,
                std::time::Instant::now(),
            ),
            super::ToastPayload::None,
        ));
        true
    }

    /// **The Trash's Bot rows** (S14): the feed reads the Bot owner's Trash
    /// filter while a Trash tab is in front of its group, and only then;
    /// its Restores tell the Bot's tabs and are said in a receipt; every
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
            // Every tab of the Bot, wherever it is, shows what it holds now.
            self.bot_moved_elsewhere(done.bot);
            if done.chat.is_none() {
                self.bot_routines_restored(done.bot, &done.routines);
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
        // A link's receipt said only *in the Trash* while the owner's
        // listing was read: said again, knowing what a Restore can do.
        if let Some((chat, plan)) = self.bot_trash.take_looked_up() {
            changed |= self.say_link_looked_up(chat, plan);
        }
        if self.has_trash_tabs() {
            let shown = crate::bots::shown();
            let rows = if shown {
                self.bot_trash.rows()
            } else {
                Vec::new()
            };
            let more = shown && self.bot_trash.more_than_listed();
            for (_, group) in self.panes.iter_groups_mut() {
                for tab in group.tabs_mut().iter_mut() {
                    if let TabView::Trash(view) = &mut tab.view {
                        changed |= view.set_bots(rows.clone());
                        changed |= view.set_bots_more(more);
                    }
                }
            }
        }
        changed
    }

    /// **Restore Bot rows from the Trash**: each through the feed, which
    /// names what the owner says hid it. A row with no Restore sends nothing.
    pub(super) fn restore_trashed_bots(
        &mut self,
        subjects: Vec<crate::bots::trash::BotSubject>,
    ) -> bool {
        let mut sent = false;
        for subject in subjects {
            sent |= self.bot_trash.restore(subject);
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
        self.bot_trash.rows().len()
    }

    /// **What the Bot tabs finished, told around**: every operation a Bot
    /// tab finished since the last pass makes the same Bot's other tabs, the
    /// roster's lists and the Trash's rows read again — each learns what
    /// holds the Bot from the owner, not from the tab. Nothing is sent from
    /// a hidden tab: it reads when next shown. `true` when anything finished.
    pub(super) fn record_bot_lifecycle(&mut self) -> bool {
        let mut finished = Vec::new();
        for (ordinal, (_, group)) in self.panes.iter_groups_mut().enumerate() {
            for (index, tab) in group.tabs_mut().iter_mut().enumerate() {
                if let TabView::Bot(view) = &mut tab.view {
                    for done in view.take_lifecycle_finished() {
                        finished.push((done.bot, (ordinal, index)));
                    }
                }
            }
        }
        if finished.is_empty() {
            return false;
        }
        for (ordinal, (_, group)) in self.panes.iter_groups_mut().enumerate() {
            for (index, tab) in group.tabs_mut().iter_mut().enumerate() {
                if let TabView::Bot(view) = &mut tab.view
                    && let Some(bot) = view.bot()
                    && finished
                        .iter()
                        .any(|(done, from)| *done == bot && *from != (ordinal, index))
                {
                    view.lifecycle_moved_elsewhere();
                }
            }
        }
        // The roster's lists and the Trash's Bot rows are old now.
        self.explore.bots_mut().owe_lifecycle_refresh();
        self.bot_trash.owe_refresh();
        true
    }
}

/// **A Bot's Archive or Trash question**: its words, with the paragraph
/// saying a reply will be stopped above the body when there is one, and
/// **the default answer by what it would stop** — Cancel unless the window
/// knows nothing is running. Escape cancels either way and Tab reaches both
/// answers (`koru_ui::Confirm`).
fn bot_question(
    title: String,
    body: String,
    verb: String,
    lead: Option<String>,
    work: WorkNow,
) -> koru_ui::Confirm {
    let lang = crate::format::lang();
    // Two whole paragraphs, each its own sentence in every language: the
    // stop, then what always happens.
    let body = match lead {
        Some(lead) => format!("{lead}\n\n{body}"),
        None => body,
    };
    let question = koru_ui::Confirm::localized(title, body, verb, t(lang, Words::BotsDialogCancel));
    if work.cancel_by_default() {
        // Cancel, the type's own default: a reply running now — or work the
        // window cannot rule out — would be stopped for good.
        question
    } else {
        // Reversible, explicitly invoked, and nothing running: ADR-265's
        // second row. Archive and Trash both come back, paused.
        question.defaulting_to_confirm()
    }
}

#[cfg(test)]
mod tests;
