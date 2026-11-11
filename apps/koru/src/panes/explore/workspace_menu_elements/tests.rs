// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::panes::explore::{PopoverKind, PopoverPart, PopoverRow};
use koru_core::{ProfileId, ProfileSummary, WorkspaceEntry, WorkspaceId};
use koru_draw::geometry::Rect;
use std::time::{Duration, Instant};

fn opened() -> ExploreView {
    let mut view = ExploreView::new();
    let profile = ProfileSummary {
        id: ProfileId::new(),
        name: "Personal".into(),
        email: String::new(),
        avatar: None,
        icon: None,
        color: None,
        account: None,
        folder: String::new(),
    };
    let workspace = WorkspaceEntry::new(WorkspaceId::new(), "Work", 0);
    view.set_context(
        vec![profile.clone()],
        profile.id,
        vec![workspace.clone()],
        Some(workspace.id),
    );
    view.toggle_workspace_menu();
    view.advance_popover(Instant::now() + Duration::from_secs(1));
    view
}

fn area() -> Rect {
    Rect::new(koru_draw::theme::RAIL_W, 0.0, 260.0, 600.0)
}
const VIEWPORT: (f32, f32) = (720.0, 600.0);

#[test]
fn fixed_actions_have_distinct_reachable_targets_and_the_list_keeps_a_row() {
    let view = opened();
    view.with_page(area(), VIEWPORT, |frame| {
        let expected = [MANAGE, MenuAction::New.key(), MenuAction::Import.key()];
        for key in expected {
            let node = frame.node(key).unwrap();
            let (x, y) = node.rect.centre();
            assert_eq!(frame.hit_at(x, y), Some(key));
            let (_, row, part) = view.page_part_at(frame, x, y).unwrap();
            assert!(row.selectable());
            assert_eq!(
                part,
                if key == MANAGE {
                    PopoverPart::Gear
                } else {
                    PopoverPart::Row
                }
            );
        }
        let (list, _, _) = popover_elements::list_of(frame).unwrap();
        assert!(list.h >= metrics::ROW_MIN.value);
        assert!(frame.node(MenuAction::Import.key()).unwrap().rect.y > list.y + list.h);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
    })
    .unwrap();
}

#[test]
fn actions_route_and_only_import_keeps_the_parent_open() {
    for action in [
        MenuAction::Sync,
        MenuAction::Cloud,
        MenuAction::New,
        MenuAction::Import,
        MenuAction::Trash,
    ] {
        let mut view = opened();
        assert_eq!(
            view.press(PopoverRow::WorkspaceAction(action), PopoverPart::Row),
            action.outcome()
        );
        assert_eq!(view.popover_open(), action == MenuAction::Import);
    }
    let mut view = opened();
    assert_eq!(
        view.press(PopoverRow::WorkspacesHeading, PopoverPart::Gear),
        Outcome::ManageWorkspaces
    );
    assert!(!view.popover_open());
}

/// The channel icons live on the Profiles page; the workspace menu no
/// longer carries them.
#[test]
fn both_channel_icons_have_distinct_keyboard_and_accessibility_destinations() {
    use koru_agent_wire::{AccountProfileCloud, AccountState, CloudConnectionStage};
    use koru_workspace_host::accessibility::{ControlPart, Occurrence, ViewSlot, WindowSlot};
    use winit::keyboard::{Key as WKey, NamedKey};

    let configured = || {
        let mut view = opened();
        view.set_linked_count(2);
        view.set_account_state(&AccountState {
            cloud_profiles: vec![AccountProfileCloud {
                discovery: None,
                profile_id: view.current_profile.unwrap().to_string(),
                stage: CloudConnectionStage::Retrying,
            }],
            ..AccountState::default()
        });
        view.toggle_profile_popover();
        view
    };
    for (action, outcome) in [
        (MenuAction::Sync, Outcome::OpenSyncSettings),
        (MenuAction::Cloud, Outcome::ManageCloud),
    ] {
        let occurrence = Occurrence::surface(
            WindowSlot::from_raw(1),
            ViewSlot::from_raw(1),
            popover_elements::SURFACE,
            ControlPart::new(action.key().part),
        );
        let mut view = configured();
        let (x, y) = view
            .with_page(area(), VIEWPORT, |frame| {
                assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
                let rect = frame.node(action.key()).unwrap().rect;
                assert_eq!(
                    frame.hit_at(rect.centre().0, rect.centre().1),
                    Some(action.key())
                );
                rect.centre()
            })
            .unwrap();
        let (_, description, _) = view.menu_tooltip(area(), VIEWPORT, x, y).unwrap();
        assert_eq!(description, view.sync_control_name());
        assert_eq!(view.mouse_down(area(), VIEWPORT, x, y, false), outcome);
        assert!(!view.popover_open());
        let mut view = configured();
        assert_eq!(
            view.accessibility_action(area(), occurrence, accesskit::Action::Click, VIEWPORT),
            Some(outcome.clone())
        );
        for key in [NamedKey::Enter, NamedKey::Space] {
            let mut view = configured();
            assert_eq!(
                view.accessibility_action(area(), occurrence, accesskit::Action::Focus, VIEWPORT),
                Some(Outcome::Consumed)
            );
            assert_eq!(view.popover_key(&WKey::Named(key)), Some(outcome.clone()));
            assert!(!view.popover_open());
        }
    }
}

