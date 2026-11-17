// SPDX-License-Identifier: MPL-2.0

//! The six diagnostic events of M8 (piece E): team pause, and the streams
//! and copies of this person's linked computers.
//!
//! | Event | Published |
//! | --- | --- |
//! | `bots.team_pause.changed` | when the team row moved to a new epoch, and when what runs the Bots confirmed that epoch |
//! | `bots.client.session` | when a linked device's stream was admitted, refused, or ended |
//! | `bots.client.command` | when a linked device's command was recorded with its outcome |
//! | `bots.client.reset` | when this computer told a linked device to rebuild its copy |
//! | `bots.mirror.state` | when a copy of another computer's Bots changed state, or a reset of it started or ended |
//! | `bots.mirror.removed` | when the last row of such a copy was deleted |
//!
//! **Content-free.** A row holds device ids, a request id, a flag, counts
//! and tokens of closed sets declared in `koru_events::registry`. This
//! module is the only place of this crate that builds one, and each builder
//! takes typed values: there is no parameter a Bot's name, a conversation, a
//! message, a path or a credential could be passed in. A code this worker
//! holds as text (a refusal, a command's kind, a receipt's failure code) is
//! looked up in its set; one that is not in it is published as `other`.
//!
//! **One row per durable transition, none on a replay.** Every builder is
//! called after the commit that made the transition and only when that
//! commit changed something. A copy's rows are compared before and after
//! each change of the copy tables ([`WorkerState::mirror_change`]), so a
//! page that was sent again, which is refused, publishes nothing.
//!
//! **Best-effort.** A row is handed to the sink once and never queued: the
//! team row, a receipt and a copy are the truth, and a reader reads those.
//!
//! With no sink ([`BotEvents::default`]) nothing is built and nothing extra
//! is read.

use std::sync::Arc;

use koru_ai_core::bots::{DeviceId, LinkedOwnerState, TeamPauseStage};
use koru_ai_sqlite::{ClientOutcome, MirrorOwner};
use koru_bot_client_wire::{GapReason, Role};
use koru_events::{
    Actor, BotsClientCommand, BotsClientReset, BotsClientRole, BotsClientSession,
    BotsCommandFailure, BotsCommandKind, BotsCommandOutcome, BotsMirrorReason, BotsMirrorRemoved,
    BotsMirrorRemovedReason, BotsMirrorState, BotsMirrorStateChanged, BotsResetReason,
    BotsSessionReason, BotsSessionStage, BotsTeamPauseChanged, BotsTeamPauseStage, Draft,
};
use uuid::Uuid;

use crate::worker::WorkerState;

/// Why a copy's state changed, as [`WorkerState::mirror_change`] is told.
pub(in crate::worker) use koru_events::BotsMirrorReason as CopyChange;

/// Where the worker's Bot events go: in production the events worker's
/// journal, through the adapter of the process that composes the two.
///
/// **`publish` must not block.** It is called on the AI writer, after a
/// commit, and on the tasks that serve linked devices' streams. A sink that
/// cannot take a row drops it.
pub trait BotEventSink: Send + Sync + 'static {
    fn publish(&self, event: Draft);
}

/// One row before it is given an id and a time: what a sink is handed. Named
/// here so a host that implements [`BotEventSink`] needs no other crate.
pub use koru_events::Draft as BotEventDraft;

/// The actor of a row this worker publishes.
const AI_WORKER: &str = "ai-worker";
/// The actor of a row the gateway's Bot ingress publishes for a stream it
/// turned away before this worker saw it.
const GATEWAY: &str = "gateway";

/// How a stream that never reached the AI worker ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotStreamTurnedAway {
    /// It was answered `refused` with this reason of Bot client protocol 1,
    /// or closed without a frame for it.
    Refused(&'static str),
    /// The other end closed it before it said anything.
    Closed,
    /// It said nothing in the time a hello may take.
    TimedOut,
}

/// The worker's handle on its sink. Cheap to clone; a default one publishes
/// nothing.
#[derive(Clone, Default)]
pub struct BotEvents(Option<Arc<dyn BotEventSink>>);

