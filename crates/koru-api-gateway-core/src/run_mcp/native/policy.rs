// SPDX-License-Identifier: MPL-2.0

//! Composite identity and fresh synchronization, separate from retained replay.
use super::*;
use koru_delegate_core::*;

pub(super) fn matches(
    proof: &FreshBotPolicyProof,
    provenance: &BotProvenance,
    captured: &CapturedBotPolicy,
    workspace: WorkspaceId,
) -> bool {
    let global = &proof.global;
    if !captured.valid()
        || captured.workspace != workspace
        || global.key.owner != provenance.owner
        || global.key.bot != provenance.bot
        || !global.policy.valid()
        || global.policy.workspace != workspace
        || !(provenance.config_revision..=i64::MAX as u64).contains(&global.config_revision)
        || !(provenance.policy_revision..=i64::MAX as u64).contains(&global.policy_revision)
        || provenance.config_revision == 0
        || provenance.policy_revision == 0
    {
        return false;
    }
    match (provenance.routine, &proof.routine) {
        (None, None) => true,
        (Some(subject), Some(routine)) => {
            routine.valid()
                && subject.grant_revision > 0
                && routine.owner == provenance.owner
                && routine.bot == provenance.bot
                && routine.schedule == subject.schedule
                && routine.policy.workspace == workspace
                && routine.grant_revision >= subject.grant_revision
        }
        _ => false,
    }
}

/// Call only after retained receipt recovery. This supplies no human authority
/// and cannot bypass the runtime's still-closed Routine execution gates.
pub(super) fn synchronize(
    proof: &FreshBotPolicyProof,
    mut call: impl FnMut(BotCommand) -> Result<BotReply, ToolFailure>,
) -> Result<(), ToolFailure> {
    if !proof.global.effects_allowed || proof.routine.as_ref().is_some_and(|r| !r.effects_allowed) {
        return Err(dispatch::failure("native_policy_pending"));
    }
    match call(BotCommand::SynchronizePolicy {
        barrier: proof.global.clone(),
    })? {
        BotReply::PolicyBarrier { receipt }
            if receipt.barrier == proof.global
                && receipt.state == BotPolicyBarrierState::Effective => {}
        _ => return Err(dispatch::failure("native_policy_pending")),
    }
    if let Some(barrier) = &proof.routine {
        match call(BotCommand::SynchronizeRoutinePolicy {
            barrier: barrier.clone(),
        })? {
            BotReply::RoutinePolicyBarrier {
                barrier: found,
                state,
            } if found == *barrier && state == BotPolicyBarrierState::Effective => {}
            _ => return Err(dispatch::failure("native_policy_pending")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
