// SPDX-License-Identifier: MPL-2.0

//! Bounded context reads leave the engine actor and worker heartbeat free.
//! Polling an unfinished read is not another read or an execution grant.
use crate::{
    Error,
    authority::{Secret, Worker},
    capacity::{Admission, Reservation},
    carrier::Reply,
    workspace::{Context, Sources},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, mpsc},
};
use uuid::Uuid;

struct Job {
    worker: Worker,
    reservation: Reservation,
    sources: Sources,
    result: Option<Result<Context, Error>>,
}
pub(super) struct Jobs {
    read: Arc<dyn Fn(Sources) -> Result<Context, Error> + Send + Sync>,
    jobs: BTreeMap<Uuid, Job>,
    send: mpsc::SyncSender<(Uuid, Result<Context, Error>)>,
    receive: mpsc::Receiver<(Uuid, Result<Context, Error>)>,
}
impl Jobs {
    pub(super) fn new(endpoint: koru_ipc::Endpoint, authority: Secret) -> Self {
        Self::with_reader(Arc::new(move |sources| {
            crate::workspace::load_context(&endpoint, authority.clone(), sources)
        }))
    }
    fn with_reader(read: Arc<dyn Fn(Sources) -> Result<Context, Error> + Send + Sync>) -> Self {
        let (send, receive) = mpsc::sync_channel(8);
        Self {
            read,
            jobs: BTreeMap::new(),
            send,
            receive,
        }
    }
    pub(super) fn poll(&mut self, admission: &Admission) {
        self.poll_current(|reservation, worker| admission.owns(reservation, worker));
    }
    fn poll_current(&mut self, current: impl Fn(Reservation, Uuid) -> bool) {
        for (id, result) in self.receive.try_iter() {
            if let Some(job) = self.jobs.get_mut(&id) {
                job.result = Some(result);
            }
        }
        // A running abandoned job keeps physical capacity until completion;
        // deleting it early would allow unbounded outstanding Store reads.
        self.jobs.retain(|_, job| {
            job.result.is_none() || current(job.reservation, job.worker.incarnation)
        });
    }
    pub(super) fn request(
        &mut self,
        worker: Worker,
        reservation: Reservation,
        sources: Sources,
    ) -> Reply {
        let id = reservation.assignment;
        if let Some(job) = self.jobs.get_mut(&id) {
            if job.worker != worker || job.reservation != reservation {
                return Reply::Refused(Error::StaleTurn);
            }
            if let Some(result) = job.result.take() {
                let unchanged = job.sources == sources;
                self.jobs.remove(&id);
                if unchanged {
                    return result.map(Reply::Context).unwrap_or_else(Reply::Refused);
                }
                // Settings changed while reading. Discard that snapshot and
                // read the current references instead of handing it to launch.
            } else {
                return Reply::ContextPending;
            }
        }
        if self.jobs.len() >= 8 {
            return Reply::ContextPending;
        }
        let read = self.read.clone();
        let send = self.send.clone();
        let reading = sources.clone();
        if std::thread::Builder::new()
            .name("koru-bot context".into())
            .spawn(move || {
                let result = read(reading);
                let _ = send.send((id, result));
            })
            .is_err()
        {
            return Reply::Refused(Error::EngineUnavailable);
        }
        self.jobs.insert(
            id,
            Job {
                worker,
                reservation,
                sources,
                result: None,
            },
        );
        Reply::ContextPending
    }
}

#[cfg(test)]
mod tests;
