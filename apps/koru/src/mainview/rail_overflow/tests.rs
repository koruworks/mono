// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::mainview::harness::container;
use crate::panes::explore::NavigatorMode;
use crate::panes::sidebar::apps_rail::Hit;
use koru_core::WorkspaceEntry;

fn prepared() -> MainView {
    let (mut view, _) = container();
    if !view.sidebar.is_open() {
        view.toggle_sidebar();
    }
    let entries: Vec<_> = (0..4)
        .map(|i| {
            let mut entry = WorkspaceEntry::new(WorkspaceId::new(), &format!("Workspace {i}"), i);
            entry.pinned = i != 0;
            entry
        })
        .collect();
    let current = entries[0].id;
    view.explore
        .set_context(vec![], ProfileId::new(), entries, current);
    view.set_viewport(720.0, 1000.0);
    view
}

#[test]
fn resizing_folds_focus_to_overflow_then_restores_the_same_view() {
    koru_features::testing::with(koru_features::Features::ALL_ON, || {
        let mut view = prepared();
        let wanted = *view.explore.rail_modes().last().unwrap();
        let index = view
            .explore
            .rail_controls(view.rail_area())
            .iter()
            .position(|hit| *hit == Hit::App(wanted))
            .unwrap();
        view.chrome_focus = Some(chrome_focus::ChromeStop::Apps(index));
        let order = view.explore.rail_order_storage();
        view.set_viewport(720.0, 600.0);
        assert_eq!(view.focused_rail_identity(), Some(Hit::Overflow));
        assert!(view.open_rail_overflow());
        let menu = view.menu.as_ref().unwrap();
        assert!(
            menu.items()
                .iter()
                .any(|action| matches!(action, MenuAction::RailView {mode, ..} if *mode == wanted))
        );
        view.set_viewport(720.0, 1000.0);
        assert!(view.menu.is_none());
        assert_eq!(view.focused_rail_identity(), Some(Hit::App(wanted)));
        assert_eq!(view.explore.rail_order_storage(), order);
    });
}

#[test]
fn promoted_keyboard_move_enters_the_saved_visible_range_without_changing_shortcut_rules() {
    koru_features::testing::with(koru_features::Features::ALL_ON, || {
        let mut view = prepared();
        view.set_viewport(720.0, 600.0);
        let modes = view.explore.rail_modes();
        let active = *modes.last().unwrap();
        assert_ne!(active, NavigatorMode::Documents);
        view.show_navigator(active);
        let projection = view.explore.view_projection(view.rail_area().h);
        assert_eq!(projection.promoted, Some(active));
        let slot = view
            .explore
            .nudge_rail_view(view.rail_area(), modes.len() - 1, true)
            .unwrap();
        assert_eq!(
            view.explore.view_projection(view.rail_area().h).promoted,
            None
        );
        assert_eq!(view.explore.rail_mode_at(slot), Some(active));
        assert_eq!(
            view.explore.rail_shortcut(active),
            koru_keys::rail_slot(slot)
        );
    });
}

#[test]
fn unfolding_the_focused_menu_item_returns_focus_to_that_view() {
    koru_features::testing::with(koru_features::Features::ALL_ON, || {
        let mut view = prepared();
        view.set_viewport(720.0, 600.0);
        assert!(view.open_rail_overflow());
        let menu = view.menu.as_mut().unwrap();
        let index = menu.items().len() - 1;
        let MenuAction::RailView { mode, .. } = menu.items()[index] else {
            unreachable!()
        };
        menu.hover = Some(index);
        view.set_viewport(720.0, 1000.0);
        assert!(view.menu.is_none());
        assert_eq!(view.focused_rail_identity(), Some(Hit::App(mode)));
    });
}
