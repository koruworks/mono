// SPDX-License-Identifier: MPL-2.0

//! The actor is the SQL owner; one fixed worker owns the sole document handle
//! across prepare, the exact writer receipt and apply. No logical timeout frees
//! the physical ticket. There is no caller-provided target or publication body.

use super::*;
use crate::publication::{Completion, PublicationWorker};

impl Actor {
    pub(super) fn start_run_publication(&mut self, run: RunId) -> Result<bool, RunError> {
        if self.publication_ack.is_some()
            || self
                .publication_worker
                .as_ref()
                .is_some_and(|worker| worker.ticket().is_some())
        {
            return Err(koru_delegate_runtime::publication_pending().into());
        }
        if self.publication_worker.is_none() {
            let commands = self.commands.clone();
            self.publication_worker = Some(
                PublicationWorker::start(move || {
                    let _ = commands.send(Command::PublicationWake);
                })
                .map_err(|_| {
                    PortError::new(
                        koru_delegate_runtime::Port::Documents,
                        "publication_worker_unavailable",
                        "the publication worker could not start",
                    )
                })?,
            );
        }
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
                        None
                    }
                };
                if let Some(worker) = self.publication_worker.as_mut() {
                    let result = match retained {
                        Some(prepared) => worker.apply(ticket, prepared),
                        None => worker.return_handle(ticket),
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
                }
            }
            Completion::Pending { ticket, error } => {
                tracing::warn!(target: "koru::delegate", code = error.code, "publication completion is unknown");
                if !shutting_down && let Some(worker) = self.publication_worker.as_mut() {
                    // Preserve handle-local staging and retained SQL bytes.
                    let _ = worker.return_handle(ticket);
                }
            }
            Completion::Lost { ticket } => {
                self.orchestrator.lost_run_publication(ticket);
            }
        }
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
