// SPDX-License-Identifier: MPL-2.0

//! Geometry only — no `TextEngine`, no window. The menu is arithmetic plus a
//! clamp, and both are worth pinning because the clamp is what stops it running
//! off the window and the hit-test is what makes a click land on the right row.

use super::*;
use koru_draw::theme;
use koru_ui::menu::{MAX_W, MIN_W, PAD, ROW_PAD_X, measure_width};

#[test]
fn schedule_menu_tracks_lifecycle_and_keeps_irreversible_delete_last() {
    let owner = koru_delegate_core::DeviceId::new();
    assert_eq!(
        agent_schedule_items(&[koru_delegate_schedule::ScheduleState::Active { owner }]),
        vec![
            MenuAction::ScheduleRename,
            MenuAction::ScheduleEdit,
            MenuAction::SchedulePause,
            MenuAction::Divider,
            MenuAction::ScheduleDelete,
        ]
    );
    assert_eq!(
        agent_schedule_items(&[koru_delegate_schedule::ScheduleState::Paused {
            owner,
            reason: koru_delegate_schedule::SchedulePauseReason::ProviderNotDelegatable,
        }]),
        vec![
            MenuAction::ScheduleRename,
            MenuAction::ScheduleEdit,
            MenuAction::ScheduleResume,
            MenuAction::Divider,
            MenuAction::ScheduleDelete,
        ]
    );
    assert_eq!(
        agent_schedule_items(&[koru_delegate_schedule::ScheduleState::HandoffPending {
            owner,
            requests: vec![koru_delegate_core::HandoffId::mint()],
        }]),
        vec![
            MenuAction::ScheduleRename,
            MenuAction::ScheduleEdit,
            MenuAction::Divider,
            MenuAction::ScheduleDelete,
        ]
    );

    assert_eq!(
        agent_schedule_items(&[
            koru_delegate_schedule::ScheduleState::Active { owner },
            koru_delegate_schedule::ScheduleState::Active { owner },
        ]),
        vec![
            MenuAction::SchedulePause,
            MenuAction::Divider,
            MenuAction::ScheduleDelete,
        ],
        "bulk management omits single-item rename/edit"
    );
    assert_eq!(
        agent_schedule_items(&[
            koru_delegate_schedule::ScheduleState::Active { owner },
            koru_delegate_schedule::ScheduleState::Paused {
                owner,
                reason: koru_delegate_schedule::SchedulePauseReason::Manual,
            },
        ]),
        vec![MenuAction::Divider, MenuAction::ScheduleDelete],
        "mixed lifecycle selections offer no ambiguous partial toggle"
    );
}

#[test]
fn remote_schedule_rows_name_the_device_that_can_manage_them() {
    use koru_ui::MenuItem as _;

    set_schedule_manage_refusal(Some("Manage this schedule on Studio PC.".to_owned()));
    assert_eq!(
        MenuAction::ScheduleEdit.disabled_reason().as_deref(),
        Some("Manage this schedule on Studio PC.")
    );
    set_schedule_manage_refusal(None);
    assert_eq!(MenuAction::ScheduleEdit.disabled_reason(), None);
}

/// A tab menu at a known width, so every test below is pure arithmetic. The
/// real constructor measures its width from the labels, which needs a font; the
/// clamp and the hit test do not care where the number came from.
const W: f32 = 268.0;

fn tab_menu(tab: usize, anchor: (f32, f32)) -> ContextMenu {
    ContextMenu::at_width(
        MenuSubject::Tab(tab),
        anchor,
        TAB_ITEMS.to_vec(),
        Anchor::Cursor,
        W,
    )
}

#[test]
fn inspector_reuses_native_menu_occurrences_across_reopen_and_flyout() {
    use koru_draw::{inspect, scene};
    use vello::kurbo::Affine;

    let first = tab_menu(0, (100.0, 50.0));
    let reopened = tab_menu(0, (100.0, 50.0));
    let sub = tab_menu(0, (368.0, 50.0));
    assert_ne!(first.instance, reopened.instance);
    let parent_row = first.row_instance(0);
    assert_eq!(first.row_of_instance(parent_row), Some(0));
    assert_eq!(reopened.row_of_instance(parent_row), None);

    let mut engine = TextEngine::new();
    let mut canvas = vello::Scene::new();
    let frame = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, 1280.0, 720.0));
    for (menu, owner) in [(&first, None), (&sub, Some(parent_row))] {
        menu.draw_inspected(
            &mut canvas,
            Affine::IDENTITY,
            &mut engine,
            (1280.0, 720.0),
            &vec![true; menu.items().len()],
            owner,
        );
    }
    let snapshot = frame.finish();
    let value = |r: &inspect::Record, name| {
        r.properties
            .iter()
            .find(|p| p.name == name)
            .unwrap()
            .value
            .clone()
    };
    let menus: Vec<_> = snapshot
        .records
        .iter()
        .filter(|r| r.name == "KoruMenu")
        .collect();
    assert_eq!(menus.len(), 2);
    assert_eq!(menus[0].parent, None);
    assert_eq!(
        menus[1].parent, None,
        "flyout stays a sibling render surface"
    );
    assert_eq!(value(menus[0], "occurrence"), first.instance.to_string());
    assert_eq!(value(menus[1], "logical owner row"), parent_row.to_string());
    let row = snapshot
        .records
        .iter()
        .find(|r| r.name == "KoruMenuItem")
        .unwrap();
    assert_eq!(value(row, "occurrence"), parent_row.to_string());
}

#[test]
fn the_menu_opens_at_the_cursor_when_it_fits() {
    let m = tab_menu(2, (100.0, 50.0));
    let p = m.panel((1280.0, 720.0));
    assert_eq!((p.x, p.y), (100.0, 50.0), "no shift when it fits");
    assert_eq!(p.w, W);
    assert_eq!(p.h, m.height());
}

#[test]
fn the_menu_pulls_back_inside_when_it_would_overflow_the_right_edge() {
    let m = tab_menu(0, (1200.0, 50.0));
    let p = m.panel((1280.0, 720.0));
    assert!(
        p.x + p.w <= 1280.0 - EDGE_MARGIN,
        "right edge {:?} stays inside",
        p.x + p.w
    );
    assert!(p.x >= EDGE_MARGIN);
}

#[test]
fn the_menu_pulls_up_inside_when_it_would_overflow_the_bottom_edge() {
    let m = tab_menu(0, (50.0, 700.0));
    let p = m.panel((1280.0, 720.0));
    assert!(
        p.y + p.h <= 720.0 - EDGE_MARGIN,
        "bottom edge {:?} stays inside",
        p.y + p.h
    );
    assert!(p.y >= EDGE_MARGIN);
}

#[test]
fn a_tiny_viewport_still_keeps_the_menu_on_screen() {
    let m = tab_menu(0, (0.0, 0.0));
    let p = m.panel((40.0, 40.0));
    // Narrower than the menu: it clamps to the margin rather than going negative.
    assert!(p.x >= 0.0);
    assert!(p.y >= 0.0);
}

