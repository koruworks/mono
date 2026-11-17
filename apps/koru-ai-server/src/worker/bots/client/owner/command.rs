// SPDX-License-Identifier: MPL-2.0

//! The command role: what a linked device may cause on this computer, and
//! the receipt that makes each command happen at most once.
//!
//! # The allow-list (contract 4.3)
//!
//! | Command | The local command it runs |
//! | --- | --- |
//! | `send_turn` | Send a message into a Bot conversation. No attachments. |
//! | `retry_turn` | Retry a reply of a Bot conversation. |
//! | `cancel_attempt` | Stop a running reply. |
//! | `steer` | Send text into the reply that is running. |
//! | `create_chat` | Open another conversation of a Bot. |
//! | `pause_bot`, `resume_bot` | Pause or resume one Bot. |
//! | `pause_team`, `resume_team` | Pause or resume every Bot of this computer. |
//! | `answer_decision` | Approve or deny a pending call ("Needs you"). |
//! | `update_bot` | Name, model, reasoning, mode, Fast and face. Never the working folder or the Role page. |
//!
//! and two reads, `read_run` and `read_operation`. **Anything else is
//! answered `executing_computer_only`**: a kind this build does not know,
//! and a message that carries more than text and page references (an
//! attachment, a skill's name, a block of another conversation, a debug
//! prompt override). There is no way to
//! name a local command: each kind is a typed message whose members are
//! turned into exactly one local command here ([`apply`], [`local_bot_command`]).
//!
//! # What every command is checked for
//!
//! Before anything is recorded: it names this computer, its capability was
//! negotiated, the device is still a client with that capability's scope,
//! its kind is on the list, and its wrapper request id is the embedded
//! command's. Then, as part of applying it: what it names (a Bot, a
//! conversation, a reply, a pending call) belongs to a Bot of this person on
//! this computer, that a copy shows, and that is copied at all; a Bot whose
//! workspace is kept on this computer answers `not_found`, as an absent one
//! does. Then the local command runs with its own checks unchanged, so a
//! paused Bot, a paused team, a held session, a stale revision and a busy
//! conversation answer what a window on this computer is answered.
//!
//! # One command, at most one effect (contract 4.4)
//!
//! The command is recorded in `bot_client_receipts` as `applying`, in its
//! own transaction, before it runs, and closed with its outcome after. A
//! repeat is answered from the row and applies nothing: the stored outcome;
//! `busy` while the first arrival is still being applied; and, for a row a
//! worker that has since stopped left open, what the command's own durable
//! state says ([`settle`]) or `unknown`. A command left open is never
//! applied again.

use std::sync::Arc;

use koru_ai_api::{AiPrincipal, AiScope, AiServerPort, RequestContext};
use koru_ai_core::bots::{
    BotCommand, BotError, BotId, BotOperationStatus, BotResponse, TeamPauseStage,
};
use koru_ai_core::{
    AiPersistence, ChatId, ContextRef, InferenceAttemptId, Timestamp, ToolInvocationId, WorkspaceId,
};
use koru_ai_sqlite::{ClientOutcome, ClientReceipt, ClientReceiptBegin};
use koru_ai_wire::Failure;
use koru_bot_client_wire::{
    Answer, BUILT_CAPABILITIES, ClientCommand, ClientQuery, Command, JsonObject, Outcome, Query,
    Receipt, Refused, WireFailure, WireMessage, refusal, scope,
};
use uuid::Uuid;

use super::{Gone, Read, Served, blocking};
use crate::worker::bots::bot_failure;
use crate::worker::bots::client::feed::{command_home_copied, public_tool_activity};
use crate::worker::bots::events::SessionClose;
use crate::worker::{WorkerState, failure, not_found};

/// The code of everything outside the allow-list.
pub(crate) const EXECUTING_COMPUTER_ONLY: &str = "executing_computer_only";
/// The code of a settled command that a worker recorded and never applied.
pub(crate) const NOT_APPLIED: &str = "not_applied";
/// The code of a limit, and of a repeat that arrives while the first
/// arrival is still being applied.
pub(crate) const BUSY: &str = "busy";

