// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::managed;
use crate::managed_surface::tests::surface;
use uuid::Uuid;

#[test]
fn the_sweep_crosses_once_and_starts_again() {
    assert_eq!(sweep_phase(0), 0.0);
    assert!((sweep_phase(SWEEP_MS / 2) - 0.5).abs() < 1e-6);
    assert_eq!(sweep_phase(SWEEP_MS), 0.0);
    assert!(sweep_phase(-1) > 0.99);
}

#[test]
fn native_status_uses_turns_without_an_ordinary_attempt() {
    let (mut surface, _, _) = surface();
    assert!(surface.attempt.is_none());
    assert_eq!(surface.chat_status(false), ChatStatus::Working);
    assert!(!surface.knows_nothing_runs());
    for state in ["needs_you", "interrupted"] {
        surface.managed.as_mut().unwrap().snapshot.turns[0].state = state.into();
        assert_eq!(surface.chat_status(false), ChatStatus::NeedsYou);
        assert!(!surface.knows_nothing_runs());
    }
    surface.managed.as_mut().unwrap().snapshot.turns.clear();
    assert_eq!(surface.chat_status(false), ChatStatus::Idle);
    assert!(surface.knows_nothing_runs());
    surface.managed.as_mut().unwrap().snapshot.waiting_messages = 1;
    assert!(!surface.is_working(), "queued input is not a running turn");
    assert!(!surface.knows_nothing_runs(), "queued work still exists");
}

fn completed(
    surface: &ChatSurface,
    message: koru_ai_wire::bot_threads::Message,
) -> managed::Refreshed {
    let state = surface.managed.as_ref().unwrap();
    let mut snapshot = state.snapshot.clone();
    snapshot.turns.clear();
    snapshot.waiting_messages = 0;
    managed::Refreshed {
        snapshot,
        after: message.sequence,
        messages: vec![message],
        navigation: Default::default(),
        attention: Default::default(),
        questions: vec![],
        feed: managed::Feed {
            boot: state.boot.unwrap_or_else(Uuid::new_v4),
            cursor: state.cursor,
            gap: false,
            entries: vec![],
        },
    }
}

#[test]
fn native_completion_refreshes_the_host_once_even_when_the_running_poll_was_missed() {
    for saw_running in [true, false] {
        let (mut surface, _, _) = surface();
        if !saw_running {
            surface.managed.as_mut().unwrap().snapshot.turns.clear();
        }
        surface.take_activity();
        surface.take_arrival();
        let message = managed::tests::message(1, Uuid::new_v4());
        let update = completed(&surface, message.clone());
        assert!(surface.managed_refreshed(update));
        assert_eq!(surface.chat_status(false), ChatStatus::Idle);
        assert!(surface.knows_nothing_runs());
        assert!(
            surface.take_activity(),
            "the roster must reread after completion"
        );
        assert!(
            surface.take_arrival(),
            "background replies can become unread"
        );
        assert!(!surface.take_activity());
        assert!(!surface.take_arrival());
        let replay = completed(&surface, message);
        surface.managed_refreshed(replay);
        assert!(
            !surface.take_activity(),
            "a repeated observation is not new work"
        );
        assert!(
            !surface.take_arrival(),
            "a repeated observation is not a new reply"
        );
    }
}
