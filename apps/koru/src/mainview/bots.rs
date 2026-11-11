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
//! tab** when the conversation is a Bot's. The window knows that from the
//! roster's main conversations and from every `Chat` label it has learned
//! (`bots::labels`: a Bot tab's listed conversations, the roster's activity,
//! a listed chat's label). **Where it does not know yet, it does not
//! guess**: it asks the Bot owner once, off its thread, and shows a neutral
//! *Opening this conversation…* tab ([`BotView::resolving`]) that becomes
//! the Bot's tab on that conversation, or an ordinary chat tab in the same
//! place ([`MainView::place_opening_tabs`]). A chat the window knows is
//! ordinary — the Chats list's, the Koru AI pane's recents — opens exactly
//! as it always did, with nothing asked.

use super::MainView;
use super::tab::{Tab, TabView};
use crate::bots::BotView;
use crate::bots::labels::{Known, Resolved};
use crate::bots::roster::RosterBot;

impl MainView {
    /// The Bot whose main conversation is `chat`, as far as the window knows.
    pub(super) fn bot_for_chat(&self, chat: koru_ai_ui::ChatId) -> Option<RosterBot> {
        self.explore.bots().bot_for_chat(chat).cloned()
    }

    /// **A tab for an existing conversation that no tab shows yet**: the
    /// Bot's tab on that conversation when it is a Bot's, an ordinary chat
    /// tab when it is known not to be, and — when the window cannot tell yet
    /// — a tab that says it is opening while the window asks. The one
    /// constructor every door to an existing chat uses (a link, a search
    /// hit, the Chats list, the place chooser, a split, a restored session),
    /// so no door can open a Bot's conversation as an ordinary chat.
    pub(super) fn chat_tab(
        &mut self,
        id: koru_ai_ui::ChatId,
        block: Option<koru_ai_ui::MessageBlockId>,
    ) -> Tab {
        if !crate::bots::shown() {
            return Tab::ai_chat_existing_at(id, block);
        }
        if let Some(bot) = self.bot_for_chat(id) {
            return self.bot_tab_on(&bot, id, block);
        }
        match self.known_chat(id) {
            Some(Known::Bot { bot, name }) => match self.roster_bot(bot) {
                Some(listed) => self.bot_tab_on(&listed, id, block),
                None => Tab::bot(BotView::labelled(bot, id, &name, block)),
            },
            Some(Known::Ordinary) => Tab::ai_chat_existing_at(id, block),
            // Not known: ask, and say it is opening. With nothing to ask
            // through — under test, or no thread — it opens as it always
            // did.
            None if self.bot_labels.can_ask(crate::search::gateway_root()) => {
                self.bot_labels.ask(id, crate::search::gateway_root());
                Tab::bot(BotView::resolving(id, block))
            }
            None => Tab::ai_chat_existing_at(id, block),
        }
    }

    /// What the window knows about whose `chat` is: a label it learned, or
    /// a chat one of its ordinary lists holds — those lists' reader cannot
    /// see a Bot's conversation, so a chat they hold without a label is an
    /// ordinary one.
    fn known_chat(&mut self, chat: koru_ai_ui::ChatId) -> Option<Known> {
        if let Some(known) = self.bot_labels.known(chat) {
            return Some(known.clone());
        }
        if let Some(listed) = self.explore.chats().chats().iter().find(|c| c.id == chat) {
            let label = listed.bot.clone();
            self.bot_labels.learn_label(chat, label.as_ref());
            return self.bot_labels.known(chat).cloned();
        }
        if self
            .ai_chat
            .recent_chats(usize::MAX)
            .iter()
            .any(|(listed, _)| *listed == chat)
        {
            self.bot_labels.learn_ordinary(chat);
            return Some(Known::Ordinary);
        }
        None
    }

    /// The roster's row for `bot`, if it lists it.
    fn roster_bot(&self, bot: koru_ai_core::bots::BotId) -> Option<RosterBot> {
        self.explore
            .bots()
            .bots()
            .iter()
            .find(|row| row.id == bot)
            .cloned()
    }

    fn bot_tab(&self, bot: &RosterBot) -> Tab {
        self.bot_tab_on(bot, bot.main_chat, None)
    }

    fn bot_tab_on(
        &self,
        bot: &RosterBot,
        chat: koru_ai_ui::ChatId,
        block: Option<koru_ai_ui::MessageBlockId>,
    ) -> Tab {
        let mut view = BotView::opened_on(bot, chat, block);
        view.set_home_name(self.workspace_name(bot.home));
        Tab::bot(view)
    }

