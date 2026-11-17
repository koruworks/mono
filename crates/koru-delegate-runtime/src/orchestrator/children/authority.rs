// SPDX-License-Identifier: MPL-2.0

//! The Run's open child wait, published beside its tool classification
//! (contract 4.6, extending O147). From the ask until the successor's
//! continuation input is acknowledged a parent Run classifies as
//! `BotChildWait`: reads and the same ask stay available, every new mutation
//! is refused by each caller of the classification.
//!
//! The fence is defined by the resume row, not by the wait's phase: after
//! the acknowledgment the wait row may stay open briefly while its last
//! publication is owed, and the Run is already `Bot` again. A failed read or
//! publication keeps whatever was published before.

use super::*;
use crate::ChildWaitMark;
use koru_delegate_core::decisions::{BotWaitPhase, DecisionResumeState};

impl Orchestrator {
    fn open_child_mark(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<ChildWaitMark>, RunError> {
        let Some(execution) = self.bot_ledger()?.child_wait_execution(owner, run)? else {
            return Ok(None);
        };
        let lifted = execution.wait.phase == BotWaitPhase::ResumeReserved
            && execution.wait.cleanup.is_none()
            && execution.resume.as_ref().is_some_and(|resume| {
                matches!(resume.state, DecisionResumeState::InputAcknowledged { .. })
            });
        Ok((!lifted).then_some(ChildWaitMark {
            wait: execution.wait.wait,
            phase: execution.wait.phase,
            open_requests: execution.open_requests,
        }))
    }

    /// Read the Run's open child wait and publish it (or its absence). The
    /// Run stays tracked until a read proves no fence is owed.
    pub fn sync_child_authority(&mut self, owner: BotOwner, run: RunId) -> Result<(), RunError> {
        self.children.published.insert(run, owner);
        let mark = self.open_child_mark(owner, run)?;
        self.ports.mcp.publish_child_wait(run, mark)?;
        if mark.is_none() {
            self.children.published.remove(&run);
        }
        Ok(())
    }

    /// Re-read every published child wait. Every Run is attempted; the first
    /// failure is returned. Waits a previous process left open are tracked
    /// again when the driver's selector first offers them.
    pub fn sync_child_authorities(&mut self) -> Result<(), RunError> {
        let runs: Vec<(RunId, BotOwner)> = self
            .children
            .published
            .iter()
            .map(|(run, owner)| (*run, *owner))
            .collect();
        let mut first = None;
        for (run, owner) in runs {
            if let Err(error) = self.sync_child_authority(owner, run) {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
    }
}
