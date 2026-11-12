// SPDX-License-Identifier: MPL-2.0

//! **The Bot service, on a thread of its own.**
//!
//! The roster's one way to the AI service's `/v1/ai/bots`. A command is
//! queued on a bounded channel and answered on another; the window thread
//! only ever `try_send`s and `try_recv`s, so a service that is slow or gone
//! costs it nothing.
//!
//! **First-party, and asking for exactly two scopes.** Bot reads need
//! `bots.manage`, which the gateway grants only to a caller the Agent vouches
//! for (`Client::connect_first_party`); `ai.chat` is the AI service's own.
//! Naming scopes filters rather than adds, so nothing else is held.
//!
//! **Never under test.** [`BotsLink::start`] answers `None` in this crate's
//! suite, so no test can dial the gateway of a Koru that is running on the
//! developer's machine; a test drives [`BotsLink::test_pair`] instead.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};

use koru_ai_core::ChatId;
use koru_ai_core::bots::{
    BOT_CHAT_PAGE_LIMIT, BOT_CREATION_PAGE_LIMIT, Bot, BotActivityPage, BotChatCursor,
    BotChatLabel, BotChatPage, BotCommand, BotCreation, BotCreationCursor, BotId, BotLifecycle,
    BotRequestId, BotResponse, BotTrashedChat, BotTrashedChatCursor,
};
use koru_store_core::ids::WorkspaceId;

/// Commands waiting for the thread. A roster asks one list at a time, so
/// this only has to absorb a refresh or two.
const COMMAND_CAPACITY: usize = 4;
/// Answers waiting for the window's next pass.
const ANSWER_CAPACITY: usize = 8;

/// The scopes the link asks for — and, because naming scopes filters rather
/// than adds, the only ones it holds.
pub(crate) const SCOPES: &[&str] = &["ai.chat", "bots.manage"];

pub(crate) enum Command {
    /// The Bots whose home is `workspace`, as the service lists them — and,
    /// as a second answer, its unfinished creations (S8).
    List { seq: u64, workspace: WorkspaceId },
    /// One Bot, as the service holds it now: what a Bot tab's header says.
    Read { seq: u64, bot: BotId },
    /// **A change a person asked for in a Bot's settings** (Bots P1.8 S5):
    /// an `Update`, `UpdateAccess`, `Pause` or `Resume` carrying the
    /// revision it read, or the `ReadOperation` that follows one until the
    /// owner says it is effective. Answered by its request's id, so a reply
    /// can only settle the change it is about. A creation rides it too (S8):
    /// `CreateBotWithRole`, `ReadBotCreation`, `RetryBotCreation` and
    /// `AbandonBotCreation`, each by the creation's request id and its
    /// reserved BotId, each answered with the creation's progress.
    Write {
        bot: BotId,
        request: BotRequestId,
        command: Box<BotCommand>,
    },
    /// **Whose conversation `chat` is** (Bots P1.8, B1): the owner's own
    /// read of the chat, whose `Chat.bot` labels a Bot's conversation for a
    /// caller that may open it. Only the label is kept.
    Label { chat: ChatId },
    /// **One page of a Bot's conversations** (S8): `ListBotChats`, main
    /// first then newest, Trash left out; `cursor` is the previous page's.
    ListChats {
        seq: u64,
        bot: BotId,
        cursor: Option<BotChatCursor>,
        limit: u16,
    },
    /// **The workspace's running and Needs-you Bot conversations** (S10):
    /// `ListActivity`, at most 200, for the roster's sub-rows.
    Activity { seq: u64, workspace: WorkspaceId },
    /// **The workspace's Bots in one lifecycle** (S14): `ListLifecycle`,
    /// the roster's Archived filter and the Trash's Bot rows. `Active` is
    /// never asked: the owner answers it `invalid_request`.
    Lifecycle {
        seq: u64,
        workspace: WorkspaceId,
        lifecycle: BotLifecycle,
    },
    /// **The workspace's Bot conversations in Trash** (`ListTrashedBotChats`,
    /// every page up to four): each with the operation that hid it and which
    /// Restore brings it back — what a conversation's Restore names, after a
    /// restart too.
    TrashedChats { seq: u64, workspace: WorkspaceId },
}

