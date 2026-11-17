// SPDX-License-Identifier: MPL-2.0

//! Sending to an owner: commands, the two reads, and the reply to a message
//! as a stream.
//!
//! # One command, at most one effect, and never a kept command
//!
//! 1. The owner must be connected: its feed stream is open. Otherwise the
//!    caller is told `bot_owner_unreachable` (`not_sent`) and **nothing is
//!    recorded**. Nothing waits to be delivered later.
//! 2. A command stream is opened. If that fails, the same answer, and
//!    again nothing is recorded.
//! 3. The command's row is written (`sent`) with the exact bytes of its
//!    frame. **Only then are the bytes written to the stream.**
//! 4. The owner's receipt settles the row. A stream that ends first leaves
//!    the row `unconfirmed`, and the caller is told `not_confirmed`.
//! 5. On the next connection each unconfirmed command is written again,
//!    byte for byte ([`Shared::resend`]). The owner's durable receipt
//!    answers it without applying it twice. An answer of `unknown` is
//!    final: that request is never sent again.
//!
//! A person's repeat of a request this computer already recorded is
//! answered from its row and is not sent: only the mirror re-sends.
//!
//! # The reply to a message
//!
//! A send is answered with the public stream a local send answers with.
//! When the owner forwards the reply while it is written (the live role),
//! its events are passed on as they are. In every case the stream ends
//! with the terminal event of the reply as the copy holds it once the feed
//! has delivered it, so a reply is never shown as ended before the copy
//! says so, and a stream whose live half was lost still ends.

use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Instant;

use koru_ai_api::AiResponseStream;
use koru_ai_core::bots::{BotId, DeviceId};
use koru_ai_core::{AiError, AiErrorClass, ChatId, MessageState, Timestamp};
use koru_ai_sqlite::{ClientOutcome, MirrorCommand, MirrorCommandBegin, MirrorCommandState};
use koru_ai_wire::{
    AI_STREAM_VERSION, Failure, PublicAiEvent, SendTurnAccepted, StreamEvent, bot_linked_codes,
};
use koru_bot_client_wire::{
    Answer, ClientCommand, ClientQuery, Command, Outcome, Query, Receipt, Role, Watch, WireMessage,
    capability, encode_body,
};
use serde::de::DeserializeOwned;
use uuid::Uuid;

use super::follow::Open;
use super::{Owner, Read, Shared, Wire, copy, lock, now_ms, unreachable, unsupported};
use crate::worker::failure;

/// How often a command the owner is still applying is asked about again
/// within one connection.
const RESEND_ROUNDS: u32 = 3;

fn outcome_of(outcome: Outcome) -> ClientOutcome {
    match outcome {
        Outcome::Completed => ClientOutcome::Completed,
        Outcome::Accepted => ClientOutcome::Accepted,
        Outcome::Refused => ClientOutcome::Refused,
        Outcome::Unknown => ClientOutcome::Unknown,
    }
}

/// The Bot a command names, for its row.
fn bot_of(command: &ClientCommand) -> Option<BotId> {
    match command {
        ClientCommand::CreateChat { bot_id, .. }
        | ClientCommand::PauseBot { bot_id, .. }
        | ClientCommand::ResumeBot { bot_id, .. }
        | ClientCommand::UpdateBot { bot_id, .. } => Some(*bot_id),
        _ => None,
    }
}

