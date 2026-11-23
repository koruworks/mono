// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn empty_preedit_retains_owner_and_unacknowledged_stream_rejects_text() {
    let target = ImeTargetId {
        owner: 1,
        control: 2,
    };
    let mut state = State {
        allowed: Some(target),
        ..State::default()
    };
    assert_eq!(state.route(&Ime::Commit("late".into())), None);
    state.route(&Ime::Enabled);
    assert_eq!(
        state.route(&Ime::Preedit("e".into(), Some((1, 1)))),
        Some(target)
    );
    assert_eq!(
        state.route(&Ime::Preedit(String::new(), None)),
        Some(target)
    );
    assert_eq!(state.route(&Ime::Commit("é".into())), Some(target));
    state.route(&Ime::Disabled);
    assert_eq!(state.route(&Ime::Commit("late".into())), None);
}

#[test]
fn captured_owner_never_routes_to_replacement() {
    let old = ImeTargetId {
        owner: 1,
        control: 2,
    };
    let new = ImeTargetId {
        owner: 3,
        control: 2,
    };
    let mut state = State {
        allowed: Some(old),
        owner: Some(old),
        enabled: true,
        ..State::default()
    };
    state.allowed = Some(new);
    assert_eq!(state.route(&Ime::Commit("late".into())), None);
}

#[test]
fn reset_waits_for_new_enabled_without_requiring_disabled_acknowledgement() {
    let target = ImeTargetId {
        owner: 3,
        control: 4,
    };
    let mut state = State {
        allowed: Some(target),
        ..State::default()
    };
    assert_eq!(state.route(&Ime::Commit("late".into())), None);
    state.route(&Ime::Enabled);
    assert_eq!(state.route(&Ime::Commit("new".into())), Some(target));
    state.route(&Ime::Disabled);
    assert_eq!(state.allowed, Some(target));
    state.route(&Ime::Enabled);
    assert_eq!(
        state.route(&Ime::Preedit("next".into(), Some((4, 4)))),
        Some(target)
    );
}

#[test]
fn event_already_entering_app_when_target_changes_cannot_enable_replacement() {
    let target = ImeTargetId {
        owner: 8,
        control: 9,
    };
    let mut state = State {
        allowed: Some(target),
        ..State::default()
    };
    assert_eq!(state.route_after_sync(&Ime::Enabled, true), None);
    assert!(!state.enabled);
    assert_eq!(
        state.route_after_sync(&Ime::Commit("old".into()), true),
        None
    );
    assert_eq!(
        state.route_after_sync(&Ime::Commit("old".into()), false),
        None
    );
    state.route_after_sync(&Ime::Enabled, false);
    assert_eq!(
        state.route_after_sync(&Ime::Commit("new".into()), false),
        Some(target)
    );
}
