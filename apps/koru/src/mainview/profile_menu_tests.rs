// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::panes::explore::popover_elements::profiles::{MANAGE_PROFILES, MenuAction};

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
            assert!(frame.node(MenuAction::Sync.key()).is_none());
            frame.node(MANAGE_PROFILES).unwrap().rect
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
    assert_eq!(
        settings.manage.page(),
        Some(crate::manage::Page::Profile(owner.id))
    );
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

/// App settings left the Profile menu for a square of its own on the rail,
/// between Help and the profile square: the app's settings are not a
/// Profile's, and they are now one press away instead of two.
#[test]
fn the_rails_settings_square_sits_between_help_and_the_profile_and_opens_settings() {
    let (mut view, mut workspace) = harness::container();
    if view.explore.apps_rail_rects(view.rail_area()).settings.w <= 0.0 {
        view.toggle_sidebar();
    }
    let rail = view.explore.apps_rail_rects(view.rail_area());
    assert!(rail.settings.w > 0.0, "the square is drawn");
    assert!(rail.help.y < rail.settings.y, "below Help");
    assert!(rail.settings.y < rail.profile.y, "above the profile square");
    assert_eq!(rail.settings.w, rail.help.w);

    let (x, y) = rail.settings.centre();
    assert!(view.mouse_down(&mut workspace.ws, x, y, false));
    assert!(matches!(
        view.tabs().active_tab().view,
        TabView::Settings(_)
    ));
    assert!(!view.explore.popover_open());
}
