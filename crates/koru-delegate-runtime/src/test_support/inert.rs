// SPDX-License-Identifier: MPL-2.0

//! The non-session ports behind [`super::decision_chain`]: in-memory Run
//! documents whose handle stages decision projections, and inert capture,
//! events, preamble, roots, MCP, registrations, clock and sleep. None of them
//! touches the disk, a Store, a provider or the network.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use koru_delegate_core::decisions::{DecisionPreparedProjection, DecisionTransitionId};
use koru_delegate_core::{
    AgentTurnId, DocumentId, EventId, ProjectId, ResultReference, RunAttemptId, RunContextItem,
    RunHeader, RunId, WorkspaceId,
};
use koru_delegate_document::{RunDocument, RunDocumentError, RunProjectionStaging};
use koru_delegate_schedule::ScheduleContextPointer;
use koru_delegate_scope::{ResolvedScope, RootLookup, ScopeIdentity, ScopeRefusal, ScopeRoots};
use koru_session_protocol::{DelegationLaunch, McpToolAllowlist};

use crate::{
    AppliedRunProjection, ArtifactRegistrations, CaptureFailure, CaptureStage, CapturedContext,
    Clock, ContextCapture, ListedRun, MaterializeFailure, MaterializedContext, McpEndpoints,
    McpLaunch, Port, PortError, PortResult, Preamble, PublishFailure, RunDocumentHandle,
    RunDocumentOpen, RunDocuments, RunEvents, RunFact, RunProjectionIntent, Sleep, SourceRefusal,
    SourceRefusalReason,
};

// ------------------------------------------------------------ the documents

/// The Run documents this fixture "stored": the latest snapshot by id, shared
/// by every handle so a reopen reads what a commit wrote.
#[derive(Default)]
struct Stored {
    snapshots: HashMap<DocumentId, Vec<u8>>,
    by_run: HashMap<RunId, DocumentId>,
    /// When set, every decision projection preparation is refused with this
    /// code (a Store that keeps refusing); nothing is staged.
    refuse_preparations: Option<&'static str>,
    /// Decision projection preparations asked of any handle.
    preparations: usize,
}

/// The test's view of the in-memory documents: script a persistent
/// preparation refusal and count the preparations asked for.
#[derive(Clone)]
pub struct DocumentFaults {
    stored: Arc<Mutex<Stored>>,
}

impl std::fmt::Debug for DocumentFaults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DocumentFaults")
    }
}

impl DocumentFaults {
    /// Refuse every later preparation with `code` (`None` stops refusing).
    pub fn refuse_preparations(&self, code: Option<&'static str>) {
        self.stored
            .lock()
            .expect("the document store is not poisoned")
            .refuse_preparations = code;
    }

    /// How many decision projection preparations were asked for so far.
    pub fn preparations(&self) -> usize {
        self.stored
            .lock()
            .expect("the document store is not poisoned")
            .preparations
    }
}

/// In-memory Run documents. Every commit is durable when it returns (the
/// handle's `durable`/`settle` keep the port defaults: true / Ok).
pub(super) struct MemoryRunDocuments {
    stored: Arc<Mutex<Stored>>,
}

impl MemoryRunDocuments {
    pub(super) fn new() -> Self {
        Self {
            stored: Arc::new(Mutex::new(Stored::default())),
        }
    }

    pub(super) fn faults(&self) -> DocumentFaults {
        DocumentFaults {
            stored: Arc::clone(&self.stored),
        }
    }

    fn open_handle(
        stored: &Arc<Mutex<Stored>>,
        document: DocumentId,
    ) -> PortResult<Box<dyn RunDocumentHandle>> {
        let doc = {
            let stored = stored.lock().expect("the document store is not poisoned");
            let bytes = stored
                .snapshots
                .get(&document)
                .ok_or_else(|| PortError::new(Port::Documents, "not_found", "no such document"))?;
            RunDocument::from_snapshot(bytes)?
        };
        Ok(Box::new(StagingHandle {
            id: document,
            doc,
            staging: RunProjectionStaging::default(),
            stored: Arc::clone(stored),
        }))
    }
}