/// The local answers that do not say whether the command ran: the worker
/// did not answer in time, or it went away with the command in hand. A
/// command that ends so is closed `unknown`, never `refused`.
pub(crate) const UNCERTAIN_CODES: [&str; 2] = ["ai_worker_timeout", "ai_unavailable"];

/// The kinds a linked device may send. A command of any other kind is
/// answered [`EXECUTING_COMPUTER_ONLY`].
pub(crate) const ALLOWED_KINDS: [&str; 11] = [
    "send_turn",
    "retry_turn",
    "cancel_attempt",
    "steer",
    "create_chat",
    "pause_bot",
    "resume_bot",
    "pause_team",
    "resume_team",
    "answer_decision",
    "update_bot",
];

fn executing_computer_only() -> Failure {
    failure(
        EXECUTING_COMPUTER_ONLY,
        "This change is made on the computer that runs the Bot.",
        false,
    )
}

fn busy() -> Failure {
    failure(BUSY, "This computer is busy. Try again.", true)
}

fn unavailable() -> Failure {
    failure(
        "bots_unavailable",
        "Bots are not available on this computer right now.",
        true,
    )
}

/// The caller a linked device's command runs as: the scope the one local
/// command it was turned into asks for, lent here and nowhere else. A linked
/// device never names a scope.
fn local_context(scopes: &[AiScope]) -> RequestContext {
    RequestContext {
        principal: AiPrincipal {
            label: "linked-device".into(),
            scopes: scopes.to_vec(),
        },
        trace_id: Uuid::new_v4().to_string(),
        deadline_millis: None,
    }
}

/// A Bot conversation's own commands: what a window sends them with.
const CHAT_SCOPES: [AiScope; 2] = [AiScope::Chat, AiScope::ManageBots];
/// The six Bot commands of the list.
const BOT_SCOPES: [AiScope; 1] = [AiScope::ManageBots];
/// The two reads.
const READ_SCOPES: [AiScope; 2] = [AiScope::ReadBots, AiScope::ManageBots];

/// The one local Bot command a command of the list is, or `None`. The wire
/// crate maps each kind to its local form; this is the engine's own fence
/// over that mapping, so that nothing but these six can ever be handed to
/// the Bot authority for a linked device, whatever the mapping answers.
pub(crate) fn local_bot_command(command: &ClientCommand) -> Option<BotCommand> {
    command.bot_command().filter(|local| {
        matches!(
            (command, local),
            (
                ClientCommand::CreateChat { .. },
                BotCommand::CreateChat { .. }
            ) | (ClientCommand::PauseBot { .. }, BotCommand::Pause { .. })
                | (ClientCommand::ResumeBot { .. }, BotCommand::Resume { .. })
                | (
                    ClientCommand::PauseTeam { .. },
                    BotCommand::PauseTeam { .. }
                )
                | (
                    ClientCommand::ResumeTeam { .. },
                    BotCommand::ResumeTeam { .. }
                )
                | (ClientCommand::UpdateBot { .. }, BotCommand::Update { .. })
        )
    })
}

/// The one local read a query is, or `None`.
pub(crate) fn local_read(query: &ClientQuery) -> Option<BotCommand> {
    query.bot_command().filter(|local| {
        matches!(
            (query, local),
            (ClientQuery::ReadRun { .. }, BotCommand::ReadRun { .. })
                | (
                    ClientQuery::ReadOperation { .. },
                    BotCommand::ReadOperation { .. }
                )
        )
    })
}

