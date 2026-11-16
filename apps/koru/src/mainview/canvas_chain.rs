// SPDX-License-Identifier: MPL-2.0

//! **A canvas's chain, run from this window** (CANVAS M5, P5.2–P5.3), and an
//! agent node run on its own (UC10).
//!
//! A person presses Run on a start node. The chain it reaches
//! (`koru_canvas_flow::chain_from`) runs a step at a time: an agent node starts
//! once everything upstream of it has a result, given the cards, images and
//! upstream agent nodes connected into it; its result is **copied into the
//! node** (Q18), so it syncs and becomes the next node's input; an output node
//! writes a page from what reaches it (P5.3). A step that fails, is stopped or
//! finishes with nothing stops what depends on it and says why (Q21, as the
//! planner builds it — CANVAS Q-8). Stop marks the rest *not started*.
//!
//! **The canvas is the state.** A press records a fresh id — an *execution* —
//! and this device on the start node; each agent node's run link and each
//! output's page link carry the execution they belong to
//! (`koru.start.execution`, `koru.start.device`, `koru.step.execution`), and
//! the start's id is cleared when the execution ends. What this module holds
//! is derived from those and from the runs, so a window that opens a canvas
//! whose chain it began before, on this device, continues it
//! ([`MainView::reconcile_canvas`]); any other device leaves it alone.
//!
//! **Everything is read and written through the store, as another writer, on
//! a worker.** [`ChainPort::edit`] joins the canvas's room with a replica of
//! its own (`koru_workspace::edit_foreign_document`), writes, waits until the
//! store has the write on its disk, and answers with what the canvas then
//! holds. So a copied result never enters the person's Ctrl+Z, the window
//! thread never waits on a store, and a step that starts after an upstream
//! copy pins a canvas that already holds it: the copy is durable before the
//! next run's context is captured. Edits to one canvas go out one at a time,
//! so every answer has seen every write before it.
//!
//! **The planner is `koru-canvas-flow`'s**, asked again whenever a step moves;
//! the runs are [`CanvasRuns`]', whose updates `maintain_canvas_runs` hands
//! here. Every word a node shows is `koru-i18n`'s — a run's through
//! `koru_runs_ui::summary`, the chain's own through its `CanvasChain*` rows.
//!
//! **The canvas surface is the caller, and it lands on its own branch.** Until
//! it calls [`MainView::run_chain`] and the rest, only this module's tests do,
//! so the build would call most of it dead; the allow below says so once and
//! goes when the surface is wired.
#![cfg_attr(not(test), allow(dead_code))]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use koru_artifacts_ui::{ArtifactVersionId, ResultBody};
use koru_canvas_flow::{
    Chain, ChainError, HaltReason, Next, StepKind, StepState, chain_from, inputs_of, plan,
};
use koru_core::canvas::{self, CanvasExecution, CanvasSnapshot, ObjectKind};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{DocumentId, NodeId};
use koru_delegate_context::ContextSource;
use koru_delegate_core::{BlobKind, BlockNodeId, ModelName, RunId, RunStatus, SessionProfileKey};
use koru_i18n::{Key, Lang, fill, t};
use koru_store_core::errors::{Code, Error};

use super::MainView;
use super::canvas_runs::{CanvasRuns, NodeRunState, Reach, ResultRead, RunUpdate};
use super::delegate::job::RunSpec;

type Proxy = winit::event_loop::EventLoopProxy<koru_workspace_host::app::Wake>;

/// A run of a chain, by where it starts: the canvas and its start node — or,
/// for an agent node run on its own, that node.
type Head = (DocumentId, NodeId);

/// The code a worker that went away without answering is refused with.
const NO_WORKER: &str = "worker_unavailable";
/// The code a window with no store client is refused with.
const UNAVAILABLE: &str = "store_unavailable";

// ------------------------------------------------------------ what a node shows

/// **What the canvas surface draws on a start, agent or output node.** The
/// words are localized already; a node this module has nothing to say about
/// is absent from [`MainView::canvas_node_states`] and is idle.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum CanvasNodeState {
    /// Nothing is running here.
    Idle,
    /// Working. `activity` is an agent's newest step as a sentence and
    /// `elapsed` how long it has worked, both in the run pill's words, once
    /// its run says so; a start node running its chain and an output node
    /// writing its page carry neither.
    Running {
        activity: Option<String>,
        elapsed: Option<String>,
    },
    /// Still running, but the provider needs a person's approval or reply.
    NeedsResponse,
    /// Stop was pressed and the run has not yet said it stopped.
    Stopping,
    Done,
    /// It failed, and why. On a start node: where its chain stopped.
    Failed(String),
    /// A person stopped it.
    Stopped,
    /// Its chain stopped before it got here.
    NotStarted,
    /// It cannot run as it stands — no instruction, no agent set up, a chain
    /// that loops — and what to do about it.
    NotReady(String),
}

// ------------------------------------------------------------ what is written

/// **One write a chain makes into its canvas**, as data, so the store's
/// worker and a test's replica apply exactly the same thing
/// ([`CanvasEdit::apply`]).
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum CanvasEdit {
    /// Nothing: read what the canvas holds.
    Read,
    /// Record a fresh run of a start node's chain, begun on `device` — only
    /// when the start has a chain to run, so a refused press leaves nothing.
    Begin {
        start: NodeId,
        execution: String,
        device: String,
    },
    /// The run has ended: clear it from the start node, unless a later press
    /// has already replaced it.
    End { start: NodeId, execution: String },
    /// Link an agent node to its latest run, in this execution.
    Run {
        node: NodeId,
        run: DocumentId,
        execution: String,
    },
    /// Keep an agent node's latest result inside it (Q18).
    Result { node: NodeId, lines: Vec<String> },
    /// Link an output node to the page it wrote, in this execution.
    Page {
        node: NodeId,
        page: DocumentId,
        execution: String,
    },
}

impl std::fmt::Debug for CanvasEdit {
    /// **A result's lines are content** (standing requirement 7): what kind
    /// of write, on which node, and how many lines.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read => f.write_str("Read"),
            Self::Begin { start, .. } => f.debug_struct("Begin").field("start", start).finish(),
            Self::End { start, .. } => f.debug_struct("End").field("start", start).finish(),
            Self::Run { node, run, .. } => f
                .debug_struct("Run")
                .field("node", node)
                .field("run", run)
                .finish(),
            Self::Result { node, lines } => f
                .debug_struct("Result")
                .field("node", node)
                .field("lines", &lines.len())
                .finish(),
            Self::Page { node, page, .. } => f
                .debug_struct("Page")
                .field("node", node)
                .field("page", page)
                .finish(),
        }
    }
}

impl CanvasEdit {
    /// Apply this to a replica of the canvas rooted at `root`.
    pub(crate) fn apply(
        &self,
        ws: &mut WorkspaceDoc,
        root: NodeId,
    ) -> koru_core::schema::Result<()> {
        match self {
            Self::Read => Ok(()),
            Self::Begin {
                start,
                execution,
                device,
            } => {
                if chain_from(&canvas::read(ws, root)?, *start).is_ok() {
                    canvas::set_execution(ws, *start, execution, device)?;
                }
                Ok(())
            }
            Self::End { start, execution } => match canvas::execution(ws, *start)? {
                Some(current) if current.id == *execution => {
                    canvas::set_execution(ws, *start, "", &current.device)
                }
                _ => Ok(()),
            },
            Self::Run {
                node,
                run,
                execution,
            } => {
                canvas::set_run(ws, *node, *run)?;
                canvas::set_step_execution(ws, *node, execution)
            }
            Self::Result { node, lines } => canvas::set_result(ws, *node, lines),
            Self::Page {
                node,
                page,
                execution,
            } => {
                canvas::set_output_page(ws, *node, *page)?;
                canvas::set_step_execution(ws, *node, execution)
            }
        }
    }
}

/// **What a canvas holds, as a chain reads it**: its objects, and the runs
/// its workflow nodes record.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct CanvasRead {
    pub(crate) snapshot: CanvasSnapshot,
    /// Each start node running its chain.
    pub(crate) starts: HashMap<NodeId, CanvasExecution>,
    /// The execution each agent and output node last ran or wrote in.
    pub(crate) steps: HashMap<NodeId, String>,
}

