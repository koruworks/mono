// SPDX-License-Identifier: MPL-2.0

//! O150: a Koru edit decision settles as soon as its one approved save is
//! positively recorded, so the decision-wait fence (O147) lifts mid-turn and
//! the resumed Bot can keep working.
//!
//! Both savers end in the same delegate write: Koru's own trigger (the AI's
//! approved-edit job) and the model's exact retry through the gateway each
//! run the shared native effect, whose positive Store outcome is recorded by
//! `ConfirmNativeResult` (`Orchestrator::confirm_native_result`). That one
//! place calls [`Orchestrator::settle_saved_decision_effect`]; the per-turn
//! resume driver calls it too, so a settlement lost to a failed write or a
//! restart is taken on a later turn.
//!
//! Positive proof only: the ledger's own retained outcome of the consumed
//! effect must be `Durable` (its exact verified evidence, lease settled). An
//! unresolved, unknown or refused save changes nothing here; the fence then
//! stays until the existing cleanup owns the wait. The settlement is the
//! ledger's existing `settle_decision_effect` (decision contract section 4,
//! "exact resolved effect safely available to its continuation settles the
//! decision"), which records the owed `DecisionResolved` Control. The wait
//! itself settles later on the ordinary path, once that Control is applied
//! and the sole handle has returned (`settle_publication_wait`); admission,
//! capacity and the successor's attempt are not touched.

use super::*;
use koru_delegate_core::decisions::{
    BotWaitPhase, DecisionAuthorization, DecisionKind, DecisionPhase, DecisionRetainedEffect,
};
use koru_delegate_sqlite::BotLedgerError;

impl Orchestrator {
    /// Settle the Run's open Koru edit decision when its consumed effect's
    /// retained outcome is positively `Durable`. `Ok(true)` when this call
    /// recorded the settlement; `Ok(false)` when there is nothing to settle
    /// (no open wait, not a consumed Koru edit in its effect phase, a cleanup
    /// already owning the wait, or an outcome that is not a positive save).
    /// A replay of an already settled decision records nothing.
    pub(crate) fn settle_saved_decision_effect(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<bool, RunError> {
        let Some(execution) = self.bot_ledger()?.decision_wait_execution(owner, run)? else {
            return Ok(false);
        };
        let (Some(expected), Some(state)) = (
            execution.expected.decision.clone(),
            execution.decision_state.as_ref(),
        ) else {
            return Ok(false);
        };
        if state.kind != DecisionKind::KoruEdit
            || state.authorization != DecisionAuthorization::Consumed
            || state.phase != DecisionPhase::Effect
            || execution.wait.phase != BotWaitPhase::ResumeReserved
            || execution.wait.cleanup.is_some()
        {
            return Ok(false);
        }
        let effect =
            match self
                .bot_ledger()?
                .decision_retained_effect(owner, run, expected.decision)
            {
                Ok(Some(effect @ DecisionRetainedEffect::Durable { .. })) => effect,
                // A refused save, nothing owed, or an effect still unresolved
                // (a held lease, an unconfirmed or unknown outcome): no change.
                Ok(_) | Err(BotLedgerError::Fenced) => return Ok(false),
                Err(error) => return Err(error.into()),
            };
        self.bot_ledger()?.settle_decision_effect(
            &expected,
            DecisionTransitionId::mint(),
            &effect,
        )?;
        Ok(true)
    }

    /// The `ConfirmNativeResult` hook: after a page result's evidence was
    /// recorded, settle the Run's decision if this was its approved save.
    /// A failure is traced, never raised: the confirmation itself answered
    /// truthfully, the fence stays up, and the resume driver retries.
    pub(crate) fn settle_decision_after_confirmation(&mut self, run: RunId) {
        let owner = match self.bot_ledger().and_then(|ledger| {
            ledger
                .bot_execution(run)
                .map(|record| record.map(|record| record.owner))
                .map_err(Into::into)
        }) {
            Ok(Some(owner)) => owner,
            Ok(None) => return,
            Err(error) => {
                self.trace(
                    run,
                    "decision_effect_settle_unread",
                    format_args!("{error}"),
                );
                return;
            }
        };
        if let Err(error) = self.settle_saved_decision_effect(owner, run) {
            self.trace(
                run,
                "decision_effect_settle_failed",
                format_args!("{error}"),
            );
        }
    }
}

#[cfg(test)]
mod tests;
