// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn schedule_management_failures_keep_their_stable_non_retryable_codes() {
    for code in [
        "schedule_not_found",
        "schedule_wrong_workspace",
        "schedule_not_owner",
        "schedule_retired",
        "schedule_read_only",
        "schedule_name_conflict",
    ] {
        let failure = schedule_failure(PortError::new(Port::Schedules, code, "safe fixture"));
        assert_eq!(failure.code, code);
        assert!(!failure.retryable);
    }
}

#[test]
fn an_idle_week_has_zero_evaluations_and_no_timer_wait() {
    let counters_before = koru_delegate_runtime::counters::snapshot();
    let start = Instant::now();
    let mut state = LoopState::default();
    for minute in 0..7 * 24 * 60 {
        let elapsed = Duration::from_secs(minute * 60);
        let monotonic_now = start + elapsed;
        let wall_now_ms = i64::try_from(elapsed.as_millis()).unwrap();
        assert!(!state.take_evaluation(false, wall_now_ms, monotonic_now));
        assert_eq!(state.next_wait(wall_now_ms, monotonic_now), None);
    }
    assert_eq!(state.evaluations, 0);
    let counters_after = koru_delegate_runtime::counters::snapshot();
    assert_eq!(
        [
            counters_after.schedule_wakes,
            counters_after.schedule_evaluations,
            counters_after.schedule_documents_evaluated,
            counters_after.schedule_evaluation_failures,
            counters_after.schedule_fired,
            counters_after.schedule_skipped_missed,
            counters_after.schedule_skipped_busy,
        ],
        [
            counters_before.schedule_wakes,
            counters_before.schedule_evaluations,
            counters_before.schedule_documents_evaluated,
            counters_before.schedule_evaluation_failures,
            counters_before.schedule_fired,
            counters_before.schedule_skipped_missed,
            counters_before.schedule_skipped_busy,
        ],
        "an idle calendar advances no live scheduler counter"
    );
}

struct BlockingScheduleEvents {
    entered: mpsc::SyncSender<()>,
    release: mpsc::Receiver<()>,
    block: bool,
    authority_gate: Option<Arc<ScheduleEventAuthorityGate>>,
}

impl ScheduleEvents for BlockingScheduleEvents {
    fn maintain(
        &mut self,
    ) -> koru_delegate_runtime::PortResult<koru_delegate_runtime::ScheduleEventMaintenance> {
        if std::mem::take(&mut self.block) {
            let _authority = self.authority_gate.as_ref().and_then(|gate| gate.enter());
            let _ = self.entered.send(());
            let _ = self.release.recv();
        }
        Ok(koru_delegate_runtime::ScheduleEventMaintenance::default())
    }

    fn drain_ready(
        &mut self,
        _limit: usize,
    ) -> koru_delegate_runtime::PortResult<koru_delegate_runtime::ScheduleEventMaintenance> {
        self.maintain()
    }

    fn reconcile_prepared(
        &mut self,
        _limit: usize,
    ) -> koru_delegate_runtime::PortResult<koru_delegate_runtime::ScheduleEventMaintenance> {
        Ok(koru_delegate_runtime::ScheduleEventMaintenance::default())
    }

    fn reconcile_created(
        &mut self,
        _schedule: koru_delegate_core::ScheduleId,
    ) -> koru_delegate_runtime::PortResult<()> {
        Ok(())
    }

    fn existing_created(
        &mut self,
        _schedule: koru_delegate_runtime::ScheduleEventRef,
        _header: &koru_delegate_schedule::ScheduleHeader,
    ) -> koru_delegate_runtime::PortResult<()> {
        Ok(())
    }

    fn prepare_created(
        &mut self,
        _schedule: koru_delegate_runtime::ScheduleEventRef,
        _header: &koru_delegate_schedule::ScheduleHeader,
    ) -> koru_delegate_runtime::PortResult<koru_delegate_runtime::PreparedScheduleEvent> {
        unreachable!()
    }

