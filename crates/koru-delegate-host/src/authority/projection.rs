// SPDX-License-Identifier: MPL-2.0

//! Immutable discovery evidence published before blocking provider creation.
//! Missing after restart is a refusal, never an ordinary-tool fallback.
use koru_delegate_api::Failure;
use koru_delegate_core::{AttemptGeneration, RunId, RunToolAuthority};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const CAPACITY: usize = 4096;
#[derive(Clone, Default)]
pub(crate) struct AuthorityProjection(
    Arc<Mutex<HashMap<RunId, (AttemptGeneration, RunToolAuthority)>>>,
);
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
        let mut entries = self.0.lock().map_err(|_| unavailable())?;
        if let Some((old, value)) = entries.get(&run) {
            if *old > generation || (*old == generation && value != &authority) {
                return Err(unavailable());
            }
        } else if entries.len() >= CAPACITY {
            return Err(unavailable());
        }
        entries.insert(run, (generation, authority));
        Ok(())
    }
    pub fn resolve(
        &self,
        run: RunId,
        generation: AttemptGeneration,
    ) -> Result<RunToolAuthority, Failure> {
        self.0
            .lock()
            .map_err(|_| unavailable())?
            .get(&run)
            .filter(|(current, _)| *current == generation)
            .map(|(_, value)| value.clone())
            .ok_or_else(unavailable)
    }
    pub fn remove(&self, run: RunId, generation: Option<AttemptGeneration>) {
        if let Ok(mut entries) = self.0.lock() {
            if entries
                .get(&run)
                .is_some_and(|(current, _)| generation.is_none_or(|expected| expected == *current))
            {
                entries.remove(&run);
            }
        }
    }
}

#[cfg(test)]
mod tests;
