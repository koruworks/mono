// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::panes::explore::WorkspaceEntry;

/// Five workspaces, four of them pinned, and the open one — `0` — not
/// pinned. The run is the first three pins, then the open workspace.
fn view() -> ExploreView {
    let mut view = ExploreView::new();
    view.workspaces = (0..5)
        .map(|i| {
            let mut row = WorkspaceEntry::new(WorkspaceId::new(), &format!("Workspace {i}"), i);
            row.pinned = i > 0;
            row
        })
        .collect();
    view.current = Some(view.workspaces[0].id);
    view
}

fn rail() -> Rect {
    Rect::new(0.0, 0.0, koru_draw::theme::RAIL_W, 600.0)
}

/// **The run is the saved pins, in their order**, and the open workspace
/// stays where it stands among them — it used to be lifted to the top. One
/// that is not pinned follows the run, and is the one square not carried.
#[test]
fn the_run_is_the_first_three_saved_pins_in_order_and_an_unpinned_open_workspace_follows_it() {
    for current in 0..5 {
        let mut view = view();
        let id = view.workspaces[current].id;
        view.current = Some(id);
        let pins: Vec<_> = view.workspaces[1..4].iter().map(|w| w.id).collect();
        let ids = view.rail_workspace_ids();
        assert_eq!(ids[..3], pins[..], "the pins keep their saved order");
        assert_eq!(ids.iter().filter(|item| **item == id).count(), 1);
        if (1..=3).contains(&current) {
            assert_eq!(ids.len(), 3, "a pinned open workspace is not repeated");
        } else {
            assert_eq!(ids, [&pins[..], &[id]].concat(), "then the open one");
        }
        assert!(view.workspaces[4].pinned, "no flag is rewritten");
        let rects = view.apps_rail_rects(rail());
        let square = rects
            .workspaces
            .iter()
            .find(|(w, _)| *w == Some(id))
            .unwrap()
            .1;
        view.begin_workspace_drag(id, &rects, square.y);
        assert_eq!(
            view.rail_dragging(),
            (1..=3).contains(&current),
            "a pinned square is carried, an unpinned open one is not"
        );
    }
}

#[test]
fn clicks_switch_on_release_and_drag_commits_only_on_release() {
    let mut view = view();
    let original = view.rail_workspace_ids();
    let rects = view.apps_rail_rects(rail());
    let (x, y) = rects.workspaces[0].1.centre();
    assert_eq!(view.apps_rail_mouse_down(rail(), x, y), Outcome::Consumed);
    assert_eq!(
        view.release_workspace_drag(rail()),
        Some(Outcome::SwitchWorkspace(original[0]))
    );
    view.apps_rail_mouse_down(rail(), x, y);
    view.rail_drag_move(rail(), y + 40.0);
    assert_eq!(
        view.rail_workspace_ids(),
        vec![original[1], original[0], original[2], original[3]]
    );
    assert_eq!(
        view.release_workspace_drag(rail()),
        Some(Outcome::ReorderWorkspace {
            workspace: original[0],
            before: Some(original[2])
        })
    );
    assert_eq!(
        view.rail_workspace_ids(),
        original,
        "only controller adoption changes stored order"
    );
}

/// A drop on the last pinned slot names no anchor: the next square there is
/// the unpinned open workspace, which the saved order does not hold.
#[test]
fn a_drop_on_the_last_pinned_slot_anchors_on_no_unpinned_square() {
    let mut view = view();
    let original = view.rail_workspace_ids();
    let rects = view.apps_rail_rects(rail());
    let (x, y) = rects.workspaces[0].1.centre();
    view.apps_rail_mouse_down(rail(), x, y);
    view.rail_drag_move(rail(), 1000.0);
    assert_eq!(
        view.rail_workspace_ids(),
        vec![original[1], original[2], original[0], original[3]],
        "carried to the end of the pins, never past the open workspace"
    );
    assert_eq!(
        view.release_workspace_drag(rail()),
        Some(Outcome::ReorderWorkspace {
            workspace: original[0],
            before: None
        })
    );
}

