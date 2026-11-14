// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::actor::tests::routine::Fixture;

fn effects() -> (ActorEffects, mpsc::Receiver<ActorCommand>) {
    let (actor, inbox) = mpsc::channel();
    (
        ActorEffects::new(actor, Arc::new(AtomicBool::new(false))),
        inbox,
    )
}
fn future() -> Instant {
    Instant::now() + Duration::from_secs(2)
}
fn start(e: &mut ActorEffects, f: &Fixture, deadline: Instant) -> PortResult<ScheduleAdmission> {
    e.begin_routine(
        &f.prepared,
        &f.admission,
        &f.proof,
        f.scope.clone(),
        f.prepared.submission.request.scope.clone(),
        deadline,
    )
}
fn held_reply(
    f: &mut Fixture,
    e: &mut ActorEffects,
) -> mpsc::Receiver<PortResult<ScheduleAdmission>> {
    let (reply, answer) = mpsc::sync_channel(1);
    e.routine = Some(PendingRoutine {
        run: f.prepared.ids.run,
        release: None,
    });
    f.handle(f.command(future(), Arc::new(AtomicBool::new(false)), reply));
    assert!(f.held());
    answer
}

#[test]
fn routine_rendezvous_real_actor_grant_blocks_reacquire_until_exact_release_ack() {
    let mut f = Fixture::new();
    let (mut e, inbox) = effects();
    let p = f.prepared.clone();
    let a = f.admission.clone();
    let proof = f.proof.clone();
    let scope = f.scope.clone();
    let task = std::thread::spawn(move || {
        let result = e.begin_routine(
            &p,
            &a,
            &proof,
            scope,
            p.submission.request.scope.clone(),
            future(),
        );
        (e, result)
    });
    f.handle(inbox.recv_timeout(Duration::from_secs(2)).unwrap());
    let (mut e, result) = task.join().unwrap();
    assert_eq!(result.unwrap(), ScheduleAdmission::Granted);
    assert!(f.held());
    assert!(start(&mut e, &f, future()).is_err());
    assert!(
        e.begin(RunId::mint(), f.scope.clone(), WorkspaceScope::Temporary)
            .is_err()
    );
    assert!(inbox.try_recv().is_err());
    assert!(
        e.release_routine(f.prepared.ids.run, Instant::now())
            .is_err()
    );
    f.handle(inbox.try_recv().unwrap());
    e.release_routine(f.prepared.ids.run, future()).unwrap();
    assert!(!f.held());
    assert!(e.routine.is_none());
}

#[test]
fn routine_rendezvous_late_successful_send_cancels_and_retains_cleanup_before_any_launch() {
    for stopped in [false, true] {
        let mut f = Fixture::new();
        let (mut e, inbox) = effects();
        // The actual actor has successfully sent Granted before cancellation.
        let answer = held_reply(&mut f, &mut e);
        let cancelled = Arc::new(AtomicBool::new(false));
        e.stopped.store(stopped, Ordering::Release);
        let deadline = if stopped { future() } else { Instant::now() };
        assert!(
            e.finish_routine_begin(f.prepared.ids.run, deadline, cancelled.clone(), answer)
                .is_err()
        );
        assert!(cancelled.load(Ordering::Acquire));
        assert!(e.routine.is_some());
        assert!(start(&mut e, &f, future()).is_err());
        assert!(
            e.begin(RunId::mint(), f.scope.clone(), WorkspaceScope::Temporary)
                .is_err()
        );
        let claim = koru_delegate_sqlite::SlotClaim {
            schedule: f.prepared.slot.schedule,
            occurrence_ms: 900,
            revision: 1,
            run: f.prepared.ids.run,
            state: koru_delegate_sqlite::SlotState::Claimed,
            busy_holder: None,
            claimed_at_ms: 1000,
        };
        assert!(
            e.reconcile(claim.clone(), f.prepared.submission.request.scope.clone())
                .is_err()
        );
        assert!(
            e.finish(
                f.prepared.ids.run,
                claim,
                koru_delegate_scope::ResolvedScope {
                    identity: f.scope.clone(),
                    directory: std::path::PathBuf::from("C:/routine-fixture")
                },
                f.prepared.submission.request.scope.clone()
            )
            .is_err()
        );
        let release = inbox.try_recv().unwrap();
        assert!(matches!(release, ActorCommand::RoutineRelease { .. }));
        assert!(inbox.try_recv().is_err());
        f.handle(release);
        // Cleanup remains legal after Stop, but only its acknowledgement clears.
        e.release_routine(f.prepared.ids.run, future()).unwrap();
        assert!(e.routine.is_none());
        assert!(!f.held());
    }
}

