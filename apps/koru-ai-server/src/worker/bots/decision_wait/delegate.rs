// SPDX-License-Identifier: MPL-2.0
//! The real `DecisionWaitDelegate`: one IPC16 `ReadBotDecisionWait` through
//! the private Bot delegate port serves both reads. `Ok(None)` only for the
//! delegate's authenticated `Missing`; a typed delegate refusal stays a
//! refusal; every transport, timeout, late, malformed or foreign answer is
//! `Unavailable`, never absence. The caller's remaining budget is honoured.
use super::port::{DecisionWaitDelegate, DecisionWaitPortError, DecisionWaitSubject};
use koru_delegate_api::bots::BotDelegatePort;
use koru_delegate_core::decisions::{BotDecisionWaitObservation, BotWaitRefusal, DecisionResume};
use std::sync::Arc;
use std::time::Duration;

pub struct BotDelegateDecisionWait {
    delegate: Arc<dyn BotDelegatePort>,
}

impl BotDelegateDecisionWait {
    pub fn new(delegate: Arc<dyn BotDelegatePort>) -> Self {
        Self { delegate }
    }

    #[allow(clippy::type_complexity)]
    fn read(
        &self,
        subject: &DecisionWaitSubject,
        within: Duration,
    ) -> Result<Option<(BotDecisionWaitObservation, Option<DecisionResume>)>, DecisionWaitPortError>
    {
        if within.is_zero() {
            return Err(DecisionWaitPortError::Unavailable);
        }
        self.delegate
            .read_bot_decision_wait(
                subject.owner,
                subject.bot,
                subject.run,
                subject.wait_id,
                subject.decision_id,
                within,
            )
            .map_err(|failure| refusal(&failure))
    }
}

/// A non-retryable delegate refusal of this exact subject keeps its meaning;
/// anything retryable or unrecognized is unavailable.
fn refusal(failure: &koru_delegate_api::Failure) -> DecisionWaitPortError {
    if failure.retryable {
        return DecisionWaitPortError::Unavailable;
    }
    match failure.code.as_str() {
        "bot_fenced" | "bot_owner_mismatch" => {
            DecisionWaitPortError::Refused(BotWaitRefusal::Forbidden)
        }
        "bot_stale" => DecisionWaitPortError::Refused(BotWaitRefusal::Stale),
        "bot_conflict" => DecisionWaitPortError::Refused(BotWaitRefusal::Conflict),
        "bot_invalid" => DecisionWaitPortError::Refused(BotWaitRefusal::Invalid),
        _ => DecisionWaitPortError::Unavailable,
    }
}

impl DecisionWaitDelegate for BotDelegateDecisionWait {
    fn read_decision_wait(
        &self,
        subject: &DecisionWaitSubject,
        within: Duration,
    ) -> Result<Option<BotDecisionWaitObservation>, DecisionWaitPortError> {
        Ok(self
            .read(subject, within)?
            .map(|(observation, _)| observation))
    }

    fn read_decision_resume(
        &self,
        subject: &DecisionWaitSubject,
        within: Duration,
    ) -> Result<Option<DecisionResume>, DecisionWaitPortError> {
        Ok(self.read(subject, within)?.and_then(|(_, resume)| resume))
    }
}

#[cfg(test)]
mod tests;
