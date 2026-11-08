// SPDX-License-Identifier: MPL-2.0

//! A canvas's runs, driven through a port that answers from memory: no
//! gateway, no store, no thread of the app's own, no telemetry. The test
//! holds each answer's sender and decides when it arrives.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use koru_delegate_core::{
    ActorRef, Attention, AttentionSet, DelegationRequest, FailureStage, Objective, ResultReference,
    RunFailureReason, RunHeader, RunId, RunStatus, StepKind, TimelineEntry, TurnOriginator,
    WorkspaceScope,
};
use koru_delegate_document::RunDocument;
use koru_i18n::{Key, Lang, t};
use koru_session_protocol::{
    CapabilitySet, ExecutableAvailability, ProviderReadiness, SessionProfileId, capability,
};

use super::*;

// ------------------------------------------------------------ the fake port

#[derive(Default)]
struct Seen {
    created: Vec<RunSpec>,
    creations: HashMap<RunId, mpsc::Sender<RunAnswer>>,
    cancels: Vec<RunId>,
    cancel_answers: HashMap<RunId, mpsc::Sender<Result<(), String>>>,
    reads: Vec<ArtifactVersionId>,
    read_answers: Vec<mpsc::Sender<Result<ResultBody, String>>>,
    watches: Vec<uuid::Uuid>,
    touched: Option<Touched>,
}

/// The port a test answers. Cloned into the `CanvasRuns` it drives; both
/// clones see the same record.
#[derive(Clone, Default)]
struct Fake {
    seen: Arc<Mutex<Seen>>,
    listening: Arc<AtomicBool>,
}

impl Fake {
    fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
        self.seen.lock().unwrap()
    }

    /// The host created `run`, registered as `document`.
    fn created(&self, run: RunId, document: DocumentId) {
        let send = self.seen().creations.remove(&run).expect("a creation");
        send.send(RunAnswer::Done {
            run,
            document,
            busy: None,
        })
        .unwrap();
    }

    fn refused(&self, run: RunId, code: &str) {
        let send = self.seen().creations.remove(&run).expect("a creation");
        send.send(RunAnswer::Refused { code: code.into() }).unwrap();
    }

    fn cancel_answered(&self, run: RunId, answer: Result<(), String>) {
        let send = self.seen().cancel_answers.remove(&run).expect("a cancel");
        send.send(answer).unwrap();
    }

    /// A `delegate.*` event named this Run document.
    fn event_about(&self, document: DocumentId) {
        let touched = self.seen().touched.clone().expect("listening");
        touched.lock().unwrap().insert(document.as_uuid());
    }
}

/// Held for as long as the fake's subscription is.
struct Listening(Arc<AtomicBool>);

impl Drop for Listening {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

impl RunPort for Fake {
    fn create(&mut self, _reach: Reach<'_>, spec: RunSpec) -> Result<RunJob, RunAnswer> {
        let (send, receive) = mpsc::channel();
        let run = spec.run;
        let mut seen = self.seen();
        seen.created.push(spec);
        seen.creations.insert(run, send);
        Ok(RunJob::from_receiver(run, receive))
    }

    fn cancel(&mut self, _reach: Reach<'_>, run: RunId) -> Receiver<Result<(), String>> {
        let (send, receive) = mpsc::channel();
        let mut seen = self.seen();
        seen.cancels.push(run);
        seen.cancel_answers.insert(run, send);
        receive
    }

    fn read(
        &mut self,
        _reach: Reach<'_>,
        version: ArtifactVersionId,
    ) -> Receiver<Result<ResultBody, String>> {
        let (send, receive) = mpsc::channel();
        let mut seen = self.seen();
        seen.reads.push(version);
        seen.read_answers.push(send);
        receive
    }

