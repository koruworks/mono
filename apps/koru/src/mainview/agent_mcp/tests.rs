// SPDX-License-Identifier: MPL-2.0

use std::path::Path;

use super::super::context_menu::{self, MenuAction};
use super::super::harness::container;
use super::*;

fn key(name: &str) -> SessionProfileKey {
    SessionProfileKey::BuiltIn(name.parse().expect("a built-in key"))
}

#[test]
fn only_the_agents_with_no_per_launch_form_are_targets() {
    assert_eq!(Target::of(&key("grok")), Some(Target::Grok));
    assert_eq!(Target::of(&key("antigravity")), Some(Target::Antigravity));
    for other in [
        "claude", "codex", "copilot", "qwen", "opencode", "kimi", "bash",
    ] {
        assert_eq!(Target::of(&key(other)), None, "{other}");
    }
}

#[test]
fn the_command_is_the_agents_own_mcp_add_with_the_bridge_after_the_dashes() {
    let bridge = Path::new(r"C:\Program Files\Koru\koru-mcp.exe");
    assert_eq!(
        arguments(bridge, None),
        [
            "mcp",
            "add",
            "koru",
            "--",
            r"C:\Program Files\Koru\koru-mcp.exe",
            "serve",
            "--stdio"
        ]
    );
    let rooted = arguments(bridge, Some(Path::new(r"D:\dev root")));
    assert_eq!(rooted[rooted.len() - 2..], ["--root", r"D:\dev root"]);
    assert_eq!(Target::Grok.program(), "grok");
    assert_eq!(Target::Antigravity.program(), "agy");
}

#[test]
fn the_menu_row_is_offered_only_where_it_is_asked_for() {
    let with = context_menu::tab_items_for(false, false, false, false, Some(false), false);
    assert!(with.contains(&MenuAction::InstallMcp));
    let at = with
        .iter()
        .position(|a| *a == MenuAction::InstallMcp)
        .unwrap();
    assert_eq!(with[at + 1], MenuAction::Divider);
    assert_eq!(
        with.last(),
        Some(&MenuAction::PopOut),
        "the window verb stays last"
    );
    assert!(!context_menu::tab_items(false, false, false, false).contains(&MenuAction::InstallMcp));
}

#[test]
fn a_document_tab_is_never_a_target_and_nothing_is_running_at_rest() {
    let (v, _workspace) = container();
    assert_eq!(v.agent_mcp_target(0), None);
    assert!(!v.agent_mcp_running());
}

#[test]
fn a_right_click_on_a_running_session_in_the_active_flyout_finds_that_session() {
    let (mut v, _workspace) = container();
    let session = koru_session_protocol::SessionId::mint();
    let other = koru_session_protocol::SessionId::mint();
    let flyout = context_menu::ContextMenu::as_submenu(
        &mut v.engine,
        context_menu::MenuSubject::NewTab,
        koru_draw::geometry::Rect::new(200.0, 100.0, 120.0, 24.0),
        vec![
            MenuAction::OpenRunningSession(session),
            MenuAction::OpenRunningSession(other),
        ],
        v.viewport,
    );
    v.submenu = Some((0, flyout));

    let mut found = Vec::new();
    for y in (0..800).step_by(4) {
        for x in (0..1200).step_by(8) {
            if let Some(id) = v.running_session_under(x as f32, y as f32) {
                if !found.contains(&id) {
                    found.push(id);
                }
            }
        }
    }
    assert!(
        found.contains(&session) && found.contains(&other),
        "{found:?}"
    );
    assert_eq!(
        v.running_session_under(1.0, 1.0),
        None,
        "off the flyout it names nothing"
    );
    v.submenu = None;
    assert_eq!(
        v.running_session_under(260.0, 120.0),
        None,
        "no flyout, no session"
    );
}

#[test]
fn a_registered_bridge_offers_remove_in_the_same_place() {
    let items = context_menu::tab_items_for(false, false, false, false, Some(true), false);
    assert!(items.contains(&MenuAction::UninstallMcp));
    assert!(!items.contains(&MenuAction::InstallMcp));
}

#[test]
fn the_listings_of_both_agents_are_read_for_a_server_called_koru() {
    assert!(lists_koru("  koru: D:\\x\\koru-mcp.exe serve --stdio\n"));
    assert!(lists_koru(
        "NAME       TYPE   STATUS   COMMAND/URL\nkoru       stdio  enabled  C:\\k.exe\nstashbase  stdio  enabled  s.cmd\n"
    ));
    assert!(!lists_koru(
        "NAME TYPE\nkorusel stdio enabled x\nstashbase stdio enabled koru.cmd\n"
    ));
    assert!(!lists_koru(""));
}