/// Read the canvas rooted at `root` the way a chain does.
pub(crate) fn read_canvas(
    ws: &WorkspaceDoc,
    root: NodeId,
) -> koru_core::schema::Result<CanvasRead> {
    let snapshot = canvas::read(ws, root)?;
    let mut starts = HashMap::new();
    let mut steps = HashMap::new();
    for object in snapshot.objects.iter().chain(&snapshot.arrivals) {
        match object.kind {
            ObjectKind::Start => {
                if let Some(execution) = canvas::execution(ws, object.id)? {
                    starts.insert(object.id, execution);
                }
            }
            ObjectKind::Agent | ObjectKind::Output => {
                if let Some(execution) = canvas::step_execution(ws, object.id)? {
                    steps.insert(object.id, execution);
                }
            }
            ObjectKind::Card | ObjectKind::Image => {}
        }
    }
    Ok(CanvasRead {
        snapshot,
        starts,
        steps,
    })
}

/// **One output page to write** (P5.3): the first run of an execution creates
/// it, once, however often the write is retried; a later run replaces what is
/// under its title.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PageWrite {
    pub(crate) workspace: koru_core::WorkspaceId,
    pub(crate) node: NodeId,
    pub(crate) execution: String,
    /// The output node's first line, or the localized default.
    pub(crate) title: String,
    /// What reaches the node: its upstream agents' results, in reading order.
    pub(crate) markdown: String,
    /// The page an earlier run wrote, to update in place.
    pub(crate) existing: Option<DocumentId>,
    /// The History labels either side of an update, localized.
    pub(crate) before: String,
    pub(crate) after: String,
}

impl PageWrite {
    /// **What makes a retry the same request**: the output node and the
    /// execution. The store answers a replay with the page it made.
    pub(crate) fn idempotency_key(&self) -> String {
        format!(
            "canvas-output:{}:{}",
            self.node.to_storage(),
            self.execution
        )
    }

    /// What the request says, hashed — the store's check that a replayed
    /// key carries the same request.
    fn request_hash(&self) -> String {
        let mut bytes = Vec::new();
        for part in [
            self.workspace.to_storage(),
            self.title.clone(),
            self.markdown.clone(),
        ] {
            bytes.extend_from_slice(part.as_bytes());
            bytes.push(0);
        }
        koru_store_core::checksum(&bytes)
    }
}

impl std::fmt::Debug for PageWrite {
    /// **The title and the text are content** (standing requirement 7).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageWrite")
            .field("node", &self.node)
            .field("execution", &self.execution)
            .field("existing", &self.existing)
            .field("markdown_bytes", &self.markdown.len())
            .finish_non_exhaustive()
    }
}

/// **Everything a chain does to the world besides its runs**, each on a
/// worker. The store answers in the app ([`StorePort`]); a test answers from
/// an in-memory canvas, which is how a whole chain is driven without a store.
pub(crate) trait ChainPort: Send {
    /// Apply `edit` to the canvas as another writer and answer what it then
    /// holds. `root` is the canvas's root node; `None` asks the worker to
    /// find it (a canvas this window has not shown).
    fn edit(
        &mut self,
        reach: Reach<'_>,
        canvas: DocumentId,
        root: Option<NodeId>,
        edit: CanvasEdit,
    ) -> Receiver<Result<CanvasRead, String>>;
    /// Write an output node's page, answering which page it is.
    fn page(&mut self, reach: Reach<'_>, write: PageWrite) -> Receiver<Result<DocumentId, String>>;
}

// ------------------------------------------------------------ bookkeeping

/// **Every chain this window runs, derived.** Nothing here is the only copy
/// of anything: the canvas records what each run is, and this holds what the
/// window is doing about it.
pub(crate) struct CanvasChains {
    port: Box<dyn ChainPort>,
    runs: HashMap<Head, ChainRun>,
    next_seq: u64,
    /// Edits waiting their turn, per canvas.
    queued: HashMap<DocumentId, VecDeque<Queued>>,
    /// The one edit per canvas on a worker.
    editing: HashMap<DocumentId, Editing>,
    pages: Vec<(Head, NodeId, Receiver<Result<DocumentId, String>>)>,
    /// The runs' updates since the last pass, from `maintain_canvas_runs`.
    heard: Vec<RunUpdate>,
    /// Canvases a surface asked about. A cell, because asking is a `&self`
    /// read the surface makes while drawing, and being asked is how a canvas
    /// this window opened is noticed.
    noticed: RefCell<HashSet<DocumentId>>,
    /// Canvases already read for chains this device began before.
    reconciled: HashSet<DocumentId>,
}

struct Queued {
    root: Option<NodeId>,
    edit: CanvasEdit,
    purpose: Purpose,
}

struct Editing {
    purpose: Purpose,
    answer: Receiver<Result<CanvasRead, String>>,
}

/// What an edit's answer is for.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Purpose {
    /// A press of Run: the chain is read from the answer.
    Begin(Head),
    /// A result copied into a node: the step finishes on the answer.
    Result(Head, NodeId),
    /// A link, or the end of a run: nothing waits on it.
    Record,
    /// A canvas this window opened, read once for chains it began before.
    Reconcile,
}

/// One run of a chain.
struct ChainRun {
    /// When it was pressed, relative to the others: the later wins a node
    /// two runs share.
    seq: u64,
    canvas: DocumentId,
    root: NodeId,
    head: NodeId,
    /// An agent node run on its own (UC10) rather than a start node's chain.
    alone: bool,
    execution: String,
    /// `None` until the canvas has answered the press.
    chain: Option<Chain>,
    read: Option<CanvasRead>,
    steps: HashMap<NodeId, Step>,
    /// Steps that will not run because something upstream halted.
    blocked: HashSet<NodeId>,
    head_state: CanvasNodeState,
    stopping: bool,
    over: bool,
}

struct Step {
    kind: StepKind,
    /// What the planner is told.
    state: StepState,
    phase: Phase,
    shown: CanvasNodeState,
    /// The run this step started, once known.
    run: Option<RunId>,
    /// The Run document already linked on the node.
    linked: Option<DocumentId>,
    /// The result version being read.
    reading: Option<ArtifactVersionId>,
    /// What that result is called, for a result that is not text.
    result_title: String,
    /// The result as copied into the node, for an output downstream.
    result: Vec<String>,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Phase {
    /// Not started.
    Waiting,
    /// Waiting for this machine's agents to be known.
    Agents,
    /// A run is working.
    Running,
    /// Its run finished with a result, which is being read.
    Reading,
    /// The result is being copied into the node.
    Copying,
    /// An output page is being written.
    Writing,
    Settled,
}

impl Step {
    fn new(kind: StepKind) -> Self {
        Self {
            kind,
            state: StepState::NotStarted,
            phase: Phase::Waiting,
            shown: CanvasNodeState::Idle,
            run: None,
            linked: None,
            reading: None,
            result_title: String::new(),
            result: Vec::new(),
        }
    }

    fn busy(&mut self, phase: Phase, shown: CanvasNodeState) {
        self.state = StepState::Running;
        self.phase = phase;
        self.shown = shown;
    }

    fn settle(&mut self, state: StepState, shown: CanvasNodeState) {
        self.state = state;
        self.phase = Phase::Settled;
        self.shown = shown;
    }

    fn in_flight(&self) -> bool {
        !matches!(self.phase, Phase::Waiting | Phase::Settled)
    }
}

impl ChainRun {
    fn new(seq: u64, head: Head, root: NodeId, alone: bool, execution: String) -> Self {
        Self {
            seq,
            canvas: head.0,
            root,
            head: head.1,
            alone,
            execution,
            chain: None,
            read: None,
            steps: HashMap::new(),
            blocked: HashSet::new(),
            head_state: CanvasNodeState::Running {
                activity: None,
                elapsed: None,
            },
            stopping: false,
            over: false,
        }
    }