impl std::fmt::Debug for BotEvents {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BotEvents")
            .field("sink", &self.0.is_some())
            .finish()
    }
}

fn role_of(role: Role) -> BotsClientRole {
    match role {
        Role::Feed => BotsClientRole::Feed,
        Role::Command => BotsClientRole::Command,
        Role::Live => BotsClientRole::Live,
    }
}

fn session_reason(code: &str) -> BotsSessionReason {
    BotsSessionReason::from_token(code).unwrap_or(BotsSessionReason::Other)
}

fn mirror_state(state: LinkedOwnerState) -> BotsMirrorState {
    match state {
        LinkedOwnerState::Never => BotsMirrorState::Never,
        LinkedOwnerState::Current => BotsMirrorState::Current,
        LinkedOwnerState::Stale => BotsMirrorState::Stale,
        LinkedOwnerState::Unsupported => BotsMirrorState::Unsupported,
        LinkedOwnerState::Withdrawing => BotsMirrorState::Withdrawing,
    }
}

impl BotEvents {
    pub fn new(sink: Arc<dyn BotEventSink>) -> Self {
        Self(Some(sink))
    }

    /// Whether anything is published at all. A caller that would read
    /// something only to publish it asks first.
    pub(crate) fn enabled(&self) -> bool {
        self.0.is_some()
    }

    fn publish(&self, actor: &str, event: impl Into<Draft>) {
        if let Some(sink) = &self.0 {
            sink.publish(event.into().actor(Actor::system(actor)));
        }
    }

    /// The team row moved to `epoch` (`pending`), or `epoch` was confirmed
    /// (`acknowledged`).
    pub(crate) fn team_pause(
        &self,
        owner_device: DeviceId,
        epoch: u64,
        paused: bool,
        stage: TeamPauseStage,
    ) {
        self.publish(
            AI_WORKER,
            BotsTeamPauseChanged {
                owner_device: owner_device.as_uuid(),
                epoch,
                paused,
                stage: match stage {
                    TeamPauseStage::Pending => BotsTeamPauseStage::Pending,
                    TeamPauseStage::Acknowledged => BotsTeamPauseStage::Acknowledged,
                },
            },
        );
    }

    /// **For the gateway's Bot ingress only**: a stream of `peer` ended
    /// there, before the AI worker's owner server was handed it. `role` is
    /// the hello's, when one was read.
    pub fn stream_turned_away(&self, peer: DeviceId, role: Option<Role>, end: BotStreamTurnedAway) {
        let (stage, reason) = match end {
            BotStreamTurnedAway::Refused(code) => (BotsSessionStage::Refused, session_reason(code)),
            BotStreamTurnedAway::Closed => (BotsSessionStage::Closed, BotsSessionReason::Closed),
            BotStreamTurnedAway::TimedOut => {
                (BotsSessionStage::Refused, BotsSessionReason::Timeout)
            }
        };
        self.publish(
            GATEWAY,
            BotsClientSession {
                peer_device: peer.as_uuid(),
                stage,
                role: role.map(role_of),
                protocol_version: None,
                reason: Some(reason),
            },
        );
    }

    /// A linked device's command was recorded with its outcome: its receipt
    /// was closed by the call this follows.
    pub(crate) fn command(
        &self,
        peer: DeviceId,
        request: Uuid,
        kind: &str,
        outcome: ClientOutcome,
        failure_code: Option<&str>,
    ) {
        self.publish(
            AI_WORKER,
            BotsClientCommand {
                peer_device: peer.as_uuid(),
                request,
                kind: BotsCommandKind::from_token(kind).unwrap_or(BotsCommandKind::Other),
                outcome: match outcome {
                    ClientOutcome::Completed => BotsCommandOutcome::Completed,
                    ClientOutcome::Accepted => BotsCommandOutcome::Accepted,
                    ClientOutcome::Refused => BotsCommandOutcome::Refused,
                    ClientOutcome::Unknown => BotsCommandOutcome::Unknown,
                },
                failure: failure_code.map(|code| {
                    BotsCommandFailure::from_token(code).unwrap_or(BotsCommandFailure::Other)
                }),
            },
        );
    }