    fn prepare_definition(
        &mut self,
        _schedule: koru_delegate_runtime::ScheduleEventRef,
        _definition: &koru_delegate_schedule::ScheduleDefinition,
        _event: koru_delegate_runtime::ScheduleDefinitionEvent,
    ) -> koru_delegate_runtime::PortResult<koru_delegate_runtime::PreparedScheduleEvent> {
        unreachable!()
    }

    fn prepare_occurrence(
        &mut self,
        _fact: koru_delegate_runtime::ScheduleOccurrenceEvent,
    ) -> koru_delegate_runtime::PortResult<Option<koru_delegate_runtime::PreparedScheduleEvent>>
    {
        unreachable!()
    }

    fn reconcile_occurrence(
        &mut self,
        _schedule: koru_delegate_runtime::ScheduleEventRef,
        _prepared: Option<koru_delegate_runtime::PreparedScheduleEvent>,
        _claim: &koru_delegate_sqlite::SlotClaim,
    ) -> koru_delegate_runtime::PortResult<Option<koru_delegate_runtime::PreparedScheduleEvent>>
    {
        unreachable!()
    }

    fn ready(
        &mut self,
        _prepared: koru_delegate_runtime::PreparedScheduleEvent,
    ) -> koru_delegate_runtime::PortResult<()> {
        unreachable!()
    }

    fn abandon(
        &mut self,
        _prepared: koru_delegate_runtime::PreparedScheduleEvent,
    ) -> koru_delegate_runtime::PortResult<()> {
        unreachable!()
    }
}

#[test]
fn a_blocked_journal_worker_cannot_delay_the_schedule_api_reply() {
    let (entered, wait_entered) = mpsc::sync_channel(1);
    let (release, wait_release) = mpsc::sync_channel(1);
    let mut publisher = EventPublisher::spawn(Box::new(BlockingScheduleEvents {
        entered,
        release: wait_release,
        block: true,
        authority_gate: None,
    }))
    .unwrap();
    wait_entered.recv_timeout(Duration::from_secs(1)).unwrap();

    let (reply, answer) = mpsc::sync_channel(1);
    assert!(answer_then_publish(
        reply,
        Ok::<_, ()>(()),
        &publisher.wake()
    ));
    assert_eq!(
        answer.recv_timeout(Duration::from_millis(100)),
        Ok(Ok(())),
        "the API answer is committed before journal delivery is woken"
    );

    release.send(()).unwrap();
    publisher.stop();
}

#[test]
fn publisher_stop_is_bounded_when_maintenance_does_not_return() {
    let (entered, wait_entered) = mpsc::sync_channel(1);
    let (release, wait_release) = mpsc::sync_channel(1);
    let mut publisher = EventPublisher::spawn_with_stop_bound(
        Box::new(BlockingScheduleEvents {
            entered,
            release: wait_release,
            block: true,
            authority_gate: None,
        }),
        Duration::from_millis(40),
    )
    .unwrap();
    wait_entered.recv_timeout(Duration::from_secs(1)).unwrap();

    let started = Instant::now();
    publisher.stop();
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "a wedged event transport cannot hold delegate shutdown beyond its bound"
    );

    // Let the detached test worker observe its already-queued Stop and finish;
    // production HTTP calls have their own two-second deadline and normally
    // join before the defensive outer bound is reached.
    drop(release);
}