#[test]
fn item_at_hits_each_row_in_order() {
    let m = tab_menu(1, (100.0, 100.0));
    let vp = (1280.0, 720.0);
    // Walked through `item_rect` rather than multiplied out: the menu has a
    // divider in it now, and a divider is shorter than a row. Multiplying was
    // only ever right while every row was the same height.
    for (i, action) in TAB_ITEMS.iter().enumerate() {
        let (_, cy) = m.item_rect(vp, i).centre();
        let hit = m.item_at(vp, m.panel(vp).x + 5.0, cy);
        if action.selectable() {
            assert_eq!(hit, Some(i), "row {i}");
        } else {
            assert_eq!(hit, None, "row {i} is a divider and cannot be hit");
        }
    }
}

#[test]
fn item_at_misses_the_padding_band_and_outside() {
    let m = tab_menu(1, (100.0, 100.0));
    let vp = (1280.0, 720.0);
    let p = m.panel(vp);
    // The band between the panel top and the first row.
    assert_eq!(m.item_at(vp, p.x + 5.0, p.y + PAD / 2.0), None);
    // Below the last row, in the bottom padding.
    let last = m.item_rect(vp, TAB_ITEMS.len() - 1);
    let below = last.y + last.h + PAD / 2.0;
    assert_eq!(m.item_at(vp, p.x + 5.0, below), None);
    // Entirely outside the panel.
    assert_eq!(m.item_at(vp, p.x - 5.0, p.y + 5.0), None);
}

#[test]
fn the_action_labels_are_the_ones_the_menu_offers() {
    assert_eq!(MenuAction::PopOut.label(), "Move to New Window");
    assert_eq!(MenuAction::Close.label(), "Close");
    assert_eq!(MenuAction::CloseOthers.label(), "Close Other Tabs");
    assert_eq!(MenuAction::CloseLeft.label(), "Close Tabs to the Left");
    assert_eq!(MenuAction::CloseRight.label(), "Close Tabs to the Right");
    assert_eq!(MenuAction::CloseAll.label(), "Close All Tabs");
    // Six verbs and the divider that separates the one which keeps the tab.
    assert_eq!(TAB_ITEMS.len(), 7);
    assert_eq!(
        TAB_ITEMS.last(),
        Some(&MenuAction::PopOut),
        "the constructive row sits below the closing ones"
    );
    assert_eq!(TAB_ITEMS[TAB_ITEMS.len() - 2], MenuAction::Divider);
    // Move to New Window is the one item that carries a glyph; the rest are
    // text alone.
    assert_eq!(MenuAction::PopOut.icon(), Some(koru_draw::icons::POP_OUT));
    assert!(MenuAction::Close.icon().is_none());
    // **No chord on Close All Tabs.** `Ctrl+Shift+W` still works; the row no
    // longer advertises it.
    assert_eq!(MenuAction::CloseAll.shortcut(), None);
    // **From the registry, so this reads ⌘W on macOS.** Asserting the
    // literal here is what made 77 label strings wrong on that platform.
    assert_eq!(
        MenuAction::Close.shortcut(),
        Some(koru_keys::label_of(koru_keys::ShortcutId::CloseTab))
    );
}

#[test]
fn a_document_tab_adds_rename_and_trash_without_changing_other_tabs() {
    assert_eq!(tab_items(false, false, false, false), TAB_ITEMS);
    assert_eq!(
        tab_items(true, false, false, false),
        vec![
            MenuAction::PinTab,
            MenuAction::Divider,
            MenuAction::Close,
            MenuAction::CloseOthers,
            MenuAction::CloseLeft,
            MenuAction::CloseRight,
            MenuAction::CloseAll,
            MenuAction::Divider,
            MenuAction::RevealInExplore,
            MenuAction::Rename,
            MenuAction::MoveToTrash,
            MenuAction::NewSocialPost,
            MenuAction::Divider,
            MenuAction::PopOut,
        ]
    );
    assert_eq!(
        tab_items(false, true, false, false),
        vec![
            MenuAction::PinTab,
            MenuAction::Divider,
            MenuAction::Close,
            MenuAction::CloseOthers,
            MenuAction::CloseLeft,
            MenuAction::CloseRight,
            MenuAction::CloseAll,
            MenuAction::Divider,
            MenuAction::PopOut,
        ],
        "A pinnable non-document tab gets the pin row without document actions"
    );
}

#[test]
fn the_new_tab_menu_leads_to_content_importers_through_a_submenu() {
    set_plus_snapshots(vec![], vec![], vec![], vec![]);
    // One *Welcome* row, always: *Get started* was a second way to open the
    // same page and went when every Welcome became the checklist.
    assert_eq!(
        new_tab_items(),
        vec![
            MenuAction::OpenWelcome,
            MenuAction::Documents,
            MenuAction::Canvases,
            MenuAction::NewShell,
            MenuAction::NewAgent,
            MenuAction::NewAiChat,
            MenuAction::Delegate,
            MenuAction::Chats,
            MenuAction::Socials,
            MenuAction::Divider,
            MenuAction::ImportAs,
        ]
    );
    assert_eq!(
        MenuAction::NewShell.icon(),
        Some(koru_draw::icons::TERMINAL)
    );
    assert_eq!(MenuAction::NewShell.label(), "Shell");
    assert_eq!(MenuAction::NewAgent.label(), "Agents");
    assert_eq!(MenuAction::NewAgent.icon(), Some(koru_draw::icons::SPARKLE));
    assert_eq!(MenuAction::Chats.label(), "Chats");
    assert_eq!(
        MenuAction::OpenWelcome.icon(),
        Some(koru_draw::icons::HOUSE)
    );
    assert_eq!(MenuAction::NewAiChat.label(), "New");
    assert_eq!(
        MenuAction::NewAiChat.icon(),
        Some(koru_draw::icons::SPARKLE)
    );
    assert_eq!(MenuAction::Documents.label(), "Documents");
    assert_eq!(MenuAction::NewDocument.label(), "New");
    assert_eq!(
        MenuAction::NewDocument.icon(),
        Some(koru_draw::icons::DOCUMENT)
    );
    let shells = shell_profile_items();
    assert!(!shells.is_empty());
    assert!(
        shells
            .iter()
            .all(|a| matches!(a, MenuAction::TerminalProfile(_)))
    );
    assert_eq!(submenu(MenuAction::NewShell), Some(shells));
    let agents = agent_profile_items();
    assert!(!agents.is_empty());
    assert!(
        agents
            .iter()
            .all(|a| matches!(a, MenuAction::TerminalProfile(_)))
    );
    assert!(
        agents.iter().all(|a| a.label() != "Codex (Embedded)"),
        "the Agents launch flyout offers the raw Codex CLI row only, not Codex (Embedded)"
    );
    assert_eq!(submenu(MenuAction::NewAgent), Some(agents));
    assert_eq!(
        submenu(MenuAction::Documents),
        Some(vec![MenuAction::NewDocument])
    );
    assert_eq!(
        submenu(MenuAction::Chats),
        Some(vec![MenuAction::NewAiChat])
    );
    assert_eq!(MenuAction::ImportAs.label(), "Import\u{2026}");
    assert_eq!(submenu(MenuAction::ImportAs), Some(import_items()));
    // The submenu is a projection of the shell's format registry, in
    // registration order — never a list maintained here.
    let ids: Vec<&'static str> = import_items()
        .into_iter()
        .map(|action| match action {
            MenuAction::ImportFormat(id) => id,
            other => panic!("unexpected import row: {other:?}"),
        })
        .collect();
    let registered: Vec<&'static str> = koru_workspace_host::formats::import_menu_formats()
        .into_iter()
        .map(|descriptor| descriptor.id)
        .collect();
    assert_eq!(ids, registered);
    // A format this build cannot run explains itself beside the dimmed row
    // (C6); one it can run has nothing to say.
    use koru_ui::MenuItem as _;
    for id in ids {
        assert_eq!(
            MenuAction::ImportFormat(id).disabled_reason().as_deref(),
            koru_workspace_host::formats::import_unavailable_reason(id, crate::format::lang()),
            "{id}"
        );
    }
    assert_eq!(MenuAction::ImportFormat("markdown").disabled_reason(), None);
    assert_eq!(MenuAction::ImportFormat("markdown").label(), "Markdown");
    assert_eq!(
        MenuAction::ImportFormat("onenote").label(),
        "Microsoft OneNote"
    );
    assert_eq!(
        MenuAction::NewDocument.shortcut(),
        Some(koru_keys::label_of(koru_keys::ShortcutId::NewTab))
    );
    // Ctrl+I is Italic; New chat has no chord of its own.
    assert_eq!(MenuAction::NewAiChat.shortcut(), None);
}