    /// **Place every tab that was opened before the window knew whose its
    /// conversation is** (B1), from what the label read answered: it
    /// becomes its Bot's tab, or an ordinary chat tab in the same place —
    /// the same tab id, pin and position. A read that did not answer leaves
    /// the tab saying the conversation cannot be opened; shown again, it is
    /// asked once more. Every tab, not only the one in front: the read is
    /// the one an ordinary chat tab made of its own before (its reader asks
    /// for its chat when the tab is made). `true` when a strip owes a
    /// remeasure or a repaint.
    pub(super) fn place_opening_tabs(&mut self) -> bool {
        let root = crate::search::gateway_root();
        let failed: Vec<koru_ai_ui::ChatId> = self
            .bot_labels
            .poll()
            .into_iter()
            .filter(|(_, resolved)| *resolved == Resolved::Unavailable)
            .map(|(chat, _)| chat)
            .collect();
        let can_ask = self.bot_labels.can_ask(root.clone());
        let roster: Vec<RosterBot> = self.explore.bots().bots().to_vec();
        let mut asks = Vec::new();
        let mut moved = false;
        for (_, group) in self.panes.iter_groups_mut() {
            let active = group.active();
            let mut relabeled = false;
            for (index, tab) in group.iter_tabs_mut().enumerate() {
                let TabView::Bot(view) = &mut tab.view else {
                    continue;
                };
                if view.bot().is_some() {
                    continue;
                }
                let chat = view.chat_id();
                let known = if can_ask {
                    self.bot_labels.known(chat).cloned()
                } else {
                    Some(Known::Ordinary)
                };
                match known {
                    Some(Known::Bot { bot, name }) => {
                        view.resolved(bot, &name);
                        if let Some(listed) = roster.iter().find(|row| row.id == bot) {
                            view.refresh_from(listed);
                        }
                        relabeled |= tab.refresh_bot_label();
                        moved = true;
                    }
                    Some(Known::Ordinary) => {
                        let block = view.pending_block();
                        become_ordinary_chat(tab, chat, block);
                        relabeled = true;
                    }
                    None if failed.contains(&chat) => {
                        view.opening_failed();
                        moved = true;
                    }
                    None if index == active && view.take_opening_retry() => {
                        asks.push(chat);
                        moved = true;
                    }
                    None if !self.bot_labels.asking(chat)
                        && view.opening() == Some(crate::bots::view::Opening::Asking) =>
                    {
                        // Not asked yet: the queue was full when it opened.
                        asks.push(chat);
                    }
                    None => {}
                }
            }
            if relabeled {
                group.mark_widths_stale();
                moved = true;
            }
        }
        for chat in asks {
            self.bot_labels.ask(chat, root.clone());
        }
        moved
    }

    /// The window's label read, for a test to hold the other end of.
    #[cfg(test)]
    pub(super) fn bot_labels_mut(&mut self) -> &mut crate::bots::labels::ChatLabels {
        &mut self.bot_labels
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
        // The tab on its main conversation, else the Bot's tab on another of
        // its conversations: one Bot, one tab the roster brings forward.
        let find = |of: &dyn Fn(&Tab) -> bool| {
            self.panes.iter_groups().find_map(|(path, group)| {
                group
                    .tabs()
                    .iter()
                    .position(|tab| of(tab))
                    .map(|index| (path, index))
            })
        };
        let showing = find(&|tab: &Tab| tab.view.chat_id() == Some(chat)).or_else(|| {
            find(&|tab: &Tab| matches!(&tab.view, TabView::Bot(view) if view.bot() == Some(bot.id)))
        });
        if let Some((path, index)) = showing {
            let here = self.panes.focused_path() == &path && self.tabs().active() == index;
            if !here {
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
        let placed = self.place_opening_tabs();
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
        moved || placed
    }
}

/// **A tab that turned out to hold an ordinary chat** takes the ordinary
/// chat tab's view and name, keeping its place: the same id (so Back and the
/// strip's order hold), its pin and its home.
fn become_ordinary_chat(
    tab: &mut Tab,
    chat: koru_ai_ui::ChatId,
    block: Option<koru_ai_ui::MessageBlockId>,
) {
    let mut ordinary = Tab::ai_chat_existing_at(chat, block);
    ordinary.id = tab.id;
    ordinary.home = tab.home;
    ordinary.pinned = tab.pinned;
    ordinary.show_title = tab.show_title;
    *tab = ordinary;
    tab.invalidate_size();
}

#[cfg(test)]
mod tests;
