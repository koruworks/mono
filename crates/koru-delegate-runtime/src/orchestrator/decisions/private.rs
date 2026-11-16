// SPDX-License-Identifier: MPL-2.0

//! Private operations (M5, delegate IPC 18): the runtime half of the six
//! private Bot commands.
//!
//! The ledger owns every transition. These methods authenticate the caller's
//! device and the Run, qualify the live runtime evidence a fresh transition
//! needs (boot, the exact live consumer, the attached document base), and
//! then run the two things the ledger cannot: the decision settle hook and
//! the content-free settled event.
//!
//! **Resolution first.** Every command that carries a dispatcher id begins by
//! closing the rows another dispatcher instance left open for the same owner
//! (`prepared` to `refused/not_dispatched`, `dispatched` to `unknown`), in a
//! transaction of its own, before any check of the command itself. It stays
//! committed when the command is then refused.
//!
//! **Who may declare an in-flight read `unknown`.** The row's own dispatcher,
//! by `AdvancePrivateEffect`; or resolution, on behalf of a command that
//! carries another dispatcher id for the same owner. Nothing here adds a
//! third: there is no timer, no `Hello` path, no Stop, no deadline and no
//! cleanup path that touches an effect row.
//!
//! **The settle hook** (O150 extended by D16). Whenever a row that belongs to
//! a consumed decision reaches a final stage, whether by its dispatcher's
//! advance or by resolution, the decision settles at once on `confirmed` and
//! on `refused`, so the decision-wait fence (O147) lifts mid-turn. `unknown`
//! settles only at cleanup. A lost settlement is retaken by the per-turn
//! resume driver, which runs the same hook.

use super::*;
use crate::{PrivateEffectLedger, PrivateOperationSettled};
use koru_delegate_core::decisions::{
    DecisionExpected, DecisionPrivateBinding, DecisionResumeState,
};
use koru_delegate_core::{
    PrivateDispatcherId, PrivateEffectKey, PrivateEffectTransition, PrivateEffectView,
    PrivateOperationProposal,
};
use koru_delegate_sqlite::{PrivateEffectRecord, PrivateLedgerError};

/// Most settled-event ids remembered as published by this process. Past it
/// the memory is dropped; a repeat then publishes the same id again and the
/// journal keeps one row.
const SETTLED_REMEMBERED: usize = 1024;

/// A private refusal as its wire code. The codes of its own are the new
/// IPC 18 ones; everything else keeps the existing ledger mapping.
fn private_refused(error: PrivateLedgerError) -> RunError {
    match error {
        PrivateLedgerError::Ledger(error) => error.into(),
        other => refused(other.code().unwrap_or("bot_fenced")),
    }
}

fn v4(id: &uuid::Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
}

impl Orchestrator {
    /// The private-operation half of the ledger, or `bot_private_unavailable`
    /// when this build's store does not compose it.
    pub(in crate::orchestrator) fn private_ledger(
        &mut self,
    ) -> Result<&mut dyn PrivateEffectLedger, RunError> {
        self.ports
            .store
            .private_effects()
            .ok_or_else(|| refused("bot_private_unavailable"))
    }

    /// One retained row was observed: if it is final, settle the decision it
    /// belongs to and publish its one settled event. Failures are traced,
    /// never raised: the command that observed the row answered truthfully,
    /// the resume driver retakes the settlement, and a repeat republishes the
    /// same event id.
    fn private_effect_observed(&mut self, record: &PrivateEffectRecord) {
        if !record.stage.is_final() {
            return;
        }
        let run = record.key.run;
        if record.decision.is_some()
            && let Err(error) = self.settle_saved_decision_effect(record.owner, run)
        {
            self.trace(run, "private_effect_settle_failed", format_args!("{error}"));
        }
        let Some(fact) = PrivateOperationSettled::of(record) else {
            return;
        };
        let id = (run, record.key.operation_id);
        if self.private_settled.contains(&id) {
            return;
        }
        // After the durable transition, under an id derived from the effect.
        match self
            .ports
            .events
            .publish_private_settled(fact.event_id(), &fact)
        {
            Ok(()) => {
                if self.private_settled.len() >= SETTLED_REMEMBERED {
                    self.private_settled.clear();
                }
                self.private_settled.insert(id);
            }
            Err(_) => self.trace(
                run,
                "private_effect_event_unpublished",
                format_args!("{:?}", record.stage),
            ),
        }
    }

