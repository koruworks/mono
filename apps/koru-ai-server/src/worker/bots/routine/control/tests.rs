// SPDX-License-Identifier: MPL-2.0
//! The owner controls through the real worker, the real authority adapter and
//! a no-path Store that holds the Routine's actual Schedule document.
//!
//! The delegate is a scripted peer, [`Coordinator`]. It applies each control
//! to that same document with the document crate's own `set_lifecycle` and
//! `edit`, so a revision conflict, a state conflict and an exact replay are
//! the document's answers, not the fixture's. It keeps the Bot's admission
//! barrier the worker installs and refuses Enable and Test while it is
//! paused, as the coordinator does. No real Store, Profile, provider or
//! network is touched, and the clock is a value the test sets.
use super::*;
use crate::BotAuthorityAdapter;
use crate::bots::routine::fixture::Fixture;
use koru_delegate_core::{
    BotAdmissionBarrier, BotAdmissionState, BotAdmissionView, BotBootAdmission, BotHello,
    BotPolicyBarrierReceipt, BotPolicyBarrierState, BotSubmissionKey, BotUnsettledAdmission,
    BotUnsettledWork, DelegateBootId, RoutineClaimObservation, RoutineControlLifecycle,
    RoutineControlOutcome, RoutineDefinitionIdentity,
};
use koru_delegate_document::{ScheduleDocument, ScheduleDocumentError, ScheduleWrite};
use koru_store_server::StoreWorkerClient;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicUsize};

mod access;
mod lifecycle;
mod team_pause;

const MINUTE: i64 = 60_000;
/// The fixed wall clock of these tests: five seconds into a minute.
const NOW: i64 = 28_000_000 * MINUTE + 5_000;

pub(in crate::worker::bots) struct Gate {
    entered: SyncSender<()>,
    release: Receiver<()>,
}
pub(in crate::worker::bots) struct Release(Option<SyncSender<()>>);
impl Release {
    pub(in crate::worker::bots) fn open(&mut self) {
        if let Some(send) = self.0.take() {
            let _ = send.send(());
        }
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        self.open();
    }
}
pub(in crate::worker::bots) fn gate() -> (Gate, Receiver<()>, Release) {
    let (entered, observe) = sync_channel(1);
    let (release, released) = sync_channel(1);
    (
        Gate {
            entered,
            release: released,
        },
        observe,
        Release(Some(release)),
    )
}

fn refused(code: &str) -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new(code, "fixture coordinator refusal", false)
}
fn document_refusal(error: ScheduleDocumentError) -> koru_delegate_api::Failure {
    refused(match error {
        ScheduleDocumentError::RevisionConflict { .. } => "schedule_revision_conflict",
        ScheduleDocumentError::LifecycleConflict => "schedule_state_conflict",
        ScheduleDocumentError::NameConflict => "schedule_name_conflict",
        ScheduleDocumentError::NotOwner => "schedule_not_owner",
        ScheduleDocumentError::HandoffPending => "schedule_handoff_pending",
        ScheduleDocumentError::Retired => "schedule_retired",
        _ => "invalid_schedule",
    })
}

/// What the Routine's Schedule document says now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::worker::bots) struct Live {
    pub revision: u32,
    pub paused: bool,
    pub name: String,
    pub effective_at_ms: i64,
}

