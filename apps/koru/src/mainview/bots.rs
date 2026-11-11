// SPDX-License-Identifier: MPL-2.0

//! **Bot tabs in the window** (Bots P1.8 S4): opening one from the roster,
//! and routing a Bot's conversation to its Bot tab wherever else it is
//! opened from.
//!
//! **One conversation, one container.** Every door to a chat — the roster, a
//! chat link, a search hit, the place chooser, a split — first asks every
//! pane for a tab already on that `ChatId` (`TabView::chat_id`, which a Bot
//! tab answers with its conversation before anything is read). Only when
//! none is found is a tab made, and [`MainView::chat_tab`] makes it a **Bot
//! tab** when the window knows the conversation is a Bot's: the roster's
//! Bots and their main conversations. Where the window cannot know — a chat
//! that is not a Bot's main conversation, or any Bot conversation before the
//! engine labels a `Chat` with its Bot (QUESTIONS O33) — it does not guess:
//! the ordinary tab opens, and the ordinary reader, which holds no
//! `bots.manage`, cannot read a Bot's conversation into it.

use super::MainView;
use super::tab::{Tab, TabView};
use crate::bots::BotView;
use crate::bots::roster::RosterBot;

impl MainView {
    /// The Bot whose main conversation is `chat`, as far as the window knows.
    pub(super) fn bot_for_chat(&self, chat: koru_ai_ui::ChatId) -> Option<RosterBot> {
        self.explore.bots().bot_for_chat(chat).cloned()
    }

    /// **A tab for an existing conversation that no tab shows yet**: the
    /// Bot's tab when the conversation is a Bot's the window knows, an
    /// ordinary chat tab otherwise. The one constructor every door to an
    /// existing chat uses, so no door can open a Bot's conversation as an
    /// ordinary chat.
    pub(super) fn chat_tab(
        &self,
        id: koru_ai_ui::ChatId,
        block: Option<koru_ai_ui::MessageBlockId>,
    ) -> Tab {
        match self.bot_for_chat(id) {
            Some(bot) if crate::bots::shown() => self.bot_tab(&bot),
            _ => Tab::ai_chat_existing_at(id, block),
        }
    }

    fn bot_tab(&self, bot: &RosterBot) -> Tab {
        let mut view = BotView::opened(bot);
        view.set_home_name(self.workspace_name(bot.home));
        Tab::bot(view)
    }

    /// The name the window's catalog gives `workspace`, if it holds it.
    pub(super) fn workspace_name(
        &self,
        workspace: koru_store_core::ids::WorkspaceId,
    ) -> Option<String> {
        self.workspaces()
            .iter()
            .find(|entry| entry.id.as_uuid() == workspace.as_uuid())
            .map(|entry| entry.name.clone())
    }

    /// **Open `bot` — the roster's press — or focus the tab that already
    /// shows its conversation**, in any pane. A tab found is told what the
    /// roster now says about the Bot; nothing is ever opened twice.
    pub(super) fn open_bot_tab(&mut self, bot: RosterBot) -> bool {
        if !crate::bots::shown() {
            return false;
        }
        let chat = bot.main_chat;
        let showing = self.panes.iter_groups().find_map(|(path, group)| {
            group
                .tabs()
                .iter()
                .position(|tab| tab.view.chat_id() == Some(chat))
                .map(|index| (path, index))
        });
        if let Some((path, index)) = showing {
            if self.tabs().active_tab().view.chat_id() != Some(chat) {
                self.record_jump();
            }
            self.panes.set_focus(path);
            let _ = self.select(index);
            let home = self.workspace_name(bot.home);
            let tab = self.tabs_mut().active_mut();
            if let TabView::Bot(view) = &mut tab.view {
                view.refresh_from(&bot);
                view.set_home_name(home);
            }
            if tab.refresh_bot_label() {
                self.mark_tab_widths_stale();
            }
            return true;
        }
        self.record_jump();
        let tab = self.bot_tab(&bot);
        self.tabs_mut().push(tab);
        self.mark_tab_widths_stale();
        true
    }

    /// **Keep every Bot tab's words current** — its label from its Bot's
    /// name, the header's workspace from the catalog. A comparison per Bot
    /// tab when nothing moved. `true` when the strip owes a remeasure.
    pub(super) fn refresh_bot_tabs(&mut self) -> bool {
        let names: Vec<(koru_store_core::ids::WorkspaceId, Option<String>)> = self
            .panes
            .iter_groups()
            .flat_map(|(_, group)| group.iter_tabs())
            .filter_map(|tab| match &tab.view {
                TabView::Bot(view) => view.facts().map(|facts| facts.home),
                _ => None,
            })
            .map(|home| (home, self.workspace_name(home)))
            .collect();
        let mut moved = false;
        for (_, group) in self.panes.iter_groups_mut() {
            let mut relabeled = false;
            for tab in group.iter_tabs_mut() {
                if let TabView::Bot(view) = &mut tab.view
                    && let Some(home) = view.facts().map(|facts| facts.home)
                    && let Some((_, name)) = names.iter().find(|(id, _)| *id == home)
                {
                    view.set_home_name(name.clone());
                }
                relabeled |= tab.refresh_bot_label();
            }
            // Its own strip, whichever column it is in.
            if relabeled {
                group.mark_widths_stale();
                moved = true;
            }
        }
        moved
    }
}

#[cfg(test)]
mod tests;
