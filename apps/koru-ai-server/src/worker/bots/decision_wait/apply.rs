// SPDX-License-Identifier: MPL-2.0
//! Koru saves an approved edit itself (O133, design B).
//!
//! When a person allows a Bot's page edit, the decision's fresh successor is
//! told that Koru is applying exactly that edit and must not repeat it
//! (`continuation_body` in the delegate runtime). Right after the successor's
//! continuation is acknowledged — when the held wait's poll rolls the task
//! over to its consumer — Koru saves the approved edit once, under the
//! successor's acknowledged consumer fence, through the same one-use consume
//! and Store executor the model's exact retry uses
//! ([`crate::native_effect::consume`]). The model never has to make the edit.
//!
//! - **Once.** "Saved exactly once" is the ledger's one-use consume: the first
//!   consume carries the one Execute lease; every replay (a lost reply, the
//!   model's repeat) only reconciles the retained intent.
//! - **Positive proof only.** An attempt starts only on the poll's
//!   authenticated read of this exact wait (`Granted/AwaitRetry`, or
//!   `Consumed/Effect` while Koru's own consume may have committed), with
//!   the receipt the trusted preparer handed over, current human proof read
//!   off the writer, the binding's own policy revisions still current and a
//!   probed delegate boot. Anything unproven sends nothing.
//! - **No spin.** At most one job per wait, started only by a completed
//!   consumer read (the existing poll cadence, 250 ms doubling to 32 s), and
//!   at most [`MAX_APPLY_ATTEMPTS`] per wait. A full lane counts as an
//!   attempt.
//! - **Records nothing.** The AI writes no row. Success is the ledger's own
//!   native receipt; any failure leaves the model's exact retry as the
//!   fallback, which saves once.
//! - **Receipt gone** (an AI restart, O146): nothing happens.
use crate::native_effect::{
    ApprovedEdit, NativeEffectHost, NativeEffectSubject, consume, parse_fingerprint, probe_boot,
};
use crate::store_tools::{NativeStoreAuthority, native_store_backend};
use koru_ai_core::InferenceAttemptId;
use koru_ai_core::bots::{BotAuthority, BotError, BotPage};
use koru_delegate_api::bots::{BotDelegatePort, DecisionReceiptView};
use koru_delegate_core::decisions::{
    BotWaitEnrollmentKey, DecisionAuthorization, DecisionBindingV1, DecisionEditAction,
    DecisionFence, DecisionKind, DecisionPhase, DecisionState, DecisionValidate,
};
use koru_delegate_core::{
    BotExecutionFence, BotId, BotOwner, FreshBotPolicyProof, NativeResultKey,
    same_native_edit_revision,
};
use koru_mcp_server::native::NativeWriteKind;
use koru_store_server::StoreWorkerClient;
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

#[cfg(test)]
mod tests;

/// Attempts per wait, the first save and its reconciliations together. The
/// consumer read's own backoff spaces them; past this, the model's exact
/// retry (and the existing native recovery) own the outcome.
pub(in crate::worker) const MAX_APPLY_ATTEMPTS: u32 = 4;

/// How one attempt ended. Only `Saved` is a positive outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::worker) enum ApplyOutcome {
    /// The ledger registered the approved edit's one receipt.
    Saved,
    /// Nothing that could commit was sent (proof read, boot probe, the
    /// shared builder, a full lane). A later read may try again.
    Unavailable,
    /// Koru's consume may have committed: only reconciliation remains.
    Unknown,
    /// A definite refusal, or nothing to apply. Nothing more is tried; the
    /// model's exact retry is the fallback.
    Refused,
}

