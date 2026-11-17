// SPDX-License-Identifier: MPL-2.0

//! The host's decision enrollment client: exactly one finite HTTP request to
//! the AI worker per call, under the decision credential domain only. It is
//! called solely from `Orchestrator::bot_enroll_decision`, which refuses while
//! the fixed publication worker holds the Run's document handle. Only an exact,
//! correlated `Enrolled` reply is returned; a refusal is typed, and every
//! failure after sending is unknown completion: never Missing, never a retry.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use koru_api_client::decision_bridge::{DecisionBridgeClient, DecisionBridgeTransportError};
use koru_delegate_api::bots::BotDelegateAuthority;
use koru_delegate_api::decision_bridge::{DECISION_BRIDGE_DEADLINE, DecisionBridgeEnvelope};
use koru_delegate_core::decisions::{BotWaitEnrollmentReply, BotWaitRefusal, EnrollBotWaitCleanup};
use koru_delegate_runtime::{DecisionEnrollmentBridge, Port, PortError, PortResult};

pub(crate) struct HostDecisionBridge {
    client: Option<DecisionBridgeClient>,
    stopped: Arc<AtomicBool>,
}

fn error(code: &'static str) -> PortError {
    PortError::new(Port::Sessions, code, "Decision enrollment is unavailable")
}
fn refused(code: BotWaitRefusal) -> PortError {
    error(match code {
        BotWaitRefusal::Invalid => "decision_enrollment_invalid",
        BotWaitRefusal::Forbidden => "decision_enrollment_forbidden",
        BotWaitRefusal::Conflict => "decision_enrollment_conflict",
        BotWaitRefusal::Stale => "decision_enrollment_stale",
        BotWaitRefusal::Unavailable => "decision_enrollment_unavailable",
    })
}

impl HostDecisionBridge {
    /// Keep the host composable without the optional HTTP capability; never
    /// fall back to an ordinary bearer or the Routine credential.
    pub(crate) fn new(
        gateway: &koru_api_client::Client,
        authority: Option<&BotDelegateAuthority>,
        stopped: Arc<AtomicBool>,
    ) -> Self {
        let client =
            authority.and_then(|a| gateway.decision_bridge(a.decision_bridge_authority()).ok());
        Self { client, stopped }
    }

    #[cfg(test)]
    pub(crate) fn with_client(client: Option<DecisionBridgeClient>) -> Self {
        Self {
            client,
            stopped: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl DecisionEnrollmentBridge for HostDecisionBridge {
    fn enroll(&mut self, request: &EnrollBotWaitCleanup) -> PortResult<BotWaitEnrollmentReply> {
        let deadline = Instant::now()
            .checked_add(DECISION_BRIDGE_DEADLINE)
            .ok_or_else(|| error("decision_enrollment_not_sent"))?;
        if self.stopped.load(Ordering::Acquire) {
            return Err(error("decision_enrollment_not_sent"));
        }
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| error("decision_bridge_unavailable"))?;
        let envelope = DecisionBridgeEnvelope::new(request.clone())
            .map_err(|_| error("decision_enrollment_invalid_request"))?;
        let answer = client
            .enroll_until(&envelope, deadline)
            .map_err(|failure| match failure {
                DecisionBridgeTransportError::NotSent(_) => error("decision_enrollment_not_sent"),
                DecisionBridgeTransportError::Unknown(_) => error("decision_enrollment_unknown"),
            });
        // A physical response cannot authorize anything after Stop or past the
        // one shared deadline; preserve what was sent as unknown.
        if self.stopped.load(Ordering::Acquire) || Instant::now() >= deadline {
            return match answer {
                Err(failure) if failure.code == "decision_enrollment_not_sent" => Err(failure),
                _ => Err(error("decision_enrollment_unknown")),
            };
        }
        match answer?.payload {
            reply @ BotWaitEnrollmentReply::Enrolled { .. }
                if reply.matches_request(&request.key, false) =>
            {
                Ok(reply)
            }
            BotWaitEnrollmentReply::Refused { code } => Err(refused(code)),
            // The client already refused a Missing reply to Enroll; anything
            // else uncorrelated is unknown completion, not absence.
            _ => Err(error("decision_enrollment_unknown")),
        }
    }
}

/// The child wait's enrollment (M6): the same finite call on the child
/// route, with the child payload. Only an exact, correlated `Enrolled` reply
/// is returned; a refusal is typed, and every failure after sending is
/// unknown completion.
impl koru_delegate_runtime::ChildEnrollmentBridge for HostDecisionBridge {
    fn enroll_child(
        &mut self,
        request: &koru_delegate_core::children::EnrollBotChildWaitCleanup,
    ) -> PortResult<koru_delegate_core::children::BotChildWaitEnrollmentReply> {
        use koru_delegate_core::children::BotChildWaitEnrollmentReply as Reply;
        let deadline = Instant::now()
            .checked_add(DECISION_BRIDGE_DEADLINE)
            .ok_or_else(|| error("decision_enrollment_not_sent"))?;
        if self.stopped.load(Ordering::Acquire) {
            return Err(error("decision_enrollment_not_sent"));
        }
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| error("decision_bridge_unavailable"))?;
        let envelope = DecisionBridgeEnvelope::new(request.clone())
            .map_err(|_| error("decision_enrollment_invalid_request"))?;
        let answer =
            client
                .enroll_child_until(&envelope, deadline)
                .map_err(|failure| match failure {
                    DecisionBridgeTransportError::NotSent(_) => {
                        error("decision_enrollment_not_sent")
                    }
                    DecisionBridgeTransportError::Unknown(_) => {
                        error("decision_enrollment_unknown")
                    }
                });
        if self.stopped.load(Ordering::Acquire) || Instant::now() >= deadline {
            return match answer {
                Err(failure) if failure.code == "decision_enrollment_not_sent" => Err(failure),
                _ => Err(error("decision_enrollment_unknown")),
            };
        }
        match answer?.payload {
            reply @ Reply::Enrolled { .. } if reply.matches_request(&request.key, false) => {
                Ok(reply)
            }
            Reply::Refused { code } => Err(refused(code)),
            _ => Err(error("decision_enrollment_unknown")),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