/// One open Run document with the real projection staging of
/// `koru-delegate-document`: prepare on private staging, apply into the
/// connected document, then acknowledge the version it now carries. No
/// proof is minted here; the orchestrator checks the returned projection.
struct StagingHandle {
    id: DocumentId,
    doc: RunDocument,
    staging: RunProjectionStaging,
    stored: Arc<Mutex<Stored>>,
}

impl StagingHandle {
    fn save(&self) -> PortResult<()> {
        let bytes = self.doc.export_snapshot()?;
        self.stored
            .lock()
            .expect("the document store is not poisoned")
            .snapshots
            .insert(self.id, bytes);
        Ok(())
    }
}

impl RunDocumentHandle for StagingHandle {
    fn document(&self) -> &RunDocument {
        &self.doc
    }

    fn commit(
        &mut self,
        write: &mut dyn FnMut(&RunDocument) -> Result<(), RunDocumentError>,
    ) -> PortResult<()> {
        write(&self.doc)?;
        self.save()
    }

    fn prepare_decision_projection(
        &mut self,
        intent: &RunProjectionIntent,
    ) -> PortResult<DecisionPreparedProjection> {
        {
            let mut stored = self
                .stored
                .lock()
                .expect("the document store is not poisoned");
            stored.preparations += 1;
            if let Some(code) = stored.refuse_preparations {
                return Err(PortError::new(
                    Port::Documents,
                    code,
                    "the scripted document refused the preparation",
                ));
            }
        }
        // Every commit was durable when it returned: the causal base is settled.
        self.staging.prepare(&self.doc, intent).map_err(Into::into)
    }

    fn apply_prepared_projection(
        &mut self,
        prepared: &DecisionPreparedProjection,
    ) -> PortResult<AppliedRunProjection> {
        self.staging.apply(&self.doc, prepared)?;
        self.save()?;
        let acknowledged = self.doc.version();
        self.staging
            .acknowledge(&self.doc, prepared, &acknowledged)
            .map_err(Into::into)
    }

    fn discard_prepared_projection(&mut self, transition: DecisionTransitionId) -> PortResult<()> {
        self.staging.discard(transition).map_err(Into::into)
    }
}