#[test]
fn workspace_chord_toggles_without_changing_the_folded_pane() {
    let mut view = opened();
    let folded = Rect::new(0.0, 0.0, 0.0, 600.0);
    let panel = view.popover_panel(folded, VIEWPORT).unwrap();
    assert!(panel.x >= koru_draw::theme::RAIL_W);
    assert!(view.toggle_workspace_menu());
    assert!(!view.popover_open());
    assert!(view.toggle_workspace_menu());
    assert_eq!(view.live().unwrap().kind, PopoverKind::Summary);
    assert_eq!(view.popover_panel(folded, VIEWPORT).unwrap(), panel);
}

#[test]
fn notice_is_the_last_row_and_import_uses_its_row_as_anchor() {
    let mut view = opened();
    view.set_workspace_notice(Some("Store unavailable".into()));
    assert_eq!(
        view.popover_rows().last(),
        Some(&PopoverRow::WorkspaceNotice)
    );
    let (x, y) = view.workspace_import_anchor(area(), VIEWPORT).unwrap();
    let panel = view.popover_panel(area(), VIEWPORT).unwrap();
    assert!(x > panel.x + panel.w);
    view.with_page(area(), VIEWPORT, |frame| {
        assert_eq!(y, frame.node(MenuAction::Import.key()).unwrap().rect.y);
        assert!(
            frame.node(NOTICE).unwrap().rect.y
                > frame.node(MenuAction::Import.key()).unwrap().rect.y
        );
    });
}

#[test]
fn configured_channels_are_independent_of_health_and_other_profiles() {
    use koru_agent_wire::{AccountProfileCloud, AccountStage, AccountState, CloudConnectionStage};
    let mut view = opened();
    let profile = view.current_profile.unwrap();
    for devices in [false, true] {
        view.set_linked_count(usize::from(devices));
        for cloud in [false, true] {
            for stage in [
                CloudConnectionStage::Connecting,
                CloudConnectionStage::Syncing,
                CloudConnectionStage::Retrying,
                CloudConnectionStage::WaitingForConsent,
            ] {
                view.set_account_state(&AccountState {
                    stage: AccountStage::SignedIn,
                    cloud_profiles: vec![AccountProfileCloud {
                        discovery: None,
                        profile_id: if cloud { profile } else { ProfileId::new() }.to_string(),
                        stage,
                    }],
                    ..AccountState::default()
                });
                assert_eq!(view.sync_channels(), SyncChannels { devices, cloud });
                assert_eq!(
                    view.sync_channels().controls().len(),
                    if devices && cloud { 2 } else { 1 }
                );
                let name = view.sync_control_name();
                assert!(name.contains(t(crate::format::lang(), view.sync_status().key())));
                assert_eq!(
                    name.contains(t(crate::format::lang(), Words::SyncChannelDevices)),
                    devices
                );
            }
        }
    }
}

/// **The menu is workspaces only.** No profile header, and none of Sync,
/// Cloud or Trash — even with both channels configured, when they would
/// have been drawn. The heading leads.
#[test]
fn the_summary_has_no_profile_header_and_no_sync_cloud_or_trash() {
    use koru_agent_wire::{AccountProfileCloud, AccountState, CloudConnectionStage};
    let mut view = opened();
    view.set_linked_count(2);
    view.set_account_state(&AccountState {
        cloud_profiles: vec![AccountProfileCloud {
            discovery: None,
            profile_id: view.current_profile.unwrap().to_string(),
            stage: CloudConnectionStage::Syncing,
        }],
        ..AccountState::default()
    });
    assert_eq!(view.sync_channels().controls().len(), 2, "both configured");
    let rows = view.popover_rows();
    assert_eq!(rows.first(), Some(&PopoverRow::WorkspacesHeading));
    for gone in [MenuAction::Sync, MenuAction::Cloud, MenuAction::Trash] {
        assert!(
            !rows.contains(&PopoverRow::WorkspaceAction(gone)),
            "{gone:?}"
        );
    }
    view.with_page(area(), VIEWPORT, |frame| {
        for gone in [MenuAction::Sync, MenuAction::Cloud, MenuAction::Trash] {
            assert!(frame.node(gone.key()).is_none(), "{gone:?}");
        }
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
    })
    .unwrap();
}