    fn watch(
        &mut self,
        _reach: Reach<'_>,
        workspace: uuid::Uuid,
        touched: Touched,
    ) -> Option<Watch> {
        let mut seen = self.seen();
        seen.watches.push(workspace);
        seen.touched = Some(touched);
        self.listening.store(true, Ordering::Relaxed);
        Some(Watch(Box::new(Listening(self.listening.clone()))))
    }
}

// ------------------------------------------------------------ fixtures

fn delegatable(kind: &str) -> ProviderReadiness {
    ProviderReadiness {
        kind: kind.parse().unwrap(),
        availability: ExecutableAvailability::Available { version: None },
        capabilities: CapabilitySet::new(vec![capability::MANAGED_DELEGATION.parse().unwrap()]),
        failure: None,
    }
}

/// Every managed provider this build registers, ready to delegate to.
fn all_ready() -> Vec<ProviderReadiness> {
    koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS
        .iter()
        .map(|entry| delegatable(entry.kind))
        .collect()
}

/// The built-in agent profiles that delegate, in catalog order.
fn agents() -> Vec<SessionProfileKey> {
    koru_session_profiles::built_in_profiles()
        .into_iter()
        .filter(|profile| {
            profile
                .provider
                .as_ref()
                .and_then(koru_session_profiles::managed_provider)
                .is_some()
        })
        .map(|profile| profile.id)
        .collect()
}

fn an_agent() -> SessionProfileKey {
    agents().into_iter().next().expect("a delegatable built-in")
}

/// The language the window draws in — what `CanvasRuns` words come out in.
fn lang() -> Lang {
    crate::format::lang()
}

const WORKSPACE: uuid::Uuid = uuid::Uuid::from_u128(0xc0ffee);

fn a_spec() -> RunSpec {
    RunSpec::new(
        koru_delegate_core::WorkspaceId::from_uuid(WORKSPACE),
        koru_store_core::ProfileId::new(),
        "Summarise the cards",
        an_agent(),
    )
}

fn a_node() -> NodeKey {
    (DocumentId::new(), NodeId::new())
}

fn reach() -> Reach<'static> {
    Reach {
        client: None,
        wake: None,
        workspace: Some(WORKSPACE),
    }
}

/// A `CanvasRuns` over the fake, with every agent ready.
fn ready_runs() -> (CanvasRuns, Fake) {
    let fake = Fake::default();
    let mut runs = CanvasRuns::with_port(Box::new(fake.clone()));
    runs.set_readiness(Some(&all_ready()), Lang::En);
    (runs, fake)
}

/// One pass the way the window makes it.
fn pass(runs: &mut CanvasRuns, states: &HashMap<String, RunPillState>) -> Vec<RunUpdate> {
    runs.launch(reach());
    runs.observe(states);
    runs.maintain()
}

fn only(updates: Vec<RunUpdate>, key: NodeKey) -> RunUpdate {
    let mut mine: Vec<RunUpdate> = updates.into_iter().filter(|u| u.key == key).collect();
    assert_eq!(mine.len(), 1, "{mine:?}");
    mine.remove(0)
}

/// A node whose run the host has created, returned with its run and Run
/// document.
fn a_created_run(runs: &mut CanvasRuns, fake: &Fake) -> (NodeKey, RunId, DocumentId) {
    let key = a_node();
    let run = runs.start(key, a_spec());
    pass(runs, &HashMap::new());
    let document = DocumentId::new();
    fake.created(run, document);
    let update = only(pass(runs, &HashMap::new()), key);
    assert_eq!(update.run_document, Some(document));
    (key, run, document)
}

/// The run's header as its executing device commits it, read the way the
/// run pills' worker reads it.
fn read_as(
    status: RunStatus,
    results: Vec<ResultReference>,
    failure: Option<RunFailureReason>,
    steps: &[StepKind],
) -> RunPillState {
    let mut header = RunHeader::from_request(
        DelegationRequest {
            requested_model: None,
            run: RunId::mint(),
            requested_by: ActorRef::Person {
                profile: koru_store_core::ProfileId::new(),
            },
            objective: Objective::new("Summarise the cards").unwrap(),
            agent: an_agent(),
            scope: WorkspaceScope::Temporary,
            context: Vec::new(),
            grants: Default::default(),
            executing_device: koru_device::DeviceId::from_uuid(uuid::Uuid::from_bytes([7; 16])),
            origin: Default::default(),
        },
        1_700_000_000_000,
    );
    header.status = status;
    header.failure_reason = failure;
    let run = RunDocument::create(&header).unwrap();
    // Results are registered the way the executing device registers them.
    for result in &results {
        run.register_result(result).unwrap();
    }
    for (n, step) in steps.iter().enumerate() {
        run.append_step(&TimelineEntry::new(
            *step,
            Default::default(),
            1_700_000_001_000 + n as i64,
        ))
        .unwrap();
    }
    RunPillState::of(&run, &run.header().unwrap(), 1_700_000_060_000)
}

