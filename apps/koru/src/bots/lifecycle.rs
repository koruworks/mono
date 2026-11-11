// SPDX-License-Identifier: MPL-2.0

//! **A Bot's lifecycle, as the window holds it** (Bots P1.8 S14; P1.7):
//! Archive, Move to Trash, Unarchive and Restore of a whole Bot, and Move to
//! Trash of one of its conversations.
//!
//! This is the model, with no drawing and no thread. The Bot tab asks it for
//! the [`Command`] a confirmed choice means, hands it the link's answers, and
//! draws what it says. Everything a person sees comes from the owner's
//! answer, never from the press:
//!
//! - **An operation takes time.** Archive and Trash stop the Bot's work as
//!   the person's own Stop and wait until it is proven ended, so the owner
//!   answers at once with the operation's progress and the model reads it
//!   again (`ReadLifecycleOperation`) until it says *completed* or parks it.
//!   It is read again only while the tab is shown: at once when the tab is
//!   shown and when the conversation's status moves, and otherwise at the
//!   settings' own pace for an operation the owner holds as pending
//!   (250 ms, doubling to 2 s). Nothing is read while it is parked.
//! - **A parked operation waits for the person.** It is never retried on its
//!   own. *Continue* sends the same command again, by its request, which the
//!   owner resumes; *Cancel* is the Restore naming it.
//! - **Every write carries the revision it read**, and a refusal is a
//!   sentence by its code ([`refusal_words`]), never the server's English.
//! - **A finished operation is reported to the window** ([`Finished`]), which
//!   keeps the book ([`Book`]) the Trash and the roster restore from: the
//!   owner's reads name an unfinished operation (`Bot.open_operation`) but
//!   no finished one, so the request a Restore must name is known only to
//!   the window that did it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use koru_ai_core::ChatId;
use koru_ai_core::bots::{
    Bot, BotCommand, BotId, BotLifecycle, BotLifecycleKind, BotLifecycleProgress,
    BotLifecycleStage, BotOpenLifecycle, BotRequestId, BotResponse,
};
use koru_i18n::Key as Words;
use koru_store_core::ids::WorkspaceId;

use super::link::{Command, Failure};

/// How soon an operation the owner holds as moving is read again while the
/// tab is shown, doubling to [`READ_MAX`]: the settings' own pace for an
/// operation the owner holds as pending.
const READ_FIRST: Duration = Duration::from_millis(250);
const READ_MAX: Duration = Duration::from_secs(2);

/// What a person asked for on a whole Bot.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    Archive,
    Trash,
    /// The Restore naming the Archive that holds the Bot.
    Unarchive,
    /// The Restore naming the Trash that holds the Bot.
    Restore,
}

/// Where an operation stands, as the owner last said.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    /// Sent, or read, and the owner holds it as moving.
    Moving,
    /// The owner parked it as incomplete: it waits for the person.
    Parked,
}

/// One operation on its way.
#[derive(Clone, Debug)]
pub(crate) struct Op {
    pub request: BotRequestId,
    pub kind: BotLifecycleKind,
    /// The conversation a conversation's operation is about.
    pub chat: Option<ChatId>,
    /// For a Restore: what it reverses, when known.
    pub reverses: Option<BotLifecycleKind>,
    pub stage: Stage,
    /// The command as sent, so *Continue* sends exactly it again. `None`
    /// for an operation learned from the Bot's read, rebuilt on *Continue*.
    command: Option<BotCommand>,
    /// A command or a read for it is with the link.
    in_flight: bool,
    /// The owner has answered something about it: a read that answers
    /// `not_found` before that means the command never reached it.
    answered: bool,
    next_read: Option<Instant>,
    backoff: Duration,
    /// What the window records when it finishes: the Bot's name and home,
    /// and the conversation's title.
    name: String,
    home: Option<WorkspaceId>,
    title: Option<String>,
}

/// What a finished operation leaves the tab saying.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Archived,
    /// Unarchived: active again, and paused.
    Unarchived,
    Trashed,
    /// The Restore of a Trash, or of a parked Archive: back, and paused.
    Restored,
    /// One conversation went to Trash.
    ChatTrashed {
        chat: ChatId,
    },
    /// One conversation's Trash was reversed (its parked Trash cancelled).
    ChatKept {
        chat: ChatId,
    },
    /// The owner refused, or did not answer: the sentence for why.
    Refused(Words),
}