/// One wait's trigger state, on the writer. Never persisted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::worker) struct ApplyProgress {
    consumer: BotExecutionFence,
    attempts: u32,
    busy: bool,
    /// An earlier attempt may have committed the one consume.
    committed: bool,
    done: bool,
}
impl ApplyProgress {
    /// For the acknowledged consumer the rollover installed.
    pub(in crate::worker) fn new(consumer: BotExecutionFence) -> Self {
        Self {
            consumer,
            attempts: 0,
            busy: false,
            committed: false,
            done: false,
        }
    }
    pub(in crate::worker) fn consumer(&self) -> &BotExecutionFence {
        &self.consumer
    }
    #[cfg(test)]
    pub(in crate::worker) fn attempts(&self) -> u32 {
        self.attempts
    }
    /// Whether this authenticated read of the wait lets one attempt start.
    /// Deny, expiry, cancellation and every settled or foreign state never do.
    pub(in crate::worker) fn due(&self, state: &DecisionState) -> bool {
        if self.busy
            || self.done
            || self.attempts >= MAX_APPLY_ATTEMPTS
            || state.kind != DecisionKind::KoruEdit
        {
            return false;
        }
        match (state.authorization, state.phase) {
            // Approved and waiting for its one consume.
            (DecisionAuthorization::Granted, DecisionPhase::AwaitRetry) => true,
            // Consumed: reconcile only what Koru itself may have committed.
            (DecisionAuthorization::Consumed, DecisionPhase::Effect) => self.committed,
            _ => false,
        }
    }
    pub(in crate::worker) fn start(&mut self) {
        self.busy = true;
        self.attempts = self.attempts.saturating_add(1);
    }
    pub(in crate::worker) fn finish(&mut self, outcome: ApplyOutcome) {
        self.busy = false;
        match outcome {
            ApplyOutcome::Saved | ApplyOutcome::Refused => self.done = true,
            ApplyOutcome::Unknown => self.committed = true,
            ApplyOutcome::Unavailable => {}
        }
    }
    /// Nothing to apply in this process (no receipt, no executor).
    pub(in crate::worker) fn stop(&mut self) {
        self.busy = false;
        self.done = true;
    }
}

/// The retained receipt of exactly this wait's interactive Koru edit, or
/// `None` (gone after an AI restart, never handed over, or another subject).
pub(in crate::worker) fn approved_receipt<'a>(
    view: Option<&'a DecisionReceiptView>,
    key: &BotWaitEnrollmentKey,
    origin: &DecisionFence,
) -> Option<&'a DecisionReceiptView> {
    let view = view?;
    let DecisionBindingV1::KoruEdit(binding) = &view.receipt.binding else {
        return None;
    };
    (view.receipt.validate().is_ok()
        && view.receipt.decision == key.decision_id
        && binding.owner == key.owner
        && binding.bot == key.bot
        && binding.run == key.run
        && binding.routine.is_none()
        && view.receipt.binding.origin() == *origin)
        .then_some(view)
}

/// What one attempt needs, gathered on the writer; it runs off the writer.
pub(in crate::worker) struct ApplyWork {
    pub(in crate::worker) store: StoreWorkerClient,
    pub(in crate::worker) port: Arc<dyn BotDelegatePort>,
    pub(in crate::worker) authority: Arc<dyn BotAuthority>,
    pub(in crate::worker) view: DecisionReceiptView,
    pub(in crate::worker) consumer: BotExecutionFence,
    /// The current global barrier, read on the writer as a card's answer reads it.
    pub(in crate::worker) proof: FreshBotPolicyProof,
}

/// An attempt's outcome and the notifications the shared effect asked for;
/// the writer acts on them after the job.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::worker) struct ApplyReport {
    pub(in crate::worker) outcome: ApplyOutcome,
    pub(in crate::worker) settled: bool,
    pub(in crate::worker) uncertain: Option<(BotOwner, BotId, NativeResultKey)>,
}

/// The shared effect's host for this job: stable codes, and the two
/// notifications kept for the writer.
#[derive(Default)]
struct Notes {
    settled: Cell<bool>,
    uncertain: RefCell<Option<(BotOwner, BotId, NativeResultKey)>>,
}
impl NativeEffectHost for Notes {
    type Failure = &'static str;
    fn failure(&self, code: &'static str) -> &'static str {
        code
    }
    fn settled(&self) {
        self.settled.set(true);
    }
    fn uncertain(
        &self,
        owner: BotOwner,
        bot: BotId,
        key: NativeResultKey,
    ) -> Result<(), &'static str> {
        *self.uncertain.borrow_mut() = Some((owner, bot, key));
        Ok(())
    }
}

/// An authority error that may pass is not a refusal.
fn transient(error: BotError) -> bool {
    matches!(error, BotError::Unavailable | BotError::Storage)
}

impl ApplyWork {
    pub(in crate::worker) fn run(&self) -> ApplyReport {
        let notes = Notes::default();
        // A panic after the consume may have committed it: reconcile later.
        let outcome =
            catch_unwind(AssertUnwindSafe(|| self.apply(&notes))).unwrap_or(ApplyOutcome::Unknown);
        ApplyReport {
            outcome,
            settled: notes.settled.get(),
            uncertain: notes.uncertain.take(),
        }
    }