#[derive(Debug)]
pub(crate) enum Answer {
    Listed {
        seq: u64,
        workspace: WorkspaceId,
        result: Result<Vec<Bot>, Failure>,
    },
    Read {
        seq: u64,
        bot: BotId,
        result: Result<Bot, Failure>,
    },
    /// **Why the owner refused a [`Command::Write`] for its agent**: the
    /// refusal's `details.reason` (`koru_ai_wire::agent_readiness_reasons`,
    /// beside `agent_models_failed` or `runtime_unavailable`), sent just
    /// before that write's [`Answer::Wrote`] on the same channel, so whoever
    /// folds the refusal already knows why. A surface that does not care
    /// ignores it.
    Reason {
        bot: BotId,
        request: BotRequestId,
        reason: String,
    },
    /// What the owner said to a [`Command::Write`].
    Wrote {
        bot: BotId,
        request: BotRequestId,
        result: Result<BotResponse, Failure>,
    },
    /// What a [`Command::Label`] found: the Bot's label, or `None` for an
    /// ordinary chat.
    Labelled {
        chat: ChatId,
        result: Result<Option<BotChatLabel>, Failure>,
    },
    /// What a [`Command::ListChats`] answered.
    Chats {
        seq: u64,
        bot: BotId,
        result: Result<BotChatPage, Failure>,
    },
    /// What a [`Command::Activity`] answered.
    Activity {
        seq: u64,
        workspace: WorkspaceId,
        result: Result<BotActivityPage, Failure>,
    },
    /// What a [`Command::Lifecycle`] answered.
    Lifecycle {
        seq: u64,
        workspace: WorkspaceId,
        lifecycle: BotLifecycle,
        result: Result<Vec<Bot>, Failure>,
    },
    /// What a [`Command::TrashedChats`] answered.
    TrashedChats {
        seq: u64,
        workspace: WorkspaceId,
        result: Result<Listing<BotTrashedChat>, Failure>,
    },
    /// **A [`Command::List`]'s second answer** (S8): the workspace's
    /// unfinished creations (`ListBotCreations`, every page), oldest first.
    Creations {
        seq: u64,
        workspace: WorkspaceId,
        result: Result<Listing<BotCreation>, Failure>,
    },
}

/// Why the owner's answer is not what was asked for. Neither is *no Bots*:
/// the roster keeps what it showed and says the service is not answering.
///
/// **One classification, here** ([`Failure::of`]): every consumer reads a
/// lost answer the same way, so no write whose outcome is unknown can be
/// shown as not applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// **No answer says what happened**: nothing connected, the connection
    /// was lost, the service timed out waiting for its worker, a reply did
    /// not parse. A command may well have run; a read says which.
    Unanswered,
    /// The owner answered with a refusal, by its stable code: it did not do
    /// what was asked (a refusal is a transaction that did not commit).
    Refused(String),
}

/// **What the client said instead of an answer**: the stable code and
/// whether the client or the service marked it retryable
/// (`koru_ai_wire::Failure`, which this crate does not name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Said {
    pub code: String,
    pub retryable: bool,
}

impl Said {
    #[cfg(test)]
    pub(crate) fn new(code: &str, retryable: bool) -> Self {
        Self {
            code: code.to_owned(),
            retryable,
        }
    }
}

/// **Codes that leave the outcome unknown**, whatever their flag, from the
/// client (`koru_api_client`: `gateway_unavailable` for a lost connection or
/// discovery, `ai_unavailable` with no gateway session, `invalid_response`
/// for a reply that did not parse), the gateway (`ai_unavailable` with no AI
/// client or a lost task) and the AI worker (`ai_worker_timeout` when the
/// writer did not answer in time, `ai_unavailable` when it went away after
/// the command was queued, `deadline_exceeded`).
const UNANSWERED: &[&str] = &[
    "gateway_unavailable",
    "ai_unavailable",
    "ai_worker_timeout",
    "deadline_exceeded",
    "invalid_response",
];

/// The client's code for an error status whose body did not parse
/// (`koru_api_client::checked`): retryable for a 5xx, which says nothing of
/// what ran; not for a 4xx, which is a refusal.
const BODYLESS: &str = "gateway_failure";

