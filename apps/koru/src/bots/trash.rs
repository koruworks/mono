// SPDX-License-Identifier: MPL-2.0

//! **Trashed Bots and Bot conversations, for the window's Trash** (Bots P1.8
//! S14; P1.7 S9).
//!
//! A Bot is not a workspace document, so its rows reach the Trash by their
//! own road, as a run's do ([`crate::mainview`]'s `run_trash`): this feed
//! reads the owner's Trash filter (`ListLifecycle`, trashed) and the
//! workspace's Bot conversations in Trash (`ListTrashedBotChats`) on its own
//! [`BotsLink`] thread, and hands the Trash view plain rows
//! ([`TrashedBotRow`]). **What a Restore names is the owner's to say**, so
//! it works after a restart: a Bot's Restore names the Trash that holds it
//! (`Bot.held_by`), a conversation's `RestoreChat` the operation that hid it
//! (`trashed_by`), each at the revision the owner last said. A conversation
//! that went with its Bot's own Trash is the Bot's row, and comes back with
//! it; one trashed on its own while its Bot is archived or trashed waits for
//! its Bot (`restore: bot`).
//!
//! **What it cannot do, and says.** A Bot whose Trash the owner does not
//! name has no Restore here. Permanent deletion of a Bot is not built: no
//! row of this feed is ever deleted, and emptying the Trash leaves them,
//! saying so.
//!
//! **Only while a Trash tab is in front.** A hidden feed sends nothing; shown
//! again, it reads once.

use std::collections::HashMap;
use std::path::PathBuf;

use koru_ai_core::ChatId;
use koru_ai_core::bots::{
    Bot, BotChatRestore, BotCommand, BotId, BotLifecycle, BotLifecycleKind, BotLifecycleStage,
    BotRequestId, BotResponse, BotTrashedChat,
};
use koru_i18n::{Key as Words, Lang, t};
use koru_store_core::ids::WorkspaceId;

use super::lifecycle::{Finished, refusal_words, restore_refusal_words};
use super::link::{Answer, BotsLink, Command, Failure};

/// What a Bot row of the Trash is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BotSubject {
    Bot(BotId),
    Chat { bot: BotId, chat: ChatId },
}

impl BotSubject {
    /// The row's identity in the Trash's table: the Bot's or the
    /// conversation's own uuid, the way a run's row is the run's.
    pub(crate) fn uuid(self) -> uuid::Uuid {
        match self {
            Self::Bot(bot) => bot.as_uuid(),
            Self::Chat { chat, .. } => chat.as_uuid(),
        }
    }
}

/// One row, as the Trash view draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrashedBotRow {
    pub subject: BotSubject,
    pub name: String,
    /// Where it came from, in the person's words.
    pub origin: String,
    pub trashed_at_ms: Option<i64>,
    /// Whether Restore can be asked for here: the owner names the Trash it
    /// undoes, and — for a conversation — its Bot is not archived or in
    /// Trash itself.
    pub restorable: bool,
}

/// **What a link's receipt can offer for a Bot conversation in Trash**,
/// from the owner's listing (`list_trashed_bot_chats`, with each entry's
/// `restore` and `bot_held_by`). Restore is offered only where one press
/// brings it back; otherwise the receipt says what must happen first.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkRestore {
    /// Trashed on its own, its Bot active: its own Restore brings it back.
    Conversation { bot: BotId },
    /// It went with its Bot's own Trash: the Bot's Restore brings both
    /// back. One press when the Trash filter lists the Bot, whose revision
    /// that Restore carries.
    WithItsBot { bot: BotId, restorable: bool },
    /// Trashed on its own before its Bot went to the Trash: the Bot's
    /// Restore first, then its own.
    BotFirst,
    /// Its Bot is archived: Unarchive the Bot first, then its own Restore.
    UnarchiveFirst,
    /// The listing does not say what holds its Bot.
    Unknown,
}

impl LinkRestore {
    /// The Bot a press restores, when one press restores it.
    pub(crate) fn press(self) -> Option<BotId> {
        match self {
            Self::Conversation { bot }
            | Self::WithItsBot {
                bot,
                restorable: true,
            } => Some(bot),
            _ => None,
        }
    }

