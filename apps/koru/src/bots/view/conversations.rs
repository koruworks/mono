// SPDX-License-Identifier: MPL-2.0

//! **A Bot's conversations, inside its tab** (Bots P1.8 S8; EXPERIENCE E3).
//!
//! The list is the shared Chats list itself — `koru_chats_ui::ChatsSurface`
//! made `without_reader`, fed here — so its rows, keyboard, focus, status
//! marks and inspector records are the Chats view's. What differs is where
//! its rows come from and what a row offers:
//!
//! - **Fed by the Bot owner**, never by the ordinary chat list: one bounded
//!   page of `ListBotChats` at a time through the tab's own [`BotsLink`]
//!   thread (main first, then newest; Trash left out), and *Show more*
//!   follows the page's cursor. A re-read asks again from the start for as
//!   many rows as were shown, up to one page's bound.
//! - **Open, and Move to Trash.** The owner refuses Rename and Pin for a
//!   Bot conversation, so the rows have no pin; their ⋯, right-click and
//!   Delete ask the tab for one conversation's Trash (S14), a lifecycle
//!   operation the window confirms and the owner settles. The row leaves
//!   only when the owner says it is done ([`Conversations::remove`]). An
//!   archived Bot's rows only open (`ChatsSurface::set_row_actions`,
//!   QUESTIONS O31.3).
//! - **New conversation** is the owner's `CreateChat`; the tab then shows the
//!   conversation it made. Branch is the conversation's own, and the branch
//!   joins the list on the next read.
//!
//! **States.** While the first page is on its way, and when the owner did
//! not answer before anything was listed, the list says so in a sentence of
//! its own and draws no rows. A failed re-read keeps the rows it showed and
//! says the list may be old. Only the main conversation is listed — or none,
//! when everything is in Trash — is said too: never an empty list without a
//! word. **A hidden tab sends nothing**; shown again, it reads once.

use std::collections::HashMap;

use koru_ai_core::ChatId;
use koru_ai_core::bots::{
    BOT_CHAT_PAGE_LIMIT, BotChatCursor, BotChatPage, BotChatPurpose, BotCommand, BotId,
    BotRequestId, BotResponse,
};
use koru_chats_ui::{ChatStatus, ChatsSurface, Listing, RowActions};
use koru_i18n::Key as Words;

use crate::bots::link::{Command, Failure};

/// How many conversations one page asks for.
pub(crate) const PAGE: u16 = 50;

/// Where the list's read stands.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub(crate) enum Load {
    /// Not asked yet.
    #[default]
    Cold,
    Loading,
    Ready,
    /// The owner did not answer. **What was listed stays.**
    Unavailable,
}

/// A page on its way.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
struct Asking {
    seq: u64,
    /// `true` for a *Show more* page, which adds to the rows; `false` for a
    /// read from the start, which replaces them.
    more: bool,
}

pub(crate) struct Conversations {
    pub(crate) surface: ChatsSurface,
    bot: Option<BotId>,
    load: Load,
    seq: u64,
    asking: Option<Asking>,
    /// Every conversation listed so far, in the owner's order.
    listed: Vec<koru_ai_core::Chat>,
    main: Option<ChatId>,
    next: Option<BotChatCursor>,
    /// A read from the start is owed: shown after being hidden, or a
    /// conversation was made or branched.
    dirty: bool,
    /// *New conversation* on its way, by its request's id.
    creating: Option<BotRequestId>,
    /// Why the last *New conversation* did not make one.
    create_refused: Option<Words>,
    /// The conversation a *New conversation* made, for the tab to show.
    made: Option<ChatId>,
}

impl Default for Conversations {
    fn default() -> Self {
        let mut surface = ChatsSurface::without_reader();
        surface.set_row_actions(RowActions::OPEN_ONLY);
        Self {
            surface,
            bot: None,
            load: Load::Cold,
            seq: 0,
            asking: None,
            listed: Vec::new(),
            main: None,
            next: None,
            dirty: false,
            creating: None,
            create_refused: None,
            made: None,
        }
    }
}

impl Conversations {
    pub(crate) fn load(&self) -> Load {
        self.load
    }

    /// The conversations listed, in the owner's order.
    pub(crate) fn listed(&self) -> &[koru_ai_core::Chat] {
        &self.listed
    }

