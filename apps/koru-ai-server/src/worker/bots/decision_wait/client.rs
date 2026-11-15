// SPDX-License-Identifier: MPL-2.0
//! Named finite starters. The waiter never owns the physical lane permit.
use super::*;
use crate::worker::routine_call::{RoutineCall, refusal_channel};

impl AiWorkerClient {
    /// `POST /private/bots/decisions/enroll-wait-cleanup`. Answers the exact
    /// durable receipt, or a refusal; never `Missing`.
    pub fn enroll_bot_wait_cleanup(
        &self,
        context: RoutineCallContext,
        request: EnrollBotWaitCleanup,
    ) -> Result<RoutineCall<BotWaitEnrollmentReply, BotWaitRefusal>, BotWaitRefusal> {
        self.start_decision_wait(context, Request::Enroll(request))
    }
    /// `POST /private/bots/decisions/read-wait-cleanup`. `Missing` only after
    /// the delegate positively knows this exact wait and the bounded
    /// repository read found no receipt.
    pub fn read_bot_wait_cleanup(
        &self,
        context: RoutineCallContext,
        request: ReadBotWaitCleanup,
    ) -> Result<RoutineCall<BotWaitEnrollmentReply, BotWaitRefusal>, BotWaitRefusal> {
        self.start_decision_wait(context, Request::Read(request))
    }
    fn start_decision_wait(
        &self,
        context: RoutineCallContext,
        request: Request,
    ) -> Result<RoutineCall<BotWaitEnrollmentReply, BotWaitRefusal>, BotWaitRefusal> {
        context.validate().map_err(bridge_refusal)?;
        if !request.valid() {
            return Err(BotWaitRefusal::Invalid);
        }
        let permit = self
            .routine_calls
            .acquire(request.read())
            .map_err(bridge_refusal)?;
        let (reply, call) = refusal_channel(context.cancellation.clone());
        let incoming = Incoming {
            request,
            reply,
            context,
            permit,
        };
        self.sender
            .try_send(Envelope::DecisionWait(Box::new(incoming)))
            .map_err(|_| BotWaitRefusal::Unavailable)?;
        Ok(call)
    }
}
