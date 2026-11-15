// SPDX-License-Identifier: MPL-2.0
use super::*;
pub(super) const WAIT: Duration = Duration::from_secs(8);
pub(super) struct Gate {
    entered: SyncSender<()>,
    release: Receiver<()>,
}
impl Gate {
    fn hold(self) {
        self.entered.send(()).unwrap();
        self.release.recv_timeout(WAIT).unwrap();
    }
}
pub(super) struct Release(Option<SyncSender<()>>);
impl Release {
    pub(super) fn open(&mut self) {
        if let Some(s) = self.0.take() {
            let _ = s.send(());
        }
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        self.open();
    }
}
pub(super) fn gate() -> (Gate, Receiver<()>, Release) {
    let (a, b) = sync_channel(1);
    let (c, d) = sync_channel(1);
    (
        Gate {
            entered: a,
            release: d,
        },
        b,
        Release(Some(c)),
    )
}
pub(super) struct Delegate {
    subject: RunBotSubject,
    pub resolves: AtomicUsize,
    pub denied: AtomicBool,
    pub unknown: AtomicBool,
    pub gate: Mutex<Option<(usize, Gate)>>,
    pub other: Mutex<Vec<&'static str>>,
    /// O147: classify the Run as waiting on this decision.
    pub waiting: Mutex<Option<koru_delegate_core::decisions::DecisionId>>,
}
impl BotDelegatePort for Delegate {
    fn execute(&self, command: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        // Existing startup chat recovery may reconcile without the tool budget.
        // It is never a successful Run proof and cannot launch a task here.
        match command {
            DelegateCommand::Reconcile { .. } => Err(koru_delegate_api::Failure::unavailable()),
            other => panic!("unexpected unbounded tool/provider call: {other:?}"),
        }
    }
    fn execute_within(
        &self,
        command: DelegateCommand,
        within: Duration,
    ) -> Result<BotReply, koru_delegate_api::Failure> {
        assert!(!within.is_zero() && within <= Duration::from_secs(30));
        let unavailable =
            || koru_delegate_api::Failure::new("unavailable", "explicit fixture refusal", true);
        match command {
            DelegateCommand::ResolveRunAuthority { run, generation } => {
                assert_eq!(run, self.subject.run);
                assert_eq!(generation, self.subject.generation);
                let n = self.resolves.fetch_add(1, Ordering::AcqRel) + 1;
                let held = {
                    let mut gate = self.gate.lock().unwrap();
                    if gate.as_ref().is_some_and(|(at, _)| *at == n) {
                        gate.take().map(|(_, g)| g)
                    } else {
                        None
                    }
                };
                if let Some(g) = held {
                    g.hold();
                }
                if self.unknown.load(Ordering::Acquire) {
                    return Err(unavailable());
                }
                let mut policy = self.subject.captured.clone();
                if self.denied.load(Ordering::Acquire) {
                    policy.workspace = WorkspaceId::new();
                }
                let provenance = self.subject.provenance.clone();
                Ok(BotReply::RunAuthority {
                    authority: match *self.waiting.lock().unwrap() {
                        Some(decision) => RunToolAuthority::BotDecisionWait {
                            provenance,
                            policy,
                            decision,
                            phase: koru_delegate_core::decisions::DecisionPhase::Holding,
                        },
                        None => RunToolAuthority::Bot { provenance, policy },
                    },
                })
            }
            DelegateCommand::RegisterRoutineRoute { identity } => {
                assert_eq!(identity.owner, self.subject.provenance.owner);
                assert_eq!(identity.bot, self.subject.provenance.bot);
                self.other.lock().unwrap().push("register");
                Ok(BotReply::RoutineRoute { identity })
            }
            DelegateCommand::Hello { .. } => {
                self.other.lock().unwrap().push("hello");
                Err(unavailable())
            }
            DelegateCommand::PendingNativeEffects { .. } => {
                self.other.lock().unwrap().push("pending");
                Err(unavailable())
            }
            DelegateCommand::Reconcile { .. } => Err(unavailable()),
            other => panic!("unexpected private command: {other:?}"),
        }
    }
}
pub(super) struct Authority {
    adapter: Arc<crate::BotAuthorityAdapter>,
    other: ProfileId,
    pub gate: Mutex<Option<Gate>>,
    pub overviews: AtomicUsize,
    pub creations: AtomicUsize,
    pub wrong_owner: AtomicBool,
    pub wrong_profile: AtomicBool,
    pub panic_once: AtomicBool,
    pub creation_cancellations: Mutex<Vec<Cancellation>>,
    pub creation_gate: Mutex<Option<Gate>>,
}
impl BotAuthority for Authority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.adapter.owner()
    }
    fn workspace(&self, w: WorkspaceId) -> Result<ProfileId, BotError> {
        self.adapter.workspace(w)
    }
    fn page(&self, p: &BotPage) -> Result<(), BotError> {
        self.adapter.page(p)
    }
    fn inspect(&self, w: &[WorkspaceId], p: &[BotPage]) -> Result<BotAuthoritySnapshot, BotError> {
        let mut s = self.adapter.inspect(w, p)?;
        if self.wrong_owner.load(Ordering::Acquire) {
            s.owner.device = koru_delegate_core::DeviceId::new();
        }
        if self.wrong_profile.load(Ordering::Acquire) {
            for profile in s.workspaces.values_mut() {
                *profile = Ok(self.other);
            }
        }
        Ok(s)
    }
    fn read_routine_overview(
        &self,
        o: BotOwner,
        p: ProfileId,
        id: &BotRoutineCreationIdentity,
        c: &Cancellation,
    ) -> Result<RoutineOverviewObservation, BotError> {
        if self.panic_once.swap(false, Ordering::AcqRel) {
            panic!("deliberate Routine model adapter panic");
        }
        self.overviews.fetch_add(1, Ordering::AcqRel);
        let value = self.adapter.read_routine_overview(o, p, id, c);
        let held = self.gate.lock().unwrap().take();
        if let Some(g) = held {
            g.hold();
        }
        value
    }
    fn prepare_routine_document(
        &self,
        o: BotOwner,
        p: ProfileId,
        id: &BotRoutineCreationIdentity,
        c: &Cancellation,
    ) -> Result<PreparedRoutineDocument, BotError> {
        self.creations.fetch_add(1, Ordering::AcqRel);
        self.creation_cancellations.lock().unwrap().push(c.clone());
        let result = self.adapter.prepare_routine_document(o, p, id, c);
        let held = self.creation_gate.lock().unwrap().take();
        if let Some(g) = held {
            g.hold();
        }
        result
    }
}
pub(super) struct Fixture {
    pub store: StoreFixture,
    repo: Mutex<Option<Repository>>,
    pub subject: RunBotSubject,
    pub authority: Arc<Authority>,
    pub delegate: Arc<Delegate>,
}
impl Fixture {
    pub(super) fn new() -> Self {
        Self::with_shared(false)
    }
    pub(super) fn shared() -> Self {
        Self::with_shared(true)
    }
    fn with_shared(shared: bool) -> Self {
        let (store, mut repo) = StoreFixture::with_pause(false);
        let PreparedRoutineDocument::Confirmed(proof) = store.prepare().unwrap() else {
            panic!("real seed");
        };
        repo.transition_bot_routine_creation(
            &store.creation.identity,
            store.creation.observed(),
            BotRoutineCreationTransition::Confirmed(proof),
            3,
        )
        .unwrap();
        let mut intent = super::super::super::super::admission::make_intent(
            &store.bot,
            BotRequestId::mint(),
            store.bot.main_chat,
            store.bot.home_workspace,
            &BotTask {
                text: "Set up recurring work".into(),
                documents: vec![],
            },
            None,
            &[RunContextItem::Document {
                workspace: store.bot.home_workspace,
                document: store.bot.config.role.document,
                version: koru_store_core::ids::DocumentVersionId::new(),
            }],
            store.profile,
        )
        .unwrap();
        if shared {
            let mut turn =
                super::super::super::super::tests::turn(store.bot.main_chat, Uuid::new_v4(), 4);
            turn.user_message.markdown = "Set up recurring work".into();
            intent.submission.provenance.ai_attempt_id = Some(turn.attempt.id.as_uuid());
            intent.submission.provenance.assistant_message_id =
                Some(turn.assistant_message.id.as_uuid());
            repo.prepare_bot_chat(
                &koru_ai_core::bots::replay::BotChatPreparation::Turn(turn.clone()),
                &intent,
                &"b".repeat(64),
                store.bot.revision,
                turn.attempt.queued_at,
            )
            .unwrap();
        } else {
            repo.prepare_bot_submission(&intent, &"b".repeat(64), store.bot.revision, 4)
                .unwrap();
        }
        let subject = RunBotSubject {
            run: intent.submission.request.run,
            generation: AttemptGeneration::FIRST,
            provenance: intent.submission.provenance,
            captured: intent.submission.policy,
        };
        let delegate = Arc::new(Delegate {
            subject: subject.clone(),
            resolves: AtomicUsize::new(0),
            denied: AtomicBool::new(false),
            unknown: AtomicBool::new(false),
            gate: Mutex::new(None),
            other: Mutex::new(vec![]),
            waiting: Mutex::new(None),
        });
        let authority = Arc::new(Authority {
            adapter: store.adapter.clone(),
            other: store.other,
            gate: Mutex::new(None),
            overviews: AtomicUsize::new(0),
            creations: AtomicUsize::new(0),
            wrong_owner: AtomicBool::new(false),
            wrong_profile: AtomicBool::new(false),
            panic_once: AtomicBool::new(false),
            creation_cancellations: Mutex::new(vec![]),
            creation_gate: Mutex::new(None),
        });
        Self {
            store,
            repo: Mutex::new(Some(repo)),
            subject,
            authority,
            delegate,
        }
    }
    pub(super) fn create(&self) -> RunRoutineCreate {
        RunRoutineCreate {
            request_id: BotRequestId::mint(),
            expected_bot_revision: 1,
            name: self.store.command.name.clone(),
            objective: self.store.command.objective.clone(),
            documents: vec![],
            recurrence: self.store.command.recurrence.clone(),
        }
    }
    pub(super) fn prepare(&self, args: &RunRoutineCreate, confirmed: bool) -> BotRoutineCreation {
        let command = RunRoutineOperation::Create(args.clone())
            .create_command(self.store.bot.id)
            .unwrap();
        let (input, fingerprint) = create_body(&command).unwrap();
        let mut lock = self.repo.lock().unwrap();
        let repo = lock.as_mut().unwrap();
        repo.prepare_bot_routine_creation(self.store.owner, &input, fingerprint, 10)
            .unwrap();
        let row = repo
            .bot_routine_creation_by_request(self.store.owner, self.store.bot.id, args.request_id)
            .unwrap()
            .unwrap();
        if confirmed {
            let PreparedRoutineDocument::Confirmed(proof) = self
                .store
                .adapter
                .prepare_routine_document(
                    self.store.owner,
                    self.store.profile,
                    &row.identity,
                    &Cancellation::default(),
                )
                .unwrap()
            else {
                panic!("real retained seed");
            };
            repo.transition_bot_routine_creation(
                &row.identity,
                row.observed(),
                BotRoutineCreationTransition::Confirmed(proof),
                11,
            )
            .unwrap();
        }
        repo.bot_routine_creation_by_request(self.store.owner, self.store.bot.id, args.request_id)
            .unwrap()
            .unwrap()
    }
    pub(super) fn change_configuration(&self) {
        let mut bot = self.store.bot.clone();
        bot.revision += 1;
        bot.config.model.requested_model = "changed-after-creation".into();
        bot.updated_at_ms += 1;
        self.repo
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .save_bot(
                &bot,
                self.store.profile,
                BotRequestId::mint(),
                &"c".repeat(64),
                Some(self.store.bot.revision),
            )
            .unwrap();
    }
    pub(super) fn damage(&self, sql: &str) {
        // Existing repository-only fixture seam, used before worker startup.
        // No production SQL mutation envelope or authority bypass is introduced.
        self.repo
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .fixture_execute_batch(sql)
            .unwrap();
    }
    pub(super) fn cancel_attempt(&self) {
        let attempt =
            InferenceAttemptId::parse(&self.subject.provenance.ai_attempt_id.unwrap().to_string())
                .unwrap();
        self.repo
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .cancel_bot_chat(attempt, Timestamp::from_millis(20))
            .unwrap();
    }
    pub(super) fn retained_subject(&self) -> RoutineToolSubject {
        self.repo
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .routine_tool_subject(self.store.owner, self.store.bot.id, self.subject.run)
            .unwrap()
    }
    pub(super) fn start(&self) -> (WorkerHandle, Receiver<Repository>) {
        let (send, recv) = sync_channel(1);
        let mut config = WorkerConfig::new(
            PathBuf::from("unused-routine-model-fixture"),
            "internal-debug",
            ProviderRegistry::default(),
        )
        .with_bot_authority(self.authority.clone())
        .with_bot_delegate(self.delegate.clone());
        config.test_repository = self.repo.lock().unwrap().take();
        config.test_repository_return = Some(send);
        (AiWorker::new(config).spawn().unwrap(), recv)
    }
}
pub(super) fn finish(worker: WorkerHandle, returned: Receiver<Repository>) -> Repository {
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    returned.recv_timeout(WAIT).unwrap()
}
pub(super) fn request(
    subject: &RunBotSubject,
    operation: RunRoutineOperation,
) -> RunRoutineRequest {
    RunRoutineRequest {
        subject: subject.clone(),
        operation,
        deadline: Instant::now() + WAIT,
        cancellation: Cancellation::default(),
    }
}
pub(super) fn call(
    client: &AiWorkerClient,
    subject: &RunBotSubject,
    operation: RunRoutineOperation,
) -> ApiResult<RunRoutineResult> {
    client.routine_for_run(request(subject, operation))
}
pub(super) fn direct(
    client: &AiWorkerClient,
    request: RunRoutineRequest,
) -> Receiver<ApiResult<RunRoutineResult>> {
    let (reply, recv) = sync_channel(1);
    client
        .sender
        .send(Envelope::RunRoutine { request, reply })
        .unwrap();
    recv
}
pub(super) fn list() -> RunRoutineOperation {
    RunRoutineOperation::List {
        after_schedule: None,
        limit: 16,
    }
}
pub(super) fn page(value: RunRoutineResult) -> BotRoutineListPage {
    match value {
        RunRoutineResult::Routines(v) => v,
        _ => panic!("list reply"),
    }
}
pub(super) fn status(value: RunRoutineResult) -> BotRoutineCreationStatus {
    match value {
        RunRoutineResult::Creation(v) => v,
        _ => panic!("status reply"),
    }
}
pub(super) fn created(value: RunRoutineResult) -> BotRoutineCreateResult {
    match value {
        RunRoutineResult::Created(v) => v,
        _ => panic!("create reply"),
    }
}
