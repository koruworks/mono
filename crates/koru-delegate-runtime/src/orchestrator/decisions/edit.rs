// SPDX-License-Identifier: MPL-2.0

//! Koru edit decisions: preparation from the exact live consumer and the
//! typed edit A's arm supplies, the one-use consume by the exact
//! acknowledged successor, and the read the AI worker's resume needs.

use super::*;
use koru_delegate_core::decisions::{
    DecisionBytes, DecisionEditAction, DecisionEditBinding, DecisionResumeState,
};
use koru_delegate_core::{DocumentId, PrepareNativeResult, same_native_edit_revision};
use koru_delegate_sqlite::NativeLeaseOutcome;

/// The edit a Koru decision protects, exactly as the tool call proposed it.
/// Typed input from the caller; the runtime adds every identity and policy.
#[derive(Clone, PartialEq, Eq)]
pub struct DecisionEditRequest {
    pub target: DocumentId,
    pub action: DecisionEditAction,
    pub operation_id: uuid::Uuid,
    pub edit_fingerprint: [u8; 32],
    /// The target's semantic version vector the edit was computed against.
    pub expected_revision: Vec<u8>,
    /// The bounded edit payload (content; never logged).
    pub payload: Vec<u8>,
}

impl std::fmt::Debug for DecisionEditRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DecisionEditRequest([REDACTED])")
    }
}

