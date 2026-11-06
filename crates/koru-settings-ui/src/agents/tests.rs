// SPDX-License-Identifier: MPL-2.0

use crate::page::{Block, Target, Widget};
use crate::state::{AgentAttention, AgentDefaults, AgentProviderRow, AgentProviderStatus};
use crate::tests::{GLOBALS, koru, page_lines, press, switch};
use crate::{PageGroup, PageId, Request, SettingsSurface};
use koru_draw::geometry::Rect;
use koru_i18n::{Key, Lang, t};
use koru_settings::AgentPermission;
use koru_ui::NavList;

/// What a host hands over: one agent of every status.
fn rows() -> Vec<AgentProviderRow> {
    let row = |key: &str, name: &str, status: AgentProviderStatus, on: bool| AgentProviderRow {
        key: key.into(),
        name: name.into(),
        status,
        guide: Some("https://example.com/guide"),
        on,
        // The host offers *Install* for what is missing and installable;
        // here, Grok's missing CLI.
        install: key == "grok",
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
            AgentProviderStatus::NeedsAttention(AgentAttention::UnsupportedVersion {
                update: None,
                desktop_app: None,
            }),
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

/// ***Install* sits before the switch on a row the host offers it on**,
/// and only there; a press asks the host, and the switch beside it still
/// works.
#[test]
fn install_sits_before_the_switch_and_asks_the_host() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = on_page(rows());
    let install = t(Lang::En, Key::SettingsAgentInstall);
    let offering: Vec<&str> = surface
        .laid
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Card(card) => Some(card.rows.iter()),
            _ => None,
        })
        .flatten()
        .filter(|row| matches!(row.widget, Widget::Pair(..)))
        .map(|row| row.label.as_str())
        .collect();
    assert_eq!(offering, ["Grok"], "only Grok's row offers it");
    let row = surface
        .laid
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Card(card) => Some(card.rows.iter()),
            _ => None,
        })
        .flatten()
        .find(|row| row.label == "Grok")
        .expect("Grok's row");
    let Widget::Pair(button, toggle) = &row.widget else {
        panic!("a button and a switch")
    };
    let (
        Widget::Button {
            rect: at, label, ..
        },
        Widget::Toggle { rect: switch, .. },
    ) = (&**button, &**toggle)
    else {
        panic!("the button first, the switch last")
    };
    assert_eq!(label, install);
    assert!(at.x + at.w <= switch.x, "the button is before the switch");
    let right = |rect: &Rect| rect.x + rect.w;
    let claude_switch = surface
        .laid
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Card(card) => Some(card.rows.iter()),
            _ => None,
        })
        .flatten()
        .find_map(|row| match &row.widget {
            Widget::Toggle { rect, .. } if row.label == "Claude" => Some(*rect),
            _ => None,
        })
        .expect("Claude's switch");
    assert_eq!(
        right(switch),
        right(&claude_switch),
        "every switch keeps the card's right edge"
    );

    assert!(press(&mut surface, install));
    assert_eq!(
        surface.take_requests(),
        [Request::InstallAgent("grok".into())]
    );
    assert_eq!(switch_of(&surface, "Grok"), Some(true));
}

fn switch_of(surface: &SettingsSurface, row: &str) -> Option<bool> {
    switch(surface, row).map(|(_, on)| on)
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

/// The segment `word` of the segmented control on the row named `row`: its
/// hit, and whether it is the selected one.
fn segment(surface: &SettingsSurface, row: &str, word: &str) -> Option<(usize, bool)> {
    surface.laid.blocks.iter().find_map(|block| match block {
        Block::Card(card) => card.rows.iter().find_map(|r| match &r.widget {
            Widget::Segmented {
                labels,
                selected,
                first,
                ..
            } if r.label == row => labels
                .iter()
                .position(|label| label == word)
                .map(|at| (first + at, *selected == Some(at))),
            _ => None,
        }),
        _ => None,
    })
}

fn press_segment(surface: &mut SettingsSurface, row: &str, word: &str) -> bool {
    let (hit, _) = segment(surface, row, word).unwrap_or_else(|| panic!("{row} › {word}"));
    let (x, y) = surface.laid.hits[hit].rect.centre();
    surface.mouse_down(x, y - surface.scroll)
}

/// **Default permission for new chats** (D39): *Supported agents* offers
/// Manual, Auto and Yolo, *Other agents* only Manual and Yolo, both on
/// Manual; the sentence under them says plainly what Yolo means; a press
/// moves the control and asks the host to store it.
#[test]
fn the_default_permission_card_offers_its_choices_and_asks_the_host() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = on_page(rows());
    let lines = page_lines(&surface);
    for line in [
        "Default permission for new chats",
        "Supported agents",
        "Other agents",
    ] {
        assert!(lines.iter().any(|l| l == line), "{line:?} in {lines:?}");
    }
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Yolo means the agent runs everything without asking")),
        "{lines:?}"
    );
    for word in ["Manual", "Auto", "Yolo"] {
        let (_, on) = segment(&surface, "Supported agents", word).expect(word);
        assert_eq!(on, word == "Manual", "{word}");
    }
    assert!(
        segment(&surface, "Other agents", "Auto").is_none(),
        "no Auto"
    );
    assert_eq!(
        segment(&surface, "Other agents", "Manual").map(|(_, on)| on),
        Some(true)
    );

    assert!(press_segment(&mut surface, "Supported agents", "Auto"));
    assert_eq!(
        surface.take_requests(),
        [Request::SetAgentPermission {
            other: false,
            permission: AgentPermission::Auto
        }]
    );
    surface.relayout();
    assert_eq!(
        segment(&surface, "Supported agents", "Auto").map(|(_, on)| on),
        Some(true)
    );
    assert!(press_segment(&mut surface, "Other agents", "Yolo"));
    assert_eq!(
        surface.take_requests(),
        [Request::SetAgentPermission {
            other: true,
            permission: AgentPermission::Yolo
        }]
    );
    // The same choice again asks for nothing.
    surface.relayout();
    assert!(press_segment(&mut surface, "Other agents", "Yolo"));
    assert!(surface.take_requests().is_empty());

    // Restore defaults puts both back on Manual.
    assert!(press(&mut surface, "Restore defaults"));
    assert_eq!(surface.take_requests(), [Request::RestoreAgentDefaults]);
    assert_eq!(*surface.agent_defaults(), AgentDefaults::default());
}

