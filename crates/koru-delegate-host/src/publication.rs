// SPDX-License-Identifier: MPL-2.0

//! One fixed physical document worker. Its handle stays here across the SQL
//! preparation receipt; only bounded observations cross back to the actor.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread::{self, JoinHandle},
};

use koru_delegate_core::decisions::DecisionPreparedProjection;
use koru_delegate_runtime::{
    AppliedRunProjection, Port, PortError, PortResult, RunDocumentHandle, RunPublicationDocument,
    RunPublicationOperation, RunPublicationStart, RunPublicationTicket,
};

enum Request {
    Start(RunPublicationStart),
    Apply {
        ticket: RunPublicationTicket,
        prepared: DecisionPreparedProjection,
    },
    Return {
        ticket: RunPublicationTicket,
    },
    Shutdown,
}

pub(crate) enum Completion {
    Prepared {
        ticket: RunPublicationTicket,
        prepared: DecisionPreparedProjection,
    },
    Applied {
        ticket: RunPublicationTicket,
        proof: AppliedRunProjection,
        handle: Box<dyn RunDocumentHandle>,
    },
    Returned {
        ticket: RunPublicationTicket,
        handle: Box<dyn RunDocumentHandle>,
        error: Option<PortError>,
    },
    /// Applying may have committed. The worker still owns the handle and the
    /// actor must reconcile/retry the exact retained preparation.
    Pending {
        ticket: RunPublicationTicket,
        error: PortError,
    },
    /// Sent only after the panicking job's actual handle has been dropped.
    Lost { ticket: RunPublicationTicket },
}
impl Completion {
    pub(crate) fn ticket(&self) -> RunPublicationTicket {
        match self {
            Self::Prepared { ticket, .. }
            | Self::Applied { ticket, .. }
            | Self::Returned { ticket, .. }
            | Self::Pending { ticket, .. }
            | Self::Lost { ticket } => *ticket,
        }
    }
    fn retains_handle(&self) -> bool {
        matches!(self, Self::Prepared { .. } | Self::Pending { .. })
    }
}

struct Active {
    ticket: RunPublicationTicket,
    handle: Box<dyn RunDocumentHandle>,
}

/// The actor owns this endpoint. A physical request is outstanding until its
/// completion is received, even when the original caller stopped waiting.
pub(crate) struct PublicationWorker {
    send: SyncSender<Request>,
    receive: Receiver<Completion>,
    thread: Option<JoinHandle<()>>,
    ticket: Option<RunPublicationTicket>,
    waiting: bool,
}
impl PublicationWorker {
    pub(crate) fn start(wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let (send, requests) = mpsc::sync_channel(1);
        let (completed, receive) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("koru-delegate publication".into())
            .spawn(move || run(requests, completed, wake))?;
        Ok(Self {
            send,
            receive,
            thread: Some(thread),
            ticket: None,
            waiting: false,
        })
    }

    pub(crate) fn begin(&mut self, start: RunPublicationStart) -> Result<(), RunPublicationStart> {
        if self.ticket.is_some() || self.waiting {
            return Err(start);
        }
        let ticket = start.ticket;
        match self.send.try_send(Request::Start(start)) {
            Ok(()) => {
                self.ticket = Some(ticket);
                self.waiting = true;
                Ok(())
            }
            Err(
                TrySendError::Full(Request::Start(start))
                | TrySendError::Disconnected(Request::Start(start)),
            ) => Err(start),
            _ => unreachable!("start request returned another variant"),
        }
    }

    pub(crate) fn apply(
        &mut self,
        ticket: RunPublicationTicket,
        prepared: DecisionPreparedProjection,
    ) -> PortResult<()> {
        self.submit(ticket, Request::Apply { ticket, prepared })
    }
    /// Returns ownership while preserving handle-local staging. It is not a
    /// discard/SQL rollback or proof that exclusive publication may end.
    pub(crate) fn return_handle(&mut self, ticket: RunPublicationTicket) -> PortResult<()> {
        self.submit(ticket, Request::Return { ticket })
    }
    fn submit(&mut self, ticket: RunPublicationTicket, request: Request) -> PortResult<()> {
        if self.ticket != Some(ticket) || self.waiting {
            return Err(busy());
        }
        self.send.try_send(request).map_err(|_| busy())?;
        self.waiting = true;
        Ok(())
    }
    pub(crate) fn completion(&mut self) -> Option<Completion> {
        match self.receive.try_recv() {
            Ok(completion) => {
                if self.ticket == Some(completion.ticket()) {
                    self.waiting = false;
                    if !completion.retains_handle() {
                        self.ticket = None;
                    }
                }
                Some(completion)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                // The worker physically ended; its local handle was dropped.
                self.waiting = false;
                self.ticket.take().map(|ticket| Completion::Lost { ticket })
            }
        }
    }
    pub(crate) fn ticket(&self) -> Option<RunPublicationTicket> {
        self.ticket
    }