    /// This computer told `peer` to rebuild its copy: the `gap` was written.
    pub(crate) fn reset(&self, peer: DeviceId, reason: GapReason) {
        self.publish(
            AI_WORKER,
            BotsClientReset {
                peer_device: peer.as_uuid(),
                reason: match reason {
                    GapReason::FirstSubscribe => BotsResetReason::FirstSubscribe,
                    GapReason::FeedReplaced => BotsResetReason::FeedReplaced,
                    GapReason::FeedRewound => BotsResetReason::FeedRewound,
                    GapReason::ClientRequested => BotsResetReason::ClientRequested,
                },
            },
        );
    }

    /// What one change of the copy tables did to the copy of one owner,
    /// from its row before and after. `why` is the reason a change of state
    /// is published with. `began_again` says the change was a page that
    /// begins a reset from the start, which the row alone cannot show when a
    /// reset was already being received.
    fn mirror(
        &self,
        before: Option<&MirrorOwner>,
        after: &MirrorOwner,
        why: BotsMirrorReason,
        began_again: bool,
    ) {
        let say = |reason: BotsMirrorReason| {
            self.publish(
                AI_WORKER,
                BotsMirrorStateChanged {
                    owner_device: after.owner_device.as_uuid(),
                    state: mirror_state(after.state),
                    reason,
                    cursor: after.cursor,
                },
            );
        };
        if let Some(before) = before {
            // A reset is finished by the page that switches the copy to the
            // generation it was received under.
            let switched = after.generation != before.generation;
            match (before.staging.is_some(), after.staging.is_some()) {
                (false, true) => say(BotsMirrorReason::ResetStarted),
                (true, false) if switched => say(BotsMirrorReason::ResetFinished),
                (true, false) => say(BotsMirrorReason::ResetDropped),
                (true, true) if began_again => {
                    say(BotsMirrorReason::ResetDropped);
                    say(BotsMirrorReason::ResetStarted);
                }
                // A reset that one page both began and finished.
                (false, false) if switched => {
                    say(BotsMirrorReason::ResetStarted);
                    say(BotsMirrorReason::ResetFinished);
                }
                _ => {}
            }
        }
        if before.map(|row| row.state) != Some(after.state) {
            say(why);
        }
    }

    /// The last row of the copy of `owner` was deleted.
    fn mirror_removed(&self, owner: DeviceId) {
        self.publish(
            AI_WORKER,
            BotsMirrorRemoved {
                owner_device: owner.as_uuid(),
                reason: BotsMirrorRemovedReason::Unlinked,
            },
        );
    }
}

/// Which copies a change of the copy tables may touch.
#[derive(Clone, Copy, Debug)]
pub(in crate::worker) enum Copies {
    One(DeviceId),
    /// Every copy that is not being deleted.
    All,
}

impl WorkerState {
    /// Run one change of the copy tables on the writer and publish what it
    /// did to each copy it may touch: the rows are read before and after it,
    /// so a change that moved nothing publishes nothing. With no sink the
    /// change runs and nothing else is read.
    pub(in crate::worker) fn mirror_change<T>(
        &mut self,
        copies: Copies,
        why: BotsMirrorReason,
        began_again: bool,
        change: impl FnOnce(&mut Self) -> T,
    ) -> T {
        if !self.bot_events.enabled() {
            return change(self);
        }
        // A row that cannot be read is not compared: nothing is published
        // for it, and the change runs all the same.
        let before: Vec<(DeviceId, Option<MirrorOwner>)> = match copies {
            Copies::One(owner) => match self.repository.mirror_owner(owner) {
                Ok(row) => vec![(owner, row)],
                Err(_) => Vec::new(),
            },
            Copies::All => self
                .repository
                .mirror_owners()
                .unwrap_or_default()
                .into_iter()
                .map(|row| (row.owner_device, Some(row)))
                .collect(),
        };
        let result = change(self);
        for (owner, before) in before {
            if let Ok(Some(after)) = self.repository.mirror_owner(owner) {
                self.bot_events
                    .mirror(before.as_ref(), &after, why, began_again);
            }
        }
        result
    }