/// Whether a command is outside the allow-list whatever it names: a kind
/// this build does not know, and a message that carries more than text and
/// page references. A linked device's message has no attachments (refused
/// in version 1), names no skill of the agent installed on this computer,
/// quotes no block of another conversation, and never replaces Koru's own
/// prompt, which is a debugging switch of the computer that runs the Bot.
pub(crate) fn outside_allow_list(command: &ClientCommand) -> bool {
    let pages_only = |refs: &[ContextRef]| {
        refs.iter()
            .all(|reference| matches!(reference, ContextRef::Document { .. }))
    };
    !ALLOWED_KINDS.contains(&command.kind())
        || command.carries_attachments()
        || match command {
            ClientCommand::SendTurn { request } => {
                request.system_prompt_override.is_some()
                    || !request.skills.is_empty()
                    || !pages_only(&request.explicit_refs)
            }
            ClientCommand::RetryTurn { request, .. } => !pages_only(&request.explicit_refs),
            _ => false,
        }
}

/// What a command names.
#[derive(Clone, Copy, Debug)]
pub(super) enum Target {
    /// Every Bot of this computer: a team pause names no Bot.
    Team,
    Bot(BotId),
    Chat(ChatId),
    Attempt(InferenceAttemptId),
    Tool(ToolInvocationId),
}

fn target_of(command: &ClientCommand) -> Option<Target> {
    Some(match command {
        ClientCommand::SendTurn { request } => Target::Chat(request.chat_id),
        ClientCommand::RetryTurn { chat_id, .. } => Target::Chat(*chat_id),
        ClientCommand::CancelAttempt { attempt_id } | ClientCommand::Steer { attempt_id, .. } => {
            Target::Attempt(*attempt_id)
        }
        ClientCommand::AnswerDecision { tool_id, .. } => Target::Tool(*tool_id),
        ClientCommand::CreateChat { bot_id, .. }
        | ClientCommand::PauseBot { bot_id, .. }
        | ClientCommand::ResumeBot { bot_id, .. }
        | ClientCommand::UpdateBot { bot_id, .. } => Target::Bot(*bot_id),
        ClientCommand::PauseTeam { .. } | ClientCommand::ResumeTeam { .. } => Target::Team,
        ClientCommand::Unsupported => return None,
    })
}

/// On the writer: the home workspace of the Bot `target` names, when that
/// Bot is this person's on this computer and, for a conversation, a reply
/// or a pending call, when a copy shows the conversation. `None` for the
/// team. Anything else is `NotFound`: an ordinary chat of the person, a
/// hidden conversation, another computer's Bot and an unknown id read
/// alike. The caller then asks the Store, off the writer, whether that home
/// is copied.
pub(super) fn resolve(
    state: &WorkerState,
    target: Target,
) -> Result<Option<WorkspaceId>, BotError> {
    let feed = state.bot_clients.feed().ok_or(BotError::Unavailable)?;
    let repository = &state.repository;
    let of_chat = |chat: ChatId| repository.client_chat_bot(chat)?.ok_or(BotError::NotFound);
    let bot = match target {
        Target::Team => return Ok(None),
        Target::Bot(bot) => bot,
        Target::Chat(chat) => of_chat(chat)?,
        Target::Attempt(attempt) => of_chat(
            repository
                .attempt(attempt)
                .map_err(|_| BotError::Storage)?
                .and_then(|attempt| attempt.chat_id)
                .ok_or(BotError::NotFound)?,
        )?,
        Target::Tool(tool) => of_chat(
            repository
                .tool_activity(tool)
                .map_err(|_| BotError::Storage)?
                .ok_or(BotError::NotFound)?
                .chat_id,
        )?,
    };
    feed.command_home(repository, bot).map(Some)
}

/// On the writer, after the Bot is known to be one a copy shows: an
/// `update_bot` may change the name, the model, the reasoning, the mode,
/// Fast and the face, and nothing else. It is compared with the Bot at the
/// revision the command expects, so that the local command's own revision
/// check then guarantees these are still the values compared: a command at
/// another revision is answered `revision_conflict` here, as the local
/// command would answer it, before anything is compared.
fn check_update(state: &WorkerState, command: &ClientCommand) -> Result<(), Failure> {
    let ClientCommand::UpdateBot {
        bot_id,
        expected_revision,
        config,
        ..
    } = command
    else {
        return Ok(());
    };
    let (bot, _) = state
        .repository
        .bot_any_lifecycle(*bot_id)
        .map_err(bot_failure)?
        .ok_or_else(not_found)?;
    if bot.revision != *expected_revision {
        return Err(bot_failure(BotError::RevisionConflict));
    }
    if config.working_folder != bot.config.working_folder || config.role != bot.config.role {
        return Err(executing_computer_only());
    }
    Ok(())
}

