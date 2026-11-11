// SPDX-License-Identifier: MPL-2.0

//! **Trashed Bots and Bot conversations, for the window's Trash** (Bots P1.8
//! S14; P1.7 S9).
//!
//! A Bot is not a workspace document, so its rows reach the Trash by their
//! own road, as a run's do ([`crate::mainview`]'s `run_trash`): this feed
//! reads the owner's Trash filter (`ListLifecycle`, trashed) for the window's
//! workspace on its own [`BotsLink`] thread, adds the conversations this
//! window moved to Trash from the window's [`Book`], and hands the Trash
//! view plain rows ([`TrashedBotRow`]). Restore is the owner's: the Restore
//! naming the Trash for a Bot, `RestoreChat` naming the conversation's own
//! Trash for one conversation, each at the revision the owner last said.
//!
//! **What it cannot do, and says.** The owner's reads name no finished
//! operation, so a Bot trashed before this window started has no Restore
//! here; its row says it cannot be restored from here yet. Permanent
//! deletion of a Bot is not built: no row of this feed is ever deleted, and
//! emptying the Trash leaves them, saying so.
//!
//! **Only while a Trash tab is in front.** A hidden feed sends nothing; shown
//! again, it reads once.

use std::collections::HashMap;
use std::path::PathBuf;

use koru_ai_core::ChatId;
use koru_ai_core::bots::{
    Bot, BotCommand, BotId, BotLifecycle, BotLifecycleKind, BotLifecycleStage, BotRequestId,
    BotResponse,
};
use koru_i18n::{Key as Words, Lang, t};
use koru_store_core::ids::WorkspaceId;

use super::lifecycle::{Book, Finished, refusal_words};
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
    /// Whether Restore can be asked for here: the window knows the Trash it
    /// names, and — for a conversation — its Bot is not in Trash itself.
    pub restorable: bool,
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
        self.fresh = false;
        self.in_flight = false;
        self.seq = self.seq.wrapping_add(1);
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
        (self.shown && self.in_flight) || self.restoring_any()
    }

    /// A Restore the person asked for is on its way — from the Trash, or a
    /// link's receipt — and its answer is folded wherever the Trash is.
    fn restoring_any(&self) -> bool {
        !self.restoring.is_empty() || !self.reading.is_empty()
    }

    /// The feed's pass: ask when owed, fold what came back. A hidden feed
    /// asks nothing; it only takes the answer to a Restore the person
    /// asked for. `true` when its rows may have changed.
    pub(crate) fn maintain(&mut self) -> bool {
        if !self.shown {
            return self.restoring_any() && self.poll();
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

    /// **The rows**, newest first: the owner's trashed Bots of the
    /// workspace, and the conversations this window moved to Trash there.
    pub(crate) fn rows(&self, book: &Book) -> Vec<TrashedBotRow> {
        let mut rows: Vec<TrashedBotRow> = self
            .listed
            .iter()
            .map(|bot| TrashedBotRow {
                subject: BotSubject::Bot(bot.id),
                name: bot.config.name.clone(),
                origin: t(self.lang, Words::TrashBotKind).to_owned(),
                trashed_at_ms: book
                    .bot(bot.id)
                    .and_then(|entry| entry.trash.map(|(_, at)| at))
                    .or(Some(bot.updated_at_ms)),
                restorable: book.sources(bot.id).1.is_some(),
            })
            .collect();
        for chat in book.trashed_chats(self.workspace) {
            let bot_trashed = self.listed.iter().any(|bot| bot.id == chat.bot);
            rows.push(TrashedBotRow {
                subject: BotSubject::Chat {
                    bot: chat.bot,
                    chat: chat.chat,
                },
                name: chat.title.clone(),
                origin: koru_i18n::fill(
                    t(self.lang, Words::TrashBotConversation),
                    &[chat.bot_name.as_str()],
                ),
                trashed_at_ms: Some(chat.at_ms),
                restorable: !bot_trashed,
            });
        }
        rows.sort_by(|a, b| b.trashed_at_ms.cmp(&a.trashed_at_ms));
        rows
    }

    /// **Restore `subject`** from the Trash. A Bot's Restore names the Trash
    /// the window knows, at the revision the Trash filter said; a
    /// conversation's reads its Bot first for the revision. `false` when
    /// nothing was sent — a row with no Restore, or the link would not take
    /// it.
    pub(crate) fn restore(&mut self, subject: BotSubject, book: &Book) -> bool {
        if self.link.is_none() {
            return false;
        }
        match subject {
            BotSubject::Bot(id) => {
                let Some(bot) = self.listed.iter().find(|bot| bot.id == id).cloned() else {
                    return false;
                };
                let Some(source) = book.sources(id).1 else {
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
            BotSubject::Chat { bot, chat } => {
                let Some(entry) = book.chat(chat) else {
                    return false;
                };
                let restoring = Restoring {
                    subject,
                    name: entry.title.clone(),
                    source: entry.request,
                    home: entry.home,
                    title: Some(entry.title.clone()),
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
        }
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
            Err(Failure::Refused(code)) => self.fail(&restoring.name, refusal_words(&code)),
        }
    }

    fn fail(&mut self, name: &str, why: Words) {
        self.receipts.push(Receipt {
            words: format!(
                "{} {}",
                koru_i18n::fill(t(self.lang, Words::TrashBotRestoreFailed), &[name]),
                t(self.lang, why)
            ),
        });
        self.fresh = false;
    }

    /// The Restores that finished, for the window's book. Taken once.
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
