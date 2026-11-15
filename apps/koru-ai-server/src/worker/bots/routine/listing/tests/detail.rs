// SPDX-License-Identifier: MPL-2.0
//! Real spawned writer/preparation lane and injected no-path Store.
use super::*;

fn request_detail(f: &Fixture) -> BotCommand {
    BotCommand::ReadRoutineDetail {
        bot_id: f.bot.id,
        schedule_id: f.creation.identity.schedule,
        before_occurrence_ms: None,
        history_limit: 16,
    }
}
fn detail(client: &AiWorkerClient, request: BotCommand) -> ApiResult<BotRoutineDetail> {
    client.bots(context(), request).map(|r| match r {
        BotResponse::RoutineDetail { reply } => reply.detail,
        _ => panic!("detail response"),
    })
}
fn direct_detail(
    client: &AiWorkerClient,
    request: BotCommand,
    cancellation: Cancellation,
    deadline: Instant,
) -> Receiver<ApiResult<BotResponse>> {
    let (reply, result) = sync_channel(1);
    client
        .sender
        .send(Envelope::RoutineDetail {
            command: WorkerCommand::Bots {
                context: context().into(),
                request,
                reply,
            },
            cancellation,
            deadline,
        })
        .unwrap();
    result
}
struct Running {
    worker: Option<WorkerHandle>,
    returned: Option<Receiver<Repository>>,
    release: Option<Release>,
}
impl Running {
    fn new(repo: Repository, authority: Arc<dyn BotAuthority>, release: Option<Release>) -> Self {
        let (worker, returned) = start(repo, authority);
        Self {
            worker: Some(worker),
            returned: Some(returned),
            release,
        }
    }
    fn client(&self) -> AiWorkerClient {
        self.worker.as_ref().unwrap().client()
    }
    fn release(&mut self) {
        if let Some(release) = &mut self.release {
            release.open();
        }
    }
    fn finish(mut self) -> Repository {
        self.release();
        finish(self.worker.take().unwrap(), self.returned.take().unwrap())
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.release();
        if let Some(worker) = self.worker.take() {
            worker.shutdown();
            let _ = worker.join();
        }
    }
}

#[test]
fn routine_detail_shared_nine_slots_tail_fairness_cancel() {
    let (f, repo) = bound();
    let authority = Authority::new(&f);
    let (g, entered, release) = gate();
    *authority.gate.lock().unwrap() = Some(g);
    let mut running = Running::new(repo, authority, Some(release));
    let client = running.client();
    let cancel = Cancellation::default();
    let deadline = Instant::now() + Duration::from_secs(8);
    let first = direct_detail(&client, request_detail(&f), cancel.clone(), deadline);
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut queued = Vec::new();
    let queued_cancel = Cancellation::default();
    for index in 0..7 {
        queued.push(direct_detail(
            &client,
            request_detail(&f),
            if index == 0 {
                queued_cancel.clone()
            } else {
                Cancellation::default()
            },
            deadline,
        ));
    }
    let overview = direct(&client, request(&f), Cancellation::default());
    running.worker.as_ref().unwrap().info().unwrap();
    let extra = direct_detail(
        &client,
        request_detail(&f),
        Cancellation::default(),
        deadline,
    );
    assert_eq!(
        extra
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap_err()
            .code,
        "ai_worker_saturated"
    );
    queued_cancel.cancel();
    cancel.cancel();
    let extra = direct_detail(
        &client,
        request_detail(&f),
        Cancellation::default(),
        deadline,
    );
    assert_eq!(
        extra
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap_err()
            .code,
        "ai_worker_saturated"
    );
    assert!(
        matches!(first.try_recv(), Err(TryRecvError::Empty)),
        "cancellation did not complete physical Store phase"
    );
    running.release();
    assert!(first.recv_timeout(Duration::from_secs(5)).unwrap().is_err());
    let results = queued
        .into_iter()
        .map(|r| r.recv_timeout(Duration::from_secs(5)).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 6);
    assert!(
        overview
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_ok(),
        "overview uses same bounded lane and progresses"
    );
    assert!(detail(&client, request_detail(&f)).is_ok());
    running.finish();
}

