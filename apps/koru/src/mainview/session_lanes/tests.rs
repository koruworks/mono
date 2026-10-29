// SPDX-License-Identifier: MPL-2.0

use std::collections::VecDeque;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use koru_agents_ui::PaneStatus;
use koru_session_backend::{BackendCommand, BackendEvent};
use koru_session_protocol::{SessionId, SessionMode};

use super::super::panes::{PanePath, SplitDir};
use super::super::session_backend::SessionBackend;
use super::super::tab::{Tab, TabView};
use super::*;
use crate::mainview::harness::*;

fn drained(commands: &Receiver<BackendCommand>) -> Vec<BackendCommand> {
    let mut all = Vec::new();
    while let Ok(command) = commands.try_recv() {
        all.push(command);
    }
    all
}

fn selects(commands: &[BackendCommand]) -> Vec<SessionId> {
    commands
        .iter()
        .filter_map(|command| match command {
            BackendCommand::Select { session_id, .. } => Some(*session_id),
            _ => None,
        })
        .collect()
}

/// Bind the session tab that was just opened to `id`, as a completed launch
/// leaves one.
fn bind_active_session_tab(v: &mut MainView, id: SessionId) -> u64 {
    let tab = v.tabs().active_tab().id;
    let view = v.session_view_by_tab(tab).expect("a session tab");
    view.session = Some(id);
    view.surface.status = PaneStatus::Running;
    tab
}

/// Two terminals side by side, the right one focused, both measured, with the
/// window's main connection held by the test.
fn two_terminals() -> (
    MainView,
    Receiver<BackendCommand>,
    (SessionId, u64),
    (SessionId, u64),
) {
    let (mut v, _workspace) = container();

    assert!(v.open_session_tab_for_profile(0, None));
    let left_id = SessionId::mint();
    let left = bind_active_session_tab(&mut v, left_id);

    v.panes
        .split(&PanePath::root(), SplitDir::Right, Tab::welcome());
    v.panes.set_focus(PanePath::root().child(1));
    assert!(v.open_session_tab_for_profile(0, None));
    let right_id = SessionId::mint();
    let right = bind_active_session_tab(&mut v, right_id);

    v.relayout();

    // The main connection exists only now, so the tabs above never launched
    // anything, and nothing has settled yet.
    let (sender, main_commands) = koru_session_backend::channel();
    v.session_backend = Some(SessionBackend {
        sender,
        events: Arc::new(Mutex::new(VecDeque::new())),
    });
    v.session_settled = None;
    v.session_attaching = None;
    (v, main_commands, (left_id, left), (right_id, right))
}

#[test]
fn a_second_visible_terminal_gets_its_own_connection() {
    let (mut v, main, (left_id, left), (right_id, right)) = two_terminals();

    v.maintain_sessions();
    let first = drained(&main);
    // The focused terminal is the main connection's; the other one is not
    // left blank — it has a connection of its own.
    assert_eq!(selects(&first), [right_id]);
    assert_eq!(v.session_settled, Some(right));
    assert_eq!(v.session_lanes.len(), 1, "one lane, for the other terminal");
    assert_eq!(v.session_lanes[0].settled, Some(left));
    assert_eq!(v.lane_commands.len(), 1);
    assert_eq!(
        selects(&drained(&v.lane_commands[0])),
        [left_id],
        "and the lane attaches the other session, not the main connection"
    );

    // A pass later changes nothing: no second lane, no repeated attach.
    v.maintain_sessions();
    assert_eq!(v.session_lanes.len(), 1);
    assert!(selects(&drained(&main)).is_empty());
    assert!(selects(&drained(&v.lane_commands[0])).is_empty());
}

#[test]
fn clicking_the_other_terminal_does_not_move_a_connection() {
    let (mut v, main, (left_id, left), (right_id, right)) = two_terminals();
    v.maintain_sessions();
    let _ = drained(&main);
    let _ = drained(&v.lane_commands[0]);

    // Focus moves to the left pane: each terminal keeps its own connection, so
    // nothing detaches and nothing re-attaches (a swap would blank both).
    v.panes.set_focus(PanePath::root().child(0));
    v.maintain_sessions();

    assert_eq!(v.session_settled, Some(right), "the main connection stays");
    assert_eq!(
        v.session_lanes[0].settled,
        Some(left),
        "and so does the lane"
    );
    let main_after = drained(&main);
    let lane_after = drained(&v.lane_commands[0]);
    assert!(selects(&main_after).is_empty() && selects(&lane_after).is_empty());
    assert!(
        !main_after
            .iter()
            .chain(lane_after.iter())
            .any(|command| matches!(command, BackendCommand::Detach | BackendCommand::Close)),
        "{main_after:?} {lane_after:?}"
    );
    let _ = (left_id, right_id);
}

#[test]
fn a_lane_ends_when_its_terminal_leaves_the_screen() {
    let (mut v, main, (_, left), _) = two_terminals();
    v.maintain_sessions();
    let _ = drained(&main);
    let _ = drained(&v.lane_commands[0]);

    // The left pane switches to another tab, so its terminal is hidden.
    let group = v
        .panes
        .group_mut(&PanePath::root().child(0))
        .expect("a group");
    let welcome = group.insert_tab_after_active(Tab::welcome());
    assert!(group.select(welcome), "the welcome tab is now showing");
    let _ = left;
    v.maintain_sessions();

    assert!(
        v.session_lanes.is_empty(),
        "the lane went with its terminal"
    );
    assert!(
        drained(&v.lane_commands[0])
            .iter()
            .any(|command| matches!(command, BackendCommand::Close)),
        "and its connection was told to close"
    );
}

/// A lane's events go through the same handler the main connection's do, so an
/// attach on the lane binds the lane's own terminal.
#[test]
fn a_lane_attach_answers_the_terminal_that_asked() {
    let (mut v, main, (left_id, left), _) = two_terminals();
    v.maintain_sessions();
    let _ = drained(&main);
    let _ = drained(&v.lane_commands[0]);

    v.session_lanes[0]
        .backend
        .as_ref()
        .expect("a lane connection")
        .events
        .lock()
        .unwrap()
        .push_back(BackendEvent::Attached {
            session_id: left_id,
            mode: SessionMode::RawPty,
            epoch: 1,
        });
    v.maintain_sessions();

    // A raw attach waits for its first sized snapshot, and asks the lane's
    // connection to resize to the grid it will arrive at — not the main one.
    assert!(
        drained(&v.lane_commands[0])
            .iter()
            .any(|command| matches!(command, BackendCommand::Intent(_))),
        "the lane's own connection is asked to size the terminal"
    );
    assert!(
        drained(&main).is_empty(),
        "the main connection is not asked anything"
    );
    let _ = left;
}

#[test]
fn a_tab_the_catalog_says_is_alive_stays_attached_through_a_lane() {
    let (mut v, _main, (left_id, left), _) = two_terminals();
    v.maintain_sessions();
    v.session_lanes[0].attached = Some(left);
    assert!(v.tab_attached_anywhere(left));
    assert!(v.controller_sender_for(left_id).is_some());
    let _ = TabView::Welcome;
}
