// SPDX-License-Identifier: MPL-2.0
//! Real spawned owner and no-path Store; no model/catalog/readiness override.
use super::*;
use crate::BotAuthorityAdapter;
use crate::bots::routine::fixture::Fixture;
use koru_delegate_core::{BotRunKey, RunId};
use std::sync::Mutex;
use std::sync::mpsc::TryRecvError;
mod detail;

struct Gate {
    entered: SyncSender<()>,
    release: Receiver<()>,
}
struct Release(Option<SyncSender<()>>);
impl Release {
    fn open(&mut self) {
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
fn gate() -> (Gate, Receiver<()>, Release) {
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
struct Authority {
    adapter: Arc<BotAuthorityAdapter>,
    gate: Mutex<Option<Gate>>,
    calls: Mutex<Vec<ScheduleId>>,
    wrong_owner: AtomicBool,
    unavailable: Mutex<Option<ScheduleId>>,
    panic_once: AtomicBool,
}
impl Authority {
    fn new(f: &Fixture) -> Arc<Self> {
        Arc::new(Self {
            adapter: f.adapter.clone(),
            gate: Mutex::new(None),
            calls: Mutex::new(vec![]),
            wrong_owner: AtomicBool::new(false),
            unavailable: Mutex::new(None),
            panic_once: AtomicBool::new(false),
        })
    }
}
impl BotAuthority for Authority {
    fn read_routine_detail(
        &self,
        o: BotOwner,
        p: ProfileId,
        id: &BotRoutineCreationIdentity,
        _at: i64,
        c: &Cancellation,
    ) -> Result<RoutineDetailObservation, BotError> {
        if self.panic_once.swap(false, Ordering::AcqRel) {
            panic!("deliberate detail adapter panic");
        }
        self.calls.lock().unwrap().push(id.schedule);
        let result = self.adapter.read_routine_detail(o, p, id, 0, c);
        if let Some(g) = self.gate.lock().unwrap().take() {
            g.entered.send(()).unwrap();
            g.release.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        if *self.unavailable.lock().unwrap() == Some(id.schedule) {
            return Ok(RoutineDetailObservation::Unavailable);
        }
        result
    }
    fn owner(&self) -> Result<BotOwner, BotError> {
        let mut o = self.adapter.owner()?;
        if self.wrong_owner.load(Ordering::Acquire) {
            o.device = koru_delegate_core::DeviceId::new();
        }
        Ok(o)
    }
    fn workspace(&self, w: WorkspaceId) -> Result<ProfileId, BotError> {
        self.adapter.workspace(w)
    }
    fn page(&self, p: &BotPage) -> Result<(), BotError> {
        self.adapter.page(p)
    }
    fn inspect(&self, w: &[WorkspaceId], p: &[BotPage]) -> Result<BotAuthoritySnapshot, BotError> {
        let mut s = self.adapter.inspect(w, p)?;
        s.owner = self.owner()?;
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
            panic!("deliberate listing adapter panic");
        }
        self.calls.lock().unwrap().push(id.schedule);
        let result = self.adapter.read_routine_overview(o, p, id, c);
        if let Some(g) = self.gate.lock().unwrap().take() {
            g.entered.send(()).unwrap();
            g.release.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        if *self.unavailable.lock().unwrap() == Some(id.schedule) {
            return Ok(RoutineOverviewObservation::Unavailable);
        }
        result
    }
}
fn bound() -> (Fixture, Repository) {
    let (f, mut r) = Fixture::new();
    let PreparedRoutineDocument::Confirmed(proof) = f.prepare().unwrap() else {
        panic!("actual Store seed")
    };
    r.transition_bot_routine_creation(
        &f.creation.identity,
        f.creation.observed(),
        BotRoutineCreationTransition::Confirmed(proof),
        3,
    )
    .unwrap();
    (f, r)
}
fn context() -> RequestContext {
    RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "owner-fixture".into(),
            scopes: vec![AiScope::ManageBots],
        },
        trace_id: "listing".into(),
        deadline_millis: None,
    }
}
fn request(f: &Fixture) -> BotCommand {
    BotCommand::ReadRoutines {
        bot_id: f.bot.id,
        after_schedule: None,
        limit: 16,
    }
}
fn start(
    repo: Repository,
    authority: Arc<dyn BotAuthority>,
) -> (WorkerHandle, Receiver<Repository>) {
    let (send, returned) = sync_channel(1);
    let mut cfg = WorkerConfig::new(
        PathBuf::from("unused-listing-fixture"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    cfg.test_repository = Some(repo);
    cfg.test_repository_return = Some(send);
    (AiWorker::new(cfg).spawn().unwrap(), returned)
}
fn finish(worker: WorkerHandle, returned: Receiver<Repository>) -> Repository {
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    returned.recv_timeout(Duration::from_secs(5)).unwrap()
}
fn list(client: &AiWorkerClient, request: BotCommand) -> ApiResult<BotRoutineListPage> {
    client.bots(context(), request).map(|r| match r {
        BotResponse::Routines { page } => page,
        _ => panic!("listing response"),
    })
}
fn direct(
    client: &AiWorkerClient,
    request: BotCommand,
    cancellation: Cancellation,
) -> Receiver<ApiResult<BotResponse>> {
    let (reply, result) = sync_channel(1);
    client
        .sender
        .send(Envelope::RoutineListing {
            command: WorkerCommand::Bots {
                context: context().into(),
                request,
                reply,
            },
            cancellation,
            deadline: Instant::now() + Duration::from_secs(8),
        })
        .unwrap();
    result
}

#[test]
fn listing_tickets_bound_workflows_across_out_of_order_completion_and_owner_epochs() {
    let mut listings = Listings::default();
    let mut tickets = (0..CAPACITY)
        .map(|_| Some(listings.acquire().unwrap()))
        .collect::<Vec<_>>();
    assert!(listings.acquire().is_err());
    let serial = tickets[3].as_ref().unwrap().serial;
    let index = tickets[3].as_ref().unwrap().index;
    drop(tickets[3].take());
    let next = listings.acquire().unwrap();
    assert_eq!(next.index, index);
    assert_ne!(next.serial, serial);
    // A stale destructor cannot vacate the new slot, even after an epoch switch.
    drop(Ticket {
        slots: listings.slots.clone(),
        epoch: listings.epoch.clone(),
        index,
        serial,
    });
    assert!(next.valid());
    listings.invalidate();
    assert!(!next.valid());
    assert!(listings.acquire().is_err());
    drop(next);
    let current = listings.acquire().unwrap();
    assert!(listings.owns(&current));
    drop(tickets);
    assert!(listings.owns(&current));
}

#[test]
fn listing_worker_reads_actual_paused_store_through_normal_owner_command() {
    let (f, repo) = bound();
    let authority = Authority::new(&f);
    let (worker, returned) = start(repo, authority.clone());
    let page = list(&worker.client(), request(&f)).unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(matches!(
        page.items[0].live,
        BotRoutineLiveObservation::Current { .. }
    ));
    assert_eq!(
        page.items[0].local_conversations,
        BotRoutineLocalConversations::Available {
            items: vec![],
            more: false
        }
    );
    assert_eq!(
        *authority.calls.lock().unwrap(),
        vec![f.creation.identity.schedule]
    );
    let repo = finish(worker, returned);
    assert_eq!(
        repo.bot_routine_creation(f.owner, f.bot.id, f.creation.identity.schedule)
            .unwrap()
            .unwrap()
            .state,
        BotRoutineCreationState::Bound
    );
}

#[test]
fn listing_adapter_panic_completes_ticket_and_next_request_uses_same_lane() {
    let (f, repo) = bound();
    let authority = Authority::new(&f);
    authority.panic_once.store(true, Ordering::Release);
    let (worker, returned) = start(repo, authority);
    assert!(list(&worker.client(), request(&f)).is_err());
    assert!(matches!(
        list(&worker.client(), request(&f)).unwrap().items[0].live,
        BotRoutineLiveObservation::Current { .. }
    ));
    finish(worker, returned);
}

#[test]
fn listing_worker_empty_page_still_checks_current_home_profile() {
    for changed in [false, true] {
        let (f, repo) = bound();
        repo.fixture_execute_batch("DELETE FROM bot_routines;")
            .unwrap();
        if changed {
            let (home, profile) = (f.bot.home_workspace, f.other);
            f.memory
                .with_store(move |s| s.move_workspace(home, profile).unwrap())
                .unwrap();
        }
        let authority = Authority::new(&f);
        let (worker, returned) = start(repo, authority.clone());
        let result = list(&worker.client(), request(&f));
        if changed {
            assert_eq!(result.unwrap_err().code, "wrong_owner");
        } else {
            assert!(result.unwrap().items.is_empty());
        }
        assert!(authority.calls.lock().unwrap().is_empty());
        finish(worker, returned);
    }
}

#[test]
fn listing_worker_permission_bounds_and_missing_subject_refuse_without_schedule_read() {
    let (f, repo) = bound();
    let authority = Authority::new(&f);
    let (worker, returned) = start(repo, authority.clone());
    let mut denied = context();
    denied.principal.scopes.clear();
    assert_eq!(
        worker.client().bots(denied, request(&f)).unwrap_err().code,
        "forbidden"
    );
    for limit in [0, 17] {
        assert!(
            list(
                &worker.client(),
                BotCommand::ReadRoutines {
                    bot_id: f.bot.id,
                    after_schedule: None,
                    limit
                }
            )
            .is_err()
        );
    }
    assert!(
        list(
            &worker.client(),
            BotCommand::ReadRoutines {
                bot_id: BotId::mint(),
                after_schedule: None,
                limit: 16
            }
        )
        .is_err()
    );
    assert!(authority.calls.lock().unwrap().is_empty());
    finish(worker, returned);
}

#[test]
fn listing_worker_archive_pause_trash_and_pending_whole_lifecycle_are_truthful() {
    for state in 0..4 {
        let (f, mut repo) = bound();
        if state == 1 {
            repo.fixture_set_bot_lifecycle(f.bot.id, BotLifecycle::Archived)
                .unwrap();
        }
        if state == 2 {
            repo.fixture_set_bot_lifecycle(f.bot.id, BotLifecycle::Trashed)
                .unwrap();
        }
        if state == 3 {
            repo.capture_bot_lifecycle(&koru_ai_sqlite::LifecycleCapture {
                operation: BotRequestId::mint(),
                owner: f.owner,
                bot: f.bot.id,
                kind: BotLifecycleKind::Archive,
                chat: None,
                fingerprint: "a".repeat(64),
                expected_revision: f.bot.revision,
                source: None,
                members: vec![],
                now: 4,
            })
            .unwrap();
        }
        let authority = Authority::new(&f);
        let (worker, returned) = start(repo, authority);
        let result = list(&worker.client(), request(&f));
        assert_eq!(result.is_ok(), state < 2);
        finish(worker, returned);
    }
}

fn add(f: &Fixture, repo: &mut Repository) -> ScheduleId {
    let mut command = f.command.clone();
    command.request = BotRequestId::mint();
    repo.prepare_bot_routine_creation(
        f.owner,
        &command,
        koru_delegate_core::RoutineDigest32::from_bytes([8; 32]),
        4,
    )
    .unwrap();
    let c = repo
        .bot_routine_creation_by_request(f.owner, f.bot.id, command.request)
        .unwrap()
        .unwrap();
    let PreparedRoutineDocument::Confirmed(proof) = f
        .adapter
        .prepare_routine_document(f.owner, f.profile, &c.identity, &Cancellation::default())
        .unwrap()
    else {
        panic!("Store proof")
    };
    repo.transition_bot_routine_creation(
        &c.identity,
        c.observed(),
        BotRoutineCreationTransition::Confirmed(proof),
        5,
    )
    .unwrap();
    c.identity.schedule
}
#[test]
fn listing_worker_item_unavailable_preserves_identity_and_page_cursor() {
    let (f, mut repo) = bound();
    let second = add(&f, &mut repo);
    let first = f.creation.identity.schedule.min(second);
    let authority = Authority::new(&f);
    *authority.unavailable.lock().unwrap() = Some(first);
    let (worker, returned) = start(repo, authority);
    let page = list(
        &worker.client(),
        BotCommand::ReadRoutines {
            bot_id: f.bot.id,
            after_schedule: None,
            limit: 1,
        },
    )
    .unwrap();
    assert_eq!(page.items[0].schedule_id, first);
    assert_eq!(
        page.items[0].live,
        BotRoutineLiveObservation::Unavailable {}
    );
    assert_eq!(page.next_schedule, Some(first));
    let next = list(
        &worker.client(),
        BotCommand::ReadRoutines {
            bot_id: f.bot.id,
            after_schedule: page.next_schedule,
            limit: 1,
        },
    )
    .unwrap();
    assert_eq!(next.items.len(), 1);
    assert!(matches!(
        next.items[0].live,
        BotRoutineLiveObservation::Current { .. }
    ));
    finish(worker, returned);
}

#[test]
fn listing_worker_invalid_execution_settings_do_not_require_model_or_catalog() {
    let (f, repo) = bound();
    repo.fixture_execute_batch(
        "UPDATE bots SET model=zeroblob(1000000),working_folder=zeroblob(1000000);",
    )
    .unwrap();
    let (worker, returned) = start(repo, Authority::new(&f));
    let page = list(&worker.client(), request(&f)).unwrap();
    assert_eq!(
        page.effective_model,
        BotRoutineModelObservation::Unavailable {}
    );
    assert!(matches!(
        page.items[0].live,
        BotRoutineLiveObservation::Current { .. }
    ));
    finish(worker, returned);
}

#[test]
fn listing_worker_processes_retained_identities_in_keyset_order() {
    let (f, mut repo) = bound();
    for _ in 0..3 {
        add(&f, &mut repo);
    }
    let authority = Authority::new(&f);
    let (worker, returned) = start(repo, authority.clone());
    let page = list(&worker.client(), request(&f)).unwrap();
    let expected = page.items.iter().map(|r| r.schedule_id).collect::<Vec<_>>();
    assert_eq!(*authority.calls.lock().unwrap(), expected);
    assert_eq!(expected.len(), 4);
    finish(worker, returned);
}

#[test]
fn listing_worker_late_profile_or_bootstrap_owner_change_refuses_whole_page() {
    for profile in [true, false] {
        let (f, repo) = bound();
        let authority = Authority::new(&f);
        let (g, entered, mut release) = gate();
        *authority.gate.lock().unwrap() = Some(g);
        let (worker, returned) = start(repo, authority.clone());
        let client = worker.client();
        let req = request(&f);
        let join = std::thread::spawn(move || list(&client, req));
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        if profile {
            let (home, other) = (f.bot.home_workspace, f.other);
            f.memory
                .with_store(move |s| s.move_workspace(home, other).unwrap())
                .unwrap();
        } else {
            authority.wrong_owner.store(true, Ordering::Release);
        }
        release.open();
        assert_eq!(join.join().unwrap().unwrap_err().code, "wrong_owner");
        finish(worker, returned);
    }
}

#[test]
fn listing_worker_cancelled_physical_phase_keeps_waiter_and_public_preparation_until_completion() {
    let (f, repo) = bound();
    let authority = Authority::new(&f);
    let (g, entered, mut release) = gate();
    *authority.gate.lock().unwrap() = Some(g);
    let (worker, returned) = start(repo, authority);
    let cancel = Cancellation::default();
    let result = direct(&worker.client(), request(&f), cancel.clone());
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    cancel.cancel();
    assert!(matches!(result.try_recv(), Err(TryRecvError::Empty)));
    let client = worker.client();
    let command = BotCommand::ReadRun {
        bot_id: f.bot.id,
        chat_id: f.bot.main_chat,
        message_id: None,
        run_id: RunId::mint(),
        from: 0,
        len: 1,
    };
    let (sent, observed) = sync_channel(1);
    client
        .send(WorkerCommand::Bots {
            context: context().into(),
            request: command,
            reply: sent,
        })
        .unwrap();
    // FIFO command receipt proves the worker remains responsive, without
    // pretending the queued public authority preparation bypasses this lane.
    worker.info().unwrap();
    assert!(matches!(observed.try_recv(), Err(TryRecvError::Empty)));
    release.open();
    assert!(
        result
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err()
    );
    assert!(
        observed
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err()
    );
    finish(worker, returned);
}

#[derive(Default)]
struct RefusingDelegate {
    calls: Mutex<Vec<&'static str>>,
}
impl BotDelegatePort for RefusingDelegate {
    fn execute(&self, _: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        panic!("finite lane calls only")
    }
    fn execute_within(
        &self,
        c: DelegateCommand,
        _: Duration,
    ) -> Result<BotReply, koru_delegate_api::Failure> {
        match c {
            DelegateCommand::Read { .. } => {
                self.calls.lock().unwrap().push("read");
                Err(koru_delegate_api::Failure::new(
                    "unknown_run",
                    "fixture absent",
                    false,
                ))
            }
            DelegateCommand::Reconcile { .. } => {
                self.calls.lock().unwrap().push("reconcile");
                Ok(BotReply::Missing)
            }
            _ => panic!("only Read and the Stop lane's initial Reconcile are expected"),
        }
    }
}
#[test]
fn listing_prepared_run_read_and_stop_lanes_are_independent_of_held_store_phase() {
    let (f, repo) = bound();
    let authority = Authority::new(&f);
    let (g, entered, mut release) = gate();
    *authority.gate.lock().unwrap() = Some(g);
    let snapshot = repo
        .routine_list_snapshot(f.owner, f.bot.id, None, 16)
        .unwrap();
    let (creation, checkpoint, item) = repo.routine_list_entry(&snapshot, 0).unwrap();
    let mut listings = Listings::default();
    let (reply, _) = sync_channel(1);
    let job = ListingJob {
        ticket: listings.acquire().unwrap(),
        context: context().into(),
        bot: f.bot.id,
        after: None,
        limit: 16,
        cancellation: Cancellation::default(),
        deadline: Instant::now() + Duration::from_secs(8),
        authority: authority.clone(),
        reply,
        phase: Phase::Item(Box::new(creation)),
        progress: Some(ListingProgress {
            snapshot,
            checkpoints: vec![checkpoint],
            items: vec![item],
        }),
        observed_at_ms: 0,
        zone: None,
    };
    let (output, completion) = sync_channel(8);
    let mut pool =
        ProviderAdminPool::spawn(None, None, output, Arc::new(AtomicBool::new(false))).unwrap();
    assert!(
        pool.try_submit(ProviderAdminJob::BotRoutineListing(job))
            .is_ok()
    );
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let (delegate, deadline) = (
        Arc::new(RefusingDelegate::default()),
        Instant::now() + Duration::from_secs(3),
    );
    let (r, read) = sync_channel(1);
    let (s, stop) = sync_channel(1);
    let key = BotRunKey {
        owner: f.creation.identity.owner,
        bot: f.bot.id,
        run: RunId::mint(),
    };
    assert!(
        pool.try_submit(ProviderAdminJob::BotRunRead(BotRunReadJob::fixture(
            authority,
            f.owner,
            f.bot.home_workspace,
            key,
            delegate.clone(),
            deadline,
            r
        )))
        .is_ok()
    );
    assert!(
        pool.try_submit(ProviderAdminJob::BotTaskStop(BotTaskStopJob::fixture(
            delegate.clone(),
            deadline,
            s
        )))
        .is_ok()
    );
    assert_eq!(
        read.recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        stop.recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap_err()
            .code,
        "not_found"
    );
    let mut calls = delegate.calls.lock().unwrap().clone();
    calls.sort_unstable();
    assert_eq!(calls, ["read", "reconcile"]);
    assert!(matches!(completion.try_recv(), Err(TryRecvError::Empty)));
    release.open();
    assert!(matches!(
        completion.recv_timeout(Duration::from_secs(5)).unwrap(),
        Envelope::RoutineListingCompleted(_)
    ));
    pool.stop();
}

#[test]
fn listing_shutdown_joins_held_physical_work_and_never_reports_late_success() {
    let (f, repo) = bound();
    let authority = Authority::new(&f);
    let (g, entered, mut release) = gate();
    *authority.gate.lock().unwrap() = Some(g);
    let (worker, returned) = start(repo, authority);
    let result = direct(&worker.client(), request(&f), Cancellation::default());
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.shutdown();
    let (done, observe) = sync_channel(1);
    let join = std::thread::spawn(move || done.send(worker.join()).unwrap());
    assert!(matches!(observe.try_recv(), Err(TryRecvError::Empty)));
    release.open();
    assert_eq!(
        observe.recv_timeout(Duration::from_secs(5)).unwrap(),
        WorkerExit::Clean
    );
    join.join().unwrap();
    returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(!matches!(
        result.recv_timeout(Duration::from_secs(1)),
        Ok(Ok(_))
    ));
}