impl RunDocuments for MemoryRunDocuments {
    fn publication_open(
        &mut self,
        _workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn RunDocumentOpen>> {
        Ok(Box::new(ColdOpen {
            stored: Arc::clone(&self.stored),
            document,
        }))
    }

    fn create(&mut self, _workspace: WorkspaceId, header: &RunHeader) -> PortResult<DocumentId> {
        let mut stored = self
            .stored
            .lock()
            .expect("the document store is not poisoned");
        if let Some(existing) = stored.by_run.get(&header.run) {
            return Ok(*existing);
        }
        let document = RunDocument::create(header)?;
        let id = DocumentId::from_uuid(uuid::Uuid::new_v4());
        stored.snapshots.insert(id, document.export_snapshot()?);
        stored.by_run.insert(header.run, id);
        Ok(id)
    }

    fn open(
        &mut self,
        _workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn RunDocumentHandle>> {
        Self::open_handle(&self.stored, document)
    }

    fn list(&mut self, _workspace: WorkspaceId) -> PortResult<Vec<ListedRun>> {
        let stored = self
            .stored
            .lock()
            .expect("the document store is not poisoned");
        let mut listed = Vec::with_capacity(stored.snapshots.len());
        for (document, bytes) in &stored.snapshots {
            listed.push(ListedRun {
                document: *document,
                header: RunDocument::from_snapshot(bytes)?.header()?,
                trashed_at_ms: None,
            });
        }
        Ok(listed)
    }
}

/// A frozen cold-open specification; opening reads only the stored snapshot.
struct ColdOpen {
    stored: Arc<Mutex<Stored>>,
    document: DocumentId,
}

impl RunDocumentOpen for ColdOpen {
    fn open(self: Box<Self>) -> PortResult<Box<dyn RunDocumentHandle>> {
        MemoryRunDocuments::open_handle(&self.stored, self.document)
    }
}

// ------------------------------------------------------------ the rest

/// Pins what it is given and writes nothing. Schedule resolution and resume
/// admission are not part of the decision chain and refuse.
pub(super) struct InertCapture;

impl ContextCapture for InertCapture {
    fn resolve_and_pin(
        &mut self,
        _run: RunId,
        _pointers: &[ScheduleContextPointer],
        _workspace: WorkspaceId,
    ) -> Result<Vec<RunContextItem>, CaptureFailure> {
        Err(CaptureFailure {
            item: 0,
            stage: CaptureStage::Resolve,
        })
    }

    fn admit(&mut self, _run: RunId, _pins: &[RunContextItem]) -> Result<(), SourceRefusal> {
        Err(SourceRefusal {
            item: 0,
            reason: SourceRefusalReason::Unanswered,
        })
    }

    fn capture(
        &mut self,
        _run: RunId,
        items: &[RunContextItem],
        _workspace: WorkspaceId,
    ) -> Result<CapturedContext, CaptureFailure> {
        Ok(CapturedContext {
            pins: items.to_vec(),
        })
    }

    fn materialize(
        &mut self,
        _run: RunId,
        captured: &CapturedContext,
        run_workspace: &Path,
    ) -> Result<MaterializedContext, MaterializeFailure> {
        // Names the folder the real capture would write; writes nothing.
        Ok(MaterializedContext {
            directory: run_workspace.join(".koru/context"),
            items: u32::try_from(captured.pins.len()).unwrap_or(u32::MAX),
        })
    }

    fn cleanup(&mut self, _run: RunId, _run_workspace: &Path) {}
}

/// Accepts every fact and keeps none.
pub(super) struct InertEvents;

impl RunEvents for InertEvents {
    fn publish(&mut self, _event: EventId, _fact: &RunFact) -> Result<(), PublishFailure> {
        Ok(())
    }
}

/// The same fixed text every time. A Bot's first submission does not read it.
pub(super) struct FixedPreamble;

impl Preamble for FixedPreamble {
    fn resolve(&mut self) -> koru_delegate_preamble::Resolved {
        koru_delegate_preamble::Resolved {
            text: std::borrow::Cow::Borrowed("stable fixture preamble"),
            source: koru_delegate_preamble::Source::Bundled,
            notice: None,
            path: None,
        }
    }
}

/// A Bot folder resolves to its pure fixture identity without canonicalizing
/// anything on disk. No project root and no temporary directory exist.
pub(super) struct FixtureRoots;

impl ScopeRoots for FixtureRoots {
    fn bot_folder(
        &self,
        folder: &koru_delegate_core::BotWorkingFolder,
    ) -> Result<ResolvedScope, ScopeRefusal> {
        let directory = PathBuf::from(folder.as_str());
        Ok(ResolvedScope {
            identity: ScopeIdentity::fixture_canonical(&directory),
            directory,
        })
    }

    fn project_root(&self, _project: ProjectId) -> RootLookup {
        RootLookup::UnknownProject
    }

    fn temporary_directory(&self, _run: RunId) -> Option<PathBuf> {
        None
    }
}

/// The run authority as a record: it issues the six run tools for each
/// attempt and remembers the working folder each launch named, as the real
/// store keeps it. The endpoint is a loopback address nothing listens on.
pub(super) struct RecordingMcp {
    workspaces: HashMap<RunId, PathBuf>,
    /// The last decision wait published per Run (O147), shared with the test.
    waits: PublishedWaits,
    /// The last child wait published per Run (M6), shared with the test.
    child_waits: super::PublishedChildWaits,
}

impl RecordingMcp {
    pub(super) fn new(waits: PublishedWaits, child_waits: super::PublishedChildWaits) -> Self {
        Self {
            workspaces: HashMap::new(),
            waits,
            child_waits,
        }
    }
}

type PublishedWait = (
    koru_delegate_core::decisions::DecisionId,
    koru_delegate_core::decisions::DecisionPhase,
);

/// The test's view of the decision waits the orchestrator published for each
/// Run's tool classification (O147), exactly as the host's projection would
/// receive them: `Some` while a Run classifies as waiting, absent once a read
/// proved no wait open (or before anything was published).
#[derive(Clone, Default)]
pub struct PublishedWaits(Arc<Mutex<HashMap<RunId, PublishedWait>>>);

impl std::fmt::Debug for PublishedWaits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PublishedWaits")
            .field(&self.0.lock().map(|waits| waits.len()).unwrap_or(0))
            .finish()
    }
}

