// SPDX-License-Identifier: MPL-2.0

//! The routing table, driven with a real registry and a clock the test holds.
//!
//! No process is spawned and no socket is opened: `Routed` is a *decision*, and
//! keeping it one is what makes every branch here assertable.

use super::*;

fn target(text: &str) -> Target {
    Target {
        profile: "p".into(),
        workspace: "w".into(),
        document: Some("d".into()),
        node: Some("n".into()),
        trashed: false,
        match_text: Some(text.into()),
        new_window: false,
        kind: koru_agent_wire::TargetKind::Document,
        list: None,
    }
}

fn ids() -> (ProfileId, WorkspaceId) {
    (ProfileId::new(), WorkspaceId::new())
}

#[test]
fn a_shell_that_holds_the_workspace_gets_it() {
    let (profile, workspace) = ids();
    let mut registry = Registry::new();
    registry.hello(ConnectionId(1), Some(7), profile, workspace);

    let routed = Router::new().open(&registry, profile, workspace, target("one"), Instant::now());
    match routed {
        Routed::Deliver {
            connection,
            pid,
            target,
        } => {
            assert_eq!(connection, ConnectionId(1));
            assert_eq!(pid, Some(7));
            assert_eq!(target.match_text.as_deref(), Some("one"));
        }
        other => panic!("expected Deliver, got {other:?}"),
    }
}

#[test]
fn another_workspace_in_the_same_profile_reuses_that_shell() {
    // Enter / click switches the open window in place. Ctrl+Enter is the path
    // that launches; ordinary open must not stack a second process.
    let (profile, current) = ids();
    let target_workspace = WorkspaceId::new();
    let mut registry = Registry::new();
    registry.hello(ConnectionId(1), Some(7), profile, current);

    let routed = Router::new().open(
        &registry,
        profile,
        target_workspace,
        target("one"),
        Instant::now(),
    );
    match routed {
        Routed::Deliver {
            connection,
            pid,
            target,
        } => {
            assert_eq!(connection, ConnectionId(1));
            assert_eq!(pid, Some(7));
            assert_eq!(target.match_text.as_deref(), Some("one"));
            assert!(!target.new_window);
        }
        other => panic!("expected Deliver, got {other:?}"),
    }
}

#[test]
fn new_window_does_not_steal_a_different_workspace() {
    let (profile, current) = ids();
    let target_workspace = WorkspaceId::new();
    let mut registry = Registry::new();
    registry.hello(ConnectionId(1), Some(7), profile, current);
    let mut t = target("one");
    t.new_window = true;

    assert_eq!(
        Router::new().open(&registry, profile, target_workspace, t, Instant::now(),),
        Routed::Launch {
            profile,
            workspace: target_workspace,
        }
    );
}

#[test]
fn nothing_holding_it_launches_once_and_holds_the_target() {
    let (profile, workspace) = ids();
    let registry = Registry::new();
    let mut router = Router::new();
    let now = Instant::now();

    assert_eq!(
        router.open(&registry, profile, workspace, target("one"), now),
        Routed::Launch { profile, workspace }
    );
    assert_eq!(router.pending_count(), 1);

    // ...and the shell that turns up gets it.
    let claimed = router.claim(profile, workspace, now).expect("waiting");
    assert_eq!(claimed.match_text.as_deref(), Some("one"));
    assert_eq!(router.pending_count(), 0, "cleared by the claiming");
}

#[test]
fn a_second_choice_before_the_window_opens_replaces_it_and_spawns_nothing() {
    // **Replace, never queue.** Two results chosen in the seconds a cold launch
    // takes means the person changed their mind; a queue would open both.
    let (profile, workspace) = ids();
    let registry = Registry::new();
    let mut router = Router::new();
    let now = Instant::now();

    assert_eq!(
        router.open(&registry, profile, workspace, target("one"), now),
        Routed::Launch { profile, workspace }
    );
    assert_eq!(
        router.open(&registry, profile, workspace, target("two"), now),
        Routed::AlreadyLaunching,
        "a second spawn would open a second window"
    );
    assert_eq!(router.pending_count(), 1);
    assert_eq!(
        router
            .claim(profile, workspace, now)
            .unwrap()
            .match_text
            .as_deref(),
        Some("two"),
        "the newest wins"
    );
}