/// **Refusals the owner marks retryable**: asking again soon may succeed,
/// so a surface waiting on a read keeps asking rather than stopping. From
/// the AI worker (`ai_worker_saturated`: its queue was full and the command
/// was never queued; `bot_failure`'s `workspace_unavailable`,
/// `storage_error` and `bot_operation_pending`; `folder_unavailable`) and
/// the Bot's execution owner (`bot_unavailable`).
const AGAIN: &[&str] = &[
    "ai_worker_saturated",
    "workspace_unavailable",
    "storage_error",
    "bot_operation_pending",
    "bot_unavailable",
    "folder_unavailable",
];

/// **The refusal of a window the Agent has not vouched for (yet)**: the AI
/// worker's `authorize` answers `forbidden` to a caller without the scope a
/// command needs. The link asks for `bots.manage` first-party; a hello with
/// no grant — the Agent refusing, or not ready to vouch, as it is for the
/// first connection after a start (`Refusal::Unverified` in its
/// `routing.log`) — is admitted with the ordinary scopes
/// (`koru_api_client::first_party_identity`), and every Bot command is then
/// answered `forbidden`. The link drops the client after any failure, so the
/// next command connects again and asks for a fresh grant.
const UNVOUCHED: &str = "forbidden";

/// **How long a read refused as unvouched is asked again** before the
/// surface stops and says the window has no access. A starting Koru vouches
/// within milliseconds (the second window run: granted 11 ms after the
/// first refusal); this is the readiness check's own bound
/// ([`super::catalog::CHECKING_LIMIT`]).
pub(crate) const UNVOUCHED_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

impl Failure {
    /// **The one classification** of what the client said: an answer that
    /// leaves the outcome unknown is [`Failure::Unanswered`]; a named
    /// refusal keeps its code.
    pub(crate) fn of(said: Said) -> Self {
        let unknown = said.code.is_empty()
            || UNANSWERED.contains(&said.code.as_str())
            || (said.code == BODYLESS && said.retryable);
        if unknown {
            Self::Unanswered
        } else {
            Self::Refused(said.code)
        }
    }

    /// Whether asking again may get another answer: an unanswered one, or
    /// a refusal the owner marks retryable. A surface waiting on a read
    /// asks again on its clock; any other refusal is final.
    pub(crate) fn again(&self) -> bool {
        match self {
            Self::Unanswered => true,
            Self::Refused(code) => AGAIN.contains(&code.as_str()),
        }
    }

    /// **Refused because this window holds no `bots.manage`** ([`UNVOUCHED`]):
    /// a refusal — a write refused so did not run — but one a *read* asks
    /// again for a bounded while ([`Vouching`]), because right after a start
    /// it is the Agent not having vouched for the window yet.
    pub(crate) fn unvouched(&self) -> bool {
        matches!(self, Self::Refused(code) if code == UNVOUCHED)
    }

    /// The failure as the real link gives it for a lost connection, for a
    /// test standing in for the link.
    #[cfg(test)]
    pub(crate) fn lost() -> Self {
        Self::of(Said::new("gateway_unavailable", true))
    }

    /// The failure as the real link gives it for a window the Agent has not
    /// vouched for yet: the AI worker's `authorize` refusing a caller with
    /// no `bots.manage` (`forbidden`, not retryable).
    #[cfg(test)]
    pub(crate) fn unvouched_refusal() -> Self {
        Self::of(Said::new("forbidden", false))
    }

    /// The failure as the real link gives it for a worker that did not
    /// answer in time.
    #[cfg(test)]
    pub(crate) fn timed_out() -> Self {
        Self::of(Said::new("ai_worker_timeout", true))
    }

    /// The failure as the real link gives it for a command **turned away
    /// for now**: the AI worker's queue was full and it was never queued
    /// (`queue_failure` in `apps/koru-ai-server/src/worker.rs`, retryable).
    #[cfg(test)]
    pub(crate) fn turned_away() -> Self {
        Self::of(Said::new("ai_worker_saturated", true))
    }
}

/// **What a surface does with a failed read**, from [`Vouching::read_failed`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reread {
    /// Asking again may answer ([`Failure::again`]): on the surface's clock.
    Again,
    /// Refused as unvouched, within [`UNVOUCHED_LIMIT`]: asked again on the
    /// clock, and said as still loading — Koru AI *is* answering.
    Unvouched,
    /// Refused as unvouched past the limit: no more asking; the surface says
    /// this window was not given access, never that Koru AI is silent.
    Denied,
    /// A refusal that would only repeat.
    Final,
}