/// Recents, running sessions and chats are snapshotted as the menu opens, so
/// a flyout names what was true then. Empty lists omit the divider.
#[test]
fn plus_flyouts_list_recents_and_running_sessions_under_new() {
    let doc = koru_core::DocumentId::new();
    let shell = SessionId::mint();
    let agent = SessionId::mint();
    let chat = koru_ai_ui::ChatId::new();
    set_plus_snapshots(
        vec![(doc, "Notes".into())],
        vec![(shell, "bash".into())],
        vec![(agent, "Claude".into())],
        vec![(chat, "Yesterday".into())],
    );

    assert_eq!(
        document_plus_items(),
        vec![
            MenuAction::NewDocument,
            MenuAction::Divider,
            MenuAction::OpenRecentDocument(doc),
            // Moved here from the Welcome page, and only offered when there
            // is something to clear.
            MenuAction::Divider,
            MenuAction::ClearRecents,
        ]
    );
    assert_eq!(MenuAction::OpenRecentDocument(doc).label(), "Notes");
    assert_eq!(MenuAction::ClearRecents.label(), "Clear recents");

    let shells = shell_plus_items();
    assert_eq!(shells[0], MenuAction::ActiveShells);
    assert_eq!(shells[1], MenuAction::Divider);
    assert!(
        shells[2..]
            .iter()
            .all(|a| matches!(a, MenuAction::TerminalProfile(_)))
    );
    assert_eq!(MenuAction::ActiveShells.label(), "Active");
    assert_eq!(
        submenu(MenuAction::ActiveShells),
        Some(vec![MenuAction::OpenRunningSession(shell)])
    );
    assert_eq!(MenuAction::OpenRunningSession(shell).label(), "bash");

    let agents = agent_plus_items();
    assert_eq!(agents[0], MenuAction::ActiveAgents);
    assert_eq!(agents[1], MenuAction::Divider);
    assert_eq!(MenuAction::ActiveAgents.label(), "Active");
    assert_eq!(
        submenu(MenuAction::ActiveAgents),
        Some(vec![MenuAction::OpenRunningSession(agent)])
    );
    assert_eq!(MenuAction::OpenRunningSession(agent).label(), "Claude");

    assert_eq!(
        chat_plus_items(),
        vec![
            MenuAction::NewAiChat,
            MenuAction::Divider,
            MenuAction::OpenRecentChat(chat),
        ]
    );
    assert_eq!(MenuAction::OpenRecentChat(chat).label(), "Yesterday");

    // Eleven recents collapse to the Jump List's ten.
    let many: Vec<_> = (0..PLUS_RECENT_MAX + 1)
        .map(|i| (koru_core::DocumentId::new(), format!("d{i}")))
        .collect();
    let eleventh = many[PLUS_RECENT_MAX].0;
    set_plus_snapshots(many, vec![], vec![], vec![]);
    let docs = document_plus_items();
    assert_eq!(docs.len(), 1 + 1 + PLUS_RECENT_MAX + 2);
    assert_eq!(docs.last(), Some(&MenuAction::ClearRecents));
    assert!(!docs.contains(&MenuAction::OpenRecentDocument(eleventh)));

    set_plus_snapshots(vec![], vec![], vec![], vec![]);
    assert_eq!(document_plus_items(), vec![MenuAction::NewDocument]);
    assert_eq!(chat_plus_items(), vec![MenuAction::NewAiChat]);
    assert_eq!(shell_plus_items(), shell_profile_items());
}

// ------------------------------------------------ the + menu's profile rows

/// **The reconciled display names** (W4.4a). Koru Terminal rendered the
/// Codex CLI profile as "Codex"; this menu routes through the same shared
/// projection (`koru_session_profiles::display_name`), so the wire fallback
/// "Codex CLI" never reaches a row and the two launch surfaces cannot drift.
#[test]
fn profile_rows_read_the_reconciled_display_names() {
    use koru_session_profiles::ProfileLabel;
    let profiles = built_in_profiles();
    let index_of = |label: ProfileLabel| {
        profiles
            .iter()
            .position(|profile| profile.label == label)
            .expect("a built-in carries this label")
    };
    assert_eq!(
        MenuAction::TerminalProfile(index_of(ProfileLabel::CodexCli)).label(),
        "Codex",
        "the reconciled name, never the wire's \"Codex CLI\""
    );
    assert_eq!(
        MenuAction::TerminalProfile(index_of(ProfileLabel::CodexEmbedded)).label(),
        "Codex (Embedded)"
    );
    // The platform shell routes through the i18n row Koru Terminal owned, so
    // this asserts against that row rather than pinning one locale's word.
    let lang = crate::format::lang();
    assert_eq!(
        MenuAction::TerminalProfile(index_of(ProfileLabel::PlatformShell)).label(),
        koru_i18n::t(lang, koru_i18n::Key::TerminalPlatformShell)
    );
    // And every row agrees with the one projection Koru Terminal drew from.
    for (index, profile) in profiles.iter().enumerate() {
        assert_eq!(
            MenuAction::TerminalProfile(index).label(),
            koru_session_profiles::display_name(
                profile,
                koru_i18n::t(lang, koru_i18n::Key::TerminalPlatformShell),
                koru_i18n::t(lang, koru_i18n::Key::TerminalCustomProfile),
            )
        );
    }
    // An index past the catalogue answers a plain word rather than panicking.
    assert_eq!(MenuAction::TerminalProfile(usize::MAX).label(), "Terminal");
}

