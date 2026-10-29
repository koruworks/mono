// SPDX-License-Identifier: MPL-2.0

use koru_agents_ui::PaneStatus;
use koru_session_protocol::SessionId;

use super::*;
use crate::mainview::harness::*;

/// Open a terminal tab bound to a fresh session, as a completed launch leaves
/// one, and return its id.
fn open_terminal(v: &mut MainView) -> SessionId {
    assert!(v.open_session_tab_for_profile(0, None));
    let id = SessionId::mint();
    let tab = v.tabs().active_tab().id;
    let view = v.session_view_by_tab(tab).expect("a session tab");
    view.session = Some(id);
    view.surface.status = PaneStatus::Running;
    id
}

#[test]
fn a_closed_terminal_comes_back_where_the_chord_says() {
    let (mut v, _workspace) = container();
    let id = open_terminal(&mut v);
    let before = v.tabs().len();

    assert!(v.close_requested(v.tabs().active()));
    assert_eq!(v.tabs().len(), before - 1);
    assert!(v.session_tab_for(id).is_none());

    assert!(v.reopen_closed_tab());
    assert_eq!(v.tabs().len(), before);
    assert!(v.session_tab_for(id).is_some(), "the same session");
    assert!(!v.reopen_closed_tab(), "nothing is left to bring back");
}

#[test]
fn the_newest_closed_tab_comes_back_first() {
    let (mut v, _workspace) = container();
    let first = open_terminal(&mut v);
    let second = open_terminal(&mut v);
    assert!(v.close_requested(v.tabs().active()));
    let index = v
        .tabs()
        .iter()
        .position(|tab| tab.id == v.session_tab_for(first).expect("first"))
        .expect("first is open");
    assert!(v.close_requested(index));

    assert!(v.reopen_closed_tab());
    assert!(v.session_tab_for(first).is_some());
    assert!(v.session_tab_for(second).is_none());
    assert!(v.reopen_closed_tab());
    assert!(v.session_tab_for(second).is_some());
}

#[test]
fn a_tab_that_is_already_open_is_not_opened_twice() {
    let (mut v, _workspace) = container();
    let id = open_terminal(&mut v);
    assert!(v.close_requested(v.tabs().active()));
    assert!(v.reopen_closed_tab());
    // Closing it again and reopening it twice finds only one record.
    assert!(v.close_requested(v.tabs().active()));
    assert!(v.reopen_closed_tab());
    assert!(!v.reopen_closed_tab());
    let _ = id;
}

#[test]
fn the_list_is_bounded() {
    let (mut v, _workspace) = container();
    for _ in 0..(KEPT + 5) {
        open_terminal(&mut v);
        assert!(v.close_requested(v.tabs().active()));
    }
    assert_eq!(v.closed_tabs.len(), KEPT);
}
