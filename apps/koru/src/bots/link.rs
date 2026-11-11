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
    Bot, BotActivityPage, BotChatCursor, BotChatLabel, BotChatPage, BotCommand, BotId,
    BotRequestId, BotResponse,
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
    /// The Bots whose home is `workspace`, as the service lists them.
    List { seq: u64, workspace: WorkspaceId },
    /// One Bot, as the service holds it now: what a Bot tab's header says.
    Read { seq: u64, bot: BotId },
    /// **A change a person asked for in a Bot's settings** (Bots P1.8 S5):
    /// an `Update`, `UpdateAccess`, `Pause` or `Resume` carrying the
    /// revision it read, or the `ReadOperation` that follows one until the
    /// owner says it is effective. Answered by its request's id, so a reply
    /// can only settle the change it is about.
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
}

/// Why a read did not answer. Neither is *no Bots*: the roster keeps what it
/// showed and says the service is not answering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// Nothing answered: no gateway, a lost connection, a reply that did not
    /// parse.
    Unreachable,
    /// The service answered with a refusal, by its stable code. Kept for a
    /// diagnostic; the sentence a person reads does not depend on it.
    Refused(String),
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
                            .map_err(|failure| failure.code),
                    ),
                    None => Err(Failure::Unreachable),
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
                            .map_err(|failure| failure.code),
                    ),
                    None => Err(Failure::Unreachable),
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
                let result = match &client {
                    Some(connected) => wrote(
                        connected
                            .ai()
                            .bots(&command)
                            .map_err(|failure| failure.code),
                    ),
                    None => Err(Failure::Unreachable),
                };
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
                            .map_err(|failure| failure.code),
                    ),
                    None => Err(Failure::Unreachable),
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
                            .map_err(|failure| failure.code),
                    ),
                    None => Err(Failure::Unreachable),
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
                            .map_err(|failure| failure.code),
                    ),
                    None => Err(Failure::Unreachable),
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
        }
    }
}

/// A `ListBotChats` reply: the page, or why not — never a page made up from
/// an answer to some other question.
pub(crate) fn chats(reply: Result<BotResponse, String>) -> Result<BotChatPage, Failure> {
    match reply {
        Ok(BotResponse::Chats { page }) => Ok(page),
        Ok(_) => Err(Failure::Unreachable),
        Err(code) if code.is_empty() => Err(Failure::Unreachable),
        Err(code) => Err(Failure::Refused(code)),
    }
}

/// A `ListActivity` reply: the page, or why not.
pub(crate) fn activity(reply: Result<BotResponse, String>) -> Result<BotActivityPage, Failure> {
    match reply {
        Ok(BotResponse::Activity { activity }) => Ok(activity),
        Ok(_) => Err(Failure::Unreachable),
        Err(code) if code.is_empty() => Err(Failure::Unreachable),
        Err(code) => Err(Failure::Refused(code)),
    }
}

/// A `Label` reply: the label, `None` for an ordinary chat, or why not.
pub(crate) fn labelled(
    reply: Result<Option<BotChatLabel>, String>,
) -> Result<Option<BotChatLabel>, Failure> {
    match reply {
        Ok(label) => Ok(label),
        Err(code) if code.is_empty() => Err(Failure::Unreachable),
        Err(code) => Err(Failure::Refused(code)),
    }
}

/// A settings change's reply, the refusal reduced to its stable code. What
/// the reply means is the settings' to read ([`super::settings`]).
pub(crate) fn wrote(reply: Result<BotResponse, String>) -> Result<BotResponse, Failure> {
    match reply {
        Ok(response) => Ok(response),
        Err(code) if code.is_empty() => Err(Failure::Unreachable),
        Err(code) => Err(Failure::Refused(code)),
    }
}

/// A `Read` reply as a Bot tab reads it: the Bot, or why not — never a Bot
/// made up from an answer to some other question.
pub(crate) fn read(reply: Result<BotResponse, String>) -> Result<Bot, Failure> {
    match reply {
        Ok(BotResponse::Bot { bot }) => Ok(bot),
        Ok(_) => Err(Failure::Unreachable),
        Err(code) if code.is_empty() => Err(Failure::Unreachable),
        Err(code) => Err(Failure::Refused(code)),
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
pub(crate) fn listed(reply: Result<BotResponse, String>) -> Result<Vec<Bot>, Failure> {
    match reply {
        Ok(BotResponse::List { bots }) => Ok(bots),
        Ok(_) => Err(Failure::Unreachable),
        Err(code) if code.is_empty() => Err(Failure::Unreachable),
        Err(code) => Err(Failure::Refused(code)),
    }
}

/// A content-free word for the log: the code, never the message.
fn failure_code(failure: &Failure) -> &str {
    match failure {
        Failure::Unreachable => "unreachable",
        Failure::Refused(code) => code,
    }
}

#[cfg(test)]
mod tests;