fn a_result() -> ResultReference {
    ResultReference {
        // A v4 id, as a stored reference must be: the document reads it back.
        artifact: ArtifactVersionId::mint(),
        held_by: koru_device::DeviceId::from_uuid(uuid::Uuid::from_bytes([7; 16])),
        title: koru_delegate_core::ArtifactTitle::new("The summary").unwrap(),
        size_bytes: 1_024,
        registered_at_ms: 1_700_000_050_000,
    }
}

fn states(document: DocumentId, state: RunPillState) -> HashMap<String, RunPillState> {
    HashMap::from([(document.to_storage(), state)])
}

// ------------------------------------------------------------ starting

/// **Many at once, and none of them the modal's.** Two agent nodes start
/// while the delegation modal's single job is in flight; both reach a
/// worker, both runs are read by the pills' worker, and the modal's job is
/// exactly where it was.
#[test]
fn two_canvas_runs_start_while_the_modals_job_is_in_flight() {
    use crate::mainview::delegate::job::Job;
    use crate::mainview::delegate::modal::{RequestCorrelation, RequestTarget, RequestToken};

    let (mut view, _workspace) = crate::mainview::harness::container();
    let fake = Fake::default();
    view.canvas_runs = CanvasRuns::with_port(Box::new(fake.clone()));
    view.provider_readiness = Some(all_ready());
    let correlation = RequestCorrelation {
        token: RequestToken::mint(),
        mode: crate::mainview::delegate::modal::repeat::Kind::RunOnce,
        target: RequestTarget::Run(RunId::mint()),
        edit: false,
    };
    let (_modal_answer, modal_receiver) = mpsc::channel();
    view.delegate_job = Some(Job {
        correlation,
        pill_anchor: None,
        schedule_name: None,
        receiver: modal_receiver,
    });

    let (first, second) = (a_node(), a_node());
    let spec = view
        .canvas_run_spec("Summarise the cards", an_agent(), Vec::new())
        .expect("a workspace is open");
    let a = view.canvas_runs.start(first, spec.clone());
    let b = view.canvas_runs.start(second, spec);
    assert_ne!(a, b, "one spec pressed twice is two runs");
    assert!(view.maintain_canvas_runs());

    assert_eq!(fake.seen().created.len(), 2, "both went to a worker");
    for key in [first, second] {
        assert_eq!(
            view.canvas_runs.current(&key).map(|u| u.state.clone()),
            Some(NodeRunState::Starting)
        );
    }
    assert_eq!(
        view.delegate_job.as_ref().map(|job| job.correlation),
        Some(correlation),
        "the modal's job is untouched"
    );

    let (doc_a, doc_b) = (DocumentId::new(), DocumentId::new());
    fake.created(a, doc_a);
    fake.created(b, doc_b);
    assert!(view.maintain_canvas_runs());
    // The pills' worker is now asked about both — the one poller.
    view.maintain_run_pills(std::time::Instant::now());
    assert!(view.run_pills_wanted.contains_key(&doc_a.to_storage()));
    assert!(view.run_pills_wanted.contains_key(&doc_b.to_storage()));

    // What that worker reads is what the nodes say.
    let working = read_as(RunStatus::running(), Vec::new(), None, &[StepKind::Started]);
    view.run_pill_states
        .insert(doc_a.to_storage(), working.clone());
    view.run_pill_states.insert(doc_b.to_storage(), working);
    assert!(view.maintain_canvas_runs());
    for key in [first, second] {
        assert_eq!(
            view.canvas_runs.current(&key).map(|u| u.state.clone()),
            Some(NodeRunState::Running)
        );
    }
    assert!(view.delegate_job.is_some());
}