/// **A finished operation, for the window's book**: enough to undo it from
/// elsewhere — the Trash, the roster's Archived filter — and to name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Finished {
    pub bot: BotId,
    pub name: String,
    pub home: Option<WorkspaceId>,
    pub kind: BotLifecycleKind,
    pub request: BotRequestId,
    pub reverses: Option<BotLifecycleKind>,
    pub chat: Option<(ChatId, String)>,
    pub at_ms: i64,
}

#[derive(Default)]
pub(crate) struct Lifecycle {
    bot: Option<BotId>,
    op: Option<Op>,
    outcome: Option<Outcome>,
    /// The finished Archive and Trash that hold the Bot now, when known.
    archive: Option<BotRequestId>,
    trash: Option<BotRequestId>,
    finished: Vec<Finished>,
    read_bot_owed: bool,
    /// The outcome an answer just reached, for the tab to act on once.
    just: Option<Outcome>,
}

impl Lifecycle {
    /// Whether `request` is the operation on its way: its answer is this
    /// model's.
    pub(crate) fn owns(&self, request: BotRequestId) -> bool {
        self.op.as_ref().is_some_and(|op| op.request == request)
    }

    /// The outcome the last answer reached, taken once: the tab moves off a
    /// conversation that went to Trash, shows a Bot that went, and so on.
    pub(crate) fn take_just(&mut self) -> Option<Outcome> {
        self.just.take()
    }

    pub(crate) fn op(&self) -> Option<&Op> {
        self.op.as_ref()
    }

    pub(crate) fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    /// Whether an operation is moving, so the tab's controls that would
    /// race it are inert.
    pub(crate) fn busy(&self) -> bool {
        self.op.is_some()
    }

    /// The finished Archive the window knows holds the Bot.
    pub(crate) fn archive_source(&self) -> Option<BotRequestId> {
        self.archive
    }

    /// The finished Trash the window knows holds the Bot.
    pub(crate) fn trash_source(&self) -> Option<BotRequestId> {
        self.trash
    }

    /// **Which Bot this is about.** Another Bot's operation is never this
    /// one's: a change starts over.
    pub(crate) fn set_bot(&mut self, bot: BotId) {
        if self.bot != Some(bot) {
            *self = Self {
                bot: Some(bot),
                ..Self::default()
            };
        }
    }

    /// What the window's book knows holds the Bot: an operation this window
    /// finished, perhaps in another tab, the Trash or the roster.
    pub(crate) fn know_sources(
        &mut self,
        archive: Option<BotRequestId>,
        trash: Option<BotRequestId>,
    ) {
        self.archive = archive.or(self.archive);
        self.trash = trash.or(self.trash);
    }

    /// **The Bot as the owner holds it now.** Its lifecycle says which
    /// finished operation can still hold it, and its open operation — moving
    /// or parked, perhaps begun before Koru last started — is shown and
    /// followed when the tab holds none of its own.
    pub(crate) fn know(&mut self, bot: &Bot) {
        if Some(bot.id) != self.bot {
            return;
        }
        match bot.lifecycle {
            BotLifecycle::Active => {
                self.archive = None;
                self.trash = None;
            }
            BotLifecycle::Archived => self.trash = None,
            BotLifecycle::Trashed => {}
        }
        if self.op.is_none()
            && let Some(open) = &bot.open_operation
        {
            self.op = Some(adopted(open, bot));
        }
    }

    /// **The command a confirmed choice means**, at the revision `bot` was
    /// read at. `None` while another operation is on its way, and for an
    /// Unarchive or a Restore whose operation the window does not know.
    pub(crate) fn begin(&mut self, ask: Ask, bot: &Bot) -> Option<Command> {
        if self.op.is_some() || Some(bot.id) != self.bot {
            return None;
        }
        let request = BotRequestId::mint();
        let (bot_id, revision) = (bot.id, bot.revision);
        let (kind, reverses, command) = match ask {
            Ask::Archive => (
                BotLifecycleKind::Archive,
                None,
                BotCommand::Archive {
                    request_id: request,
                    bot_id,
                    expected_revision: revision,
                },
            ),
            Ask::Trash => (
                BotLifecycleKind::Trash,
                None,
                BotCommand::Trash {
                    request_id: request,
                    bot_id,
                    expected_revision: revision,
                },
            ),
            Ask::Unarchive => (
                BotLifecycleKind::Restore,
                Some(BotLifecycleKind::Archive),
                BotCommand::Restore {
                    request_id: request,
                    bot_id,
                    expected_revision: revision,
                    source_request_id: self.archive?,
                },
            ),
            Ask::Restore => (
                BotLifecycleKind::Restore,
                Some(BotLifecycleKind::Trash),
                BotCommand::Restore {
                    request_id: request,
                    bot_id,
                    expected_revision: revision,
                    source_request_id: self.trash?,
                },
            ),
        };
        Some(self.send(Op {
            request,
            kind,
            chat: None,
            reverses,
            stage: Stage::Moving,
            command: Some(command),
            in_flight: true,
            answered: false,
            next_read: None,
            backoff: READ_FIRST,
            name: bot.config.name.clone(),
            home: Some(bot.home_workspace),
            title: None,
        }))
    }

