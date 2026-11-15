// SPDX-License-Identifier: MPL-2.0

//! One fixed physical document worker. Its handle stays here across the SQL
//! preparation receipt; only bounded observations cross back to the actor.
//!
//! The same worker runs the finite no-document decision enrollment job (plan
//! section 4): it shares the single ticket, so it starts only while no Run's
//! document handle is out, and the HTTP call runs on this thread, never the
//! actor's. Its reply comes back on its own capacity-one slot.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread::{self, JoinHandle},
};

use koru_delegate_core::decisions::{
    BotWaitEnrollmentKey, BotWaitEnrollmentReply, DecisionPreparedProjection, EnrollBotWaitCleanup,
};
use koru_delegate_runtime::{
    AppliedRunProjection, DecisionEnrollmentBridge, DecisionUsageObservation, Port, PortError,
    PortResult, RunDocumentHandle, RunPublicationDocument, RunPublicationOperation,
    RunPublicationStart, RunPublicationTicket,
};

/// The host's enrollment client, owned by the worker thread.
pub(crate) type EnrollmentClient = Box<dyn DecisionEnrollmentBridge + Send>;
/// One finished enrollment job: the exact key it was for and the bridge's
/// answer. A transport failure stays an error (unknown), never a reply.
type EnrollmentReply = (BotWaitEnrollmentKey, PortResult<BotWaitEnrollmentReply>);

