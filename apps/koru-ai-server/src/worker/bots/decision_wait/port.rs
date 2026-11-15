// SPDX-License-Identifier: MPL-2.0
//! The delegate reads the AI decision owner needs, as a narrow port.
//!
//! The real implementation is the delegate API author's: IPC16
//! `BotCommand::ReadBotDecisionWait` answered by `BotReply::DecisionWait`
//! (contract section 6), plus the retained exact resume row. Until it exists
//! production composition installs no port, and every decision job refuses
//! `Unavailable`. There is deliberately no default implementation.
use koru_delegate_core::decisions::{
    BotDecisionWaitObservation, BotWaitEnrollmentKey, BotWaitId, BotWaitRefusal, DecisionId,
    DecisionResume,
};
use std::time::Duration;

/// Exactly what `ReadBotDecisionWait { owner, bot, run, wait_id, decision_id }`
/// names. Identity only; it authenticates nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecisionWaitSubject {
    pub owner: koru_delegate_core::BotOwner,
    pub bot: koru_delegate_core::BotId,
    pub run: koru_delegate_core::RunId,
    pub wait_id: BotWaitId,
    pub decision_id: DecisionId,
}
impl DecisionWaitSubject {
    pub fn of(key: &BotWaitEnrollmentKey) -> Self {
        Self {
            owner: key.owner,
            bot: key.bot,
            run: key.run,
            wait_id: key.wait_id,
            decision_id: key.decision_id,
        }
    }
}

/// A failed read is never absence. `Unavailable` covers timeout, transport
/// loss, a closed peer and any answer that could not be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionWaitPortError {
    Unavailable,
    Refused(BotWaitRefusal),
}

/// Both reads share one caller deadline: `within` is what remains of it, and
/// an implementation must not block past it. `Ok(None)` is an authenticated
/// answer that the delegate retains no such wait (or no resume row).
pub trait DecisionWaitDelegate: Send + Sync {
    fn read_decision_wait(
        &self,
        subject: &DecisionWaitSubject,
        within: Duration,
    ) -> Result<Option<BotDecisionWaitObservation>, DecisionWaitPortError>;
    /// The retained exact resume row of this decision's consumer, used only
    /// to qualify the AI replay rollover after acknowledged input.
    fn read_decision_resume(
        &self,
        subject: &DecisionWaitSubject,
        within: Duration,
    ) -> Result<Option<DecisionResume>, DecisionWaitPortError>;
}