    /// Run one batch of a copy's deletion and publish the removal when this
    /// batch deleted the copy's last row. `deleted` answers whether the
    /// batch says nothing of the copy is left, which it also says of a copy
    /// that was never being deleted: the row is looked at on both sides.
    pub(in crate::worker) fn mirror_deletion<T>(
        &mut self,
        owner: DeviceId,
        batch: impl FnOnce(&mut Self) -> T,
        deleted: impl FnOnce(&T) -> bool,
    ) -> T {
        if !self.bot_events.enabled() {
            return batch(self);
        }
        let withdrawing = matches!(
            self.repository.mirror_owner(owner),
            Ok(Some(row)) if row.state == LinkedOwnerState::Withdrawing
        );
        let result = batch(self);
        if withdrawing
            && deleted(&result)
            && matches!(self.repository.mirror_owner(owner), Ok(None))
        {
            self.bot_events.mirror_removed(owner);
        }
        result
    }
}

/// One Bot stream of a linked device on the owner server, from the moment
/// the server was handed it to the moment it is dropped. It publishes
/// `admitted` when told, and **exactly one** of `refused` or `closed` when
/// it is dropped, wherever the stream's task ended.
pub(in crate::worker) struct SessionLog {
    events: BotEvents,
    peer: DeviceId,
    role: Role,
    version: Option<u32>,
    admitted: bool,
    end: Option<(BotsSessionStage, BotsSessionReason)>,
}

impl SessionLog {
    pub(in crate::worker) fn new(events: BotEvents, peer: DeviceId, role: Role) -> Self {
        Self {
            events,
            peer,
            role,
            version: None,
            admitted: false,
            end: None,
        }
    }

    fn say(&self, stage: BotsSessionStage, reason: Option<BotsSessionReason>) {
        self.events.publish(
            AI_WORKER,
            BotsClientSession {
                peer_device: self.peer.as_uuid(),
                stage,
                role: Some(role_of(self.role)),
                protocol_version: self.version,
                reason,
            },
        );
    }

    /// The two agreed on a protocol version.
    pub(in crate::worker) fn negotiated(&mut self, version: u32) {
        self.version = Some(version);
    }

    /// The stream's hello was answered.
    pub(in crate::worker) fn admitted(&mut self) {
        if !self.admitted {
            self.admitted = true;
            self.say(BotsSessionStage::Admitted, None);
        }
    }

    /// The stream ends with a `refused` of this reason: before its hello was
    /// answered that is the refusal of the stream, after it the way an
    /// admitted stream closed.
    pub(in crate::worker) fn refused(&mut self, reason: &str) {
        let stage = if self.admitted {
            BotsSessionStage::Closed
        } else {
            BotsSessionStage::Refused
        };
        self.end.get_or_insert((stage, session_reason(reason)));
    }

    /// The stream ends without a refusal, for this reason. The first reason
    /// given stands.
    pub(in crate::worker) fn closing(&mut self, reason: SessionClose) {
        self.end.get_or_insert((
            BotsSessionStage::Closed,
            match reason {
                SessionClose::Timeout => BotsSessionReason::Timeout,
                SessionClose::Idle => BotsSessionReason::Idle,
            },
        ));
    }
}

impl Drop for SessionLog {
    fn drop(&mut self) {
        // No reason was given: the other end closed the stream, the carrier
        // ended it, or this worker is stopping.
        let (stage, reason) = self
            .end
            .take()
            .unwrap_or((BotsSessionStage::Closed, BotsSessionReason::Closed));
        self.say(stage, Some(reason));
    }
}

/// Why a stream is closed without a refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::worker) enum SessionClose {
    /// The client left a page unacknowledged for as long as it may.
    Timeout,
    /// Nothing was asked on a command stream for as long as it may stay
    /// open.
    Idle,
}

#[cfg(any(test, feature = "fixtures"))]
pub mod recorder;

#[cfg(test)]
mod tests;