/// A dimmed profile row explains itself. The row's only way to dim is the
/// presence probe saying its executable is missing (`item_is_enabled`), so
/// the note is the same *Not installed* Koru Terminal showed — and it exists
/// for every profile row, ready for whichever the probe dims.
#[test]
fn a_dimmed_profile_row_says_not_installed() {
    use koru_ui::MenuItem as _;
    let rows = shell_profile_items()
        .into_iter()
        .chain(agent_profile_items());
    let mut seen = 0;
    for action in rows {
        assert_eq!(
            action.disabled_reason().as_deref(),
            Some("Not installed"),
            "{action:?}"
        );
        seen += 1;
    }
    assert!(seen > 0, "the flyouts offer rows to explain");
}

/// **Every built-in profile row wears its product's own mark** (Q-E6 P3/P4).
/// The mapping is total over the live catalogue — no row falls back to the
/// launcher rows' generic terminal/sparkle — and each row answers exactly one
/// of the two channels the menu widget draws (a Phosphor glyph via `icon`, or
/// vector path data via `brand_icon`), so a row can never draw two marks or
/// none.
#[test]
fn every_profile_row_carries_its_products_official_mark() {
    use koru_draw::brand::ProfileMark;
    let profiles = built_in_profiles();
    assert!(!profiles.is_empty());
    for (index, profile) in profiles.iter().enumerate() {
        let key = match &profile.id {
            SessionProfileKey::BuiltIn(key) => key.as_str(),
            SessionProfileKey::Custom(_) => panic!("built-in became custom"),
        };
        let mark = koru_draw::brand::for_profile(key)
            .unwrap_or_else(|| panic!("{key} has no mark — the mapping must be total"));
        if let ProfileMark::Glyph(glyph) = mark {
            assert_ne!(glyph, koru_draw::icons::TERMINAL, "{key} fell back");
            assert_ne!(glyph, koru_draw::icons::SPARKLE, "{key} fell back");
        }
        let action = MenuAction::TerminalProfile(index);
        match mark {
            ProfileMark::Glyph(glyph) => {
                assert_eq!(action.icon(), Some(glyph), "{key}");
                assert_eq!(action.brand_icon(), None, "{key}");
            }
            ProfileMark::Brand(brand) => {
                assert_eq!(action.brand_icon(), Some(brand), "{key}");
                assert_eq!(action.icon(), None, "{key}");
            }
        }
        // The widget sees the same answers through the trait.
        assert_eq!(koru_ui::MenuItem::icon(&action), action.icon());
        assert_eq!(koru_ui::MenuItem::brand_icon(&action), action.brand_icon());
    }
    // A row past the catalogue keeps a plain face rather than panicking.
    assert_eq!(MenuAction::TerminalProfile(usize::MAX).icon(), None);
    assert_eq!(MenuAction::TerminalProfile(usize::MAX).brand_icon(), None);
}

// ------------------------------------------------------ the workspace menu

fn workspace_menu(pinned: bool, archived: bool) -> ContextMenu {
    ContextMenu::at_width(
        MenuSubject::Workspace(WorkspaceId::new()),
        (100.0, 100.0),
        workspace_items(pinned, archived, false),
        Anchor::Cursor,
        W,
    )
}

/// A divider is shorter than a row, so the rows below it are not at
/// `PAD + i * ITEM_H`. Every item still has to hit-test back to itself, or a
/// click lands one row off — the failure mode of walking the list in one place
/// and multiplying in another.
#[test]
fn every_workspace_item_hits_back_to_itself_across_the_dividers() {
    let m = workspace_menu(false, false);
    let vp = (1280.0, 720.0);
    assert!(
        m.items().iter().filter(|a| !a.selectable()).count() >= 2,
        "this test is only meaningful with dividers in the list"
    );
    for (i, action) in m.items().iter().enumerate() {
        let r = m.item_rect(vp, i);
        let (x, y) = r.centre();
        if action.selectable() {
            assert_eq!(m.item_at(vp, x, y), Some(i), "{action:?}");
        } else {
            assert_eq!(m.item_at(vp, x, y), None, "a divider is not a target");
        }
    }
}

#[test]
fn the_workspace_rows_run_top_to_bottom_inside_the_panel() {
    let m = workspace_menu(false, false);
    let vp = (1280.0, 720.0);
    let p = m.panel(vp);
    for i in 1..m.items().len() {
        let above = m.item_rect(vp, i - 1);
        let here = m.item_rect(vp, i);
        assert_eq!(above.y + above.h, here.y, "row {i}");
    }
    let last = m.item_rect(vp, m.items().len() - 1);
    assert!(
        last.y + last.h <= p.y + p.h + 0.01,
        "overflows its own panel"
    );
}

/// The label has to say what pressing it does. A pinned workspace offering
/// "Pin to Top" is a menu that lies.
#[test]
fn pin_and_archive_read_as_what_they_would_do() {
    let plain = workspace_items(false, false, false);
    assert!(plain.contains(&MenuAction::Pin) && !plain.contains(&MenuAction::Unpin));
    assert!(plain.contains(&MenuAction::Archive) && !plain.contains(&MenuAction::Unarchive));

    let both = workspace_items(true, true, false);
    assert!(both.contains(&MenuAction::Unpin) && !both.contains(&MenuAction::Pin));
    assert!(both.contains(&MenuAction::Unarchive) && !both.contains(&MenuAction::Archive));
    assert_eq!(plain.len(), both.len(), "and the shape does not change");
}

#[test]
fn a_deleted_workspace_offers_restore_instead_of_more_lifecycle_actions() {
    let deleted = workspace_items(false, false, true);
    assert!(deleted.contains(&MenuAction::RestoreWorkspace));
    assert!(!deleted.contains(&MenuAction::Archive));
    assert!(!deleted.contains(&MenuAction::Unarchive));
    assert!(!deleted.contains(&MenuAction::Delete));
}

/// Delete is the only red item. Archive is reversible and must not wear the
/// colour that means "this one is different".
#[test]
fn only_delete_is_destructive() {
    let destructive: Vec<_> = workspace_items(false, false, false)
        .into_iter()
        .filter(|a| a.destructive())
        .collect();
    assert_eq!(destructive, vec![MenuAction::Delete]);
}

/// A tall menu near the floor of a short window still fits — the workspace menu
/// is twice the height of the tab one, so this clamp does real work now.
#[test]
fn a_tall_menu_near_the_bottom_is_pulled_fully_inside() {
    let m = workspace_menu(false, false);
    let vp = (1280.0, 420.0);
    let p = m.panel(vp);
    assert!(p.y >= EDGE_MARGIN);
    assert!(p.y + p.h <= vp.1 - EDGE_MARGIN, "bottom {}", p.y + p.h);
}

/// Stepping with the keyboard skips dividers and disabled rows and wraps —
/// otherwise Down stops dead on the first separator.
#[test]
fn stepping_skips_dividers_and_disabled_rows() {
    let mut m = workspace_menu(false, false);
    let n = m.items().len();
    let mut enabled = vec![true; n];
    // Disable the first real item, so the first Down has to pass over it.
    enabled[0] = false;

    m.step(&enabled, 1);
    let first = m.hover.expect("something is selectable");
    assert_ne!(first, 0, "the disabled first row was skipped");
    assert!(m.items()[first].selectable());

    // Walk the whole list and back to where it started.
    let selectable = (0..n)
        .filter(|&i| m.items()[i].selectable() && enabled[i])
        .count();
    for _ in 0..selectable {
        m.step(&enabled, 1);
    }
    assert_eq!(m.hover, Some(first), "stepping wraps");

    m.step(&enabled, -1);
    assert!(m.hover.is_some_and(|i| m.items()[i].selectable()));
}