impl Orchestrator {
    /// Prepare a Koru edit decision for the exact live consumer's current
    /// turn. C commits the decision, its wait and the initial projection.
    ///
    /// Exact retained replay comes first (contract section 2): when the
    /// Run's open decision is this operation, the retained record is
    /// returned to its origin or to its acknowledged consumer without fresh
    /// policy, boot, live-fence or publication checks, and nothing is
    /// written. The same operation with a different edit is `bot_conflict`;
    /// another operation while a decision owns the Run is
    /// `bot_decision_owned`. Only when no decision is open does fresh
    /// preparation run.
    pub fn bot_prepare_edit_decision(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        edit: &DecisionEditRequest,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<DecisionRecord, RunError> {
        owner_device(fence.owner, device)?;
        if let Some(record) = self.retained_edit_decision(fence, edit)? {
            return Ok(record);
        }
        self.decision_boot_current(boot)?;
        self.bot_live_fence(fence, device, true, true)?;
        let run = fence.run;
        let turn = fence.turn.ok_or_else(|| refused("bot_turn_stale"))?;
        self.restore_publication_owner(run)?;
        if self.run_publication_pending(run) {
            return Err(refused("bot_decision_owned"));
        }
        let admission = self
            .bot_ledger()?
            .bot_admission(run)?
            .ok_or(RunError::UnknownRun)?;
        let submission = &admission.submission;
        let provenance = &submission.provenance;
        if provenance.owner != fence.owner || provenance.bot != fence.bot {
            return Err(refused("bot_fenced"));
        }
        // O105: no Routine setup-grant bypass through a one-use edit.
        if provenance.routine.is_some() {
            return Err(refused("bot_routine_decision_unsupported"));
        }
        let ActorRef::Person { profile } = submission.request.requested_by else {
            return Err(refused("bot_fenced"));
        };
        // Plan section 5: without the existing delegated launch a successor
        // could never be started, so no answerable Koru decision is prepared.
        if let Some(code) = self.decision_successor_unsupported(run)? {
            return Err(refused(code));
        }
        let global_policy = self.decision_global_policy(proof)?;
        let binding = DecisionBindingV1::KoruEdit(DecisionEditBinding {
            version: DecisionVersionV1,
            owner: fence.owner,
            bot: fence.bot,
            run,
            origin_attempt: fence.attempt,
            origin_generation: fence.generation,
            origin_session: fence.session,
            origin_turn: turn,
            submission: provenance.submission,
            routine: None,
            human_profile: profile,
            workspace: submission.workspace,
            target: edit.target,
            action: edit.action.clone(),
            operation_id: edit.operation_id,
            edit_fingerprint: edit.edit_fingerprint,
            expected_revision: DecisionBytes::new(edit.expected_revision.clone())
                .map_err(|_| refused("bot_invalid"))?,
            payload_version: DecisionVersionV1,
            payload: DecisionBytes::new(edit.payload.clone())
                .map_err(|_| refused("bot_invalid"))?,
            captured_policy: submission.policy.clone(),
            global_policy,
            routine_policy: None,
        });
        let base = self.decision_base(run)?;
        Ok(self
            .bot_ledger()?
            .prepare_bot_decision(&binding, proof, boot, &base)?)
    }

    /// The one-use consume, only by the exact acknowledged successor fence.
    /// `execute` is Some exactly once (first fresh commit); a replay returns
    /// the original consuming key with None and never a second license.
    pub fn bot_consume_decision_edit(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        request: &PrepareNativeResult,
        payload: &[u8],
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<NativeLeaseOutcome, RunError> {
        owner_device(fence.owner, device)?;
        self.decision_boot_current(boot)?;
        let execution = self
            .bot_ledger()?
            .decision_wait_execution(fence.owner, fence.run)?
            .ok_or_else(|| refused("bot_fenced"))?;
        let expected = execution
            .expected
            .decision
            .clone()
            .ok_or_else(|| refused("bot_fenced"))?;
        Ok(self.bot_ledger()?.consume_decision_edit(
            &expected,
            DecisionTransitionId::mint(),
            request,
            payload,
            fence,
            proof,
            boot,
        )?)
    }

    /// One read for the AI worker's `DecisionWaitDelegate`: the historical
    /// observation together with the current retained resume, if any.
    pub fn bot_read_decision_wait_and_resume(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        run: RunId,
        wait: BotWaitId,
        decision: DecisionId,
        device: DeviceId,
    ) -> Result<
        (
            Option<BotDecisionWaitObservation>,
            Option<koru_delegate_core::decisions::DecisionResume>,
        ),
        RunError,
    > {
        let observation = self.bot_read_decision_wait(owner, bot, run, wait, decision, device)?;
        let resume = self
            .bot_ledger()?
            .decision_wait_execution(owner, run)?
            .filter(|execution| execution.wait.wait == wait)
            .and_then(|execution| execution.resume);
        Ok((observation, resume))
    }

    /// The Run's open Koru edit decision for exactly this operation, read
    /// through the sole ledger decoder. History only: it grants nothing and
    /// writes nothing. `Ok(None)` only when the Run has no open wait at all
    /// (a settled decision is still replayed by C's own exact binding match).
    fn retained_edit_decision(
        &mut self,
        fence: &BotExecutionFence,
        edit: &DecisionEditRequest,
    ) -> Result<Option<DecisionRecord>, RunError> {
        let Some(execution) = self
            .bot_ledger()?
            .decision_wait_execution(fence.owner, fence.run)?
        else {
            return Ok(None);
        };
        // An open no-decision cleanup wait also owns the Run: no new question.
        let expected = execution
            .expected
            .decision
            .ok_or_else(|| refused("bot_decision_owned"))?;
        let record = self
            .bot_ledger()?
            .bot_decision(fence.owner, fence.run, expected.decision)?
            .ok_or_else(|| refused("bot_fenced"))?;
        let DecisionBindingV1::KoruEdit(binding) = &record.receipt.binding else {
            return Err(refused("bot_decision_owned"));
        };
        if binding.operation_id != edit.operation_id {
            return Err(refused("bot_decision_owned"));
        }
        if binding.bot != fence.bot || binding.run != fence.run {
            return Err(refused("bot_fenced"));
        }
        let origin = binding.origin_attempt == fence.attempt
            && binding.origin_generation == fence.generation
            && binding.origin_session == fence.session
            && Some(binding.origin_turn) == fence.turn;
        let consumer = record.resume.as_ref().is_some_and(|resume| {
            let (attempt, generation, session, _) = resume.consumer_identity();
            let turn = match &resume.state {
                DecisionResumeState::InputAcknowledged { acknowledgment }
                | DecisionResumeState::Closed {
                    acknowledgment: Some(acknowledgment),
                } => Some(acknowledgment.turn),
                _ => None,
            };
            attempt == fence.attempt
                && generation == fence.generation
                && session == Some(fence.session)
                && turn.is_some()
                && turn == fence.turn
        });
        if !origin && !consumer {
            return Err(refused("bot_fenced"));
        }
        let same_revision = same_native_edit_revision(
            &edit.expected_revision,
            binding.expected_revision.as_slice(),
        )
        .unwrap_or(false);
        if binding.action != edit.action
            || !same_revision
            || !record.receipt.binding.matches_edit_except_revision(
                edit.operation_id,
                edit.target,
                edit.edit_fingerprint,
                &edit.payload,
            )
        {
            return Err(refused("bot_conflict"));
        }
        Ok(Some(record))
    }

    pub(super) fn decision_global_policy(
        &mut self,
        proof: &FreshBotPolicyProof,
    ) -> Result<DecisionGlobalPolicy, RunError> {
        let retained = self
            .native_ledger()?
            .native_policy(&proof.global.key)?
            .ok_or_else(|| refused("bot_policy_unavailable"))?;
        Ok(DecisionGlobalPolicy {
            key: proof.global.key.clone(),
            config_revision: DecisionRevision::new(proof.global.config_revision)
                .map_err(|_| refused("bot_policy_unavailable"))?,
            policy_revision: DecisionRevision::new(proof.global.policy_revision)
                .map_err(|_| refused("bot_policy_unavailable"))?,
            digest: DecisionDigest::from_bytes(retained.payload_digest),
        })
    }
}

#[cfg(test)]
mod tests;
