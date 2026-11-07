// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::panes::explore::popover_elements::profiles::{MANAGE_PROFILES, MenuAction};

#[test]
fn profile_menu_actions_open_the_existing_pages_and_close_the_menu() {
    for key in [
        MANAGE_PROFILES,
        MenuAction::New.key(),
        MenuAction::Sync.key(),
        MenuAction::Settings.key(),
    ] {
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
        view.explore.toggle_profile_popover();
        let rect = view
            .explore
            .with_page(view.explore_popover_area(), view.viewport, |frame| {
                frame.node(key).unwrap().rect
            })
            .unwrap();
        let (x, y) = rect.centre();
        assert!(view.mouse_down(&mut workspace.ws, x, y, false));
        assert!(!view.explore.popover_open());
        let tab = view.tabs().active_tab();
        if key == MenuAction::Settings.key() {
            assert!(matches!(tab.view, TabView::Settings(_)));
        } else if key == MenuAction::Sync.key() {
            assert_eq!(
                tab.whats_new.as_ref().map(|page| page.page),
                Some(tab::PrivatePage::SyncSettings)
            );
        } else {
            let TabView::Manage(manage) = &tab.view else {
                panic!("Manage page");
            };
            assert_eq!(
                manage.page(),
                Some(if key == MANAGE_PROFILES {
                    crate::manage::Page::Profile(owner.id)
                } else {
                    crate::manage::Page::NewProfile
                })
            );
        }
    }
}