#[test]
fn full_handle_shutdown_wakes_a_scheduler_waiting_behind_a_blocked_publisher() {
    let gate = Arc::new(ScheduleEventAuthorityGate::new());
    let (publisher_entered, wait_publisher_entered) = mpsc::sync_channel(1);
    let (release_publisher, wait_release_publisher) = mpsc::sync_channel(1);
    let publisher = EventPublisher::spawn_with_stop_bound(
        Box::new(BlockingScheduleEvents {
            entered: publisher_entered,
            release: wait_release_publisher,
            block: true,
            authority_gate: Some(Arc::clone(&gate)),
        }),
        Duration::from_millis(40),
    )
    .unwrap();
    wait_publisher_entered
        .recv_timeout(Duration::from_secs(1))
        .unwrap();

    let (scheduler_waiting, wait_scheduler_waiting) = mpsc::sync_channel(1);
    let (scheduler_answer, wait_scheduler_answer) = mpsc::sync_channel(1);
    let scheduler_gate = Arc::clone(&gate);
    let scheduler_thread = std::thread::spawn(move || {
        let _ = scheduler_waiting.send(());
        let admitted = scheduler_gate.enter().is_some();
        let _ = scheduler_answer.send(admitted);
    });
    wait_scheduler_waiting
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        wait_scheduler_answer.recv_timeout(Duration::from_millis(20)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    let (commands, _inbox) = mpsc::channel();
    let handle = Handle {
        stopped: Arc::new(AtomicBool::new(false)),
        commands,
        wake: ScheduleWake::new(|| {}),
        catalog: Arc::new(CatalogControl::new()),
        catalog_thread: None,
        authority_gate: Arc::clone(&gate),
        publisher: Some(publisher),
        thread: Some(scheduler_thread),
    };

    let started = Instant::now();
    handle.stop();
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "one host deadline must cover the publisher and the scheduler waiting on its gate"
    );
    assert!(
        !wait_scheduler_answer
            .recv_timeout(Duration::from_millis(50))
            .unwrap(),
        "closing the authority gate releases the scheduler without admitting new work"
    );

    drop(release_publisher);
}

#[test]
fn a_coalesced_wake_and_due_deadline_service_one_evaluation() {
    let now = Instant::now();
    let mut state = LoopState {
        deadline_ms: Some(42),
        ..LoopState::default()
    };
    assert!(state.take_evaluation(true, 42, now));
    assert_eq!(state.evaluations, 1);
}

#[test]
fn a_slow_schedule_watcher_is_dropped_at_its_bound() {
    let (sink, receive) = mpsc::sync_channel(1);
    let mut watchers = vec![ScheduleWatcher { watcher: 1, sink }];
    let first = ScheduleChange {
        instance: uuid::Uuid::new_v4(),
        generation: 1,
    };
    notify_watchers(&mut watchers, &first);
    assert_eq!(watchers.len(), 1);
    let second = ScheduleChange {
        instance: first.instance,
        generation: 2,
    };
    notify_watchers(&mut watchers, &second);
    assert!(watchers.is_empty());
    assert!(matches!(
        receive.recv().unwrap(),
        ScheduleWatchEvent::Changed(change) if change == first
    ));
}

#[test]
fn cancelling_an_idle_watcher_drops_its_sender_immediately() {
    let (sink, changes) = mpsc::sync_channel(1);
    let mut watchers = vec![ScheduleWatcher { watcher: 7, sink }];

    cancel_watcher(&mut watchers, 7);

    assert!(watchers.is_empty());
    assert!(matches!(changes.recv(), Err(mpsc::RecvError)));
}

#[test]
fn repeated_idle_watcher_cancellation_leaves_the_registry_empty() {
    let mut watchers = Vec::new();
    let mut receivers = Vec::new();
    for watcher in 1..=32 {
        let (sink, receive) = mpsc::sync_channel(1);
        watchers.push(ScheduleWatcher { watcher, sink });
        receivers.push(receive);
    }

    for watcher in 1..=32 {
        cancel_watcher(&mut watchers, watcher);
    }

    assert!(watchers.is_empty());
    assert!(
        receivers
            .into_iter()
            .all(|receive| matches!(receive.recv(), Err(mpsc::RecvError)))
    );
}

#[test]
fn schedule_generation_contains_no_domain_identity() {
    let change = ScheduleChange {
        instance: uuid::Uuid::new_v4(),
        generation: 42,
    };
    let encoded = serde_json::to_value(change).unwrap();
    assert_eq!(encoded.as_object().unwrap().len(), 2);
    assert!(encoded.get("workspace").is_none());
    assert!(encoded.get("schedule").is_none());
    assert!(encoded.get("document").is_none());
}
