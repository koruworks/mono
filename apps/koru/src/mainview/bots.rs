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

/// **A chat a door was asked to open while Koru AI is off**, held until the
/// window learns whether it is a Bot's conversation
/// ([`MainView::chat_door_open`]): the chat and its block, the door, and
/// whether a read about it is on its way — `false` once a read went
/// unanswered, until the window is next activated and asks again.
pub(super) struct HeldChatOpen {
    target: koru_editor::ChatTarget,
    where_: koru_editor::OpenWhere,
    asking: bool,
}

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

    /// **Whether a door may open `target` now** — the one test both of the
    /// window's chat doors make (`open_ai_chat_tab_at`, `open_chat_where`).
    /// With Koru AI on, any chat. With it off, only a Bot's conversation:
    /// Bots follow agents alone (QUESTIONS O32). A conversation the window
    /// knows is a Bot's opens at once; one it knows is ordinary is dropped,
    /// exactly as every chat was with Koru AI off. **One it cannot place is
    /// held, not guessed**: the label read is asked once and nothing is
    /// shown — no tab that could turn out to be an ordinary chat — until it
    /// answers ([`Self::open_held_chats`]): a Bot's conversation then opens
    /// through the door it was asked by; an ordinary or missing chat is
    /// dropped. **A read that does not answer drops nothing**: the open stays
    /// held and is asked about again when the window is next activated
    /// ([`Self::ask_held_chats_again`]) or the link is followed again. Without
    /// a label read to ask (under test, or no thread), nothing opens, as
    /// before.
    pub(super) fn chat_door_open(
        &mut self,
        target: koru_editor::ChatTarget,
        where_: koru_editor::OpenWhere,
    ) -> bool {
        if koru_features::ai() {
            return true;
        }
        let chat = koru_ai_ui::ChatId::from_uuid(target.chat);
        if self.known_bot_conversation(chat) {
            return true;
        }
        if !crate::bots::shown() || self.known_chat(chat).is_some() {
            return false;
        }
        let root = crate::search::gateway_root();
        if let Some(held) = self
            .held_chat_opens
            .iter_mut()
            .find(|held| held.target.chat == target.chat)
        {
            // Followed again: the latest door and block are the ones meant,
            // and a read that went unanswered is asked once more now.
            held.target = target;
            held.where_ = where_;
            if !held.asking {
                held.asking = self.bot_labels.ask(chat, root);
            }
            return false;
        }
        if !self.bot_labels.can_ask(root.clone()) {
            return false;
        }
        // A full queue still holds it: it is asked on the next activation.
        let asking = self.bot_labels.ask(chat, root);
        self.held_chat_opens.push(HeldChatOpen {
            target,
            where_,
            asking,
        });
        false
    }

    /// **The window was activated: ask again about every held chat whose
    /// read went unanswered** — once each, the rule the neutral tab follows
    /// when it is shown again. Nothing is asked for a chat already being
    /// asked about, and nothing at all while none is held.
    pub(super) fn ask_held_chats_again(&mut self) {
        let root = crate::search::gateway_root();
        for held in self.held_chat_opens.iter_mut().filter(|held| !held.asking) {
            held.asking = self.bot_labels.ask(
                koru_ai_ui::ChatId::from_uuid(held.target.chat),
                root.clone(),
            );
        }
    }

    /// **Open what was held while the window asked whose it is** (Koru AI
    /// off): each held chat the label read has now placed opens through the
    /// door it was asked by when it is a Bot's conversation, and is dropped
    /// when it is ordinary or does not exist — with Koru AI off a chat that
    /// might be ordinary is never shown. A read that did not answer keeps it
    /// held, to be asked again. `true` when a tab opened.
    fn open_held_chats(&mut self, answered: &[(koru_ai_ui::ChatId, Resolved)]) -> bool {
        if self.held_chat_opens.is_empty() {
            return false;
        }
        let mut opened = false;
        for (chat, resolved) in answered {
            let Some(index) = self
                .held_chat_opens
                .iter()
                .position(|held| held.target.chat == chat.as_uuid())
            else {
                continue;
            };
            match resolved {
                Resolved::Known(Known::Bot { .. }) => {
                    let held = self.held_chat_opens.remove(index);
                    opened |= self.open_chat_where(held.target, held.where_);
                }
                // Ordinary, or not found (the label read learns that as
                // ordinary): what every chat link does with Koru AI off.
                Resolved::Known(Known::Ordinary) => {
                    self.held_chat_opens.remove(index);
                    tracing::info!(
                        target: "koru::chat_link",
                        "chat link dropped: Koru AI is off and it is not a Bot's conversation"
                    );
                }
                Resolved::Unavailable => {
                    self.held_chat_opens[index].asking = false;
                    tracing::info!(
                        target: "koru::chat_link",
                        "chat link held: whose it is went unanswered; asked again on activation"
                    );
                }
                // In Trash (S14): said once, with Restore when the window
                // knows that Trash — never held and asked about again.
                Resolved::Trashed => {
                    self.held_chat_opens.remove(index);
                    self.say_link_in_trash(*chat);
                    tracing::info!(
                        target: "koru::chat_link",
                        "chat link not opened: the Bot conversation is in Trash"
                    );
                }
            }
        }
        opened
    }

    /// **A link led to a Bot conversation in Trash** (S14): a receipt says
    /// so. **Restore is offered only where one press restores** — the
    /// conversation's own Restore, or its Bot's when it went to the Trash
    /// with its Bot — as the owner's Trash listing says (`restore`,
    /// `trashed_by`, `bot_held_by`); otherwise the receipt says what must
    /// happen first. While the listing is read the receipt says only that
    /// it is in the Trash, and is said again once the owner answers
    /// ([`Self::say_link_looked_up`]).
    pub(super) fn say_link_in_trash(&mut self, chat: koru_ai_ui::ChatId) {
        self.bot_trash.start(crate::search::gateway_root());
        let plan = self.bot_trash.link_restore(chat);
        if plan.is_none() {
            self.bot_trash.look_up(chat);
        }
        self.say_link_plan(chat, plan);
    }

    /// The receipt for `chat` in Trash, by what its Restore can do.
    fn say_link_plan(
        &mut self,
        chat: koru_ai_ui::ChatId,
        plan: Option<crate::bots::trash::LinkRestore>,
    ) {
        let lang = crate::format::lang();
        let words = plan.map_or(koru_i18n::Key::BotsLinkInTrash, |plan| plan.words());
        let (action, payload) = match plan.and_then(|plan| plan.press()) {
            Some(bot) => (
                Some(koru_i18n::t(lang, koru_i18n::Key::TrashRestore)),
                super::ToastPayload::RestoreBotChat { bot, chat },
            ),
            None => (None, super::ToastPayload::BotChatInTrash { chat }),
        };
        self.toast = Some((
            koru_ui::Toast::new(
                koru_i18n::t(lang, words).to_owned(),
                action,
                std::time::Instant::now(),
            ),
            payload,
        ));
    }

    /// **The owner's listing answered a link's lookup**: the receipt still up
    /// for that conversation is said again, now knowing what a Restore can
    /// do. A receipt already replaced or dismissed is left alone.
    pub(super) fn say_link_looked_up(
        &mut self,
        chat: koru_ai_ui::ChatId,
        plan: Option<crate::bots::trash::LinkRestore>,
    ) -> bool {
        let still_up = matches!(
            &self.toast,
            Some((_, super::ToastPayload::BotChatInTrash { chat: up })) if *up == chat
        );
        if still_up && plan.is_some() {
            self.say_link_plan(chat, plan);
        }
        still_up
    }

    /// **Whether the window knows `chat` is a Bot's conversation** — the
    /// roster's main conversations, a learned label, or a Bot tab already on
    /// it. What lets a Bot's conversation through the window's Koru AI gates
    /// when that switch is off: Bots follow agents alone (QUESTIONS O32).
    /// A chat the window cannot place is asked about first
    /// ([`Self::chat_door_open`]).
    pub(super) fn known_bot_conversation(&self, chat: koru_ai_ui::ChatId) -> bool {
        if !crate::bots::shown() {
            return false;
        }
        self.bot_for_chat(chat).is_some()
            || matches!(self.bot_labels.known(chat), Some(Known::Bot { .. }))
            || self.panes.iter_groups().any(|(_, group)| {
                group.iter_tabs().any(|tab| {
                    matches!(&tab.view, TabView::Bot(view) if view.bot().is_some())
                        && tab.view.chat_id() == Some(chat)
                })
            })
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
        let answered = self.bot_labels.poll();
        let held = self.open_held_chats(&answered);
        let failed: Vec<koru_ai_ui::ChatId> = answered
            .iter()
            .filter(|(_, resolved)| *resolved == Resolved::Unavailable)
            .map(|(chat, _)| *chat)
            .collect();
        let trashed: Vec<koru_ai_ui::ChatId> = answered
            .into_iter()
            .filter(|(_, resolved)| *resolved == Resolved::Trashed)
            .map(|(chat, _)| chat)
            .collect();
        let mut said_trashed = Vec::new();
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
                // Known, or creating one (S8): nothing to place.
                if view.bot().is_some() || view.is_creating() {
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
                    // In Trash (S14): the tab says so and is not asked
                    // about again on its own.
                    None if trashed.contains(&chat) => {
                        view.opening_in_trash();
                        said_trashed.push(chat);
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
        if let Some(chat) = said_trashed.pop() {
            self.say_link_in_trash(chat);
        }
        moved || held
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

    /// **A running or Needs-you conversation the roster offered** (S10):
    /// the tab already on it, else the Bot's tab — brought forward and
    /// moved to that conversation — else a new Bot tab on it.
    pub(super) fn open_bot_conversation(
        &mut self,
        bot: RosterBot,
        chat: koru_ai_ui::ChatId,
    ) -> bool {
        if !crate::bots::shown() {
            return false;
        }
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
        self.record_jump();
        let Some((path, index)) = showing else {
            let tab = self.bot_tab_on(&bot, chat, None);
            self.tabs_mut().push(tab);
            self.mark_tab_widths_stale();
            return true;
        };
        self.panes.set_focus(path);
        let _ = self.select(index);
        let tab = self.tabs_mut().active_mut();
        if let TabView::Bot(view) = &mut tab.view {
            view.refresh_from(&bot);
            view.show_conversation(chat);
        }
        true
    }

    /// A fresh Routine reference has independently resolved its label on the
    /// Bot link. It must never take the generic ordinary-chat fallback or
    /// focus another Bot merely because its tab carries the same ChatId.
    pub(super) fn open_routine_conversation(
        &mut self,
        expected: koru_ai_core::bots::BotId,
        chat: koru_ai_ui::ChatId,
    ) -> bool {
        let showing = self.panes.iter_groups().find_map(|(path, group)| {
            group
                .tabs()
                .iter()
                .enumerate()
                .find(|(_, tab)| tab.view.chat_id() == Some(chat))
                .map(|(index, tab)| {
                    (
                        path,
                        index,
                        matches!(&tab.view,TabView::Bot(view) if view.bot()==Some(expected)),
                    )
                })
        });
        if let Some((path, index, owned)) = showing {
            if !owned {
                return false;
            }
            self.panes.set_focus(path);
            let _ = self.select(index);
            return true;
        }
        match self.active_view() {
            TabView::Bot(view) if view.bot() == Some(expected) => view.show_conversation(chat),
            _ => false,
        }
    }
    pub(super) fn drain_bot_routine_open(&mut self) -> bool {
        self.sync_bot_context();
        if !self.bot_context_current()
            || !self.bot_context_unobstructed()
            || self.context_view.section() != crate::panes::context::Section::Routines
        {
            if let TabView::Bot(view) = self.active_view() {
                let _ = view.take_routine_open();
            }
            return false;
        }
        let request = match self.active_view() {
            TabView::Bot(view) => view.take_routine_open(),
            _ => None,
        };
        let Some((bot, chat)) = request else {
            return false;
        };
        if !self.open_routine_conversation(bot, chat) {
            if let TabView::Bot(view) = self.active_view() {
                view.routines.invalidate();
            }
        } else {
            self.context_return_tab = None;
            self.bot_context.focus = None;
            if self.bot_context.floating {
                self.context.set_immediately(false);
                self.relayout();
            }
        }
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
        // The create form's folder (S8): its answer goes to the draft, found
        // by the BotId it reserves — it has no conversation yet.
        if let Some(draft) = view.create_bot() {
            self.bot_create_folder_ask = Some(draft);
            self.ai_chat_folder_ask = None;
            self.ask_pane(koru_workspace_host::PaneRequest::ChooseAiChatFolder {
                title: koru_i18n::t(crate::format::lang(), koru_i18n::Key::BotsFolder).to_owned(),
                start,
            });
            return;
        }
        let chat = view.chat_id();
        self.bot_create_folder_ask = None;
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

    /// **The folder dialog answered a create form** (S8): the folder joins
    /// that draft, wherever its tab now is, found by the BotId the draft
    /// reserves. Nothing is sent.
    pub(super) fn bot_create_folder_chosen(
        &mut self,
        draft: koru_ai_core::bots::BotId,
        chosen: &std::path::Path,
    ) -> bool {
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.iter_tabs_mut() {
                if let TabView::Bot(view) = &mut tab.view
                    && view.create_bot() == Some(draft)
                {
                    return view.create_folder_chosen(chosen);
                }
            }
        }
        false
    }

    /// **Create a bot** (S8) — the roster's heading or its empty state:
    /// a Bot tab in create mode for the window's workspace, or the create
    /// tab with nothing sent yet that is already open for it, brought
    /// forward rather than a second one beside it. `false` with no
    /// workspace to create in.
    pub(super) fn open_bot_create_tab(&mut self) -> bool {
        if !crate::bots::shown() {
            return false;
        }
        let Some(home) = self
            .current_workspace()
            .map(|id| koru_store_core::ids::WorkspaceId::from_uuid(id.as_uuid()))
        else {
            return false;
        };
        let open = self.panes.iter_groups().find_map(|(path, group)| {
            group
                .tabs()
                .iter()
                .position(|tab| {
                    matches!(&tab.view, TabView::Bot(view)
                        if view.create_untouched() && view.home() == Some(home))
                })
                .map(|index| (path, index))
        });
        self.record_jump();
        if let Some((path, index)) = open {
            self.panes.set_focus(path);
            let _ = self.select(index);
            return true;
        }
        let view = BotView::creating(home, self.workspace_name(home));
        self.tabs_mut().push(Tab::bot(view));
        self.mark_tab_widths_stale();
        true
    }

    /// **The create tab in front asked to abandon its creation** (S8): the
    /// window asks first, saying nothing is deleted. `true` when it asked.
    pub(super) fn drain_bot_create_asks(&mut self) -> bool {
        let TabView::Bot(view) = &mut self.tabs_mut().active_mut().view else {
            return false;
        };
        let Some(draft) = view.create_bot() else {
            return false;
        };
        let Some(name) = view.take_abandon_ask() else {
            return false;
        };
        self.ask_abandon_creation(name, super::trash::AbandonFrom::Tab(draft))
    }

    pub(super) fn drain_bot_memory_asks(&mut self) -> bool {
        let TabView::Bot(view) = &mut self.tabs_mut().active_mut().view else {
            return false;
        };
        let Some(ask) = view.take_memory_ask() else {
            return false;
        };
        let lang = crate::format::lang();
        self.confirm = Some((
            koru_ui::Confirm::localized(
                koru_i18n::t(lang, koru_i18n::Key::BotsMemoryAbandonTitle).to_owned(),
                koru_i18n::t(lang, koru_i18n::Key::BotsMemoryAbandonBody).to_owned(),
                koru_i18n::t(lang, koru_i18n::Key::BotsAbandon).to_owned(),
                koru_i18n::t(lang, koru_i18n::Key::BotsDialogCancel),
            ),
            super::trash::ConfirmSubject::AbandonBotMemory(ask),
        ));
        true
    }

    pub(super) fn settle_bot_memory_ask(
        &mut self,
        ask: &crate::bots::view::memory::Ask,
        confirmed: bool,
    ) {
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.iter_tabs_mut() {
                if let TabView::Bot(view) = &mut tab.view {
                    if view.settle_memory_ask(ask, confirmed) {
                        return;
                    }
                }
            }
        }
    }

    /// **Ask before a creation is abandoned** — a create tab's or the
    /// roster's Unfinished row's Abandon: nothing is deleted, and what it
    /// leaves stays where it is.
    pub(super) fn ask_abandon_creation(
        &mut self,
        name: String,
        from: super::trash::AbandonFrom,
    ) -> bool {
        let lang = crate::format::lang();
        let shown = if name.trim().is_empty() {
            koru_i18n::t(lang, koru_i18n::Key::BotsNewBot).to_owned()
        } else {
            name
        };
        self.confirm = Some((
            koru_ui::Confirm::localized(
                koru_i18n::fill(
                    koru_i18n::t(lang, koru_i18n::Key::BotsAbandonTitle),
                    &[shown.as_str()],
                ),
                koru_i18n::t(lang, koru_i18n::Key::BotsAbandonBody).to_owned(),
                koru_i18n::t(lang, koru_i18n::Key::BotsAbandon).to_owned(),
                koru_i18n::t(lang, koru_i18n::Key::BotsDialogCancel),
            ),
            super::trash::ConfirmSubject::AbandonBotCreation(from),
        ));
        true
    }

    /// The person answered the Abandon question: a yes is sent by whoever
    /// asked; a no sends nothing; the keyboard goes back to the control
    /// that asked.
    pub(super) fn settle_abandon_creation(
        &mut self,
        from: super::trash::AbandonFrom,
        confirmed: bool,
    ) {
        match from {
            super::trash::AbandonFrom::Tab(draft) => {
                for (_, group) in self.panes.iter_groups_mut() {
                    for tab in group.iter_tabs_mut() {
                        if let TabView::Bot(view) = &mut tab.view
                            && view.create_bot() == Some(draft)
                        {
                            if confirmed {
                                view.abandon_confirmed();
                            }
                            view.dialog_closed();
                            return;
                        }
                    }
                }
            }
            super::trash::AbandonFrom::Roster(request) => {
                if confirmed {
                    self.explore.bots_mut().abandon(request);
                }
            }
        }
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
            TabView::Bot(view) => view.home().map(|home| home.as_uuid()),
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
        let Some(target) = view.take_document_request() else {
            return false;
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
        self.sync_bot_context();
        let placed = self.place_opening_tabs();
        // What every Bot tab's lifecycle finished, into the window's book,
        // and the book back into every tab (S14).
        let recorded = self.record_bot_lifecycle();
        let names: Vec<(koru_store_core::ids::WorkspaceId, Option<String>)> = self
            .panes
            .iter_groups()
            .flat_map(|(_, group)| group.iter_tabs())
            .filter_map(|tab| match &tab.view {
                TabView::Bot(view) => view.home(),
                _ => None,
            })
            .map(|home| (home, self.workspace_name(home)))
            .collect();
        let titles = self.bot_page_titles();
        // The person's choices about agents, for the tabs' catalogs (S6):
        // worked out only while a Bot tab is open.
        let agent_settings = self
            .panes
            .iter_groups()
            .flat_map(|(_, group)| group.iter_tabs())
            .any(|tab| matches!(tab.view, TabView::Bot(_)))
            .then(|| self.bot_agent_settings());
        let roster = self.explore.bots();
        let mut work_moved = false;
        let mut moved = false;
        let mut created = false;
        let mut news = Vec::new();
        let mut creations = Vec::new();
        let mut conversation_titles = Vec::new();
        for (_, group) in self.panes.iter_groups_mut() {
            let mut relabeled = false;
            let active = group.active();
            for (index, tab) in group.iter_tabs_mut().enumerate() {
                if let TabView::Bot(view) = &mut tab.view {
                    if index != active {
                        view.hidden();
                    }
                    if let Some(settings) = &agent_settings {
                        moved |= view.set_agent_settings(settings);
                    }
                    // A conversation's work moved (started, stopped, asked):
                    // the roster's activity is read again (S10).
                    work_moved |= view.take_status_moved();
                    if index == active
                        && let Some(bot) = view.bot()
                    {
                        // The list's marks: the owner's activity, as the
                        // roster last read it (S8, S10).
                        moved |= view.set_activity(&roster.activity_of(bot));
                    }
                    // A tab restored behind another has read nothing yet:
                    // it wears its Bot's name as the roster lists it.
                    if let Some(bot) = view.bot()
                        && let Some(listed) = roster.bots().iter().find(|row| row.id == bot)
                    {
                        view.know_name(&listed.name);
                    }
                    if let Some(home) = view.home()
                        && let Some((_, name)) = names.iter().find(|(id, _)| *id == home)
                    {
                        view.set_home_name(name.clone());
                    }
                    // A create tab that just became its new Bot's (S8):
                    // the roster lists it.
                    created |= view.take_created();
                    // A rename, a pause or a resume the tab's own answers
                    // carried: the roster's row says it now.
                    if let Some(bot) = view.take_roster_news() {
                        news.push(bot);
                    }
                    // Where a create tab's creation stands, as its own
                    // answer said: the Unfinished group says it now.
                    if let Some(creation) = view.take_creation_news() {
                        creations.push(creation);
                    }
                    // Its conversations' titles, so the roster's activity
                    // rows say the same words as the list's rows.
                    conversation_titles.extend(view.conversation_titles());
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
        if work_moved {
            self.explore.bots_mut().owe_activity();
        }
        if created {
            self.explore.bots_mut().owe_lifecycle_refresh();
            moved = true;
        }
        // The tabs' own news, into the roster's rows at once; its list is
        // read again too, on its next shown pass, for whatever else moved.
        if !news.is_empty() {
            let roster = self.explore.bots_mut();
            for bot in news {
                roster.know_bot(crate::bots::roster::RosterBot::from(bot));
            }
            roster.owe_lifecycle_refresh();
            moved = true;
        }
        // A create tab's creation stopped unfinished, moved or ended: the
        // roster's Unfinished group says so at once, shown or not (the
        // second window run: it appeared only once the roster was shown
        // again).
        for creation in creations {
            moved |= self.explore.bots_mut().know_creation(creation);
        }
        moved |= self.explore.bots_mut().know_titles(conversation_titles);
        moved || placed || recorded
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