/// Nothing enabled — every item disabled by a workspace that vanished from the
/// list — must not spin or panic.
#[test]
fn stepping_with_nothing_enabled_selects_nothing() {
    let mut m = workspace_menu(false, false);
    let enabled = vec![false; m.items().len()];
    m.step(&enabled, 1);
    assert_eq!(m.hover, None);
}

// ------------------------------------------------------------- the width

/// **Measured from the items, not fixed.** A one-row menu saying *Move to trash* used
/// to open at 268 — the width the tab menu needs for "Close Tabs to the Right"
/// — and read as a dialog somebody forgot to fill in.
#[test]
fn a_short_menu_is_narrower_than_a_long_one() {
    let mut e = koru_draw::text::TextEngine::new();
    let short = measure_width(&mut e, &[MenuAction::MoveToTrash]);
    let long = measure_width(&mut e, &TAB_ITEMS);
    assert!(short < long, "{short} should be under {long}");
}

/// Bounded at both ends: never so narrow it reads as a tooltip, never so wide
/// it becomes a panel.
#[test]
fn the_width_stays_between_its_bounds() {
    let mut e = koru_draw::text::TextEngine::new();
    for items in [
        vec![MenuAction::Restore],
        TAB_ITEMS.to_vec(),
        workspace_items(true, true, false),
        trashed_items(),
    ] {
        let w = measure_width(&mut e, &items);
        assert!((MIN_W..=MAX_W).contains(&w), "{w} out of bounds");
    }
}

/// A divider carries nothing, so it must not be what decides the width.
#[test]
fn a_divider_does_not_measure() {
    let mut e = koru_draw::text::TextEngine::new();
    let with = measure_width(&mut e, &[MenuAction::Restore, MenuAction::Divider]);
    let without = measure_width(&mut e, &[MenuAction::Restore]);
    assert_eq!(with, without);
}

/// Moving to Trash is deliberately not advertised as a key action.
#[test]
fn move_to_trash_has_no_keyboard_shortcut_and_fits_its_label() {
    let mut e = koru_draw::text::TextEngine::new();
    let w = measure_width(&mut e, &[MenuAction::MoveToTrash]);
    let label = e
        .simple_layout(MenuAction::MoveToTrash.label().as_ref(), theme::role::row())
        .width();
    assert_eq!(MenuAction::MoveToTrash.shortcut(), None);
    assert!(w >= label + ROW_PAD_X * 2.0 || w == MAX_W);
}

#[test]
fn a_multi_node_menu_names_the_number_in_the_header() {
    assert_eq!(
        node_items(4),
        vec![
            MenuAction::SelectionHeader(4),
            MenuAction::Divider,
            MenuAction::MoveNodesTo(4),
            MenuAction::OpenInNewTab,
            MenuAction::OpenDocumentInNewWindow,
            MenuAction::ExportAs,
            MenuAction::Divider,
            MenuAction::MoveNodesToTrash(4),
        ]
    );
    assert_eq!(node_items(4)[0].label(), "4 Documents");
    assert!(!node_items(4)[0].selectable());
    assert_eq!(node_items(4)[2].label(), "Move to\u{2026}");
    assert_eq!(node_items(4)[3].label(), "Open in new tabs");
    assert_eq!(node_items(4)[4].label(), "Open in a new window");
    assert_eq!(node_items(4)[5].label(), "Export");
    assert_eq!(node_items(4)[7].label(), "Move to trash");
    assert_eq!(
        node_items(1),
        vec![
            MenuAction::OpenInNewTab,
            MenuAction::OpenDocumentInNewWindow,
            MenuAction::Rename,
            MenuAction::Attributes,
            MenuAction::MoveTo,
            MenuAction::ExportAs,
            MenuAction::Divider,
            MenuAction::MoveToTrash,
            MenuAction::NewSocialPost,
        ]
    );
    assert_eq!(node_items(1)[0].label(), "Open in new tabs");
    assert_eq!(node_items(1)[1].label(), "Open in a new window");
    assert_eq!(node_items(1)[2].label(), "Rename\u{2026}");
    assert!(node_items(1)[2].shortcut().is_some());
    assert_eq!(node_items(1)[3].label(), "Attributes");
    assert_eq!(node_items(1)[4].label(), "Move to\u{2026}");
    assert!(node_items(1)[4].shortcut().is_some());
    assert_eq!(node_items(1)[7].label(), "Move to trash");
    // Attributes is a submenu parent, and its flyout holds the four pickers
    // R0 ships — A2's two, then A3/A4's.
    assert_eq!(
        submenu(MenuAction::Attributes),
        Some(vec![
            MenuAction::AttributeTags,
            MenuAction::AttributeColor,
            MenuAction::AttributeDue,
            MenuAction::AttributePriority,
        ])
    );
}

#[test]
fn a_file_menu_opens_or_copies_the_opaque_file() {
    assert_eq!(
        file_items(1, None),
        vec![
            MenuAction::OpenFileInApp,
            MenuAction::Rename,
            MenuAction::SaveFileAs,
            MenuAction::Divider,
            MenuAction::MoveToTrash,
        ]
    );
    assert_eq!(MenuAction::OpenFileInApp.label(), "Open in app");
    assert_eq!(file_items(1, None)[1].label(), "Rename\u{2026}");
    assert!(file_items(1, None)[1].shortcut().is_some());
    assert_eq!(MenuAction::SaveFileAs.label(), "Save as\u{2026}");
    assert_eq!(file_items(3, None)[3].label(), "Move 3 to trash");
    assert_eq!(file_items(1, None)[4].label(), "Move to trash");
}

/// *Add note* when the file has none, *Open note* when it does — one row,
/// decided when the menu is built, never both. A multi-selection has no single
/// note to open, so it offers neither.
#[test]
fn a_file_menu_offers_the_note_row_the_file_has_earned() {
    let has = |action| file_items(1, Some(false)).contains(&action);
    assert!(has(MenuAction::AddNote));
    assert!(!has(MenuAction::OpenNote));

    let has = |action| file_items(1, Some(true)).contains(&action);
    assert!(has(MenuAction::OpenNote));
    assert!(!has(MenuAction::AddNote));

    let has = |action| file_items(1, None).contains(&action);
    assert!(!has(MenuAction::AddNote));
    assert!(!has(MenuAction::OpenNote));

    assert_eq!(MenuAction::AddNote.label(), "Add note");
    assert_eq!(MenuAction::OpenNote.label(), "Open note");

    // The note row sits between *Save as…* and the divider, so trash moves
    // to 5 when a note row is present.
    assert_eq!(file_items(1, Some(false))[5].label(), "Move to trash");
    assert_eq!(file_items(1, Some(true))[5].label(), "Move to trash");
}

