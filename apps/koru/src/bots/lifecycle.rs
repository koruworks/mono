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
//! - **What holds a Bot is the owner's to say.** A read of an archived Bot
//!   and the Archived and Trash filters name the completed Archive or Trash
//!   that holds it (`Bot.held_by`), which is what an Unarchive or a Restore
//!   names — after a restart too. The one thing the model remembers itself
//!   is an operation it just finished, until the read that follows reports
//!   its holder: an Unarchive offered the moment an Archive completes.
//! - **A finished operation is reported to the window** ([`Finished`]), which
//!   tells the Bot's other tabs, the roster and the Trash to read again.

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
    /// **The last ask went unanswered** — Koru AI did not answer. It is
    /// still asked about on the clock; the tab says Koru AI is not
    /// answering rather than that the Bot's work is finishing.
    pub unanswered: bool,
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
    /// The finished Archive and Trash that hold the Bot now: the owner's
    /// `held_by`, or the operation this model just finished, before a read
    /// has reported it.
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

    /// The finished Archive that holds the Bot, as last known.
    pub(crate) fn archive_source(&self) -> Option<BotRequestId> {
        self.archive
    }

    /// The finished Trash that holds the Bot, as last known.
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

    /// **The Bot as the owner holds it now.** Its lifecycle says which
    /// finished operation can still hold it, and its open operation — moving
    /// or parked, perhaps begun before Koru last started — is shown and
    /// followed when the tab holds none of its own.
    ///
    /// **Only a read may say this** — a `Read`, or a lifecycle filter's
    /// entry. A write's answer does not report the open operation, so it
    /// goes to [`Self::know_written`], which never drops or replaces one.
    pub(crate) fn know(&mut self, bot: &Bot) {
        if Some(bot.id) != self.bot {
            return;
        }
        self.know_holders(bot);
        // **The owner's open operation is the one shown.** One this tab
        // has on its way stands until it is answered; otherwise an
        // operation the owner no longer names — finished, cancelled or
        // continued from another tab — is dropped, and the one it names is
        // followed in its place.
        let named = bot.open_operation.as_ref();
        if self.op.as_ref().is_some_and(|op| op.in_flight) {
            return;
        }
        match (named, self.op.as_mut()) {
            // **The same operation, at the stage the owner says now**: a
            // parked Archive or Trash the owner took up again by itself
            // (its execution owner answered) is moving and read on the
            // clock; a moving one the owner parked waits for the person.
            (Some(open), Some(op)) if op.request == open.request_id => {
                let parked = open.stage == BotLifecycleStage::Incomplete;
                match (op.stage, parked) {
                    (Stage::Parked, false) => {
                        op.stage = Stage::Moving;
                        op.next_read = Some(Instant::now());
                        op.backoff = READ_FIRST;
                        op.unanswered = false;
                        self.outcome = None;
                    }
                    (Stage::Moving, true) => {
                        op.stage = Stage::Parked;
                        op.next_read = None;
                        op.backoff = READ_FIRST;
                    }
                    _ => {}
                }
            }
            _ => self.op = named.map(|open| adopted(open, bot, Instant::now())),
        }
    }

    /// **The Bot a write answered with** — a settings save's `Bot`. It says
    /// what lifecycle the Bot is in, but nothing of its open operation (the
    /// owner's write answer leaves `open_operation` out, and a
    /// conversation's Trash does not block a save), so the operation this
    /// model follows stays as it is until a read says otherwise.
    pub(crate) fn know_written(&mut self, bot: &Bot) {
        if Some(bot.id) == self.bot {
            self.know_holders(bot);
        }
    }

    /// Which finished operation can still hold the Bot, from its lifecycle.
    fn know_holders(&mut self, bot: &Bot) {
        // The holder the owner names wins; an operation this model finished
        // stands until a read names one (`held_by` came with the read).
        let held = |kind: BotLifecycleKind| {
            bot.held_by
                .as_ref()
                .filter(|holder| holder.kind == kind)
                .map(|holder| holder.request_id)
        };
        match bot.lifecycle {
            BotLifecycle::Active => {
                self.archive = None;
                self.trash = None;
            }
            BotLifecycle::Archived => {
                self.archive = held(BotLifecycleKind::Archive).or(self.archive);
                self.trash = None;
            }
            BotLifecycle::Trashed => {
                self.trash = held(BotLifecycleKind::Trash).or(self.trash);
            }
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
            unanswered: false,
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
            unanswered: false,
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
        op.unanswered = false;
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
            unanswered: false,
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
        let kind = op.kind;
        // An answer about something else is no answer: what happened is
        // as unknown as after a lost connection.
        let result = match result {
            Ok(BotResponse::Lifecycle { operation }) if operation.request_id == request => {
                Ok(operation)
            }
            Ok(_) => Err(Failure::Unanswered),
            Err(failure) => Err(failure),
        };
        op.unanswered = matches!(result, Err(Failure::Unanswered));
        match result {
            Ok(operation) => {
                op.answered = true;
                self.fold(&operation, now);
            }
            Err(Failure::Unanswered) => {
                // **The outcome is unknown** — a lost connection, a worker
                // that timed out, a reply that did not parse: the command
                // may well have run. A repeat of the read is progress, so
                // ask about it on the clock rather than claim either.
                op.next_read = Some(now.checked_add(op.backoff).unwrap_or(now));
                op.backoff = (op.backoff * 2).min(READ_MAX);
            }
            // A read of an operation the owner never received.
            Err(Failure::Refused(code)) if code == "not_found" && !op.answered => {
                self.refuse(Words::BotsErrUnreachable);
            }
            Err(Failure::Refused(code)) => self.refuse(words_for(kind, &code)),
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
            _ => Outcome::Refused(Words::BotsErrNotDone),
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
///
/// **A moving one is read on the bounded clock from now**, as one this tab
/// sent is, so a tab that learned it after a restart or from another tab
/// sees it finish; a parked one waits for the person.
fn adopted(open: &BotOpenLifecycle, bot: &Bot, now: Instant) -> Op {
    let parked = open.stage == BotLifecycleStage::Incomplete;
    Op {
        request: open.request_id,
        kind: open.kind,
        chat: open.chat_id,
        reverses: None,
        stage: if parked { Stage::Parked } else { Stage::Moving },
        command: None,
        in_flight: false,
        answered: true,
        unanswered: false,
        next_read: (!parked).then_some(now),
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
/// (`bot_conversation_trashed`, `bot_lifecycle_pending`), and an Archive's,
/// a Trash's or a conversation's Trash's `request_conflict`, which means
/// another operation of the Bot is unfinished (a fresh request is minted
/// for every new command, so it is never this request's own repeat).
/// Everything else in the general sentences — no settings are shown where
/// a lifecycle refusal is said ([`super::settings::general_refusal_words`]).
/// A Restore's is [`restore_refusal_words`].
pub(crate) fn refusal_words(code: &str) -> Words {
    match code {
        "request_conflict" => Words::BotsErrPending,
        _ => super::settings::general_refusal_words(code),
    }
}

/// **A Restore's refusal** — an Unarchive, a Restore from the Trash, a
/// conversation's, or the Cancel of a parked operation. Its
/// `request_conflict` is the owner's `require_source` (or the lifecycle's
/// prior state) and its `revision_conflict` is `capture_on`'s revision
/// check: the operation it names was restored already, or the Bot moved
/// since — another tab, another window, the Trash. Neither says it *was
/// not* restored: whoever asked reads the Bot again, and that read says
/// what holds it now.
pub(crate) fn restore_refusal_words(code: &str) -> Words {
    if restored_or_moved(code) {
        Words::BotsErrRestoreConflict
    } else {
        refusal_words(code)
    }
}

/// **A Restore refused because it was done already, or the Bot moved**
/// (`require_source`'s and `allowed_prior`'s `request_conflict`, and
/// `capture_on`'s `revision_conflict`): not a Restore that failed. Whoever
/// asked reads again and lets the read say what the Bot is.
pub(crate) fn restored_or_moved(code: &str) -> bool {
    matches!(code, "request_conflict" | "revision_conflict")
}

/// The refusal's sentence for an operation of `kind`.
fn words_for(kind: BotLifecycleKind, code: &str) -> Words {
    match kind {
        BotLifecycleKind::Restore | BotLifecycleKind::RestoreChat => restore_refusal_words(code),
        _ => refusal_words(code),
    }
}

/// The sentence a parked operation says, by what it is.
pub(crate) fn parked_words(kind: BotLifecycleKind) -> Words {
    match kind {
        BotLifecycleKind::TrashChat => Words::BotsChatTrashIncomplete,
        _ => Words::BotsLifecycleIncomplete,
    }
}

#[cfg(test)]
mod tests;
