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
        let expected = [
            MenuAction::Sync.key(),
            MANAGE,
            MenuAction::New.key(),
            MenuAction::Import.key(),
            MenuAction::Trash.key(),
        ];
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
        assert!(frame.node(MenuAction::Trash.key()).unwrap().rect.y > list.y + list.h);
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
    })
    .unwrap();
}

#[test]
fn actions_route_and_only_import_keeps_the_parent_open() {
    for action in [
        MenuAction::Sync,
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
                > frame.node(MenuAction::Trash.key()).unwrap().rect.y
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
                    view.sync_channels().glyphs().len(),
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

#[test]
fn profile_caption_is_inert_and_only_the_sync_button_is_actionable() {
    let view = opened();
    view.with_page(area(), VIEWPORT, |frame| {
        let header = frame.node(HEADER).unwrap().rect;
        assert_eq!(
            frame.hit_at(header.x + 1.0, header.y + header.h / 2.0),
            None
        );
        let button = frame.node(MenuAction::Sync.key()).unwrap().rect;
        assert!(button.w < header.w);
        assert_eq!(
            frame.hit_at(button.centre().0, button.centre().1),
            Some(MenuAction::Sync.key())
        );
    })
    .unwrap();
}