#[test]
fn export_opens_a_format_submenu() {
    assert_eq!(submenu(MenuAction::ExportAs), Some(export_items()));
    let ids: Vec<&'static str> = export_items()
        .into_iter()
        .map(|action| match action {
            MenuAction::ExportFormat(id) => id,
            other => panic!("unexpected export row: {other:?}"),
        })
        .collect();
    let registered: Vec<&'static str> = koru_workspace_host::formats::export_menu_formats()
        .into_iter()
        .map(|descriptor| descriptor.id)
        .collect();
    assert_eq!(ids, registered);
    assert_eq!(MenuAction::ExportAs.label(), "Export");
    assert_eq!(MenuAction::ExportFormat("markdown").label(), "Markdown");
    assert_eq!(MenuAction::ExportFormat("textbundle").label(), "TextBundle");
}

/// A parent row on the left of the window has room on the right: the flyout
/// opens outward and keeps a level handoff from the row that owns it.
#[test]
fn a_submenu_prefers_the_right_when_there_is_room() {
    let parent = koru_draw::geometry::Rect::new(40.0, 100.0, 160.0, 80.0);
    let mut e = koru_draw::text::TextEngine::new();
    let sub = ContextMenu::as_submenu(
        &mut e,
        MenuSubject::Nodes(vec![koru_core::NodeId::new()]),
        parent,
        export_items(),
        (1280.0, 720.0),
    );
    let p = sub.panel((1280.0, 720.0));
    assert!(
        p.x >= parent.x + parent.w - 0.5,
        "submenu should hang right of the parent (x={} parent right={})",
        p.x,
        parent.x + parent.w
    );
    assert_eq!(p.y, parent.y, "tops of parent row and submenu line up");
}

/// A parent row jammed against the right edge has no room: hang left with the
/// same level handoff instead of forcing a diagonal path through another row.
#[test]
fn a_submenu_falls_back_left_when_the_right_is_full() {
    let parent = koru_draw::geometry::Rect::new(1100.0, 100.0, 160.0, 80.0);
    let mut e = koru_draw::text::TextEngine::new();
    let sub = ContextMenu::as_submenu(
        &mut e,
        MenuSubject::Window,
        parent,
        export_items(),
        (1280.0, 720.0),
    );
    let p = sub.panel((1280.0, 720.0));
    assert!(
        p.x + p.w <= parent.x + 0.5,
        "submenu should hang left of the parent (right={} parent left={})",
        p.x + p.w,
        parent.x
    );
    assert_eq!(p.y, parent.y, "tops of parent row and submenu line up");
}

/// The `…` anchors the menu's **right** edge, so the panel hangs back under the
/// button whatever it turns out to be wide.
#[test]
fn a_button_anchored_menu_ends_at_the_button() {
    let m = ContextMenu::at_width(
        MenuSubject::Nodes(vec![koru_core::NodeId::new()]),
        (400.0, 120.0),
        vec![MenuAction::MoveToTrash],
        Anchor::ButtonRight,
        160.0,
    );
    let p = m.panel((1280.0, 720.0));
    assert_eq!(p.x + p.w, 400.0);
    assert_eq!(p.y, 120.0);
}

// ------------------------------------------------------ the window menu

fn window_labels(pinned: bool, maximized: bool) -> Vec<String> {
    window_items(pinned, maximized, false)
        .into_iter()
        .map(|action| action.label().into_owned())
        .collect()
}

/// Menu hints come from the same registry that dispatches their chords.
#[test]
fn every_window_row_names_the_chord_that_does_the_same_thing() {
    assert_eq!(
        MenuAction::HeaderSettings.shortcut(),
        Some(koru_keys::label_of(koru_keys::ShortcutId::Settings))
    );
}

#[test]
fn the_overflow_keeps_the_pin_glyph_and_uses_text_for_destinations() {
    assert_eq!(
        MenuAction::WindowAlwaysOnTop.icon(),
        Some(koru_draw::icons::PUSH_PIN)
    );
    assert_eq!(MenuAction::HeaderSettings.icon(), None);
    assert_eq!(MenuAction::HeaderUpdates.icon(), None);
    assert_eq!(
        MenuAction::HeaderFeedback.icon(),
        Some(koru_draw::icons::USER_SOUND)
    );
}

/// Support took Help's slot: the first row after the zoom divider, ahead of
/// Feedback.
#[test]
fn support_sits_where_help_was_with_the_headset_glyph() {
    let items = window_items(false, false, false);
    let zoom = items
        .iter()
        .position(|a| *a == MenuAction::HeaderZoom)
        .expect("zoom row");
    assert_eq!(items[zoom + 1], MenuAction::Divider);
    if OVERFLOW_HIDDEN_ROWS_SHOWN {
        assert_eq!(items[zoom + 2], MenuAction::HeaderSupport);
        assert_eq!(items[zoom + 3], MenuAction::HeaderFeedback);
    } else {
        // Hidden for now: Feedback is the only destination after zoom.
        assert_eq!(&items[zoom + 2..], &[MenuAction::HeaderFeedback]);
    }
    assert_eq!(
        MenuAction::HeaderSupport.icon(),
        Some(koru_draw::icons::HEADSET)
    );
    assert_eq!(MenuAction::HeaderSupport.label(), "Support");
    assert!(submenu(MenuAction::HeaderSupport).is_none());
}

#[test]
fn always_on_top_flips_its_label_when_the_window_is_pinned() {
    assert!(
        window_labels(false, false)
            .iter()
            .any(|s| s == "Always on Top")
    );
    assert!(
        window_labels(true, false)
            .iter()
            .any(|s| s == "Turn Off Always on Top")
    );
}

/// Nothing in this menu is destructive; it contains state and destinations.
#[test]
fn the_window_menu_has_no_destructive_rows() {
    for pinned in [false, true] {
        for maximized in [false, true] {
            for recording in [false, true] {
                let items = window_items(pinned, maximized, recording);
                assert!(
                    items.iter().all(|a| !a.destructive()),
                    "red is for losing data, not for closing a window"
                );
                // Updates closes the public menu; where the internal session
                // recorder exists its row sits below it (SESSIONRECORD).
                let want_last = match (recording_rows_exist(), recording) {
                    (true, true) => MenuAction::HeaderStopRecording,
                    (true, false) => MenuAction::HeaderStartRecording,
                    (false, _) if OVERFLOW_HIDDEN_ROWS_SHOWN => MenuAction::HeaderUpdates,
                    (false, _) => MenuAction::HeaderFeedback,
                };
                assert_eq!(items.last().copied(), Some(want_last));
            }
        }
    }
}

/// Whether this build shows the recorder rows — the same gate
/// [`window_items`] applies, said once for the tests below.
fn recording_rows_exist() -> bool {
    OVERFLOW_HIDDEN_ROWS_SHOWN
        && cfg!(windows)
        && koru_channel::ReleaseChannel::this_build_is_internal()
}

