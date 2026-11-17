// SPDX-License-Identifier: MPL-2.0

//! `RequestChild`: what the runtime proves before the ledger's one
//! transaction, and what it does right after.
//!
//! Replay comes first and needs no live parent: the same ask is answered from
//! the retained rows even after the parent was stopped. A fresh ask needs the
//! parent's exact live attempt and turn, a child folder that does not overlap
//! the claim the parent keeps for the whole wait, and a host that could
//! launch the parent's successor at all.

use super::*;
use koru_delegate_core::children::{
    ChildRefusal, ChildRefused, ChildRequest, ChildRequestOutcome, ChildRequested, ChildWaitView,
};
use koru_delegate_core::decisions::{BotWaitPhase, DecisionResumeState, DecisionValidate};
use koru_delegate_core::{ActorRef, DelegateBootId, DeviceId};
use koru_delegate_scope::ScopeRefusal;

fn refusal(code: ChildRefusal, ordinal: Option<usize>) -> ChildRequestOutcome {
    ChildRequestOutcome::Refused(ChildRefused {
        code,
        ordinal: ordinal.and_then(|index| u8::try_from(index).ok()),
    })
}

impl Orchestrator {
    /// One ask of one to three other Bots. `Ok(Refused)` committed nothing.
    pub fn bot_request_child(
        &mut self,
        request: &ChildRequest,
        device: DeviceId,
        boot: DelegateBootId,
    ) -> Result<ChildRequestOutcome, RunError> {
        owner_device(request.parent.owner, device)?;
        self.decision_boot_current(boot)?;
        // Self-ask has a code of its own, so it is named before the shape
        // check that would report it as a malformed binding.
        if let Some(index) = request
            .requests
            .iter()
            .position(|item| item.binding.recipient_bot == request.parent.bot)
        {
            return Ok(refusal(ChildRefusal::SelfAsk, Some(index)));
        }
        request.validate().map_err(|_| refused("bot_invalid"))?;
        let (owner, run) = (request.parent.owner, request.parent.run);
        if let Some(replayed) = self.child_replay(request)? {
            return Ok(replayed);
        }
        // The Run's waits, before anything that needs its document: a Run
        // has one open wait (A5), and while one is open the handle may be
        // away, so the answer comes from the ledger's rows.
        self.restore_publication_owner(run)?;
        if let Some(open) = self.bot_ledger()?.decision_wait_execution(owner, run)? {
            if open.expected.decision.is_some() {
                return Ok(refusal(ChildRefusal::DecisionOpen, None));
            }
            return match self.bot_ledger()?.child_wait_execution(owner, run)? {
                Some(child) => {
                    let acknowledged = child.resume.as_ref().is_some_and(|resume| {
                        matches!(resume.state, DecisionResumeState::InputAcknowledged { .. })
                    });
                    Ok(refusal(
                        if acknowledged {
                            ChildRefusal::WaitClosing
                        } else {
                            ChildRefusal::WaitOpen
                        },
                        None,
                    ))
                }
                // A cleanup-only wait owns the Run; nothing new is admitted.
                None => Err(refused("bot_fenced")),
            };
        }
        // The parent's exact executing attempt and acknowledged turn, its
        // controller, its budget and its current policy.
        self.bot_live_fence(&request.parent, device, true, true)?;
        let admission = self
            .bot_ledger()?
            .bot_admission(run)?
            .ok_or(RunError::UnknownRun)?;
        let parent = &admission.submission;
        let first = &request.requests[0].binding;
        // The person is the parent Run's own originating person (3.4), and
        // the binding names the parent as it was admitted.
        if parent.request.requested_by
            != (ActorRef::Person {
                profile: request.human,
            })
            || parent.provenance.owner != owner
            || parent.provenance.bot != request.parent.bot
            || first.parent_submission != u32::from(admission.submission_version)
            || first.parent_routine != parent.provenance.routine
        {
            return Err(refused("bot_invalid"));
        }
        // The waiting parent keeps its folder claim until its successor takes
        // it over, so a child whose folder overlaps it could never start.
        for (index, item) in request.requests.iter().enumerate() {
            let scope =
                match self.plan_scope(item.binding.child_run, &item.submission.request.scope) {
                    Ok(scope) => scope,
                    // Scope recovery is incomplete: not a folder overlap, and
                    // worth the caller's retry. The code does not start with
                    // `bot_`, so the wire marks it retryable (the gateway's
                    // `child_status_unknown`); a missing folder stays final.
                    Err(ScopeRefusal::Busy) => {
                        self.ensure_scope_recovery()?;
                        return Err(refused("scope_recovery_pending"));
                    }
                    // The recipient's folder cannot be resolved; never recreated.
                    Err(_) => return Err(refused("bot_folder_unavailable")),
                };
            if self
                .claims
                .holders_of(&scope.identity)
                .iter()
                .any(|holder| holder.run == run)
            {
                return Ok(refusal(ChildRefusal::FolderOverlap, Some(index)));
            }
        }
        // Release qualification: the parent is held by stop-and-successor, so
        // an ask is refused where no successor could be launched.
        if self.child_successor_unsupported(run)?.is_some() {
            return Ok(refusal(ChildRefusal::Capacity, None));
        }
        // The gate (M8): under a paused team every recipient is held, so the
        // ask is refused whole with the refusal a paused recipient already
        // has. A recipient that is itself paused is the ledger's to name.
        // Not told: admitted and queued, as a submission is.
        for (index, item) in request.requests.iter().enumerate() {
            let recipient = &item.submission.provenance;
            if self.bot_held(recipient.owner, recipient.bot)? == crate::orchestrator::Held::Team {
                return Ok(refusal(ChildRefusal::RecipientPaused, Some(index)));
            }
        }
        let base = self.child_base(owner, run)?;
        let outcome = self.bot_ledger()?.request_child(request, boot, &base)?;
        if let ChildRequestOutcome::Created(requested) = &outcome {
            self.child_publish(&requested.receipts, true);
            self.children.clock.started(self.ports.clock.now_ms());
            // Each child's Run document, as `bot_submit` creates one after
            // `admit_bot`: a queued child can then be stopped like any queued
            // Run. A failure leaves the intent pending for dispatch (or the
            // next Stop) to complete forward.
            for item in &request.requests {
                self.child_complete_intent(item.binding.child_run);
            }
        }
        if !matches!(outcome, ChildRequestOutcome::Refused(_)) {
            // The wait is open: publish it for the Run's classification
            // before the caller learns of it (4.6).
            self.sync_child_authority(owner, run)?;
        }
        Ok(outcome)
    }