/// The local commands a linked device's command or read is run as. One
/// implementation in production ([`PortCommands`]); a test may put a gate or
/// a scripted answer in front of it.
pub(crate) trait LocalCommands: Send + Sync + 'static {
    /// Run the one local command `command` is. May wait as long as a local
    /// command may: called on a blocking thread.
    fn apply(&self, command: ClientCommand) -> Result<Applied, Failure>;
    /// Run one of the two local reads.
    fn read(&self, read: BotCommand) -> Result<BotResponse, Failure>;
}

/// The local commands through the worker's own port, the one a window's
/// request goes through, with each command's own checks.
pub(crate) struct PortCommands(pub(crate) Arc<dyn AiServerPort>);

impl LocalCommands for PortCommands {
    fn apply(&self, command: ClientCommand) -> Result<Applied, Failure> {
        apply(self.0.as_ref(), command)
    }

    fn read(&self, read: BotCommand) -> Result<BotResponse, Failure> {
        // Only the two reads of the list ever reach the port.
        if !matches!(
            read,
            BotCommand::ReadRun { .. } | BotCommand::ReadOperation { .. }
        ) {
            return Err(executing_computer_only());
        }
        self.0.bots(local_context(&READ_SCOPES), read)
    }
}

/// What a local command answered.
pub(crate) struct Applied {
    /// The local command's public response JSON.
    response: Option<JsonObject>,
    /// The change is durable and waits for its execution owner.
    pending: bool,
}

impl Applied {
    fn done<T: serde::Serialize>(response: &T) -> Self {
        Self {
            response: JsonObject::from_serialize(response).ok(),
            pending: false,
        }
    }
}

/// Run the one local command `command` is, through the port a window's
/// request goes through, with that command's own checks.
pub(crate) fn apply(port: &dyn AiServerPort, command: ClientCommand) -> Result<Applied, Failure> {
    let chat = || local_context(&CHAT_SCOPES);
    Ok(match command {
        // The response stream is dropped: the reply is followed through the
        // feed and the live role, and a dropped stream cancels nothing.
        ClientCommand::SendTurn { request } => {
            Applied::done(&port.send_turn(chat(), request)?.accepted)
        }
        ClientCommand::RetryTurn { chat_id, request } => {
            Applied::done(&port.retry_turn(chat(), chat_id, request)?.accepted)
        }
        ClientCommand::CancelAttempt { attempt_id } => {
            Applied::done(&port.cancel_attempt(chat(), attempt_id)?)
        }
        ClientCommand::Steer {
            attempt_id,
            request,
        } => Applied::done(&port.steer_attempt(chat(), attempt_id, request)?),
        ClientCommand::AnswerDecision {
            tool_id,
            approve,
            decision,
        } => {
            let mut answered = if approve {
                port.approve_tool_call(chat(), tool_id, decision)?
            } else {
                port.deny_tool_call(chat(), tool_id, decision.choice)?
            };
            public_tool_activity(&mut answered.activity);
            Applied::done(&answered)
        }
        command @ (ClientCommand::CreateChat { .. }
        | ClientCommand::PauseBot { .. }
        | ClientCommand::ResumeBot { .. }
        | ClientCommand::PauseTeam { .. }
        | ClientCommand::ResumeTeam { .. }
        | ClientCommand::UpdateBot { .. }) => {
            let local = local_bot_command(&command).ok_or_else(executing_computer_only)?;
            let response = port.bots(local_context(&BOT_SCOPES), local)?;
            Applied {
                pending: match &response {
                    BotResponse::Operation { operation } => {
                        operation.status == BotOperationStatus::Pending
                    }
                    BotResponse::TeamPause { team } => team.stage == TeamPauseStage::Pending,
                    _ => false,
                },
                response: JsonObject::from_serialize(&response).ok(),
            }
        }
        ClientCommand::Unsupported => return Err(executing_computer_only()),
    })
}