impl PublishedWaits {
    /// The wait last published for `run`, if its classification is waiting.
    pub fn get(&self, run: RunId) -> Option<PublishedWait> {
        self.0
            .lock()
            .expect("the published waits are not poisoned")
            .get(&run)
            .copied()
    }

    /// Forget everything published, as a new process's projection starts.
    pub fn clear(&self) {
        self.0
            .lock()
            .expect("the published waits are not poisoned")
            .clear();
    }
}

const RUN_TOOLS: [&str; 6] = [
    "attach_to_run",
    "create",
    "create_version",
    "get",
    "read",
    "search",
];

impl McpEndpoints for RecordingMcp {
    fn publish_decision_wait(
        &mut self,
        run: RunId,
        wait: Option<(
            koru_delegate_core::decisions::DecisionId,
            koru_delegate_core::decisions::DecisionPhase,
        )>,
    ) -> PortResult<()> {
        let mut waits = self
            .waits
            .0
            .lock()
            .expect("the published waits are not poisoned");
        match wait {
            Some(wait) => {
                waits.insert(run, wait);
            }
            None => {
                waits.remove(&run);
            }
        }
        Ok(())
    }
    fn publish_child_wait(
        &mut self,
        run: RunId,
        wait: Option<crate::ChildWaitMark>,
    ) -> PortResult<()> {
        let mut waits = self
            .child_waits
            .0
            .lock()
            .expect("the published child waits are not poisoned");
        match wait {
            Some(wait) => {
                waits.insert(run, wait);
            }
            None => {
                waits.remove(&run);
            }
        }
        Ok(())
    }
    fn launch(&mut self, launch: McpLaunch<'_>) -> PortResult<DelegationLaunch> {
        self.workspaces
            .insert(launch.run, launch.run_workspace.to_path_buf());
        Ok(DelegationLaunch {
            model: None,
            mcp_endpoint: format!("http://127.0.0.1:1/v1/runs/{}/mcp", launch.run.to_storage())
                .parse()
                .expect("a loopback endpoint"),
            credential: Some(
                format!(
                    "koru_run_{}_{}",
                    launch.run.to_storage().replace('-', ""),
                    launch.generation.get()
                )
                .parse()
                .expect("a fixture bearer"),
            ),
            tools: McpToolAllowlist::new(
                RUN_TOOLS
                    .iter()
                    .map(|name| name.parse().expect("a run tool name")),
            )
            .expect("the run tool allowlist"),
            resume: None,
        })
    }

    fn run_workspace(&mut self, run: RunId) -> PortResult<Option<PathBuf>> {
        Ok(self.workspaces.get(&run).cloned())
    }
}

/// No turn registered anything.
pub(super) struct NoRegistrations;

impl ArtifactRegistrations for NoRegistrations {
    fn registered(
        &mut self,
        _run: RunId,
        _attempt: RunAttemptId,
        _turn: Option<AgentTurnId>,
    ) -> Vec<ResultReference> {
        Vec::new()
    }
}

/// One shared instant for the orchestrator and the ledger. It never moves by
/// itself; a test advances it through the `Arc` it holds.
pub(super) struct SharedClock(pub(super) Arc<AtomicI64>);

impl Clock for SharedClock {
    fn now_ms(&mut self) -> i64 {
        self.0.load(Ordering::Acquire)
    }
}

impl koru_delegate_sqlite::Clock for SharedClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::Acquire)
    }
}

/// Never waits.
pub(super) struct NoSleep;

impl Sleep for NoSleep {
    fn sleep(&mut self, _duration: Duration) {}
}