#[test]
fn two_workspaces_launch_independently() {
    // The pending set is keyed by the pair, so a result in one workspace does
    // not suppress a launch for another.
    let (profile, workspace) = ids();
    let elsewhere = WorkspaceId::new();
    let registry = Registry::new();
    let mut router = Router::new();
    let now = Instant::now();

    assert!(matches!(
        router.open(&registry, profile, workspace, target("one"), now),
        Routed::Launch { .. }
    ));
    assert!(matches!(
        router.open(&registry, profile, elsewhere, target("two"), now),
        Routed::Launch { .. }
    ));
    assert_eq!(router.pending_count(), 2);
}

#[test]
fn a_target_nobody_came_for_expires_rather_than_waiting_forever() {
    // Without this, the next time that workspace is opened — twenty minutes
    // later, for an unrelated reason — a result nobody remembers choosing
    // appears.
    let (profile, workspace) = ids();
    let registry = Registry::new();
    let mut router = Router::new();
    let now = Instant::now();
    router.open(&registry, profile, workspace, target("one"), now);

    let later = now + LAUNCH_DEADLINE + Duration::from_secs(1);
    assert_eq!(router.expire(later), vec![(profile, workspace)]);
    assert_eq!(router.pending_count(), 0);
    assert!(router.claim(profile, workspace, later).is_none());
}

#[test]
fn a_hello_after_the_deadline_gets_nothing() {
    // The claim checks the deadline itself, because a `Hello` can arrive in the
    // same breath as the sweep and must not win the race by luck.
    let (profile, workspace) = ids();
    let registry = Registry::new();
    let mut router = Router::new();
    let now = Instant::now();
    router.open(&registry, profile, workspace, target("one"), now);
    assert!(
        router
            .claim(
                profile,
                workspace,
                now + LAUNCH_DEADLINE + Duration::from_secs(1)
            )
            .is_none()
    );
}

#[test]
fn asking_again_after_a_launch_never_arrived_starts_another() {
    // An expired entry is not a launch in flight. Reporting `AlreadyLaunching`
    // would leave the person pressing Enter on a result that opens nothing,
    // forever.
    let (profile, workspace) = ids();
    let registry = Registry::new();
    let mut router = Router::new();
    let now = Instant::now();
    router.open(&registry, profile, workspace, target("one"), now);

    let later = now + LAUNCH_DEADLINE + Duration::from_secs(1);
    assert_eq!(
        router.open(&registry, profile, workspace, target("two"), later),
        Routed::Launch { profile, workspace }
    );
}

#[test]
fn a_spawn_that_failed_drops_its_target() {
    // There is no `Hello` coming. Leaving it pending would make the next honest
    // open of that workspace inherit it.
    let (profile, workspace) = ids();
    let registry = Registry::new();
    let mut router = Router::new();
    let now = Instant::now();
    router.open(&registry, profile, workspace, target("one"), now);
    router.launch_failed(profile, workspace);
    assert_eq!(router.pending_count(), 0);
    assert!(router.claim(profile, workspace, now).is_none());
}

#[test]
fn a_shell_that_appears_while_a_launch_is_pending_is_delivered_to_directly() {
    // The window the person already had, arriving in the registry between the
    // spawn and the `Hello`. Routing to it beats waiting for a second window
    // that is still starting.
    let (profile, workspace) = ids();
    let mut registry = Registry::new();
    let mut router = Router::new();
    let now = Instant::now();
    router.open(&registry, profile, workspace, target("one"), now);

    registry.hello(ConnectionId(1), None, profile, workspace);
    assert!(matches!(
        router.open(&registry, profile, workspace, target("two"), now),
        Routed::Deliver { .. }
    ));
}

#[test]
fn the_loop_can_sleep_until_the_earliest_deadline() {
    // The agent waits on this rather than on a clock of its own, which is what
    // keeps a tray-resident process at zero CPU.
    let (profile, workspace) = ids();
    let registry = Registry::new();
    let mut router = Router::new();
    assert!(router.next_deadline().is_none(), "nothing to wake for");

    let now = Instant::now();
    router.open(&registry, profile, workspace, target("one"), now);
    router.open(
        &registry,
        profile,
        WorkspaceId::new(),
        target("two"),
        now + Duration::from_secs(5),
    );
    assert_eq!(router.next_deadline(), Some(now + LAUNCH_DEADLINE));
}