/// On the writer: what became of a command that a worker which has since
/// stopped recorded and never closed. The command is not applied again; two
/// families can be read from their own durable state.
///
/// - `pause_team`, `resume_team`: the request is on the team's row
///   (`accepted` while the row waits for the delegate, else `completed`); or
///   the team is in the state the command asked for, whoever put it there
///   (`completed`: applying it now would change nothing either); otherwise
///   the row moved on or never moved, and which cannot be told (`unknown`).
/// - `pause_bot`, `resume_bot`, `create_chat`, `update_bot`: the command's
///   own receipt is written in the transaction of its effect and nothing
///   deletes one. A row is the effect (`accepted` while it waits for its
///   owner, else `completed`); no row is no effect (`refused`,
///   [`NOT_APPLIED`]).
///
/// Every other kind, and a read that fails, is `unknown`.
pub(super) fn settle(
    state: &WorkerState,
    kind: &str,
    request: Uuid,
) -> (ClientOutcome, Option<&'static str>) {
    let unknown = (ClientOutcome::Unknown, None);
    let Some(feed) = state.bot_clients.feed() else {
        return unknown;
    };
    let owner = feed.owner();
    let effect = |pending: bool| {
        (
            if pending {
                ClientOutcome::Accepted
            } else {
                ClientOutcome::Completed
            },
            None,
        )
    };
    match kind {
        "pause_team" | "resume_team" => match state.repository.team_pause(owner) {
            Ok(Some(row)) if row.request.as_uuid() == request => {
                effect(row.stage == TeamPauseStage::Pending)
            }
            Ok(row) if row.as_ref().is_some_and(|row| row.paused) == (kind == "pause_team") => {
                effect(false)
            }
            _ => unknown,
        },
        "pause_bot" | "resume_bot" | "create_chat" | "update_bot" => {
            let own = match kind {
                "pause_bot" => "pause",
                "resume_bot" => "resume",
                "create_chat" => "create_chat",
                _ => "update",
            };
            match state
                .repository
                .client_settle_bot_request(owner.user_scope, request)
            {
                Ok(Some(found)) if found.kind == own => effect(found.pending),
                Ok(None) => (ClientOutcome::Refused, Some(NOT_APPLIED)),
                _ => unknown,
            }
        }
        _ => unknown,
    }
}

/// What a command asked for, as its receipt keeps it: 64 hexadecimal
/// digits over the command's own JSON. `None` for a command that is never
/// written, which is never recorded either.
pub(crate) fn fingerprint(command: &ClientCommand) -> Option<String> {
    if matches!(command, ClientCommand::Unsupported) {
        return None;
    }
    let body = serde_json::to_vec(command).ok()?;
    Some(blake3::hash(&body).to_hex().to_string())
}

fn wire_failure(failure: Failure) -> WireFailure {
    WireFailure {
        code: failure.code,
        message: failure.message,
        retryable: failure.retryable,
    }
}

/// The failure of a receipt answered from its row. The row holds a code and
/// no message; a stored refusal is final for its request id, so it is never
/// retryable.
fn stored_failure(code: &str) -> WireFailure {
    WireFailure {
        code: code.to_owned(),
        message: if code == NOT_APPLIED {
            "The computer that runs the Bot restarted before this was applied. Nothing changed."
        } else {
            "The command was refused."
        }
        .to_owned(),
        retryable: false,
    }
}

fn outcome_of(outcome: ClientOutcome) -> Outcome {
    match outcome {
        ClientOutcome::Completed => Outcome::Completed,
        ClientOutcome::Accepted => Outcome::Accepted,
        ClientOutcome::Refused => Outcome::Refused,
        ClientOutcome::Unknown => Outcome::Unknown,
    }
}