/// The scripted schedule coordinator and Bot ledger of one device.
pub(in crate::worker::bots) struct Coordinator {
    store: StoreWorkerClient,
    identity: BotRoutineCreationIdentity,
    boot: DelegateBootId,
    /// Every command that reached the owner, by its wire name, with the
    /// revision a control named.
    pub calls: Mutex<Vec<String>>,
    /// Refusal codes for the next controls or Tests, answered before
    /// anything is applied.
    pub script: Mutex<VecDeque<&'static str>>,
    /// The next controls or Tests are applied and their reply is lost.
    pub lose_replies: AtomicUsize,
    /// The coordinator answers no control at all, and applies none.
    pub silent: AtomicBool,
    /// Holds the next control or Test before it is handled.
    pub gate: Mutex<Option<Gate>>,
    /// The delegate's own current minute.
    pub minute: AtomicI64,
    /// Every accepted Test: its minute and the paused revision it names.
    pub tests: Mutex<Vec<(i64, u32)>>,
    /// The Runs of the Bot the owner has not settled, and whether more are
    /// queued past the page.
    pub unsettled: Mutex<(Vec<koru_delegate_core::RunId>, bool)>,
    /// Every Routine policy barrier installed, in order.
    pub routine_policies: Mutex<Vec<RoutinePolicyBarrier>>,
    /// The next Routine policy installs get no answer.
    pub policy_failures: AtomicUsize,
    /// The Bot's admission barrier in the ledger.
    pub admission: Mutex<Option<BotAdmissionBarrier>>,
}
impl Coordinator {
    pub(in crate::worker::bots) fn new(f: &Fixture) -> Arc<Self> {
        let identity = f.creation.identity.clone();
        Arc::new(Self {
            store: f.memory.client(),
            // The Bot's boot pass installed its barrier: present, not paused.
            admission: Mutex::new(Some(BotAdmissionBarrier {
                owner: identity.owner,
                bot: identity.bot,
                request: BotRequestId::mint(),
                epoch: f.bot.revision,
                paused: f.bot.paused,
            })),
            identity,
            boot: DelegateBootId::mint(),
            calls: Default::default(),
            script: Default::default(),
            lose_replies: AtomicUsize::new(0),
            silent: AtomicBool::new(false),
            gate: Default::default(),
            minute: AtomicI64::new(NOW - NOW % MINUTE),
            tests: Default::default(),
            unsettled: Default::default(),
            routine_policies: Default::default(),
            policy_failures: AtomicUsize::new(0),
        })
    }
    pub(in crate::worker::bots) fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    /// The calls of one wire name, with what each named.
    pub(in crate::worker::bots) fn named(&self, prefix: &str) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|call| call.starts_with(prefix))
            .collect()
    }
    fn bot_paused(&self) -> bool {
        self.admission
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|barrier| barrier.paused)
    }
    /// One read-modify-write of the Routine's Schedule document in the Store.
    fn document<R: Send + 'static>(
        &self,
        change: impl FnOnce(&ScheduleDocument) -> Result<R, koru_delegate_api::Failure> + Send + 'static,
    ) -> Result<R, koru_delegate_api::Failure> {
        let id = self.identity.clone();
        self.store
            .with_store(move |store| {
                let candidate = koru_ai_core::DocumentId::from_uuid(id.schedule.as_uuid());
                if store
                    .document_entry_live(id.home, candidate)
                    .ok()
                    .flatten()
                    .is_none()
                {
                    return Err(refused("schedule_not_found"));
                }
                let doc =
                    ScheduleDocument::from_snapshot(&store.snapshot(id.home, candidate).unwrap())
                        .unwrap();
                let before = doc.version();
                let result = change(&doc)?;
                if doc.version() != before {
                    store
                        .apply_update(
                            id.home,
                            candidate,
                            doc.export_updates_since(&before).unwrap(),
                            Timestamp::from_millis(5),
                            None,
                        )
                        .unwrap();
                    store
                        .flush(id.home, candidate, Timestamp::from_millis(5))
                        .unwrap();
                }
                Ok(result)
            })
            .map_err(|_| koru_delegate_api::Failure::unavailable())?
    }
    pub(in crate::worker::bots) fn live(&self) -> Live {
        self.document(|doc| {
            let definition = doc.header().unwrap().definition;
            Ok(Live {
                revision: definition.revision,
                paused: BotRoutineCaptureLifecycle::of(&definition.lifecycle)
                    != Some(BotRoutineCaptureLifecycle::Active),
                name: doc.name().unwrap().as_str().to_owned(),
                effective_at_ms: definition.effective_at_ms,
            })
        })
        .unwrap()
    }
    /// The live definition, as the scheduler would offer it to the AI owner.
    pub(in crate::worker::bots) fn definition(&self) -> RoutineScheduleDefinition {
        self.document(|doc| Ok(doc.header().unwrap().definition))
            .unwrap()
    }
    /// Record one occurrence in the Schedule document, as the scheduler does.
    pub(in crate::worker::bots) fn fire(&self, occurrence_ms: i64, outcome: &'static str) {
        self.document(move |doc| {
            let revision = doc.header().unwrap().definition.revision;
            let firing = serde_json::from_value(serde_json::json!({
                "occurrence_ms": occurrence_ms,
                "revision": revision,
                "run": koru_delegate_core::RunId::mint(),
                "outcome": outcome,
                "at": occurrence_ms + 7,
            }))
            .unwrap();
            doc.append_firing(&firing).unwrap();
            Ok(())
        })
        .unwrap()
    }
    fn hold(&self) {
        let held = self.gate.lock().unwrap().take();
        if let Some(gate) = held {
            gate.entered.send(()).unwrap();
            gate.release.recv_timeout(Duration::from_secs(20)).unwrap();
        }
    }
    /// A scripted refusal, or silence, before anything is applied.
    fn interposed(&self) -> Result<(), koru_delegate_api::Failure> {
        if self.silent.load(Ordering::SeqCst) {
            return Err(koru_delegate_api::Failure::unavailable());
        }
        match self.script.lock().unwrap().pop_front() {
            Some("delegate_unavailable") => Err(koru_delegate_api::Failure::unavailable()),
            Some(code) => Err(koru_delegate_api::Failure::new(
                code,
                "fixture coordinator refusal",
                matches!(
                    code,
                    "schedule_unavailable"
                        | "routine_test_minute_changed"
                        | "routine_test_too_soon"
                        | "routine_test_busy"
                ),
            )),
            None => Ok(()),
        }
    }
    fn lost(&self) -> Result<(), koru_delegate_api::Failure> {
        if self
            .lose_replies
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok()
        {
            return Err(koru_delegate_api::Failure::unavailable());
        }
        Ok(())
    }
    fn serve(&self, command: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        // No owner call may run on the thread that answers a person.
        assert!(
            std::thread::current()
                .name()
                .unwrap_or_default()
                .starts_with("koru-ai-provider"),
            "an owner call ran on the writer"
        );
        let name = serde_json::to_value(&command)
            .ok()
            .and_then(|value| value["command"].as_str().map(str::to_owned))
            .unwrap_or_default();
        match command {
            DelegateCommand::ControlRoutine {
                route,
                expected_revision,
                action,
            } => {
                let label = match &action {
                    RoutineControlAction::Enable {} => "enable",
                    RoutineControlAction::Pause {} => "pause",
                    RoutineControlAction::Edit { .. } => "edit",
                };
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("{label}@{expected_revision}"));
                self.hold();
                self.interposed()?;
                if route != self.identity.route() {
                    return Err(refused("routine_route_mismatch"));
                }
                // The Bot's current admission barrier, read inside Enable.
                if matches!(action, RoutineControlAction::Enable {}) && self.bot_paused() {
                    return Err(refused("routine_bot_paused"));
                }
                let device = route.owner.device;
                let (write, name, revision, effective_at_ms, active) =
                    self.document(move |doc| {
                        let current = doc.header().unwrap().definition;
                        let effective = current.effective_at_ms + 1;
                        let write = match action {
                            RoutineControlAction::Enable {} => doc.set_lifecycle(
                                device,
                                expected_revision,
                                serde_json::from_str("\"active\"").unwrap(),
                                effective,
                            ),
                            RoutineControlAction::Pause {} => doc.set_lifecycle(
                                device,
                                expected_revision,
                                serde_json::from_str(
                                    r#"{"paused":{"reason":{"reason":"manual"}}}"#,
                                )
                                .unwrap(),
                                effective,
                            ),
                            RoutineControlAction::Edit {
                                expected_name,
                                name,
                                recurrence,
                            } => {
                                // The coordinator builds the definition from
                                // the current one, and reuses the stored
                                // effective time on an exact replay.
                                let mut next = current.clone();
                                if current.revision == expected_revision {
                                    next.revision = expected_revision + 1;
                                    next.effective_at_ms = effective;
                                }
                                next.recurrence = recurrence;
                                doc.edit(device, expected_revision, &expected_name, &name, &next)
                            }
                        }
                        .map_err(document_refusal)?;
                        let after = doc.header().unwrap().definition;
                        Ok((
                            write,
                            doc.name().unwrap(),
                            after.revision,
                            after.effective_at_ms,
                            BotRoutineCaptureLifecycle::of(&after.lifecycle)
                                == Some(BotRoutineCaptureLifecycle::Active),
                        ))
                    })?;
                self.lost()?;
                Ok(BotReply::RoutineControlled {
                    route,
                    outcome: match write {
                        ScheduleWrite::Applied => RoutineControlOutcome::Applied,
                        ScheduleWrite::AlreadyApplied => RoutineControlOutcome::AlreadyApplied,
                    },
                    name,
                    definition: RoutineDefinitionIdentity {
                        revision,
                        digest: RoutineDigest32::from_bytes([9; 32]),
                        owner_handoff: None,
                    },
                    effective_at_ms,
                    lifecycle: if active {
                        RoutineControlLifecycle::Active
                    } else {
                        RoutineControlLifecycle::Paused
                    },
                })
            }
            DelegateCommand::TestRoutine {
                route,
                expected_revision,
                occurrence_ms,
            } => {
                self.calls.lock().unwrap().push(format!(
                    "test@{expected_revision}/{}",
                    occurrence_ms / MINUTE
                ));
                self.hold();
                self.interposed()?;
                if route != self.identity.route() {
                    return Err(refused("routine_route_mismatch"));
                }
                if self.bot_paused() {
                    return Err(refused("routine_bot_paused"));
                }
                let live = self.live();
                if !live.paused {
                    return Err(refused("routine_test_requires_paused"));
                }
                if live.revision != expected_revision {
                    return Err(refused("schedule_revision_conflict"));
                }
                if occurrence_ms != self.minute.load(Ordering::SeqCst) {
                    return Err(koru_delegate_api::Failure::new(
                        "routine_test_minute_changed",
                        "fixture minute",
                        true,
                    ));
                }
                self.tests
                    .lock()
                    .unwrap()
                    .push((occurrence_ms, expected_revision));
                self.lost()?;
                Ok(BotReply::RoutineTestAccepted {
                    slot: RoutineSlot {
                        owner: route.owner,
                        bot: route.bot,
                        schedule: route.schedule,
                        occurrence_ms,
                    },
                    definition: RoutineDefinitionIdentity {
                        revision: expected_revision,
                        digest: RoutineDigest32::from_bytes([9; 32]),
                        owner_handoff: None,
                    },
                })
            }
            DelegateCommand::SynchronizeRoutinePolicy { barrier } => {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("{name}@{}", barrier.grant_revision));
                if self
                    .policy_failures
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                        left.checked_sub(1)
                    })
                    .is_ok()
                {
                    return Err(koru_delegate_api::Failure::unavailable());
                }
                // One request per grant revision; a lower revision is refused.
                let mut installed = self.routine_policies.lock().unwrap();
                if installed.last().is_some_and(|last| {
                    barrier.grant_revision < last.grant_revision
                        || (barrier.grant_revision == last.grant_revision && *last != barrier)
                }) {
                    return Err(refused("bot_conflict"));
                }
                installed.push(barrier.clone());
                Ok(BotReply::RoutinePolicyBarrier {
                    barrier,
                    state: BotPolicyBarrierState::Effective,
                })
            }
            DelegateCommand::UnsettledWork { owner, bot, .. } => {
                self.calls.lock().unwrap().push(name);
                let (runs, more) = self.unsettled.lock().unwrap().clone();
                Ok(BotReply::UnsettledWork {
                    work: BotUnsettledWork {
                        admissions: runs
                            .into_iter()
                            .map(|run| BotUnsettledAdmission {
                                admission: BotAdmissionView {
                                    key: BotSubmissionKey {
                                        owner,
                                        bot,
                                        request: BotRequestId::mint(),
                                    },
                                    run,
                                    state: BotAdmissionState::Executing,
                                    queue_sequence: 1,
                                    fence: None,
                                },
                                held: false,
                            })
                            .collect(),
                        more,
                    },
                })
            }
            DelegateCommand::Barrier { barrier } => {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("{name}:paused={}", barrier.paused));
                // An epoch never goes back.
                let mut held = self.admission.lock().unwrap();
                if held.as_ref().is_some_and(|old| barrier.epoch < old.epoch) {
                    return Err(refused("bot_stale"));
                }
                *held = Some(barrier.clone());
                Ok(BotReply::Barrier { barrier })
            }
            DelegateCommand::SynchronizePolicy { barrier } => {
                self.calls.lock().unwrap().push(name);
                Ok(BotReply::PolicyBarrier {
                    receipt: BotPolicyBarrierReceipt {
                        barrier,
                        state: BotPolicyBarrierState::Effective,
                    },
                })
            }
            DelegateCommand::ReadAdmissionBarrier { owner, bot } => {
                Ok(BotReply::AdmissionBarrier {
                    owner,
                    bot,
                    admission: match self.admission.lock().unwrap().clone() {
                        Some(barrier) => BotBootAdmission::Present { barrier },
                        None => BotBootAdmission::Missing {},
                    },
                })
            }
            DelegateCommand::Hello { .. } => {
                self.calls.lock().unwrap().push(name);
                Ok(BotReply::Hello {
                    hello: BotHello {
                        boot: self.boot,
                        entries: vec![],
                        next: None,
                    },
                })
            }
            DelegateCommand::ReadRoutineClaim { slot } => Ok(BotReply::RoutineClaim {
                observation: RoutineClaimObservation::Absent {
                    slot,
                    observed_at_ms: slot.occurrence_ms + 1,
                },
            }),
            // Nothing else belongs to these tests: the worker's unrelated
            // background passes back off on an owner that does not answer.
            _ => {
                self.calls.lock().unwrap().push(format!("other:{name}"));
                Err(koru_delegate_api::Failure::unavailable())
            }
        }
    }
}
impl BotDelegatePort for Coordinator {
    fn execute(&self, command: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        self.serve(command)
    }
    fn execute_within(
        &self,
        command: DelegateCommand,
        within: Duration,
    ) -> Result<BotReply, koru_delegate_api::Failure> {
        assert!(!within.is_zero(), "an owner call with no time left");
        self.serve(command)
    }
}

