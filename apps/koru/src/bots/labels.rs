// SPDX-License-Identifier: MPL-2.0

//! **Which Bot a conversation belongs to, as far as the window knows** (Bots
//! P1.8, the open half of blocker B1).
//!
//! A Bot's conversation must open as its Bot's tab and never as an ordinary
//! chat: the ordinary reader holds no `bots.manage`, cannot read it, and turns
//! the tab into a new chat. The engine labels every `Chat` it returns to a
//! caller whose proof admits that Bot conversation (`Chat.bot`), so the window
//! can know — once something has read the chat.
//!
//! **What the window already knows** is gathered here as it is learned: the
//! roster's main conversations, a Bot tab's listed conversations, the
//! activity read's conversations, and a `Chat` whose label says so. What it
//! does not know yet it asks **once, off the window thread**: a first-party
//! `read_chat` through a [`BotsLink`], whose answer carries the label or none.
//! The window shows a neutral *Opening this conversation…* tab meanwhile
//! rather than guessing (`mainview/bots.rs`).
//!
//! **Not found is ordinary.** A chat the owner's reader cannot find either is
//! not a Bot conversation this person can open; it opens as an ordinary chat
//! does today, and the ordinary tab says what it says about a missing chat.
//! **In Trash is said** (`bot_conversation_trashed`, S14): the window tells
//! the person once and does not ask again on its own. Any other failure is
//! *not known*: the tab says it cannot be opened and asks again the next
//! time it is shown.
//!
//! **Without a link** — under test, or when the thread could not be spawned —
//! nothing can be asked, and a chat the window does not know opens as an
//! ordinary chat, exactly as before this read existed.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use koru_ai_core::ChatId;
use koru_ai_core::bots::{BotChatLabel, BotId};

use super::link::{Answer, BotsLink, Command, Failure};

/// What the window knows about one conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Known {
    /// A Bot's conversation: which Bot, and the name the label carried, for
    /// the tab's name until the Bot is read.
    Bot { bot: BotId, name: String },
    /// Not a Bot's conversation this person can open: an ordinary chat.
    Ordinary,
}

/// A read's outcome, as the window acts on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    Known(Known),
    /// Nothing answered, or the owner refused for a reason that says nothing
    /// about whose the conversation is. Not remembered: asked again later.
    Unavailable,
    /// **The owner says the conversation is in Trash** (`bot_conversation_
    /// trashed`, S14): a Bot's, whose Bot it does not say. Not remembered,
    /// and not asked about again on its own — the window says it is in
    /// Trash, and offers Restore when its book knows that Trash.
    Trashed,
}

/// What the resolver has done, so a test can assert a count.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct Work {
    pub commands: usize,
}

#[derive(Default)]
pub(crate) struct ChatLabels {
    known: HashMap<ChatId, Known>,
    /// Asked and not answered yet.
    asking: HashSet<ChatId>,
    link: Option<BotsLink>,
    started: bool,
    work: Work,
}

impl ChatLabels {
    /// A resolver over a link a test holds the other end of.
    #[cfg(test)]
    pub(crate) fn with_link(link: BotsLink) -> Self {
        Self {
            link: Some(link),
            started: true,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn work(&self) -> Work {
        self.work
    }

    /// What the window knows about `chat`. `None` is *not known*, never
    /// *not a Bot's*.
    pub(crate) fn known(&self, chat: ChatId) -> Option<&Known> {
        self.known.get(&chat)
    }

    /// `chat` is `bot`'s conversation, learned from a read that said so.
    pub(crate) fn learn_bot(&mut self, chat: ChatId, bot: BotId, name: &str) {
        self.known.insert(
            chat,
            Known::Bot {
                bot,
                name: name.to_owned(),
            },
        );
    }

    /// `chat` is an ordinary chat, learned from a read that listed it with no
    /// Bot label. A Bot's label already learned is kept: a label is only
    /// ever given by a reader that may see it, so its absence elsewhere says
    /// nothing.
    pub(crate) fn learn_ordinary(&mut self, chat: ChatId) {
        self.known.entry(chat).or_insert(Known::Ordinary);
    }

    /// What a `Chat` read anywhere says about itself.
    pub(crate) fn learn_label(&mut self, chat: ChatId, label: Option<&BotChatLabel>) {
        match label {
            Some(label) => self.learn_bot(chat, label.bot_id, &label.bot_name),
            None => self.learn_ordinary(chat),
        }
    }

    /// Whether a read can be asked at all: `false` under test without a
    /// link, and when the link's thread could not be started.
    pub(crate) fn can_ask(&mut self, root: Option<PathBuf>) -> bool {
        if !self.started {
            self.started = true;
            self.link = BotsLink::start(root);
        }
        self.link.is_some()
    }

    pub(crate) fn asking(&self, chat: ChatId) -> bool {
        self.asking.contains(&chat)
    }

    /// **Ask whose `chat` is**, once while a read of it is on its way. `false`
    /// when it could not be asked now — no link, or a full queue — and the
    /// caller asks again on a later pass.
    pub(crate) fn ask(&mut self, chat: ChatId, root: Option<PathBuf>) -> bool {
        if self.asking.contains(&chat) {
            return true;
        }
        if !self.can_ask(root) {
            return false;
        }
        let Some(link) = &self.link else {
            return false;
        };
        if !link.send(Command::Label { chat }) {
            return false;
        }
        self.asking.insert(chat);
        self.work.commands += 1;
        true
    }

    /// Whether an answer is owed: the one reason the window keeps a wake
    /// clock for this read.
    pub(crate) fn awaiting(&self) -> bool {
        !self.asking.is_empty()
    }

    /// Fold what the link answered, and say what each read found. Never
    /// blocks.
    pub(crate) fn poll(&mut self) -> Vec<(ChatId, Resolved)> {
        let Some(link) = &self.link else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for answer in link.drain() {
            let Answer::Labelled { chat, result } = answer else {
                continue;
            };
            self.asking.remove(&chat);
            let resolved = match result {
                Ok(label) => {
                    self.learn_label(chat, label.as_ref());
                    Resolved::Known(self.known[&chat].clone())
                }
                // The owner's reader cannot find it either: not a Bot
                // conversation this person can open.
                Err(Failure::Refused(code)) if code == "not_found" => {
                    self.learn_ordinary(chat);
                    Resolved::Known(Known::Ordinary)
                }
                // In Trash: the conversation, or its whole Bot.
                Err(Failure::Refused(code))
                    if code == super::settings::BOT_TRASHED
                        || code == super::settings::BOT_TRASHED_BOT =>
                {
                    Resolved::Trashed
                }
                Err(_) => Resolved::Unavailable,
            };
            out.push((chat, resolved));
        }
        out
    }
}

#[cfg(test)]
mod tests;