/// A refusal that recorded nothing: the command was not looked up and
/// nothing of it ran.
fn refused(request_id: Uuid, failure: Failure) -> Receipt {
    Receipt {
        request_id,
        outcome: Outcome::Refused,
        response: None,
        failure: Some(wire_failure(failure)),
        replayed: false,
        feed_cursor: None,
    }
}

/// The answer from a closed row.
fn replayed(request_id: Uuid, stored: ClientReceipt) -> Receipt {
    Receipt {
        request_id,
        outcome: outcome_of(stored.outcome),
        response: None,
        failure: stored.failure_code.as_deref().map(stored_failure),
        replayed: true,
        feed_cursor: stored.feed_cursor,
    }
}

/// A failure code as a receipt row keeps it: a short token. A local failure
/// always carries one; anything else is recorded as a plain refusal.
fn stored_code(code: &str) -> &str {
    if !code.is_empty() && code.len() <= 64 && code.bytes().all(|byte| byte.is_ascii_graphic()) {
        code
    } else {
        "refused"
    }
}

fn scope_of(capability: Option<&str>) -> &'static str {
    capability
        .and_then(|name| {
            BUILT_CAPABILITIES
                .iter()
                .find(|capability| capability.name == name)
        })
        .and_then(|capability| capability.scope)
        // A kind this build does not know is answered, not applied: reading
        // is the least a client holds.
        .unwrap_or(scope::READ)
}