/// The real adapter, with the two things a test changes about a caller.
struct Authority {
    adapter: Arc<BotAuthorityAdapter>,
    wrong_owner: AtomicBool,
    /// A page the person may see but not edit.
    forbidden: Mutex<Option<BotPage>>,
    /// Holds the next live Schedule read.
    read_gate: Mutex<Option<Gate>>,
}
impl BotAuthority for Authority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        let mut owner = self.adapter.owner()?;
        if self.wrong_owner.load(Ordering::SeqCst) {
            owner.device = koru_delegate_core::DeviceId::new();
        }
        Ok(owner)
    }
    fn workspace(&self, workspace: WorkspaceId) -> Result<ProfileId, BotError> {
        self.adapter.workspace(workspace)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.adapter.page(page)
    }
    fn inspect(
        &self,
        workspaces: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<BotAuthoritySnapshot, BotError> {
        let mut snapshot = self.adapter.inspect(workspaces, pages)?;
        snapshot.owner = self.owner()?;
        if let Some(page) = self.forbidden.lock().unwrap().clone()
            && snapshot.pages.contains_key(&page)
        {
            snapshot.pages.insert(page, Err(BotError::Forbidden));
        }
        Ok(snapshot)
    }
    fn read_routine_overview(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        identity: &BotRoutineCreationIdentity,
        cancel: &Cancellation,
    ) -> Result<RoutineOverviewObservation, BotError> {
        self.adapter
            .read_routine_overview(owner, reader, identity, cancel)
    }
    fn read_routine_detail(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        identity: &BotRoutineCreationIdentity,
        at: i64,
        cancel: &Cancellation,
    ) -> Result<RoutineDetailObservation, BotError> {
        let held = self.read_gate.lock().unwrap().take();
        if let Some(gate) = held {
            gate.entered.send(()).unwrap();
            gate.release.recv_timeout(Duration::from_secs(20)).unwrap();
        }
        self.adapter
            .read_routine_detail(owner, reader, identity, at, cancel)
    }
    fn read_routine_detail_page(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        identity: &BotRoutineCreationIdentity,
        at: i64,
        before: Option<i64>,
        limit: u8,
        cancel: &Cancellation,
    ) -> Result<(RoutineDetailObservation, RoutineFiringsObservation), BotError> {
        self.adapter
            .read_routine_detail_page(owner, reader, identity, at, before, limit, cancel)
    }
}

fn context() -> RequestContext {
    RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "routine-control-fixture".into(),
            scopes: vec![AiScope::ManageBots],
        },
        trace_id: "routine-control".into(),
        deadline_millis: None,
    }
}