/// A spec reused for a second press is a second run, never the first id
/// re-pinned.
#[test]
fn a_spec_pressed_twice_is_two_runs() {
    let (mut runs, fake) = ready_runs();
    let spec = a_spec();
    let first = runs.start(a_node(), spec.clone());
    let second = runs.start(a_node(), spec.clone());
    assert_eq!(first, spec.run, "the first press keeps the id it was given");
    assert_ne!(second, first);
    pass(&mut runs, &HashMap::new());
    let ids: Vec<RunId> = fake.seen().created.iter().map(|spec| spec.run).collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
}

/// **Nothing is created before this machine's agents are known**, and an
/// agent that cannot run here leaves the node `NotReady` with what to do.
#[test]
fn a_run_waits_for_readiness_and_an_agent_that_cannot_run_is_not_ready() {
    let fake = Fake::default();
    let mut runs = CanvasRuns::with_port(Box::new(fake.clone()));
    let key = a_node();
    runs.start(key, a_spec());
    assert!(runs.take_wants_readiness(), "starting asks for readiness");
    let update = only(pass(&mut runs, &HashMap::new()), key);
    assert_eq!(update.state, NodeRunState::Starting);
    assert!(fake.seen().created.is_empty(), "nothing before readiness");

    // The chosen agent is known and missing.
    let mut missing = all_ready();
    for row in &mut missing {
        row.availability = ExecutableAvailability::Missing;
        row.capabilities = CapabilitySet::default();
    }
    runs.set_readiness(Some(&missing), Lang::En);
    let row = runs
        .agents
        .iter()
        .find(|row| row.key == an_agent())
        .expect("the chosen agent is listed")
        .clone();
    let guidance = readiness::guidance(&row.readiness).expect("a missing agent has guidance");
    let update = only(pass(&mut runs, &HashMap::new()), key);
    assert_eq!(
        update.state,
        NodeRunState::NotReady(t(lang(), guidance).to_owned())
    );
    assert_eq!(update.run, None, "no run exists");
    assert_eq!(update.status_word, None);
    assert!(fake.seen().created.is_empty());

    // An agent this machine has never heard of, and nothing ready at all.
    let other = a_node();
    let mut spec = a_spec();
    spec.agent = SessionProfileKey::Custom(SessionProfileId::mint());
    runs.start(other, spec);
    let update = only(pass(&mut runs, &HashMap::new()), other);
    assert_eq!(
        update.state,
        NodeRunState::NotReady(t(lang(), Key::DelegateNoAgents).to_owned())
    );
}

/// A create the host refuses is a failed node, in words.
#[test]
fn a_refused_create_is_a_failed_node_with_a_sentence() {
    let (mut runs, fake) = ready_runs();
    let key = a_node();
    let run = runs.start(key, a_spec());
    pass(&mut runs, &HashMap::new());
    fake.refused(run, "delegate_unavailable");
    let update = only(pass(&mut runs, &HashMap::new()), key);
    assert_eq!(
        update.state,
        NodeRunState::Failed(t(lang(), Key::DelegateUnavailable).to_owned())
    );
    assert_eq!(update.run, None);
    assert!(!runs.take_created(), "nothing was created");
}

// ------------------------------------------------------------ status