    /// **One conversation to Trash** (`TrashChat`), at the revision `bot`
    /// was read at. Its siblings keep running; the Bot is not paused.
    pub(crate) fn begin_chat_trash(
        &mut self,
        bot: &Bot,
        chat: ChatId,
        title: &str,
    ) -> Option<Command> {
        if self.op.is_some() || Some(bot.id) != self.bot {
            return None;
        }
        let request = BotRequestId::mint();
        let command = BotCommand::TrashChat {
            request_id: request,
            bot_id: bot.id,
            chat_id: chat,
            expected_revision: bot.revision,
        };
        Some(self.send(Op {
            request,
            kind: BotLifecycleKind::TrashChat,
            chat: Some(chat),
            reverses: None,
            stage: Stage::Moving,
            command: Some(command),
            in_flight: true,
            answered: false,
            next_read: None,
            backoff: READ_FIRST,
            name: bot.config.name.clone(),
            home: Some(bot.home_workspace),
            title: Some(title.to_owned()),
        }))
    }

    /// **Continue a parked operation**: the same command again, by its
    /// request, which the owner resumes whatever revision it carries.
    pub(crate) fn continue_parked(&mut self, bot: &Bot) -> Option<Command> {
        let op = self.op.as_mut()?;
        if op.stage != Stage::Parked || Some(bot.id) != self.bot {
            return None;
        }
        let command = match op.command.clone() {
            Some(command) => command,
            None => resume_command(op, bot)?,
        };
        op.stage = Stage::Moving;
        op.in_flight = true;
        op.next_read = None;
        op.backoff = READ_FIRST;
        self.outcome = None;
        Some(Command::Write {
            bot: bot.id,
            request: op.request,
            command: Box::new(command),
        })
    }

    /// **Cancel a parked operation**: the Restore naming it, at the revision
    /// `bot` was read at. A conversation's parked Trash is undone by its
    /// own `RestoreChat`.
    pub(crate) fn cancel_parked(&mut self, bot: &Bot) -> Option<Command> {
        let parked = self.op.as_ref()?;
        if parked.stage != Stage::Parked || Some(bot.id) != self.bot {
            return None;
        }
        let request = BotRequestId::mint();
        let source = parked.request;
        let (kind, command) = match (parked.kind, parked.chat) {
            (BotLifecycleKind::TrashChat, Some(chat)) => (
                BotLifecycleKind::RestoreChat,
                BotCommand::RestoreChat {
                    request_id: request,
                    bot_id: bot.id,
                    chat_id: chat,
                    expected_revision: bot.revision,
                    source_request_id: source,
                },
            ),
            (BotLifecycleKind::Archive | BotLifecycleKind::Trash, _) => (
                BotLifecycleKind::Restore,
                BotCommand::Restore {
                    request_id: request,
                    bot_id: bot.id,
                    expected_revision: bot.revision,
                    source_request_id: source,
                },
            ),
            _ => return None,
        };
        let op = Op {
            request,
            kind,
            chat: parked.chat,
            reverses: Some(parked.kind),
            stage: Stage::Moving,
            command: Some(command),
            in_flight: true,
            answered: false,
            next_read: None,
            backoff: READ_FIRST,
            name: parked.name.clone(),
            home: parked.home,
            title: parked.title.clone(),
        };
        self.op = None;
        Some(self.send(op))
    }