#[test]
fn routine_rendezvous_release_timeout_preserves_receiver_and_rejects_foreign_run() {
    let mut f = Fixture::new();
    let (mut e, inbox) = effects();
    let answer = held_reply(&mut f, &mut e);
    assert_eq!(answer.recv().unwrap().unwrap(), ScheduleAdmission::Granted);
    let run = f.prepared.ids.run;
    assert!(e.release_routine(run, Instant::now()).is_err());
    let release = inbox.try_recv().unwrap();
    assert!(e.release_routine(run, Instant::now()).is_err());
    assert!(e.release_routine(RunId::mint(), future()).is_err());
    assert!(inbox.try_recv().is_err());
    assert!(start(&mut e, &f, future()).is_err());
    f.handle(release);
    e.release_routine(run, future()).unwrap();
    assert!(e.routine.is_none());
    assert!(!f.held());
}

#[test]
fn routine_rendezvous_lost_release_reply_retries_without_reacquiring() {
    let mut f = Fixture::new();
    let (mut e, inbox) = effects();
    let answer = held_reply(&mut f, &mut e);
    answer.recv().unwrap().unwrap();
    let run = f.prepared.ids.run;
    assert!(e.release_routine(run, Instant::now()).is_err());
    let ActorCommand::RoutineRelease {
        run: observed,
        reply: lost,
    } = inbox.try_recv().unwrap()
    else {
        panic!("release")
    };
    let (replacement, answer) = mpsc::sync_channel(1);
    f.handle(ActorCommand::RoutineRelease {
        run: observed,
        reply: replacement,
    });
    answer.recv().unwrap().unwrap();
    drop(lost);
    assert!(!f.held());
    assert!(e.release_routine(run, future()).is_err());
    assert!(e.routine.is_some());
    assert!(start(&mut e, &f, future()).is_err());
    assert!(e.release_routine(run, Instant::now()).is_err());
    f.handle(inbox.try_recv().unwrap());
    e.release_routine(run, future()).unwrap();
    assert!(e.routine.is_none());
}

#[test]
fn routine_rendezvous_unknown_begin_queues_cleanup_and_never_assumes_absence() {
    let f = Fixture::new();
    let (mut e, inbox) = effects();
    let run = f.prepared.ids.run;
    let (_reply, answer) = mpsc::sync_channel(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    e.routine = Some(PendingRoutine { run, release: None });
    assert!(
        e.finish_routine_begin(run, Instant::now(), cancelled.clone(), answer)
            .is_err()
    );
    assert!(cancelled.load(Ordering::Acquire));
    assert!(
        matches!(inbox.try_recv().unwrap(),ActorCommand::RoutineRelease{run:held,..} if held==run)
    );
    assert!(start(&mut e, &f, future()).is_err());
}

#[test]
fn routine_rendezvous_expired_stopped_and_closed_send_do_not_enqueue_or_retain_begin() {
    let f = Fixture::new();
    let (mut e, inbox) = effects();
    assert!(start(&mut e, &f, Instant::now()).is_err());
    assert!(inbox.try_recv().is_err());
    e.stopped.store(true, Ordering::Release);
    assert!(start(&mut e, &f, future()).is_err());
    assert!(inbox.try_recv().is_err());
    assert!(e.routine.is_none());
    let (mut closed, inbox) = effects();
    drop(inbox);
    assert!(start(&mut closed, &f, future()).is_err());
    assert!(closed.routine.is_none());
}

#[test]
fn routine_rendezvous_control_stop_sets_shared_cancellation_before_queued_stop() {
    let (commands, inbox) = mpsc::channel();
    let stopped = Arc::new(AtomicBool::new(false));
    let control = Control {
        commands,
        wake: ScheduleWake::new(|| {}),
        stopped: stopped.clone(),
    };
    control.stop();
    assert!(stopped.load(Ordering::Acquire));
    assert!(matches!(inbox.try_recv().unwrap(), Command::Stop));
}