/// **Every state a node shows comes from the run's own header**, read the
/// way the run pills read it.
#[test]
fn every_node_state_comes_from_the_runs_header() {
    let tools = StepKind::ToolsCompleted {
        calls: 3,
        failed: 0,
        failures: Default::default(),
    };
    let cases: Vec<(RunPillState, NodeRunState)> = vec![
        (
            read_as(RunStatus::Created, Vec::new(), None, &[]),
            NodeRunState::Starting,
        ),
        (
            read_as(
                RunStatus::running(),
                Vec::new(),
                None,
                &[StepKind::Started, tools],
            ),
            NodeRunState::Running,
        ),
        (
            read_as(RunStatus::Completed, vec![a_result()], None, &[]),
            NodeRunState::Finished { has_result: true },
        ),
        (
            read_as(RunStatus::Completed, Vec::new(), None, &[]),
            NodeRunState::Finished { has_result: false },
        ),
        (
            read_as(
                RunStatus::Failed {
                    stage: FailureStage::ProviderStart,
                },
                Vec::new(),
                Some(RunFailureReason::AuthenticationRequired),
                &[],
            ),
            NodeRunState::Failed(
                koru_runs_ui::failed_because(
                    lang(),
                    &RunStatus::Failed {
                        stage: FailureStage::ProviderStart,
                    },
                    Some(RunFailureReason::AuthenticationRequired),
                )
                .unwrap(),
            ),
        ),
        (
            read_as(RunStatus::Cancelled, Vec::new(), None, &[]),
            NodeRunState::Stopped,
        ),
    ];
    for (read, expected) in cases {
        let (mut runs, fake) = ready_runs();
        let (key, run, document) = a_created_run(&mut runs, &fake);
        // A created run already says *Starting…*; a read that says the same
        // is not news, so the latest is asked for rather than the change.
        pass(&mut runs, &states(document, read.clone()));
        let update = runs.current(&key).cloned().expect("a node");
        assert_eq!(update.state, expected, "{:?}", read.status);
        assert_eq!(update.run, Some(run));
        assert_eq!(update.results, read.results);
        assert_eq!(
            update.status_word.as_deref(),
            Some(
                koru_runs_ui::summary(lang(), &read.status, None, None)
                    .status
                    .as_str()
            ),
            "the node says the pill's word"
        );
    }
}

/// **A running run says what it is doing**: its newest step in the run
/// views' own words, and how long it has worked. A settled run says no step.
#[test]
fn a_running_run_says_its_newest_step_and_a_settled_one_does_not() {
    let tools = StepKind::ToolsCompleted {
        calls: 3,
        failed: 0,
        failures: Default::default(),
    };
    let (mut runs, fake) = ready_runs();
    let (key, _, document) = a_created_run(&mut runs, &fake);
    let working = read_as(
        RunStatus::running(),
        Vec::new(),
        None,
        &[
            StepKind::Started,
            StepKind::TurnStarted {
                originator: TurnOriginator::Orchestrator,
            },
            tools,
        ],
    );
    let update = only(pass(&mut runs, &states(document, working)), key);
    assert_eq!(update.state, NodeRunState::Running);
    assert_eq!(
        update.activity,
        koru_runs_ui::summary(lang(), &RunStatus::running(), Some(tools), None).activity
    );
    assert!(update.elapsed_ms.is_some_and(|ms| ms > 0), "{update:?}");
    assert!(!update.needs_response);

    // A run that needs a person is still running, and says so in the pill's word.
    let needs_you = read_as(
        RunStatus::Running {
            attention: AttentionSet::new([Attention::ApprovalRequired]),
            control: Default::default(),
        },
        Vec::new(),
        None,
        &[StepKind::Started, StepKind::ApprovalRequired],
    );
    let update = only(pass(&mut runs, &states(document, needs_you)), key);
    assert_eq!(update.state, NodeRunState::Running);
    assert!(update.needs_response);
    assert_eq!(
        update.status_word.as_deref(),
        Some(t(lang(), Key::DelegateRunNeedsYou))
    );

    let done = read_as(
        RunStatus::Completed,
        vec![a_result()],
        None,
        &[StepKind::Started, StepKind::Completed],
    );
    let update = only(pass(&mut runs, &states(document, done)), key);
    assert_eq!(update.activity, None);
    assert!(!update.needs_response);
}