    fn private_rows_closed(&mut self, rows: &[PrivateEffectRecord]) {
        for row in rows {
            self.private_effect_observed(row);
        }
    }

    /// Resolution first, for a command that carries a dispatcher id: the
    /// ledger's own resolution in its own transaction, then the settle hook
    /// for every row it closed. Returns how many it closed.
    fn private_resolve_first(
        &mut self,
        owner: BotOwner,
        dispatcher: PrivateDispatcherId,
    ) -> Result<usize, RunError> {
        let resolved = self
            .private_ledger()?
            .announce_private_dispatcher(owner, dispatcher)
            .map_err(private_refused)?;
        self.private_rows_closed(&resolved);
        Ok(resolved.len())
    }

    /// `AnnouncePrivateDispatcher`: the AI worker adopted `owner` under this
    /// dispatcher id. Closes what a dead instance left open for that owner,
    /// settles the decisions those rows belonged to, and answers how many
    /// rows it closed. Another owner's rows and the caller's own are
    /// untouched. Idempotent: a retry finds nothing.
    pub fn bot_announce_private_dispatcher(
        &mut self,
        owner: BotOwner,
        dispatcher: PrivateDispatcherId,
        device: DeviceId,
    ) -> Result<u16, RunError> {
        owner_device(owner, device)?;
        let resolved = self.private_resolve_first(owner, dispatcher)?;
        Ok(u16::try_from(resolved).unwrap_or(u16::MAX))
    }