    fn apply(&self, notes: &Notes) -> ApplyOutcome {
        use ApplyOutcome::{Refused, Saved, Unavailable, Unknown};
        let receipt = &self.view.receipt;
        let DecisionBindingV1::KoruEdit(binding) = &receipt.binding else {
            return Refused;
        };
        let consumer = &self.consumer;
        let Some(turn) = consumer.turn else {
            return Refused;
        };
        if receipt.validate().is_err()
            || binding.routine.is_some()
            || binding.action != DecisionEditAction::EditDocument
            || consumer.owner != binding.owner
            || consumer.bot != binding.bot
            || consumer.run != binding.run
        {
            return Refused;
        }
        // Current human proof, off the writer, as a card's answer reads it:
        // the same owner, and the decision's human still the home's profile.
        let page = BotPage {
            workspace: binding.workspace,
            document: binding.target,
        };
        let checked = match self
            .authority
            .inspect(&[binding.workspace], std::slice::from_ref(&page))
        {
            Ok(checked) => checked,
            Err(error) if transient(error) => return Unavailable,
            Err(_) => return Refused,
        };
        let owner = koru_ai_core::bots::BotOwner {
            user_scope: binding.owner.user_scope,
            device: binding.owner.device,
        };
        if checked.owner != owner {
            return Refused;
        }
        let profile = match checked.workspace(binding.workspace) {
            Ok(profile) if profile == binding.human_profile => profile,
            Err(error) if transient(error) => return Unavailable,
            _ => return Refused,
        };
        match checked.page(&page) {
            Ok(()) => {}
            Err(error) if transient(error) => return Unavailable,
            Err(_) => return Refused,
        }
        // The binding's own policy identity, still current and effective.
        let global = &self.proof.global;
        if self.proof.routine.is_some()
            || !global.effects_allowed
            || global.key != binding.global_policy.key
            || global.config_revision != binding.global_policy.config_revision.get()
            || global.policy_revision != binding.global_policy.policy_revision.get()
        {
            return Refused;
        }
        let Some(boot) = probe_boot(self.port.as_ref(), binding.owner) else {
            return Unavailable;
        };
        // The prepared write is the shared builder's, over the exact approved
        // arguments; it must describe exactly the bound edit.
        let Ok(arguments) = serde_json::from_slice(binding.payload.as_slice()) else {
            return Refused;
        };
        let authority = NativeStoreAuthority {
            owner_scope: binding.owner.user_scope,
            expected_profile: profile,
            workspace: binding.workspace,
        };
        let backend = native_store_backend(self.store.clone(), authority.clone());
        let Ok(prepared) = backend.prepare_native_write("edit_document", arguments) else {
            // Nothing was sent; the Store may answer on a later read.
            return Unavailable;
        };
        let description = prepared.description();
        let Some(fingerprint) = parse_fingerprint(&description.fingerprint) else {
            return Refused;
        };
        let exact = description.kind == NativeWriteKind::Edited
            && description.workspace == binding.workspace
            && description.document == Some(binding.target)
            && description
                .expected_revision
                .as_deref()
                .is_some_and(|revision| {
                    same_native_edit_revision(revision, binding.expected_revision.as_slice())
                        .unwrap_or(false)
                })
            && receipt.binding.matches_edit_except_revision(
                binding.operation_id,
                binding.target,
                fingerprint,
                binding.payload.as_slice(),
            );
        if !exact {
            return Refused;
        }
        let key = NativeResultKey {
            run: consumer.run,
            attempt: consumer.attempt,
            generation: consumer.generation,
            turn,
            operation_id: binding.operation_id,
        };
        let edit = ApprovedEdit {
            fence: consumer,
            key: &key,
            fingerprint,
            payload: &binding.payload,
        };
        match consume(
            notes,
            &self.store,
            self.port.as_ref(),
            &backend,
            &authority,
            NativeEffectSubject {
                owner: binding.owner,
                bot: binding.bot,
            },
            &prepared,
            &edit,
            &self.view,
            &self.proof,
            boot,
        ) {
            Ok(_) => Saved,
            // The consume or its effect may have committed.
            Err("save_status_unknown" | "native_owner_unavailable" | "native_recovery_pending") => {
                Unknown
            }
            // A typed refusal, a refused Store write or a conflicting intent.
            Err(_) => Refused,
        }
    }
}

/// One attempt on the existing administration pool.
pub(in crate::worker) struct ApplyJob {
    pub(in crate::worker) attempt: InferenceAttemptId,
    pub(in crate::worker) key: BotWaitEnrollmentKey,
    pub(in crate::worker) work: ApplyWork,
}
pub(in crate::worker) struct ApplyResult {
    pub(in crate::worker) attempt: InferenceAttemptId,
    pub(in crate::worker) key: BotWaitEnrollmentKey,
    pub(in crate::worker) report: ApplyReport,
}
impl ApplyJob {
    pub(in crate::worker) fn run(self) -> ApplyResult {
        let report = self.work.run();
        ApplyResult {
            attempt: self.attempt,
            key: self.key,
            report,
        }
    }
}
