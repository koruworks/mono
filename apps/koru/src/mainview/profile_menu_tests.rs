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
            let TabView::Manage(manage) = &tab.view else {
                panic!("Global sync must open Manage, not a document");
            };
            assert_eq!(manage.page(), Some(crate::manage::Page::Devices));
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

#[test]
fn workspace_channel_icons_open_their_manage_pages_and_reuse_the_tab() {
    use crate::manage::Page;
    use koru_agent_wire::{AccountProfileCloud, AccountStage, AccountState, CloudConnectionStage};
    use koru_draw::el::Key;

    for (devices, cloud) in [(false, false), (true, false), (false, true), (true, true)] {
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
        view.explore.set_linked_count(usize::from(devices));
        if cloud {
            view.explore.set_account_state(&AccountState {
                stage: AccountStage::SignedIn,
                cloud_profiles: vec![AccountProfileCloud {
                    discovery: None,
                    profile_id: owner.id.to_string(),
                    stage: CloudConnectionStage::Retrying,
                }],
                ..AccountState::default()
            });
        }
        let before = view.tabs().len();
        let destinations: Vec<_> = [
            (devices || !cloud, "workspace-sync", Page::Devices),
            (cloud, "workspace-cloud", Page::Cloud),
        ]
        .into_iter()
        .filter(|(shown, _, _)| *shown)
        .collect();
        for profile_menu in [false, true, false, true] {
            for &(_, part, page) in &destinations {
                view.select(0);
                if profile_menu {
                    view.explore.toggle_profile_popover();
                } else {
                    view.explore.toggle_workspace_menu();
                }
                let key = Key::surface("explore-popover", part);
                let (x, y) = view
                    .explore
                    .with_page(view.explore_popover_area(), view.viewport, |frame| {
                        let rect = frame.node(key).expect("configured channel control").rect;
                        assert_eq!(frame.hit_at(rect.centre().0, rect.centre().1), Some(key));
                        rect.centre()
                    })
                    .unwrap();
                assert!(view.mouse_down(&mut workspace.ws, x, y, false));
                assert!(!view.explore.popover_open());
                let tab = view.tabs().active_tab();
                let TabView::Manage(manage) = &tab.view else {
                    panic!("channel icons must open Manage, not a document");
                };
                assert_eq!(manage.page(), Some(page));
                assert!(tab.document.is_none());
                assert!(tab.whats_new.is_none());
                assert_eq!(view.tabs().len(), before + 1, "reuse the Manage tab");
            }
        }
    }
}