/// **Since when a surface's reads are refused as unvouched**: the bounded
/// time it asks again for ([`UNVOUCHED_LIMIT`]). Cleared by an answer, and
/// by the surface being hidden — shown again, it has the whole while again.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Vouching {
    since: Option<std::time::Instant>,
}

impl Vouching {
    /// **A read failed at `now`**: what the surface does next. The bound
    /// runs from the first unvouched refusal and is not reset by another
    /// failure in between — a lost connection while the Agent still has not
    /// vouched is the same wait.
    pub(crate) fn read_failed(&mut self, failure: &Failure, now: std::time::Instant) -> Reread {
        if !failure.unvouched() {
            return if failure.again() {
                Reread::Again
            } else {
                Reread::Final
            };
        }
        let since = *self.since.get_or_insert(now);
        if now.saturating_duration_since(since) >= UNVOUCHED_LIMIT {
            Reread::Denied
        } else {
            Reread::Unvouched
        }
    }

    /// A read answered, or the surface was hidden: the next refusal starts
    /// a new while.
    pub(crate) fn reset(&mut self) {
        self.since = None;
    }

    /// The while began `ago`: a test standing in for the time passing.
    #[cfg(test)]
    pub(crate) fn age_for_test(&mut self, ago: std::time::Duration) {
        if let Some(since) = &mut self.since {
            *since = since.checked_sub(ago).unwrap_or(*since);
        }
    }
}

/// What the client said, as [`Said`].
fn said(code: String, retryable: bool) -> Said {
    Said { code, retryable }
}

pub(crate) struct BotsLink {
    commands: SyncSender<Command>,
    answers: Receiver<Answer>,
}

impl BotsLink {
    /// The link, with its thread dialling `root`'s gateway when the window
    /// was given `--root`. `None` under test, and when the thread cannot be
    /// spawned.
    pub(crate) fn start(root: Option<PathBuf>) -> Option<Self> {
        if cfg!(test) {
            return None;
        }
        let (commands, command_rx) = sync_channel(COMMAND_CAPACITY);
        let (answer_tx, answers) = sync_channel(ANSWER_CAPACITY);
        std::thread::Builder::new()
            .name("koru-bots-ui-client".into())
            .spawn(move || run(root, command_rx, answer_tx))
            .ok()?;
        Some(Self { commands, answers })
    }

    /// Queue `command`. `false` when the queue is full or the thread is
    /// gone; the caller keeps what it shows and may ask again later.
    pub(crate) fn send(&self, command: Command) -> bool {
        self.commands.try_send(command).is_ok()
    }

    /// Every answer that has arrived, oldest first. Never blocks.
    pub(crate) fn drain(&self) -> Vec<Answer> {
        let mut out = Vec::new();
        loop {
            match self.answers.try_recv() {
                Ok(answer) => out.push(answer),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return out,
            }
        }
    }

    /// A link with no thread behind it: the test holds the other ends.
    #[cfg(test)]
    pub(crate) fn test_pair() -> (Self, Receiver<Command>, SyncSender<Answer>) {
        let (commands, command_rx) = sync_channel(COMMAND_CAPACITY);
        let (answer_tx, answers) = sync_channel(ANSWER_CAPACITY);
        (Self { commands, answers }, command_rx, answer_tx)
    }
}

