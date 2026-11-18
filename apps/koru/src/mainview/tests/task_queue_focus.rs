// SPDX-License-Identifier: MPL-2.0

//! Existing shortcuts must transfer the real keyboard to the Tasks queues.
use super::*;
use crate::panes::explore::rail::{Bucket, RailTarget};
use winit::keyboard::{Key, ModifiersState, NamedKey};

fn chord(view: &mut MainView, workspace: &mut Workspace, key: &str, mods: ModifiersState) {
    assert!(View::key(
        view,
        &mut workspace.ws,
        &Key::Character(key.into()),
        None,
        mods
    ));
}

fn next_queue(view: &mut MainView, workspace: &mut Workspace) {
    assert!(View::key(
        view,
        &mut workspace.ws,
        &Key::Named(NamedKey::ArrowDown),
        None,
        ModifiersState::empty()
    ));
    assert!(View::key(
        view,
        &mut workspace.ws,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty()
    ));
    let TabView::Tasks(page) = &view.tabs().active_tab().view else {
        panic!("queue activation must open the Tasks page");
    };
    assert_eq!(page.target(), RailTarget::Bucket(Bucket::Next));
    assert_eq!(
        view.explore.rail().selected(),
        Some(RailTarget::Bucket(Bucket::Next))
    );
}

#[test]
fn tasks_sidebar_chord_focuses_toggles_and_navigates_from_open_or_closed() {
    for initially_open in [false, true] {
        let (mut view, mut workspace, _, _) = on_a_task_row(Vec::new());
        if view.sidebar.is_open() != initially_open {
            view.toggle_sidebar();
        }
        // The fixture's press on Today leaves the keyboard on that row, as a
        // press on a sidebar row does; start from the page holding it.
        view.explore.clear_focus();
        assert!(!view.explore.has_focus());
        let mods = koru_keys::PRIMARY_SHIFT;
        chord(&mut view, &mut workspace, "e", mods);
        assert!(view.sidebar.is_open() && view.explore.has_focus());
        assert_eq!(
            view.explore.rail().selected(),
            Some(RailTarget::Bucket(Bucket::Today))
        );
        chord(&mut view, &mut workspace, "e", mods);
        assert!(!view.sidebar.is_open() && !view.explore.has_focus());
        chord(&mut view, &mut workspace, "e", mods);
        assert!(view.sidebar.is_open() && view.explore.has_focus());
        next_queue(&mut view, &mut workspace);
    }
}

#[test]
fn alt_three_enters_queue_navigation_from_a_document_with_open_or_closed_sidebar() {
    for initially_open in [false, true] {
        let (mut view, mut workspace) = on_document();
        if view.sidebar.is_open() != initially_open {
            view.toggle_sidebar();
        }
        view.explore.clear_focus();
        chord(&mut view, &mut workspace, "3", koru_keys::VIEW);
        assert!(view.sidebar.is_open() && view.explore.showing_tasks());
        assert!(view.explore.has_focus());
        assert!(matches!(view.tabs().active_tab().view, TabView::Leaf(_)));
        next_queue(&mut view, &mut workspace);
    }
}

#[test]
fn task_under_caret_reveal_keeps_keyboard_on_the_task_page() {
    let (mut view, mut workspace, _) = a_focused_document_task();
    chord(&mut view, &mut workspace, "e", koru_keys::PRIMARY_SHIFT);
    assert!(matches!(view.tabs().active_tab().view, TabView::Tasks(_)));
    assert!(view.sidebar.is_open() && view.explore.showing_tasks());
    assert!(
        !view.explore.has_focus(),
        "the existing task-row reveal must retain page keyboard ownership"
    );
}
