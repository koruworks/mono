// SPDX-License-Identifier: MPL-2.0

//! Independent Routine policy; the global Bot snapshot supplies fencing only.
use super::*;
use crate::{BotPolicyBarrier, CapturedBotPolicy, NativePolicyRequestId};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutinePolicyBarrier {
    pub owner: BotOwner,
    pub bot: BotId,
    pub schedule: ScheduleId,
    pub request: NativePolicyRequestId,
    pub grant_revision: u64,
    pub policy: CapturedBotPolicy,
    pub effects_allowed: bool,
}
impl std::fmt::Debug for RoutinePolicyBarrier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutinePolicyBarrier")
            .field("bot", &self.bot)
            .field("schedule", &self.schedule)
            .field("grant_revision", &self.grant_revision)
            .finish_non_exhaustive()
    }
}
impl RoutinePolicyBarrier {
    /// Complete independent Routine policy encoding, including request and
    /// effects fencing. This does not replace the ordinary global policy hash.
    pub fn digest(&self) -> Result<RoutineDigest32, RoutineDigestError> {
        if !self.valid() {
            return Err(RoutineDigestError::Invalid);
        }
        routine_digest_json(
            b"koru.bot.routine.barrier.v1",
            &BarrierDigestV1 {
                version: 1,
                barrier: self,
            },
            65536,
        )
    }
    pub fn valid(&self) -> bool {
        RoutineSlot {
            owner: self.owner,
            bot: self.bot,
            schedule: self.schedule,
            occurrence_ms: 0,
        }
        .valid()
            && self.grant_revision > 0
            && self.grant_revision <= i64::MAX as u64
            && self.policy.valid()
    }
}

// No digest field: the complete typed barrier is the immutable hash input.
#[derive(Serialize)]
struct BarrierDigestV1<'a> {
    version: u8,
    barrier: &'a RoutinePolicyBarrier,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreshBotPolicyProof {
    pub global: BotPolicyBarrier,
    pub routine: Option<RoutinePolicyBarrier>,
}
impl std::fmt::Debug for FreshBotPolicyProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FreshBotPolicyProof([REDACTED])")
    }
}
impl FreshBotPolicyProof {
    /// Shape and subject comparison only. The repository must additionally prove
    /// latest/Effective state and the current human authority in its transaction.
    pub fn matches_submission(&self, submission: &BotSubmission) -> bool {
        let p = &submission.provenance;
        let version = if p.routine.is_some() {
            ROUTINE_BOT_SUBMISSION_VERSION
        } else {
            INTERACTIVE_BOT_SUBMISSION_VERSION
        };
        let global = &self.global;
        if !submission.valid_for_version(version)
            || global.key.owner != p.owner
            || global.key.bot != p.bot
            || !global.policy.valid()
            || global.policy.workspace != submission.workspace
            || global.config_revision < p.config_revision
            || global.config_revision > i64::MAX as u64
            || global.policy_revision < p.policy_revision
            || global.policy_revision > i64::MAX as u64
        {
            return false;
        }
        match (&p.routine, &self.routine) {
            (None, None) => true,
            (Some(admitted), Some(current)) => {
                current.valid()
                    && current.owner == p.owner
                    && current.bot == p.bot
                    && current.schedule == admitted.schedule
                    && current.policy.workspace == submission.workspace
                    && current.grant_revision >= admitted.grant_revision
            }
            _ => false,
        }
    }
    pub fn effects_allowed_for(&self, submission: &BotSubmission) -> bool {
        self.matches_submission(submission)
            && self.global.effects_allowed
            && self.routine.as_ref().is_none_or(|r| r.effects_allowed)
    }
}

#[cfg(test)]
mod tests;