/// The thread: connect when first asked, answer each command, drop the
/// client after a failure so the next command connects again.
fn run(root: Option<PathBuf>, commands: Receiver<Command>, answers: SyncSender<Answer>) {
    let mut client: Option<koru_api_client::Client> = None;
    while let Ok(command) = commands.recv() {
        match command {
            Command::List { seq, workspace } => {
                if client.is_none() {
                    client = connect(root.as_deref());
                }
                let result = match &client {
                    // The refusal's stable code is all that is kept of it.
                    Some(connected) => listed(
                        connected
                            .ai()
                            .bots(&BotCommand::List { workspace })
                            .map_err(|failure| said(failure.code, failure.retryable)),
                    ),
                    None => Err(Failure::Unanswered),
                };
                if let Err(failure) = &result {
                    tracing::info!(
                        target: "koru::bots",
                        failure = failure_code(failure),
                        "The Bot roster could not be listed"
                    );
                    client = None;
                }
                if answers
                    .send(Answer::Listed {
                        seq,
                        workspace,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
                // **Beside every list, its unfinished creations** (S8), so a
                // creation a restart interrupted is never invisible: the
                // same request's second answer.
                if client.is_none() {
                    client = connect(root.as_deref());
                }
                let result = match &client {
                    Some(connected) => unfinished(|cursor| {
                        connected
                            .ai()
                            .bots(&BotCommand::ListBotCreations {
                                workspace,
                                cursor,
                                limit: BOT_CREATION_PAGE_LIMIT,
                            })
                            .map_err(|failure| said(failure.code, failure.retryable))
                    }),
                    None => Err(Failure::Unanswered),
                };
                if let Err(failure) = &result {
                    tracing::info!(
                        target: "koru::bots",
                        failure = failure_code(failure),
                        "A workspace's unfinished Bot creations could not be listed"
                    );
                    client = None;
                }
                if answers
                    .send(Answer::Creations {
                        seq,
                        workspace,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            Command::Read { seq, bot } => {
                if client.is_none() {
                    client = connect(root.as_deref());
                }
                let result = match &client {
                    Some(connected) => read(
                        connected
                            .ai()
                            .bots(&BotCommand::Read { bot_id: bot })
                            .map_err(|failure| said(failure.code, failure.retryable)),
                    ),
                    None => Err(Failure::Unanswered),
                };
                if let Err(failure) = &result {
                    tracing::info!(
                        target: "koru::bots",
                        failure = failure_code(failure),
                        "A Bot could not be read"
                    );
                    client = None;
                }
                if answers.send(Answer::Read { seq, bot, result }).is_err() {
                    return;
                }
            }
            Command::Write {
                bot,
                request,
                command,
            } => {
                if client.is_none() {
                    client = connect(root.as_deref());
                }
                let mut reason = None;
                let result = match &client {
                    Some(connected) => wrote(connected.ai().bots(&command).map_err(|failure| {
                        reason = refusal_reason(&failure.details);
                        said(failure.code, failure.retryable)
                    })),
                    None => Err(Failure::Unanswered),
                };
                if let (Some(reason), Err(Failure::Refused(_))) = (reason, &result)
                    && answers
                        .send(Answer::Reason {
                            bot,
                            request,
                            reason,
                        })
                        .is_err()
                {
                    return;
                }
                if let Err(failure) = &result {
                    tracing::info!(
                        target: "koru::bots",
                        failure = failure_code(failure),
                        "A Bot's settings change was not answered"
                    );
                    client = None;
                }
                if answers
                    .send(Answer::Wrote {
                        bot,
                        request,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            Command::Label { chat } => {
                if client.is_none() {
                    client = connect(root.as_deref());
                }
                let result = match &client {
                    // The transcript is read and dropped here, on this
                    // thread: only its label reaches the window.
                    Some(connected) => labelled(
                        connected
                            .ai()
                            .read_chat(chat)
                            .map(|transcript| transcript.chat.bot)
                            .map_err(|failure| said(failure.code, failure.retryable)),
                    ),
                    None => Err(Failure::Unanswered),
                };
                if let Err(failure) = &result {
                    tracing::info!(
                        target: "koru::bots",
                        failure = failure_code(failure),
                        "A conversation's Bot label could not be read"
                    );
                    // A conversation that is not found is an answer, not a
                    // broken connection.
                    if !matches!(failure, Failure::Refused(code) if code == "not_found") {
                        client = None;
                    }
                }
                if answers.send(Answer::Labelled { chat, result }).is_err() {
                    return;
                }
            }
            Command::ListChats {
                seq,
                bot,
                cursor,
                limit,
            } => {
                if client.is_none() {
                    client = connect(root.as_deref());
                }
                let result = match &client {
                    Some(connected) => chats(
                        connected
                            .ai()
                            .bots(&BotCommand::ListBotChats {
                                bot_id: bot,
                                cursor,
                                limit,
                            })
                            .map_err(|failure| said(failure.code, failure.retryable)),
                    ),
                    None => Err(Failure::Unanswered),
                };
                if let Err(failure) = &result {
                    tracing::info!(
                        target: "koru::bots",
                        failure = failure_code(failure),
                        "A Bot's conversations could not be listed"
                    );
                    client = None;
                }
                if answers.send(Answer::Chats { seq, bot, result }).is_err() {
                    return;
                }
            }
            Command::Activity { seq, workspace } => {
                if client.is_none() {
                    client = connect(root.as_deref());
                }
                let result = match &client {
                    Some(connected) => activity(
                        connected
                            .ai()
                            .bots(&BotCommand::ListActivity { workspace })
                            .map_err(|failure| said(failure.code, failure.retryable)),
                    ),
                    None => Err(Failure::Unanswered),
                };
                if let Err(failure) = &result {
                    tracing::info!(
                        target: "koru::bots",
                        failure = failure_code(failure),
                        "The Bots' activity could not be read"
                    );
                    client = None;
                }
                if answers
                    .send(Answer::Activity {
                        seq,
                        workspace,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            Command::Lifecycle {
                seq,
                workspace,
                lifecycle,
            } => {
                if client.is_none() {
                    client = connect(root.as_deref());
                }
                let result = match &client {
                    // The same reply shape as `List`: a list of Bots.
                    Some(connected) => listed(
                        connected
                            .ai()
                            .bots(&BotCommand::ListLifecycle {
                                workspace,
                                lifecycle,
                            })
                            .map_err(|failure| said(failure.code, failure.retryable)),
                    ),
                    None => Err(Failure::Unanswered),
                };
                if let Err(failure) = &result {
                    tracing::info!(
                        target: "koru::bots",
                        failure = failure_code(failure),
                        "A Bot lifecycle filter could not be listed"
                    );
                    client = None;
                }
                if answers
                    .send(Answer::Lifecycle {
                        seq,
                        workspace,
                        lifecycle,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            Command::TrashedChats { seq, workspace } => {
                if client.is_none() {
                    client = connect(root.as_deref());
                }
                let result = match &client {
                    Some(connected) => trashed_chats(|cursor| {
                        connected
                            .ai()
                            .bots(&BotCommand::ListTrashedBotChats {
                                workspace,
                                cursor,
                                limit: BOT_CHAT_PAGE_LIMIT,
                            })
                            .map_err(|failure| said(failure.code, failure.retryable))
                    }),
                    None => Err(Failure::Unanswered),
                };
                if let Err(failure) = &result {
                    tracing::info!(
                        target: "koru::bots",
                        failure = failure_code(failure),
                        "A workspace's Bot conversations in Trash could not be listed"
                    );
                    client = None;
                }
                if answers
                    .send(Answer::TrashedChats {
                        seq,
                        workspace,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

/// A `ListBotChats` reply: the page, or why not — never a page made up from
/// an answer to some other question.
pub(crate) fn chats(reply: Result<BotResponse, Said>) -> Result<BotChatPage, Failure> {
    match reply {
        Ok(BotResponse::Chats { page }) => Ok(page),
        Ok(_) => Err(Failure::Unanswered),
        Err(said) => Err(Failure::of(said)),
    }
}

/// The most pages of unfinished creations one listing reads: a person's
/// unfinished creations are a handful, and a page holds a hundred.
const CREATION_PAGES: usize = 4;

/// **What one bounded listing read**: the entries, and whether the owner
/// still had pages after the last one read — never a silent cut. A surface
/// that shows `items` says so when `more` is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listing<T> {
    pub items: Vec<T>,
    pub more: bool,
}

impl<T> Listing<T> {
    /// Everything the owner holds: nothing left unread.
    pub(crate) fn all(items: Vec<T>) -> Self {
        Self { items, more: false }
    }
}

/// **Every unfinished creation of a workspace**, oldest first, page after
/// page through `ask`, at most [`CREATION_PAGES`] of them: the list and
/// whether pages were left unread, or why not — never a list made up from
/// an answer to some other question.
pub(crate) fn unfinished(
    mut ask: impl FnMut(Option<BotCreationCursor>) -> Result<BotResponse, Said>,
) -> Result<Listing<BotCreation>, Failure> {
    let mut listing = Listing::all(Vec::new());
    let mut cursor = None;
    for page_read in 1..=CREATION_PAGES {
        match ask(cursor.take()) {
            Ok(BotResponse::Creations { page }) => {
                listing.items.extend(page.creations);
                match page.next_cursor {
                    Some(next) if page_read < CREATION_PAGES => cursor = Some(next),
                    Some(_) => listing.more = true,
                    None => break,
                }
            }
            Ok(_) => return Err(Failure::Unanswered),
            Err(said) => return Err(Failure::of(said)),
        }
    }
    Ok(listing)
}

/// **Every Bot conversation of a workspace in Trash**, page after page
/// through `ask`, at most [`CREATION_PAGES`] of them: the list and whether
/// pages were left unread, or why not.
pub(crate) fn trashed_chats(
    mut ask: impl FnMut(Option<BotTrashedChatCursor>) -> Result<BotResponse, Said>,
) -> Result<Listing<BotTrashedChat>, Failure> {
    let mut listing = Listing::all(Vec::new());
    let mut cursor = None;
    for page_read in 1..=CREATION_PAGES {
        match ask(cursor.take()) {
            Ok(BotResponse::TrashedChats { page }) => {
                listing.items.extend(page.chats);
                match page.next_cursor {
                    Some(next) if page_read < CREATION_PAGES => cursor = Some(next),
                    Some(_) => listing.more = true,
                    None => break,
                }
            }
            Ok(_) => return Err(Failure::Unanswered),
            Err(said) => return Err(Failure::of(said)),
        }
    }
    Ok(listing)
}

/// A `ListActivity` reply: the page, or why not.
pub(crate) fn activity(reply: Result<BotResponse, Said>) -> Result<BotActivityPage, Failure> {
    match reply {
        Ok(BotResponse::Activity { activity }) => Ok(activity),
        Ok(_) => Err(Failure::Unanswered),
        Err(said) => Err(Failure::of(said)),
    }
}

/// A `Label` reply: the label, `None` for an ordinary chat, or why not.
pub(crate) fn labelled(
    reply: Result<Option<BotChatLabel>, Said>,
) -> Result<Option<BotChatLabel>, Failure> {
    reply.map_err(Failure::of)
}

/// A settings change's reply, classified ([`Failure::of`]). What the reply
/// means is the settings' to read ([`super::settings`]).
pub(crate) fn wrote(reply: Result<BotResponse, Said>) -> Result<BotResponse, Failure> {
    reply.map_err(Failure::of)
}

/// A `Read` reply as a Bot tab reads it: the Bot, or why not — never a Bot
/// made up from an answer to some other question.
pub(crate) fn read(reply: Result<BotResponse, Said>) -> Result<Bot, Failure> {
    match reply {
        Ok(BotResponse::Bot { bot }) => Ok(bot),
        Ok(_) => Err(Failure::Unanswered),
        Err(said) => Err(Failure::of(said)),
    }
}

fn connect(root: Option<&std::path::Path>) -> Option<koru_api_client::Client> {
    let segment = koru_channel::segment();
    match root {
        Some(root) => koru_api_client::Client::connect_at_first_party(segment, root, SCOPES),
        None => koru_api_client::Client::connect_first_party(segment, SCOPES),
    }
    .ok()
}

/// A `List` reply as the roster reads it, the refusal reduced to its
/// stable code. Any other answer to a `List` is the service misbehaving,
/// which is not an empty roster either; nor is a refusal with no code.
pub(crate) fn listed(reply: Result<BotResponse, Said>) -> Result<Vec<Bot>, Failure> {
    match reply {
        Ok(BotResponse::List { bots }) => Ok(bots),
        Ok(_) => Err(Failure::Unanswered),
        Err(said) => Err(Failure::of(said)),
    }
}

/// **A refusal's `details.reason`**, when it carries one: the stable word
/// for why an agent cannot run (`koru_ai_wire::agent_readiness_reasons`).
pub(crate) fn refusal_reason(
    details: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Option<String> {
    details
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .filter(|reason| !reason.is_empty())
        .map(str::to_owned)
}

/// A content-free word for the log: the code, never the message.
fn failure_code(failure: &Failure) -> &str {
    match failure {
        Failure::Unanswered => "unanswered",
        Failure::Refused(code) => code,
    }
}

#[cfg(test)]
mod tests;