    /// The projection base of a new wait: the attached document's state, as
    /// for a first decision, continuing from the Run's last applied
    /// projection when an earlier wait left one (ledger notes, piece R: "for
    /// a second wait of one Run the base must continue from the last applied
    /// projection"). The ledger checks every field.
    fn child_base(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<koru_delegate_core::decisions::DecisionPublicationBase, RunError> {
        let mut base = self.decision_base(run)?;
        if let Some(applied) = self
            .bot_ledger()?
            .latest_applied_decision_projection(owner, run)?
        {
            base.predecessor.projection =
                Some(koru_delegate_core::decisions::DecisionProjectionPrior {
                    transition: applied.identity.transition,
                    sequence: applied.identity.sequence,
                    target_digest: applied.identity.target_digest,
                });
        }
        Ok(base)
    }

    /// Complete a child Run's pending intent (its Run document), best
    /// effort. Nothing here refuses the ask: it is already committed.
    pub(super) fn child_complete_intent(&mut self, run: RunId) {
        let pending = matches!(
            self.ports.store.intent(run),
            Ok(Some(intent)) if intent.state == koru_delegate_sqlite::IntentState::Pending
        );
        if !pending {
            return;
        }
        let admission = match self
            .bot_ledger()
            .and_then(|ledger| Ok(ledger.bot_admission(run)?))
        {
            Ok(Some(admission)) => admission,
            _ => return,
        };
        let submission = admission.submission;
        if let Err(error) = self.complete_intent(submission.request, submission.workspace) {
            self.trace(run, "child_intent_pending", format_args!("{error}"));
        }
    }

    /// The same ask again, answered from the retained rows. `None` when no
    /// request of the named wait exists; a difference is `bot_conflict`.
    fn child_replay(
        &mut self,
        request: &ChildRequest,
    ) -> Result<Option<ChildRequestOutcome>, RunError> {
        let (owner, bot, run) = (request.parent.owner, request.parent.bot, request.parent.run);
        let wait = request.wait().ok_or_else(|| refused("bot_invalid"))?;
        let now = self.ports.clock.now_ms();
        let Some(execution) = self.bot_ledger()?.bot_execution(run)? else {
            return Err(RunError::UnknownRun);
        };
        if execution.owner != owner || execution.bot != bot {
            return Err(refused("bot_owner_mismatch"));
        }
        let all = self.bot_ledger()?.read_child_requests(owner, run, now)?;
        let mine: Vec<_> = all
            .requests
            .iter()
            .filter(|receipt| receipt.wait == wait)
            .cloned()
            .collect();
        if mine.is_empty() {
            return Ok(None);
        }
        let mut same = mine.len() == request.requests.len();
        for (receipt, item) in mine.iter().zip(&request.requests) {
            let digest = item.binding.digest().map_err(|_| refused("bot_invalid"))?;
            same &= receipt.child_request == item.child_request
                && receipt.ordinal == item.binding.ordinal
                && receipt.binding_digest == digest;
        }
        if !same {
            return Err(refused("bot_conflict"));
        }
        let observation = self
            .bot_ledger()?
            .read_bot_child_wait(owner, bot, run, wait)?
            .ok_or_else(|| refused("bot_stale"))?;
        let view = all
            .wait
            .filter(|open| open.wait == wait)
            .unwrap_or(ChildWaitView {
                wait,
                phase: BotWaitPhase::Settled,
                revision: observation.current_revision,
                open_requests: 0,
            });
        Ok(Some(ChildRequestOutcome::AlreadyCreated(ChildRequested {
            wait: view,
            enrollment: observation.cleanup_intent,
            receipts: mine,
        })))
    }
}