#[test]
fn routine_detail_deadline_late_epoch_and_panic() {
    // Generous initial bound; expiry is induced only after positive peer entry.
    let (f, repo) = bound();
    let authority = Authority::new(&f);
    let (g, entered, release) = gate();
    *authority.gate.lock().unwrap() = Some(g);
    let mut running = Running::new(repo, authority, Some(release));
    let deadline = Instant::now() + Duration::from_secs(5);
    let result = direct_detail(
        &running.client(),
        request_detail(&f),
        Cancellation::default(),
        deadline,
    );
    entered.recv_timeout(Duration::from_secs(4)).unwrap();
    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
    running.release();
    assert!(
        result
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err()
    );
    running.finish();
    let (f, repo) = bound();
    let authority = Authority::new(&f);
    let (g, entered, release) = gate();
    *authority.gate.lock().unwrap() = Some(g);
    let mut running = Running::new(repo, authority.clone(), Some(release));
    let client = running.client();
    let first = direct_detail(
        &client,
        request_detail(&f),
        Cancellation::default(),
        Instant::now() + Duration::from_secs(8),
    );
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    authority.wrong_owner.store(true, Ordering::Release);
    let newer = direct_detail(
        &client,
        request_detail(&f),
        Cancellation::default(),
        Instant::now() + Duration::from_secs(8),
    );
    running.release();
    assert!(first.recv_timeout(Duration::from_secs(5)).unwrap().is_err());
    assert!(newer.recv_timeout(Duration::from_secs(5)).unwrap().is_err());
    running.finish();
    let (f, repo) = bound();
    let authority = Authority::new(&f);
    authority.panic_once.store(true, Ordering::Release);
    let running = Running::new(repo, authority, None);
    let client = running.client();
    assert!(detail(&client, request_detail(&f)).is_err());
    assert!(detail(&client, request_detail(&f)).is_ok());
    running.finish();
}

#[test]
fn routine_detail_missing_nonbound_archived_paused_no_capture() {
    for mode in 0..5 {
        let (f, mut repo) = if mode == 1 { Fixture::new() } else { bound() };
        if mode == 2 {
            repo.fixture_set_bot_lifecycle(f.bot.id, BotLifecycle::Archived)
                .unwrap();
        }
        if mode == 3 {
            repo.fixture_set_bot_lifecycle(f.bot.id, BotLifecycle::Trashed)
                .unwrap();
        }
        if mode == 4 {
            repo.fixture_execute_batch(
                "UPDATE bots SET model=zeroblob(1000000),working_folder=zeroblob(1000000)",
            )
            .unwrap();
        }
        let authority = Authority::new(&f);
        let running = Running::new(repo, authority.clone(), None);
        let client = running.client();
        let response = detail(&client, request_detail(&f));
        if mode == 3 {
            assert!(response.is_err());
        } else {
            let response = response.unwrap();
            assert_eq!(response.schedule_id, f.creation.identity.schedule);
            if mode == 1 {
                assert_eq!(response.live, RoutineDetailLive::Unavailable {});
                assert_eq!(
                    response.local_history,
                    RoutineLocalHistoryObservation::Unavailable {}
                );
                assert!(authority.calls.lock().unwrap().is_empty());
            } else {
                assert!(matches!(response.live, RoutineDetailLive::Current { .. }));
            }
            if mode == 4 {
                assert_eq!(
                    response.destination,
                    RoutineDestinationObservation::Unavailable {}
                );
                assert_eq!(
                    response.configured_model,
                    BotRoutineModelObservation::Unavailable {}
                );
            }
            if mode == 2 {
                assert!(
                    matches!(response.readiness,RoutineReadinessObservation::Blocked{reasons} if reasons.contains(&RoutineReadinessReason::BotArchived))
                );
            }
        }
        let mut missing = request_detail(&f);
        let BotCommand::ReadRoutineDetail { schedule_id, .. } = &mut missing else {
            panic!()
        };
        *schedule_id = ScheduleId::mint();
        assert_eq!(detail(&client, missing).unwrap_err().code, "not_found");
        for limit in [0, 17] {
            let mut bad = request_detail(&f);
            let BotCommand::ReadRoutineDetail { history_limit, .. } = &mut bad else {
                panic!()
            };
            *history_limit = limit;
            assert!(detail(&client, bad).is_err());
        }
        let mut denied = context();
        denied.principal.scopes.clear();
        assert_eq!(
            client.bots(denied, request_detail(&f)).unwrap_err().code,
            "forbidden"
        );
        running.finish();
    }
}
