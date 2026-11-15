// SPDX-License-Identifier: MPL-2.0

//! The Run's open decision wait, published beside its tool classification
//! (O147; decision contract section 3). While the ledger holds an open
//! decision wait for a Bot Run, that Run classifies as `BotDecisionWait`:
//! reads, retained replay and the exact decision retry stay available and
//! new unrelated mutations are refused by every caller of the classification.
//!
//! The value is read from the ledger only (`decision_wait_execution` and
//! `bot_decision`, both existing reads) and published through the MCP port,
//! which the host's server answers from without the actor. A failed read or
//! publication keeps whatever was published before, so an open wait stays
//! fenced until a later read proves it closed. A no-decision cleanup wait has
//! no decision to name and is not this value; the ledger already refuses
//! fresh native and Memory work during it.

use super::*;
use koru_delegate_core::decisions::DecisionPhase;
use koru_delegate_sqlite::DECISION_RESUME_RUN_PAGE;

/// Pages of open decision waits read once per process to seed the overlay.
/// 50 waits per device plus the occupied two fit in four pages; the bound
/// only stops an unexpected ledger from looping.
const SEED_PAGES: usize = 8;

impl Orchestrator {
    /// The Run's open decision wait as the ledger holds it now.
    fn open_decision_wait(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<(DecisionId, DecisionPhase)>, RunError> {
        let Some(execution) = self.bot_ledger()?.decision_wait_execution(owner, run)? else {
            return Ok(None);
        };
        let Some(expected) = execution.expected.decision else {
            return Ok(None);
        };
        let record = self
            .bot_ledger()?
            .bot_decision(owner, run, expected.decision)?
            .ok_or_else(|| refused("bot_fenced"))?;
        Ok(Some((record.receipt.decision, record.receipt.state.phase)))
    }

    /// Read the Run's open decision wait and publish it (or its absence).
    /// The Run stays tracked until a read proves no wait is open.
    pub fn sync_decision_authority(&mut self, owner: BotOwner, run: RunId) -> Result<(), RunError> {
        self.decision_authority.insert(run, owner);
        let wait = self.open_decision_wait(owner, run)?;
        self.ports.mcp.publish_decision_wait(run, wait)?;
        if wait.is_none() {
            self.decision_authority.remove(&run);
        }
        Ok(())
    }

    /// Re-read the Run's published wait if this process published one; a
    /// Run with no wait is untouched (a decision is opened only by its own
    /// preparation, which publishes it).
    pub fn sync_tracked_decision_authority(&mut self, run: RunId) -> Result<(), RunError> {
        match self.decision_authority.get(&run).copied() {
            Some(owner) => self.sync_decision_authority(owner, run),
            None => Ok(()),
        }
    }

    /// Re-read every published wait, after first seeding (once per process)
    /// every open decision wait the ledger holds, so a restart republishes
    /// the fence. Every Run is attempted; the first failure is returned.
    pub fn sync_decision_authorities(&mut self) -> Result<(), RunError> {
        let mut first = None;
        if !self.decision_authority_seeded {
            match self.seed_decision_authority() {
                Ok(()) => self.decision_authority_seeded = true,
                Err(error) => first = Some(error),
            }
        }
        let runs: Vec<(RunId, BotOwner)> = self
            .decision_authority
            .iter()
            .map(|(run, owner)| (*run, *owner))
            .collect();
        for (run, owner) in runs {
            if let Err(error) = self.sync_decision_authority(owner, run) {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
    }

    fn seed_decision_authority(&mut self) -> Result<(), RunError> {
        let mut after = None;
        for _ in 0..SEED_PAGES {
            let page = self
                .bot_ledger()?
                .decision_resume_runs(after, DECISION_RESUME_RUN_PAGE)?;
            for (owner, run) in &page {
                self.decision_authority.insert(*run, *owner);
            }
            if page.len() < DECISION_RESUME_RUN_PAGE {
                return Ok(());
            }
            after = page.last().map(|(_, run)| *run);
        }
        Err(refused("bot_decision_inventory_unbounded"))
    }
}
