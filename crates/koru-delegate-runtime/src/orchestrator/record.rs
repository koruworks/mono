// SPDX-License-Identifier: MPL-2.0

//! The one path every fact takes: into the Run document, then into the
//! pending-publication queue, then onto the bus, then acknowledged.
//!
//! §17.1's order is the correction this module holds: *the document
//! first*. A step or a usage fact is committed durably in the run's own
//! storage before anything travels, the queue row is written under a
//! stable [`EventId`] before the bus is asked, and the row is acknowledged
//! only when the bus accepted it. A lost answer is retried under the same
//! id — [`Orchestrator::retry_publications`] — and the journal dedupes.

use koru_delegate_core::{
    EventId, RunEvent, RunId, RunStatus, StepKind, TimelineEntry, TimelineRefs, transition,
    transition_bot,
};

use crate::{RunFact, orchestrator::RunError};

use super::Orchestrator;

use koru_delegate_sqlite::{PendingPublicationRunCursor, PendingPublicationRunSeek};

/// Volatile traversal only. Durable rows remain the backlog; no body or Run
/// collection is retained here. The anchor is a live key bound, not a snapshot.
#[derive(Default)]
pub(super) struct PublicationRetry {
    cursor: Option<PendingPublicationRunCursor>,
    pass: Option<PublicationPass>,
}
struct PublicationPass {
    anchor: PendingPublicationRunCursor,
    stop: Option<PendingPublicationRunCursor>,
    after: Option<PendingPublicationRunCursor>,
    wrapped: bool,
}

impl Orchestrator {
    /// The run's status as the document holds it.
    pub(crate) fn status(&mut self, run: RunId) -> Result<RunStatus, RunError> {
        Ok(self.open(run)?.doc.document()?.header()?.status)
    }

    /// Move the status through the one closed function and commit it.
    pub(crate) fn apply(&mut self, run: RunId, event: &RunEvent) -> Result<RunStatus, RunError> {
        self.apply_with_failure_reason(run, event, None)
    }

    pub(crate) fn apply_with_failure_reason(
        &mut self,
        run: RunId,
        event: &RunEvent,
        reason: Option<koru_delegate_core::RunFailureReason>,
    ) -> Result<RunStatus, RunError> {
        self.apply_recorded_event(run, event, reason, None)
    }

    pub(crate) fn apply_with_step(
        &mut self,
        run: RunId,
        event: &RunEvent,
        step: &TimelineEntry,
    ) -> Result<RunStatus, RunError> {
        self.apply_recorded_event(run, event, None, Some(step))
    }

    fn apply_recorded_event(
        &mut self,
        run: RunId,
        event: &RunEvent,
        reason: Option<koru_delegate_core::RunFailureReason>,
        step: Option<&TimelineEntry>,
    ) -> Result<RunStatus, RunError> {
        if matches!(event, RunEvent::BotResponseDelivered) {
            return Err(super::bots::refused("bot_delivery_owner_required"));
        }
        let header = self.header(run)?;
        let from = header.status;
        let next = if header.bot.is_some() {
            transition_bot(&from, event)?
        } else {
            transition(&from, event)?
        };
        let now = self.ports.clock.now_ms();
        let state = self.open(run)?;
        let failure = if let RunStatus::Failed { stage } = &next {
            let refs = TimelineRefs::new(
                state
                    .attempt_ids()
                    .map(|(attempt, _)| koru_delegate_core::TimelineRef::Attempt { attempt }),
            )
            .expect("at most one ref");
            Some(TimelineEntry::new(
                StepKind::Failed {
                    stage: *stage,
                    reason,
                },
                refs,
                now,
            ))
        } else {
            None
        };
        let status = next.clone();
        state.doc.commit(&mut |document| {
            document.set_status_with_failure_reason(&status, reason)?;
            if let Some(entry) = &failure {
                document.append_step(entry)?;
            }
            if let Some(entry) = step {
                document.append_step(entry)?;
            }
            Ok(())
        })?;
        self.trace(
            run,
            "status",
            format_args!("{} -> {} on {}", from.name(), next.name(), event.name()),
        );
        // The run is over: close its endpoint (§7.3). Here rather than
        // beside the session's termination because this is the one funnel
        // every terminal status passes through — completed, failed and
        // cancelled, whether an attempt was open or not — and because a
        // provider that outlives the worker is exactly what the revocation
        // is for: the record is read on every call, so the endpoint stops
        // answering it even though nothing told it to stop.
        if !from.is_terminal() && next.is_terminal() {
            self.ports.mcp.revoke(run);
        }
        Ok(next)
    }

