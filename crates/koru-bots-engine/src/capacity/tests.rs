// SPDX-License-Identifier: MPL-2.0
use super::*;
fn request(bot: u128, thread: u128, time: u64) -> Waiting {
    Waiting {
        turn: Uuid::new_v4(),
        session: SessionKey {
            bot: Uuid::from_u128(bot),
            thread: Uuid::from_u128(thread),
        },
        queued_at_ms: time,
        routine_due_ms: None,
    }
}
fn ready(since_ms: u64) -> Idle {
    Idle {
        since_ms,
        resume_ready: true,
        state_saved: true,
        ..Idle::default()
    }
}
fn start(s: &mut Scheduler, r: Waiting, at: u64) {
    s.enqueue(r.clone()).unwrap();
    assert_eq!(
        s.next(at),
        Some(Action::Start {
            request: r,
            reuse: false
        })
    );
}
#[test]
fn capacity_one_yields_main_then_person_and_due_routine_run_on_time() {
    let mut s = Scheduler::new(1);
    let main = request(1, 1, 0);
    start(&mut s, main.clone(), 0);
    assert!(s.completed(main.session, main.turn, ready(1)));
    let person = request(2, 2, 10);
    let mut routine = request(3, 3, 5);
    routine.routine_due_ms = Some(5);
    s.enqueue(routine.clone()).unwrap();
    s.enqueue(person.clone()).unwrap();
    assert_eq!(
        s.next(10),
        Some(Action::End {
            session: main.session
        })
    );
    assert_eq!(s.occupied(), 1);
    assert_eq!(
        s.next(11),
        None,
        "stop request is not proof of process death"
    );
    s.ended(main.session);
    assert_eq!(
        s.next(12),
        Some(Action::Start {
            request: person.clone(),
            reuse: false
        })
    );
    assert!(s.completed(person.session, person.turn, ready(13)));
    assert_eq!(
        s.next(14),
        Some(Action::End {
            session: person.session
        })
    );
    s.ended(person.session);
    assert_eq!(
        s.next(15),
        Some(Action::Start {
            request: routine,
            reuse: false
        })
    );
}
#[test]
fn every_blocker_prevents_yield_and_running_work_is_never_a_victim() {
    for blocked in [
        Idle {
            offered_or_unknown_delivery: true,
            ..ready(0)
        },
        Idle {
            leased_or_unresolved_operation: true,
            ..ready(0)
        },
        Idle {
            resume_ready: false,
            ..ready(0)
        },
        Idle {
            state_saved: false,
            ..ready(0)
        },
    ] {
        let mut s = Scheduler::new(1);
        let first = request(1, 1, 0);
        start(&mut s, first.clone(), 0);
        s.enqueue(request(2, 2, 1)).unwrap();
        assert_eq!(s.next(1), None);
        s.completed(first.session, first.turn, blocked);
        assert_eq!(s.next(2), None);
    }
}
#[test]
fn fairness_expiry_and_capacity_reduction_preserve_live_work() {
    let mut s = Scheduler::new(2);
    let a = request(1, 1, 0);
    start(&mut s, a.clone(), 0);
    let a2 = request(1, 2, 1);
    let b = request(2, 3, 2);
    s.enqueue(a2.clone()).unwrap();
    s.enqueue(b.clone()).unwrap();
    assert_eq!(
        s.next(2),
        Some(Action::Start {
            request: b,
            reuse: false
        })
    );
    s.set_capacity(1);
    assert_eq!(s.next(3), None);
    assert_eq!(s.occupied(), 2);
    let mut late = request(3, 4, 0);
    late.routine_due_ms = Some(0);
    s.enqueue(late.clone()).unwrap();
    assert_eq!(
        s.next(ROUTINE_GRACE_MS + 1),
        Some(Action::Missed { turn: late.turn })
    );
    assert_eq!(s.queued(), 1);
}
#[test]
fn idle_expiry_side_threads_and_stale_completions() {
    let mut s = Scheduler::new(1);
    let a = request(1, 1, 0);
    start(&mut s, a.clone(), 0);
    assert!(!s.completed(a.session, Uuid::new_v4(), ready(0)));
    s.completed(a.session, a.turn, ready(10));
    assert_eq!(s.next(IDLE_WINDOW_MS + 9), None);
    assert_eq!(
        s.next(IDLE_WINDOW_MS + 10),
        Some(Action::End { session: a.session })
    );
    s.ended(a.session);
    let b = request(1, 2, 1);
    start(&mut s, b.clone(), 1);
    s.completed(
        b.session,
        b.turn,
        Idle {
            side_thread: true,
            ..ready(2)
        },
    );
    assert_eq!(s.next(2), Some(Action::End { session: b.session }));
}
#[test]
fn installed_ram_rounds_down_and_never_yields_zero() {
    assert_eq!(default_capacity(0), 1);
    assert_eq!(default_capacity(7 * 1024 * 1024 * 1024), 3);
}

#[test]
fn a_nonresumable_session_gets_the_idle_window_before_reconstruction() {
    let mut s = Scheduler::new(1);
    let a = request(1, 1, 0);
    start(&mut s, a.clone(), 0);
    s.completed(
        a.session,
        a.turn,
        Idle {
            since_ms: 10,
            side_thread: true,
            ..Idle::default()
        },
    );
    assert_eq!(s.next(10), None);
    assert_eq!(
        s.next(IDLE_WINDOW_MS + 10),
        Some(Action::End { session: a.session })
    );
}
