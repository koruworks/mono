// SPDX-License-Identifier: MPL-2.0

//! Typed jobs use the existing bounded Store dispatcher, with synchronous or
//! asynchronous replies. Reply cancellation never cancels accepted Store work.
use super::{
    StoreWorkerClient, WorkerCommand, WorkerError, map_admission_error, map_response_error,
};
use koru_replication::cloud::{
    Refusal,
    client_store::{self, Export, ExportRequest, Failure, Receipt},
    transfer::{Completed, Scope, Unit},
};
use koru_store_core::Timestamp;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

/// Reply interest in an already accepted Store job, not ownership of the job.
/// Dropping this without ever polling it cannot cancel the queued operation.
pub struct StoreReply<R> {
    response: tokio::sync::oneshot::Receiver<Result<R, WorkerError>>,
}
impl<R> Future for StoreReply<R> {
    type Output = Result<R, WorkerError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.response).poll(cx).map(|result| {
            result
                .map_err(|_| WorkerError::Stopped)
                .and_then(|value| value)
        })
    }
}

impl<R> StoreReply<R> {
    /// The reply half of a job an executor other than the Store worker owns —
    /// a per-replica actor, an in-memory composition's own thread. The
    /// executor sends the job's result, or the reason it never ran, on the
    /// other half; dropping that half unanswered reads as `Stopped`.
    pub fn from_channel(response: tokio::sync::oneshot::Receiver<Result<R, WorkerError>>) -> Self {
        Self { response }
    }
}

/// What a Cloud pass needs from whoever owns the Store it reconciles.
///
/// **The seam the driver is generic over**, so a pass runs the same against
/// the Store worker's queue ([`StoreWorkerClient`]) and against any other
/// executor of bounded Store jobs — a host that keeps each replica's Store on
/// its own actor thread, with the pass's jobs at a lower priority than a tool
/// call's. Two things, and nothing about transport or authority:
///
/// - **one bounded job, admitted now.** Admission is eager and the reply lazy:
///   once `submit_store` answers `Ok`, the executor owns the job even if the
///   reply is never polled, exactly as the worker's queue does. A job never
///   waits for Cloud; the pass awaits its reply off the executor's thread.
/// - **a subscription to a dataset's durable commits**, so an idle pass wakes
///   on a local change instead of polling.
pub trait StoreJobs: Clone + Send + Sync + 'static {
    fn submit_store<R, F>(&self, task: F) -> Result<StoreReply<R>, WorkerError>
    where
        R: Send + 'static,
        F: FnOnce(&mut crate::WorkerStore) -> R + Send + 'static;

    fn watch_cloud(
        &self,
        dataset: koru_store_api::cloud_changes::Dataset,
    ) -> impl Future<Output = Result<koru_store_api::cloud_changes::Subscription, WorkerError>> + Send;

    /// [`StoreWorkerClient::submit_cloud_export`], over any executor.
    fn submit_cloud_export<F>(
        &self,
        request: ExportRequest,
        at: Timestamp,
        authorize: F,
    ) -> Result<StoreReply<Result<Option<Export>, Failure>>, WorkerError>
    where
        F: FnOnce(&mut crate::WorkerStore, Scope, Unit) -> Result<(), Refusal> + Send + 'static,
    {
        self.submit_store(move |store| request.run(store, at, authorize))
    }

    /// [`StoreWorkerClient::submit_cloud`], over any executor.
    fn submit_cloud<F>(
        &self,
        scope: Scope,
        complete: Completed,
        at: Timestamp,
        authorize: F,
    ) -> Result<StoreReply<Result<Receipt, Failure>>, WorkerError>
    where
        F: FnOnce(&mut crate::WorkerStore, Scope, Unit) -> Result<(), Refusal> + Send + 'static,
    {
        self.submit_store(move |store| client_store::receive(store, scope, complete, at, authorize))
    }
}

impl StoreJobs for StoreWorkerClient {
    fn submit_store<R, F>(&self, task: F) -> Result<StoreReply<R>, WorkerError>
    where
        R: Send + 'static,
        F: FnOnce(&mut crate::WorkerStore) -> R + Send + 'static,
    {
        StoreWorkerClient::submit_store(self, task)
    }

    fn watch_cloud(
        &self,
        dataset: koru_store_api::cloud_changes::Dataset,
    ) -> impl Future<Output = Result<koru_store_api::cloud_changes::Subscription, WorkerError>> + Send
    {
        StoreWorkerClient::watch_cloud(self, dataset)
    }
}