    /// What a step shows: its own state once it has one, and before that
    /// whether it will still run.
    fn shown(&self, node: NodeId, step: &Step) -> CanvasNodeState {
        if step.phase != Phase::Waiting {
            return step.shown.clone();
        }
        if self.alone {
            return self.head_state.clone();
        }
        if self.over || self.stopping || self.blocked.contains(&node) {
            CanvasNodeState::NotStarted
        } else {
            CanvasNodeState::Idle
        }
    }
}

impl Default for CanvasChains {
    fn default() -> Self {
        Self::with_port(Box::new(StorePort))
    }
}

impl CanvasChains {
    pub(crate) fn with_port(port: Box<dyn ChainPort>) -> Self {
        Self {
            port,
            runs: HashMap::new(),
            next_seq: 0,
            queued: HashMap::new(),
            editing: HashMap::new(),
            pages: Vec::new(),
            heard: Vec::new(),
            noticed: RefCell::new(HashSet::new()),
            reconciled: HashSet::new(),
        }
    }

    /// The runs' updates from this pass, for the chains they belong to.
    pub(crate) fn heard(&mut self, updates: Vec<RunUpdate>) {
        if !updates.is_empty() {
            self.heard.extend(updates);
        }
    }

    /// Whether a pass has nothing to do: nothing heard, nothing on a worker,
    /// nothing waiting to be read, and no canvas newly shown.
    fn idle(&self) -> bool {
        self.heard.is_empty()
            && self.editing.is_empty()
            && self.pages.is_empty()
            && self.queued.values().all(VecDeque::is_empty)
            && self.noticed.borrow().len() <= self.reconciled.len()
            && !self.runs.values().any(|run| {
                !run.over
                    && run
                        .steps
                        .values()
                        .any(|step| matches!(step.phase, Phase::Agents | Phase::Reading))
            })
    }

    fn seq(&mut self) -> u64 {
        self.next_seq += 1;
        self.next_seq
    }

    fn queue(
        &mut self,
        canvas: DocumentId,
        root: Option<NodeId>,
        edit: CanvasEdit,
        purpose: Purpose,
    ) {
        self.queued.entry(canvas).or_default().push_back(Queued {
            root,
            edit,
            purpose,
        });
    }

    /// Whether `node` is running, or waiting its turn, in a run on `canvas`
    /// — a press of its own Run waits for that run to end, so one node never
    /// has two runs the table would confuse.
    fn busy(&self, canvas: DocumentId, node: NodeId) -> bool {
        self.runs.values().any(|run| {
            run.canvas == canvas
                && !run.over
                && (run.head == node
                    || run
                        .steps
                        .get(&node)
                        .is_some_and(|step| step.phase != Phase::Settled))
        })
    }

    /// The live run whose step `node` is waiting on its run, when `run` is
    /// that run (or not yet known).
    fn owner(&self, canvas: DocumentId, node: NodeId, run: Option<RunId>) -> Option<Head> {
        self.runs
            .iter()
            .filter(|(_, chain)| chain.canvas == canvas && !chain.over)
            .find(|(_, chain)| {
                chain.steps.get(&node).is_some_and(|step| {
                    step.phase == Phase::Running
                        && (run.is_none() || step.run.is_none() || step.run == run)
                })
            })
            .map(|(head, _)| *head)
    }

    /// Whether a result read is one a step is waiting for.
    fn expects(&self, read: &ResultRead) -> bool {
        self.reading(read).is_some()
    }

    fn reading(&self, read: &ResultRead) -> Option<Head> {
        let (canvas, node) = read.key;
        self.runs
            .iter()
            .filter(|(_, chain)| chain.canvas == canvas && !chain.over)
            .find(|(_, chain)| {
                chain.steps.get(&node).is_some_and(|step| {
                    step.phase == Phase::Reading && step.reading == Some(read.version)
                })
            })
            .map(|(head, _)| *head)
    }
}

// ------------------------------------------------------------ the window

/// **Test hooks the canvas tab's container tests drive** (they were written
/// against a stand-in of this module while it was being built). A test records
/// every Run and Stop the tab asks for, and may inject the node states a canvas
/// shows; in a shipping build none of this exists.
#[cfg(test)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum ChainCall {
    RunChain {
        canvas: DocumentId,
        root: NodeId,
        start: NodeId,
    },
    RunAgentNode {
        canvas: DocumentId,
        root: NodeId,
        node: NodeId,
    },
    StopChain {
        canvas: DocumentId,
        start: NodeId,
    },
    StopNode {
        canvas: DocumentId,
        node: NodeId,
    },
}

