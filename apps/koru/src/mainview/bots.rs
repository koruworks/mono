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

    /// The titles the window knows for the pages the Bot tabs in front
    /// list, from the entity titles the search service has given it.
    fn bot_page_titles(&self) -> std::collections::HashMap<koru_ai_core::DocumentId, String> {
        let mut titles = std::collections::HashMap::new();
        for (_, group) in self.panes.iter_groups() {
            if let TabView::Bot(view) = &group.active_tab().view {
                for page in view.listed_pages() {
                    let key =
                        koru_core::DocumentId::from_uuid(page.document.as_uuid()).to_storage();
                    if let Some(title) = self.entity_titles.get(&key) {
                        titles.insert(page.document, title.clone());
                    }
                }
            }
        }
        titles
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

    /// **A Bot tab's *Change…*** (Bots P1.8 S5): the window's existing
    /// folder dialog, the one an agent chat's working folder uses; its answer
    /// comes back through [`Self::bot_folder_chosen`] to the same tab.
    pub(super) fn ask_pending_bot_folder(&mut self) {
        let TabView::Bot(view) = self.active_view() else {
            return;
        };
        let Some(start) = view.take_folder_request() else {
            return;
        };
        let chat = view.chat_id();
        self.ai_chat_folder_ask = Some((super::AiChatFolderSource::BotTab, chat));
        self.ask_pane(koru_workspace_host::PaneRequest::ChooseAiChatFolder {
            title: koru_i18n::t(crate::format::lang(), koru_i18n::Key::BotsFolder).to_owned(),
            start,
        });
    }

    /// The folder dialog answered a Bot tab: the change goes to that tab's
    /// Bot owner, wherever the tab now is. The keyboard stays where it was.
    pub(super) fn bot_folder_chosen(
        &mut self,
        chat: koru_ai_ui::ChatId,
        chosen: &std::path::Path,
    ) -> bool {
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.iter_tabs_mut() {
                if let TabView::Bot(view) = &mut tab.view
                    && view.chat_id() == chat
                {
                    return view.folder_chosen(chosen);
                }
            }
        }
        false
    }

    /// **Feed a Bot tab's *Add a page…* picker** (S5): the scope — the
    /// Bot's home workspace alone — the window's recents while nothing is
    /// typed, and the search it owes, through the search service the `@`
    /// list and the Tasks page's picker use. Nothing when it is not up.
    pub(super) fn sync_bot_page_picker(&mut self) -> bool {
        if !matches!(&self.tabs().active_tab().view, TabView::Bot(b) if b.page_picker_open()) {
            return false;
        }
        let workspaces = self.entity_workspaces.clone();
        // The window's recents are its own workspace's: offered only when
        // that is the Bot's home, so no other workspace's page is offered.
        let home = match &self.tabs().active_tab().view {
            TabView::Bot(view) => view.facts().map(|facts| facts.home.as_uuid()),
            _ => None,
        };
        let recents = if home.is_some() && self.current_workspace().map(|id| id.as_uuid()) == home {
            self.recent_document_choices()
        } else {
            Vec::new()
        };
        let TabView::Bot(view) = self.active_view() else {
            return false;
        };
        let mut changed = view.set_picker_context(workspaces);
        changed |= view.feed_picker_recents(recents);
        let Some(request) = view.take_picker_search() else {
            return changed;
        };
        if request.query.trim().is_empty() {
            return changed;
        }
        let scope = request
            .workspaces
            .into_iter()
            .map(|id| koru_store_core::ids::WorkspaceId::from_uuid(id.as_uuid()))
            .collect();
        self.entity_search.request(request.query, scope);
        true
    }

    /// **A Bot tab's *Open role page***: the Role is an ordinary page, so it
    /// opens as an ordinary page tab, through the door every document link
    /// uses.
    pub(super) fn open_pending_bot_role(&mut self) -> bool {
        let TabView::Bot(view) = self.active_view() else {
            return false;
        };
        let Some(role) = view.take_role_request() else {
            return false;
        };
        let target = koru_editor::trigger::DocumentTarget {
            document: koru_core::DocumentId::from_uuid(role.document.as_uuid()),
            node: None,
        };
        self.open_document_target(target, koru_editor::OpenWhere::NewTab)
    }

    /// **Keep every Bot tab's words current** — its label from its Bot's
    /// name, the header's workspace from the catalog, the titles of its
    /// selected pages — and **tell every Bot tab that is not in front of its
    /// group that it is hidden**, so it reads its Bot once when it is shown
    /// again. A comparison per Bot tab when nothing moved. `true` when the
    /// strip owes a remeasure.
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
        let titles = self.bot_page_titles();
        let mut moved = false;
        for (_, group) in self.panes.iter_groups_mut() {
            let mut relabeled = false;
            let active = group.active();
            for (index, tab) in group.iter_tabs_mut().enumerate() {
                if let TabView::Bot(view) = &mut tab.view {
                    if index != active {
                        view.hidden();
                    }
                    if let Some(home) = view.facts().map(|facts| facts.home)
                        && let Some((_, name)) = names.iter().find(|(id, _)| *id == home)
                    {
                        view.set_home_name(name.clone());
                    }
                    if index == active {
                        let mine = view
                            .listed_pages()
                            .iter()
                            .filter_map(|page| {
                                titles
                                    .get(&page.document)
                                    .map(|title| (page.document, title.clone()))
                            })
                            .collect();
                        view.set_page_titles(mine);
                    }
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
