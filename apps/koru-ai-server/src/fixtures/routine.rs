// SPDX-License-Identifier: MPL-2.0
//! Real worker, retained Store seed, capture, and catalog composition.
use super::{Repository, catalog::Catalog};
use crate::{
    AiWorker, AiWorkerClient, BotAuthorityAdapter, WorkerConfig, WorkerError, WorkerExit,
    WorkerHandle, bots::routine::fixture::Fixture,
};
use koru_ai_core::{Cancellation, ProfileId, WorkspaceId, bots::*};
use koru_ai_provider::ProviderRegistry;
use koru_delegate_api::bots::BotDelegatePort;
use koru_delegate_core::{PrepareOccurrence, RunContextItem};
use std::{
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::{Receiver, sync_channel},
    },
    time::Duration,
};

#[derive(Default)]
struct CaptureGate {
    state: Mutex<(bool, bool)>, // entered, released
    changed: Condvar,
}
impl CaptureGate {
    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
    fn enter(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.changed.notify_all();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(15), |state| !state.1)
            .unwrap();
        state.1
    }
}

/// Holds the next real capture's return, after its actual Store/history read.
/// Dropping this handle releases it. The fixture also releases it before join.
pub struct RoutineCaptureHold(Arc<CaptureGate>);
impl RoutineCaptureHold {
    /// Waits without polling. False means the supplied observation timeout elapsed.
    pub fn wait_entered(&self, within: Duration) -> bool {
        let state = self.0.state.lock().unwrap();
        let (state, _) = self
            .0
            .changed
            .wait_timeout_while(state, within, |state| !state.0)
            .unwrap();
        state.0
    }
    pub fn release(self) {
        self.0.release();
    }
}
impl Drop for RoutineCaptureHold {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct Authority {
    adapter: Arc<BotAuthorityAdapter>,
    captures: AtomicUsize,
    gate: Mutex<Option<Arc<CaptureGate>>>,
}
impl Authority {
    fn release(&self) {
        if let Some(gate) = self.gate.lock().unwrap().as_ref() {
            gate.release();
        }
    }
}
impl BotAuthority for Authority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.adapter.owner()
    }
    fn workspace(&self, home: WorkspaceId) -> Result<ProfileId, BotError> {
        self.adapter.workspace(home)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.adapter.page(page)
    }
    fn inspect(
        &self,
        homes: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<BotAuthoritySnapshot, BotError> {
        self.adapter.inspect(homes, pages)
    }
    fn verify_routine_definition(
        &self,
        owner: BotOwner,
        profile: ProfileId,
        identity: &BotRoutineCreationIdentity,
        definition: &RoutineScheduleDefinition,
        handoff: Option<koru_delegate_core::HandoffId>,
        cancel: &Cancellation,
    ) -> Result<(), BotError> {
        self.adapter
            .verify_routine_definition(owner, profile, identity, definition, handoff, cancel)
    }
    fn capture(
        &self,
        owner: BotOwner,
        home: WorkspaceId,
        profile: ProfileId,
        pages: &[BotPage],
    ) -> Result<Vec<RunContextItem>, BotError> {
        self.captures.fetch_add(1, Ordering::AcqRel);
        let captured = self.adapter.capture(owner, home, profile, pages)?;
        let gate = self.gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            let released = gate.enter();
            self.gate.lock().unwrap().take();
            if !released {
                return Err(BotError::Unavailable);
            }
        }
        Ok(captured)
    }
    fn capture_memory(
        &self,
        owner: BotOwner,
        profile: ProfileId,
        binding: &BotMemoryBinding,
        cancel: &Cancellation,
    ) -> Result<PreparedMemoryCapture, MemoryAuthorityError> {
        self.adapter.capture_memory(owner, profile, binding, cancel)
    }
}

/// Windows-only composition with no filesystem Store, provider or session launch.
/// Startup uses the ordinary worker and delegate Hello; readiness is never forced.
pub struct RoutineWorkerFixture {
    worker: Option<WorkerHandle>,
    returned: Receiver<Repository>,
    request: PrepareOccurrence,
    authority: Arc<Authority>,
    catalog: Catalog,
    // Keep the no-path Store alive through worker shutdown and join.
    _store: Fixture,
}
impl RoutineWorkerFixture {
    /// Builds the real retained seed and request before invoking the delegate
    /// factory, then starts the worker. Owner discovery may still be pending;
    /// callers observe it through ordinary ReadPreparation replies.
    pub fn spawn(
        factory: impl FnOnce(&PrepareOccurrence) -> Arc<dyn BotDelegatePort>,
    ) -> Result<Self, WorkerError> {
        let (store, repository, request) = Fixture::occurrence();
        let delegate = factory(&request);
        let catalog = Catalog::new();
        let authority = Arc::new(Authority {
            adapter: store.adapter.clone(),
            captures: AtomicUsize::new(0),
            gate: Mutex::new(None),
        });
        let (returning, returned) = sync_channel(1);
        let mut config = WorkerConfig::new(
            PathBuf::from("unused-routine-occurrence"),
            "internal-debug",
            ProviderRegistry::default(),
        )
        .with_bot_authority(authority.clone())
        .with_bot_delegate(delegate)
        .with_session_host(catalog.endpoint.clone());
        config.test_repository = Some(repository);
        config.test_repository_return = Some(returning);
        let worker = AiWorker::new(config).spawn()?;
        Ok(Self {
            worker: Some(worker),
            returned,
            request,
            authority,
            catalog,
            _store: store,
        })
    }
    pub fn client(&self) -> AiWorkerClient {
        self.worker.as_ref().unwrap().client()
    }
    pub fn preparation(&self) -> PrepareOccurrence {
        self.request.clone()
    }
    /// At most one hold may be armed or active. A forgotten hold expires after
    /// 15 seconds with Unavailable; finish/drop release it before worker join.
    pub fn hold_next_capture(&self) -> RoutineCaptureHold {
        let mut pending = self.authority.gate.lock().unwrap();
        assert!(pending.is_none(), "one capture hold at a time");
        let gate = Arc::new(CaptureGate::default());
        *pending = Some(gate.clone());
        RoutineCaptureHold(gate)
    }
    pub fn capture_count(&self) -> usize {
        self.authority.captures.load(Ordering::Acquire)
    }
    pub fn catalog_reads(&self) -> usize {
        self.catalog.reads.load(Ordering::Acquire)
    }
    /// Returns the actual in-memory writer only after its real completion.
    /// Non-clean exits and a missing writer return are observable errors.
    pub fn finish(mut self) -> Result<Repository, WorkerExit> {
        self.authority.release();
        let worker = self.worker.take().unwrap();
        worker.shutdown();
        match worker.join() {
            WorkerExit::Clean => self
                .returned
                .recv_timeout(Duration::from_secs(5))
                .map_err(|error| WorkerExit::Failed(format!("fixture repository return: {error}"))),
            exit => Err(exit),
        }
    }
}
impl Drop for RoutineWorkerFixture {
    fn drop(&mut self) {
        self.authority.release();
        if let Some(worker) = self.worker.take() {
            worker.shutdown();
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests;