    /// Close only after physical completion, draining the single completion
    /// slot so a worker finishing a Store call cannot deadlock shutdown. Store
    /// operations retain their inherited finite bounds; no false hard timeout.
    pub(crate) fn shutdown(&mut self) -> Vec<Completion> {
        let mut completions = Vec::with_capacity(2);
        if self.thread.is_none() {
            return completions;
        }
        // At most one request can precede Shutdown. Sending this control is
        // bounded by that physical request, not by a new cancellation timer.
        if self.send.send(Request::Shutdown).is_ok() {
            while let Ok(completion) = self.receive.recv() {
                completions.push(completion);
            }
        } else {
            while let Ok(completion) = self.receive.try_recv() {
                completions.push(completion);
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.ticket = None;
        self.waiting = false;
        completions
    }
}
impl Drop for PublicationWorker {
    fn drop(&mut self) {
        drop(self.shutdown());
    }
}

fn busy() -> PortError {
    PortError::new(
        Port::Documents,
        "publication_pending",
        "the physical publication ticket is occupied",
    )
}

fn run(requests: Receiver<Request>, completed: SyncSender<Completion>, wake: impl Fn()) {
    let mut active: Option<Active> = None;
    while let Ok(request) = requests.recv() {
        if matches!(request, Request::Shutdown) {
            if let Some(active) = active.take() {
                let completion = Completion::Returned {
                    ticket: active.ticket,
                    handle: active.handle,
                    error: None,
                };
                if completed.send(completion).is_ok() {
                    wake();
                }
            }
            break;
        }
        let ticket = match &request {
            Request::Start(start) => start.ticket,
            Request::Apply { ticket, .. } | Request::Return { ticket } => *ticket,
            Request::Shutdown => unreachable!(),
        };
        let completion = match catch_unwind(AssertUnwindSafe(|| execute(&mut active, request))) {
            Ok(completion) => completion,
            Err(_) => {
                // Drop before reporting physical loss. No replacement connection
                // can be opened while this stack still owns the canonical one.
                drop(active.take());
                Completion::Lost { ticket }
            }
        };
        if completed.send(completion).is_err() {
            break;
        }
        wake();
    }
}

fn execute(active: &mut Option<Active>, request: Request) -> Completion {
    match request {
        Request::Start(start) => {
            if active.is_some() {
                // Endpoint protocol forbids a second Start. Never open a second
                // canonical connection if that invariant is violated.
                return match start.document {
                    RunPublicationDocument::Attached(handle) => Completion::Returned {
                        ticket: start.ticket,
                        handle,
                        error: Some(busy()),
                    },
                    RunPublicationDocument::Cold(job) => {
                        drop(job);
                        Completion::Lost {
                            ticket: start.ticket,
                        }
                    }
                };
            }
            let handle = match start.document {
                RunPublicationDocument::Attached(handle) => handle,
                RunPublicationDocument::Cold(job) => match job.open() {
                    Ok(handle) => handle,
                    Err(error) => {
                        tracing::warn!(target: "koru::delegate", code = error.code, "publication cold open remains owed");
                        return Completion::Lost {
                            ticket: start.ticket,
                        };
                    }
                },
            };
            *active = Some(Active {
                ticket: start.ticket,
                handle,
            });
            match start.operation {
                RunPublicationOperation::Prepare(intent) => {
                    let current = active.as_mut().expect("assigned above");
                    match current.handle.prepare_decision_projection(&intent) {
                        Ok(prepared) => Completion::Prepared {
                            ticket: current.ticket,
                            prepared,
                        },
                        Err(error) => returned(active, Some(error)),
                    }
                }
                RunPublicationOperation::Apply(prepared) => apply(active, start.ticket, &prepared),
            }
        }
        Request::Apply { ticket, prepared } => apply(active, ticket, &prepared),
        Request::Return { ticket } => {
            if active.as_ref().is_some_and(|a| a.ticket == ticket) {
                returned(active, None)
            } else {
                Completion::Pending {
                    ticket,
                    error: busy(),
                }
            }
        }
        Request::Shutdown => unreachable!(),
    }
}
fn returned(active: &mut Option<Active>, error: Option<PortError>) -> Completion {
    let active = active.take().expect("a returned handle is held");
    Completion::Returned {
        ticket: active.ticket,
        handle: active.handle,
        error,
    }
}
fn apply(
    active: &mut Option<Active>,
    ticket: RunPublicationTicket,
    prepared: &DecisionPreparedProjection,
) -> Completion {
    let Some(current) = active.as_mut().filter(|a| a.ticket == ticket) else {
        return Completion::Pending {
            ticket,
            error: busy(),
        };
    };
    match current.handle.apply_prepared_projection(prepared) {
        Ok(proof) => {
            let active = active.take().expect("checked above");
            Completion::Applied {
                ticket,
                proof,
                handle: active.handle,
            }
        }
        Err(error) => Completion::Pending { ticket, error },
    }
}
