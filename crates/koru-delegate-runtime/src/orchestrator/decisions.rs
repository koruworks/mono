// SPDX-License-Identifier: MPL-2.0

//! Runtime entry points for decision waits. The sole C ledger owns every
//! decision transition; these methods qualify the live runtime evidence a
//! transition needs (exact consumer, retained native prompt, attached
//! document base) and forward physical native answers with their journaled
//! key. Nothing here grants authority from cached state or a timeout.

use super::bots::{owner_device, refused};
use super::{Orchestrator, RunError};
use koru_delegate_core::decisions::{
    BotDecisionWaitObservation, BotWaitId, DecisionBindingV1, DecisionDigest, DecisionGlobalPolicy,
    DecisionId, DecisionList, DecisionNativeBinding, DecisionNativeResponse,
    DecisionNativeResponseKey, DecisionProjectionPredecessor, DecisionPublicationBase,
    DecisionRevision, DecisionTransitionId, DecisionVersionV1, decision_projection_subject_digest,
};
use koru_delegate_core::{
    ActorRef, ApprovalId, BotExecutionFence, BotId, BotOwner, DelegateBootId, DeviceId,
    FreshBotPolicyProof, RunId,
};
use koru_delegate_sqlite::{AnswerDecision, DecisionRecord};

mod authority;
mod driver;
mod edit;
mod launch;
mod resume;
mod settle;
mod successor;
pub use driver::DecisionResumeStep;
pub use edit::DecisionEditRequest;
pub use resume::DecisionEnrollmentBridge;

impl Orchestrator {
    /// A decision is prepared, answered, consumed and resumed only under this
    /// delegate process's current boot (as `synchronize_boot_bot` requires).
    /// The ledger keeps the caller's boot as the decision's creation boot, so
    /// a stale one is refused here, before any ledger write.
    pub(super) fn decision_boot_current(&self, boot: DelegateBootId) -> Result<(), RunError> {
        if boot != self.delegate_boot() {
            return Err(refused("bot_boot_stale"));
        }
        Ok(())
    }

