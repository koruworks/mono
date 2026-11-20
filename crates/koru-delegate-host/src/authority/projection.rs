// SPDX-License-Identifier: MPL-2.0

//! Positive exact-attempt evidence, read without waiting on provider creation.
use koru_delegate_api::Failure;
use koru_delegate_core::{AttemptGeneration, RunId};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
pub(crate) struct AuthorityProjection(Arc<Mutex<HashMap<RunId, AttemptGeneration>>>);

fn unavailable() -> Failure {
    Failure::new(
        "run_authority_unavailable",
        "The Run tool authority is unavailable.",
        true,
    )
}

impl AuthorityProjection {
    pub fn publish(&self, run: RunId, generation: AttemptGeneration) -> Result<(), Failure> {
        let mut entries = self.0.lock().map_err(|_| unavailable())?;
        if entries.get(&run).is_some_and(|old| *old > generation)
            || (!entries.contains_key(&run) && entries.len() >= 4096)
        {
            return Err(unavailable());
        }
        entries.insert(run, generation);
        Ok(())
    }

    pub fn resolve(&self, run: RunId, generation: AttemptGeneration) -> Result<(), Failure> {
        let entries = self.0.lock().map_err(|_| unavailable())?;
        if entries.get(&run) == Some(&generation) {
            Ok(())
        } else {
            Err(unavailable())
        }
    }

    pub fn remove(&self, run: RunId) {
        if let Ok(mut entries) = self.0.lock() {
            entries.remove(&run);
        }
    }
}

#[cfg(test)]
mod tests;
