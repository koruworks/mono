// SPDX-License-Identifier: MPL-2.0
//! Automatic AI interruption records uncertainty without borrowing owner Stop.
use super::*;
use crate::orchestrator::state::Attempt;
use koru_delegate_core::{
    PriorAttempt, RunEvent, StepKind, TimelineEntry, TimelineRef, TimelineRefs,
};
use koru_delegate_sqlite::AttemptPhase;

impl Orchestrator {
    pub fn bot_interrupt(
        &mut self,
        key: &BotRunKey,
        device: DeviceId,
        fence: Option<&BotExecutionFence>,
    ) -> Result<(), RunError> {
        let record = self.bot_record(key, device)?;
        let row = self.ports.store.current_attempt(key.run)?;
        let expected = self.bot_admission_view(&record)?.fence;
        if let Some(fence) = fence {
            if expected.as_ref() != Some(fence) {
                return Err(refused("bot_fenced"));
            }
        } else if row.as_ref().is_some_and(|row| !row.retired) && expected.is_some() {
            return Err(refused("bot_fence_required"));
        }
        let header = self.header(key.run)?;
        if header.run != key.run
            || header
                .bot
                .as_ref()
                .is_none_or(|bot| bot.owner != key.owner || bot.bot != key.bot)
        {
            return Err(refused("bot_fenced"));
        }
        let finished = matches!(
            record.state,
            BotAdmissionState::Settled | BotAdmissionState::Refused
        ) && row.as_ref().is_none_or(|row| row.retired);
        if let Some(row) = &row
            && !finished
        {
            self.open(key.run)?.attempt.get_or_insert_with(|| {
                let mut attempt = Attempt::new(row.attempt, row.generation);
                attempt.is_bot = true;
                attempt.session = row.provider_session.as_ref().and_then(|key| key.session);
                attempt
            });
        }
        if !header.status.is_terminal() && !matches!(header.status, RunStatus::Interrupted { .. }) {
            let refs = TimelineRefs::new(row.as_ref().map(|row| TimelineRef::Attempt {
                attempt: row.attempt,
            }))
            .expect("at most one recorded attempt");
            let entry = TimelineEntry::new(
                StepKind::Interrupted {
                    prior_attempt: PriorAttempt::Unknown,
                },
                refs,
                self.ports.clock.now_ms(),
            );
            self.apply_with_step(
                key.run,
                &RunEvent::Interrupt {
                    prior_attempt: PriorAttempt::Unknown,
                },
                &entry,
            )?;
            // Published once, with the transition it describes; a retry finds
            // the absorbing status and publishes nothing. It travels only after
            // the document is durable. A queued Run has no attempt to name.
            if let Some(row) = &row {
                self.publish(
                    key.run,
                    crate::RunFact::Interrupted {
                        run: key.run,
                        attempt: row.attempt,
                        generation: row.generation,
                        prior_attempt: PriorAttempt::Unknown,
                    },
                )?;
            }
        }
        // The existing canonical acknowledgment guard precedes any lifecycle
        // settlement, including the positive no-attempt queued case.
        self.bot_settle_before_retirement(key.run)?;
        self.ports.mcp.revoke(key.run);
        if finished {
            return Ok(());
        }
        if let Some(row) = row {
            // Unknown, not Interrupted: nothing here proves the process ended,
            // and a person may still hold it. Restart uses the same phase.
            if !row.phase.is_terminal()
                && matches!(self.status(key.run)?, RunStatus::Interrupted { .. })
            {
                self.ports
                    .store
                    .advance(key.run, row.generation, AttemptPhase::Unknown)?;
            }
            // As at restart: the terminal ledger phase fences the active
            // clock, and the existing cleanup deadline owns this attempt now.
            self.arm_bot_cleanup(key.run)?;
            let settled = self
                .bot_ledger()?
                .bot_execution(key.run)?
                .is_some_and(|record| {
                    matches!(
                        record.state,
                        BotAdmissionState::Settled | BotAdmissionState::Refused
                    )
                });
            if !settled {
                return Err(refused("bot_interrupt_pending"));
            }
        } else if !matches!(
            record.state,
            BotAdmissionState::Settled | BotAdmissionState::Refused
        ) {
            self.bot_ledger()?
                .settle_bot(key.run, BotSettlement::Settled)?;
        }
        Ok(())
    }
}
