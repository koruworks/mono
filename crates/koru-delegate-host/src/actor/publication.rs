// SPDX-License-Identifier: MPL-2.0

//! The actor is the SQL owner; one fixed worker owns the sole document handle
//! across prepare, the exact writer receipt and apply. No logical timeout frees
//! the physical ticket. There is no caller-provided target or publication body.

use super::*;
use crate::publication::{Completion, EnrollmentClient, PublicationWorker};
use koru_delegate_runtime::SourceProgress;

impl Actor {
    /// Start the one fixed worker now, carrying the host's decision
    /// enrollment client onto its thread. Enrollment then runs there, only
    /// while no Run's document handle is out, under the worker's single
    /// ticket. Called once at composition, before the loop runs.
    pub(crate) fn attach_decision_bridge(
        &mut self,
        client: EnrollmentClient,
    ) -> std::io::Result<()> {
        if self.publication_worker.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "the publication worker already started",
            ));
        }
        self.publication_worker = Some(PublicationWorker::start_with_enrollment(
            Self::publication_wake(self.commands.clone()),
            client,
        )?);
        Ok(())
    }

    /// Owned sender in, so the wake borrows nothing of the actor.
    fn publication_wake(commands: mpsc::Sender<Command>) -> impl Fn() + Send + 'static {
        move || {
            let _ = commands.send(Command::PublicationWake);
        }
    }

    /// The composed host attaches its worker eagerly. An actor without an
    /// enrollment client (no decision bridge composed) gets a worker on first
    /// use whose enrollment answers `decision_enrollment_unavailable`.
    pub(super) fn ensure_publication_worker(&mut self) -> Result<(), RunError> {
        if self.publication_worker.is_none() {
            self.publication_worker = Some(
                PublicationWorker::start(Self::publication_wake(self.commands.clone())).map_err(
                    |_| {
                        PortError::new(
                            koru_delegate_runtime::Port::Documents,
                            "publication_worker_unavailable",
                            "the publication worker could not start",
                        )
                    },
                )?,
            );
        }
        Ok(())
    }

    fn start_run_publication(&mut self, run: RunId) -> Result<bool, RunError> {
        if self.publication_ack.is_some()
            || self
                .publication_worker
                .as_ref()
                .is_some_and(|worker| worker.ticket().is_some())
        {
            return Err(koru_delegate_runtime::publication_pending().into());
        }
        self.ensure_publication_worker()?;
        let Some(start) = self.orchestrator.next_run_publication(run)? else {
            return Ok(false);
        };
        // Invalidate already queued ordinary updates before the handle leaves.
        // The decision producer resumes from the separately retained frontiers.
        if let Some(pump) = self.pumps.remove(&run) {
            pump.stop.store(true, Ordering::Release);
        }
        if let Err(start) = self.publication_worker.as_mut().unwrap().begin(start) {
            self.orchestrator.cancel_unstarted_publication(start);
            return Err(koru_delegate_runtime::publication_pending().into());
        }
        Ok(true)
    }

    pub(super) fn drain_publication_completions(&mut self) {
        // Endpoint protocol permits only one completion per request. A second
        // iteration can observe an immediate follow-on completion, never drain
        // an unbounded queue or run another Run's workflow inline.
        for _ in 0..2 {
            let completion = self
                .publication_worker
                .as_mut()
                .and_then(|worker| worker.completion());
            let Some(completion) = completion else { break };
            self.publication_completed(completion, false);
        }
        self.drive_publication();
        // After the drain (an enrollment reply also wakes the actor here):
        // one Run's decision wait per turn, rotating.
        self.advance_decision_resume();
    }

    /// One maintenance step: start at most one physical workflow, then offer
    /// producers to Runs whose journal has drained. A wake alone frees no
    /// ticket; this only acts on what the orchestrator positively reports.
    pub(super) fn drive_publication(&mut self) {
        self.schedule_publication();
        self.settle_publication_waits();
        self.connect_source_producers();
    }

    /// A drained Run whose wait C may now settle leaves exclusive ownership
    /// and resumes its ordinary stream; admission and capacity are untouched.
    fn settle_publication_waits(&mut self) {
        for run in self.orchestrator.publication_candidates() {
            match self.orchestrator.settle_publication_wait(run) {
                Ok(Some(started)) => self.pump(started),
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(target: "koru::delegate", run = ?run, %error, "decision wait not settled");
                }
            }
        }
    }

    /// Rotate through the retained exclusive inventory, at most one visit per
    /// Run per step, starting after the last Run served so a failing or busy
    /// Run cannot monopolize the single ticket.
    fn schedule_publication(&mut self) {
        if self.publication_ack.is_some()
            || self
                .publication_worker
                .as_ref()
                .is_some_and(|worker| worker.ticket().is_some())
        {
            return;
        }
        // A Run whose last workflow failed waits one ordinary TICK; others
        // keep rotating meanwhile, and its handle stays home for the resume
        // driver and enrollment. Expired entries leave the map.
        let now = Instant::now();
        self.publication_retry_at.retain(|_, at| *at > now);
        // Durable open projections come from the bounded keyset reader (also
        // after a restart, with no RunState); observations from memory.
        let candidates = match self.orchestrator.publication_runs(self.publication_cursor) {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::debug!(target: "koru::delegate", %error, "publication selection unavailable");
                return;
            }
        };
        for run in candidates {
            if self.publication_retry_at.contains_key(&run) {
                continue;
            }
            match self.start_run_publication(run) {
                Ok(true) => {
                    self.publication_cursor = Some(run);
                    return;
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::debug!(target: "koru::delegate", run = ?run, %error, "publication remains owed");
                }
            }
            if self
                .publication_worker
                .as_ref()
                .is_some_and(|worker| worker.ticket().is_some())
            {
                return;
            }
        }
    }

    /// A drained exclusive Run reconnects from its exact accepted frontier
    /// pair with a new epoch. Runs already pumped are left alone.
    fn connect_source_producers(&mut self) {
        for run in self.orchestrator.publication_candidates() {
            if self.pumps.contains_key(&run) {
                continue;
            }
            match self.orchestrator.subscribe_source_producer(run) {
                Ok(Some((epoch, events))) => self.decision_pump(run, epoch, events),
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(target: "koru::delegate", run = ?run, %error, "decision producer not connected");
                }
            }
        }
    }

    fn publication_completed(&mut self, completion: Completion, shutting_down: bool) {
        match completion {
            Completion::Prepared { ticket, prepared } => {
                if shutting_down {
                    return;
                }
                // A failed/unknown SQL receipt cannot authorize import. Return
                // the actual handle, then re-read the durable row on recovery.
                let retained = match self.orchestrator.record_run_preparation(ticket, &prepared) {
                    Ok(prepared) => Some(prepared),
                    Err(error) => {
                        tracing::warn!(target: "koru::delegate", %error, "publication preparation receipt remains unknown");
                        self.defer_publication(ticket.run);
                        None
                    }
                };
                if let Some(worker) = self.publication_worker.as_mut() {
                    let result = match retained {
                        Some(prepared) => worker.apply(ticket, prepared),
                        // The unrecorded candidate must not stay staged on
                        // the handle: the re-read may select another
                        // transition, which a stale candidate would refuse.
                        None => worker.return_unrecorded(ticket, prepared.identity.transition),
                    };
                    if let Err(error) = result {
                        tracing::warn!(target: "koru::delegate", code = error.code, "publication ownership remains unresolved");
                    }
                }
            }
            Completion::Applied {
                ticket,
                proof,
                handle,
            } => {
                self.publication_retry_at.remove(&ticket.run);
                if self.return_publication_handle(ticket, handle) {
                    // A writer failure retains this checked observation and the
                    // exclusive attached handle. It never becomes fresh authority.
                    if self
                        .orchestrator
                        .acknowledge_run_projection(&proof)
                        .is_err()
                    {
                        self.publication_ack = Some(proof);
                    }
                }
            }
            Completion::Returned {
                ticket,
                handle,
                error,
            } => {
                self.return_publication_handle(ticket, handle);
                if let Some(error) = error {
                    tracing::warn!(target: "koru::delegate", code = error.code, "publication remains owed");
                    self.defer_publication(ticket.run);
                }
            }
            Completion::Pending { ticket, error } => {
                tracing::warn!(target: "koru::delegate", code = error.code, "publication completion is unknown");
                self.defer_publication(ticket.run);
                if !shutting_down && let Some(worker) = self.publication_worker.as_mut() {
                    // Preserve handle-local staging and retained SQL bytes.
                    let _ = worker.return_handle(ticket);
                }
            }
            Completion::Observed {
                ticket,
                handle,
                observation,
            } => {
                // The returned handle installs first; the bounded answer then
                // fills only the one exact owed key, or retains a refusal.
                if let Err(handle) =
                    self.orchestrator
                        .accept_source_observation(ticket, handle, observation)
                {
                    drop(handle);
                }
            }
            Completion::Lost { ticket } => {
                self.orchestrator.lost_run_publication(ticket);
                // A cold open that keeps failing reports Lost every time.
                self.defer_publication(ticket.run);
            }
        }
    }

    /// The decision producer: one outstanding frame per permit. The actor
    /// grants the next permit only after it accepted a no-op frame; a retained
    /// target or obligation drops the subscription instead.
    fn decision_pump(
        &mut self,
        run: RunId,
        epoch: uuid::Uuid,
        mut events: Box<dyn koru_delegate_runtime::SessionEvents>,
    ) {
        self.stop_pump(run);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let (grant, permits) = mpsc::sync_channel::<()>(1);
        let commands = self.commands.clone();
        let spawned = std::thread::Builder::new()
            .name("koru-delegate decision producer".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                'frames: while !flag.load(Ordering::Acquire) {
                    let Some(update) = events.next(PUMP_POLL) else {
                        continue;
                    };
                    let closed = matches!(update, SessionUpdate::Closed { .. });
                    if commands
                        .send(Command::Update { run, epoch, update })
                        .is_err()
                        || closed
                    {
                        break;
                    }
                    loop {
                        if flag.load(Ordering::Acquire) {
                            break 'frames;
                        }
                        match permits.recv_timeout(PUMP_POLL) {
                            Ok(()) => break,
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                            Err(mpsc::RecvTimeoutError::Disconnected) => break 'frames,
                        }
                    }
                }
                let _ = commands.send(Command::PumpEnded { run, epoch });
            });
        match spawned {
            Ok(_) => {
                self.pumps.insert(
                    run,
                    Pump {
                        stop,
                        epoch,
                        permit: Some(grant),
                    },
                );
            }
            Err(error) => {
                tracing::error!(target: "koru::delegate", run = ?run, %error, "the decision producer could not be started");
            }
        }
        self.count_active();
    }

    /// One frame from the decision producer, already epoch-checked.
    pub(super) fn decision_update(&mut self, run: RunId, epoch: uuid::Uuid, update: SessionUpdate) {
        match self.orchestrator.handle_from_producer(run, epoch, update) {
            Ok(SourceProgress::Continue | SourceProgress::Ordinary(HandleOutcome::Continue)) => {
                if let Some(permit) = self.pumps.get(&run).and_then(|pump| pump.permit.as_ref()) {
                    let _ = permit.try_send(());
                }
            }
            Ok(SourceProgress::Ordinary(HandleOutcome::Ended)) => self.stop_pump(run),
            Ok(SourceProgress::Paused) => {
                self.stop_pump(run);
                self.drive_publication();
            }
            Err(error) => {
                // Nothing past the accepted frontier was consumed; reconnect
                // happens only when the orchestrator offers a producer again.
                tracing::debug!(target: "koru::delegate", run = ?run, %error, "a decision frame was not accepted");
                self.stop_pump(run);
            }
        }
    }

    /// A failed or unknown workflow (refused receipt, refused prepare or open,
    /// unknown apply) is not started again for that Run until one ordinary
    /// TICK has passed. Without this a persistent failure restarted on every
    /// worker wake, spinning the actor thread and keeping the handle out of
    /// the resume driver's reach. The loop's existing TICK deadline brings it
    /// back; nothing is persisted and no timer is added.
    fn defer_publication(&mut self, run: RunId) {
        self.publication_retry_at.insert(run, Instant::now() + TICK);
    }

    fn return_publication_handle(
        &mut self,
        ticket: koru_delegate_runtime::RunPublicationTicket,
        handle: Box<dyn koru_delegate_runtime::RunDocumentHandle>,
    ) -> bool {
        match self.orchestrator.return_run_publication(ticket, handle) {
            Ok(()) => true,
            Err(handle) => {
                // A late epoch never replaces the current canonical owner.
                // Only this physically returned, unmatched handle is discarded.
                drop(handle);
                false
            }
        }
    }

    pub(super) fn retry_publication_ack(&mut self) -> bool {
        let Some(proof) = self.publication_ack.as_ref() else {
            return true;
        };
        if self.orchestrator.acknowledge_run_projection(proof).is_err() {
            return false;
        }
        self.publication_ack = None;
        true
    }

    pub(super) fn shutdown_publication(&mut self) {
        let completions = self
            .publication_worker
            .as_mut()
            .map(PublicationWorker::shutdown)
            .unwrap_or_default();
        for completion in completions {
            // Shutdown has already completed physically. Prepared/Pending is
            // followed by the actual Returned handle in the same bounded list.
            self.publication_completed(completion, true);
        }
        self.retry_publication_ack();
    }
}