enum Request {
    Start(RunPublicationStart),
    Apply {
        ticket: RunPublicationTicket,
        prepared: DecisionPreparedProjection,
    },
    Return {
        ticket: RunPublicationTicket,
    },
    #[cfg_attr(not(test), allow(dead_code))] // sent once the actor composes the mailbox
    Enroll {
        request: EnrollBotWaitCleanup,
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
    /// A read-only source observation finished; the handle comes straight
    /// back with it. Nothing was prepared, imported or written.
    Observed {
        ticket: RunPublicationTicket,
        handle: Box<dyn RunDocumentHandle>,
        observation: PortResult<DecisionUsageObservation>,
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
            | Self::Observed { ticket, .. }
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
    enrollment: EnrollmentSlot,
}

/// The enrollment half of the single ticket. `active` is the key of the one
/// job physically in flight: only its reply frees it, never a caller's
/// timeout. `returned` holds a finished reply until its own key claims it.
pub(crate) struct EnrollmentSlot {
    replies: Receiver<EnrollmentReply>,
    active: Option<BotWaitEnrollmentKey>,
    returned: Option<EnrollmentReply>,
    /// Claims by another key since `returned` arrived. A reply nobody claims
    /// within one further attempt is discarded; its owner resends the same
    /// request and C replays the exact retained enrollment.
    misses: u8,
}
impl EnrollmentSlot {
    fn new(replies: Receiver<EnrollmentReply>) -> Self {
        Self {
            replies,
            active: None,
            returned: None,
            misses: 0,
        }
    }
}

impl PublicationWorker {
    /// A worker without an enrollment client: every enrollment answers
    /// `decision_enrollment_unavailable` (no default reply).
    pub(crate) fn start(wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        Self::spawn(wake, None)
    }

    /// A worker that runs enrollment through the host's client on its thread.
    #[cfg_attr(not(test), allow(dead_code))] // composed by the actor's per-turn call
    pub(crate) fn start_with_enrollment(
        wake: impl Fn() + Send + 'static,
        client: EnrollmentClient,
    ) -> std::io::Result<Self> {
        Self::spawn(wake, Some(client))
    }

    fn spawn(
        wake: impl Fn() + Send + 'static,
        client: Option<EnrollmentClient>,
    ) -> std::io::Result<Self> {
        let (send, requests) = mpsc::sync_channel(1);
        let (completed, receive) = mpsc::sync_channel(1);
        let (enrolled, replies) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("koru-delegate publication".into())
            .spawn(move || run_with(requests, completed, enrolled, client, wake))?;
        Ok(Self {
            send,
            receive,
            thread: Some(thread),
            ticket: None,
            waiting: false,
            enrollment: EnrollmentSlot::new(replies),
        })
    }

    pub(crate) fn begin(&mut self, start: RunPublicationStart) -> Result<(), RunPublicationStart> {
        self.collect_enrollment();
        if self.ticket.is_some() || self.waiting || self.enrollment.active.is_some() {
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

    /// Take a finished enrollment reply off its slot, if one came back.
    fn collect_enrollment(&mut self) {
        match self.enrollment.replies.try_recv() {
            Ok(reply) => {
                self.enrollment.active = None;
                self.enrollment.returned = Some(reply);
                self.enrollment.misses = 0;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                // The worker physically ended: an in-flight job's outcome is
                // unknown, never a reply.
                if let Some(key) = self.enrollment.active.take() {
                    self.enrollment.returned = Some((key, Err(enrollment_lost())));
                    self.enrollment.misses = 0;
                }
            }
        }
    }

    /// The mailbox behind `EnrollmentMailbox`: the finished reply for exactly
    /// this request's key if one came back; otherwise start this request when
    /// the single ticket is idle (no handle out, nothing in flight) and answer
    /// pending. Never blocks the caller and never invents a reply.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn enrollment(
        &mut self,
        request: &EnrollBotWaitCleanup,
    ) -> PortResult<BotWaitEnrollmentReply> {
        self.collect_enrollment();
        if let Some((key, _)) = &self.enrollment.returned {
            if *key == request.key {
                let (_, result) = self.enrollment.returned.take().expect("matched above");
                return result;
            }
            if self.enrollment.misses == 0 {
                self.enrollment.misses = 1;
                return Err(enrollment_pending());
            }
            self.enrollment.returned = None;
            self.enrollment.misses = 0;
        }
        if self.enrollment.active.is_some() || self.ticket.is_some() || self.waiting {
            return Err(enrollment_pending());
        }
        if self
            .send
            .try_send(Request::Enroll {
                request: request.clone(),
            })
            .is_ok()
        {
            self.enrollment.active = Some(request.key.clone());
        }
        Err(enrollment_pending())
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
        // An enrollment in flight finished (or the thread ended) before the
        // join returned; nothing is delivered after shutdown.
        self.collect_enrollment();
        self.enrollment.active = None;
        self.enrollment.returned = None;
        // A worker may end outside execute's unwind boundary (for example its
        // wake callback fails). Joining is actual physical completion; an
        // absent returned handle is now loss, not an indefinitely live ticket.
        // Prepared alone never returns ownership. Keep that receipt followed by
        // Lost so the actor can retain SQL debt and enter ReopenOwed.
        if let Some(ticket) = self.ticket
            && !completions
                .iter()
                .any(|completion| completion.ticket() == ticket && !completion.retains_handle())
        {
            completions.push(Completion::Lost { ticket });
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

/// What a driver turn hears while enrollment is not finished.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn enrollment_pending() -> PortError {
    PortError::new(
        Port::Documents,
        "decision_enrollment_pending",
        "the decision enrollment job has not finished",
    )
}

fn enrollment_lost() -> PortError {
    PortError::new(
        Port::Documents,
        "decision_enrollment_unknown",
        "the decision enrollment job ended without an answer",
    )
}

/// Through this the per-turn resume driver reaches the worker's enrollment
/// slot; it never runs the HTTP call itself.
#[cfg_attr(not(test), allow(dead_code))] // composed by the actor's per-turn call
pub(crate) struct EnrollmentMailbox<'a> {
    worker: &'a mut PublicationWorker,
}
impl<'a> EnrollmentMailbox<'a> {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(worker: &'a mut PublicationWorker) -> Self {
        Self { worker }
    }
}
impl DecisionEnrollmentBridge for EnrollmentMailbox<'_> {
    fn enroll(&mut self, request: &EnrollBotWaitCleanup) -> PortResult<BotWaitEnrollmentReply> {
        self.worker.enrollment(request)
    }
}

fn busy() -> PortError {
    PortError::new(
        Port::Documents,
        "publication_pending",
        "the physical publication ticket is occupied",
    )
}

#[cfg(test)]
fn run(requests: Receiver<Request>, completed: SyncSender<Completion>, wake: impl Fn()) {
    let (enrolled, _replies) = mpsc::sync_channel(1);
    run_with(requests, completed, enrolled, None, wake)
}

fn run_with(
    requests: Receiver<Request>,
    completed: SyncSender<Completion>,
    enrolled: SyncSender<EnrollmentReply>,
    mut client: Option<EnrollmentClient>,
    wake: impl Fn(),
) {
    let mut active: Option<Active> = None;
    while let Ok(request) = requests.recv() {
        if let Request::Enroll { request } = request {
            // No document handle is held: the single ticket admitted this job
            // only while none was out.
            let key = request.key.clone();
            let result = if active.is_some() {
                Err(busy())
            } else {
                match client.as_mut() {
                    Some(client) => catch_unwind(AssertUnwindSafe(|| client.enroll(&request)))
                        .unwrap_or_else(|_| Err(enrollment_lost())),
                    None => Err(PortError::new(
                        Port::Documents,
                        "decision_enrollment_unavailable",
                        "this host has no decision enrollment client",
                    )),
                }
            };
            if enrolled.send((key, result)).is_err() {
                break;
            }
            wake();
            continue;
        }
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
            Request::Enroll { .. } | Request::Shutdown => unreachable!(),
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
                RunPublicationOperation::ObserveSource(observation) => {
                    // One bounded current-state read for one exact cold key.
                    let current = active.take().expect("assigned above");
                    let observed = observation.observe(current.handle.as_ref());
                    Completion::Observed {
                        ticket: current.ticket,
                        handle: current.handle,
                        observation: observed,
                    }
                }
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
        Request::Enroll { .. } | Request::Shutdown => unreachable!(),
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

#[cfg(test)]
mod tests;