    /// What the receipt says.
    pub(crate) fn words(self) -> Words {
        match self {
            Self::Conversation { .. } | Self::Unknown => Words::BotsLinkInTrash,
            Self::WithItsBot { .. } => Words::BotsLinkInTrashWithBot,
            Self::BotFirst => Words::BotsLinkRestoreBotFirst,
            Self::UnarchiveFirst => Words::BotsLinkUnarchiveFirst,
        }
    }
}

/// What a Restore said, for the window's receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Receipt {
    pub words: String,
}

/// A Restore on its way.
#[derive(Clone, Debug)]
struct Restoring {
    subject: BotSubject,
    name: String,
    source: BotRequestId,
    home: Option<WorkspaceId>,
    /// For a conversation: its title, for the book.
    title: Option<String>,
}

/// What the work is, for a test to count.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct Work {
    pub commands: usize,
}

#[derive(Default)]
pub(crate) struct BotTrash {
    lang: Lang,
    workspace: Option<WorkspaceId>,
    shown: bool,
    fresh: bool,
    seq: u64,
    in_flight: bool,
    listed: Vec<Bot>,
    /// The workspace's Bot conversations in Trash, as last read, and that
    /// read's sequence while it is on its way.
    chats: Vec<BotTrashedChat>,
    chats_seq: u64,
    chats_in_flight: bool,
    /// A conversation's Restore asked for before the feed had listed it — a
    /// link's receipt — sent once the listing names it.
    wanted: Option<ChatId>,
    /// **A link led to this conversation in Trash** and the feed had not
    /// listed it: both listings are read, shown or not, and what a Restore
    /// could do is said once they answer ([`Self::take_looked_up`]).
    looking: Option<ChatId>,
    looked_up: Option<(ChatId, Option<LinkRestore>)>,
    /// The last listing stopped at its last page with more to read: not
    /// every Bot conversation in Trash is listed, and the Trash says so.
    chats_more: bool,
    /// Restores by their request, and the Bot reads a conversation's
    /// Restore waits on, by their sequence.
    restoring: HashMap<BotRequestId, Restoring>,
    reading: HashMap<u64, Restoring>,
    read_seq: u64,
    finished: Vec<Finished>,
    receipts: Vec<Receipt>,
    link: Option<BotsLink>,
    started: bool,
    work: Work,
}

impl BotTrash {
    /// A feed over a link a test holds the other end of.
    #[cfg(test)]
    pub(crate) fn with_link(link: BotsLink) -> Self {
        Self {
            link: Some(link),
            started: true,
            ..Self::default()
        }
    }

    /// What the owner's two Trash reads would have answered, for a window
    /// test that has no link.
    #[cfg(test)]
    pub(crate) fn listed_for_test(&mut self, bots: Vec<Bot>, chats: Vec<BotTrashedChat>) {
        self.listed = bots;
        self.chats = chats;
    }

    #[cfg(test)]
    pub(crate) fn work(&self) -> Work {
        self.work
    }

    pub(crate) fn set_lang(&mut self, lang: Lang) {
        self.lang = lang;
    }

    /// **Which workspace the Trash is about**: the window's. Another
    /// workspace's Bots are never this one's.
    pub(crate) fn set_workspace(&mut self, workspace: Option<WorkspaceId>) -> bool {
        if self.workspace == workspace {
            return false;
        }
        self.workspace = workspace;
        self.listed.clear();
        self.chats.clear();
        self.wanted = None;
        self.looking = None;
        self.looked_up = None;
        self.chats_more = false;
        self.fresh = false;
        self.in_flight = false;
        self.chats_in_flight = false;
        self.seq = self.seq.wrapping_add(1);
        self.chats_seq = self.chats_seq.wrapping_add(1);
        true
    }

    /// Start the feed's link, once: the first time a Trash tab is shown, or
    /// a link's Restore is asked for. Nothing under test.
    pub(crate) fn start(&mut self, root: Option<PathBuf>) {
        if !self.started {
            self.started = true;
            self.link = BotsLink::start(root);
        }
    }

    /// **Whether a Trash tab is in front.** Hiding marks the listing stale:
    /// shown again, it reads once.
    pub(crate) fn set_shown(&mut self, shown: bool, root: Option<PathBuf>) {
        if shown {
            self.start(root);
        }
        if self.shown != shown && !shown {
            self.fresh = false;
        }
        self.shown = shown;
    }