/// What a repeat of a recorded request is answered with. Nothing is sent.
fn known(row: &MirrorCommand, fingerprint: &str) -> Failure {
    if row.fingerprint != fingerprint {
        return failure(
            "request_conflict",
            "This request id was used for something else.",
            false,
        );
    }
    match row.state {
        MirrorCommandState::Sent | MirrorCommandState::Unconfirmed => {
            unreachable(bot_linked_codes::DELIVERY_NOT_CONFIRMED)
        }
        MirrorCommandState::Expired => unreachable(bot_linked_codes::DELIVERY_UNKNOWN),
        MirrorCommandState::Receipted => {
            if row.outcome == Some(ClientOutcome::Unknown) {
                return unreachable(bot_linked_codes::DELIVERY_UNKNOWN);
            }
            // The owner's first answer is not kept, so it cannot be given
            // again; how it ended is.
            let mut answered = failure(
                bot_linked_codes::BOT_REQUEST_ALREADY_ANSWERED,
                "This request was already answered by the computer that runs the Bot.",
                false,
            );
            if let Some(outcome) = row.outcome {
                answered
                    .details
                    .insert("outcome".into(), outcome.token().into());
            }
            if let Some(code) = &row.failure_code {
                answered
                    .details
                    .insert("failure_code".into(), code.clone().into());
            }
            answered
        }
    }
}

/// A receipt as the answer of the local command it stands for.
pub(super) fn answer<T: DeserializeOwned>(receipt: Receipt) -> Result<T, Failure> {
    match receipt.outcome {
        Outcome::Completed | Outcome::Accepted => receipt
            .response
            .and_then(|response| response.parse::<T>().ok())
            .ok_or_else(|| unreachable(bot_linked_codes::DELIVERY_UNKNOWN)),
        Outcome::Refused => Err(match receipt.failure {
            Some(refused) => failure(&refused.code, &refused.message, refused.retryable),
            None => failure("refused", "The command was refused.", false),
        }),
        Outcome::Unknown => Err(unreachable(bot_linked_codes::DELIVERY_UNKNOWN)),
    }
}

impl Shared {
    /// The owner a command goes to, when a stream to it is open.
    fn reachable(&self, owner: DeviceId) -> Result<Arc<Owner>, Failure> {
        self.owner(owner)
            .filter(|handle| handle.connected() && self.visible(&owner))
            .ok_or_else(|| unreachable(bot_linked_codes::DELIVERY_NOT_SENT))
    }

    /// Send one command to `owner` and answer its receipt. See the module
    /// documentation for what is recorded when.
    pub(super) async fn command(
        self: &Arc<Self>,
        owner: DeviceId,
        request_id: Uuid,
        command: ClientCommand,
    ) -> Result<Receipt, Failure> {
        let handle = self.reachable(owner)?;
        let Some(needed) = command.capability() else {
            return Err(koru_ai_client::executing_computer_only());
        };
        if !handle.has(needed) {
            return Err(unsupported());
        }
        let kind = command.kind();
        let bot = bot_of(&command);
        let fingerprint = blake3::hash(
            &serde_json::to_vec(&command)
                .map_err(|_| failure("invalid_request", "The command is not valid.", false))?,
        )
        .to_hex()
        .to_string();
        let body = encode_body(&WireMessage::Command(Command {
            request_id,
            owner_device: owner,
            command,
        }))
        .ok()
        .and_then(|body| String::from_utf8(body).ok())
        .ok_or_else(|| failure("invalid_request", "The command is too large.", false))?;
        let _lane = handle.lane.lock().await;
        // A repeat of a request this computer recorded is answered from
        // its row, before anything is opened.
        let stored = self
            .link
            .stored(move |state| state.repository.mirror_command(owner, request_id))
            .await
            .ok_or_else(|| unreachable(bot_linked_codes::DELIVERY_NOT_SENT))?;
        if let Some(row) = stored {
            return Err(known(&row, &fingerprint));
        }
        // The stream first: with none, nothing is recorded.
        let (mut wire, _, negotiated) =
            self.open(owner, Role::Command)
                .await
                .map_err(|open| match open {
                    Open::Unsupported => unsupported(),
                    Open::Unreachable => unreachable(bot_linked_codes::DELIVERY_NOT_SENT),
                })?;
        if !negotiated.has(needed) {
            return Err(unsupported());
        }
        let now = now_ms();
        let (print, bytes) = (fingerprint.clone(), body.clone());
        let begun = self
            .link
            .stored(move |state| {
                state
                    .repository
                    .mirror_command_begin(owner, request_id, &print, kind, bot, &bytes, now)
            })
            .await
            .ok_or_else(|| unreachable(bot_linked_codes::DELIVERY_NOT_SENT))?;
        match begun {
            MirrorCommandBegin::Send => {}
            MirrorCommandBegin::Known(row) => return Err(known(&row, &fingerprint)),
            MirrorCommandBegin::Conflict => {
                return Err(failure(
                    "request_conflict",
                    "This request id was used for something else.",
                    false,
                ));
            }
        }
        // The row is written. Only now do the bytes leave.
        let receipt = match wire.send_body(body.as_bytes(), self.timing.hello).await {
            Ok(()) => self.receipt(&mut wire, request_id).await,
            Err(()) => None,
        };
        let Some(receipt) = receipt else {
            let now = now_ms();
            let _ = self
                .link
                .stored(move |state| {
                    state
                        .repository
                        .mirror_command_unconfirmed(owner, request_id, now)
                })
                .await;
            tracing::info!(kind, "a command to a Bot owner was not confirmed");
            return Err(unreachable(bot_linked_codes::DELIVERY_NOT_CONFIRMED));
        };
        self.record(owner, &receipt).await;
        tracing::debug!(kind, outcome = ?receipt.outcome, "a Bot owner answered a command");
        Ok(receipt)
    }