impl StoreWorkerClient {
    pub async fn watch_cloud(
        &self,
        dataset: koru_store_api::cloud_changes::Dataset,
    ) -> Result<koru_store_api::cloud_changes::Subscription, WorkerError> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.requests
            .try_send(WorkerCommand::WatchCloud { dataset, reply })
            .map_err(map_admission_error)?;
        response.await.map_err(|_| WorkerError::Stopped)
    }

    /// Queue a reserved local export eagerly. Authorization and the durable VV
    /// comparison run inside the Store job. A dropped reply never frees a queued
    /// export's reservation early; its eventual result is then discarded.
    pub fn submit_cloud_export<F>(
        &self,
        request: ExportRequest,
        at: Timestamp,
        authorize: F,
    ) -> Result<StoreReply<Result<Option<Export>, Failure>>, WorkerError>
    where
        F: FnOnce(&mut crate::WorkerStore, Scope, Unit) -> Result<(), Refusal> + Send + 'static,
    {
        self.submit_store(move |store| request.run(store, at, authorize))
    }

    /// Execute one typed operation on the worker-owned Store. The closure and
    /// its owned result cross the bounded worker queue; no Store handle, lock,
    /// HTTP request, or serialized envelope crosses back into the caller.
    pub fn with_store<R, F>(&self, task: F) -> Result<R, WorkerError>
    where
        R: Send + 'static,
        F: FnOnce(&mut crate::WorkerStore) -> R + Send + 'static,
    {
        let (reply, response) = std::sync::mpsc::sync_channel(1);
        self.requests
            .try_send(WorkerCommand::Store(Box::new(move |store| {
                let _ = reply.send(store.map(task));
            })))
            .map_err(map_admission_error)?;
        response
            .recv_timeout(Duration::from_secs(30))
            .map_err(map_response_error)?
    }

    /// Submit on first poll, refusing a full queue without waiting for space.
    /// Await the reply without blocking the caller's executor. Once admitted,
    /// the dispatcher owns the task even if this future is dropped; it does
    /// not skip work because the reply receiver closed. A caller may bound its
    /// own wait, but timeout/cancellation is not evidence of a failed commit.
    ///
    /// This does not create a runtime or a blocking thread. Execution still
    /// uses the Store worker's existing serialized dispatcher and Store lock.
    pub async fn with_store_async<R, F>(&self, task: F) -> Result<R, WorkerError>
    where
        R: Send + 'static,
        F: FnOnce(&mut crate::WorkerStore) -> R + Send + 'static,
    {
        self.submit_store(task)?.await
    }

    /// Admit now, without waiting for queue space. Only the reply is lazy.
    /// Use this when a coordinator must serialize actual ownership handoff
    /// with policy changes before it yields to another event.
    pub fn submit_store<R, F>(&self, task: F) -> Result<StoreReply<R>, WorkerError>
    where
        R: Send + 'static,
        F: FnOnce(&mut crate::WorkerStore) -> R + Send + 'static,
    {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.requests
            .try_send(WorkerCommand::Store(Box::new(move |store| {
                let _ = reply.send(store.map(task));
            })))
            .map_err(map_admission_error)?;
        Ok(StoreReply { response })
    }

    /// Hand the complete, budget-retaining Cloud download to the Store owner.
    /// Current account/mode/scope authorization runs inside that job, not at
    /// enqueue time. Outer errors describe queue/worker failure; inner errors
    /// describe transfer validation, authorization or Store persistence.
    /// Only an inner success exposes a durable receipt; this method sends no
    /// network Ack and does not authorize or activate a Cloud session.
    pub async fn receive_cloud<F>(
        &self,
        scope: Scope,
        complete: Completed,
        at: Timestamp,
        authorize: F,
    ) -> Result<Result<Receipt, Failure>, WorkerError>
    where
        F: FnOnce(&mut crate::WorkerStore, Scope, Unit) -> Result<(), Refusal> + Send + 'static,
    {
        self.submit_cloud(scope, complete, at, authorize)?.await
    }

    /// Eager counterpart of receive_cloud: success means the Store queue
    /// already owns the complete transfer, even before the reply is polled.
    pub fn submit_cloud<F>(
        &self,
        scope: Scope,
        complete: Completed,
        at: Timestamp,
        authorize: F,
    ) -> Result<StoreReply<Result<Receipt, Failure>>, WorkerError>
    where
        F: FnOnce(&mut crate::WorkerStore, Scope, Unit) -> Result<(), Refusal> + Send + 'static,
    {
        self.submit_store(move |store| client_store::receive(store, scope, complete, at, authorize))
    }
}

#[cfg(test)]
mod tests;