/// One command of a linked device, from its checks to its receipt.
async fn command(served: &Served, message: Command) -> Result<Receipt, Gone> {
    let shared = served.shared();
    let device = served.stream.device;
    let Command {
        request_id,
        owner_device: _,
        command,
    } = message;
    if outside_allow_list(&command) {
        return Ok(refused(request_id, executing_computer_only()));
    }
    // One request id per intent: the wrapper's and the embedded command's.
    if command
        .embedded_request_id()
        .is_some_and(|embedded| embedded != request_id)
    {
        return Ok(refused(
            request_id,
            failure(
                "invalid_request",
                "The command and its request carry different request ids.",
                false,
            ),
        ));
    }
    let Some(_inflight) = shared.admit_command(device) else {
        return Ok(refused(request_id, busy()));
    };
    let kind = command.kind();
    let Some(fingerprint) = fingerprint(&command) else {
        return Ok(refused(request_id, executing_computer_only()));
    };
    let boot_id = served.identity.boot_id.clone();
    // For the second look a stranded request gets before it is closed.
    let again = fingerprint.clone();
    let begin = shared
        .link
        .on_writer(move |state| {
            state.repository.client_receipt_begin(
                device,
                request_id,
                &fingerprint,
                kind,
                &boot_id,
                Timestamp::now().millis(),
            )
        })
        .await?;
    match begin {
        // Nothing was recorded, so nothing is applied.
        Err(error) => Ok(refused(request_id, bot_failure(error))),
        Ok(ClientReceiptBegin::Conflict) => {
            Ok(refused(request_id, bot_failure(BotError::Conflict)))
        }
        Ok(ClientReceiptBegin::Done(stored)) => Ok(replayed(request_id, stored)),
        Ok(ClientReceiptBegin::InFlight) => Ok(refused(request_id, busy())),
        Ok(ClientReceiptBegin::Stranded { kind }) => {
            let boot_id = served.identity.boot_id.clone();
            let closed = shared
                .link
                .on_writer(move |state| {
                    // Two streams may carry the same stranded request. Only
                    // the call that finds the row still open closes it, and
                    // only that one publishes: the other is answered the
                    // closed row.
                    let open = state.bot_events.enabled()
                        && matches!(
                            state.repository.client_receipt_begin(
                                device,
                                request_id,
                                &again,
                                &kind,
                                &boot_id,
                                Timestamp::now().millis(),
                            ),
                            Ok(ClientReceiptBegin::Stranded { .. })
                        );
                    let (outcome, code) = settle(state, &kind, request_id);
                    let closed = state.repository.client_receipt_finish(
                        device,
                        request_id,
                        outcome,
                        code,
                        Timestamp::now().millis(),
                    );
                    if open && let Ok(stored) = &closed {
                        state.bot_events.command(
                            device,
                            request_id,
                            &kind,
                            stored.outcome,
                            stored.failure_code.as_deref(),
                        );
                    }
                    closed
                })
                .await?;
            tracing::debug!(kind = "stranded", "a Bot client command was settled");
            Ok(match closed {
                Ok(stored) => replayed(request_id, stored),
                // The row stays open and is settled at the next arrival.
                Err(_) => refused(request_id, unavailable()),
            })
        }
        Ok(ClientReceiptBegin::Apply) => {
            let result = run(served, command).await?;
            let (outcome, code) = match &result {
                Ok(applied) if applied.pending => (ClientOutcome::Accepted, None),
                Ok(_) => (ClientOutcome::Completed, None),
                // The local command was handed over and no answer came
                // back: it may have run, or may still. That is not a
                // refusal.
                Err(failure) if UNCERTAIN_CODES.contains(&failure.code.as_str()) => (
                    ClientOutcome::Unknown,
                    Some(stored_code(&failure.code).to_owned()),
                ),
                Err(failure) => (
                    ClientOutcome::Refused,
                    Some(stored_code(&failure.code).to_owned()),
                ),
            };
            let closed = shared
                .link
                .on_writer(move |state| {
                    let closed = state.repository.client_receipt_finish(
                        device,
                        request_id,
                        outcome,
                        code.as_deref(),
                        Timestamp::now().millis(),
                    );
                    // This call wrote the row as `applying` and is the only
                    // one that closes it: once per request id. A row that
                    // could not be closed stays open and is published when
                    // it is settled.
                    if let Ok(stored) = &closed {
                        state.bot_events.command(
                            device,
                            request_id,
                            kind,
                            stored.outcome,
                            stored.failure_code.as_deref(),
                        );
                    }
                    closed
                })
                .await?;
            tracing::debug!(
                kind,
                outcome = outcome.token(),
                "a Bot client command was applied"
            );
            let (response, failure) = match result {
                Ok(applied) => (applied.response, None),
                Err(failure) => (None, Some(wire_failure(failure))),
            };
            Ok(Receipt {
                request_id,
                outcome: outcome_of(outcome),
                response,
                failure,
                replayed: false,
                // A row that could not be closed stays open; the command's
                // first answer is still the truth of what it did.
                feed_cursor: closed.ok().and_then(|stored| stored.feed_cursor),
            })
        }
    }
}

/// Apply one recorded command: what it names, then the local command.
async fn run(served: &Served, command: ClientCommand) -> Result<Result<Applied, Failure>, Gone> {
    let shared = served.shared();
    let Some(target) = target_of(&command) else {
        return Ok(Err(executing_computer_only()));
    };
    if let Err(failure) = copied(served, target).await? {
        return Ok(Err(failure));
    }
    if matches!(command, ClientCommand::UpdateBot { .. }) {
        let compared = command.clone();
        if let Err(failure) = shared
            .link
            .on_writer(move |state| check_update(state, &compared))
            .await?
        {
            return Ok(Err(failure));
        }
    }
    let local = Arc::clone(&shared.local);
    blocking(move || local.apply(command)).await
}

/// Whether `target` is a Bot of this person on this computer that is copied
/// to linked desktops: the identity on the writer, the Store on a blocking
/// thread.
async fn copied(served: &Served, target: Target) -> Result<Result<(), Failure>, Gone> {
    let shared = served.shared();
    let home = shared
        .link
        .on_writer(move |state| resolve(state, target))
        .await?;
    let home = match home {
        Ok(Some(home)) => home,
        Ok(None) => return Ok(Ok(())),
        Err(error) => return Ok(Err(bot_failure(error))),
    };
    let authority = Arc::clone(&shared.authority);
    Ok(
        blocking(move || command_home_copied(authority.as_ref(), home))
            .await?
            .map_err(bot_failure),
    )
}