#[cfg(test)]
thread_local! {
    static CALLS: std::cell::RefCell<Vec<ChainCall>> = const { std::cell::RefCell::new(Vec::new()) };
    static STATES: std::cell::RefCell<HashMap<DocumentId, HashMap<NodeId, CanvasNodeState>>> =
        std::cell::RefCell::new(HashMap::new());
    static MOVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Every Run and Stop recorded on this thread since the last call.
#[cfg(test)]
pub(crate) fn take_calls() -> Vec<ChainCall> {
    CALLS.with(|calls| std::mem::take(&mut *calls.borrow_mut()))
}

/// Make `canvas_node_states(canvas)` answer `states`, and the next
/// `maintain_canvas_chains` report a change.
#[cfg(test)]
pub(crate) fn set_states(canvas: DocumentId, states: HashMap<NodeId, CanvasNodeState>) {
    STATES.with(|all| {
        all.borrow_mut().insert(canvas, states);
    });
    MOVED.with(|moved| moved.set(true));
}

#[cfg(test)]
fn record(call: ChainCall) {
    CALLS.with(|calls| calls.borrow_mut().push(call));
}

impl MainView {
    /// **Run a start node's chain.** Reads the canvas as the store holds it,
    /// builds the chain, records a fresh execution and this device on the
    /// start node, then starts what is ready — all on workers, answered on
    /// later passes. A chain that cannot run (it loops, or reaches nothing)
    /// says why on the start node. Pressing again while it runs does nothing.
    pub(crate) fn run_chain(&mut self, canvas: DocumentId, root: NodeId, start: NodeId) {
        #[cfg(test)]
        record(ChainCall::RunChain {
            canvas,
            root,
            start,
        });
        let head = (canvas, start);
        if self
            .canvas_chains
            .runs
            .get(&head)
            .is_some_and(|run| !run.over)
        {
            return;
        }
        let execution = uuid::Uuid::new_v4().to_string();
        let device = self
            .this_device
            .map(|device| device.to_storage())
            .unwrap_or_default();
        let seq = self.canvas_chains.seq();
        self.canvas_chains.runs.insert(
            head,
            ChainRun::new(seq, head, root, false, execution.clone()),
        );
        self.canvas_chains.queue(
            canvas,
            Some(root),
            CanvasEdit::Begin {
                start,
                execution,
                device,
            },
            Purpose::Begin(head),
        );
    }

    /// **Run one agent node on its own** (UC10): the same step a chain would
    /// take, with no start node and nothing downstream. Does nothing while
    /// the node is already running.
    pub(crate) fn run_agent_node(&mut self, canvas: DocumentId, root: NodeId, node: NodeId) {
        #[cfg(test)]
        record(ChainCall::RunAgentNode { canvas, root, node });
        if self.canvas_chains.busy(canvas, node) {
            return;
        }
        let head = (canvas, node);
        let seq = self.canvas_chains.seq();
        let execution = uuid::Uuid::new_v4().to_string();
        let mut run = ChainRun::new(seq, head, root, true, execution);
        run.steps.insert(node, Step::new(StepKind::Agent));
        self.canvas_chains.runs.insert(head, run);
        self.canvas_chains
            .queue(canvas, Some(root), CanvasEdit::Read, Purpose::Begin(head));
    }

    /// **Stop a start node's chain**: its running steps are stopped, and the
    /// rest of that execution is marked not started.
    pub(crate) fn stop_chain(&mut self, canvas: DocumentId, start: NodeId) {
        #[cfg(test)]
        record(ChainCall::StopChain { canvas, start });
        self.stop_run((canvas, start));
    }

    /// **Stop the run a node is part of** — its own, or the chain it is a
    /// step of. What had not started is marked not started.
    pub(crate) fn stop_node(&mut self, canvas: DocumentId, node: NodeId) {
        #[cfg(test)]
        record(ChainCall::StopNode { canvas, node });
        let head = self
            .canvas_chains
            .runs
            .iter()
            .filter(|(_, run)| run.canvas == canvas && !run.over)
            .find(|(_, run)| run.head == node || run.steps.get(&node).is_some_and(Step::in_flight))
            .map(|(head, _)| *head);
        if let Some(head) = head {
            self.stop_run(head);
        }
    }

    /// **What each workflow node on `canvas` shows**, for the surface to
    /// draw. A node absent from the answer is idle. Where two runs share a
    /// node, the later press wins.
    ///
    /// Asking is also how this window notices a canvas it has opened: the
    /// next pass reads it once for a chain this device began before and did
    /// not finish, and continues it.
    pub(crate) fn canvas_node_states(
        &self,
        canvas: DocumentId,
    ) -> HashMap<NodeId, CanvasNodeState> {
        #[cfg(test)]
        if let Some(states) = STATES.with(|all| all.borrow().get(&canvas).cloned()) {
            return states;
        }
        self.canvas_chains.noticed.borrow_mut().insert(canvas);
        let mut runs: Vec<&ChainRun> = self
            .canvas_chains
            .runs
            .values()
            .filter(|run| run.canvas == canvas)
            .collect();
        runs.sort_by_key(|run| run.seq);
        let mut states = HashMap::new();
        for run in runs {
            if !run.alone {
                states.insert(run.head, run.head_state.clone());
            }
            for (node, step) in &run.steps {
                states.insert(*node, run.shown(*node, step));
            }
        }
        states
    }

    /// **One pass over the chains**, right after `maintain_canvas_runs`: the
    /// runs' updates and results heard, the store's answers folded in, the
    /// planner asked again, and what is ready started. Never waits; an idle
    /// window returns after a few comparisons.
    pub(crate) fn maintain_canvas_chains(&mut self) -> bool {
        #[cfg(test)]
        if MOVED.with(|moved| moved.replace(false)) {
            return true;
        }
        if self.canvas_chains.idle() {
            return false;
        }
        let lang = crate::format::lang();
        let mut changed = self.notice_canvases();

        for update in std::mem::take(&mut self.canvas_chains.heard) {
            changed |= self.hear_run(update, lang);
        }

        let chains = &self.canvas_chains;
        let reads = self
            .canvas_runs
            .take_results_where(|read| chains.expects(read));
        for read in reads {
            changed = true;
            self.hear_result(read, lang);
        }

        changed |= self.retry_agents(lang);

        let canvases: Vec<DocumentId> = self.canvas_chains.editing.keys().copied().collect();
        for canvas in canvases {
            let answer = match self.canvas_chains.editing[&canvas].answer.try_recv() {
                Ok(answer) => answer,
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => Err(NO_WORKER.to_owned()),
            };
            let editing = self
                .canvas_chains
                .editing
                .remove(&canvas)
                .expect("an edit in flight");
            changed = true;
            self.hear_edit(canvas, editing.purpose, answer, lang);
        }

        let mut index = 0;
        while index < self.canvas_chains.pages.len() {
            let answer = match self.canvas_chains.pages[index].2.try_recv() {
                Ok(answer) => answer,
                Err(TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => Err(NO_WORKER.to_owned()),
            };
            let (head, node, _) = self.canvas_chains.pages.swap_remove(index);
            changed = true;
            self.hear_page(head, node, answer, lang);
        }

        self.launch_canvas_edits();
        changed
    }

    // ------------------------------------------------------------ inside

    /// The workspace the canvas is in: this window's.
    fn chain_workspace(&self) -> Option<koru_core::WorkspaceId> {
        self.pane_context
            .as_ref()
            .and_then(|(_, _, _, workspace)| *workspace)
    }

    /// Hand each canvas's next edit to a worker, one per canvas at a time.
    fn launch_canvas_edits(&mut self) {
        let ready: Vec<DocumentId> = self
            .canvas_chains
            .queued
            .iter()
            .filter(|(canvas, queue)| {
                !queue.is_empty() && !self.canvas_chains.editing.contains_key(*canvas)
            })
            .map(|(canvas, _)| *canvas)
            .collect();
        if ready.is_empty() {
            return;
        }
        let wake = self.agent.wake_proxy();
        let workspace = self.chain_workspace().map(|workspace| workspace.as_uuid());
        for canvas in ready {
            let Some(queued) = self
                .canvas_chains
                .queued
                .get_mut(&canvas)
                .and_then(VecDeque::pop_front)
            else {
                continue;
            };
            let answer = self.canvas_chains.port.edit(
                Reach {
                    client: self.delegate_client.as_ref(),
                    wake: wake.as_ref(),
                    workspace,
                },
                canvas,
                queued.root,
                queued.edit,
            );
            self.canvas_chains.editing.insert(
                canvas,
                Editing {
                    purpose: queued.purpose,
                    answer,
                },
            );
        }
    }

    fn stop_run(&mut self, head: Head) {
        let Some(run) = self.canvas_chains.runs.get_mut(&head) else {
            return;
        };
        if run.over || run.stopping {
            return;
        }
        run.stopping = true;
        run.head_state = CanvasNodeState::Stopping;
        let mut stops = Vec::new();
        for step in run.steps.values_mut() {
            match step.phase {
                Phase::Running => {
                    // A run adopted from an earlier session is stopped once
                    // its first read names it (`hear_run`).
                    stops.extend(step.run);
                    step.shown = CanvasNodeState::Stopping;
                }
                // Never started: it goes back to not started.
                Phase::Agents => *step = Step::new(step.kind),
                _ => {}
            }
        }
        for run in stops {
            self.canvas_runs.stop(run);
        }
        self.advance(head);
    }

    /// **Ask the planner again and do what it says**, until nothing more can
    /// start: start agent steps, write output pages, and end the run once
    /// nothing is in flight.
    fn advance(&mut self, head: Head) {
        let lang = crate::format::lang();
        loop {
            let Some(run) = self.canvas_chains.runs.get_mut(&head) else {
                return;
            };
            if run.over {
                return;
            }
            let Some(chain) = run.chain.clone() else {
                return;
            };
            let states: HashMap<NodeId, StepState> = run
                .steps
                .iter()
                .map(|(node, step)| (*node, step.state))
                .collect();
            let mut starts = Vec::new();
            let mut writes = Vec::new();
            let mut halts = Vec::new();
            let mut done = false;
            for next in plan(&chain, &states) {
                match next {
                    Next::StartAgent(node) => starts.push(node),
                    Next::WriteOutput(node) => writes.push(node),
                    Next::Halted { at, reason } => halts.push((at, reason)),
                    Next::Done => done = true,
                    Next::Wait => {}
                }
            }
            run.blocked = blocked(&chain, &states);
            if run.stopping {
                starts.clear();
                writes.clear();
            }
            if starts.is_empty() && writes.is_empty() {
                if run.steps.values().any(Step::in_flight) {
                    run.head_state = if run.stopping {
                        CanvasNodeState::Stopping
                    } else {
                        CanvasNodeState::Running {
                            activity: None,
                            elapsed: None,
                        }
                    };
                    return;
                }
                run.over = true;
                run.head_state = ended(run, done, &halts, lang);
                if !run.alone {
                    let edit = CanvasEdit::End {
                        start: run.head,
                        execution: run.execution.clone(),
                    };
                    let (canvas, root) = (run.canvas, run.root);
                    self.canvas_chains
                        .queue(canvas, Some(root), edit, Purpose::Record);
                }
                return;
            }
            for node in starts {
                self.start_agent_step(head, node, lang);
            }
            for node in writes {
                self.start_output_step(head, node, lang);
            }
            // A step that could not start settled at once: plan again.
        }
    }

    fn step_mut(&mut self, head: Head, node: NodeId) -> Option<&mut Step> {
        self.canvas_chains.runs.get_mut(&head)?.steps.get_mut(&node)
    }

    /// Start one agent step: its instruction, its agent and its inputs from
    /// the canvas as last read, through `CanvasRuns`.
    fn start_agent_step(&mut self, head: Head, node: NodeId, lang: Lang) {
        let starting = CanvasNodeState::Running {
            activity: None,
            elapsed: None,
        };
        match self.prepare_agent(head, node, lang) {
            Prepared::Spec(spec) => {
                let run = self.canvas_runs.start((head.0, node), *spec);
                if let Some(step) = self.step_mut(head, node) {
                    step.busy(Phase::Running, starting);
                    step.run = Some(run);
                    step.linked = None;
                    step.reading = None;
                }
            }
            Prepared::Agents => {
                self.canvas_runs.want_agents();
                if let Some(step) = self.step_mut(head, node) {
                    step.busy(Phase::Agents, starting);
                }
            }
            Prepared::Refused(shown) => {
                if let Some(step) = self.step_mut(head, node) {
                    step.settle(StepState::Failed, shown);
                }
            }
        }
    }

    /// The spec an agent step runs with, or why it cannot run.
    fn prepare_agent(&self, head: Head, node: NodeId, lang: Lang) -> Prepared {
        let unavailable = || {
            Prepared::Refused(CanvasNodeState::Failed(
                t(lang, Key::DelegateUnavailable).into(),
            ))
        };
        let Some(read) = self
            .canvas_chains
            .runs
            .get(&head)
            .and_then(|run| run.read.as_ref())
        else {
            return unavailable();
        };
        let Some(object) = read.snapshot.get(node) else {
            return Prepared::Refused(CanvasNodeState::Failed(fill(
                t(lang, Key::CanvasChainWriteFailed),
                &["not_on_canvas"],
            )));
        };
        let objective = objective_of(object);
        if objective.is_empty() {
            return Prepared::Refused(CanvasNodeState::NotReady(
                t(lang, Key::CanvasChainNoInstruction).into(),
            ));
        }
        if !self.canvas_runs.agents_known() {
            return Prepared::Agents;
        }
        let Some(agent) = choose_agent(object.agent.as_deref(), &self.canvas_runs) else {
            return Prepared::Refused(CanvasNodeState::NotReady(
                t(lang, Key::DelegateNoAgents).into(),
            ));
        };
        let Some(workspace) = self.chain_workspace() else {
            return unavailable();
        };
        let sources = sources_for(&read.snapshot, head.0, workspace, node);
        let Some(mut spec) = self.canvas_run_spec(objective, agent, sources) else {
            return unavailable();
        };
        spec.model = object
            .agent_model
            .as_deref()
            .and_then(|model| ModelName::new(model).ok());
        Prepared::Spec(Box::new(spec))
    }

    /// Start writing an output node's page, on a worker.
    fn start_output_step(&mut self, head: Head, node: NodeId, lang: Lang) {
        match self.prepare_page(head, node, lang) {
            Ok(write) => {
                let wake = self.agent.wake_proxy();
                let workspace = Some(write.workspace.as_uuid());
                let answer = self.canvas_chains.port.page(
                    Reach {
                        client: self.delegate_client.as_ref(),
                        wake: wake.as_ref(),
                        workspace,
                    },
                    write,
                );
                self.canvas_chains.pages.push((head, node, answer));
                if let Some(step) = self.step_mut(head, node) {
                    step.busy(
                        Phase::Writing,
                        CanvasNodeState::Running {
                            activity: None,
                            elapsed: None,
                        },
                    );
                }
            }
            Err(shown) => {
                if let Some(step) = self.step_mut(head, node) {
                    step.settle(StepState::Failed, shown);
                }
            }
        }
    }

    /// The page an output step writes: what its upstream agent nodes handed
    /// on, in the canvas's reading order, under the node's first line.
    fn prepare_page(
        &self,
        head: Head,
        node: NodeId,
        lang: Lang,
    ) -> Result<PageWrite, CanvasNodeState> {
        let unavailable = || CanvasNodeState::Failed(t(lang, Key::DelegateUnavailable).into());
        let run = self.canvas_chains.runs.get(&head).ok_or_else(unavailable)?;
        let (Some(read), Some(chain)) = (run.read.as_ref(), run.chain.as_ref()) else {
            return Err(unavailable());
        };
        let Some(object) = read.snapshot.get(node) else {
            return Err(CanvasNodeState::Failed(fill(
                t(lang, Key::CanvasChainWriteFailed),
                &["not_on_canvas"],
            )));
        };
        let markdown = inputs_of(&read.snapshot, node)
            .into_iter()
            .filter(|input| chain.edges.contains(&(*input, node)))
            .filter_map(|agent| {
                let lines = match run.steps.get(&agent) {
                    Some(step) if !step.result.is_empty() => step.result.clone(),
                    _ => read.snapshot.get(agent)?.result.clone(),
                };
                let text = lines.join("\n");
                (!text.trim().is_empty()).then_some(text)
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let title = match object.text.trim() {
            "" => t(lang, Key::CanvasChainPageTitle).to_owned(),
            text => text.to_owned(),
        };
        let short: String = run.execution.chars().take(8).collect();
        let workspace = self.chain_workspace().ok_or_else(unavailable)?;
        Ok(PageWrite {
            workspace,
            node,
            execution: run.execution.clone(),
            before: fill(t(lang, Key::CanvasChainPageBefore), &[&title, &short]),
            after: fill(t(lang, Key::CanvasChainPageAfter), &[&title, &short]),
            title,
            markdown,
            existing: object.output_page,
        })
    }

    /// One run's update, for the step waiting on it.
    fn hear_run(&mut self, update: RunUpdate, lang: Lang) -> bool {
        let (canvas, node) = update.key;
        let Some(head) = self.canvas_chains.owner(canvas, node, update.run) else {
            return false;
        };
        let run = self.canvas_chains.runs.get_mut(&head).expect("its owner");
        let (execution, root, stopping) = (run.execution.clone(), run.root, run.stopping);
        let step = run.steps.get_mut(&node).expect("its owner");
        if step.run.is_none() {
            step.run = update.run;
        }
        let link = update
            .run_document
            .filter(|document| step.linked != Some(*document));
        if link.is_some() {
            step.linked = link;
        }
        let mut read = None;
        let mut stop = None;
        match &update.state {
            // Stop was pressed before this run could be named.
            NodeRunState::Starting | NodeRunState::Running if stopping => {
                stop = step.run;
                step.shown = CanvasNodeState::Stopping;
            }
            NodeRunState::Starting => {
                step.shown = CanvasNodeState::Running {
                    activity: None,
                    elapsed: None,
                }
            }
            NodeRunState::Running => {
                step.shown = if update.needs_response {
                    CanvasNodeState::NeedsResponse
                } else {
                    CanvasNodeState::Running {
                        activity: update.activity.clone(),
                        elapsed: update.elapsed_ms.and_then(|ms| elapsed_words(lang, ms)),
                    }
                };
            }
            NodeRunState::Stopping => step.shown = CanvasNodeState::Stopping,
            NodeRunState::Finished { has_result: true } => match update.results.last() {
                // The newest result is the node's (Q18).
                Some(result) => {
                    step.phase = Phase::Reading;
                    step.reading = Some(result.artifact);
                    step.result_title = result.title.as_str().to_owned();
                    read = Some(result.artifact);
                }
                None => step.settle(
                    StepState::Finished { has_result: false },
                    CanvasNodeState::Failed(t(lang, Key::CanvasChainNoResult).into()),
                ),
            },
            NodeRunState::Finished { has_result: false } => step.settle(
                StepState::Finished { has_result: false },
                CanvasNodeState::Failed(t(lang, Key::CanvasChainNoResult).into()),
            ),
            NodeRunState::Failed(reason) => {
                step.settle(StepState::Failed, CanvasNodeState::Failed(reason.clone()))
            }
            NodeRunState::Stopped => step.settle(StepState::Stopped, CanvasNodeState::Stopped),
            NodeRunState::NotReady(reason) => {
                step.settle(StepState::Failed, CanvasNodeState::NotReady(reason.clone()))
            }
        }
        if let Some(document) = link {
            self.canvas_chains.queue(
                canvas,
                Some(root),
                CanvasEdit::Run {
                    node,
                    run: document,
                    execution,
                },
                Purpose::Record,
            );
        }
        if let Some(version) = read {
            self.canvas_runs.read_result((canvas, node), version);
        }
        if let Some(run) = stop {
            self.canvas_runs.stop(run);
        }
        self.advance(head);
        true
    }

    /// A result read for a step: copied into its node as lines.
    fn hear_result(&mut self, read: ResultRead, lang: Lang) {
        let Some(head) = self.canvas_chains.reading(&read) else {
            return;
        };
        let (canvas, node) = read.key;
        let run = self.canvas_chains.runs.get_mut(&head).expect("reading");
        let root = run.root;
        let step = run.steps.get_mut(&node).expect("reading");
        step.reading = None;
        let lines = match read.body {
            Ok(ResultBody::Markdown(text)) => markdown_lines(&text),
            // No image node yet: a line that names it (CANVAS Q-12).
            Ok(ResultBody::Image { .. }) => vec![fill(
                t(lang, Key::CanvasChainResultImage),
                &[&step.result_title],
            )],
            Ok(ResultBody::Other { .. }) => vec![fill(
                t(lang, Key::CanvasChainResultFile),
                &[&step.result_title],
            )],
            Err(reason) => {
                step.settle(StepState::Failed, CanvasNodeState::Failed(reason));
                self.advance(head);
                return;
            }
        };
        if lines.is_empty() {
            step.settle(
                StepState::Finished { has_result: false },
                CanvasNodeState::Failed(t(lang, Key::CanvasChainNoResult).into()),
            );
            self.advance(head);
            return;
        }
        step.phase = Phase::Copying;
        step.result = lines.clone();
        self.canvas_chains.queue(
            canvas,
            Some(root),
            CanvasEdit::Result { node, lines },
            Purpose::Result(head, node),
        );
    }

    /// Steps that waited for this machine's agents, once they are known.
    fn retry_agents(&mut self, lang: Lang) -> bool {
        if !self.canvas_runs.agents_known() {
            return false;
        }
        let waiting: Vec<(Head, NodeId)> = self
            .canvas_chains
            .runs
            .iter()
            .filter(|(_, run)| !run.over)
            .flat_map(|(head, run)| {
                run.steps
                    .iter()
                    .filter(|(_, step)| step.phase == Phase::Agents)
                    .map(|(node, _)| (*head, *node))
            })
            .collect();
        for &(head, node) in &waiting {
            self.start_agent_step(head, node, lang);
            self.advance(head);
        }
        !waiting.is_empty()
    }

    /// The store answered an edit.
    fn hear_edit(
        &mut self,
        canvas: DocumentId,
        purpose: Purpose,
        answer: Result<CanvasRead, String>,
        lang: Lang,
    ) {
        if let Ok(read) = &answer {
            self.refresh_read(canvas, read);
        }
        match purpose {
            Purpose::Begin(head) => self.begun(head, answer, lang),
            Purpose::Result(head, node) => {
                let Some(step) = self.step_mut(head, node) else {
                    return;
                };
                if step.phase != Phase::Copying {
                    return;
                }
                match answer {
                    Ok(_) => step.settle(
                        StepState::Finished { has_result: true },
                        CanvasNodeState::Done,
                    ),
                    Err(code) => step.settle(
                        StepState::Failed,
                        CanvasNodeState::Failed(fill(
                            t(lang, Key::CanvasChainWriteFailed),
                            &[&code],
                        )),
                    ),
                }
                self.advance(head);
            }
            Purpose::Record => {
                if let Err(code) = answer {
                    // Ids and a code, never the canvas's text.
                    tracing::warn!(
                        canvas = %canvas.to_storage(),
                        "a canvas chain could not record a run on its canvas: {code}"
                    );
                }
            }
            Purpose::Reconcile => {
                if let Ok(read) = answer {
                    self.reconcile_canvas(canvas, read);
                }
            }
        }
    }

    /// Every live run on `canvas` reads it as it now is. A start node pressed
    /// again elsewhere — on another device, or in another window — has a new
    /// run that advances from there; this one stops advancing.
    fn refresh_read(&mut self, canvas: DocumentId, read: &CanvasRead) {
        for run in self.canvas_chains.runs.values_mut() {
            if run.canvas != canvas || run.over || run.root != read.snapshot.root {
                continue;
            }
            let superseded = run.chain.is_some()
                && !run.alone
                && read.starts.get(&run.head).map(|execution| &execution.id)
                    != Some(&run.execution);
            if superseded {
                run.over = true;
                run.head_state = CanvasNodeState::Idle;
                for step in run.steps.values_mut() {
                    if step.in_flight() {
                        step.phase = Phase::Settled;
                        step.shown = CanvasNodeState::Idle;
                    }
                }
                continue;
            }
            run.read = Some(read.clone());
        }
    }

    /// The canvas answered a press: build the chain, and start it.
    fn begun(&mut self, head: Head, answer: Result<CanvasRead, String>, lang: Lang) {
        let Some(run) = self.canvas_chains.runs.get_mut(&head) else {
            return;
        };
        if run.over || run.chain.is_some() {
            return;
        }
        let read = match answer {
            Ok(read) => read,
            Err(code) => {
                run.over = true;
                run.head_state =
                    CanvasNodeState::Failed(fill(t(lang, Key::CanvasChainWriteFailed), &[&code]));
                return;
            }
        };
        let chain = if run.alone {
            match read.snapshot.get(run.head).map(|object| object.kind) {
                Some(ObjectKind::Agent) => Ok(Chain {
                    start: run.head,
                    steps: vec![(run.head, StepKind::Agent)],
                    edges: Vec::new(),
                }),
                // Not an agent node (any longer): nothing to run, and
                // nothing to say about it.
                _ => {
                    run.over = true;
                    run.head_state = CanvasNodeState::Idle;
                    return;
                }
            }
        } else {
            chain_from(&read.snapshot, run.head)
        };
        match chain {
            Err(error) => {
                run.over = true;
                run.head_state =
                    CanvasNodeState::NotReady(chain_refusal(lang, &read.snapshot, error));
            }
            Ok(chain) => {
                for (node, kind) in &chain.steps {
                    run.steps.entry(*node).or_insert_with(|| Step::new(*kind));
                }
                run.chain = Some(chain);
            }
        }
        run.read = Some(read);
        self.advance(head);
    }

    /// The page an output step wrote, or why it could not.
    fn hear_page(
        &mut self,
        head: Head,
        node: NodeId,
        answer: Result<DocumentId, String>,
        lang: Lang,
    ) {
        let Some(run) = self.canvas_chains.runs.get_mut(&head) else {
            return;
        };
        let (canvas, root, execution) = (run.canvas, run.root, run.execution.clone());
        let Some(step) = run.steps.get_mut(&node) else {
            return;
        };
        if step.phase != Phase::Writing {
            return;
        }
        match answer {
            Ok(page) => {
                step.settle(
                    StepState::Finished { has_result: true },
                    CanvasNodeState::Done,
                );
                self.canvas_chains.queue(
                    canvas,
                    Some(root),
                    CanvasEdit::Page {
                        node,
                        page,
                        execution,
                    },
                    Purpose::Record,
                );
            }
            Err(code) => step.settle(
                StepState::Failed,
                CanvasNodeState::Failed(fill(t(lang, Key::CanvasChainPageFailed), &[&code])),
            ),
        }
        self.advance(head);
    }

    /// Canvases a surface has asked about since the last pass, each read
    /// once for a chain this device began and did not finish. Waits until
    /// this device knows its own id: without it, no run is this device's.
    fn notice_canvases(&mut self) -> bool {
        if self.this_device.is_none() {
            return false;
        }
        let fresh: Vec<DocumentId> = self
            .canvas_chains
            .noticed
            .borrow()
            .iter()
            .filter(|canvas| !self.canvas_chains.reconciled.contains(*canvas))
            .copied()
            .collect();
        for &canvas in &fresh {
            self.canvas_chains.reconciled.insert(canvas);
            self.canvas_chains
                .queue(canvas, None, CanvasEdit::Read, Purpose::Reconcile);
        }
        !fresh.is_empty()
    }

    /// **Continue what this device began** (P5.2: closing the window pauses a
    /// chain; opening the canvas again continues it). For each start node
    /// running a chain begun on this device that this window is not already
    /// running: an agent step that ran in that execution is followed again
    /// through its Run document, an output that wrote in it is done, and
    /// everything else has not started — so the planner starts what is ready.
    /// A run begun on another device is that device's to advance.
    fn reconcile_canvas(&mut self, canvas: DocumentId, read: CanvasRead) {
        let Some(here) = self.this_device.map(|device| device.to_storage()) else {
            return;
        };
        let root = read.snapshot.root;
        let mut starts: Vec<(NodeId, CanvasExecution)> = read
            .starts
            .iter()
            .filter(|(_, execution)| execution.device == here)
            .map(|(start, execution)| (*start, execution.clone()))
            .collect();
        starts.sort_by_key(|(start, _)| start.as_uuid());
        for (start, execution) in starts {
            let head = (canvas, start);
            if self.canvas_chains.runs.contains_key(&head) {
                continue;
            }
            let Ok(chain) = chain_from(&read.snapshot, start) else {
                continue;
            };
            let seq = self.canvas_chains.seq();
            let mut run = ChainRun::new(seq, head, root, false, execution.id.clone());
            let mut adopt = Vec::new();
            for (node, kind) in &chain.steps {
                let mut step = Step::new(*kind);
                if read.steps.get(node) == Some(&execution.id) {
                    match kind {
                        StepKind::Agent => {
                            if let Some(document) =
                                read.snapshot.get(*node).and_then(|object| object.run)
                            {
                                step.busy(
                                    Phase::Running,
                                    CanvasNodeState::Running {
                                        activity: None,
                                        elapsed: None,
                                    },
                                );
                                step.linked = Some(document);
                                adopt.push((*node, document));
                            }
                        }
                        StepKind::Output => step.settle(
                            StepState::Finished { has_result: true },
                            CanvasNodeState::Done,
                        ),
                    }
                }
                run.steps.insert(*node, step);
            }
            run.chain = Some(chain);
            run.read = Some(read.clone());
            self.canvas_chains.runs.insert(head, run);
            for (node, document) in adopt {
                self.canvas_runs.adopt((canvas, node), document);
            }
            self.advance(head);
        }
    }
}

/// What an agent step starts with.
enum Prepared {
    Spec(Box<RunSpec>),
    /// This machine's agents are not known yet; asked for, and retried.
    Agents,
    Refused(CanvasNodeState),
}

// ------------------------------------------------------------ pure pieces

/// What a finished run's start node says.
fn ended(
    run: &ChainRun,
    done: bool,
    halts: &[(NodeId, HaltReason)],
    lang: Lang,
) -> CanvasNodeState {
    if run.stopping {
        return CanvasNodeState::Stopped;
    }
    match halts.first() {
        Some((_, HaltReason::Stopped)) => CanvasNodeState::Stopped,
        Some((at, HaltReason::Failed | HaltReason::NoResult)) => {
            let name = name_of(run.read.as_ref().map(|read| &read.snapshot), *at, lang);
            CanvasNodeState::Failed(fill(t(lang, Key::CanvasChainHaltedAt), &[&name]))
        }
        None if done => CanvasNodeState::Done,
        None => CanvasNodeState::Idle,
    }
}

/// The steps that will not run in this execution: each downstream of a halt
/// that has not started.
fn blocked(chain: &Chain, states: &HashMap<NodeId, StepState>) -> HashSet<NodeId> {
    let mut upstream: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for &(from, to) in &chain.edges {
        upstream.entry(to).or_default().push(from);
    }
    let mut halted = HashSet::new();
    let mut blocked = HashSet::new();
    for (step, _) in &chain.steps {
        let above_halted = upstream
            .get(step)
            .is_some_and(|above| above.iter().any(|id| halted.contains(id)));
        match states.get(step).copied().unwrap_or(StepState::NotStarted) {
            StepState::Failed | StepState::Stopped | StepState::Finished { has_result: false } => {
                halted.insert(*step);
            }
            StepState::NotStarted if above_halted => {
                halted.insert(*step);
                blocked.insert(*step);
            }
            _ => {}
        }
    }
    blocked
}

/// What a node is called in a sentence: its first line, or its kind.
fn name_of(snapshot: Option<&CanvasSnapshot>, node: NodeId, lang: Lang) -> String {
    let object = snapshot.and_then(|snapshot| snapshot.get(node));
    match object.map(|object| object.text.trim()) {
        Some(text) if !text.is_empty() => text.to_owned(),
        _ => t(
            lang,
            match object.map(|object| object.kind) {
                Some(ObjectKind::Start) => Key::CanvasStart,
                Some(ObjectKind::Output) => Key::CanvasOutput,
                _ => Key::CanvasAgent,
            },
        )
        .to_owned(),
    }
}

/// Why a start node has no chain to run, as a sentence.
fn chain_refusal(lang: Lang, snapshot: &CanvasSnapshot, error: ChainError) -> String {
    match error {
        ChainError::NotAStart => t(lang, Key::CanvasChainNotAStart).to_owned(),
        ChainError::Cycle(node) => fill(
            t(lang, Key::CanvasChainLoop),
            &[&name_of(Some(snapshot), node, lang)],
        ),
        ChainError::Empty => t(lang, Key::CanvasChainEmpty).to_owned(),
    }
}

/// An agent node's instruction: its text and its lines, as one objective.
fn objective_of(object: &canvas::CanvasObject) -> String {
    std::iter::once(object.text.as_str())
        .chain(object.lines.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// The agent an agent node runs: its own while that one is ready here, and
/// otherwise the one a new node would start with.
fn choose_agent(chosen: Option<&str>, runs: &CanvasRuns) -> Option<SessionProfileKey> {
    let ready = runs.ready_agents();
    chosen
        .and_then(parse_agent_key)
        .filter(|key| ready.iter().any(|(ready, _)| ready == key))
        .or_else(|| runs.default_agent())
}

/// **How an agent node names its agent** (`koru.agent`): a built-in
/// profile's own token (`codex`), or `custom:` and a custom profile's id.
/// The surface writes what this answers when a person chooses an agent.
pub(crate) fn agent_key_text(key: &SessionProfileKey) -> String {
    match key {
        SessionProfileKey::BuiltIn(key) => key.as_str().to_owned(),
        SessionProfileKey::Custom(id) => format!("custom:{id}"),
    }
}

/// [`agent_key_text`] read back — and a bare custom profile id, which is what
/// another writer is likeliest to leave. `None` for anything else.
pub(crate) fn parse_agent_key(text: &str) -> Option<SessionProfileKey> {
    let text = text.trim();
    if let Some(id) = text.strip_prefix("custom:") {
        return id.parse().ok().map(SessionProfileKey::Custom);
    }
    if let Ok(id) = text.parse::<koru_session_protocol::SessionProfileId>() {
        return Some(SessionProfileKey::Custom(id));
    }
    text.parse::<koru_session_protocol::BuiltInProfileKey>()
        .ok()
        .map(SessionProfileKey::BuiltIn)
}

/// **What an agent node is given**, in the reading order of their sources: a
/// card as a block of the canvas, an image as its blob, and an upstream
/// agent node as a block — its subtree carries its copied result.
fn sources_for(
    snapshot: &CanvasSnapshot,
    canvas: DocumentId,
    workspace: koru_core::WorkspaceId,
    node: NodeId,
) -> Vec<ContextSource> {
    let workspace = koru_delegate_core::WorkspaceId::from_uuid(workspace.as_uuid());
    let document = koru_delegate_core::DocumentId::from_uuid(canvas.as_uuid());
    inputs_of(snapshot, node)
        .into_iter()
        .filter_map(|input| {
            let object = snapshot.get(input)?;
            match object.kind {
                ObjectKind::Card | ObjectKind::Agent => Some(ContextSource::Block {
                    workspace,
                    document,
                    node: BlockNodeId::from_uuid(input.as_uuid()),
                }),
                ObjectKind::Image => Some(ContextSource::Blob {
                    workspace,
                    blob: koru_delegate_core::BlobHash::parse(&object.image.as_ref()?.hash.0)
                        .ok()?,
                    blob_kind: BlobKind::Image,
                }),
                ObjectKind::Start | ObjectKind::Output => None,
            }
        })
        .collect()
}

/// A Markdown result as the node's lines, without the blank lines around it.
fn markdown_lines(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let first = lines.iter().position(|line| !line.trim().is_empty());
    let last = lines.iter().rposition(|line| !line.trim().is_empty());
    match (first, last) {
        (Some(first), Some(last)) => lines[first..=last]
            .iter()
            .map(|line| (*line).to_owned())
            .collect(),
        _ => Vec::new(),
    }
}

/// How long a run has worked, in the run views' own words.
fn elapsed_words(lang: Lang, ms: u64) -> Option<String> {
    koru_runs_ui::summary(lang, &RunStatus::running(), None, Some(ms)).elapsed
}

// ------------------------------------------------------------ the store

/// [`ChainPort`] over the window's gateway client: a thread per request,
/// each answering once, queue first and wake second.
struct StorePort;

fn deliver<T>(send: &mpsc::Sender<T>, wake: Option<&Proxy>, value: T) {
    if send.send(value).is_ok()
        && let Some(proxy) = wake
    {
        let _ = proxy.send_event(koru_workspace_host::app::Wake::Store);
    }
}

impl ChainPort for StorePort {
    fn edit(
        &mut self,
        reach: Reach<'_>,
        canvas: DocumentId,
        root: Option<NodeId>,
        edit: CanvasEdit,
    ) -> Receiver<Result<CanvasRead, String>> {
        let (send, receive) = mpsc::channel();
        let (Some(client), Some(workspace)) = (reach.client.cloned(), reach.workspace) else {
            let _ = send.send(Err(UNAVAILABLE.to_owned()));
            return receive;
        };
        let wake = reach.wake.cloned();
        // A thread that cannot start drops `send`, and the pass reads the
        // closed channel as `worker_unavailable`.
        let _ = std::thread::Builder::new()
            .name("koru-canvas-chain-edit".into())
            .spawn(move || {
                let workspace = koru_core::WorkspaceId::from_uuid(workspace);
                let answer = edit_over_store(client, workspace, canvas, root, &edit)
                    .map_err(|error| error.code.as_str().to_owned());
                deliver(&send, wake.as_ref(), answer);
            });
        receive
    }

    fn page(&mut self, reach: Reach<'_>, write: PageWrite) -> Receiver<Result<DocumentId, String>> {
        let (send, receive) = mpsc::channel();
        let Some(client) = reach.client.cloned() else {
            let _ = send.send(Err(UNAVAILABLE.to_owned()));
            return receive;
        };
        let wake = reach.wake.cloned();
        let _ = std::thread::Builder::new()
            .name("koru-canvas-chain-page".into())
            .spawn(move || {
                let answer =
                    write_page(&client, &write).map_err(|error| error.code.as_str().to_owned());
                deliver(&send, wake.as_ref(), answer);
            });
        receive
    }
}

fn internal(error: koru_core::StoreError) -> Error {
    Error::new(Code::Internal, error.to_string())
}

fn canvas_root(doc: &WorkspaceDoc, root: Option<NodeId>) -> Result<NodeId, Error> {
    root.or_else(|| koru_workspace::document_root(doc))
        .ok_or_else(|| Error::new(Code::InvalidRequest, "the canvas has no root"))
}

/// **One edit, as another writer.** A read is the store's snapshot; anything
/// else joins the canvas's room with a replica of its own, writes, and
/// answers once the store has it on its disk.
fn edit_over_store(
    client: koru_api_client::Client,
    workspace: koru_core::WorkspaceId,
    canvas: DocumentId,
    root: Option<NodeId>,
    edit: &CanvasEdit,
) -> Result<CanvasRead, Error> {
    use koru_workspace::explore::{store_document, store_workspace};
    if *edit == CanvasEdit::Read {
        let bytes = client.snapshot(store_workspace(workspace), store_document(canvas))?;
        let doc = WorkspaceDoc::from_snapshot(&bytes).map_err(internal)?;
        let root = canvas_root(&doc, root)?;
        return read_canvas(&doc, root).map_err(internal);
    }
    let factory: koru_workspace::store::ClientFactory = Arc::new(move || Ok(client.clone()));
    koru_workspace::edit_foreign_document(factory, workspace, canvas, |doc| {
        let root = canvas_root(doc, root)?;
        edit.apply(doc, root).map_err(internal)?;
        read_canvas(doc, root).map_err(internal)
    })
}

/// **Write an output page** (P5.3). The first write of an execution is the
/// Store's seeded create under the node's and the execution's key, so a
/// retry answers with the page it made. A page an earlier run wrote is
/// updated in place instead; one that has gone is made again.
fn write_page(client: &koru_api_client::Client, write: &PageWrite) -> Result<DocumentId, Error> {
    use koru_workspace::explore::{koru_document, store_workspace};
    if let Some(page) = write.existing {
        match replace_page(client, write, page) {
            Ok(()) => return Ok(page),
            Err(error) if error.code == Code::DocumentNotFound => {}
            Err(error) => return Err(error),
        }
    }
    let seeded = client.seed_document(
        store_workspace(write.workspace),
        &write.title,
        koru_core::explore::KIND_DOCUMENT,
        None,
        None,
        &write.idempotency_key(),
        &write.request_hash(),
        page_snapshot(write)?,
    )?;
    Ok(koru_document(seeded.id))
}

/// **Replace what is under a page's title, in one publish, as another
/// writer**, between a History marker before and one after (reason
/// `manual`, labelled with the node and the run), so the earlier version is
/// one click away (Q19). Nested pages and files under the title stay.
fn replace_page(
    client: &koru_api_client::Client,
    write: &PageWrite,
    page: DocumentId,
) -> Result<(), Error> {
    replace_page_text(
        client,
        write.workspace,
        page,
        &write.title,
        &write.markdown,
        (&write.before, &write.after),
    )
}

/// [`replace_page`] for any caller that holds the words rather than a
/// chain's write: `markdown` goes under `page`'s title in `workspace`,
/// between the two History `labels` (before, after). `title` is only what
/// the Markdown builder is given; the page's own title row is not touched.
/// A Bot's quick setup writes its Role page through this
/// (`mainview/bot_onboard.rs`).
pub(super) fn replace_page_text(
    client: &koru_api_client::Client,
    workspace: koru_core::WorkspaceId,
    page: DocumentId,
    title: &str,
    markdown: &str,
    labels: (&str, &str),
) -> Result<(), Error> {
    use koru_workspace::explore::{store_document, store_workspace};
    let fresh = page_content(workspace, title, markdown)?;
    let write_workspace = workspace;
    let marker = |label: &str| koru_store_wire::CaptureHistory {
        reason: "manual".into(),
        frontiers: None,
        label: Some(label.to_owned()),
        message: None,
        boundary: None,
    };
    let (workspace, document) = (store_workspace(write_workspace), store_document(page));
    client.capture_history(workspace, document, &marker(labels.0))?;
    let factory: koru_workspace::store::ClientFactory = {
        let client = client.clone();
        Arc::new(move || Ok(client.clone()))
    };
    koru_workspace::edit_foreign_document(factory, write_workspace, page, |doc| {
        let root = koru_workspace::document_root(doc)
            .ok_or_else(|| Error::new(Code::InvalidRequest, "the page has no title row"))?;
        for child in doc.children(Parent::Node(root)).map_err(internal)? {
            if matches!(
                doc.kind_of(child),
                Some(koru_core::Kind::Document | koru_core::Kind::File | koru_core::Kind::Trash)
            ) {
                continue;
            }
            doc.delete(child).map_err(internal)?;
        }
        for cutting in &fresh {
            doc.plant(cutting, Parent::Node(root), "")
                .map_err(internal)?;
        }
        Ok(())
    })?;
    client.capture_history(workspace, document, &marker(labels.1))?;
    Ok(())
}

/// The page, built from Markdown the way Koru AI's export builds one.
fn page_snapshot(write: &PageWrite) -> Result<Vec<u8>, Error> {
    snapshot_of(write.workspace, &write.title, &write.markdown)
}

fn snapshot_of(
    workspace: koru_core::WorkspaceId,
    title: &str,
    markdown: &str,
) -> Result<Vec<u8>, Error> {
    koru_import_markdown::document_snapshot(workspace, title, markdown)
        .map_err(|_| Error::new(Code::InvalidRequest, "the page could not be built"))
}

/// What goes under a page's title, as cuttings to plant.
fn page_content(
    workspace: koru_core::WorkspaceId,
    title: &str,
    markdown: &str,
) -> Result<Vec<koru_core::schema::Cutting>, Error> {
    let built =
        WorkspaceDoc::from_snapshot(&snapshot_of(workspace, title, markdown)?).map_err(internal)?;
    let Some(root) = koru_workspace::document_root(&built) else {
        return Ok(Vec::new());
    };
    built
        .children(Parent::Node(root))
        .map_err(internal)?
        .into_iter()
        .map(|child| built.cutting(child).map_err(internal))
        .collect()
}

#[cfg(test)]
mod tests;