/// The recorder's rows are an internal-Windows affair: present there, with a
/// divider between them and the public menu, and absent everywhere else. The
/// row reads Start or Stop from the state the menu was opened in, because a
/// menu cannot relabel between press and release.
#[test]
fn the_recording_rows_follow_their_gate() {
    let items = window_items(false, false, false);
    if recording_rows_exist() {
        let at = items
            .iter()
            .position(|a| *a == MenuAction::HeaderStartRecording)
            .expect("the row exists where the gate is open");
        assert_eq!(
            items[at - 1],
            MenuAction::Divider,
            "set off from the public rows"
        );
        assert_eq!(
            MenuAction::HeaderStartRecording.shortcut_id(),
            Some(koru_keys::ShortcutId::ToggleRecording),
            "the row teaches the chord that opens the same modal"
        );
        assert_eq!(
            MenuAction::HeaderStartRecording.icon(),
            Some(koru_draw::icons::RECORD)
        );
        // …and mid-lifecycle the same slot is the stop square on the same
        // chord.
        let items = window_items(false, false, true);
        let at = items
            .iter()
            .position(|a| *a == MenuAction::HeaderStopRecording)
            .expect("the stop row stands in the same slot");
        assert_eq!(items[at - 1], MenuAction::Divider);
        assert_eq!(
            MenuAction::HeaderStopRecording.shortcut_id(),
            Some(koru_keys::ShortcutId::ToggleRecording)
        );
        assert_eq!(
            MenuAction::HeaderStopRecording.icon(),
            Some(koru_draw::icons::STOP)
        );
    } else {
        assert!(!items.contains(&MenuAction::HeaderStartRecording));
        assert!(!items.contains(&MenuAction::HeaderStopRecording));
    }
}

#[test]
#[cfg(windows)]
fn window_placement_stays_in_the_direct_os_controls_not_the_overflow() {
    let top = window_items(false, false, false);
    assert!(!top.contains(&MenuAction::WindowArrange));
    for inner in arrange_items().iter().filter(|a| a.selectable()) {
        assert!(!top.contains(inner), "{inner:?} leaked into overflow");
    }
}

/// The two halves name the chords they stand in for, and the row that has none
/// says so with a blank rather than an invented chord.
#[test]
#[cfg(windows)]
fn the_arrange_rows_name_their_chords() {
    assert_eq!(
        MenuAction::WindowSnapLeft.shortcut(),
        cfg!(windows).then(|| "Win+Left".to_string())
    );
    assert_eq!(
        MenuAction::WindowSnapRight.shortcut(),
        cfg!(windows).then(|| "Win+Right".to_string())
    );
    assert_eq!(MenuAction::WindowResetPosition.shortcut(), None);
}

/// **Solid means on.** The pin row draws filled only while the window actually
/// is on top — which is the one place that state is still visible once the
/// caption's pin button has become a menu.
#[test]
fn only_the_pinned_state_draws_a_filled_glyph() {
    assert!(MenuAction::WindowUnpinTop.filled_icon());
    assert!(!MenuAction::WindowAlwaysOnTop.filled_icon());
    // And nothing else in any menu has opinions about weight.
    for action in window_items(false, false, false)
        .into_iter()
        .chain(TAB_ITEMS)
        .chain(trashed_items())
    {
        assert!(!action.filled_icon(), "{action:?}");
    }
}

#[test]
fn overflow_destinations_are_distinct_from_standalone_window_controls() {
    let items = window_items(false, false, false);
    assert!(items.contains(&MenuAction::HeaderSettings));
    assert!(items.contains(&MenuAction::HeaderFeedback));
    assert_eq!(
        items.contains(&MenuAction::HeaderUpdates),
        OVERFLOW_HIDDEN_ROWS_SHOWN
    );
    let labels: Vec<_> = items.into_iter().map(MenuAction::label).collect();
    assert!(!labels.iter().any(|label| label == "Minimize"));
    assert!(!labels.iter().any(|label| label == "Close Window"));
}

/// **The run menu's one row says how many runs it is about** (ADR-379). A
/// menu raised on four selected runs whose verb read *Delete run* would be a
/// promise about one of them; the count is snapshotted when the menu opens,
/// for the refusal sentence's reason.
#[test]
fn delete_run_counts_the_selection_it_was_opened_over() {
    set_run_delete_count(1);
    let one = MenuAction::DeleteRun.label().into_owned();
    assert!(!one.contains('1'), "one run keeps its plain wording: {one}");

    set_run_delete_count(4);
    let many = MenuAction::DeleteRun.label().into_owned();
    assert!(many.contains('4'), "{many}");
    assert_ne!(many, one);

    // The snapshot is left as the next menu will find it.
    set_run_delete_count(1);
    assert_eq!(MenuAction::DeleteRun.label().into_owned(), one);
}

// ------------------------------------------------------------- the zoom row

/// The row is a word and three controls, not a label with spaces in it. It was
/// the latter — `"Zoom     −     (100%)     +     ⛶"` — which drew a
/// missing-glyph box for the `⛶` and did nothing at all when pressed.
#[test]
fn the_zoom_row_is_a_label_and_its_controls_are_the_pressable_part() {
    assert_eq!(MenuAction::HeaderZoom.label(), "Zoom");
    assert!(
        !MenuAction::HeaderZoom.selectable(),
        "there is no such thing as choosing Zoom"
    );
    assert!(koru_ui::MenuItem::controls(&MenuAction::HeaderZoom).len() == 4);
    assert!(
        koru_ui::MenuItem::controls(&MenuAction::HeaderSettings).is_empty(),
        "only the stepper carries controls"
    );
}

/// The glyph and what pressing it does come from one list, so a fourth control
/// cannot arrive with no action or an action with no glyph.
#[test]
fn each_zoom_control_names_the_action_it_takes() {
    let controls = zoom_controls(1.0);
    let actions: Vec<Option<MenuAction>> = controls.iter().map(|(_, a)| *a).collect();
    assert_eq!(
        actions,
        vec![
            Some(MenuAction::ZoomOut),
            None,
            Some(MenuAction::ZoomIn),
            Some(MenuAction::ZoomActualSize),
        ],
        "the readout in the middle takes no action"
    );
    for (i, (_, expected)) in controls.iter().enumerate() {
        assert_eq!(
            control_action(MenuAction::HeaderZoom, i),
            *expected,
            "control {i}"
        );
    }
    assert_eq!(control_action(MenuAction::HeaderZoom, 4), None, "no fifth");
    assert_eq!(
        control_action(MenuAction::HeaderSettings, 0),
        None,
        "a plain row has no controls to press"
    );
}

/// The readout is the live percentage, in the same words the transient flash
/// uses — one window, one number.
#[test]
fn the_readout_reads_the_zoom_it_was_given() {
    for factor in [0.5f32, 0.9, 1.0, 1.25, 2.0] {
        let Control::Readout(value) = &zoom_controls(factor)[1].0 else {
            panic!("the middle cell is the readout");
        };
        assert_eq!(*value, koru_draw::theme::zoom_percent(factor));
    }
    let Control::Readout(hundred) = &zoom_controls(1.0)[1].0 else {
        panic!("readout");
    };
    assert_eq!(hundred, "100%");
}