    /// Append one lean step, then publish the fact that describes it.
    pub(crate) fn step(
        &mut self,
        run: RunId,
        kind: StepKind,
        refs: TimelineRefs,
        fact: Option<RunFact>,
    ) -> Result<(), RunError> {
        let now = self.ports.clock.now_ms();
        let entry = TimelineEntry::new(kind, refs, now);
        let state = self.open(run)?;
        state
            .doc
            .commit(&mut |document| document.append_step(&entry))?;
        self.trace(run, "step", format_args!("{kind:?}"));
        if let Some(fact) = fact {
            self.publish(run, fact)?;
        }
        Ok(())
    }

    /// Publish a fact under a fresh id: queue, bus, acknowledgement.
    pub(crate) fn publish(&mut self, run: RunId, fact: RunFact) -> Result<EventId, RunError> {
        let event = EventId::mint();
        self.publish_as(run, event, &fact)?;
        Ok(event)
    }

    /// Publish a fact under an id already chosen — a usage retry reuses the
    /// id its first delivery carried (§17.2).
    pub(crate) fn publish_as(
        &mut self,
        run: RunId,
        event: EventId,
        fact: &RunFact,
    ) -> Result<(), RunError> {
        let body = serde_json::to_string(fact).map_err(|_| {
            crate::PortError::new(
                crate::Port::Events,
                "unencodable",
                "the fact did not encode",
            )
        })?;
        let queued = self
            .ports
            .store
            .enqueue(event, run, fact.publication_kind(), &body)?;
        // §17.1: the document first, *durably*. The row is queued under its
        // id either way; it travels now if the Store has already called the
        // commit durable, and from `retry_publications` once it has.
        if self.document_durable(run) {
            if queued == koru_delegate_sqlite::WriteOutcome::AlreadyApplied
                && self
                    .ports
                    .store
                    .publication(event)?
                    .is_some_and(|row| row.acknowledged_at_ms.is_some())
            {
                // A repeated usage report replays the already accepted id.
                // Its first delivery is ordered; the journal deduplicates it.
                self.deliver(event, fact)?;
                return Ok(());
            }
            // A newly durable fact must not overtake earlier facts queued
            // while the document was saving. Immediate sends and retries use
            // the same bounded FIFO drain; remaining work stays on the queue.
            let open = self.ports.store.open_publications_for(run, 64)?;
            self.drain_publications(open)?;
        } else {
            self.trace(
                run,
                "publish",
                format_args!("{} queued until durable", fact.name()),
            );
        }
        Ok(())
    }

    /// Whether the run's document has nothing in flight. A run this device
    /// does not hold open has no commit of its own to wait for.
    fn document_durable(&self, run: RunId) -> bool {
        self.runs.get(&run).is_none_or(|state| state.doc.durable())
    }

    /// The bus half: deliver, and acknowledge only on acceptance.
    fn deliver(&mut self, event: EventId, fact: &RunFact) -> Result<bool, RunError> {
        match self.ports.events.publish(event, fact) {
            Ok(()) => {
                self.ports.store.acknowledge(event)?;
            }
            Err(failure) => {
                let attempts = self.ports.store.bump_attempts(event)?;
                if failure.retryable {
                    tracing::info!(
                        target: "koru::delegate",
                        fact = fact.name(),
                        attempts,
                        "a run fact could not be published; it stays queued"
                    );
                    return Ok(false);
                } else {
                    // The bus will never take it. An open row that can never
                    // close would grow the queue forever; the fact is still in
                    // the Run document, which is the authority.
                    tracing::warn!(
                        target: "koru::delegate",
                        fact = fact.name(),
                        "a run fact was refused by the bus and is acknowledged unpublished"
                    );
                    self.ports.store.acknowledge(event)?;
                }
            }
        }
        Ok(true)
    }

