// SPDX-License-Identifier: MPL-2.0

use super::*;

impl RunsDb {
    /// Internal trusted owner entry. Current human/Store and elapsed-clock
    /// qualification stay with the authenticated runtime adapter. The binding,
    /// runtime fence and initial Control either all commit or all roll back.
    pub fn prepare_bot_decision(
        &mut self,
        binding: &DecisionBindingV1,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
        base: &DecisionPublicationBase,
    ) -> Result<DecisionRecord, BotLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            let mutation = super::super::write::prepare_on(tx, binding, proof, boot, now)?;
            commit_initial_on(tx, mutation, binding, base, now)
        })
    }

    /// `PreparePrivateDecision` (M5): the question "may this Bot read this
    /// item once". The same owner entry as [`Self::prepare_bot_decision`],
    /// after the private refusals that have a code of their own. A Routine
    /// Run, a grant both policies hold, an open effect and an owned Run each
    /// refuse before anything is written.
    pub fn prepare_private_decision(
        &mut self,
        binding: &DecisionBindingV1,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
        base: &DecisionPublicationBase,
    ) -> Result<DecisionRecord, crate::PrivateLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            let mutation = super::super::private::prepare_on(tx, binding, proof, boot, now)?;
            Ok(commit_initial_on(tx, mutation, binding, base, now)?)
        })
    }
}

/// The initial Control of a freshly prepared decision, in the transaction
/// that wrote the decision and its wait. A replay records nothing.
fn commit_initial_on(
    tx: &Transaction<'_>,
    mutation: super::super::write::DecisionMutation,
    binding: &DecisionBindingV1,
    base: &DecisionPublicationBase,
    now: i64,
) -> Result<DecisionRecord, BotLedgerError> {
    {
        {
            if mutation.outcome == crate::WriteOutcome::AlreadyApplied {
                // Exact receipt lookup precedes canonical base/configuration
                // freshness and does not allocate another transition identity.
                return Ok(mutation.record);
            }
            require(!base.predecessor.status.is_terminal())?;
            let record = mutation.record;
            let event = RunEvent::ApprovalRequired;
            let status = transition_bot(&base.predecessor.status, &event)
                .map_err(|_| BotLedgerError::Fenced)?;
            let target = RunProjectionTargetV1::Control {
                version: DecisionVersionV1,
                predecessor: base.predecessor.clone(),
                effects: DecisionProjectionEffects {
                    event: Some(event),
                    status,
                    failure_reason: None,
                    economics_at_ms: None,
                    steps: DecisionList::default(),
                    models: DecisionList::default(),
                    publications: DecisionList::default(),
                },
                fence: binding.origin(),
                disposition: Some(DecisionDisposition::Open {
                    state: record.receipt.state.clone(),
                }),
                wait_phase: record.wait.phase,
                reason: record.wait.reason,
                action: DecisionControlAction::DecisionWaiting {},
                retained_bookkeeping: base.bookkeeping.clone(),
                flush: DecisionList::default(),
                delivery: None,
            };
            insert::insert_on(
                tx,
                binding.origin().owner,
                DecisionTransitionId::mint(),
                record.wait.wait,
                base,
                &target,
                now,
            )?;
            Ok(record)
        }
    }
}

/// Positive first projection acknowledgment is the publication barrier. Its
/// immutable historical target keeps Publishing; the local next state becomes
/// answer/hold-ready only in the same transaction which records Applied.
pub(super) fn activate_on(
    tx: &Transaction<'_>,
    owner: BotOwner,
    projection: &DecisionProjectionRecord,
    now: i64,
) -> Result<(), BotLedgerError> {
    let RunProjectionTargetV1::Control {
        action: DecisionControlAction::DecisionWaiting {},
        ..
    } = &projection.target
    else {
        return Ok(());
    };
    let Some(revision) = &projection.identity.decision else {
        return Ok(());
    };
    let record = super::super::rows::read(tx, owner, projection.identity.run, revision.decision)?
        .ok_or_else(|| corrupt("bot_decisions", "initial_projection"))?;
    if record.receipt.state.phase != DecisionPhase::Publishing {
        return Ok(());
    }
    require(
        record.receipt.state.authorization == DecisionAuthorization::Pending
            && record.receipt.state.revision == revision.revision
            && record.wait.phase == BotWaitPhase::Fenced
            && record.wait.wait == projection.identity.wait,
    )?;
    let phase = if record.receipt.state.kind == DecisionKind::NativeApproval {
        "native_wait"
    } else {
        "holding"
    };
    let changed = tx
        .execute(
            "UPDATE bot_decisions SET phase=?2,revision=?3,updated_at_ms=max(updated_at_ms,?4)
        WHERE decision_id=?1 AND revision=?5 AND phase='publishing'",
            params![
                revision.decision.to_storage(),
                phase,
                next(revision.revision)?.get(),
                now,
                revision.revision.get()
            ],
        )
        .map_err(sql)?;
    require(changed == 1)
}
