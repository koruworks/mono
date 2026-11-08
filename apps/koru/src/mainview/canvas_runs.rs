// SPDX-License-Identifier: MPL-2.0

//! **The runs a canvas's agent nodes start** — the run machinery a canvas
//! calls, with no canvas in it.
//!
//! A person presses Run on a canvas's start node, and each agent node
//! downstream starts a Koru delegated run whose context is the cards
//! connected into it, shows that run's live status, can stop it, and keeps
//! what it produced.
//! This module is that machinery, keyed by the node — `(canvas document,
//! node)` — rather than by a modal:
//!
//! - [`CanvasRuns::start`] creates a run through the one path every run is
//!   created by (`delegate::job::create_run`), each on a worker of its own, so
//!   **many are in flight at once** — and none of them is the delegation
//!   modal's single job, which a canvas neither uses nor blocks.
//! - **Status is the run pills'**, not a second poller: a canvas's runs join
//!   the set `MainView::maintain_run_pills` reads every three seconds, and the
//!   same read keeps the newest step and the elapsed time. A `delegate.*`
//!   event about one of them, heard on the event bus for the canvas's
//!   workspace, brings the next read forward; the clock is the fallback.
//! - [`CanvasRuns::stop`] cancels on a worker. The node says *Stopping…*
//!   until the run says it has: a cancel that lands mid-turn leaves the run
//!   running until its provider confirms.
//! - [`CanvasRuns::read_result`] reads one result version on a worker, by the
//!   Result tab's own decision (`koru_artifacts_ui::read_result`).
//! - [`CanvasRuns::ready_agents`] and [`CanvasRuns::default_agent`] come from
//!   the readiness snapshot the modal's agent list is built from.
//!
//! **Nothing here waits on the world.** A call records what was asked and
//! returns; `MainView::maintain_canvas_runs` hands the work to workers
//! ([`CanvasRuns::launch`]) and folds their answers
//! ([`CanvasRuns::maintain`]) once per pass, the way `maintain_delegation`
//! drains the modal's job. Every word a node shows is `koru-i18n`'s, through
//! `koru_runs_ui::summary`, so a node and a pill on the same run agree.
//!
//! **The canvas surface is the caller, and it lands on its own branch.** Until
//! it calls [`CanvasRuns::start`] and the rest, only this module's tests do,
//! so the build would call most of it dead; the allow below says so once
//! rather than item by item, and goes when the surface is wired.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use koru_artifacts_ui::{ArtifactVersionId, ResultBody};
use koru_core::{DocumentId, NodeId};
use koru_delegate_core::{ResultReference, RunId, RunStatus, SessionProfileKey};
use koru_i18n::{Key, Lang, fill, t};
use koru_session_protocol::{ExecutableAvailability, ProviderReadiness};

use super::MainView;
use super::delegate::job::{RunAnswer, RunJob, RunSpec, start_delegation};
use super::delegate::modal::readiness;
use super::delegate::pills::RunPillState;
use super::delegate::world::{AgentRow, agents_for};

type Proxy = winit::event_loop::EventLoopProxy<koru_workspace_host::app::Wake>;

/// Which agent node a run belongs to: the canvas document and the node on it.
pub(crate) type NodeKey = (DocumentId, NodeId);

/// The code a worker that went away without answering is refused with.
const NO_WORKER: &str = "worker_unavailable";
/// The code a window with no store client is refused with — the gateway's
/// own token for a device with no delegate transport.
const UNAVAILABLE: &str = "delegate_unavailable";

/// Where an agent node's run is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum NodeRunState {
    /// Pressed and not yet running: waiting for this machine's agents to be
    /// known, the context being pinned, the run being created, or created
    /// and not yet started.
    Starting,
    /// Working — including a run that needs a person, is interrupted, or
    /// waits for review; `RunUpdate::status_word` says which.
    Running,
    /// Stop was pressed and the run has not yet said it stopped.
    Stopping,
    /// Completed. `has_result` is whether it registered anything.
    Finished { has_result: bool },
    /// It failed, or could not be created. The words say why, localized.
    Failed(String),
    /// It was stopped — or stopped before it existed.
    Stopped,
    /// It was not started, because the agent cannot run here — no agent is
    /// set up, or the chosen one is signed out. The words say what to do.
    NotReady(String),
}

/// What changed about one agent node's run. [`CanvasRuns::maintain`]
/// answers with one per node whose state moved; [`CanvasRuns::current`]
/// holds the latest for every node.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct RunUpdate {
    pub(crate) key: NodeKey,
    /// The provider is waiting for an approval or a reply. Kept as data,
    /// rather than inferred from the localized status sentence.
    pub(crate) needs_response: bool,
    /// The run this press minted. `None` once it is known no run exists:
    /// not ready, refused, or stopped before it was created.
    pub(crate) run: Option<RunId>,
    /// The Run document, once the run exists — what a run pill names.
    pub(crate) run_document: Option<DocumentId>,
    pub(crate) state: NodeRunState,
    /// **The run pill's own word** for where the run is — *Starting…*,
    /// *Working…*, *Needs you*, *Interrupted*, *Stopping…*, *Finished* —
    /// so a node and a pill on the same run say the same thing. `None` for
    /// `NotReady`, whose words are the reason.
    pub(crate) status_word: Option<String>,
    /// What the run is doing: its newest step as a sentence, localized.
    /// `None` before its first step and once it has settled.
    pub(crate) activity: Option<String>,
    /// How long it has worked (§17.2), as of the last read.
    pub(crate) elapsed_ms: Option<u64>,
    /// Everything it registered, in order. A title's `Debug` is redacted.
    pub(crate) results: Vec<ResultReference>,
}