    fn send(&mut self, op: Op) -> Command {
        let bot = self.bot.expect("a Bot to change");
        let command = op.command.clone().expect("a command to send");
        let request = op.request;
        self.outcome = None;
        self.op = Some(op);
        Command::Write {
            bot,
            request,
            command: Box::new(command),
        }
    }

    /// **Read the operation now** — the tab was shown, or its conversation's
    /// status moved. Nothing for a parked operation: it waits for the person.
    pub(crate) fn read_now(&mut self, now: Instant) {
        if let Some(op) = &mut self.op
            && op.stage == Stage::Moving
            && !op.in_flight
        {
            op.next_read = Some(now);
        }
    }

    /// The `ReadLifecycleOperation` owed by `now`, marked as with the link.
    pub(crate) fn read(&mut self, now: Instant) -> Option<Command> {
        let bot = self.bot?;
        let op = self.op.as_mut()?;
        if op.stage != Stage::Moving || op.in_flight || !op.next_read.is_some_and(|at| at <= now) {
            return None;
        }
        op.in_flight = true;
        Some(Command::Write {
            bot,
            request: op.request,
            command: Box::new(BotCommand::ReadLifecycleOperation {
                request_id: op.request,
                bot_id: bot,
            }),
        })
    }

    /// When a pass is next owed: a short tick while something is with the
    /// link, the next read while the owner holds it as moving. `None` while
    /// it is parked or there is none.
    pub(crate) fn deadline(&self, now: Instant) -> Option<Instant> {
        let op = self.op.as_ref()?;
        if op.in_flight {
            return now.checked_add(Duration::from_millis(50));
        }
        match op.stage {
            Stage::Moving => op.next_read,
            Stage::Parked => None,
        }
    }

    /// The link would not take `request`'s command or read.
    pub(crate) fn unsent(&mut self, request: BotRequestId, now: Instant) {
        let Some(op) = self.op.as_mut().filter(|op| op.request == request) else {
            return;
        };
        op.in_flight = false;
        if op.answered {
            // The owner holds it: ask again on a later pass.
            op.next_read = Some(now.checked_add(op.backoff).unwrap_or(now));
        } else {
            // Nothing left: nothing is on its way.
            self.op = None;
            self.outcome = Some(Outcome::Refused(Words::BotsErrUnreachable));
        }
    }

    /// **What the owner said about `request`**. `true` when it was this
    /// model's.
    pub(crate) fn answer(
        &mut self,
        request: BotRequestId,
        result: Result<BotResponse, Failure>,
        now: Instant,
    ) -> bool {
        let Some(op) = self.op.as_mut().filter(|op| op.request == request) else {
            return false;
        };
        op.in_flight = false;
        match result {
            Ok(BotResponse::Lifecycle { operation }) if operation.request_id == request => {
                op.answered = true;
                self.fold(&operation, now);
            }
            Ok(_) => self.refuse(Words::BotsErrNotApplied),
            Err(Failure::Unreachable) => {
                // The command may or may not have reached the owner; a
                // repeat is progress, so ask about it rather than claim
                // either.
                op.next_read = Some(now.checked_add(op.backoff).unwrap_or(now));
                op.backoff = (op.backoff * 2).min(READ_MAX);
            }
            // A read of an operation the owner never received.
            Err(Failure::Refused(code)) if code == "not_found" && !op.answered => {
                self.refuse(Words::BotsErrUnreachable);
            }
            Err(Failure::Refused(code)) => self.refuse(refusal_words(&code)),
        }
        true
    }

    fn refuse(&mut self, words: Words) {
        self.op = None;
        self.outcome = Some(Outcome::Refused(words));
        self.just = Some(Outcome::Refused(words));
        self.read_bot_owed = true;
    }

    fn fold(&mut self, progress: &BotLifecycleProgress, now: Instant) {
        let Some(op) = self.op.as_mut() else {
            return;
        };
        match progress.stage {
            BotLifecycleStage::Completed => self.finish(),
            BotLifecycleStage::Incomplete => {
                op.stage = Stage::Parked;
                op.next_read = None;
                op.backoff = READ_FIRST;
                // A Bot read shows its revision now: Cancel names it.
                self.read_bot_owed = true;
            }
            _ => {
                let wait = op.backoff;
                op.stage = Stage::Moving;
                op.next_read = Some(now.checked_add(wait).unwrap_or(now));
                op.backoff = (wait * 2).min(READ_MAX);
            }
        }
    }