#[test]
fn only_provider_questions_offer_a_canvas_response() {
    for marker in [
        Attention::WaitingForInput,
        Attention::ChildSessionDetected,
        Attention::FinishedNeedingAttention,
    ] {
        let (mut runs, fake) = ready_runs();
        let (key, run, document) = a_created_run(&mut runs, &fake);
        let observed = read_as(
            RunStatus::Running {
                attention: AttentionSet::new([marker]),
                control: Default::default(),
            },
            Vec::new(),
            None,
            &[],
        );
        let update = only(pass(&mut runs, &states(document, observed)), key);
        assert_eq!(update.needs_response, marker == Attention::WaitingForInput);
        runs.stop(run);
        let update = only(pass(&mut runs, &HashMap::new()), key);
        assert!(
            !update.needs_response,
            "stopping removes the response action"
        );
    }
}

/// A pass that learned nothing new reports nothing; the latest stays in
/// `current`.
#[test]
fn an_unchanged_run_reports_nothing_twice() {
    let (mut runs, fake) = ready_runs();
    let (key, _, document) = a_created_run(&mut runs, &fake);
    let read = states(
        document,
        read_as(RunStatus::running(), Vec::new(), None, &[]),
    );
    assert_eq!(pass(&mut runs, &read).len(), 1);
    assert!(pass(&mut runs, &read).is_empty());
    assert_eq!(
        runs.current(&key).map(|u| u.state.clone()),
        Some(NodeRunState::Running)
    );
}

// ------------------------------------------------------------ stopping

/// **Stop sends one cancel and says *Stopping…* until the run stops** — a
/// cancel mid-turn leaves the run running until the provider confirms.
#[test]
fn stop_sends_one_cancel_and_says_stopping_until_the_run_has_stopped() {
    let (mut runs, fake) = ready_runs();
    let (key, run, document) = a_created_run(&mut runs, &fake);
    let working = states(
        document,
        read_as(RunStatus::running(), Vec::new(), None, &[StepKind::Started]),
    );
    assert_eq!(
        only(pass(&mut runs, &working), key).state,
        NodeRunState::Running
    );

    runs.stop(run);
    runs.stop(run);
    let update = only(pass(&mut runs, &working), key);
    assert_eq!(fake.seen().cancels, vec![run], "one cancel");
    assert_eq!(update.state, NodeRunState::Stopping);
    assert_eq!(
        update.status_word.as_deref(),
        Some(t(lang(), Key::DelegateRunStopping))
    );

    // Accepted: read it again now, and still stopping while it runs.
    fake.cancel_answered(run, Ok(()));
    pass(&mut runs, &working);
    assert!(runs.take_early_read());
    assert_eq!(
        runs.current(&key).map(|u| u.state.clone()),
        Some(NodeRunState::Stopping)
    );

    // The provider confirmed.
    let stopped = states(
        document,
        read_as(RunStatus::Cancelled, Vec::new(), None, &[]),
    );
    assert_eq!(
        only(pass(&mut runs, &stopped), key).state,
        NodeRunState::Stopped
    );
    runs.stop(run);
    pass(&mut runs, &stopped);
    assert_eq!(
        fake.seen().cancels.len(),
        1,
        "a stopped run is not stopped again"
    );
}

/// A stop the host refuses goes back to what the run says, and the window
/// is told why.
#[test]
fn a_refused_stop_returns_to_the_runs_state_and_says_so() {
    let (mut runs, fake) = ready_runs();
    let (key, run, document) = a_created_run(&mut runs, &fake);
    let working = states(
        document,
        read_as(RunStatus::running(), Vec::new(), None, &[]),
    );
    pass(&mut runs, &working);
    runs.stop(run);
    pass(&mut runs, &working);
    fake.cancel_answered(run, Err("forbidden".into()));
    let update = only(pass(&mut runs, &working), key);
    assert_eq!(update.state, NodeRunState::Running);
    let notices = runs.take_notices();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("forbidden"), "{notices:?}");
}