/// One result version read for a node: Markdown text, a picture's blob and
/// media type, anything else by its type — or why it could not be read,
/// localized.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ResultRead {
    pub(crate) key: NodeKey,
    pub(crate) version: ArtifactVersionId,
    pub(crate) body: Result<ResultBody, String>,
}

/// What the workers are handed: the window's gateway client and its wake,
/// and the workspace the canvas is in. Borrowed — a client is cloned only
/// when a worker is actually started.
#[derive(Copy, Clone, Default)]
pub(crate) struct Reach<'a> {
    pub(crate) client: Option<&'a koru_api_client::Client>,
    pub(crate) wake: Option<&'a Proxy>,
    pub(crate) workspace: Option<uuid::Uuid>,
}

/// The Run documents a `delegate.*` event named since the last pass.
type Touched = Arc<Mutex<HashSet<uuid::Uuid>>>;

/// An event-bus subscription, alive for as long as this is held.
pub(crate) struct Watch(#[allow(dead_code)] Box<dyn Send>);

/// **Everything a canvas's runs do to the world**, each on a worker. The
/// gateway answers them in the app ([`GatewayPort`]); a test answers them
/// from memory, which is how "many at once" and "one cancel" are asserted
/// without a store.
pub(crate) trait RunPort: Send {
    /// Pin the spec's sources and create the run.
    fn create(&mut self, reach: Reach<'_>, spec: RunSpec) -> Result<RunJob, RunAnswer>;
    /// `POST /v1/runs/{run}/cancel`, answering the refusal's code.
    fn cancel(&mut self, reach: Reach<'_>, run: RunId) -> Receiver<Result<(), String>>;
    /// One result version, answering the store's code when it cannot.
    fn read(
        &mut self,
        reach: Reach<'_>,
        version: ArtifactVersionId,
    ) -> Receiver<Result<ResultBody, String>>;
    /// Listen for `delegate.*` events in `workspace`, noting each Run
    /// document they name. `None` when this device has no event bus.
    fn watch(&mut self, reach: Reach<'_>, workspace: uuid::Uuid, touched: Touched)
    -> Option<Watch>;
}

/// Why a pressed run has not been handed to a worker yet.
enum Admission {
    /// The agents are still being checked.
    Wait,
    Go,
    NotReady(String),
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Stop {
    /// Nobody asked.
    None,
    /// Asked; sent once the run exists.
    Asked,
    /// The cancel is out or answered; the run has not said it stopped.
    Sent,
}

enum Phase {
    /// Pressed; waiting for readiness, or for the next launch.
    Queued(Box<RunSpec>),
    /// On a worker: pinning and creating.
    Creating,
    /// The run exists. `key` is the Run document's storage id — the run
    /// pills' key — spelled once rather than every pass.
    Created {
        document: DocumentId,
        key: String,
    },
    /// Not created, and why.
    Refused(String),
    NotReady(String),
    /// Stopped before it was created.
    Withdrawn,
}

struct NodeRun {
    run: RunId,
    phase: Phase,
    stop: Stop,
    /// The run as the pills' worker last read it.
    observed: Option<RunPillState>,
    /// Started before this window opened, and followed since
    /// ([`CanvasRuns::adopt`]): `run` is a stand-in until the first read.
    adopted: bool,
}

/// **Every agent node's run, for every canvas in this window.**
pub(crate) struct CanvasRuns {
    port: Box<dyn RunPort>,
    nodes: HashMap<NodeKey, NodeRun>,
    /// Creations in flight, by node. A node pressed again keeps its old
    /// creation here until it answers: the run is durable either way.
    creating: Vec<(NodeKey, RunJob)>,
    cancelling: Vec<(RunId, Receiver<Result<(), String>>)>,
    asked_reads: Vec<(NodeKey, ArtifactVersionId)>,
    reading: Vec<(
        NodeKey,
        ArtifactVersionId,
        Receiver<Result<ResultBody, String>>,
    )>,
    read: Vec<ResultRead>,
    /// Every run id a press here has used, so a spec pressed twice is two
    /// runs rather than one id re-pinned.
    minted: HashSet<RunId>,
    /// This machine's agents, from the host's last readiness snapshot.
    agents: Vec<AgentRow>,
    readiness: Option<Vec<ProviderReadiness>>,
    wants_readiness: bool,
    /// The agent the delegation modal last started a run with.
    modal_agent: Option<SessionProfileKey>,
    /// The coding agents turned off on this machine (Agent providers), by
    /// agent key — which is also each row's managed provider kind. Left out
    /// of the agents a canvas offers; a node already set to one still runs.
    hidden_agents: Vec<String>,
    /// The order the agents are listed in (Agent providers), by agent key.
    agent_order: Vec<String>,
    touched: Touched,
    watch: Option<(uuid::Uuid, Option<Watch>)>,
    early: bool,
    created: bool,
    notices: Vec<String>,
    dirty: HashSet<NodeKey>,
    reported: HashMap<NodeKey, RunUpdate>,
}

impl Default for CanvasRuns {
    fn default() -> Self {
        Self::with_port(Box::new(GatewayPort))
    }
}

impl CanvasRuns {
    pub(crate) fn with_port(port: Box<dyn RunPort>) -> Self {
        Self {
            port,
            nodes: HashMap::new(),
            creating: Vec::new(),
            cancelling: Vec::new(),
            asked_reads: Vec::new(),
            reading: Vec::new(),
            read: Vec::new(),
            minted: HashSet::new(),
            agents: Vec::new(),
            readiness: None,
            wants_readiness: false,
            modal_agent: None,
            hidden_agents: Vec::new(),
            agent_order: Vec::new(),
            touched: Arc::default(),
            watch: None,
            early: false,
            created: false,
            notices: Vec::new(),
            dirty: HashSet::new(),
            reported: HashMap::new(),
        }
    }

    // ------------------------------------------------------------ what a canvas asks

    /// **Start `key`'s run.** Returns the run id the press uses.
    ///
    /// Many can be running at once; this never waits and never touches the
    /// modal's job. The run is created on the next pass, once this machine's
    /// agents are known: an agent that cannot run here leaves the node
    /// `NotReady` with the reason rather than a refusal from the host.
    ///
    /// A spec whose id was already pressed gets a new one: retrying a run id
    /// after re-pinning could capture different versions under one run. A
    /// node pressed again while its run is live starts a second run; the
    /// first carries on, reachable from the Agents navigator.
    pub(crate) fn start(&mut self, key: NodeKey, mut spec: RunSpec) -> RunId {
        while !self.minted.insert(spec.run) {
            spec.run = RunId::mint();
        }
        let run = spec.run;
        self.wants_readiness = true;
        self.nodes.insert(
            key,
            NodeRun {
                run,
                phase: Phase::Queued(Box::new(spec)),
                stop: Stop::None,
                observed: None,
                adopted: false,
            },
        );
        self.dirty.insert(key);
        run
    }

    /// **Stop a node's run.** The cancel goes out on a worker; the node says
    /// `Stopping` until the run's status says it stopped. A run not yet
    /// created is withdrawn before it is; asking twice sends one cancel.
    pub(crate) fn stop(&mut self, run: RunId) {
        let Some((key, node)) = self.nodes.iter_mut().find(|(_, node)| node.run == run) else {
            return;
        };
        let settled = node
            .observed
            .as_ref()
            .is_some_and(|observed| observed.status.is_terminal());
        match node.phase {
            Phase::Queued(_) => node.phase = Phase::Withdrawn,
            Phase::Creating | Phase::Created { .. } if node.stop == Stop::None && !settled => {
                node.stop = Stop::Asked;
            }
            _ => return,
        }
        self.dirty.insert(*key);
    }

    /// Read one of a node's results, on a worker. The answer arrives in
    /// [`Self::take_results`].
    pub(crate) fn read_result(&mut self, key: NodeKey, version: ArtifactVersionId) {
        self.asked_reads.push((key, version));
    }

    /// The results read since the last call.
    pub(crate) fn take_results(&mut self) -> Vec<ResultRead> {
        std::mem::take(&mut self.read)
    }

    /// The results read since the last call that `wanted` claims; the rest
    /// stay for [`Self::take_results`]. A chain takes the results it asked
    /// for this way, so a surface reading a node's result for itself is
    /// never robbed of its answer.
    pub(crate) fn take_results_where(
        &mut self,
        mut wanted: impl FnMut(&ResultRead) -> bool,
    ) -> Vec<ResultRead> {
        let (taken, kept) = std::mem::take(&mut self.read)
            .into_iter()
            .partition(|read| wanted(read));
        self.read = kept;
        taken
    }

    /// Whether this machine's agents are known — the host has answered at
    /// least once. Until then [`Self::ready_agents`] is empty for want of an
    /// answer, not for want of an agent.
    pub(crate) fn agents_known(&self) -> bool {
        self.readiness.is_some()
    }

    /// **Follow a run a node started before this window opened** — the
    /// canvas remembers the Run document; this table does not. The run is
    /// read by the pills' worker like any other, and its id is taken from
    /// the first read, so a stop asked for before that waits for it.
    pub(crate) fn adopt(&mut self, key: NodeKey, document: DocumentId) {
        if self.nodes.get(&key).is_some_and(|node| {
            matches!(&node.phase, Phase::Created { document: known, .. } if *known == document)
        }) {
            return;
        }
        self.nodes.insert(
            key,
            NodeRun {
                // Stands in until the first read names the real one; never
                // sent anywhere before then (`launch` waits for a read).
                run: RunId::mint(),
                phase: Phase::Created {
                    document,
                    key: document.to_storage(),
                },
                stop: Stop::None,
                observed: None,
                adopted: true,
            },
        );
        self.dirty.insert(key);
    }

    /// The agents that can run delegated work on this machine now, with the
    /// names the modal shows — for a canvas's agent menu — less any turned
    /// off on *Agent providers*, in the order set there. Empty until the
    /// host has answered; [`Self::want_agents`] asks it.
    pub(crate) fn ready_agents(&self) -> Vec<(SessionProfileKey, String)> {
        let mut rows: Vec<&AgentRow> = self
            .agents
            .iter()
            .filter(|row| row.choosable())
            .filter(|row| {
                !self
                    .hidden_agents
                    .iter()
                    .any(|off| off == row.provider.as_str())
            })
            .collect();
        rows.sort_by_key(|row| {
            super::agent_providers::order_rank(&self.agent_order, Some(row.provider.as_str()))
        });
        rows.into_iter()
            .map(|row| (row.key.clone(), row.name.clone()))
            .collect()
    }

    /// **The agents turned off, and their order** (Settings → Coding →
    /// Agent providers): a canvas's agent menu and a new node's default
    /// leave the first out and follow the second. Answers whether anything
    /// moved.
    pub(crate) fn set_agent_settings(&mut self, hidden: Vec<String>, order: Vec<String>) -> bool {
        let moved = self.hidden_agents != hidden || self.agent_order != order;
        self.hidden_agents = hidden;
        self.agent_order = order;
        moved
    }

    /// The agent a new node should start with: the one the delegation modal
    /// last used, while it is still ready here, and otherwise the first ready.
    pub(crate) fn default_agent(&self) -> Option<SessionProfileKey> {
        let ready = self.ready_agents();
        self.modal_agent
            .as_ref()
            .filter(|agent| ready.iter().any(|(key, _)| key == *agent))
            .cloned()
            .or_else(|| ready.into_iter().next().map(|(key, _)| key))
    }

    /// Ask the host which agents are ready, when a canvas is about to offer
    /// them. Starting a run asks too.
    pub(crate) fn want_agents(&mut self) {
        self.wants_readiness = true;
    }

    /// The latest update for a node, whatever pass it came in.
    pub(crate) fn current(&self, key: &NodeKey) -> Option<&RunUpdate> {
        self.reported.get(key)
    }

    // ------------------------------------------------------------ what the window feeds

    /// The agent the modal just started a run with.
    pub(crate) fn remember_modal_agent(&mut self, agent: SessionProfileKey) {
        self.modal_agent = Some(agent);
    }

    /// Take the host's readiness snapshot, when it changed. Answers whether
    /// the agent list moved.
    pub(crate) fn set_readiness(
        &mut self,
        snapshot: Option<&[ProviderReadiness]>,
        lang: Lang,
    ) -> bool {
        if self.readiness.as_deref() == snapshot {
            return false;
        }
        self.readiness = snapshot.map(<[ProviderReadiness]>::to_vec);
        self.agents = agents_for(
            &koru_session_profiles::built_in_profiles(),
            koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS,
            snapshot,
            t(lang, Key::TerminalPlatformShell),
            t(lang, Key::TerminalCustomProfile),
        );
        // A run waiting for readiness is decided on the next launch.
        true
    }

    /// Whether readiness was asked for since the last call.
    pub(crate) fn take_wants_readiness(&mut self) -> bool {
        std::mem::take(&mut self.wants_readiness)
    }

    /// The Run documents the run pills' worker should read for the canvas,
    /// keyed the way it keys its cache.
    pub(crate) fn run_documents(
        &self,
    ) -> impl Iterator<Item = (String, koru_store_core::ids::DocumentId)> + '_ {
        self.nodes.values().filter_map(|node| match &node.phase {
            Phase::Created { document, key } => Some((
                key.clone(),
                koru_store_core::ids::DocumentId::from_uuid(document.as_uuid()),
            )),
            _ => None,
        })
    }

    /// Fold what the run pills' worker last read into the nodes it names.
    pub(crate) fn observe(&mut self, states: &HashMap<String, RunPillState>) {
        for (node_key, node) in &mut self.nodes {
            let Phase::Created { key, .. } = &node.phase else {
                continue;
            };
            let Some(state) = states.get(key) else {
                continue;
            };
            if node.adopted {
                // The run's own id, from its own header.
                node.run = state.run;
            }
            if node.observed.as_ref() != Some(state) {
                node.observed = Some(state.clone());
                self.dirty.insert(*node_key);
            }
        }
    }

    /// Whether nothing has been asked and nothing is in flight — the pass
    /// then does no work at all.
    pub(crate) fn idle(&self) -> bool {
        self.nodes.is_empty()
            && self.creating.is_empty()
            && self.cancelling.is_empty()
            && self.asked_reads.is_empty()
            && self.reading.is_empty()
            && self.watch.is_none()
    }

    /// **Hand what was asked to workers.** Runs whose agent is ready are
    /// created, stops for runs that exist are sent, reads are started, and
    /// the event bus is listened to while a run is live.
    pub(crate) fn launch(&mut self, reach: Reach<'_>) {
        let lang = crate::format::lang();
        let queued: Vec<NodeKey> = self
            .nodes
            .iter()
            .filter(|(_, node)| matches!(node.phase, Phase::Queued(_)))
            .map(|(key, _)| *key)
            .collect();
        for key in queued {
            let admission = match &self.nodes[&key].phase {
                Phase::Queued(spec) => self.admit(&spec.agent, lang),
                _ => continue,
            };
            let node = self.nodes.get_mut(&key).expect("queued node");
            match admission {
                Admission::Wait => continue,
                Admission::NotReady(reason) => node.phase = Phase::NotReady(reason),
                Admission::Go => {
                    let Phase::Queued(spec) = std::mem::replace(&mut node.phase, Phase::Creating)
                    else {
                        continue;
                    };
                    let run = spec.run;
                    match self.port.create(reach, *spec) {
                        Ok(job) => self.creating.push((key, job)),
                        Err(answer) => self.answered(key, run, answer),
                    }
                }
            }
            self.dirty.insert(key);
        }

        // A run that settled while its creation answered needs no cancel.
        let stops: Vec<RunId> = self
            .nodes
            .values_mut()
            .filter(|node| {
                node.stop == Stop::Asked
                    && matches!(node.phase, Phase::Created { .. })
                    && (!node.adopted || node.observed.is_some())
                    && !node
                        .observed
                        .as_ref()
                        .is_some_and(|observed| observed.status.is_terminal())
            })
            .map(|node| {
                node.stop = Stop::Sent;
                node.run
            })
            .collect();
        for run in stops {
            let answer = self.port.cancel(reach, run);
            self.cancelling.push((run, answer));
        }

        for (key, version) in std::mem::take(&mut self.asked_reads) {
            let answer = self.port.read(reach, version);
            self.reading.push((key, version, answer));
        }

        // **Listen only while a run could still move**, and only in the
        // workspace the canvas is in; an idle window holds no subscription.
        let live = !self.creating.is_empty()
            || self.nodes.values().any(|node| match node.phase {
                Phase::Created { .. } => !node
                    .observed
                    .as_ref()
                    .is_some_and(|observed| observed.status.is_terminal()),
                Phase::Creating => true,
                _ => false,
            });
        match (live, reach.workspace) {
            (true, Some(workspace)) => {
                if self.watch.as_ref().map(|(watched, _)| *watched) != Some(workspace) {
                    let watch = self.port.watch(reach, workspace, self.touched.clone());
                    // A device with no event bus is asked once; a window
                    // with no client yet is asked again once it has one.
                    if watch.is_some() || reach.client.is_some() {
                        self.watch = Some((workspace, watch));
                    }
                }
            }
            (true, None) => {}
            (false, _) => self.watch = None,
        }
    }

    /// **Fold every answer in, and say what changed.** Never blocks.
    pub(crate) fn maintain(&mut self) -> Vec<RunUpdate> {
        let lang = crate::format::lang();

        let mut index = 0;
        while index < self.creating.len() {
            match self.creating[index].1.try_take() {
                Some(answer) => {
                    let (key, job) = self.creating.swap_remove(index);
                    self.answered(key, job.run, answer);
                }
                None => index += 1,
            }
        }

        let mut index = 0;
        while index < self.cancelling.len() {
            let answer = match self.cancelling[index].1.try_recv() {
                Ok(answer) => answer,
                Err(TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => Err(NO_WORKER.to_owned()),
            };
            let (run, _) = self.cancelling.swap_remove(index);
            match answer {
                // Accepted: the run says it stopped when its provider does.
                // Read it again now rather than in three seconds.
                Ok(()) => self.early = true,
                Err(code) => {
                    if let Some((key, node)) =
                        self.nodes.iter_mut().find(|(_, node)| node.run == run)
                        && node.stop == Stop::Sent
                    {
                        node.stop = Stop::None;
                        self.dirty.insert(*key);
                    }
                    self.notices
                        .push(fill(t(lang, Key::DelegateRunStopRefused), &[&code]));
                }
            }
        }

        let mut index = 0;
        while index < self.reading.len() {
            let body = match self.reading[index].2.try_recv() {
                Ok(body) => body,
                Err(TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => Err(NO_WORKER.to_owned()),
            };
            let (key, version, _) = self.reading.swap_remove(index);
            self.read.push(ResultRead {
                key,
                version,
                body: body.map_err(|code| read_failure(lang, &code)),
            });
        }

        let touched: HashSet<uuid::Uuid> = self
            .touched
            .lock()
            .map(|mut touched| std::mem::take(&mut *touched))
            .unwrap_or_default();
        if !touched.is_empty()
            && self.nodes.values().any(|node| {
                matches!(&node.phase, Phase::Created { document, .. }
                    if touched.contains(&document.as_uuid()))
            })
        {
            self.early = true;
        }

        let mut updates = Vec::new();
        for key in std::mem::take(&mut self.dirty) {
            let Some(update) = self.update_for(key, lang) else {
                continue;
            };
            if self.reported.get(&key) != Some(&update) {
                self.reported.insert(key, update.clone());
                updates.push(update);
            }
        }
        updates
    }

    /// Whether a run on a canvas moved enough to read it again at once.
    pub(crate) fn take_early_read(&mut self) -> bool {
        std::mem::take(&mut self.early)
    }

    /// Whether a run was created since the last call — the Agents navigator
    /// then has one it does not know about.
    pub(crate) fn take_created(&mut self) -> bool {
        std::mem::take(&mut self.created)
    }

    /// Sentences for the window to show once: a stop that was refused.
    pub(crate) fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    // ------------------------------------------------------------ inside

    fn admit(&self, agent: &SessionProfileKey, lang: Lang) -> Admission {
        if self.readiness.is_none() {
            return Admission::Wait;
        }
        match self.agents.iter().find(|row| row.key == *agent) {
            Some(row) if row.choosable() => Admission::Go,
            Some(row) if matches!(row.readiness.availability, ExecutableAvailability::Checking) => {
                Admission::Wait
            }
            Some(row) => Admission::NotReady(
                t(
                    lang,
                    readiness::guidance(&row.readiness).unwrap_or(Key::DelegateNoAgents),
                )
                .to_owned(),
            ),
            None if self.agents.iter().any(AgentRow::choosable) => {
                Admission::NotReady(t(lang, Key::TerminalManagedChooseAnother).to_owned())
            }
            None => Admission::NotReady(t(lang, Key::DelegateNoAgents).to_owned()),
        }
    }

    /// A creation answered. A node pressed again since keeps its new run;
    /// the old run exists all the same and is counted as created.
    fn answered(&mut self, key: NodeKey, run: RunId, answer: RunAnswer) {
        if matches!(answer, RunAnswer::Done { .. }) {
            self.created = true;
        }
        let Some(node) = self.nodes.get_mut(&key).filter(|node| node.run == run) else {
            return;
        };
        node.phase = match answer {
            RunAnswer::Done { document, .. } => Phase::Created {
                document,
                key: document.to_storage(),
            },
            // Stop was pressed while it was being created, and it never was.
            RunAnswer::Refused { .. } if node.stop != Stop::None => Phase::Withdrawn,
            RunAnswer::Refused { code } => Phase::Refused(refusal(crate::format::lang(), &code)),
        };
        self.dirty.insert(key);
    }

    fn update_for(&self, key: NodeKey, lang: Lang) -> Option<RunUpdate> {
        let node = self.nodes.get(&key)?;
        let observed = node.observed.as_ref();
        let settled = observed.is_some_and(|observed| observed.status.is_terminal());
        let stopping = node.stop != Stop::None && !settled;
        let (state, exists) = match &node.phase {
            Phase::Queued(_) => (NodeRunState::Starting, true),
            Phase::Creating if stopping => (NodeRunState::Stopping, true),
            Phase::Creating => (NodeRunState::Starting, true),
            Phase::Refused(reason) => (NodeRunState::Failed(reason.clone()), false),
            Phase::NotReady(reason) => (NodeRunState::NotReady(reason.clone()), false),
            Phase::Withdrawn => (NodeRunState::Stopped, false),
            Phase::Created { .. } if stopping => (NodeRunState::Stopping, true),
            Phase::Created { .. } => (
                match observed {
                    None => NodeRunState::Starting,
                    Some(observed) => run_state(lang, observed),
                },
                true,
            ),
        };
        let words = observed.map(|observed| {
            koru_runs_ui::summary(
                lang,
                &observed.status,
                observed.latest_step.filter(|_| !settled),
                None,
            )
        });
        let status_word = match (&state, &words) {
            (NodeRunState::Stopping, _) => Some(t(lang, Key::DelegateRunStopping).to_owned()),
            (NodeRunState::NotReady(_), _) => None,
            (_, Some(words)) => Some(words.status.clone()),
            (NodeRunState::Starting, None) => Some(t(lang, Key::DelegateRunStarting).to_owned()),
            (NodeRunState::Stopped, None) => Some(t(lang, Key::DelegateRunStopped).to_owned()),
            (_, None) => Some(t(lang, Key::DelegateRunFailed).to_owned()),
        };
        // An adopted run's id is a stand-in until its first read.
        let named = !node.adopted || observed.is_some();
        Some(RunUpdate {
            key,
            needs_response: matches!(state, NodeRunState::Running)
                && observed.is_some_and(|observed| matches!(
                    &observed.status,
                    RunStatus::Running { attention, .. }
                        if attention.contains(koru_delegate_core::Attention::ApprovalRequired)
                            || attention.contains(koru_delegate_core::Attention::WaitingForInput)
                )),
            run: (exists && named).then_some(node.run),
            run_document: match &node.phase {
                Phase::Created { document, .. } => Some(*document),
                _ => None,
            },
            state,
            status_word,
            activity: words.and_then(|words| words.activity),
            elapsed_ms: observed.and_then(|observed| observed.elapsed_ms),
            results: observed
                .map(|observed| observed.results.clone())
                .unwrap_or_default(),
        })
    }
}

/// Where a run is, from what its document says.
fn run_state(lang: Lang, observed: &RunPillState) -> NodeRunState {
    match &observed.status {
        RunStatus::Created | RunStatus::Starting => NodeRunState::Starting,
        RunStatus::Completed => NodeRunState::Finished {
            has_result: !observed.results.is_empty(),
        },
        RunStatus::Cancelled => NodeRunState::Stopped,
        status @ RunStatus::Failed { .. } => NodeRunState::Failed(
            koru_runs_ui::failed_because(lang, status, observed.failure_reason)
                .unwrap_or_else(|| t(lang, Key::DelegateRunFailed).to_owned()),
        ),
        RunStatus::Running { .. }
        | RunStatus::Interrupted { .. }
        | RunStatus::AwaitingReview
        | RunStatus::WaitingForParticipant => NodeRunState::Running,
    }
}

/// A create the host or this window refused, as a sentence.
fn refusal(lang: Lang, code: &str) -> String {
    match code {
        UNAVAILABLE => t(lang, Key::DelegateUnavailable).to_owned(),
        _ => fill(t(lang, Key::DelegateRefused), &[code]),
    }
}

/// A result read that failed, as a sentence. Bytes held on another device
/// say so (D27); everything else is the Artifacts surface's own sentence.
fn read_failure(lang: Lang, code: &str) -> String {
    match code {
        "blob_not_found" | "blob_unavailable" | "blob_waiting_for_peer" => {
            t(lang, Key::ArtifactsHeldElsewhereUnknown).to_owned()
        }
        "unreadable" => t(lang, Key::MarkdownUnreadable).to_owned(),
        _ => koru_artifacts_ui::text::refusal(lang, code).to_owned(),
    }
}

// ------------------------------------------------------------ the gateway

/// [`RunPort`] over the window's gateway client: a thread per request, each
/// answering once, queue first and wake second.
struct GatewayPort;

fn deliver<T>(send: &mpsc::Sender<T>, wake: Option<&Proxy>, value: T) {
    if send.send(value).is_ok()
        && let Some(proxy) = wake
    {
        let _ = proxy.send_event(koru_workspace_host::app::Wake::Store);
    }
}

impl RunPort for GatewayPort {
    fn create(&mut self, reach: Reach<'_>, spec: RunSpec) -> Result<RunJob, RunAnswer> {
        start_delegation(reach.client.cloned(), spec, reach.wake.cloned())
    }

    fn cancel(&mut self, reach: Reach<'_>, run: RunId) -> Receiver<Result<(), String>> {
        let (send, receive) = mpsc::channel();
        let Some(client) = reach.client.cloned() else {
            let _ = send.send(Err(UNAVAILABLE.to_owned()));
            return receive;
        };
        let wake = reach.wake.cloned();
        // A thread that cannot start drops `send`, and the pass reads the
        // closed channel as `worker_unavailable`.
        let _ = std::thread::Builder::new()
            .name("koru-canvas-run-stop".into())
            .spawn(move || {
                let answer = client
                    .runs()
                    .cancel(run)
                    .map(drop)
                    .map_err(|failure| failure.code);
                deliver(&send, wake.as_ref(), answer);
            });
        receive
    }

    fn read(
        &mut self,
        reach: Reach<'_>,
        version: ArtifactVersionId,
    ) -> Receiver<Result<ResultBody, String>> {
        let (send, receive) = mpsc::channel();
        let Some(client) = reach.client.cloned() else {
            let _ = send.send(Err("store_unavailable".to_owned()));
            return receive;
        };
        let wake = reach.wake.cloned();
        let _ = std::thread::Builder::new()
            .name("koru-canvas-run-result".into())
            .spawn(move || {
                let answer = koru_artifacts_ui::read_result(&client, version)
                    .map_err(|failure| failure.code);
                deliver(&send, wake.as_ref(), answer);
            });
        receive
    }

    fn watch(
        &mut self,
        reach: Reach<'_>,
        workspace: uuid::Uuid,
        touched: Touched,
    ) -> Option<Watch> {
        let client = reach.client?.clone();
        if !client.events().available() {
            return None;
        }
        let wake = reach.wake.cloned();
        let stop = Arc::new(AtomicBool::new(false));
        let held: Arc<Mutex<Option<koru_api_client::Subscription>>> = Arc::default();
        let (thread_stop, thread_held) = (stop.clone(), held.clone());
        // **Subscribing is a request** — `on` asks the journal where its head
        // is — so it happens here, off the window thread, retrying with the
        // same backoff the reminders' subscription uses.
        std::thread::Builder::new()
            .name("koru-canvas-run-events".into())
            .spawn(move || {
                let mut delay = Duration::from_millis(250);
                while !thread_stop.load(Ordering::Relaxed) {
                    let (touched, wake) = (touched.clone(), wake.clone());
                    let subscribed = client.events().on(
                        None,
                        koru_api_client::Filter::names("delegate.").workspace(workspace),
                        move |event| {
                            let Some(subject) = event.envelope.subject.as_ref() else {
                                return;
                            };
                            if subject.kind.token() != "document" {
                                return;
                            }
                            let Ok(id) = uuid::Uuid::parse_str(&subject.id) else {
                                return;
                            };
                            if let Ok(mut touched) = touched.lock() {
                                touched.insert(id);
                            }
                            if let Some(proxy) = &wake {
                                let _ = proxy.send_event(koru_workspace_host::app::Wake::Store);
                            }
                        },
                    );
                    match subscribed {
                        Ok(subscription) => {
                            if let Ok(mut held) = thread_held.lock()
                                && !thread_stop.load(Ordering::Relaxed)
                            {
                                *held = Some(subscription);
                            }
                            return;
                        }
                        Err(_) => {
                            std::thread::sleep(delay);
                            delay = (delay * 2).min(Duration::from_secs(5));
                        }
                    }
                }
            })
            .ok()?;
        Some(Watch(Box::new(Listening { stop, held })))
    }
}

/// The subscription, and the flag its setup thread watches. Dropping it
/// stops both.
struct Listening {
    stop: Arc<AtomicBool>,
    held: Arc<Mutex<Option<koru_api_client::Subscription>>>,
}

impl Drop for Listening {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut held) = self.held.lock() {
            // `Subscription`'s own drop stops its thread without joining.
            held.take();
        }
    }
}

// ------------------------------------------------------------ the window

impl MainView {
    /// **One pass over the canvas's runs**: readiness in, work out, answers
    /// folded, updates handed on. Nothing here waits: every request is a
    /// worker's, and an idle window returns after two comparisons.
    ///
    /// Runs after `maintain_run_pills`, whose read it folds in. The updates
    /// are held by [`CanvasRuns::current`] for whichever canvas draws the
    /// node, and handed to the chains they belong to
    /// (`MainView::maintain_canvas_chains`, the next call in the pass).
    pub(crate) fn maintain_canvas_runs(&mut self) -> bool {
        let lang = crate::format::lang();
        let agents_changed = self
            .canvas_runs
            .set_readiness(self.provider_readiness.as_deref(), lang);
        if self.canvas_runs.take_wants_readiness() {
            self.ensure_provider_readiness();
        }
        if self.canvas_runs.idle() {
            return agents_changed;
        }
        let wake = self.agent.wake_proxy();
        let workspace = self
            .pane_context
            .as_ref()
            .and_then(|(_, _, _, workspace)| *workspace)
            .map(|workspace| workspace.as_uuid());
        self.canvas_runs.launch(Reach {
            client: self.delegate_client.as_ref(),
            wake: wake.as_ref(),
            workspace,
        });
        self.canvas_runs.observe(&self.run_pill_states);
        let updates = self.canvas_runs.maintain();
        let moved = !updates.is_empty();
        // The chains these runs belong to hear of them next, in
        // `maintain_canvas_chains`, which runs right after this pass.
        self.canvas_chains.heard(updates);
        if self.canvas_runs.take_early_read() {
            self.run_pill_worker.nudge();
        }
        if self.canvas_runs.take_created() {
            // The Agents navigator lists runs only when asked, and nothing
            // else on this device knows these were made.
            self.run_index.refresh();
        }
        let notices = self.canvas_runs.take_notices();
        if let Some(notice) = notices.last() {
            self.toast = Some((
                koru_ui::Toast::new(notice.clone(), None, std::time::Instant::now()),
                super::ToastPayload::None,
            ));
        }
        let read = !self.canvas_runs.read.is_empty();
        agents_changed || moved || !notices.is_empty() || read
    }

    /// A spec for an agent node in this window's workspace, as the person
    /// this window acts for — `None` with no workspace open.
    pub(crate) fn canvas_run_spec(
        &self,
        objective: impl Into<String>,
        agent: SessionProfileKey,
        sources: Vec<koru_delegate_context::ContextSource>,
    ) -> Option<RunSpec> {
        let (_, profile, _, Some(workspace)) = self.pane_context.as_ref()? else {
            return None;
        };
        let mut spec = RunSpec::new(
            koru_delegate_core::WorkspaceId::from_uuid(workspace.as_uuid()),
            koru_store_core::ProfileId::from_uuid(profile.as_uuid()),
            objective,
            agent,
        );
        spec.sources = sources;
        Some(spec)
    }
}

#[cfg(test)]
mod tests;
