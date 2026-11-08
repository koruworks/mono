// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::panes::explore::WorkspaceEntry;

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

#[test]
fn current_is_first_and_first_three_saved_pins_are_deduplicated_without_rewriting_flags() {
    for current in 0..5 {
        let mut view = view();
        let id = view.workspaces[current].id;
        view.current = Some(id);
        let ids = view.rail_workspace_ids();
        assert_eq!(ids[0], id);
        assert_eq!(ids.iter().filter(|item| **item == id).count(), 1);
        assert_eq!(ids.len(), if (1..=3).contains(&current) { 3 } else { 4 });
        assert!(view.workspaces[4].pinned);
        let rects = view.apps_rail_rects(rail());
        view.begin_workspace_drag(id, &rects, rects.workspaces[0].1.y);
        assert!(!view.rail_dragging(), "current is inert");
    }
}

#[test]
fn clicks_switch_on_release_and_drag_commits_only_on_release() {
    let mut view = view();
    let original = view.rail_workspace_ids();
    let rects = view.apps_rail_rects(rail());
    let (x, y) = rects.workspaces[1].1.centre();
    assert_eq!(view.apps_rail_mouse_down(rail(), x, y), Outcome::Consumed);
    assert_eq!(
        view.release_workspace_drag(rail()),
        Some(Outcome::SwitchWorkspace(original[1]))
    );
    view.apps_rail_mouse_down(rail(), x, y);
    view.rail_drag_move(rail(), y + 40.0);
    assert_eq!(
        view.rail_workspace_ids(),
        vec![original[0], original[2], original[1], original[3]]
    );
    assert_eq!(
        view.release_workspace_drag(rail()),
        Some(Outcome::ReorderWorkspace {
            workspace: original[1],
            before: Some(original[3])
        })
    );
    assert_eq!(
        view.rail_workspace_ids(),
        original,
        "only controller adoption changes stored order"
    );
}

#[test]
fn escape_or_membership_change_cancels_and_the_current_square_is_never_crossed() {
    let mut view = view();
    let original = view.rail_workspace_ids();
    let rects = view.apps_rail_rects(rail());
    let (x, y) = rects.workspaces[2].1.centre();
    view.apps_rail_mouse_down(rail(), x, y);
    view.rail_drag_move(rail(), -1000.0);
    assert_eq!(view.rail_workspace_ids()[0], original[0]);
    view.cancel_rail_drag(rail());
    assert_eq!(view.rail_workspace_ids(), original);
    assert_eq!(view.release_workspace_drag(rail()), None);
    view.apps_rail_mouse_down(rail(), x, y);
    view.workspaces[1].pinned = false;
    view.check_workspace_drag_membership();
    assert!(!view.rail_dragging());
    assert_eq!(view.release_workspace_drag(rail()), None);
}