/// One read of a linked device. It changes nothing and has no receipt.
async fn query(served: &Served, message: Query) -> Result<Answer, Gone> {
    let shared = served.shared();
    let Query {
        query_id,
        owner_device: _,
        query,
    } = message;
    let failed = |query_id: String, failure: Failure| Answer {
        query_id,
        response: None,
        failure: Some(wire_failure(failure)),
    };
    let Some(local) = local_read(&query) else {
        return Ok(failed(query_id, executing_computer_only()));
    };
    let (bot, chat) = match &query {
        ClientQuery::ReadRun {
            bot_id, chat_id, ..
        } => (*bot_id, Some(*chat_id)),
        ClientQuery::ReadOperation { bot_id, .. } => (*bot_id, None),
        ClientQuery::Unsupported => return Ok(failed(query_id, executing_computer_only())),
    };
    if let Err(failure) = copied(served, Target::Bot(bot)).await? {
        return Ok(failed(query_id, failure));
    }
    // A Run is read through its conversation, which must be one of that Bot
    // that a copy shows.
    if let Some(chat) = chat {
        let shown = shared
            .link
            .on_writer(move |state| state.repository.client_chat_bot(chat))
            .await?;
        if !matches!(shown, Ok(Some(of)) if of == bot) {
            return Ok(failed(query_id, not_found()));
        }
    }
    let commands = Arc::clone(&shared.local);
    let answered = blocking(move || commands.read(local)).await?;
    Ok(match answered {
        Ok(mut response) => {
            // The session of a running attempt is a handle of this computer:
            // nothing on a linked desktop can take a session over.
            if let BotResponse::Run { view } = &mut response {
                view.execution.session = None;
            }
            Answer {
                query_id,
                response: JsonObject::from_serialize(&response).ok(),
                failure: None,
            }
        }
        Err(failure) => failed(query_id, failure),
    })
}

/// Serve one command stream until it ends. One message at a time: the next
/// is not read until the one before it is answered, so a stream has one
/// command in flight.
pub(super) async fn serve(mut served: Served) {
    let idle = served.shared().timing.command_idle;
    let refusal = loop {
        let read = match tokio::time::timeout(idle, served.stream.read()).await {
            Ok(read) => read,
            // An idle command stream is closed.
            Err(_) => {
                served.stream.log.closing(SessionClose::Idle);
                break None;
            }
        };
        let message = match read {
            Read::Message(message) => message,
            Read::Closed => break None,
            Read::Malformed => break Some(Refused::new(refusal::PROTOCOL_ERROR)),
        };
        let scope = match &message {
            WireMessage::Command(command) => scope_of(command.command.capability()),
            WireMessage::Query(query) => scope_of(query.query.capability()),
            _ => scope::READ,
        };
        if let Err(refusal) = served.check(&message, scope) {
            break Some(refusal);
        }
        let answer = match message {
            WireMessage::Command(message) => {
                if let Err(refusal) = served.names_this_owner(&message.owner_device) {
                    break Some(refusal);
                }
                command(&served, message).await.map(WireMessage::Receipt)
            }
            WireMessage::Query(message) => {
                if let Err(refusal) = served.names_this_owner(&message.owner_device) {
                    break Some(refusal);
                }
                query(&served, message).await.map(WireMessage::Answer)
            }
            // `check` let only this role's two messages through.
            _ => break Some(Refused::new(refusal::PROTOCOL_ERROR)),
        };
        match answer {
            Ok(answer) => {
                if served.stream.send(answer).await.is_err() {
                    break None;
                }
            }
            // The worker is stopping. A command it recorded and did not
            // close is settled by the next start.
            Err(Gone) => break Some(Refused::new(refusal::BOTS_UNAVAILABLE)),
        }
    };
    if let Some(refusal) = refusal {
        served.stream.refuse(refusal).await;
    }
}
