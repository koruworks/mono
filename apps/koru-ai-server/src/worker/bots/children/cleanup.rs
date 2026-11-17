// SPDX-License-Identifier: MPL-2.0
//! The delegate's two private routes for a child wait (contract 2.3, R's
//! notes): `enroll-wait-cleanup` and `read-wait-cleanup`, both answering
//! `BotChildWaitEnrollmentReply`.
//!
//! Served on the writer: each is one ai.db transaction and asks nobody. The
//! enroll route writes the row if it is absent, from the intent the delegate
//! holds, or answers the row the ask already wrote when its digest is the
//! same (section 9 row 5): one repository function serves both writers. It
//! answers `Enrolled` for the exact key or `Refused`, never `Missing`. Only
//! the read route can answer `Missing`, after the subject is proven.
use super::*;
use crate::worker::routine_call::{
    RoutineCall, RoutineCallContext, RoutinePermit, RoutineResponder, refusal_channel,
};
use koru_delegate_api::routines::RoutineBridgeRefusal;
use koru_delegate_core::children::{
    BotChildWaitEnrollmentKey, BotChildWaitEnrollmentReply, EnrollBotChildWaitCleanup,
    ReadBotChildWaitCleanup,
};
use koru_delegate_core::decisions::{BotWaitRefusal, DecisionValidate};

#[derive(Clone)]
pub(in crate::worker) enum CleanupRequest {
    Enroll(EnrollBotChildWaitCleanup),
    Read(ReadBotChildWaitCleanup),
}
impl CleanupRequest {
    fn key(&self) -> &BotChildWaitEnrollmentKey {
        match self {
            Self::Enroll(request) => &request.key,
            Self::Read(request) => &request.key,
        }
    }
    fn read(&self) -> bool {
        matches!(self, Self::Read(_))
    }
    fn valid(&self) -> bool {
        match self {
            Self::Enroll(request) => request.validate().is_ok(),
            Self::Read(request) => request.validate().is_ok(),
        }
    }
}

pub(in crate::worker) struct CleanupIncoming {
    request: CleanupRequest,
    reply: RoutineResponder<BotChildWaitEnrollmentReply, BotWaitRefusal>,
    context: RoutineCallContext,
    // Held until the answer is sent, whatever the caller does meanwhile.
    _permit: RoutinePermit,
}

/// Repository refusals in the bridge's vocabulary. Nothing here produces
/// absence.
pub(super) fn refusal(error: BotError) -> BotWaitRefusal {
    match error {
        BotError::Invalid => BotWaitRefusal::Invalid,
        BotError::Forbidden => BotWaitRefusal::Forbidden,
        BotError::Conflict => BotWaitRefusal::Conflict,
        BotError::NotFound | BotError::RevisionConflict => BotWaitRefusal::Stale,
        _ => BotWaitRefusal::Unavailable,
    }
}
fn bridge_refusal(code: RoutineBridgeRefusal) -> BotWaitRefusal {
    match code {
        RoutineBridgeRefusal::Invalid | RoutineBridgeRefusal::ContextTooLarge => {
            BotWaitRefusal::Invalid
        }
        RoutineBridgeRefusal::Forbidden | RoutineBridgeRefusal::Unauthorized => {
            BotWaitRefusal::Forbidden
        }
        RoutineBridgeRefusal::Conflict => BotWaitRefusal::Conflict,
        RoutineBridgeRefusal::Stale => BotWaitRefusal::Stale,
        RoutineBridgeRefusal::Unavailable | RoutineBridgeRefusal::Capacity => {
            BotWaitRefusal::Unavailable
        }
    }
}

type Call = RoutineCall<BotChildWaitEnrollmentReply, BotWaitRefusal>;

impl AiWorkerClient {
    /// `POST /private/bots/children/enroll-wait-cleanup`. The exact durable
    /// receipt, or a refusal; never `Missing`.
    pub fn enroll_bot_child_wait_cleanup(
        &self,
        context: RoutineCallContext,
        request: EnrollBotChildWaitCleanup,
    ) -> Result<Call, BotWaitRefusal> {
        self.start_child_cleanup(context, CleanupRequest::Enroll(request))
    }
    /// `POST /private/bots/children/read-wait-cleanup`. `Missing` only once
    /// the subject is proven and the named origin is the Run's current
    /// execution.
    pub fn read_bot_child_wait_cleanup(
        &self,
        context: RoutineCallContext,
        request: ReadBotChildWaitCleanup,
    ) -> Result<Call, BotWaitRefusal> {
        self.start_child_cleanup(context, CleanupRequest::Read(request))
    }
    fn start_child_cleanup(
        &self,
        context: RoutineCallContext,
        request: CleanupRequest,
    ) -> Result<Call, BotWaitRefusal> {
        context.validate().map_err(bridge_refusal)?;
        if !request.valid() {
            return Err(BotWaitRefusal::Invalid);
        }
        let permit = self
            .routine_calls
            .acquire(request.read())
            .map_err(bridge_refusal)?;
        let (reply, call) = refusal_channel(context.cancellation.clone());
        self.sender
            .try_send(Envelope::Children(Incoming::Cleanup(Box::new(
                CleanupIncoming {
                    request,
                    reply,
                    context,
                    _permit: permit,
                },
            ))))
            .map_err(|_| BotWaitRefusal::Unavailable)?;
        Ok(call)
    }
}

impl WorkerState {
    pub(super) fn serve_child_cleanup(&mut self, incoming: CleanupIncoming) {
        let CleanupIncoming {
            request,
            reply,
            context,
            _permit,
        } = incoming;
        let answer = self.child_cleanup_answer(&request, &context);
        reply.send(Ok(answer));
    }

    /// The answer of one route call, from ai.db alone.
    pub(super) fn child_cleanup_answer(
        &mut self,
        request: &CleanupRequest,
        context: &RoutineCallContext,
    ) -> BotChildWaitEnrollmentReply {
        let refused = |code| BotChildWaitEnrollmentReply::Refused { code };
        if let Err(code) = context.validate() {
            return refused(bridge_refusal(code));
        }
        let (owner, _) = self.routine_recovery.owner_epoch();
        let Some(owner) = owner else {
            return refused(BotWaitRefusal::Unavailable);
        };
        let key = request.key();
        if owner.user_scope != key.owner.user_scope || owner.device != key.owner.device {
            return refused(BotWaitRefusal::Forbidden);
        }
        let answer = match request {
            CleanupRequest::Enroll(enroll) => {
                self.repository
                    .enroll_bot_child_wait(owner, enroll, Timestamp::now())
            }
            CleanupRequest::Read(read) => self.repository.read_bot_child_wait_cleanup(owner, read),
        };
        match answer {
            Ok(reply) if reply.matches_request(key, request.read()) => reply,
            // Enroll cannot turn absence into an answer; nothing else matches.
            Ok(_) => refused(BotWaitRefusal::Unavailable),
            Err(error) => refused(refusal(error)),
        }
    }
}