    /// The Trash changed in this window — a Bot tab finished an operation:
    /// read again when shown.
    pub(crate) fn owe_refresh(&mut self) {
        self.fresh = false;
    }

    /// Whether an answer is owed: the one reason the window keeps a wake.
    pub(crate) fn awaiting(&self) -> bool {
        (self.shown && (self.in_flight || self.chats_in_flight))
            || self.restoring_any()
            || self.looking.is_some()
    }

    /// A Restore the person asked for is on its way — from the Trash, or a
    /// link's receipt — and its answer is folded wherever the Trash is.
    fn restoring_any(&self) -> bool {
        !self.restoring.is_empty() || !self.reading.is_empty() || self.wanted.is_some()
    }

    /// The feed's pass: ask when owed, fold what came back. A hidden feed
    /// asks nothing; it only takes the answer to a Restore the person
    /// asked for. `true` when its rows may have changed.
    pub(crate) fn maintain(&mut self) -> bool {
        if !self.shown {
            // A Restore asked for from a link's receipt reads the listing
            // it needs once, shown or not; nothing else is asked.
            let asked = self.wanted.is_some() && !self.chats_in_flight && self.ask_chats();
            return asked || ((self.restoring_any() || self.looking.is_some()) && self.poll());
        }
        let asked = !self.fresh && self.refresh();
        let answered = self.poll();
        asked || answered
    }

    fn refresh(&mut self) -> bool {
        let (Some(workspace), Some(link)) = (self.workspace, &self.link) else {
            return false;
        };
        let seq = self.seq.wrapping_add(1);
        if !link.send(Command::Lifecycle {
            seq,
            workspace,
            lifecycle: BotLifecycle::Trashed,
        }) {
            return false;
        }
        self.seq = seq;
        self.fresh = true;
        self.in_flight = true;
        self.work.commands += 1;
        self.ask_chats();
        true
    }

    /// Ask for the workspace's Bot conversations in Trash. `true` when the
    /// read went.
    fn ask_chats(&mut self) -> bool {
        let (Some(workspace), Some(link)) = (self.workspace, &self.link) else {
            return false;
        };
        let seq = self.chats_seq.wrapping_add(1);
        if !link.send(Command::TrashedChats { seq, workspace }) {
            return false;
        }
        self.chats_seq = seq;
        self.chats_in_flight = true;
        self.work.commands += 1;
        true
    }