/// Stop pressed before the run exists: a queued run is withdrawn, and one
/// being created is cancelled the moment it is.
#[test]
fn a_stop_before_the_run_exists_withdraws_or_waits_for_it() {
    // Queued: readiness is not known, so it never leaves this window.
    let fake = Fake::default();
    let mut runs = CanvasRuns::with_port(Box::new(fake.clone()));
    let key = a_node();
    let run = runs.start(key, a_spec());
    runs.stop(run);
    let update = only(pass(&mut runs, &HashMap::new()), key);
    assert_eq!(update.state, NodeRunState::Stopped);
    assert_eq!(update.run, None);

    // Being created: stopping now, cancelled once it exists.
    let (mut runs, fake) = ready_runs();
    let key = a_node();
    let run = runs.start(key, a_spec());
    pass(&mut runs, &HashMap::new());
    runs.stop(run);
    assert_eq!(
        only(pass(&mut runs, &HashMap::new()), key).state,
        NodeRunState::Stopping
    );
    assert!(fake.seen().cancels.is_empty(), "nothing to cancel yet");
    fake.created(run, DocumentId::new());
    pass(&mut runs, &HashMap::new());
    pass(&mut runs, &HashMap::new());
    assert_eq!(fake.seen().cancels, vec![run]);
}

// ------------------------------------------------------------ results

/// **A result is read on a worker**, and a picture comes back as its blob.
#[test]
fn a_result_is_read_on_a_worker_and_answers_markdown_or_a_picture() {
    let (mut runs, fake) = ready_runs();
    let key = a_node();
    let text = ArtifactVersionId::from_uuid(uuid::Uuid::from_u128(1));
    let picture = ArtifactVersionId::from_uuid(uuid::Uuid::from_u128(2));
    let elsewhere = ArtifactVersionId::from_uuid(uuid::Uuid::from_u128(3));
    runs.read_result(key, text);
    runs.read_result(key, picture);
    runs.read_result(key, elsewhere);
    assert!(
        fake.seen().reads.is_empty(),
        "nothing is read on the calling thread"
    );
    pass(&mut runs, &HashMap::new());
    assert_eq!(fake.seen().reads, vec![text, picture, elsewhere]);

    let blob = koru_artifacts_ui::BlobHash::from_digest("b3", &"ab".repeat(32)).unwrap();
    {
        let seen = fake.seen();
        seen.read_answers[0]
            .send(Ok(ResultBody::Markdown("# Summary".into())))
            .unwrap();
        seen.read_answers[1]
            .send(Ok(ResultBody::Image {
                blob: blob.clone(),
                mime: "image/png".into(),
            }))
            .unwrap();
        seen.read_answers[2]
            .send(Err("blob_not_found".into()))
            .unwrap();
    }
    pass(&mut runs, &HashMap::new());
    let mut read = runs.take_results();
    read.sort_by_key(|answer| answer.version.as_uuid());
    assert_eq!(read.len(), 3);
    assert_eq!(read[0].body, Ok(ResultBody::Markdown("# Summary".into())));
    assert_eq!(
        read[1].body,
        Ok(ResultBody::Image {
            blob,
            mime: "image/png".into()
        })
    );
    assert_eq!(
        read[2].body,
        Err(t(lang(), Key::ArtifactsHeldElsewhereUnknown).to_owned())
    );
    assert!(read.iter().all(|answer| answer.key == key));
    assert!(runs.take_results().is_empty());
}

// ------------------------------------------------------------ agents

/// **The agent menu is the modal's list**, the ready rows of it, and a new
/// node starts with the agent the modal last used while it is still ready.
#[test]
fn ready_agents_come_from_the_modals_snapshot_and_the_default_follows_the_modal() {
    let (mut runs, _fake) = ready_runs();
    let ready = runs.ready_agents();
    let every = agents();
    assert_eq!(
        ready.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(),
        every
    );
    assert!(ready.iter().all(|(_, name)| !name.is_empty()));
    assert_eq!(runs.default_agent(), every.first().cloned());

    if let Some(second) = every.get(1) {
        runs.remember_modal_agent(second.clone());
        assert_eq!(runs.default_agent(), Some(second.clone()));
    }
    // An agent that is no longer ready is not offered as the default.
    runs.remember_modal_agent(SessionProfileKey::Custom(SessionProfileId::mint()));
    assert_eq!(runs.default_agent(), every.first().cloned());

    // Nothing known yet: nothing offered.
    let fresh = CanvasRuns::with_port(Box::new(Fake::default()));
    assert!(fresh.ready_agents().is_empty());
    assert_eq!(fresh.default_agent(), None);
}