    /// `PreparePrivateDecision`: the question "may this Bot read this item
    /// once", for the exact live consumer's current turn. The ledger commits
    /// the decision, its wait and the initial projection; no effect row.
    ///
    /// Exact retained replay comes first, as for an edit: when the Run's open
    /// decision is this operation, the retained record is returned to its
    /// origin or to its acknowledged consumer without fresh boot, live-fence,
    /// policy or publication checks, and nothing is written. The same
    /// operation with another proposal is `bot_conflict`; another operation
    /// while a decision owns the Run is `bot_decision_owned`.
    pub fn bot_prepare_private_decision(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        proposal: &PrivateOperationProposal,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<DecisionRecord, RunError> {
        owner_device(fence.owner, device)?;
        if !proposal.valid() {
            return Err(refused("bot_invalid"));
        }
        self.private_ledger()?;
        if let Some(record) = self.retained_private_decision(fence, proposal)? {
            // A replay republishes the open wait (O147); history only.
            self.sync_decision_authority(fence.owner, fence.run)?;
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
        // D6: a Routine Run never has a private question. The stored
        // admission decides, and the ledger repeats it in its writer.
        if provenance.routine.is_some() || proof.routine.is_some() {
            return Err(refused("bot_routine_decision_unsupported"));
        }
        let ActorRef::Person { profile } = submission.request.requested_by else {
            return Err(refused("bot_fenced"));
        };
        // Without the existing delegated launch a successor could never be
        // started, so no answerable question is prepared.
        if let Some(code) = self.decision_successor_unsupported(run)? {
            return Err(refused(code));
        }
        let global_policy = self.decision_global_policy(proof)?;
        let binding = DecisionBindingV1::PrivateOperation(DecisionPrivateBinding {
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
            connection: proposal.connection,
            connection_revision: proposal.connection_revision,
            account: proposal.account,
            item: proposal.item,
            item_ref: proposal.item_ref,
            operation: proposal.operation,
            operation_id: proposal.operation_id,
            request_fingerprint: proposal.request_fingerprint,
            captured_policy: submission.policy.clone(),
            global_policy,
            routine_policy: None,
        });
        let base = self.decision_base(run)?;
        let record = self
            .private_ledger()?
            .prepare_private_decision(&binding, proof, boot, &base)
            .map_err(private_refused)?;
        // The wait is open: publish it for the Run's classification (O147)
        // before the caller learns of the decision.
        self.sync_decision_authority(fence.owner, run)?;
        Ok(record)
    }

    /// `ConsumePrivateDecision`: the one-use consume, only by the exact
    /// acknowledged successor fence. The reply's disposition is `Execute`
    /// exactly once (the first fresh commit); a replay answers the retained
    /// row and never a second license.
    ///
    /// The decision is found by the operation id, not by the Run's open wait,
    /// so the replay still answers after a mid-turn settlement closed the
    /// wait. The ledger compares the proposal, the consumer, the policy and
    /// the deadline in its own transaction.
    pub fn bot_consume_private_decision(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        dispatcher: PrivateDispatcherId,
        proposal: &PrivateOperationProposal,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<PrivateEffectView, RunError> {
        owner_device(fence.owner, device)?;
        if !proposal.valid() {
            return Err(refused("bot_invalid"));
        }
        self.private_resolve_first(fence.owner, dispatcher)?;
        self.decision_boot_current(boot)?;
        let record = self
            .private_ledger()?
            .private_decision_for_operation(fence.owner, fence.run, proposal.operation_id)?
            .ok_or_else(|| refused("bot_fenced"))?;
        let expected = DecisionExpected {
            owner: fence.owner,
            run: fence.run,
            decision: record.receipt.decision,
            revision: record.receipt.state.revision,
            binding_digest: record.receipt.binding_digest,
        };
        let resolved = self
            .private_ledger()?
            .consume_private_decision(
                &expected,
                DecisionTransitionId::mint(),
                dispatcher,
                proposal,
                fence,
                proof,
                boot,
            )
            .map_err(private_refused)?;
        self.private_rows_closed(&resolved.resolved);
        let outcome = resolved.outcome.map_err(private_refused)?;
        // A replay may be the first observer of a final row whose settlement
        // was lost; the hook is idempotent.
        self.private_effect_observed(&outcome.record);
        Ok(outcome.view())
    }

    /// `PreparePrivateEffect`: the intent of a granted read, with no
    /// decision. The reply's disposition is `Execute` on the first fresh
    /// commit only.
    ///
    /// A retained row replays first, without fresh boot or live-consumer
    /// checks. A fresh intent needs this delegate's boot, the exact live
    /// consumer with its current turn, and no decision or cleanup wait on the
    /// Run (O147: during a wait only the exact decision retry passes). The
    /// ledger then proves the grant, captured and current, and both barriers.
    pub fn bot_prepare_private_effect(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        dispatcher: PrivateDispatcherId,
        proposal: &PrivateOperationProposal,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<PrivateEffectView, RunError> {
        owner_device(fence.owner, device)?;
        if !proposal.valid() {
            return Err(refused("bot_invalid"));
        }
        self.private_resolve_first(fence.owner, dispatcher)?;
        let retained = self
            .private_ledger()?
            .private_effect(fence.run, proposal.operation_id)?
            .is_some();
        if !retained {
            if self
                .bot_ledger()?
                .decision_wait_execution(fence.owner, fence.run)?
                .is_some()
            {
                return Err(refused("bot_decision_owned"));
            }
            self.decision_boot_current(boot)?;
            if fence.turn.is_none() {
                return Err(refused("bot_turn_stale"));
            }
            self.native_live_turn(fence, device)?;
        }
        let resolved = self
            .private_ledger()?
            .prepare_private_effect(dispatcher, proposal, fence, proof)
            .map_err(private_refused)?;
        self.private_rows_closed(&resolved.resolved);
        let outcome = resolved.outcome.map_err(private_refused)?;
        self.private_effect_observed(&outcome.record);
        Ok(outcome.view())
    }

    /// The Run's immutable owner, authenticated for a command that names a
    /// Run and no owner (`AdvancePrivateEffect`, `ReadPrivateEffect`): the
    /// Run must be a Bot Run this ledger admitted for this device. The ledger
    /// derives the owner from the same admission and does not compare it
    /// with a caller, so the comparison is made here.
    ///
    /// Deliberately no liveness check: the row's dispatcher must be able to
    /// record the truth after a Stop, a retired attempt or a terminal Run,
    /// and a retry must be able to read a retained outcome then.
    fn private_run_owner(&mut self, run: RunId, device: DeviceId) -> Result<BotOwner, RunError> {
        let admission = self
            .bot_ledger()?
            .bot_admission(run)?
            .ok_or(RunError::UnknownRun)?;
        let provenance = &admission.submission.provenance;
        if admission.submission.request.run != run {
            return Err(refused("bot_fenced"));
        }
        owner_device(provenance.owner, device)?;
        Ok(provenance.owner)
    }

    /// `AdvancePrivateEffect`: the row's own dispatcher records what it
    /// knows. A dispatch mark that meets a moved barrier is committed as
    /// `refused/access_denied` and answered as a reply, never a failure. A
    /// final row then settles its consumed decision (the settle hook) and
    /// publishes its one settled event.
    pub fn bot_advance_private_effect(
        &mut self,
        key: &PrivateEffectKey,
        dispatcher: PrivateDispatcherId,
        transition: &PrivateEffectTransition,
        device: DeviceId,
    ) -> Result<PrivateEffectView, RunError> {
        if !v4(&key.operation_id) {
            return Err(refused("bot_invalid"));
        }
        self.private_ledger()?;
        self.private_run_owner(key.run, device)?;
        let resolved = self
            .private_ledger()?
            .advance_private_effect(key, dispatcher, transition)
            .map_err(private_refused)?;
        self.private_rows_closed(&resolved.resolved);
        let outcome = resolved.outcome.map_err(private_refused)?;
        self.private_effect_observed(&outcome.record);
        Ok(outcome.view())
    }

    /// `ReadPrivateEffect`: the retained effect of one operation, or `None`
    /// after the Run's exact owner read succeeded. History only: it never
    /// answers `Execute` or `deliver`. It carries a dispatcher id, so it
    /// resolves first; a final row it finds is offered to the settle hook.
    pub fn bot_read_private_effect(
        &mut self,
        run: RunId,
        operation: uuid::Uuid,
        dispatcher: PrivateDispatcherId,
        device: DeviceId,
    ) -> Result<Option<PrivateEffectView>, RunError> {
        if !v4(&operation) {
            return Err(refused("bot_invalid"));
        }
        self.private_ledger()?;
        self.private_run_owner(run, device)?;
        let resolved = self
            .private_ledger()?
            .read_private_effect(run, operation, dispatcher)
            .map_err(private_refused)?;
        self.private_rows_closed(&resolved.resolved);
        let Some(outcome) = resolved.outcome.map_err(private_refused)? else {
            return Ok(None);
        };
        self.private_effect_observed(&outcome.record);
        Ok(Some(outcome.view()))
    }

    /// The Run's open private decision for exactly this operation, read
    /// through the sole ledger decoder. History only: it grants nothing and
    /// writes nothing. `Ok(None)` only when the Run has no open wait at all
    /// (a settled decision is still replayed by the ledger's own exact
    /// binding match).
    fn retained_private_decision(
        &mut self,
        fence: &BotExecutionFence,
        proposal: &PrivateOperationProposal,
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
        let DecisionBindingV1::PrivateOperation(binding) = &record.receipt.binding else {
            return Err(refused("bot_decision_owned"));
        };
        if binding.operation_id != proposal.operation_id {
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
        if !record.receipt.binding.matches_private(proposal) {
            return Err(refused("bot_conflict"));
        }
        Ok(Some(record))
    }
}

#[cfg(test)]
mod tests;