    /// Rotate across pending Runs, retaining FIFO within each Run. At most 53
    /// indexed seeks and one 64-row/4 MiB body prefix per call. Unavailable
    /// exclusive owners cannot hide another Run behind the old global LIMIT.
    pub fn retry_publications(&mut self, limit: usize) -> Result<usize, RunError> {
        if limit == 0 {
            return Ok(0);
        }
        let mut refreshed = false;
        for _ in 0..53 {
            if self.publication_retry.pass.is_none() {
                let Some(anchor) = self.ports.store.pending_publication_run_anchor()? else {
                    return Ok(0);
                };
                self.publication_retry.pass = Some(PublicationPass {
                    anchor,
                    stop: self.publication_retry.cursor,
                    after: self.publication_retry.cursor,
                    wrapped: false,
                });
            }
            let pass = self
                .publication_retry
                .pass
                .as_ref()
                .expect("initialized above");
            let seek = self.ports.store.next_pending_publication_run(
                pass.after,
                pass.anchor,
                if pass.wrapped { pass.stop } else { None },
            )?;
            match seek {
                PendingPublicationRunSeek::Candidate { cursor, run } => {
                    self.publication_retry.cursor = Some(cursor);
                    self.publication_retry.pass.as_mut().unwrap().after = Some(cursor);
                    // This tests actual durability, not membership of the cache:
                    // attached historical readers do not consume exclusion slots.
                    if !self.document_durable(run) {
                        continue;
                    }
                    let open = self
                        .ports
                        .store
                        .decision_open_publications_for(run, limit.min(64))?;
                    // A short byte-bounded prefix is not a drain observation.
                    // The next rotation reads the same Run again if debt remains.
                    return self.drain_publications(open);
                }
                PendingPublicationRunSeek::Malformed { cursor } => {
                    self.publication_retry.pass.as_mut().unwrap().after = Some(cursor);
                    // Preserve malformed debt and the last valid restart cursor,
                    // but do not let this key hide later keys in the active pass.
                    tracing::warn!(target: "koru::delegate", "malformed publication Run key remains queued");
                }
                PendingPublicationRunSeek::Exhausted => {
                    let pass = self.publication_retry.pass.as_mut().unwrap();
                    if !pass.wrapped && pass.stop.is_some() {
                        pass.after = None;
                        pass.wrapped = true;
                    } else {
                        self.publication_retry.pass = None;
                        return Ok(0);
                    }
                }
                PendingPublicationRunSeek::StaleCursor => {
                    self.publication_retry.pass = None;
                    if refreshed {
                        // A stale lower bound must be discarded, but never restart
                        // a scan repeatedly within the same actor turn.
                        self.publication_retry.cursor = None;
                        return Ok(0);
                    }
                    refreshed = true;
                }
            }
        }
        Ok(0)
    }

    fn drain_publications(
        &mut self,
        open: Vec<koru_delegate_sqlite::OpenPublication>,
    ) -> Result<usize, RunError> {
        let mut delivered = 0;
        let mut blocked = std::collections::HashSet::new();
        for publication in open {
            // Not yet: the document this fact describes is still in flight.
            if blocked.contains(&publication.run) || !self.document_durable(publication.run) {
                continue;
            }
            let Ok(fact) = serde_json::from_str::<RunFact>(publication.body.as_str()) else {
                // A body this build cannot read is one it cannot send.
                self.ports.store.acknowledge(publication.event)?;
                continue;
            };
            if !self.deliver(publication.event, &fact)? {
                // Preserve this run's causal order without holding up others.
                blocked.insert(publication.run);
            }
            delivered += 1;
        }
        Ok(delivered)
    }
}