#[test]
fn escape_or_membership_change_cancels_and_the_unpinned_open_square_is_never_crossed() {
    let mut view = view();
    let original = view.rail_workspace_ids();
    let rects = view.apps_rail_rects(rail());
    let (x, y) = rects.workspaces[1].1.centre();
    view.apps_rail_mouse_down(rail(), x, y);
    view.rail_drag_move(rail(), -1000.0);
    assert_eq!(
        view.rail_workspace_ids()[0],
        original[1],
        "the first slot is a slot like the others"
    );
    view.rail_drag_move(rail(), 1000.0);
    assert_eq!(
        *view.rail_workspace_ids().last().unwrap(),
        original[3],
        "the unpinned open workspace stays after the run"
    );
    view.cancel_rail_drag(rail());
    assert_eq!(view.rail_workspace_ids(), original);
    assert_eq!(view.release_workspace_drag(rail()), None);
    view.apps_rail_mouse_down(rail(), x, y);
    view.workspaces[1].pinned = false;
    view.check_workspace_drag_membership();
    assert!(!view.rail_dragging());
    assert_eq!(view.release_workspace_drag(rail()), None);
}

/// **Switching moves nothing.** Every square is drawn where it was before
/// the switch; only which one is selected changes.
#[test]
fn switching_to_another_pinned_workspace_leaves_every_square_where_it_was() {
    let mut view = view();
    view.current = Some(view.workspaces[1].id);
    let before = view.apps_rail_rects(rail());
    view.current = Some(view.workspaces[3].id);
    let after = view.apps_rail_rects(rail());
    assert_eq!(before.workspaces, after.workspaces);
    assert_eq!(before.workspace_menu, after.workspace_menu);
    let selected: Vec<_> = view
        .rail_model()
        .workspaces
        .iter()
        .filter(|item| item.selected)
        .map(|item| item.hit)
        .collect();
    assert_eq!(
        selected,
        [apps_rail::Hit::Workspace(view.workspaces[3].id)],
        "the open one is the one drawn selected"
    );

    // A press on the open square answers nothing: it is already open.
    let (x, y) = after.workspaces[2].1.centre();
    assert_eq!(view.apps_rail_mouse_down(rail(), x, y), Outcome::Consumed);
    assert_eq!(view.release_workspace_drag(rail()), Some(Outcome::Consumed));
}

#[test]
fn an_unpinned_open_workspace_follows_the_run_and_a_press_on_it_starts_nothing() {
    let mut view = view();
    let open = view.workspaces[0].id;
    let rects = view.apps_rail_rects(rail());
    let (last, square) = *rects.workspaces.last().unwrap();
    assert_eq!(last, Some(open));
    let (x, y) = square.centre();
    assert_eq!(view.apps_rail_mouse_down(rail(), x, y), Outcome::Consumed);
    assert!(!view.rail_dragging(), "it is not carried");
    assert_eq!(view.release_workspace_drag(rail()), None, "nor switched to");
}

#[test]
fn the_move_chords_reorder_the_pins_from_the_first_slot_and_leave_the_open_one() {
    let view = view();
    let pins = view.rail_workspace_ids();
    assert_eq!(view.nudge_workspace(pins[0], true), None, "already first");
    assert_eq!(
        view.nudge_workspace(pins[1], true),
        Some(Outcome::ReorderWorkspace {
            workspace: pins[1],
            before: Some(pins[0])
        }),
        "into the first slot"
    );
    assert_eq!(
        view.nudge_workspace(pins[0], false),
        Some(Outcome::ReorderWorkspace {
            workspace: pins[0],
            before: Some(pins[2])
        })
    );
    assert_eq!(view.nudge_workspace(pins[2], false), None, "already last");
    assert_eq!(
        view.nudge_workspace(pins[3], true),
        None,
        "the unpinned open workspace has no saved place"
    );
}