    /// Prepare a native approval decision for the exact live consumer's
    /// pending prompt, from its retained offered choices. The document base is
    /// the attached handle's current state; C commits the decision, its wait
    /// and the initial projection in one writer transaction.
    pub fn bot_prepare_native_decision(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
        approval: ApprovalId,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<DecisionRecord, RunError> {
        self.decision_boot_current(boot)?;
        self.bot_live_fence(fence, device, true, true)?;
        let run = fence.run;
        let turn = fence.turn.ok_or_else(|| refused("bot_turn_stale"))?;
        self.restore_publication_owner(run)?;
        if self.run_publication_pending(run) {
            return Err(refused("bot_decision_owned"));
        }
        let (choices, origin_session) = {
            let attempt = self
                .open(run)?
                .attempt
                .as_ref()
                .ok_or_else(|| refused("bot_fenced"))?;
            if attempt.pending_approvals.get(&approval) != Some(&turn) {
                return Err(refused("bot_approval_stale"));
            }
            let choices = attempt
                .native_prompts
                .get(&approval)
                .cloned()
                .ok_or_else(|| refused("bot_prompt_unretained"))?;
            (choices, attempt.session)
        };
        if origin_session != Some(fence.session) {
            return Err(refused("bot_fenced"));
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
        // A Routine native subject needs its Routine policy binding; that
        // composition is not part of this runtime slice and is refused.
        if provenance.routine.is_some() {
            return Err(refused("bot_routine_decision_unsupported"));
        }
        let ActorRef::Person { profile } = submission.request.requested_by else {
            return Err(refused("bot_fenced"));
        };
        let retained = self
            .native_ledger()?
            .native_policy(&proof.global.key)?
            .ok_or_else(|| refused("bot_policy_unavailable"))?;
        let global_policy = DecisionGlobalPolicy {
            key: proof.global.key.clone(),
            config_revision: DecisionRevision::new(proof.global.config_revision)
                .map_err(|_| refused("bot_policy_unavailable"))?,
            policy_revision: DecisionRevision::new(proof.global.policy_revision)
                .map_err(|_| refused("bot_policy_unavailable"))?,
            digest: DecisionDigest::from_bytes(retained.payload_digest),
        };
        let binding = DecisionBindingV1::NativeApproval(DecisionNativeBinding {
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
            prompt: approval,
            choices: DecisionList::new(choices).map_err(|_| refused("bot_prompt_unretained"))?,
            // O116: the session protocol carries no native expiry.
            native_deadline_at_ms: None,
            captured_policy: submission.policy.clone(),
            global_policy,
            routine_policy: None,
        });
        let base = self.decision_base(run)?;
        let record = self
            .bot_ledger()?
            .prepare_bot_decision(&binding, proof, boot, &base)?;
        // The wait is open: publish it for the Run's classification (O147)
        // before the caller learns of the decision.
        self.sync_decision_authority(fence.owner, run)?;
        Ok(record)
    }

    /// The attached document's current state as the first projection base.
    /// No synthetic header, status or timeline offset is used.
    fn decision_base(&mut self, run: RunId) -> Result<DecisionPublicationBase, RunError> {
        let state = self.open(run)?;
        let (workspace, document) = (state.workspace, state.document);
        let doc = state.doc.document()?;
        let header = doc.header()?;
        let timeline_len = doc.timeline_len() as u64;
        let source_schema = u8::try_from(doc.schema_version().unwrap_or_default())
            .ok()
            .and_then(|schema| schema.try_into().ok())
            .ok_or_else(|| refused("bot_document_schema"))?;
        let subject_digest =
            decision_projection_subject_digest(&header).map_err(|_| refused("bot_fenced"))?;
        Ok(DecisionPublicationBase {
            workspace,
            document,
            source_schema,
            observed_created_at_ms: header.created_at_ms,
            timeline_len,
            subject_digest,
            predecessor: DecisionProjectionPredecessor {
                observed_created_at_ms: header.created_at_ms,
                timeline_start: timeline_len,
                projection: None,
                status: header.status,
                failure_reason: header.failure_reason,
            },
            bookkeeping: None,
        })
    }

    /// Answer an open decision through the sole C writer (the human route
    /// authenticates before this call). A native answer is then forwarded
    /// physically with the key C journaled, and its observed outcome is
    /// recorded: Acknowledged only on the session host's positive answer,
    /// otherwise Unconfirmed. Retrying the same answer replays its key.
    pub fn bot_answer_decision(
        &mut self,
        input: &AnswerDecision,
        device: DeviceId,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<DecisionRecord, RunError> {
        owner_device(input.owner, device)?;
        self.decision_boot_current(boot)?;
        let record = self.bot_ledger()?.answer_bot_decision(
            input,
            DecisionTransitionId::mint(),
            proof,
            boot,
        )?;
        match record.native_response.clone() {
            Some(DecisionNativeResponse::Prepared { key })
            | Some(DecisionNativeResponse::Unconfirmed { key }) => {
                self.bot_forward_native_answer(input.owner, input.run, key)
            }
            _ => Ok(record),
        }
    }

    /// Forward one journaled native answer with its stable key. The prompt
    /// must still be the exact live consumer's pending prompt.
    fn bot_forward_native_answer(
        &mut self,
        owner: BotOwner,
        run: RunId,
        key: DecisionNativeResponseKey,
    ) -> Result<DecisionRecord, RunError> {
        let pending = self
            .runs
            .get(&run)
            .and_then(|state| state.attempt.as_ref())
            .is_some_and(|attempt| {
                attempt.session == Some(key.session)
                    && attempt.pending_approvals.get(&key.prompt) == Some(&key.turn)
            });
        if !pending {
            return Err(refused("bot_approval_stale"));
        }
        let answered = self.ports.sessions.bot_answer_approval(
            key.session,
            key.response_key,
            key.prompt,
            key.choice.decision(),
            Some(key.choice.id),
        );
        let response = match answered {
            Ok(()) => DecisionNativeResponse::Acknowledged {
                key: key.clone(),
                acknowledgment: key,
                observed_at_ms: self.ports.clock.now_ms(),
            },
            // A transport failure may still have delivered the answer: it is
            // unknown, never a refusal; the same key retries it later.
            Err(_) => DecisionNativeResponse::Unconfirmed { key },
        };
        Ok(self.bot_ledger()?.record_decision_native_answer(
            owner,
            run,
            DecisionTransitionId::mint(),
            &response,
        )?)
    }

    /// The authenticated Run route behind IPC16 `ReadBotDecisionWait`.
    /// Read-only and historical; it never grants permission to stop.
    pub fn bot_read_decision_wait(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        run: RunId,
        wait: BotWaitId,
        decision: DecisionId,
        device: DeviceId,
    ) -> Result<Option<BotDecisionWaitObservation>, RunError> {
        owner_device(owner, device)?;
        Ok(self
            .bot_ledger()?
            .read_bot_decision_wait(owner, bot, run, wait, decision)?)
    }
}