    fn poll(&mut self) -> bool {
        let Some(link) = &self.link else {
            return false;
        };
        let answers = link.drain();
        let mut changed = false;
        for answer in answers {
            match answer {
                Answer::Lifecycle {
                    seq,
                    workspace,
                    lifecycle: BotLifecycle::Trashed,
                    result,
                } if seq == self.seq && Some(workspace) == self.workspace => {
                    self.in_flight = false;
                    // A read that did not answer keeps what was listed.
                    if let Ok(bots) = result {
                        self.listed = bots;
                    }
                    self.settle_lookup();
                    changed = true;
                }
                Answer::TrashedChats {
                    seq,
                    workspace,
                    result,
                } if seq == self.chats_seq && Some(workspace) == self.workspace => {
                    self.chats_in_flight = false;
                    // A read that did not answer keeps what was listed.
                    if let Ok(listing) = result {
                        self.chats = listing.items;
                        self.chats_more = listing.more;
                    }
                    if let Some(chat) = self.wanted.take() {
                        self.restore_listed_chat(chat);
                    }
                    self.settle_lookup();
                    changed = true;
                }
                Answer::Read { seq, bot, result } => {
                    if let Some(restoring) = self.reading.remove(&seq) {
                        self.restore_chat_at(restoring, bot, result);
                        changed = true;
                    }
                }
                Answer::Wrote {
                    request, result, ..
                } => {
                    if let Some(restoring) = self.restoring.remove(&request) {
                        self.restored(request, restoring, result);
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        changed
    }

    /// The Trash that holds a listed Bot, as the owner names it.
    fn trash_of(bot: &Bot) -> Option<BotRequestId> {
        bot.held_by
            .as_ref()
            .filter(|holder| holder.kind == BotLifecycleKind::Trash)
            .map(|holder| holder.request_id)
    }

    /// **The conversations the Trash lists on their own**: those trashed by
    /// their own operation. One hidden by its Bot's Trash is the Bot's row.
    fn own_chats(&self) -> impl Iterator<Item = &BotTrashedChat> {
        self.chats
            .iter()
            .filter(|entry| entry.trashed_by.kind == BotLifecycleKind::TrashChat)
    }

    /// **What a Restore of `chat` can do now**, from the owner's listing:
    /// `None` while the feed has not listed it.
    pub(crate) fn link_restore(&self, chat: ChatId) -> Option<LinkRestore> {
        let entry = self
            .chats
            .iter()
            .find(|entry| entry.conversation.chat.id == chat)?;
        let bot = entry.conversation.label.bot_id;
        if entry.restore == BotChatRestore::Conversation {
            return Some(LinkRestore::Conversation { bot });
        }
        // The Bot first. What holds it is the entry's `bot_held_by`, or the
        // Trash filter's entry for it.
        let listed = self.listed.iter().find(|listed| listed.id == bot);
        if entry.trashed_by.kind == BotLifecycleKind::Trash {
            return Some(LinkRestore::WithItsBot {
                bot,
                restorable: listed.and_then(Self::trash_of).is_some(),
            });
        }
        Some(match entry.bot_held_by.as_ref().map(|holder| holder.kind) {
            Some(BotLifecycleKind::Archive) => LinkRestore::UnarchiveFirst,
            Some(BotLifecycleKind::Trash) => LinkRestore::BotFirst,
            _ if listed.is_some() => LinkRestore::BotFirst,
            _ => LinkRestore::Unknown,
        })
    }

    /// **A link led to `chat`, in Trash**: what its Restore can do is read
    /// from the owner — both listings, shown or not — unless the feed has
    /// listed it already. `true` when an answer is owed
    /// ([`Self::take_looked_up`]).
    pub(crate) fn look_up(&mut self, chat: ChatId) -> bool {
        if self.link_restore(chat).is_some() {
            return false;
        }
        if self.looking == Some(chat) {
            return true;
        }
        let asked = self.in_flight || self.chats_in_flight || self.refresh();
        if asked {
            self.looking = Some(chat);
        }
        asked
    }

    /// Both listings answered: what the looked-up conversation's Restore
    /// can do, said once.
    fn settle_lookup(&mut self) {
        if self.in_flight || self.chats_in_flight {
            return;
        }
        if let Some(chat) = self.looking.take() {
            self.looked_up = Some((chat, self.link_restore(chat)));
        }
    }

    /// What a link's lookup found, taken once: `None` inside when the
    /// owner's listing does not name the conversation.
    pub(crate) fn take_looked_up(&mut self) -> Option<(ChatId, Option<LinkRestore>)> {
        self.looked_up.take()
    }

    /// Whether the last listing stopped with more pages to read: not every
    /// Bot conversation in Trash is listed.
    pub(crate) fn more_than_listed(&self) -> bool {
        self.chats_more
    }

    /// Whether a Restore of `subject` is on its way already: a second press
    /// sends nothing, so a refused duplicate never reads as the outcome.
    fn restoring_subject(&self, subject: BotSubject) -> bool {
        self.restoring
            .values()
            .chain(self.reading.values())
            .any(|restoring| restoring.subject == subject)
            || matches!(subject, BotSubject::Chat { chat, .. } if self.wanted == Some(chat))
    }

    /// **The rows**, newest first: the owner's trashed Bots of the
    /// workspace, and its Bot conversations trashed on their own.
    pub(crate) fn rows(&self) -> Vec<TrashedBotRow> {
        let mut rows: Vec<TrashedBotRow> = self
            .listed
            .iter()
            .map(|bot| TrashedBotRow {
                subject: BotSubject::Bot(bot.id),
                name: bot.config.name.clone(),
                origin: t(self.lang, Words::TrashBotKind).to_owned(),
                trashed_at_ms: bot
                    .held_by
                    .as_ref()
                    .map(|holder| holder.completed_at_ms)
                    .or(Some(bot.updated_at_ms)),
                restorable: Self::trash_of(bot).is_some(),
            })
            .collect();
        for entry in self.own_chats() {
            let label = &entry.conversation.label;
            rows.push(TrashedBotRow {
                subject: BotSubject::Chat {
                    bot: label.bot_id,
                    chat: entry.conversation.chat.id,
                },
                name: entry.conversation.chat.title.clone(),
                origin: koru_i18n::fill(
                    t(self.lang, Words::TrashBotConversation),
                    &[label.bot_name.as_str()],
                ),
                trashed_at_ms: Some(entry.trashed_at_ms),
                restorable: entry.restore == BotChatRestore::Conversation,
            });
        }
        rows.sort_by(|a, b| b.trashed_at_ms.cmp(&a.trashed_at_ms));
        rows
    }

    /// **Restore `subject`** from the Trash. A Bot's Restore names the Trash
    /// the owner says holds it, at the revision the Trash filter said; a
    /// conversation's names the operation that hid it, after a read of its
    /// Bot for the revision. `false` when nothing was sent — a row with no
    /// Restore, or the link would not take it. A conversation the feed has
    /// not listed yet (a link's receipt) is read for first, and restored once
    /// the listing names it.
    pub(crate) fn restore(&mut self, subject: BotSubject) -> bool {
        if self.link.is_none() || self.restoring_subject(subject) {
            return false;
        }
        match subject {
            BotSubject::Bot(id) => {
                let Some(bot) = self.listed.iter().find(|bot| bot.id == id).cloned() else {
                    return false;
                };
                let Some(source) = Self::trash_of(&bot) else {
                    return false;
                };
                let request = BotRequestId::mint();
                let restoring = Restoring {
                    subject,
                    name: bot.config.name.clone(),
                    source,
                    home: Some(bot.home_workspace),
                    title: None,
                };
                self.send_restore(
                    id,
                    request,
                    BotCommand::Restore {
                        request_id: request,
                        bot_id: id,
                        expected_revision: bot.revision,
                        source_request_id: source,
                    },
                    restoring,
                )
            }
            BotSubject::Chat { chat, .. } => {
                if self.link_restore(chat).is_none() {
                    self.wanted = Some(chat);
                    return self.chats_in_flight || self.ask_chats();
                }
                self.restore_listed_chat(chat)
            }
        }
    }

    /// A listed conversation's Restore: its Bot read for the revision, then
    /// `RestoreChat` naming the operation that hid it. Only one the owner
    /// says comes back by its own Restore.
    fn restore_listed_chat(&mut self, chat: ChatId) -> bool {
        let Some(entry) = self
            .chats
            .iter()
            .find(|entry| entry.conversation.chat.id == chat)
            .cloned()
        else {
            return false;
        };
        // **Its Bot comes back first.** One that went with its Bot's own
        // Trash, whose Trash the owner names, comes back with the Bot: that
        // Restore is the one press. Otherwise it is said, never silent.
        match self.link_restore(chat) {
            Some(LinkRestore::Conversation { .. }) | None => {}
            Some(plan) => {
                if let Some(bot) = plan.press() {
                    return self.restore(BotSubject::Bot(bot));
                }
                let why = match plan {
                    LinkRestore::UnarchiveFirst => Words::BotsErrArchived,
                    _ => Words::BotsErrBotTrashed,
                };
                self.fail(&entry.conversation.chat.title, why);
                return true;
            }
        }
        let entry = &entry;
        let bot = entry.conversation.label.bot_id;
        let restoring = Restoring {
            subject: BotSubject::Chat { bot, chat },
            name: entry.conversation.chat.title.clone(),
            source: entry.trashed_by.request_id,
            home: self.workspace,
            title: Some(entry.conversation.chat.title.clone()),
        };
        let seq = self.read_seq.wrapping_add(1);
        let Some(link) = &self.link else {
            return false;
        };
        if !link.send(Command::Read { seq, bot }) {
            return false;
        }
        self.read_seq = seq;
        self.work.commands += 1;
        self.reading.insert(seq, restoring);
        true
    }

    fn send_restore(
        &mut self,
        bot: BotId,
        request: BotRequestId,
        command: BotCommand,
        restoring: Restoring,
    ) -> bool {
        let Some(link) = &self.link else {
            return false;
        };
        if !link.send(Command::Write {
            bot,
            request,
            command: Box::new(command),
        }) {
            return false;
        }
        self.work.commands += 1;
        self.restoring.insert(request, restoring);
        true
    }

    /// A conversation's Bot read answered: its `RestoreChat` goes at that
    /// revision, or the Restore fails in words.
    fn restore_chat_at(&mut self, restoring: Restoring, bot: BotId, result: Result<Bot, Failure>) {
        let BotSubject::Chat { chat, .. } = restoring.subject else {
            return;
        };
        match result {
            Ok(read) if read.id == bot => {
                let request = BotRequestId::mint();
                let command = BotCommand::RestoreChat {
                    request_id: request,
                    bot_id: bot,
                    chat_id: chat,
                    expected_revision: read.revision,
                    source_request_id: restoring.source,
                };
                let name = restoring.name.clone();
                if !self.send_restore(bot, request, command, restoring) {
                    self.fail(&name, Words::BotsErrUnreachable);
                }
            }
            // A trashed Bot reads as not found: its conversation comes back
            // with the Bot, or by its own Restore once the Bot is back.
            Err(Failure::Refused(code)) if code == "not_found" => {
                self.fail(&restoring.name, Words::BotsErrBotTrashed);
            }
            Err(Failure::Refused(code)) => self.fail(&restoring.name, refusal_words(&code)),
            _ => self.fail(&restoring.name, Words::BotsErrUnreachable),
        }
    }

    fn restored(
        &mut self,
        request: BotRequestId,
        restoring: Restoring,
        result: Result<BotResponse, Failure>,
    ) {
        match result {
            Ok(BotResponse::Lifecycle { operation })
                if operation.request_id == request
                    && operation.stage == BotLifecycleStage::Completed =>
            {
                let (bot, kind, reverses, chat) = match restoring.subject {
                    BotSubject::Bot(bot) => (
                        bot,
                        BotLifecycleKind::Restore,
                        Some(BotLifecycleKind::Trash),
                        None,
                    ),
                    BotSubject::Chat { bot, chat } => (
                        bot,
                        BotLifecycleKind::RestoreChat,
                        Some(BotLifecycleKind::TrashChat),
                        Some((chat, restoring.title.clone().unwrap_or_default())),
                    ),
                };
                self.finished.push(Finished {
                    bot,
                    name: restoring.name.clone(),
                    home: restoring.home,
                    kind,
                    request,
                    reverses,
                    chat,
                    at_ms: 0,
                });
                let words = match restoring.subject {
                    BotSubject::Bot(_) => Words::TrashBotRestored,
                    BotSubject::Chat { .. } => Words::TrashBotChatRestored,
                };
                self.receipts.push(Receipt {
                    words: koru_i18n::fill(t(self.lang, words), &[restoring.name.as_str()]),
                });
                self.fresh = false;
            }
            // A Restore is one transaction: anything but *completed* did
            // not happen as asked.
            Ok(_) => self.fail(&restoring.name, Words::BotsErrNotApplied),
            Err(Failure::Unreachable) => self.fail(&restoring.name, Words::BotsErrUnreachable),
            // `require_source`'s `request_conflict`: what it names was
            // restored already, or the Bot moved since.
            Err(Failure::Refused(code)) => {
                self.fail(&restoring.name, restore_refusal_words(&code));
            }
        }
    }

    /// A Restore that did not happen, said in one sentence: the row's name
    /// and why, through one template each language places and spaces.
    fn fail(&mut self, name: &str, why: Words) {
        self.receipts.push(Receipt {
            words: koru_i18n::fill(
                t(self.lang, Words::TrashBotRestoreFailed),
                &[name, t(self.lang, why)],
            ),
        });
        self.fresh = false;
    }

    /// The Restores that finished, for the window to tell the Bot's tabs and
    /// the roster. Taken once.
    pub(crate) fn take_finished(&mut self) -> Vec<Finished> {
        std::mem::take(&mut self.finished)
    }

    /// What the Restores said, for the window's receipt. Taken once.
    pub(crate) fn take_receipts(&mut self) -> Vec<Receipt> {
        std::mem::take(&mut self.receipts)
    }
}

#[cfg(test)]
mod tests;
