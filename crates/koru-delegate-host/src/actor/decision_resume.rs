// SPDX-License-Identifier: MPL-2.0

//! The per-turn decision resume driver, as the actor calls it (plan slice E,
//! O123). Each maintenance turn offers exactly one Run with an open decision
//! wait to the runtime's driver, rotating after the last Run offered so no
//! Run (failing, waiting or busy) monopolizes the turn. The driver chooses the
//! path and takes at most one exact step; the actor only rotates and traces.
//! Enrollment's HTTP call never runs here: the driver reaches it through the
//! fixed publication worker's mailbox, which starts the job on the worker
//! thread only while no Run's document handle is out.

use koru_delegate_core::{BotOwner, RunId};
use koru_delegate_runtime::{DecisionEnrollmentBridge, DecisionResumeStep, Orchestrator, RunError};

use super::Actor;
use crate::publication::{EnrollmentMailbox, PublicationWorker};

/// The orchestrator's half of the resume driver, as one maintenance turn uses
/// it. The orchestrator is the only production implementation; the seam lets
/// the turn's rotation be tested without preparing ledger states.
pub(super) trait DecisionResume {
    /// One bounded rotation of Runs with an open decision wait after `after`.
    fn resume_runs(&mut self, after: Option<RunId>) -> Result<Vec<(BotOwner, RunId)>, RunError>;
    /// At most one exact step for this Run's open wait.
    fn advance(
        &mut self,
        owner: BotOwner,
        run: RunId,
        bridge: &mut dyn DecisionEnrollmentBridge,
    ) -> Result<DecisionResumeStep, RunError>;
}

impl DecisionResume for Orchestrator {
    fn resume_runs(&mut self, after: Option<RunId>) -> Result<Vec<(BotOwner, RunId)>, RunError> {
        self.bot_decision_resume_runs(after)
    }
    fn advance(
        &mut self,
        owner: BotOwner,
        run: RunId,
        bridge: &mut dyn DecisionEnrollmentBridge,
    ) -> Result<DecisionResumeStep, RunError> {
        self.bot_advance_decision_resume(owner, run, bridge)
    }
}

/// What one maintenance turn of the resume driver did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ResumeTurn {
    /// No Run has an open decision wait: the turn did nothing.
    Idle,
    /// The open waits could not be listed; retried on a later turn.
    Unavailable,
    /// This Run was offered and nothing changed: it is waiting (including
    /// `decision_enrollment_pending`), the path is unsupported, the wait
    /// closed meanwhile, or the driver refused. Neither is success.
    Unchanged(RunId),
    /// This Run took this one exact step.
    Stepped(RunId, DecisionResumeStep),
}

/// One turn as a test observes it: whether a Run's document handle was out
/// when the turn began, how many enrollment jobs the worker had been handed
/// before and after it, and what the turn did.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(super) struct ResumeTurnRecord {
    pub handle_out: bool,
    pub jobs_before: usize,
    pub jobs_after: usize,
    pub turn: ResumeTurn,
}

/// One maintenance turn: offer the first Run after `cursor` (wrapping) to the
/// driver, then move the cursor to it whatever the outcome, so an erroring or
/// waiting Run never blocks the next. A per-Run error is traced, never raised:
/// it must not stop the tick, drop a deadline or starve publications.
pub(super) fn decision_resume_turn(
    source: &mut dyn DecisionResume,
    cursor: &mut Option<RunId>,
    worker: &mut PublicationWorker,
) -> ResumeTurn {
    let runs = match source.resume_runs(*cursor) {
        Ok(runs) => runs,
        Err(error) => {
            tracing::debug!(target: "koru::delegate", %error, "decision resume selection unavailable");
            return ResumeTurn::Unavailable;
        }
    };
    let Some(&(owner, run)) = runs.first() else {
        return ResumeTurn::Idle;
    };
    *cursor = Some(run);
    match source.advance(owner, run, &mut EnrollmentMailbox::new(worker)) {
        Ok(DecisionResumeStep::Idle) => ResumeTurn::Unchanged(run),
        Ok(DecisionResumeStep::Waiting(code)) => {
            tracing::debug!(target: "koru::delegate", run = ?run, code, "decision resume waiting");
            ResumeTurn::Unchanged(run)
        }
        Ok(DecisionResumeStep::Unsupported(code)) => {
            tracing::debug!(target: "koru::delegate", run = ?run, code, "decision resume unsupported; the origin keeps running");
            ResumeTurn::Unchanged(run)
        }
        Ok(step) => {
            tracing::debug!(target: "koru::delegate", run = ?run, step = ?step, "decision resume stepped");
            ResumeTurn::Stepped(run, step)
        }
        Err(error) => {
            tracing::warn!(target: "koru::delegate", run = ?run, %error, "decision resume step refused");
            ResumeTurn::Unchanged(run)
        }
    }
}

impl Actor {
    /// One maintenance turn of the resume driver. A step taken lets the loop
    /// offer the next turn without waiting a tick.
    pub(super) fn advance_decision_resume(&mut self) -> ResumeTurn {
        if let Err(error) = self.ensure_publication_worker() {
            tracing::warn!(target: "koru::delegate", %error, "decision resume has no publication worker");
            return ResumeTurn::Unavailable;
        }
        let Some(worker) = self.publication_worker.as_mut() else {
            return ResumeTurn::Unavailable;
        };
        #[cfg(test)]
        let (handle_out, jobs_before) = (worker.ticket().is_some(), worker.enrollments_started());
        let turn = decision_resume_turn(
            &mut self.orchestrator,
            &mut self.decision_resume_cursor,
            worker,
        );
        #[cfg(test)]
        self.resume_log.push(ResumeTurnRecord {
            handle_out,
            jobs_before,
            jobs_after: worker.enrollments_started(),
            turn,
        });
        // Idle stops asking until something opens a wait; anything else keeps
        // the maintenance tick alive for the next turn.
        self.decision_resume_pending = turn != ResumeTurn::Idle;
        turn
    }
}
