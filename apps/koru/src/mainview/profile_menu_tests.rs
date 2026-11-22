// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::panes::explore::popover_elements::profiles::MenuAction;

#[test]
fn opening_another_profile_in_a_window_does_not_switch_the_source() {
    let (mut view, workspace) = harness::container();
    let destination = ProfileId::new();
    let before = view.explore.profile().map(|p| p.id);
    let tab = view.tabs().active_tab().id;
    view.pane_requests.clear();
    assert!(view.apply(
        &workspace.ws,
        crate::panes::Outcome::OpenProfileInNewWindow(destination)
    ));
    assert!(
        matches!(view.pane_requests.pop_front(), Some(PaneRequest::OpenProfileInNewWindow(id)) if id == destination)
    );
    assert_eq!(view.explore.profile().map(|p| p.id), before);
    assert_eq!(view.tabs().active_tab().id, tab);
}

#[test]
fn profile_menu_actions_open_the_existing_pages_and_close_the_menu() {
    let (mut view, mut workspace) = harness::container();
    let owner = ProfileSummary {
        id: ProfileId::new(),
        name: "Owner".into(),
        email: String::new(),
        avatar: None,
        icon: None,
        color: None,
        account: None,
        folder: String::new(),
    };
    view.set_pane_context(vec![owner.clone()], owner.id, vec![], workspace.ws.id());
    view.open_settings_tab();
    let settings_id = view.tabs().active_tab().id;
    view.explore.toggle_profile_popover();
    let rect = view
        .explore
        .with_page(view.explore_popover_area(), view.viewport, |frame| {
            frame.node(MenuAction::Sync.key()).unwrap().rect
        })
        .unwrap();
    let (x, y) = rect.centre();
    assert!(view.mouse_down(&mut workspace.ws, x, y, false));
    assert!(!view.explore.popover_open());
    assert_eq!(view.tabs().active_tab().id, settings_id);
    let TabView::Settings(settings) = &view.tabs().active_tab().view else {
        panic!("Settings tab")
    };
    assert!(settings.manage_visible());
    assert_eq!(settings.manage.page(), Some(crate::manage::Page::Cloud));
}

/// Management destinations and preferences share one Settings tab.
#[test]
fn linked_devices_and_preferences_share_one_settings_tab() {
    let (mut view, _workspace) = harness::container();
    view.open_settings_tab();
    let id = view.tabs().active_tab().id;
    view.open_manage(crate::manage::Page::Devices);
    assert_eq!(view.tabs().active_tab().id, id);
    assert_eq!(view.tabs().active_tab().label, "Settings");
    let TabView::Settings(settings) = &mut view.tabs_mut().active_mut().view else {
        panic!("Settings tab")
    };
    assert!(settings.manage_visible());
    assert_eq!(settings.manage.page(), Some(crate::manage::Page::Devices));
    settings.show_page(koru_settings_ui::PageId::Appearance);
    assert!(!settings.manage_visible());
}

/// Settings is a fixed action below the profile list.
#[test]
fn settings_is_reachable_from_the_profile_menu() {
    let (mut view, mut workspace) = harness::container();
    view.explore
        .open_popover(crate::panes::explore::PopoverKind::Profiles);
    let rect = view
        .explore
        .with_page(view.explore_popover_area(), view.viewport, |frame| {
            frame.node(MenuAction::Settings.key()).unwrap().rect
        })
        .unwrap();
    let (x, y) = rect.centre();
    assert!(view.mouse_down(&mut workspace.ws, x, y, false));
    assert!(matches!(
        view.tabs().active_tab().view,
        TabView::Settings(_)
    ));
    assert!(!view.explore.popover_open());
}

#[test]
fn hovering_help_keeps_the_profile_menu_and_crossing_the_gap_keeps_the_submenu() {
    use crate::panes::explore::PopoverKind;
    let (mut view, _workspace) = harness::container();
    view.explore.open_popover(PopoverKind::Profiles);
    let area = view.explore_popover_area();
    let parent = view.explore.popover_panel(area, view.viewport).unwrap();
    let trigger = view
        .explore
        .with_page(area, view.viewport, |frame| {
            frame.node(MenuAction::Help.key()).unwrap().rect
        })
        .unwrap();
    let (x, y) = trigger.centre();
    assert!(view.mouse_move(x, y));
    assert!(view.explore.popover_is(PopoverKind::Help));
    assert_eq!(
        view.explore.help_parent_panel(area, view.viewport),
        Some(parent)
    );
    let child = view.explore.popover_panel(area, view.viewport).unwrap();
    assert!(child.x > parent.x + parent.w);
    assert!((child.y + child.h - parent.y - parent.h).abs() < 0.01);
    view.explore
        .with_page(area, view.viewport, |frame| {
            let feedback = frame
                .node(crate::panes::explore::help::Action::Feedback.key())
                .unwrap()
                .rect;
            assert_eq!(
                feedback.h, trigger.h,
                "both menus use the same action-row height"
            );
            assert_eq!(
                feedback.x - child.x,
                trigger.x - parent.x,
                "hover plates have the same inset"
            );
            assert!(feedback.x + feedback.w < child.x + child.w);
        })
        .unwrap();
    let gap_x = (parent.x + parent.w + child.x) * 0.5;
    assert!(view.popover_hit(gap_x, y));
    view.mouse_move(gap_x, y);
    assert!(view.explore.popover_is(PopoverKind::Help));
    let (x, y) = child.centre();
    view.mouse_move(x, y);
    assert!(view.explore.popover_is(PopoverKind::Help));
    view.mouse_move(parent.x + 16.0, parent.y + 16.0);
    assert!(view.explore.popover_is(PopoverKind::Profiles));
    assert!(
        view.explore
            .help_parent_panel(area, view.viewport)
            .is_none()
    );
}
