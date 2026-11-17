// SPDX-License-Identifier: MPL-2.0

//! Immutable discovery evidence published before blocking provider creation.
//! Missing after restart is a refusal, never an ordinary-tool fallback.
//!
//! Beside each attempt's immutable launch classification, the actor keeps a
//! per-Run overlay of the ledger's open decision wait (O147). The server
//! answers `ResolveRunAuthority` from here without the actor, which may be
//! blocked creating a provider whose first tool call classifies itself; so
//! the wait is published into the projection rather than read on demand.
//! A Bot Run with an open wait resolves as `BotDecisionWait`; the launch
//! value itself never changes.
use koru_delegate_api::Failure;
use koru_delegate_core::decisions::{DecisionId, DecisionPhase};
use koru_delegate_core::{AttemptGeneration, RunId, RunToolAuthority};
use koru_delegate_runtime::ChildWaitMark;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const CAPACITY: usize = 4096;

#[derive(Default)]
struct Entries {
    launch: HashMap<RunId, (AttemptGeneration, RunToolAuthority)>,
    /// The ledger's open decision wait per Run, as the actor last read it.
    waits: HashMap<RunId, (DecisionId, DecisionPhase)>,
    /// The ledger's open child wait per Run (M6), as the actor last read it.
    child_waits: HashMap<RunId, ChildWaitMark>,
}

#[derive(Clone, Default)]
pub(crate) struct AuthorityProjection(Arc<Mutex<Entries>>);
fn unavailable() -> Failure {
    Failure::new(
        "run_authority_unavailable",
        "The Run tool authority is unavailable.",
        true,
    )
}
impl AuthorityProjection {
    pub fn publish(
        &self,
        run: RunId,
        generation: AttemptGeneration,
        authority: RunToolAuthority,
    ) -> Result<(), Failure> {
        // Only a launch classification is published here; the wait is the
        // overlay's, so a waiting value can never become a launch value.
        if matches!(
            authority,
            RunToolAuthority::BotDecisionWait { .. } | RunToolAuthority::BotChildWait { .. }
        ) {
            return Err(unavailable());
        }
        let mut entries = self.0.lock().map_err(|_| unavailable())?;
        if let Some((old, value)) = entries.launch.get(&run) {
            if *old > generation || (*old == generation && value != &authority) {
                return Err(unavailable());
            }
        } else if entries.launch.len() >= CAPACITY {
            return Err(unavailable());
        }
        entries.launch.insert(run, (generation, authority));
        Ok(())
    }
    /// Set (`Some`) or clear (`None`) the Run's open decision wait, exactly
    /// as the actor read it from the ledger. Bounded like the launch map.
    pub fn publish_wait(
        &self,
        run: RunId,
        wait: Option<(DecisionId, DecisionPhase)>,
    ) -> Result<(), Failure> {
        let mut entries = self.0.lock().map_err(|_| unavailable())?;
        match wait {
            Some(wait) => {
                if !entries.waits.contains_key(&run) && entries.waits.len() >= CAPACITY {
                    return Err(unavailable());
                }
                entries.waits.insert(run, wait);
            }
            None => {
                entries.waits.remove(&run);
            }
        }
        Ok(())
    }
    /// Set (`Some`) or clear (`None`) the Run's open child wait, exactly as
    /// the actor read it from the ledger. Bounded like the launch map.
    pub fn publish_child_wait(
        &self,
        run: RunId,
        wait: Option<ChildWaitMark>,
    ) -> Result<(), Failure> {
        let mut entries = self.0.lock().map_err(|_| unavailable())?;
        match wait {
            Some(wait) => {
                if !entries.child_waits.contains_key(&run) && entries.child_waits.len() >= CAPACITY
                {
                    return Err(unavailable());
                }
                entries.child_waits.insert(run, wait);
            }
            None => {
                entries.child_waits.remove(&run);
            }
        }
        Ok(())
    }
    pub fn resolve(
        &self,
        run: RunId,
        generation: AttemptGeneration,
    ) -> Result<RunToolAuthority, Failure> {
        let entries = self.0.lock().map_err(|_| unavailable())?;
        let launch = entries
            .launch
            .get(&run)
            .filter(|(current, _)| *current == generation)
            .map(|(_, value)| value.clone())
            .ok_or_else(unavailable)?;
        // A Run has one open wait (A5). Should both ever be published, the
        // decision wait is named: both refuse every new mutation.
        Ok(
            match (
                launch,
                entries.waits.get(&run),
                entries.child_waits.get(&run),
            ) {
                (RunToolAuthority::Bot { provenance, policy }, Some((decision, phase)), _) => {
                    RunToolAuthority::BotDecisionWait {
                        provenance,
                        policy,
                        decision: *decision,
                        phase: *phase,
                    }
                }
                (RunToolAuthority::Bot { provenance, policy }, None, Some(child)) => {
                    RunToolAuthority::BotChildWait {
                        provenance,
                        policy,
                        wait: child.wait,
                        phase: child.phase,
                        open_requests: child.open_requests,
                    }
                }
                (launch, _, _) => launch,
            },
        )
    }
    pub fn remove(&self, run: RunId, generation: Option<AttemptGeneration>) {
        if let Ok(mut entries) = self.0.lock() {
            if entries
                .launch
                .get(&run)
                .is_some_and(|(current, _)| generation.is_none_or(|expected| expected == *current))
            {
                entries.launch.remove(&run);
            }
        }
    }
}

#[cfg(test)]
mod tests;