    /// Read the receipt of `request_id`, or `None` when the stream ends, a
    /// refusal arrives or nothing comes in time.
    async fn receipt(&self, wire: &mut Wire, request_id: Uuid) -> Option<Receipt> {
        let deadline = Instant::now() + self.timing.receipt;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            match wire.read(left).await {
                Read::Message(WireMessage::Receipt(receipt))
                    if receipt.request_id == request_id =>
                {
                    return Some(receipt);
                }
                Read::Message(WireMessage::Receipt(_)) => {}
                _ => return None,
            }
        }
    }

    /// Settle a command's row with the owner's answer.
    async fn record(&self, owner: DeviceId, receipt: &Receipt) {
        let request_id = receipt.request_id;
        let outcome = outcome_of(receipt.outcome);
        let code = receipt.failure.as_ref().map(|failure| failure.code.clone());
        let cursor = receipt.feed_cursor;
        let now = now_ms();
        let _ = self
            .link
            .stored(move |state| {
                state.repository.mirror_command_receipt(
                    owner,
                    request_id,
                    outcome,
                    code.as_deref(),
                    cursor,
                    now,
                )
            })
            .await;
    }

    /// Wait, for a bounded time, until the copy holds what a command did:
    /// a command is shown as done only when its receipt arrived and the
    /// copy reached the receipt's cursor.
    pub(super) async fn settled(&self, owner: DeviceId, receipt: &Receipt) {
        let (Some(cursor), Some(handle)) = (receipt.feed_cursor, self.owner(owner)) else {
            return;
        };
        let mut follow = handle.cursor.subscribe();
        let _ = tokio::time::timeout(self.timing.settle, async {
            loop {
                if *follow.borrow_and_update() >= cursor {
                    return;
                }
                if follow.changed().await.is_err() {
                    return;
                }
            }
        })
        .await;
    }

    /// Ask the owner again about every command that was sent and never
    /// answered, with the bytes that were sent. Called once per connection,
    /// when the copy is up to date.
    pub(super) async fn resend(self: Arc<Self>, owner: Arc<Owner>) {
        let device = owner.device;
        for _ in 0..RESEND_ROUNDS {
            let lane = owner.lane.lock().await;
            let now = now_ms();
            let Some(open) = self
                .link
                .stored(move |state| {
                    state.repository.mirror_commands_sweep(now)?;
                    state.repository.mirror_commands_unconfirmed(device)
                })
                .await
            else {
                return;
            };
            if open.is_empty() {
                return;
            }
            let mut still_applying = false;
            for command in open {
                let Some(body) = command.command_json else {
                    continue;
                };
                let Ok((mut wire, _, _)) = self.open(device, Role::Command).await else {
                    return;
                };
                if wire
                    .send_body(body.as_bytes(), self.timing.hello)
                    .await
                    .is_err()
                {
                    return;
                }
                let Some(receipt) = self.receipt(&mut wire, command.request_id).await else {
                    return;
                };
                // The first arrival is still being applied over there, or
                // that computer is at a limit: the row stays as it is and
                // is asked about again.
                let busy = receipt.outcome == Outcome::Refused
                    && receipt
                        .failure
                        .as_ref()
                        .is_some_and(|failure| failure.code == "busy" && failure.retryable);
                if busy {
                    still_applying = true;
                    continue;
                }
                self.record(device, &receipt).await;
                tracing::info!(
                    kind = %command.kind,
                    outcome = ?receipt.outcome,
                    "a command to a Bot owner was confirmed after a reconnect"
                );
            }
            if !still_applying {
                return;
            }
            drop(lane);
            tokio::time::sleep(self.timing.resend_pause).await;
        }
    }

    /// One of the two reads an owner answers. It changes nothing and is
    /// recorded nowhere.
    pub(super) async fn query<T: DeserializeOwned>(
        self: &Arc<Self>,
        owner: DeviceId,
        query: ClientQuery,
    ) -> Result<T, Failure> {
        let handle = self.reachable(owner)?;
        let Some(needed) = query.capability() else {
            return Err(koru_ai_client::executing_computer_only());
        };
        if !handle.has(needed) {
            return Err(unsupported());
        }
        let not_sent = || unreachable(bot_linked_codes::DELIVERY_NOT_SENT);
        let (mut wire, _, negotiated) =
            self.open(owner, Role::Command)
                .await
                .map_err(|open| match open {
                    Open::Unsupported => unsupported(),
                    Open::Unreachable => not_sent(),
                })?;
        if !negotiated.has(needed) {
            return Err(unsupported());
        }
        let query_id = Uuid::new_v4().simple().to_string();
        wire.send(
            WireMessage::Query(Query {
                query_id: query_id.clone(),
                owner_device: owner,
                query,
            }),
            self.timing.hello,
        )
        .await
        .map_err(|()| not_sent())?;
        match wire.read(self.timing.receipt).await {
            Read::Message(WireMessage::Answer(Answer {
                query_id: answered,
                response,
                failure: refused,
            })) if answered == query_id => match (response, refused) {
                (Some(response), _) => response.parse::<T>().map_err(|_| not_sent()),
                (None, Some(refused)) => {
                    Err(failure(&refused.code, &refused.message, refused.retryable))
                }
                (None, None) => Err(not_sent()),
            },
            _ => Err(not_sent()),
        }
    }

    /// Ask the owner to forward the replies of `chat` while they are
    /// written. `None` when it does not offer that or the stream cannot be
    /// opened: the reply then arrives with the feed.
    async fn watch(&self, owner: &Owner, chat: ChatId) -> Option<Wire> {
        if !owner.has(capability::LIVE_AI_STREAM) {
            return None;
        }
        let (wire, _, negotiated) = self.open(owner.device, Role::Live).await.ok()?;
        if !negotiated.has(capability::LIVE_AI_STREAM) {
            return None;
        }
        wire.send(
            WireMessage::Watch(Watch {
                owner_device: owner.device,
                chat_id: chat,
            }),
            self.timing.hello,
        )
        .await
        .ok()?;
        Some(wire)
    }

    /// Send a message or a retry into a copied conversation and answer the
    /// public stream of its reply.
    pub(super) async fn turn(
        self: &Arc<Self>,
        owner: DeviceId,
        chat: ChatId,
        request_id: Uuid,
        command: ClientCommand,
    ) -> Result<AiResponseStream, Failure> {
        let handle = self.reachable(owner)?;
        // Watched before the message is sent, so the reply's first events
        // are not missed.
        let live = self.watch(&handle, chat).await;
        let receipt = self.command(owner, request_id, command).await?;
        self.settled(owner, &receipt).await;
        let accepted: SendTurnAccepted = answer(receipt)?;
        lock(&self.index).learn(
            owner,
            copy::Learned {
                attempts: vec![(accepted.attempt_id, chat)],
                tools: Vec::new(),
            },
        );
        let (events, stream) = std::sync::mpsc::channel();
        self.runtime
            .spawn(Arc::clone(self).forward(handle, accepted.clone(), live, events));
        Ok(AiResponseStream {
            accepted,
            events: stream,
        })
    }

    /// The reply's terminal event as the copy holds it, once the feed has
    /// delivered the reply in a final state.
    async fn ended(&self, owner: DeviceId, accepted: &SendTurnAccepted) -> Option<PublicAiEvent> {
        let message = accepted.assistant_message_id;
        let record = self
            .link
            .stored(move |state| state.repository.mirror_message(owner, message))
            .await??;
        let message = copy::message(&record)?.message;
        let attempt_id = accepted.attempt_id;
        let at = message.completed_at.unwrap_or_else(Timestamp::now);
        Some(match message.state {
            MessageState::Pending => return None,
            MessageState::Complete => PublicAiEvent::Completed {
                attempt_id,
                stop_reason: None,
                at,
            },
            MessageState::Interrupted => PublicAiEvent::Cancelled { attempt_id, at },
            MessageState::Failed => PublicAiEvent::Failed {
                attempt_id,
                error: AiError::new(
                    message.error_class.unwrap_or(AiErrorClass::Provider),
                    "The reply ended with an error.",
                    false,
                ),
                at,
            },
        })
    }

    /// Feed the public stream of one reply: the owner's live events as they
    /// come, and at the end the terminal event of the reply as the copy
    /// holds it. Ends when that is sent, when nobody reads the stream any
    /// more, or when the worker stops.
    async fn forward(
        self: Arc<Self>,
        owner: Arc<Owner>,
        accepted: SendTurnAccepted,
        mut live: Option<Wire>,
        events: Sender<StreamEvent>,
    ) {
        let mut cursor = owner.cursor.subscribe();
        let mut sequence = 0u64;
        // Whether the copy should be looked at: at first, and after each
        // page and each end of the live half. The owner's own terminal
        // event is held back until the copy holds the reply in a final
        // state, so the stream never ends before the copy shows the reply.
        let mut look = true;
        let mut last: Option<StreamEvent> = None;
        loop {
            if look {
                look = false;
                if let Some(event) = self.ended(owner.device, &accepted).await {
                    let _ = events.send(last.take().unwrap_or(StreamEvent {
                        version: AI_STREAM_VERSION,
                        sequence: sequence + 1,
                        event,
                    }));
                    return;
                }
            }
            tokio::select! {
                read = async {
                    match live.as_mut() {
                        Some(wire) => wire.read(self.timing.silence).await,
                        None => std::future::pending().await,
                    }
                } => match read {
                    Read::Message(WireMessage::Live(event))
                        if event.attempt_id == accepted.attempt_id =>
                    {
                        if event.event.event.is_terminal() {
                            last = Some(event.event);
                            look = true;
                            continue;
                        }
                        sequence = sequence.max(event.event.sequence);
                        if events.send(event.event).is_err() {
                            return;
                        }
                    }
                    Read::Message(WireMessage::Live(_)) | Read::Silent => {}
                    // The watch ended or the stream closed: the feed ends
                    // the reply.
                    _ => {
                        live = None;
                        look = true;
                    }
                },
                changed = cursor.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    look = true;
                }
                () = tokio::time::sleep(self.timing.sweep) => {
                    if self.link.stopping() {
                        return;
                    }
                }
            }
        }
    }
}