    fn finish(&mut self) {
        let Some(op) = self.op.take() else {
            return;
        };
        let outcome = match (op.kind, op.reverses, op.chat) {
            (BotLifecycleKind::Archive, _, _) => {
                self.archive = Some(op.request);
                Outcome::Archived
            }
            (BotLifecycleKind::Trash, _, _) => {
                self.trash = Some(op.request);
                Outcome::Trashed
            }
            (BotLifecycleKind::Restore, Some(BotLifecycleKind::Archive), _) => {
                self.archive = None;
                Outcome::Unarchived
            }
            (BotLifecycleKind::Restore, _, _) => {
                self.trash = None;
                Outcome::Restored
            }
            (BotLifecycleKind::TrashChat, _, Some(chat)) => Outcome::ChatTrashed { chat },
            (BotLifecycleKind::RestoreChat, _, Some(chat)) => Outcome::ChatKept { chat },
            _ => Outcome::Refused(Words::BotsErrNotApplied),
        };
        if let Some(bot) = self.bot {
            self.finished.push(Finished {
                bot,
                name: op.name.clone(),
                home: op.home,
                kind: op.kind,
                request: op.request,
                reverses: op.reverses,
                chat: op.chat.zip(op.title.clone()),
                at_ms: now_ms(),
            });
        }
        self.just = Some(outcome.clone());
        self.outcome = Some(outcome);
        self.read_bot_owed = true;
    }

    /// The operations finished since the window last asked, for its book.
    pub(crate) fn take_finished(&mut self) -> Vec<Finished> {
        std::mem::take(&mut self.finished)
    }

    /// A read of the Bot is owed: an operation finished, parked or was
    /// refused, so what the tab shows may be old. Taken once.
    pub(crate) fn take_read_bot_owed(&mut self) -> bool {
        std::mem::take(&mut self.read_bot_owed)
    }

    /// **Another surface of this window changed the Bot** — the Trash
    /// restored it, the roster unarchived it: the outcome this tab showed is
    /// old.
    pub(crate) fn moved_elsewhere(&mut self) {
        if self.op.is_none() {
            self.outcome = None;
        }
        self.read_bot_owed = true;
    }
}

/// The operation a Bot's read names, as the tab follows it.
fn adopted(open: &BotOpenLifecycle, bot: &Bot) -> Op {
    Op {
        request: open.request_id,
        kind: open.kind,
        chat: open.chat_id,
        reverses: None,
        stage: if open.stage == BotLifecycleStage::Incomplete {
            Stage::Parked
        } else {
            Stage::Moving
        },
        command: None,
        in_flight: false,
        answered: true,
        next_read: None,
        backoff: READ_FIRST,
        name: bot.config.name.clone(),
        home: Some(bot.home_workspace),
        title: None,
    }
}