/// A control at the end of its range is dim rather than gone: the range is
/// information, and a cell that vanished would move the one beside it under a
/// pointer that is stepping.
#[test]
fn the_controls_dim_at_the_ends_of_the_range_and_at_actual_size() {
    let enabled = |factor: f32| -> Vec<bool> {
        zoom_controls(factor)
            .into_iter()
            .filter_map(|(control, _)| match control {
                Control::Button { enabled, .. } => Some(enabled),
                Control::Readout(_) => None,
            })
            .collect()
    };
    use koru_draw::theme::{ZOOM_MAX, ZOOM_MIN};
    // − , + , ↺
    assert_eq!(enabled(ZOOM_MIN), vec![false, true, true], "at the floor");
    assert_eq!(enabled(ZOOM_MAX), vec![true, false, true], "at the ceiling");
    assert_eq!(
        enabled(1.0),
        vec![true, true, false],
        "nothing to come back from at 100%"
    );
    assert_eq!(enabled(1.2), vec![true, true, true], "in the middle");
}

/// The `⛶` that drew as a box was a text codepoint the UI font has no glyph
/// for. Every control now names an icon-font glyph, which is the only kind
/// this menu can actually draw.
#[test]
fn every_zoom_control_uses_an_icon_font_glyph() {
    for (control, _) in zoom_controls(1.2) {
        let Control::Button { glyph, .. } = control else {
            continue;
        };
        let cp = glyph as u32;
        assert!(
            (0xE000..=0xF8FF).contains(&cp),
            "U+{cp:04X} is outside the Private Use Area, so Phosphor has no glyph for it"
        );
    }
}

/// **A glyph is a good button and a poor label.** `−` says which way and not
/// how far; `↺` says back without saying back to what. The chip says both, and
/// teaches the chord on the way.
#[test]
fn each_zoom_control_names_itself_and_the_chord_that_does_the_same_thing() {
    use koru_keys::ShortcutId;
    for (control, id) in [
        (0, ShortcutId::ZoomOut),
        (2, ShortcutId::ZoomIn),
        (3, ShortcutId::ZoomReset),
    ] {
        let (label, chord) = control_tooltip(MenuAction::HeaderZoom, control)
            .unwrap_or_else(|| panic!("control {control} earns a tooltip"));
        assert_eq!(
            label,
            koru_keys::registry::get(id).label,
            "control {control} says what the View menu says"
        );
        assert_eq!(
            chord.as_deref(),
            Some(koru_keys::label_of(id).as_str()),
            "control {control} teaches the chord that actually dispatches"
        );
    }
}

/// The readout is not a control, so it explains nothing — and neither does a
/// row that has no controls at all.
#[test]
fn the_readout_and_a_plain_row_earn_no_control_tooltip() {
    assert_eq!(control_tooltip(MenuAction::HeaderZoom, 1), None, "readout");
    assert_eq!(control_tooltip(MenuAction::HeaderZoom, 4), None, "no fifth");
    assert_eq!(control_tooltip(MenuAction::HeaderSettings, 0), None);
}

/// The chord column and the chip read one list. They used to be one `match`
/// that only `shortcut` could see, which is how a second lookup becomes a
/// second list saying something else.
#[test]
fn the_row_chord_and_the_control_chip_name_the_same_registry_entry() {
    for action in [
        MenuAction::HeaderSettings,
        MenuAction::WindowAlwaysOnTop,
        MenuAction::Close,
        MenuAction::Rename,
    ] {
        let id = action.shortcut_id().expect("a registry chord");
        assert_eq!(action.shortcut(), Some(koru_keys::label_of(id)));
    }
    // A modified click and a shell chord are not registry entries, and must not
    // pretend to be ids.
    assert_eq!(MenuAction::OpenInNewWindow.shortcut_id(), None);
    assert!(MenuAction::OpenInNewWindow.shortcut().is_some());
    assert_eq!(MenuAction::WindowSnapLeft.shortcut_id(), None);
}

/// **The chip teaches `=`, not `+`.** Zoom in is bound to the plain `=`,
/// deliberately: `Ctrl+Plus` is `Ctrl+Shift+=`, which the editor already binds
/// to superscript, and the shell's `primary_only` guard excludes Shift. A chip
/// spelling it `+` would teach a chord that does nothing.
///
/// The **chord**, not its rendered label, because the label is the platform's
/// (`⌘=` on macOS) while which key is bound is not.
#[test]
fn the_chip_spells_zoom_in_the_way_the_chord_is_actually_bound() {
    use koru_keys::{On, ShortcutId, registry};
    assert_eq!(registry::get(ShortcutId::ZoomIn).chord.on, On::Letter('='));
    assert_eq!(registry::get(ShortcutId::ZoomOut).chord.on, On::Letter('-'));
    assert_eq!(
        registry::get(ShortcutId::ZoomReset).chord.on,
        On::Letter('0')
    );
    // And on this platform, that is what the chip says out loud.
    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(koru_keys::label_of(ShortcutId::ZoomOut), "Ctrl+-");
        assert_eq!(koru_keys::label_of(ShortcutId::ZoomIn), "Ctrl+=");
        assert_eq!(koru_keys::label_of(ShortcutId::ZoomReset), "Ctrl+0");
    }
}

/// **Canvases sit beside Documents** on the `+` menu, as a flyout of the same
/// shape: *New canvas* first, then the recent canvases — and the Documents
/// flyout keeps to pages, because the container splits the recents by kind as
/// the menu opens.
#[test]
fn the_canvases_flyout_offers_new_canvas_then_recent_canvases() {
    let page = koru_core::DocumentId::new();
    let canvas = koru_core::DocumentId::new();
    set_plus_snapshots(vec![(page, "Notes".into())], vec![], vec![], vec![]);
    set_plus_canvases(vec![(canvas, "Board".into())]);

    assert_eq!(
        submenu(MenuAction::Canvases),
        Some(vec![
            MenuAction::NewCanvas,
            MenuAction::Divider,
            MenuAction::OpenRecentDocument(canvas),
        ])
    );
    assert_eq!(MenuAction::OpenRecentDocument(canvas).label(), "Board");
    assert_eq!(
        document_plus_items(),
        vec![
            MenuAction::NewDocument,
            MenuAction::Divider,
            MenuAction::OpenRecentDocument(page),
            MenuAction::Divider,
            MenuAction::ClearRecents,
        ],
        "no canvas among the documents"
    );
    assert_eq!(MenuAction::Canvases.icon(), Some(koru_draw::icons::CANVAS));
    assert_eq!(MenuAction::NewCanvas.icon(), Some(koru_draw::icons::CANVAS));
    assert_eq!(MenuAction::NewCanvas.shortcut(), None);

    // No recent canvases: the flyout is its one verb.
    set_plus_canvases(Vec::new());
    assert_eq!(canvas_plus_items(), vec![MenuAction::NewCanvas]);
    set_plus_snapshots(vec![], vec![], vec![], vec![]);
}

/// Both rows are localized, never an English literal beside a translated one.
#[test]
fn the_canvases_rows_speak_every_language() {
    let lang = crate::format::lang();
    assert_eq!(
        MenuAction::Canvases.label(),
        koru_i18n::t(lang, koru_i18n::Key::CanvasesTitle)
    );
    assert_eq!(
        MenuAction::NewCanvas.label(),
        koru_i18n::t(lang, koru_i18n::Key::CanvasNew)
    );
}
