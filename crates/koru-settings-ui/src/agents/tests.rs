// SPDX-License-Identifier: MPL-2.0

use crate::page::{Block, Widget};
use crate::state::{AgentAttention, AgentProviderRow, AgentProviderStatus};
use crate::tests::{GLOBALS, koru, page_lines, press, switch};
use crate::{PageGroup, PageId, Request, SettingsSurface};
use koru_draw::geometry::Rect;
use koru_i18n::{Key, Lang, t};
use koru_ui::NavList;

/// What a host hands over: one agent of every status.
fn rows() -> Vec<AgentProviderRow> {
    let row = |key: &str, name: &str, status: AgentProviderStatus, on: bool| AgentProviderRow {
        key: key.into(),
        name: name.into(),
        status,
        guide: Some("https://example.com/guide"),
        on,
    };
    vec![
        row("claude", "Claude", AgentProviderStatus::Ready, true),
        row(
            "grok",
            "Grok",
            AgentProviderStatus::NotInstalled {
                program: "grok".into(),
            },
            true,
        ),
        row(
            "codex",
            "Codex",
            AgentProviderStatus::NeedsAttention(AgentAttention::UnsupportedVersion),
            true,
        ),
        row(
            "antigravity",
            "Antigravity",
            AgentProviderStatus::NeedsAttention(AgentAttention::AdapterMissing {
                program: "agy_acp_server".into(),
            }),
            false,
        ),
        row("muse", "Muse", AgentProviderStatus::Checking, true),
    ]
}

fn on_page(rows: Vec<AgentProviderRow>) -> SettingsSurface {
    let mut surface = koru();
    assert!(surface.set_agent_providers(rows));
    assert!(surface.show_page(PageId::AgentProviders));
    surface.relayout();
    surface
}

/// The agent rows on the showing page, as `(mark, name, status)`.
fn agent_rows(surface: &SettingsSurface) -> Vec<(String, String, String)> {
    surface
        .laid
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Card(card) => Some(card.rows.iter()),
            _ => None,
        })
        .flatten()
        .filter_map(|row| {
            Some((
                row.mark.clone()?,
                row.label.clone(),
                row.hint.clone().unwrap_or_default(),
            ))
        })
        .collect()
}

/// **The page lists every agent the host gave it, with its status in plain
/// words**: its mark, its name, *Ready*, *Not installed — `grok` isn't on
/// PATH.*, *Needs attention* and why, a *Setup guide ↗* link each, its switch
/// — and the header's *Restore defaults*, *Refresh status* and *N of M
/// enabled*.
#[test]
fn the_page_lists_every_agent_with_its_status() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let surface = on_page(rows());
    let lines = page_lines(&surface);
    for line in [
        "Agent providers",
        "Choose which coding agents Koru shows, and check their CLI tools.",
        "Restore defaults",
        "Enabled agents",
        "Refresh status",
        "4 of 5 enabled",
        t(Lang::En, Key::SettingsEnabledAgentsHint),
    ] {
        assert!(lines.iter().any(|l| l == line), "{line:?} in {lines:?}");
    }
    assert_eq!(
        agent_rows(&surface),
        [
            ("claude", "Claude", "Ready"),
            ("grok", "Grok", "Not installed — `grok` isn't on PATH."),
            (
                "codex",
                "Codex",
                "Needs attention — This version isn't one Koru supports yet."
            ),
            (
                "antigravity",
                "Antigravity",
                "Needs attention — `agy_acp_server` isn't on PATH."
            ),
            ("muse", "Muse", "Checking…"),
        ]
        .map(|(a, b, c)| (a.to_owned(), b.to_owned(), c.to_owned()))
    );
    assert_eq!(
        lines.iter().filter(|line| *line == "Setup guide ↗").count(),
        5
    );
    for (name, on) in [("Claude", true), ("Antigravity", false)] {
        assert_eq!(switch(&surface, name).map(|(_, on)| on), Some(on), "{name}");
    }
}

/// A switch moves at once and asks the host to store it; the count follows.
#[test]
fn a_switch_turns_an_agent_off_and_asks_the_host() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = on_page(rows());
    let (hit, on) = switch(&surface, "Grok").expect("Grok's switch");
    assert!(on);
    let rect = surface.laid.hits[hit].rect;
    let (x, y) = rect.centre();
    assert!(surface.mouse_down(x, y - surface.scroll));
    assert_eq!(
        surface.take_requests(),
        [Request::SetAgentEnabled {
            key: "grok".into(),
            on: false
        }]
    );
    surface.relayout();
    assert_eq!(switch(&surface, "Grok").map(|(_, on)| on), Some(false));
    assert!(page_lines(&surface).iter().any(|l| l == "3 of 5 enabled"));
}

/// **Restore defaults turns every agent on**, here and through the host.
#[test]
fn restore_defaults_turns_every_agent_on() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = on_page(rows());
    assert!(press(&mut surface, "Restore defaults"));
    assert_eq!(surface.take_requests(), [Request::RestoreAgentDefaults]);
    assert!(surface.agent_providers().iter().all(|row| row.on));
    surface.show_page(PageId::AgentProviders);
    surface.relayout();
    assert!(page_lines(&surface).iter().any(|l| l == "5 of 5 enabled"));
}

/// *Refresh status* and *Setup guide ↗* are the host's to perform.
#[test]
fn refresh_and_the_setup_guide_ask_the_host() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = on_page(rows());
    assert!(press(&mut surface, "Refresh status"));
    assert_eq!(surface.take_requests(), [Request::RefreshAgentStatus]);
    assert!(press(&mut surface, "Setup guide ↗"));
    assert_eq!(
        surface.take_requests(),
        [Request::OpenAgentGuide("https://example.com/guide")]
    );
}

/// **The sidebar puts the page under a Coding heading**, below the others,
/// and a press on it shows the page.
#[test]
fn the_page_sits_under_the_coding_heading_and_opens_from_the_sidebar() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = on_page(rows());
    surface.show_page(PageId::Appearance);
    surface.relayout();
    let listed = surface.listed();
    let runs = surface.nav_runs(&listed);
    assert_eq!(runs.len(), 2, "the ungrouped pages, then Coding");
    assert_eq!(runs[0].group, None);
    assert_eq!(runs[1].group, Some(PageGroup::Coding));
    let heading = runs[1].heading.expect("a heading over Coding");
    assert!(heading.y >= runs[0].area.y + runs[0].area.h);
    assert!(runs[1].area.y > heading.y);
    let row: Rect = NavList::row_rect(runs[1].area, 0);
    let (x, y) = row.centre();
    assert!(surface.mouse_down(x, y));
    assert_eq!(surface.page(), PageId::AgentProviders);
}

/// Every word the page and its heading say exists in every language.
#[test]
fn the_page_reads_in_every_language() {
    for lang in Lang::ALL {
        assert!(!PageGroup::Coding.label(lang).is_empty());
        assert!(!PageId::AgentProviders.label(lang).is_empty());
        for status in rows().into_iter().map(|row| row.status) {
            assert!(!status.words(lang).is_empty(), "{lang:?} {status:?}");
        }
    }
    // A switch is a widget the shared toggle draws.
    let surface = on_page(rows());
    let toggles = surface
        .laid
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Card(card) => Some(card.rows.iter()),
            _ => None,
        })
        .flatten()
        .filter(|row| matches!(row.widget, Widget::Toggle { .. }))
        .count();
    assert_eq!(toggles, 5);
}