/// The command that resumes a parked operation learned from the Bot's read:
/// the same kind under the same request and Bot (and conversation), which
/// the owner resumes whatever revision it carries.
fn resume_command(op: &Op, bot: &Bot) -> Option<BotCommand> {
    let (request_id, bot_id, expected_revision) = (op.request, bot.id, bot.revision);
    Some(match op.kind {
        BotLifecycleKind::Archive => BotCommand::Archive {
            request_id,
            bot_id,
            expected_revision,
        },
        BotLifecycleKind::Trash => BotCommand::Trash {
            request_id,
            bot_id,
            expected_revision,
        },
        BotLifecycleKind::TrashChat => BotCommand::TrashChat {
            request_id,
            bot_id,
            chat_id: op.chat?,
            expected_revision,
        },
        _ => return None,
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// **A lifecycle refusal's sentence, by its stable code**: the holds
/// (`bot_conversation_trashed`, `bot_lifecycle_pending`), and a lifecycle
/// command's `request_conflict`, which means another operation of the Bot
/// is unfinished (a fresh request is minted for every new command, so it is
/// never this request's own repeat). Everything else as the settings say it
/// ([`super::settings::refusal_words`]).
pub(crate) fn refusal_words(code: &str) -> Words {
    match code {
        "request_conflict" => Words::BotsErrPending,
        _ => super::settings::refusal_words(code),
    }
}

/// The sentence a parked operation says, by what it is.
pub(crate) fn parked_words(kind: BotLifecycleKind) -> Words {
    match kind {
        BotLifecycleKind::TrashChat => Words::BotsChatTrashIncomplete,
        _ => Words::BotsLifecycleIncomplete,
    }
}

/// **What the window remembers about the lifecycle operations it finished**
/// — the one place the request a Restore must name can be found, because the
/// owner's reads name no finished operation. Kept in memory for the window's
/// life: after Koru restarts, a Bot or a conversation trashed before has no
/// Restore here (the Trash says so), until the owner can be asked for it.
#[derive(Default, Debug)]
pub(crate) struct Book {
    bots: HashMap<BotId, BookBot>,
    chats: HashMap<ChatId, BookChat>,
}

/// One Bot the window archived or trashed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BookBot {
    pub name: String,
    pub home: Option<WorkspaceId>,
    pub archive: Option<BotRequestId>,
    pub trash: Option<(BotRequestId, i64)>,
}

/// One Bot conversation the window moved to Trash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BookChat {
    pub chat: ChatId,
    pub bot: BotId,
    pub bot_name: String,
    pub title: String,
    pub home: Option<WorkspaceId>,
    pub request: BotRequestId,
    pub at_ms: i64,
}

impl Book {
    /// Fold a finished operation in. `true` when the book changed.
    pub(crate) fn record(&mut self, finished: &Finished) -> bool {
        match (finished.kind, finished.reverses, &finished.chat) {
            (BotLifecycleKind::Archive, _, _) => {
                let entry = self.bots.entry(finished.bot).or_default();
                entry.name = finished.name.clone();
                entry.home = finished.home;
                entry.archive = Some(finished.request);
            }
            (BotLifecycleKind::Trash, _, _) => {
                let entry = self.bots.entry(finished.bot).or_default();
                entry.name = finished.name.clone();
                entry.home = finished.home;
                entry.trash = Some((finished.request, finished.at_ms));
            }
            (BotLifecycleKind::Restore, Some(BotLifecycleKind::Archive), _) => {
                if let Some(entry) = self.bots.get_mut(&finished.bot) {
                    entry.archive = None;
                }
            }
            (BotLifecycleKind::Restore, _, _) => {
                if let Some(entry) = self.bots.get_mut(&finished.bot) {
                    entry.trash = None;
                }
            }
            (BotLifecycleKind::TrashChat, _, Some((chat, title))) => {
                self.chats.insert(
                    *chat,
                    BookChat {
                        chat: *chat,
                        bot: finished.bot,
                        bot_name: finished.name.clone(),
                        title: title.clone(),
                        home: finished.home,
                        request: finished.request,
                        at_ms: finished.at_ms,
                    },
                );
            }
            (BotLifecycleKind::RestoreChat, _, Some((chat, _))) => {
                return self.chats.remove(chat).is_some();
            }
            _ => return false,
        }
        true
    }

    /// The finished Archive and Trash the window knows hold `bot`.
    pub(crate) fn sources(&self, bot: BotId) -> (Option<BotRequestId>, Option<BotRequestId>) {
        self.bots.get(&bot).map_or((None, None), |entry| {
            (entry.archive, entry.trash.map(|(request, _)| request))
        })
    }

    /// Every Bot whose finished Archive the window knows, and that Archive:
    /// what the roster's Unarchive names.
    pub(crate) fn archive_sources(&self) -> HashMap<BotId, BotRequestId> {
        self.bots
            .iter()
            .filter_map(|(bot, entry)| entry.archive.map(|request| (*bot, request)))
            .collect()
    }

    /// The Bot's own record, when the window holds one.
    pub(crate) fn bot(&self, bot: BotId) -> Option<&BookBot> {
        self.bots.get(&bot)
    }

    /// The conversations the window moved to Trash whose Bot's home is
    /// `workspace`, newest first.
    pub(crate) fn trashed_chats(&self, workspace: Option<WorkspaceId>) -> Vec<BookChat> {
        let mut chats: Vec<BookChat> = self
            .chats
            .values()
            .filter(|chat| workspace.is_some() && chat.home == workspace)
            .cloned()
            .collect();
        chats.sort_by(|a, b| {
            b.at_ms
                .cmp(&a.at_ms)
                .then(a.chat.as_uuid().cmp(&b.chat.as_uuid()))
        });
        chats
    }

    /// A conversation the window moved to Trash, by its id.
    pub(crate) fn chat(&self, chat: ChatId) -> Option<&BookChat> {
        self.chats.get(&chat)
    }
}

#[cfg(test)]
mod tests;