    /// The Bot's main conversation, as the list says.
    pub(crate) fn main(&self) -> Option<ChatId> {
        self.main
    }

    /// **Whether the Bot's main conversation is in Trash**, as the owner's
    /// list says it: listed from the start and no row of it is the main one.
    /// The owner lists the main conversation first unless it is in Trash.
    pub(crate) fn main_missing(&self) -> bool {
        self.load == Load::Ready && self.main.is_none()
    }

    /// **Offer one conversation's Trash on the rows, or not** (S14): the ⋯
    /// menu, its right-click and menu key, and Delete — through the shared
    /// list's row-actions mask, which is pure data. Pin is the owner's to
    /// refuse and stays off. Off while the Bot is archived or trashed.
    pub(crate) fn offer_trash(&mut self, offered: bool) {
        self.surface.set_row_actions(if offered {
            RowActions {
                pin: false,
                menu: true,
                delete: true,
            }
        } else {
            RowActions::OPEN_ONLY
        });
    }

    /// **The owner put `chat` in Trash**: its row leaves the list now — the
    /// owner said so — and the list reads again behind it.
    pub(crate) fn remove(&mut self, chat: ChatId) {
        self.listed.retain(|held| held.id != chat);
        if self.main == Some(chat) {
            self.main = None;
        }
        self.surface.take_deleted(chat, Ok(()));
        // The surface records it for a host that closes tabs; this host
        // moves its own tab instead.
        let _ = self.surface.take_deleted_chats();
        self.owe_read();
    }

    /// Whether *Show more* has a page to ask for.
    pub(crate) fn has_more(&self) -> bool {
        self.next.is_some()
    }

    pub(crate) fn creating(&self) -> bool {
        self.creating.is_some()
    }

    pub(crate) fn create_refused(&self) -> Option<Words> {
        self.create_refused
    }

    /// Whether a page is on its way: the one reason the tab keeps its wake
    /// clock for the list.
    pub(crate) fn awaiting(&self) -> bool {
        self.asking.is_some() || self.creating.is_some()
    }

    /// **Which Bot the list is about.** Another Bot's rows are never this
    /// one's: a change empties it and asks again.
    pub(crate) fn set_bot(&mut self, bot: BotId) {
        if self.bot == Some(bot) {
            return;
        }
        *self = Self {
            bot: Some(bot),
            seq: self.seq.wrapping_add(1),
            ..Self::default()
        };
    }

    /// A read from the start is owed: the tab was hidden, or the owner
    /// made or branched a conversation.
    pub(crate) fn owe_read(&mut self) {
        if self.load != Load::Cold {
            self.dirty = true;
        }
    }

    /// The read the list owes now, if any: the first page, or a read from
    /// the start once one is owed. Nothing while a page is on its way.
    pub(crate) fn read(&mut self) -> Option<Command> {
        let bot = self.bot?;
        if self.asking.is_some() || !(self.load == Load::Cold || self.dirty) {
            return None;
        }
        // As many rows as were shown, so a re-read does not shorten the
        // list under the person; one page's bound at most.
        let shown = u16::try_from(self.listed.len()).unwrap_or(u16::MAX);
        let limit = shown.max(PAGE).min(BOT_CHAT_PAGE_LIMIT);
        Some(self.ask(bot, None, limit, false))
    }

    /// *Show more*: the page after the last one, when there is one.
    pub(crate) fn more(&mut self) -> Option<Command> {
        let bot = self.bot?;
        if self.asking.is_some() {
            return None;
        }
        let cursor = self.next.clone()?;
        Some(self.ask(bot, Some(cursor), PAGE, true))
    }

    fn ask(
        &mut self,
        bot: BotId,
        cursor: Option<BotChatCursor>,
        limit: u16,
        more: bool,
    ) -> Command {
        self.seq = self.seq.wrapping_add(1);
        self.asking = Some(Asking {
            seq: self.seq,
            more,
        });
        self.dirty = false;
        if self.load == Load::Cold {
            self.load = Load::Loading;
        }
        Command::ListChats {
            seq: self.seq,
            bot,
            cursor,
            limit,
        }
    }

    /// The link would not take a read: ask it again on a later pass.
    pub(crate) fn unsent(&mut self) {
        let more = self.asking.take().is_some_and(|asking| asking.more);
        if !more {
            self.dirty = true;
        }
        if self.load == Load::Loading && self.listed.is_empty() {
            self.load = Load::Cold;
        }
    }