/// The *Default agent* chips, as `(label, mark, selected, hit)`.
fn chips(surface: &SettingsSurface) -> Vec<(String, Option<String>, bool, usize)> {
    surface
        .laid
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Card(card) => Some(card.rows.iter()),
            _ => None,
        })
        .flatten()
        .find_map(|row| match &row.widget {
            Widget::Chips(chips) => Some(
                chips
                    .iter()
                    .map(|chip| {
                        (
                            chip.label.clone(),
                            chip.mark.clone(),
                            chip.selected,
                            chip.hit,
                        )
                    })
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

/// **Default agent** (D40): *Auto* first and chosen, then one chip per agent
/// that is on, each with its mark; a press asks the host; a note the host
/// gives is said under the chips; Restore defaults goes back to *Auto*.
#[test]
fn the_default_agent_chips_offer_auto_and_every_agent_that_is_on() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = on_page(rows());
    let lines = page_lines(&surface);
    assert!(lines.iter().any(|l| l == "Default agent"), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l == t(Lang::En, Key::SettingsDefaultAgentHint)),
        "{lines:?}"
    );
    let shown = chips(&surface);
    assert_eq!(
        shown
            .iter()
            .map(|(label, mark, on, _)| (label.as_str(), mark.as_deref(), *on))
            .collect::<Vec<_>>(),
        [
            ("Auto", None, true),
            ("Claude", Some("claude"), false),
            ("Grok", Some("grok"), false),
            ("Codex", Some("codex"), false),
            ("Muse", Some("muse"), false),
        ],
        "Antigravity is off, so it is no choice"
    );
    let codex = shown[3].3;
    let (x, y) = surface.laid.hits[codex].rect.centre();
    assert!(surface.mouse_down(x, y - surface.scroll));
    assert_eq!(
        surface.take_requests(),
        [Request::SetDefaultAgent(Some("codex".into()))]
    );
    surface.relayout();
    assert!(chips(&surface)[3].2, "Codex is chosen");

    // The host says the choice cannot be had; Auto shows chosen for an
    // agent that has no chip.
    surface.set_agent_defaults(AgentDefaults {
        agent: Some("antigravity".into()),
        agent_note: Some("Antigravity is turned off.".into()),
        ..AgentDefaults::default()
    });
    surface.relayout();
    assert!(chips(&surface)[0].2, "Auto");
    assert!(
        page_lines(&surface)
            .iter()
            .any(|l| l == "Antigravity is turned off.")
    );

    assert!(press(&mut surface, "Restore defaults"));
    assert_eq!(surface.take_requests(), [Request::RestoreAgentDefaults]);
    assert_eq!(surface.agent_defaults().agent, None);
}

/// The page hit whose target `want` picks.
fn hit_of(surface: &SettingsSurface, want: impl Fn(&Target) -> bool) -> usize {
    surface
        .laid
        .hits
        .iter()
        .position(|hit| want(&hit.target))
        .expect("a hit")
}

fn keys(surface: &SettingsSurface) -> Vec<&str> {
    surface
        .agent_providers()
        .iter()
        .map(|row| row.key.as_str())
        .collect()
}

/// **An agent row is dragged by its handle** (D41), the way a paragraph
/// block is: a press on the handle takes the row, the drop line follows the
/// pointer, and the release puts the row there and asks the host to store
/// the whole order. Dropping it where it was asks for nothing.
#[test]
fn an_agent_row_is_dragged_by_its_handle_to_a_new_place() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = on_page(rows());
    assert_eq!(surface.laid.drag_rows.len(), 5, "every agent row moves");
    let grip = hit_of(&surface, |target| matches!(target, Target::Grip(0)));
    let (x, y) = surface.laid.hits[grip].rect.centre();
    assert!(surface.mouse_down(x, y - surface.scroll));
    assert_eq!(surface.focused_agent(), Some("claude"));
    assert_eq!(surface.agent_drag, Some((0, 0)));
    let over = surface.laid.drag_rows[2];
    assert!(surface.mouse_move(x, over.y + over.h * 0.5 - surface.scroll));
    assert_eq!(
        surface.agent_drag,
        Some((0, 2)),
        "the drop line is at Codex"
    );
    assert!(surface.mouse_up());
    assert_eq!(
        surface.take_requests(),
        [Request::SetAgentOrder(
            ["grok", "codex", "claude", "antigravity", "muse"]
                .map(str::to_owned)
                .to_vec()
        )]
    );
    assert_eq!(
        keys(&surface),
        ["grok", "codex", "claude", "antigravity", "muse"]
    );
    surface.relayout();
    assert_eq!(
        agent_rows(&surface)
            .into_iter()
            .map(|(mark, _, _)| mark)
            .collect::<Vec<_>>(),
        ["grok", "codex", "claude", "antigravity", "muse"],
        "the page lists them so"
    );
    // Past the last row lands last; back where it began asks for nothing.
    let grip = hit_of(&surface, |target| matches!(target, Target::Grip(4)));
    let (x, y) = surface.laid.hits[grip].rect.centre();
    assert!(surface.mouse_down(x, y - surface.scroll));
    surface.mouse_move(x, y - surface.scroll + 500.0);
    surface.mouse_move(x, y - surface.scroll);
    assert!(surface.mouse_up());
    assert!(surface.take_requests().is_empty());
}