/// **An agent turned off on Agent providers is not offered on a canvas**
/// (D42): it leaves the agent menu and is never a new node's default, even
/// as the one the modal last used; the rest follow the order set there.
/// Turned back on, it is offered again.
#[test]
fn canvas_runs_hide_agents_turned_off_and_follow_the_order() {
    let (mut runs, _fake) = ready_runs();
    let every = agents();
    let Some(first) = every.first().cloned() else {
        return;
    };
    // An agent's key is its managed provider kind.
    let kind = |runs: &CanvasRuns, key: &SessionProfileKey| {
        runs.agents
            .iter()
            .find(|row| row.key == *key)
            .map(|row| row.provider.as_str().to_owned())
            .expect("a row")
    };
    let first_kind = kind(&runs, &first);
    runs.remember_modal_agent(first.clone());
    assert!(runs.set_agent_settings(vec![first_kind.clone()], Vec::new()));
    assert!(
        !runs.set_agent_settings(vec![first_kind.clone()], Vec::new()),
        "no change"
    );
    let ready: Vec<_> = runs
        .ready_agents()
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    assert!(!ready.contains(&first), "turned off, so not offered");
    assert_eq!(ready.len(), every.len() - 1);
    assert_ne!(runs.default_agent(), Some(first.clone()), "nor the default");

    // The order: the last agent listed first.
    if let Some(last) = every.last().cloned().filter(|last| *last != first) {
        let last_kind = kind(&runs, &last);
        runs.set_agent_settings(Vec::new(), vec![last_kind]);
        assert_eq!(
            runs.ready_agents().first().map(|(key, _)| key.clone()),
            Some(last)
        );
    }
    runs.set_agent_settings(Vec::new(), Vec::new());
    assert_eq!(runs.ready_agents().len(), every.len(), "back on");
    assert_eq!(runs.default_agent(), Some(first));
}

// ------------------------------------------------------------ the event bus

/// **A `delegate.*` event about a canvas run reads it early**; one about
/// another run does not. Listening ends once every run has settled.
#[test]
fn an_event_about_a_canvas_run_reads_it_early_and_listening_ends_when_all_settle() {
    let (mut runs, fake) = ready_runs();
    let (key, _, document) = a_created_run(&mut runs, &fake);
    assert_eq!(fake.seen().watches, vec![WORKSPACE], "one subscription");
    assert!(fake.listening.load(Ordering::Relaxed));
    runs.take_early_read();

    fake.event_about(DocumentId::new());
    pass(&mut runs, &HashMap::new());
    assert!(!runs.take_early_read(), "another run's event is not ours");

    fake.event_about(document);
    pass(&mut runs, &HashMap::new());
    assert!(runs.take_early_read());

    let done = states(
        document,
        read_as(RunStatus::Completed, Vec::new(), None, &[]),
    );
    assert_eq!(
        only(pass(&mut runs, &done), key).state,
        NodeRunState::Finished { has_result: false }
    );
    pass(&mut runs, &done);
    assert!(
        !fake.listening.load(Ordering::Relaxed),
        "a settled canvas holds no subscription"
    );
    assert_eq!(fake.seen().watches.len(), 1);
}

/// An idle window asks nothing and holds nothing.
#[test]
fn an_idle_window_does_no_canvas_work() {
    let (mut view, _workspace) = crate::mainview::harness::container();
    let fake = Fake::default();
    view.canvas_runs = CanvasRuns::with_port(Box::new(fake.clone()));
    assert!(view.canvas_runs.idle());
    assert!(!view.maintain_canvas_runs());
    let seen = fake.seen();
    assert!(seen.created.is_empty() && seen.watches.is_empty() && seen.reads.is_empty());
}