    /// **Fold a page's answer**: only the latest request's about this Bot.
    /// `true` when the list changed.
    pub(crate) fn answer(
        &mut self,
        seq: u64,
        bot: BotId,
        result: Result<BotChatPage, Failure>,
    ) -> bool {
        let Some(asking) = self.asking else {
            return false;
        };
        if asking.seq != seq || self.bot != Some(bot) {
            return false;
        }
        self.asking = None;
        match result {
            Ok(page) => {
                if !asking.more {
                    self.listed.clear();
                    // The main conversation is listed first, or not at all:
                    // a read from the start that does not list it says it is
                    // in Trash (O7).
                    self.main = None;
                }
                for conversation in page.chats {
                    if conversation.label.bot_id != bot {
                        continue;
                    }
                    if conversation.label.purpose == BotChatPurpose::Main {
                        self.main = Some(conversation.chat.id);
                    }
                    let id = conversation.chat.id;
                    match self.listed.iter_mut().find(|held| held.id == id) {
                        Some(held) => *held = conversation.chat,
                        None => self.listed.push(conversation.chat),
                    }
                }
                self.next = page.next_cursor;
                self.load = Load::Ready;
                self.surface.take_listing(Ok(Listing {
                    chats: self.listed.clone(),
                    agents: Vec::new(),
                }));
            }
            // What was listed stays; the sentence says it may be old.
            Err(_) => self.load = Load::Unavailable,
        }
        true
    }

    /// *New conversation*: the owner's `CreateChat`, unless one is on its
    /// way already.
    pub(crate) fn create(&mut self) -> Option<Command> {
        let bot = self.bot?;
        if self.creating.is_some() {
            return None;
        }
        let request = BotRequestId::mint();
        self.creating = Some(request);
        self.create_refused = None;
        Some(Command::Write {
            bot,
            request,
            command: Box::new(BotCommand::CreateChat {
                request_id: request,
                bot_id: bot,
            }),
        })
    }

    /// The link would not take *New conversation*: nothing was sent.
    pub(crate) fn create_unsent(&mut self) {
        self.creating = None;
        self.create_refused = Some(Words::BotsErrUnreachable);
    }

    /// The owner's answer to a write, if it is *New conversation*'s. `true`
    /// when it was; the conversation it made is then
    /// [`take_made`](Self::take_made).
    pub(crate) fn wrote(
        &mut self,
        request: BotRequestId,
        result: &Result<BotResponse, Failure>,
    ) -> bool {
        if self.creating != Some(request) {
            return false;
        }
        self.creating = None;
        match result {
            Ok(BotResponse::Chat { reference }) if Some(reference.bot_id) == self.bot => {
                self.made = Some(reference.chat_id);
                self.owe_read();
                if self.load == Load::Cold {
                    self.dirty = true;
                }
            }
            Ok(_) => self.create_refused = Some(Words::BotsErrNotApplied),
            Err(Failure::Unreachable) => self.create_refused = Some(Words::BotsErrUnreachable),
            Err(Failure::Refused(code)) => {
                self.create_refused = Some(crate::bots::settings::refusal_words(code));
            }
        }
        true
    }

    /// The conversation *New conversation* made, taken once.
    pub(crate) fn take_made(&mut self) -> Option<ChatId> {
        self.made.take()
    }

    /// The sentence over the list, when there is one to say.
    pub(crate) fn notice(&self) -> Option<Words> {
        match self.load {
            Load::Cold | Load::Loading if self.listed.is_empty() => {
                Some(Words::BotsConversationsLoading)
            }
            Load::Unavailable => Some(Words::BotsConversationsUnavailable),
            Load::Ready if self.listed.len() <= 1 => Some(Words::BotsConversationsOnlyMain),
            _ => None,
        }
    }

    /// Whether the list draws its rows: once it has listed any.
    pub(crate) fn shows_rows(&self) -> bool {
        !self.listed.is_empty()
    }

    /// The marks the rows wear: running and Needs-you from the owner's
    /// activity read, and the open conversation's own status. `true` when a
    /// row's mark changed.
    pub(crate) fn set_statuses(&mut self, statuses: HashMap<ChatId, ChatStatus>) -> bool {
        self.surface.set_statuses(statuses)
    }
}

#[cfg(test)]
mod tests;
