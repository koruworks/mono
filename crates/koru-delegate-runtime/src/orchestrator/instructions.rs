// SPDX-License-Identifier: MPL-2.0

//! Volatile instruction snapshots stay stable across uncertain input retries.
//! A new attempt or successor reservation captures current instructions once.
//! After process loss, the durable input fingerprint still refuses changed bytes.

use koru_delegate_core::RunId;
use koru_delegate_preamble::Resolved;

use super::{Orchestrator, RunError};

#[derive(Default)]
pub(crate) struct Snapshot(Option<(String, Resolved)>);

impl Snapshot {
    fn get(&self, scope: &str) -> Option<Resolved> {
        self.0
            .as_ref()
            .filter(|(key, _)| key == scope)
            .map(|(_, source)| source.clone())
    }

    fn keep(&mut self, scope: String, source: Resolved) -> Resolved {
        self.0 = Some((scope, source.clone()));
        source
    }
}

impl Orchestrator {
    pub(super) fn session_instructions(
        &mut self,
        run: RunId,
        scope: String,
    ) -> Result<Resolved, RunError> {
        if let Some(source) = self.open(run)?.instructions.get(&scope) {
            return Ok(source);
        }
        let source = self.ports.preamble.orientation();
        if let Some(notice) = source.notice {
            tracing::warn!(target: "koru::delegate", source = ?source.source, ?notice,
                "session instructions unavailable; using compiled default");
        }
        Ok(self.open(run)?.instructions.keep(scope, source))
    }
}

#[cfg(test)]
mod tests;
