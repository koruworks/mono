// SPDX-License-Identifier: MPL-2.0
//! Retry positive terminal cleanup through the actor's existing Bot deadline.
use super::*;
use crate::orchestrator::state::{Attempt, BotCleanupRetry};
use std::time::{Duration, Instant};

impl Orchestrator {
    pub(crate) fn arm_bot_cleanup(&mut self, run: RunId) -> Result<bool, RunError> {
        let header = self.header(run)?;
        if header.bot.is_none()
            || !matches!(
                header.status,
                RunStatus::Interrupted { .. }
                    | RunStatus::Completed
                    | RunStatus::Failed { .. }
                    | RunStatus::Cancelled
            )
        {
            return Ok(false);
        }
        let Some(row) = self.ports.store.current_attempt(run)? else {
            return Ok(false);
        };
        let Some(record) = self.bot_ledger()?.bot_execution(run)? else {
            return Ok(false);
        };
        if record.state == BotAdmissionState::Refused {
            return Ok(false);
        }
        let state = self.open(run)?;
        if record.state == BotAdmissionState::Settled && state.attempt.is_none() {
            return Ok(false);
        }
        let attempt = state.attempt.get_or_insert_with(|| {
            let mut attempt = Attempt::new(row.attempt, row.generation);
            attempt.is_bot = true;
            attempt.session = row.provider_session.and_then(|key| key.session);
            attempt
        });
        attempt.bot_cleanup.get_or_insert(BotCleanupRetry {
            due: Instant::now(),
            round: 0,
        });
        Ok(true)
    }

    /// Some means terminal cleanup owns this attempt; budget cancellation must
    /// not turn its absorbing Interrupted/Unknown status into Cancelled.
    pub(crate) fn bot_cleanup_tick(&mut self, run: RunId) -> Result<Option<bool>, RunError> {
        if !self.arm_bot_cleanup(run)? {
            return Ok(None);
        }
        let now = Instant::now();
        if self
            .attempt_mut(run)?
            .bot_cleanup
            .as_ref()
            .is_some_and(|retry| retry.due > now)
        {
            return Ok(Some(false));
        }
        let result = (|| -> Result<bool, RunError> {
            let progressed = self.reconcile_verified_native_for_run(run)?;
            if self.native_effects_pending(run)? {
                if progressed {
                    self.attempt_mut(run)?
                        .bot_cleanup
                        .as_mut()
                        .expect("armed")
                        .round = 0;
                }
                return Ok(false);
            }
            let ended = if self.status(run)? == RunStatus::Cancelled {
                self.bot_prove_end(run, true)?
            } else {
                self.bot_prove_end_owned(run)?
            };
            if !ended {
                return Ok(false);
            }
            self.ports.mcp.revoke(run);
            self.bot_end_attempt(run, koru_delegate_sqlite::AttemptPhase::Ended, false)?;
            Ok(true)
        })();
        if !matches!(result, Ok(true)) {
            if let Some(retry) = self.attempt_mut(run)?.bot_cleanup.as_mut() {
                retry.due = now + Duration::from_secs(1u64 << retry.round.min(5));
                retry.round = (retry.round + 1).min(5);
            }
        }
        result.map(Some)
    }
}