struct Options {
    bot_paused: bool,
    zone: Option<&'static str>,
    lifecycle_unit: Option<Duration>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            bot_paused: false,
            zone: None,
            lifecycle_unit: Some(Duration::from_millis(5)),
        }
    }
}

struct H {
    f: Fixture,
    owner: Arc<Coordinator>,
    authority: Arc<Authority>,
    clock: Arc<AtomicI64>,
    options: Options,
    worker: Option<WorkerHandle>,
    returned: Receiver<Repository>,
}
impl H {
    /// An active Bot with one bound Routine, paused as creation leaves it,
    /// at definition revision 1.
    fn new() -> Self {
        Self::with(Options::default())
    }
    fn with(options: Options) -> Self {
        let (mut f, mut repository) = Fixture::with_pause(false);
        if options.bot_paused {
            // The Bot's own pause, as its row holds it.
            repository
                .fixture_execute_batch("UPDATE bots SET paused=1;")
                .unwrap();
            f.bot.paused = true;
        }
        let PreparedRoutineDocument::Confirmed(proof) = f.prepare().unwrap() else {
            panic!("actual Store seed")
        };
        repository
            .transition_bot_routine_creation(
                &f.creation.identity,
                f.creation.observed(),
                BotRoutineCreationTransition::Confirmed(proof),
                3,
            )
            .unwrap();
        Self::over(f, repository, options)
    }
    fn over(f: Fixture, repository: Repository, options: Options) -> Self {
        let owner = Coordinator::new(&f);
        let authority = Arc::new(Authority {
            adapter: f.adapter.clone(),
            wrong_owner: AtomicBool::new(false),
            forbidden: Default::default(),
            read_gate: Default::default(),
        });
        let clock = Arc::new(AtomicI64::new(NOW));
        let (worker, returned) = spawn(repository, &owner, &authority, &clock, &options);
        let h = Self {
            f,
            owner,
            authority,
            clock,
            options,
            worker: Some(worker),
            returned,
        };
        h.ready(0);
        h
    }
    fn probes(&self) -> usize {
        self.owner.named("hello").len()
    }
    /// The worker's first owner probe ends the owner epoch every Routine
    /// read and control started before it belongs to. Wait it out, as a
    /// person's first read would simply be retried. `before` is how many
    /// probes the owner had answered when this worker was started.
    fn ready(&self, before: usize) {
        until("the owner probe completed", || self.probes() > before);
        until("a Routine read is served in the settled epoch", || {
            !matches!(
                self.send(BotCommand::ReadRoutines {
                    bot_id: self.bot(),
                    after_schedule: None,
                    limit: 1,
                }),
                Err(error) if error.code == "ai_unavailable"
            )
        });
    }
    fn client(&self) -> AiWorkerClient {
        self.worker.as_ref().unwrap().client()
    }
    fn bot(&self) -> BotId {
        self.f.bot.id
    }
    fn schedule(&self) -> ScheduleId {
        self.f.creation.identity.schedule
    }
    fn send(&self, command: BotCommand) -> ApiResult<BotResponse> {
        self.client().bots(context(), command)
    }
    fn control(&self, command: BotCommand) -> ApiResult<BotRoutineControl> {
        self.send(command).map(|response| match response {
            BotResponse::RoutineControl { control } => control,
            _ => panic!("routine_control"),
        })
    }
    fn lifecycle(&self, action: BotRoutineLifecycleAction, revision: u32) -> BotCommand {
        BotCommand::SetRoutineLifecycle {
            bot_id: self.bot(),
            schedule_id: self.schedule(),
            expected_definition_revision: revision,
            action,
        }
    }
    fn enable(&self, revision: u32) -> ApiResult<BotRoutineControl> {
        self.control(self.lifecycle(BotRoutineLifecycleAction::Enable, revision))
    }
    fn pause(&self, revision: u32) -> ApiResult<BotRoutineControl> {
        self.control(self.lifecycle(BotRoutineLifecycleAction::Pause, revision))
    }
    fn edit_command(&self, revision: u32, from: &str, to: &str, recurrence: &str) -> BotCommand {
        BotCommand::EditRoutine {
            bot_id: self.bot(),
            schedule_id: self.schedule(),
            expected_definition_revision: revision,
            expected_name: RoutineScheduleName::new(from).unwrap(),
            name: RoutineScheduleName::new(to).unwrap(),
            recurrence: serde_json::from_str(recurrence).unwrap(),
        }
    }
    fn test(&self, revision: u32) -> ApiResult<BotRoutineTest> {
        self.send(BotCommand::TestRoutine {
            bot_id: self.bot(),
            schedule_id: self.schedule(),
            expected_definition_revision: revision,
        })
        .map(|response| match response {
            BotResponse::RoutineTest { test } => test,
            _ => panic!("routine_test"),
        })
    }
    fn access_command(
        &self,
        request: BotRequestId,
        expected: u64,
        access: BotAccess,
        pages: Vec<BotPage>,
    ) -> BotCommand {
        BotCommand::UpdateRoutineAccess {
            request_id: request,
            bot_id: self.bot(),
            schedule_id: self.schedule(),
            expected_grant_revision: expected,
            access,
            selected_pages: pages,
        }
    }
    fn access(
        &self,
        request: BotRequestId,
        expected: u64,
        access: BotAccess,
    ) -> ApiResult<BotRoutineAccess> {
        self.send(self.access_command(request, expected, access, vec![]))
            .map(|response| match response {
                BotResponse::RoutineAccess { access } => access,
                _ => panic!("routine_access"),
            })
    }
    fn detail(&self) -> BotRoutineDetail {
        match self
            .send(BotCommand::ReadRoutineDetail {
                bot_id: self.bot(),
                schedule_id: self.schedule(),
                before_occurrence_ms: None,
                history_limit: 16,
            })
            .unwrap()
        {
            BotResponse::RoutineDetail { reply } => reply.detail,
            _ => panic!("routine_detail"),
        }
    }
    fn finish(&mut self) -> Repository {
        let worker = self.worker.take().unwrap();
        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
        self.returned.recv_timeout(Duration::from_secs(5)).unwrap()
    }
    fn restart(&mut self, repository: Repository) {
        assert!(self.worker.is_none());
        let before = self.probes();
        let (worker, returned) = spawn(
            repository,
            &self.owner,
            &self.authority,
            &self.clock,
            &self.options,
        );
        self.worker = Some(worker);
        self.returned = returned;
        self.ready(before);
    }
}
impl Drop for H {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.shutdown();
            let _ = worker.join();
        }
    }
}
fn spawn(
    repository: Repository,
    owner: &Arc<Coordinator>,
    authority: &Arc<Authority>,
    clock: &Arc<AtomicI64>,
    options: &Options,
) -> (WorkerHandle, Receiver<Repository>) {
    let (send, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-routine-control-fixture"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority.clone())
    .with_bot_delegate(owner.clone());
    if let Some(zone) = options.zone {
        config = config.with_owner_zone(Arc::new(move || Some(zone.to_owned())));
    }
    config.test_repository = Some(repository);
    config.test_repository_return = Some(send);
    config.test_lifecycle_unit = options.lifecycle_unit;
    let clock = clock.clone();
    config.test_routine_clock = Some(Arc::new(move || clock.load(Ordering::SeqCst)));
    (AiWorker::new(config).spawn().unwrap(), returned)
}
fn until(what: &str, mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < end, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(15));
    }
}
fn code<T: std::fmt::Debug>(result: ApiResult<T>) -> String {
    result.unwrap_err().code
}
fn reasons(error: &Failure) -> Vec<String> {
    error.details["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reason| reason.as_str().unwrap().to_owned())
        .collect()
}
const ZONED: &str = r#"{"type":"zoned","cron":"30 7 * * *","zone":"Europe/Zurich"}"#;
const FLOATING: &str = r#"{"type":"floating","cron":"0 9 * * *"}"#;

#[test]
fn routine_control_slot_holds_one_operation_and_a_test_until_its_minute_ends() {
    let clock = Arc::new(AtomicI64::new(NOW));
    let mut controls = RoutineControls::new(None);
    let read = clock.clone();
    controls.set_clock(Arc::new(move || read.load(Ordering::SeqCst)));
    let (a, b) = (ScheduleId::mint(), ScheduleId::mint());
    let minute = NOW - NOW % MINUTE;
    let first = controls.acquire(a, None).unwrap();
    assert_eq!(code(controls.acquire(a, None)), codes::CONTROL_PENDING);
    // Another Routine has its own slot.
    let other = controls.acquire(b, None).unwrap();
    controls.release(a, first, false);
    controls.release(b, other, false);
    // A stale release cannot free a newer control.
    let second = controls.acquire(a, None).unwrap();
    controls.release(a, first, false);
    assert_eq!(code(controls.acquire(a, None)), codes::CONTROL_PENDING);
    controls.release(a, second, false);

    // A Test is expected for exactly its Routine, minute and revision.
    let test = TestSlot {
        minute,
        revision: 3,
    };
    let serial = controls.acquire(a, Some(test)).unwrap();
    assert!(controls.expects_test(a, minute, 3));
    assert!(!controls.expects_test(a, minute, 4));
    assert!(!controls.expects_test(a, minute + MINUTE, 3));
    assert!(!controls.expects_test(b, minute, 3));
    // Accepted: its minute stays outstanding, and holds the slot.
    controls.release(a, serial, true);
    assert!(controls.expects_test(a, minute, 3));
    assert_eq!(code(controls.acquire(a, None)), codes::CONTROL_PENDING);
    assert_eq!(
        code(controls.acquire(
            a,
            Some(TestSlot {
                minute,
                revision: 4
            })
        )),
        codes::CONTROL_PENDING
    );
    // The same Test again is that Test (a lost reply), not a second one.
    let replay = controls.acquire(a, Some(test)).unwrap();
    controls.release(a, replay, true);
    // Its claim was observed: free.
    controls.test_claimed(a, minute + MINUTE);
    assert!(controls.expects_test(a, minute, 3));
    controls.test_claimed(a, minute);
    assert!(!controls.expects_test(a, minute, 3));
    // Or its minute ended: no expectation is left and the slot is free.
    let serial = controls.acquire(a, Some(test)).unwrap();
    controls.release(a, serial, true);
    clock.store(minute + MINUTE, Ordering::SeqCst);
    assert!(!controls.expects_test(a, minute, 3));
    let free = controls.acquire(a, None).unwrap();
    // A refused Test leaves nothing expected.
    controls.release(a, free, false);
    let serial = controls.acquire(
        a,
        Some(TestSlot {
            minute: minute + MINUTE,
            revision: 3,
        }),
    );
    controls.release(a, serial.unwrap(), false);
    assert!(!controls.expects_test(a, minute + MINUTE, 3));
}

#[test]
fn routine_control_owner_refusals_map_to_their_public_codes() {
    let delegate = |code: &str| koru_delegate_api::Failure::new(code, "x", false);
    for (from, to, retryable) in [
        ("schedule_revision_conflict", "revision_conflict", false),
        ("schedule_name_conflict", "revision_conflict", false),
        ("schedule_state_conflict", codes::STATE_CONFLICT, false),
        ("schedule_not_found", codes::NOT_LIVE, false),
        ("schedule_retired", codes::NOT_LIVE, false),
        ("schedule_not_owner", codes::OWNER_ELSEWHERE, false),
        ("schedule_handoff_pending", codes::OWNER_ELSEWHERE, false),
        ("invalid_schedule", "invalid_request", false),
        ("routine_test_requires_paused", codes::MUST_BE_PAUSED, false),
        ("routine_test_too_soon", codes::TEST_TOO_SOON, true),
        ("routine_test_minute_taken", codes::TEST_MINUTE_TAKEN, false),
        ("routine_test_busy", codes::TEST_BUSY, true),
        ("routine_route_mismatch", codes::UNAVAILABLE, false),
        ("bot_authority_unavailable", codes::UNAVAILABLE, false),
    ] {
        let mapped = owner_refusal(&delegate(from), true);
        assert_eq!(
            (mapped.code.as_str(), mapped.retryable),
            (to, retryable),
            "{from}"
        );
    }
    let paused = owner_refusal(&delegate("routine_bot_paused"), true);
    assert_eq!(paused.code, codes::NOT_READY);
    assert_eq!(reasons(&paused), ["bot_paused"]);
    let unavailable = koru_delegate_api::Failure::new("schedule_unavailable", "x", true);
    assert_eq!(owner_refusal(&unavailable, true).code, codes::UNAVAILABLE);
    assert!(owner_refusal(&unavailable, true).retryable);
    // No answer to a call that may have written is unknown, never "not
    // applied"; to a read or an idempotent install it is just unavailable.
    let silent = koru_delegate_api::Failure::unavailable();
    assert_eq!(owner_refusal(&silent, true).code, codes::CONTROL_UNKNOWN);
    assert!(owner_refusal(&silent, true).retryable);
    assert_eq!(owner_refusal(&silent, false).code, codes::UNAVAILABLE);
    assert!(owner_refusal(&silent, false).retryable);
}

#[test]
fn routine_control_enable_pause_and_edit_commit_through_the_coordinator() {
    let h = H::new();
    assert_eq!(h.owner.live().revision, 1);
    assert!(h.owner.live().paused);

    let enabled = h.enable(1).unwrap();
    assert_eq!(enabled.schedule_id, h.schedule());
    assert_eq!(enabled.action, BotRoutineControlAction::Enable);
    assert_eq!(enabled.outcome, RoutineControlOutcome::Applied);
    assert_eq!(enabled.definition_revision, 2);
    assert_eq!(
        enabled.state,
        RoutineScheduleState::Active {
            owner: h.f.owner.device
        }
    );
    // A floating rule with no zone source reads owner local, as before.
    assert_eq!(enabled.next, RoutineNextObservation::OwnerLocal {});
    assert!(!h.owner.live().paused);
    assert_eq!(h.owner.live().revision, 2);

    let paused = h.pause(2).unwrap();
    assert_eq!(paused.action, BotRoutineControlAction::Pause);
    assert_eq!(paused.outcome, RoutineControlOutcome::Applied);
    assert_eq!(paused.definition_revision, 3);
    assert!(matches!(
        paused.state,
        RoutineScheduleState::Paused {
            reason: koru_delegate_core::SchedulePauseReason::Manual,
            ..
        }
    ));
    assert_eq!(paused.next, RoutineNextObservation::Inactive {});
    assert!(h.owner.live().paused);

    // Edit replaces name and timing in one revision, while paused.
    let edited = h
        .control(h.edit_command(3, "Routine", "Evening", ZONED))
        .unwrap();
    assert_eq!(edited.action, BotRoutineControlAction::Edit);
    assert_eq!(edited.outcome, RoutineControlOutcome::Applied);
    assert_eq!(edited.definition_revision, 4);
    assert_eq!(edited.next, RoutineNextObservation::Inactive {});
    let live = h.owner.live();
    assert_eq!(
        (live.revision, live.paused, live.name.as_str()),
        (4, true, "Evening")
    );

    // And while on (D12): the new time applies at once, with its next time.
    h.enable(4).unwrap();
    let edited = h
        .control(h.edit_command(5, "Evening", "Morning", ZONED))
        .unwrap();
    assert_eq!(edited.definition_revision, 6);
    let RoutineNextObservation::Exact { at_ms } = edited.next else {
        panic!("a fixed zone has an exact next time")
    };
    assert!(at_ms > NOW);
    assert!(matches!(edited.state, RoutineScheduleState::Active { .. }));
    let live = h.owner.live();
    assert_eq!(
        (live.revision, live.paused, live.name.as_str()),
        (6, false, "Morning")
    );
    assert_eq!(
        h.owner
            .named("e")
            .into_iter()
            .chain(h.owner.named("p"))
            .collect::<Vec<_>>(),
        ["enable@1", "edit@3", "enable@4", "edit@5", "pause@2"]
    );
    // The detail a pane reads next agrees with what the control answered.
    let RoutineDetailLive::Current {
        definition_revision,
        name,
        ..
    } = h.detail().live
    else {
        panic!("live")
    };
    assert_eq!((definition_revision, name.as_str()), (6, "Morning"));
}

#[test]
fn routine_control_lost_reply_replay_is_already_applied_and_a_later_revision_conflicts() {
    let h = H::new();
    // Enable: the commit lands, the reply is lost.
    h.owner.lose_replies.store(1, Ordering::SeqCst);
    let unknown = h.enable(1).unwrap_err();
    assert_eq!(unknown.code, codes::CONTROL_UNKNOWN);
    assert!(unknown.retryable);
    assert!(!h.owner.live().paused, "the commit did land");
    let replay = h.enable(1).unwrap();
    assert_eq!(replay.outcome, RoutineControlOutcome::AlreadyApplied);
    assert_eq!(replay.definition_revision, 2);
    assert!(matches!(replay.state, RoutineScheduleState::Active { .. }));

    // Pause.
    h.owner.lose_replies.store(1, Ordering::SeqCst);
    assert_eq!(code(h.pause(2)), codes::CONTROL_UNKNOWN);
    let replay = h.pause(2).unwrap();
    assert_eq!(replay.outcome, RoutineControlOutcome::AlreadyApplied);
    assert_eq!(replay.definition_revision, 3);

    // Edit.
    h.owner.lose_replies.store(1, Ordering::SeqCst);
    assert_eq!(
        code(h.control(h.edit_command(3, "Routine", "Evening", ZONED))),
        codes::CONTROL_UNKNOWN
    );
    let replay = h
        .control(h.edit_command(3, "Routine", "Evening", ZONED))
        .unwrap();
    assert_eq!(replay.outcome, RoutineControlOutcome::AlreadyApplied);
    assert_eq!(replay.definition_revision, 4);
    assert_eq!(h.owner.live().revision, 4, "no replay wrote a revision");

    // Once any later revision exists, the same retry is a conflict, and
    // nothing is ever sent to undo it.
    h.enable(4).unwrap();
    assert_eq!(code(h.pause(2)), "revision_conflict");
    assert_eq!(
        code(h.control(h.edit_command(3, "Routine", "Evening", ZONED))),
        "revision_conflict"
    );
    assert_eq!(h.owner.live().revision, 5);
    assert!(!h.owner.live().paused);
}

#[test]
fn routine_control_refusals_carry_their_codes_and_write_nothing() {
    let h = H::new();
    // A stale revision, and the state the Routine is already in.
    assert_eq!(code(h.enable(7)), "revision_conflict");
    assert_eq!(code(h.pause(1)), codes::STATE_CONFLICT);
    h.enable(1).unwrap();
    assert_eq!(code(h.enable(2)), codes::STATE_CONFLICT);
    // A changed name is a conflict, not an overwrite.
    assert_eq!(
        code(h.control(h.edit_command(2, "Someone else's", "Mine", FLOATING))),
        "revision_conflict"
    );
    // Revision zero names nothing.
    assert_eq!(code(h.enable(0)), "invalid_request");
    // A Routine that is not this Bot's.
    let mut unknown = h.lifecycle(BotRoutineLifecycleAction::Pause, 2);
    if let BotCommand::SetRoutineLifecycle { schedule_id, .. } = &mut unknown {
        *schedule_id = ScheduleId::mint();
    }
    assert_eq!(code(h.send(unknown)), "not_found");
    let mut missing = h.lifecycle(BotRoutineLifecycleAction::Pause, 2);
    if let BotCommand::SetRoutineLifecycle { bot_id, .. } = &mut missing {
        *bot_id = BotId::mint();
    }
    assert_eq!(code(h.send(missing)), "not_found");
    assert_eq!(h.owner.live().revision, 2, "no refusal wrote");

    // The coordinator's own answers.
    for (answer, expected, retryable) in [
        ("schedule_not_found", codes::NOT_LIVE, false),
        ("schedule_retired", codes::NOT_LIVE, false),
        ("schedule_not_owner", codes::OWNER_ELSEWHERE, false),
        ("schedule_handoff_pending", codes::OWNER_ELSEWHERE, false),
        ("schedule_unavailable", codes::UNAVAILABLE, true),
    ] {
        h.owner.script.lock().unwrap().push_back(answer);
        let refused = h.pause(2).unwrap_err();
        assert_eq!(
            (refused.code.as_str(), refused.retryable),
            (expected, retryable)
        );
    }
    // The Bot's barrier in the ledger is paused: reported as a reason.
    h.pause(2).unwrap();
    h.owner
        .script
        .lock()
        .unwrap()
        .push_back("routine_bot_paused");
    let refused = h.enable(3).unwrap_err();
    assert_eq!(refused.code, codes::NOT_READY);
    assert_eq!(reasons(&refused), ["bot_paused"]);
    assert!(h.owner.live().paused);
    assert_eq!(h.owner.live().revision, 3);
}

#[test]
fn routine_control_readiness_refuses_enable_and_allows_pause() {
    // A paused Bot: Enable and Test are not ready; Pause only reduces work.
    let h = H::with(Options {
        bot_paused: true,
        ..Options::default()
    });
    let refused = h.enable(1).unwrap_err();
    assert_eq!(
        (refused.code.as_str(), refused.retryable),
        (codes::NOT_READY, false)
    );
    assert_eq!(reasons(&refused), ["bot_paused"]);
    let refused = h.test(1).unwrap_err();
    assert_eq!(reasons(&refused), ["bot_paused"]);
    assert!(h.owner.named("enable").is_empty() && h.owner.named("test").is_empty());
    // Pause reaches the coordinator even so (it is already paused there).
    assert_eq!(code(h.pause(1)), codes::STATE_CONFLICT);
    assert_eq!(h.owner.named("pause"), ["pause@1"]);

    drop(h);

    // A creation that is not bound (its Schedule was never confirmed): a
    // reason for Enable, its own code for the controls that are not
    // readiness checks.
    let (f, repository) = Fixture::with_pause(false);
    let h = H::over(f, repository, Options::default());
    let refused = h.enable(1).unwrap_err();
    assert_eq!(refused.code, codes::NOT_READY);
    assert_eq!(reasons(&refused), ["creation_not_bound"]);
    assert_eq!(code(h.pause(1)), codes::NOT_BOUND);
    assert_eq!(
        code(h.control(h.edit_command(1, "Routine", "Other", FLOATING))),
        codes::NOT_BOUND
    );
    assert_eq!(
        code(h.access(BotRequestId::mint(), 1, BotAccess::Workspace)),
        codes::NOT_BOUND
    );
    assert!(
        h.owner.named("pause").is_empty(),
        "nothing reached the owner"
    );
}

#[test]
fn routine_control_wrong_scope_and_foreign_owner_are_refused_before_any_owner_call() {
    let h = H::new();
    let commands = [
        h.lifecycle(BotRoutineLifecycleAction::Enable, 1),
        h.lifecycle(BotRoutineLifecycleAction::Pause, 1),
        h.edit_command(1, "Routine", "Other", FLOATING),
        BotCommand::TestRoutine {
            bot_id: h.bot(),
            schedule_id: h.schedule(),
            expected_definition_revision: 1,
        },
        h.access_command(BotRequestId::mint(), 1, BotAccess::Workspace, vec![]),
    ];
    // A caller without the Bot scope.
    for command in commands.clone() {
        let mut denied = context();
        denied.principal.scopes = vec![AiScope::Chat];
        assert_eq!(code(h.client().bots(denied, command)), "forbidden");
    }
    // The vouched owner is another device: the owner is never a request
    // field, so nothing in the command can change this.
    h.authority.wrong_owner.store(true, Ordering::SeqCst);
    for command in commands.clone() {
        assert_eq!(code(h.send(command)), "wrong_owner");
    }
    h.authority.wrong_owner.store(false, Ordering::SeqCst);
    // The home now belongs to another Person's Profile.
    let (home, other) = (h.f.bot.home_workspace, h.f.other);
    h.f.memory
        .with_store(move |store| store.move_workspace(home, other).unwrap())
        .unwrap();
    for command in commands {
        assert_eq!(code(h.send(command)), "wrong_owner");
    }
    let calls = h.owner.calls();
    assert!(
        !calls.iter().any(|call| {
            call.starts_with("enable")
                || call.starts_with("pause")
                || call.starts_with("edit")
                || call.starts_with("test")
                || call.starts_with("synchronize_routine_policy")
        }),
        "{calls:?}"
    );
    assert_eq!(h.owner.live().revision, 1);
}

#[test]
fn routine_control_second_control_of_the_same_routine_is_pending() {
    let h = H::new();
    let (held, entered, mut release) = gate();
    *h.owner.gate.lock().unwrap() = Some(held);
    let client = h.client();
    let first = h.lifecycle(BotRoutineLifecycleAction::Enable, 1);
    let running = std::thread::spawn(move || client.bots(context(), first));
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    // The first is at the coordinator. A second one is not queued behind it.
    let pending = h.pause(1).unwrap_err();
    assert_eq!(
        (pending.code.as_str(), pending.retryable),
        (codes::CONTROL_PENDING, true)
    );
    let pending = h
        .access(BotRequestId::mint(), 1, BotAccess::Workspace)
        .unwrap_err();
    assert_eq!(pending.code, codes::CONTROL_PENDING);
    release.open();
    assert!(matches!(
        running.join().unwrap().unwrap(),
        BotResponse::RoutineControl { .. }
    ));
    // Resolved: the next control is admitted.
    h.pause(2).unwrap();
    assert_eq!(h.owner.named("pause"), ["pause@2"]);
}

#[test]
fn routine_control_test_is_accepted_for_the_current_minute_and_never_turns_the_routine_on() {
    let h = H::new();
    let minute = NOW - NOW % MINUTE;
    let accepted = h.test(1).unwrap();
    assert_eq!(
        accepted,
        BotRoutineTest {
            schedule_id: h.schedule(),
            occurrence_ms: minute
        }
    );
    assert_eq!(*h.owner.tests.lock().unwrap(), [(minute, 1)]);
    // The Routine stays paused at the same revision: a Test is not Enable.
    let live = h.owner.live();
    assert_eq!((live.revision, live.paused), (1, true));
    // While its minute is outstanding it holds the Routine's one slot, and
    // the same Test again is that Test.
    assert_eq!(code(h.enable(1)), codes::CONTROL_PENDING);
    assert_eq!(h.test(1).unwrap().occurrence_ms, minute);
    assert_eq!(h.owner.tests.lock().unwrap().len(), 2);
    // The next minute is a new Test, and the slot is free again.
    h.clock.store(NOW + MINUTE, Ordering::SeqCst);
    h.owner.minute.store(minute + MINUTE, Ordering::SeqCst);
    assert_eq!(h.test(1).unwrap().occurrence_ms, minute + MINUTE);
}

#[test]
fn routine_control_test_refusals_and_its_one_minute_retry() {
    let h = H::new();
    let minute = NOW - NOW % MINUTE;
    // Under ten seconds left: a wait the pane knows the length of.
    h.clock.store(minute + 50_001, Ordering::SeqCst);
    let ending = h.test(1).unwrap_err();
    assert_eq!(
        (ending.code.as_str(), ending.retryable),
        (codes::TEST_MINUTE_ENDING, true)
    );
    assert!(h.owner.named("test").is_empty());
    h.clock.store(NOW, Ordering::SeqCst);

    for (answer, expected, retryable) in [
        ("routine_test_too_soon", codes::TEST_TOO_SOON, true),
        ("routine_test_minute_taken", codes::TEST_MINUTE_TAKEN, false),
        ("routine_test_busy", codes::TEST_BUSY, true),
    ] {
        h.owner.script.lock().unwrap().push_back(answer);
        let refused = h.test(1).unwrap_err();
        assert_eq!(
            (refused.code.as_str(), refused.retryable),
            (expected, retryable)
        );
    }
    // A refused Test expects nothing and holds nothing.
    assert_eq!(code(h.test(9)), "revision_conflict");

    // The owner's clock is already in the next minute: retried there, once.
    h.owner.minute.store(minute + MINUTE, Ordering::SeqCst);
    h.owner
        .script
        .lock()
        .unwrap()
        .push_back("routine_test_minute_changed");
    h.clock.store(NOW + MINUTE, Ordering::SeqCst);
    assert_eq!(h.test(1).unwrap().occurrence_ms, minute + MINUTE);
    // Twice in a row is the wait, never a third minute chosen by itself.
    h.clock.store(NOW + 2 * MINUTE, Ordering::SeqCst);
    h.owner.minute.store(minute + 2 * MINUTE, Ordering::SeqCst);
    for _ in 0..2 {
        h.owner
            .script
            .lock()
            .unwrap()
            .push_back("routine_test_minute_changed");
    }
    assert_eq!(code(h.test(1)), codes::TEST_MINUTE_ENDING);

    // A Routine that is on must be paused first: refused before the owner.
    h.clock.store(NOW + 3 * MINUTE, Ordering::SeqCst);
    h.owner.minute.store(minute + 3 * MINUTE, Ordering::SeqCst);
    h.enable(1).unwrap();
    let before = h.owner.named("test").len();
    assert_eq!(code(h.test(2)), codes::MUST_BE_PAUSED);
    assert_eq!(h.owner.named("test").len(), before);
}

#[test]
fn routine_control_test_lost_reply_is_unknown_and_the_same_test_is_accepted_again() {
    let h = H::new();
    let minute = NOW - NOW % MINUTE;
    h.owner.lose_replies.store(1, Ordering::SeqCst);
    let unknown = h.test(1).unwrap_err();
    assert_eq!(
        (unknown.code.as_str(), unknown.retryable),
        (codes::CONTROL_UNKNOWN, true)
    );
    // The owner did accept it. The expectation was kept, so another control
    // is pending, and the same Test in the same minute is that Test.
    assert_eq!(*h.owner.tests.lock().unwrap(), [(minute, 1)]);
    assert_eq!(code(h.pause(1)), codes::CONTROL_PENDING);
    assert_eq!(h.test(1).unwrap().occurrence_ms, minute);
    assert_eq!(
        h.owner.named("test"),
        vec![format!("test@1/{}", minute / MINUTE); 2]
    );
}

#[test]
fn routine_control_list_row_and_detail_show_next_time_zone_and_history() {
    let h = H::with(Options {
        zone: Some("Europe/Zurich"),
        ..Options::default()
    });
    let row = |h: &H| match h
        .send(BotCommand::ReadRoutines {
            bot_id: h.bot(),
            after_schedule: None,
            limit: 16,
        })
        .unwrap()
    {
        BotResponse::Routines { mut page } => page.items.remove(0),
        _ => panic!("routines"),
    };
    // Paused: nothing is next, and the zone is this computer's.
    let BotRoutineLiveObservation::Current { next, .. } = row(&h).live else {
        panic!("live row")
    };
    assert_eq!(next, RoutineNextObservation::Inactive {});
    let detail = h.detail();
    let RoutineDetailLive::Current { owner_zone, .. } = &detail.live else {
        panic!("live detail")
    };
    assert_eq!(
        *owner_zone,
        RoutineOwnerZoneObservation::ThisComputer {
            iana: "Europe/Zurich".into()
        }
    );
    assert_eq!(
        detail.occurrences,
        RoutineOccurrenceHistoryObservation::Available {
            items: vec![],
            next_before_occurrence_ms: None
        }
    );

    // On: the row and the detail agree on an exact next time, in that zone.
    h.enable(1).unwrap();
    let BotRoutineLiveObservation::Current { next: row_next, .. } = row(&h).live else {
        panic!("live row")
    };
    let RoutineNextObservation::Exact { at_ms } = row_next else {
        panic!("this computer owns the floating rule: {row_next:?}")
    };
    // A read observes at its own time: the next 09:00 in that zone.
    let read_at = Timestamp::now().millis();
    assert!(at_ms > read_at - MINUTE && at_ms <= read_at + 24 * 60 * MINUTE);
    let RoutineDetailLive::Current { next, .. } = h.detail().live else {
        panic!("live detail")
    };
    assert_eq!(next, row_next);

    // Missed work and a busy skip are history lines. Nothing is caught up.
    let minute = NOW - NOW % MINUTE;
    h.owner.fire(minute - 3 * MINUTE, "fired");
    h.owner.fire(minute - MINUTE, "skipped_missed");
    h.owner.fire(minute - 2 * MINUTE, "skipped_missed");
    let RoutineOccurrenceHistoryObservation::Available {
        items,
        next_before_occurrence_ms,
    } = h.detail().occurrences
    else {
        panic!("history")
    };
    assert_eq!(next_before_occurrence_ms, None);
    assert_eq!(
        items
            .iter()
            .map(|line| (
                line.occurrence_ms,
                line.outcome,
                line.test,
                line.conversation.is_some()
            ))
            .collect::<Vec<_>>(),
        [
            (
                minute - MINUTE,
                RoutineOccurrenceOutcome::Missed,
                false,
                false
            ),
            (
                minute - 2 * MINUTE,
                RoutineOccurrenceOutcome::Missed,
                false,
                false
            ),
            // It ran on a computer that holds no conversation of it here.
            (
                minute - 3 * MINUTE,
                RoutineOccurrenceOutcome::Ran,
                false,
                false
            ),
        ]
    );
    assert_eq!(items[0].recorded_at_ms, minute - MINUTE + 7);
    // The cursor pages below it.
    let BotResponse::RoutineDetail { reply } = h
        .send(BotCommand::ReadRoutineDetail {
            bot_id: h.bot(),
            schedule_id: h.schedule(),
            before_occurrence_ms: Some(minute - MINUTE),
            history_limit: 1,
        })
        .unwrap()
    else {
        panic!("detail")
    };
    assert_eq!(
        reply.detail.occurrences,
        RoutineOccurrenceHistoryObservation::Available {
            items: vec![RoutineOccurrenceLine {
                occurrence_ms: minute - 2 * MINUTE,
                outcome: RoutineOccurrenceOutcome::Missed,
                test: false,
                recorded_at_ms: minute - 2 * MINUTE + 7,
                conversation: None,
            }],
            next_before_occurrence_ms: Some(minute - 2 * MINUTE),
        }
    );
    // The list row read no history: it has no such field at all.
    assert!(
        serde_json::to_value(row(&h))
            .unwrap()
            .get("occurrences")
            .is_none()
    );
}

#[test]
fn routine_control_zone_stays_owner_local_without_a_zone_source() {
    let h = H::new();
    h.enable(1).unwrap();
    let RoutineDetailLive::Current {
        owner_zone, next, ..
    } = h.detail().live
    else {
        panic!("live detail")
    };
    assert_eq!(owner_zone, RoutineOwnerZoneObservation::OwnerLocal {});
    assert_eq!(next, RoutineNextObservation::OwnerLocal {});
}