/// **The move chords move a focused row** — the same registry entries a
/// paragraph block moves with — Up and Down move the keyboard between rows,
/// Escape lets go, and the first row goes no higher.
#[test]
fn the_move_chords_move_the_focused_agent_row() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = on_page(rows());
    let chord = |id| koru_keys::registry::get(id).chord.mods;
    let up = chord(koru_keys::ShortcutId::MoveRowUp);
    let down = chord(koru_keys::ShortcutId::MoveRowDown);
    let key = |surface: &mut SettingsSurface, named, mods| {
        surface.key(&winit::keyboard::Key::Named(named), None, mods)
    };
    use winit::keyboard::{ModifiersState, NamedKey};

    // No row has the keyboard: the chord is not the page's.
    assert!(!key(&mut surface, NamedKey::ArrowDown, down));

    let row = hit_of(&surface, |target| matches!(target, Target::Row(1)));
    let rect = surface.laid.hits[row].rect;
    assert!(surface.mouse_down(rect.x + rect.w * 0.4, rect.y + 4.0 - surface.scroll));
    assert_eq!(surface.focused_agent(), Some("grok"));
    assert!(
        surface.take_requests().is_empty(),
        "a press on the row only focuses it"
    );

    assert!(key(&mut surface, NamedKey::ArrowDown, down));
    assert_eq!(
        keys(&surface),
        ["claude", "codex", "grok", "antigravity", "muse"]
    );
    assert_eq!(
        surface.take_requests(),
        [Request::SetAgentOrder(
            ["claude", "codex", "grok", "antigravity", "muse"]
                .map(str::to_owned)
                .to_vec()
        )]
    );
    assert_eq!(
        surface.focused_agent(),
        Some("grok"),
        "the row keeps the keyboard"
    );

    assert!(key(
        &mut surface,
        NamedKey::ArrowUp,
        ModifiersState::empty()
    ));
    assert_eq!(surface.focused_agent(), Some("codex"));
    assert!(key(&mut surface, NamedKey::ArrowUp, up));
    assert!(
        key(&mut surface, NamedKey::ArrowUp, up),
        "taken, and nothing moves"
    );
    assert_eq!(keys(&surface)[0], "codex");
    assert_eq!(surface.take_requests().len(), 1, "only the move that moved");

    assert!(key(&mut surface, NamedKey::Escape, ModifiersState::empty()));
    assert_eq!(surface.focused_agent(), None);
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
        .filter(|row| {
            matches!(row.widget, Widget::Toggle { .. })
                || matches!(&row.widget, Widget::Pair(_, b) if matches!(**b, Widget::Toggle { .. }))
        })
        .count();
    assert_eq!(toggles, 5);
    // *Install* is said in every language too.
    for lang in Lang::ALL {
        assert!(!t(lang, Key::SettingsAgentInstall).is_empty());
    }
}
