// SPDX-License-Identifier: MPL-2.0

//! The Welcome page: who it greets, what each row does, what is counted, and
//! that what is drawn is exactly what can be pressed.

use super::layout::Layout;
use super::rows::Section;
use super::*;
use koru_core::WorkspaceId;
use koru_draw::inspect;

/// Serializes the process-global Guide hint switch across these tests.
static GUIDE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn guide() -> std::sync::MutexGuard<'static, ()> {
    GUIDE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn doc() -> WorkspaceDoc {
    WorkspaceDoc::create(WorkspaceId::new(), "t").expect("create")
}

/// A page tall enough that nothing scrolls, so a rect's centre is a click.
fn page() -> WelcomeView {
    let mut view = WelcomeView::first_minutes("Ana Lima", "Team Roadmap");
    view.set_identity(WelcomeIdentity {
        owner: Some("Ana Lima".into()),
        workspace: "Team Roadmap".into(),
        profile: "Work".into(),
    });
    view.set_viewport(1000.0, 4000.0);
    view
}

fn laid(view: &mut WelcomeView) -> &Layout {
    view.ensure_laid()
}

fn rect_of(view: &mut WelcomeView, target: Target) -> Rect {
    laid(view)
        .hits
        .iter()
        .find(|(t, _)| *t == target)
        .unwrap_or_else(|| panic!("{target:?} is on the page"))
        .1
}

fn row_rect(view: &mut WelcomeView, row: RowId) -> Rect {
    laid(view)
        .sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .find(|r| r.id == row)
        .unwrap_or_else(|| panic!("{row:?} is drawn"))
        .rect
}

fn press(view: &mut WelcomeView, rect: Rect) -> bool {
    let (x, y) = rect.centre();
    View::mouse_down(view, &mut doc(), x, y - view.scroll, false)
}

fn key(view: &mut WelcomeView, named: NamedKey, mods: ModifiersState) -> bool {
    View::key(view, &mut doc(), &Key::Named(named), None, mods)
}

// --------------------------------------------------------- what is counted

/// *Your first five minutes* counts five rows, and Linux's app-menu row too.
fn first_minutes() -> usize {
    5 + usize::from(koru_autostart::app_menu::SUPPORTED)
}

/// **Linux's app-menu row**: on the page only where there is an app menu to
/// add Koru to, and ticked by the container once the entry is written, never
/// by the press, so a write that failed leaves it open.
#[test]
fn the_app_menu_row_asks_the_container_and_does_not_tick_itself() {
    assert_eq!(
        Section::FirstMinutes.rows().contains(&RowId::AppMenu),
        koru_autostart::app_menu::SUPPORTED
    );
    assert_eq!(RowId::from_key("app-menu"), Some(RowId::AppMenu));
    let mut view = page();
    view.activate(Target::Row(RowId::AppMenu));
    assert_eq!(view.take_wants(), vec![WelcomeWant::AddToAppMenu]);
    assert!(!view.is_done(RowId::AppMenu));
    assert!(view.take_newly_done().is_empty());
    view.set_done(["app-menu"]);
    assert!(view.is_done(RowId::AppMenu));
}

#[test]
fn only_rows_with_an_action_tick_and_only_checklist_rows_count() {
    let view = page();
    assert_eq!(
        view.progress_of(Section::FirstMinutes),
        (0, first_minutes()),
        "Learn your way around is off the page, so not counted"
    );
    assert_eq!(
        view.progress_of(Section::Further),
        (0, 2),
        "Set up your AI is off the page, so not counted"
    );
    assert_eq!(
        view.progress_of(Section::Devices),
        (0, 0),
        "devices have no count"
    );
    for inert in [RowId::TaskList, RowId::Watch, RowId::OwnApps] {
        assert!(!inert.ticks() && !inert.counted(), "{inert:?}");
    }
    assert!(RowId::Mcp.ticks() && RowId::Mcp.counted());
    for device in [RowId::Phone, RowId::Computer] {
        assert!(device.ticks() && !device.counted(), "{device:?}");
    }
    assert_eq!(
        RowId::persisted(),
        [
            "first-document",
            "documents",
            "files",
            "learn",
            "app-menu",
            "reminder",
            "feedback",
            "phone",
            "computer",
            "ai",
            "mcp",
            "terminal-agent"
        ]
    );
}

#[test]
fn inert_rows_are_drawn_without_a_tick_circle() {
    let mut view = page();
    for row in laid(&mut view).sections.iter().flat_map(|s| s.rows.iter()) {
        assert_eq!(row.tick.is_some(), row.id.ticks(), "{:?}", row.id);
    }
}

/// **A finished card stays open**: its header says *m of m done* and every
/// row is still there, with no *Show* or *Hide* to press.
#[test]
fn the_header_counts_and_a_finished_card_stays_open() {
    let mut view = page();
    let first = |view: &mut WelcomeView| {
        let section = &laid(view).sections[0];
        (
            section.progress.as_ref().map(|(text, _)| text.clone()),
            section.rows.len(),
        )
    };
    let count = first_minutes();
    let done = |n| Some(format!("{n} of {count} done"));
    assert_eq!(first(&mut view), (done(0), count));
    assert!(
        laid(&mut view).sections[1].progress.is_none(),
        "Connect your devices has no count"
    );

    // A tick stored for a row that is off the page counts for nothing.
    view.set_done(["first-document", "documents", "files", "learn"]);
    assert_eq!(first(&mut view), (done(3), count));

    view.set_done(["reminder", "feedback", "app-menu"]);
    assert_eq!(
        first(&mut view),
        (done(count), count),
        "all counted rows done: the card keeps its rows"
    );

    view.set_done(["mcp", "terminal-agent"]);
    let further = &laid(&mut view).sections[2];
    assert_eq!(
        further.progress.as_ref().map(|(text, _)| text.as_str()),
        Some("2 of 2 done")
    );
    assert_eq!(further.rows.len(), 2, "still open");
    let card = further.card;
    let last = further.rows.last().expect("a row").rect;
    assert!(
        (card.y + card.h - (last.y + last.h)).abs() < 0.5,
        "the card is as tall as its rows"
    );
    // The section headers carry nothing to press.
    let laid = laid(&mut view);
    for section in &laid.sections {
        let (top, bottom) = (section.label_y, section.label_y + layout::LABEL_H);
        assert!(
            !laid
                .hits
                .iter()
                .any(|(_, rect)| rect.y < bottom && rect.y + rect.h > top),
            "{:?} has a control in its header",
            section.section
        );
    }
}

// ------------------------------------------------------------ the actions

#[test]
fn every_action_raises_its_existing_command_and_ticks_its_row() {
    let _hints = guide();
    let cases: [(Target, Option<WelcomeWant>, Option<RowId>); 12] = [
        (Target::Customize, Some(WelcomeWant::Settings), None),
        (
            Target::Row(RowId::FirstDocument),
            Some(WelcomeWant::NewDocument),
            Some(RowId::FirstDocument),
        ),
        (Target::MicLink, Some(WelcomeWant::VoiceSettings), None),
        (
            Target::Import("markdown"),
            Some(WelcomeWant::Import("markdown")),
            Some(RowId::Documents),
        ),
        (
            Target::ChooseFiles,
            Some(WelcomeWant::ImportFiles),
            Some(RowId::Files),
        ),
        (
            Target::Row(RowId::Reminder),
            Some(WelcomeWant::Reminders),
            Some(RowId::Reminder),
        ),
        (
            Target::FeedbackForm,
            Some(WelcomeWant::Feedback),
            Some(RowId::Feedback),
        ),
        (
            Target::Email,
            Some(WelcomeWant::Email),
            Some(RowId::Feedback),
        ),
        (
            Target::Row(RowId::Computer),
            Some(WelcomeWant::LinkDevice),
            Some(RowId::Computer),
        ),
        (Target::PairQr, Some(WelcomeWant::LinkDevice), None),
        (
            Target::CopyMcp,
            Some(WelcomeWant::CopyMcpCommand(mcp::Agent::ClaudeCode)),
            Some(RowId::Mcp),
        ),
        (
            Target::Row(RowId::TerminalAgent),
            Some(WelcomeWant::Agents),
            Some(RowId::TerminalAgent),
        ),
    ];
    for (target, want, tick) in cases {
        let mut view = page();
        let rect = rect_of(&mut view, target);
        assert!(press(&mut view, rect), "{target:?} took the press");
        assert_eq!(
            view.take_wants(),
            want.into_iter().collect::<Vec<_>>(),
            "{target:?}"
        );
        assert_eq!(
            view.take_newly_done(),
            tick.map(RowId::key).into_iter().collect::<Vec<_>>(),
            "{target:?}"
        );
        assert!(view.take_wants().is_empty(), "{target:?}: taken once");
    }
}

#[test]
fn every_import_chip_names_its_registered_format() {
    let mut view = page();
    let chips: Vec<(&str, String)> = laid(&mut view)
        .sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .flat_map(|r| r.chips.iter())
        .map(|(id, label, _)| (*id, label.clone()))
        .collect();
    let labels: Vec<&str> = chips.iter().map(|(_, label)| label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Markdown",
            "Evernote",
            "TextBundle",
            "Microsoft OneNote",
            "Google Docs",
            "Notion"
        ]
    );
    for (id, _) in chips {
        let mut view = page();
        let rect = rect_of(&mut view, Target::Import(id));
        assert!(press(&mut view, rect));
        assert_eq!(view.take_wants(), [WelcomeWant::Import(id)]);
    }
}

#[test]
fn rows_with_nothing_behind_them_do_nothing() {
    let mut view = page();
    for row in [
        // These rows act only through their links, chips and buttons.
        RowId::Documents,
        RowId::Files,
        RowId::Feedback,
        RowId::Mcp,
    ] {
        let rect = row_rect(&mut view, row);
        // The left edge of the row: no link, chip or button lives there.
        let x = rect.x + 4.0;
        let y = rect.y + 4.0;
        assert!(
            !View::mouse_down(&mut view, &mut doc(), x, y, false),
            "{row:?} took a press"
        );
    }
    assert!(view.take_wants().is_empty());
    assert!(view.take_newly_done().is_empty());
}

/// *Phone*, *Watch*, *Write your own apps*, *Learn your way around*, *Turn
/// one document into a task list* and *Set up your AI* are hidden for now,
/// *Run an agent in a terminal* opens *Further* in the AI row's place, and
/// the feedback sentence offers the form and email only.
#[test]
fn the_hidden_rows_and_chat_are_not_on_the_page() {
    let mut view = page();
    let drawn: Vec<RowId> = laid(&mut view)
        .sections
        .iter()
        .flat_map(|s| s.rows.iter().map(|r| r.id))
        .collect();
    for hidden in [
        RowId::Phone,
        RowId::Watch,
        RowId::OwnApps,
        RowId::Learn,
        RowId::TaskList,
        RowId::Ai,
    ] {
        assert!(!drawn.contains(&hidden), "{hidden:?} is drawn");
        assert!(
            !laid(&mut view)
                .hits
                .iter()
                .any(|(t, _)| *t == Target::Row(hidden)),
            "{hidden:?} can be pressed"
        );
    }
    assert_eq!(
        Section::FirstMinutes.rows(),
        [
            RowId::FirstDocument,
            RowId::Documents,
            RowId::Files,
            RowId::Reminder,
            RowId::Feedback
        ]
    );
    assert_eq!(Section::Devices.rows(), [RowId::Computer]);
    assert_eq!(
        Section::Further.rows(),
        [RowId::TerminalAgent, RowId::Mcp],
        "the agent row first, where Set up your AI was"
    );
    let feedback: String = laid(&mut view)
        .sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .find(|r| r.id == RowId::Feedback)
        .expect("drawn")
        .sentence
        .iter()
        .map(|run| run.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(!feedback.contains("chat"), "{feedback}");
    assert!(feedback.contains("feedback form") && feedback.contains("email"));
}

/// **The MCP command**: the bridge beside this executable, `serve --stdio`,
/// shown on the row and copied whole by its chip and by *Copy*.
#[test]
fn the_mcp_row_shows_the_command_and_copies_it() {
    let mut view = page();
    let (shown, _) = laid(&mut view)
        .sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .find(|r| r.id == RowId::Mcp)
        .and_then(|r| r.code.clone())
        .expect("the command is drawn");
    assert!(shown.ends_with("serve --stdio"), "{shown}");
    assert_eq!(view.mcp_command, mcp::command(mcp::Agent::ClaudeCode));
    let copy_targets = laid(&mut view)
        .hits
        .iter()
        .filter(|(t, _)| *t == Target::CopyMcp)
        .count();
    assert_eq!(copy_targets, 2, "the chip and the button");
}

#[test]
fn the_mcp_command_quotes_paths_with_spaces_and_carries_the_root() {
    use std::path::Path;
    assert_eq!(
        mcp::command_line(Path::new("/opt/koru/koru-mcp"), None),
        "/opt/koru/koru-mcp serve --stdio"
    );
    assert_eq!(
        mcp::command_line(
            Path::new(r"C:\Program Files\Koru\koru-mcp.exe"),
            Some(Path::new(r"D:\dev data")),
        ),
        r#""C:\Program Files\Koru\koru-mcp.exe" serve --stdio --root "D:\dev data""#
    );
}

/// **Each agent's own `mcp add`.** The bridge goes after `--` where the
/// agent's parser wants it and without one where it refuses it, and the agents
/// that default to the current folder are told `--scope user`.
#[test]
fn each_agent_gets_its_own_registration_command() {
    use std::path::Path;
    let bridge = Path::new(r"C:\Program Files\Koru\koru-mcp.exe");
    let tail = r#""C:\Program Files\Koru\koru-mcp.exe" serve --stdio"#;
    for (agent, prefix) in [
        (
            mcp::Agent::ClaudeCode,
            "claude mcp add --scope user koru -- ",
        ),
        (mcp::Agent::Codex, "codex mcp add koru -- "),
        (mcp::Agent::Gemini, "gemini mcp add --scope user koru "),
        (mcp::Agent::Copilot, "copilot mcp add koru -- "),
        (mcp::Agent::Qwen, "qwen mcp add --scope user koru "),
        (mcp::Agent::Grok, "grok mcp add koru -- "),
        (mcp::Agent::Antigravity, "agy mcp add koru -- "),
        (mcp::Agent::Other, ""),
    ] {
        assert_eq!(
            mcp::agent_line(agent, bridge, None),
            format!("{prefix}{tail}"),
            "{agent:?}"
        );
    }
    assert!(
        mcp::agent_line(mcp::Agent::Gemini, bridge, Some(Path::new(r"D:\data")))
            .ends_with(r"serve --stdio --root D:\data")
    );
}

/// **One name for the bridge everywhere** (D55). The welcome page's
/// registrations and the launchers' `mcp` default use the name a chat's own
/// bridge goes by (`koru-session-host`'s `chat_bridge`), because agents that
/// import other tools' MCP configuration (Devin) key servers by name: the
/// same name means the chat's bridge replaces the person's, another means
/// every Koru tool twice.
#[test]
fn every_registration_uses_the_chat_bridges_name() {
    use std::path::Path;
    let name = koru_session_profiles::MCP_SERVER_NAME;
    assert_eq!(name, "koru");
    let bridge = Path::new("koru-mcp");
    for agent in mcp::Agent::ALL {
        if agent == mcp::Agent::Other {
            continue;
        }
        let line = mcp::agent_line(agent, bridge, None);
        assert!(line.contains(" mcp add "), "{agent:?}: {line}");
        let registered = line
            .split(" mcp add ")
            .nth(1)
            .and_then(|rest| {
                rest.split_whitespace()
                    .find(|word| !word.starts_with("--") && *word != "user")
            })
            .unwrap_or_default();
        assert_eq!(registered, name, "{agent:?}: {line}");
    }
    let bat = include_str!("../../../../koru.bat");
    assert!(
        bat.contains(&format!("set \"MCP_NAME={name}\"")),
        "koru.bat mcp registers under another default name"
    );
    let sh = include_str!("../../../../koru.sh");
    assert!(
        sh.contains(&format!("MCP_NAME=\"${{1:-{name}}}\"")),
        "koru.sh mcp registers under another default name"
    );
}

/// **The row asks which agent.** One chip per agent, Claude Code chosen
/// first; picking another changes the command shown and the one *Copy* puts
/// on the clipboard.
#[test]
fn picking_an_agent_changes_the_command_the_row_copies() {
    let mut view = page();
    let row_agents = |view: &mut WelcomeView| {
        laid(view)
            .sections
            .iter()
            .flat_map(|s| s.rows.iter())
            .find(|r| r.id == RowId::Mcp)
            .map(|r| r.agents.clone())
            .expect("the MCP row")
    };
    let agents = row_agents(&mut view);
    assert_eq!(agents.len(), mcp::Agent::ALL.len());
    assert_eq!(
        agents
            .iter()
            .filter(|(_, _, chosen)| *chosen)
            .map(|(a, _, _)| *a)
            .collect::<Vec<_>>(),
        vec![mcp::Agent::ClaudeCode]
    );
    assert!(
        view.mcp_command.starts_with("claude mcp add"),
        "{}",
        view.mcp_command
    );

    view.activate(Target::McpAgent(mcp::Agent::Gemini));
    assert!(
        view.mcp_command.starts_with("gemini mcp add"),
        "{}",
        view.mcp_command
    );
    assert!(
        row_agents(&mut view)
            .iter()
            .any(|(a, _, chosen)| *a == mcp::Agent::Gemini && *chosen)
    );
    view.take_wants();
    view.activate(Target::CopyMcp);
    assert_eq!(
        view.take_wants(),
        vec![WelcomeWant::CopyMcpCommand(mcp::Agent::Gemini)]
    );
}

#[test]
fn a_long_command_is_shortened_in_the_middle_to_fit() {
    let mut engine = TextEngine::new();
    let long = format!(
        "{}koru-mcp.exe serve --stdio",
        r"C:\Users\someone\AppData\Local\Programs\Koru\".repeat(4)
    );
    let max = layout::field_w(&mut engine, "koru-mcp.exe serve --stdio") + 120.0;
    let shown = layout::elide_middle(&mut engine, &long, max);
    assert!(layout::field_w(&mut engine, &shown) <= max);
    assert!(
        shown.contains('\u{2026}') && shown.ends_with("--stdio"),
        "{shown}"
    );
    assert_eq!(
        layout::elide_middle(&mut engine, "koru-mcp serve --stdio", 10_000.0),
        "koru-mcp serve --stdio",
        "what fits is left alone"
    );
}

/// **The MCP row is two lines under its text**: every agent chip on one
/// line, then the command with *Copy* at its right.
#[test]
fn the_mcp_row_puts_the_agents_on_one_line_and_copy_beside_the_command() {
    let mut view = page();
    let row = laid(&mut view)
        .sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .find(|r| r.id == RowId::Mcp)
        .expect("the MCP row");
    let strip = row.agents_strip.expect("the agents' line");
    let first_y = row.agents[0].1.y;
    assert!(
        row.agents
            .iter()
            .all(|(_, rect, _)| (rect.y - first_y).abs() < 0.01),
        "one line, never wrapped"
    );
    let (_, code) = row.code.clone().expect("the command");
    let layout::Trailing::Button(label, Target::CopyMcp, copy) = row.trailing else {
        panic!("Copy is the MCP row's button")
    };
    assert_eq!(label, MCP_COPY);
    assert!(code.y >= strip.window.y + strip.window.h, "under the chips");
    assert!(
        (code.centre().1 - copy.centre().1).abs() < 0.5,
        "Copy is on the command's line"
    );
    assert!(copy.x >= code.x + code.w, "Copy is at the command's right");
    assert!(
        (copy.x + copy.w - (strip.window.x + strip.window.w)).abs() < 0.5,
        "and ends where the text does"
    );
}

/// **A line wider than the row scrolls sideways** rather than wrapping: a
/// chip out of view cannot be pressed but keeps its place in the Tab order,
/// focus brings it into view, and the wheel over the line moves it before
/// the page.
#[test]
fn a_line_of_agents_wider_than_the_row_scrolls_sideways() {
    let strip_of = |view: &mut WelcomeView| {
        laid(view)
            .sections
            .iter()
            .flat_map(|s| s.rows.iter())
            .find_map(|r| r.agents_strip)
            .expect("the agents' line")
    };
    let mut view = page();
    view.set_viewport(420.0, 4000.0);
    let strip = strip_of(&mut view);
    assert!(strip.overflow > 0.0, "eight agents do not fit 420 px");
    assert_eq!(strip.scroll, 0.0);
    let other = rect_of(&mut view, Target::McpAgent(mcp::Agent::Other));
    assert_eq!(other.w, 0.0, "Other is out of view");
    assert!(
        view.focus_order()
            .contains(&Target::McpAgent(mcp::Agent::Other)),
        "and still reached by Tab"
    );

    // Focus brings it whole into the window.
    view.focus = Some(Target::McpAgent(mcp::Agent::Other));
    view.reveal_focus();
    let strip = strip_of(&mut view);
    assert!(strip.scroll > 0.0);
    let other = rect_of(&mut view, Target::McpAgent(mcp::Agent::Other));
    assert!(other.w > 0.0);
    assert!(other.x + other.w <= strip.window.x + strip.window.w + 0.5);
    assert!(press(&mut view, other));
    assert_eq!(view.mcp_agent, mcp::Agent::Other);

    // The wheel over the line scrolls it back, and the page stays put.
    let (x, y) = strip.window.centre();
    let page_scroll = view.scroll;
    View::mouse_move(&mut view, x, y - page_scroll);
    View::scroll_by(&mut view, MouseScrollDelta::LineDelta(0.0, 1.0));
    assert!(strip_of(&mut view).scroll < strip.scroll, "the line moved");
    assert_eq!(view.scroll, page_scroll, "the page did not");
}

/// **The pairing square is a pointer, not the code**: no taller than the
/// row beside it, so the card is one row tall, and pressed it opens *Link
/// another device* as before.
#[test]
fn the_pairing_square_fits_its_row_and_opens_link_device() {
    let mut view = page();
    let devices = laid(&mut view)
        .sections
        .iter()
        .find(|s| s.section == Section::Devices)
        .expect("Connect your devices");
    let square = devices.qr.expect("the pairing square");
    let [row] = devices.rows.as_slice() else {
        panic!("one device row")
    };
    let row = row.rect;
    assert!((devices.card.h - row.h).abs() < 0.5, "one row tall");
    assert!(square.h <= row.h && square.h <= layout::QR);
    assert!((square.w - square.h).abs() < 0.01, "square");
    assert!(square.y >= row.y && square.y + square.h <= row.y + row.h);
    assert!(
        (square.centre().1 - row.centre().1).abs() < 0.5,
        "centred on the row"
    );
    assert_eq!(rect_of(&mut view, Target::PairQr), square);
    assert!(press(&mut view, square));
    assert_eq!(view.take_wants(), [WelcomeWant::LinkDevice]);
}

// ------------------------------------------------------------- the ticks

#[test]
fn ticks_are_stable_ids_taken_once_and_merged_from_the_store() {
    let mut view = page();
    view.set_done(["reminder", "a-row-from-a-newer-build"]);
    assert!(view.is_done(RowId::Reminder));
    let rect = rect_of(&mut view, Target::Row(RowId::Reminder));
    assert!(press(&mut view, rect));
    assert!(
        view.take_newly_done().is_empty(),
        "a row the Store already has is not written again"
    );
    let rect = rect_of(&mut view, Target::Row(RowId::TerminalAgent));
    assert!(press(&mut view, rect));
    assert_eq!(view.take_newly_done(), ["terminal-agent"]);
    assert!(view.take_newly_done().is_empty());
    view.set_done([]);
    assert!(
        view.is_done(RowId::TerminalAgent),
        "a tick is never taken back"
    );
}

// ----------------------------------------------------- keyboard and scroll

#[test]
fn tab_walks_the_actions_in_page_order_and_enter_presses_the_focused_one() {
    let mut view = page();
    assert!(
        !key(&mut view, NamedKey::Enter, ModifiersState::empty()),
        "no focus yet"
    );
    assert!(key(&mut view, NamedKey::Tab, ModifiersState::empty()));
    assert_eq!(view.focus, Some(Target::Customize));
    assert!(key(&mut view, NamedKey::Tab, ModifiersState::empty()));
    assert_eq!(view.focus, Some(Target::Row(RowId::FirstDocument)));
    assert!(key(&mut view, NamedKey::Tab, ModifiersState::empty()));
    assert_eq!(view.focus, Some(Target::MicLink));
    assert!(key(&mut view, NamedKey::Enter, ModifiersState::empty()));
    assert_eq!(view.take_wants(), [WelcomeWant::VoiceSettings]);

    let order = view.focus_order();
    assert!(order.contains(&Target::Email) && order.contains(&Target::PairQr));
    assert!(key(&mut view, NamedKey::Tab, ModifiersState::SHIFT));
    assert!(key(&mut view, NamedKey::Tab, ModifiersState::SHIFT));
    assert_eq!(view.focus, Some(Target::Customize));
    assert!(key(&mut view, NamedKey::Tab, ModifiersState::SHIFT));
    assert_eq!(
        view.focus,
        order.last().copied(),
        "Shift+Tab wraps to the end"
    );
}

#[test]
fn the_page_scrolls_to_its_last_section_and_keeps_its_hits_while_scrolled() {
    let mut view = page();
    view.set_viewport(760.0, 500.0);
    View::scroll_by(
        &mut view,
        MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, -100_000.0)),
    );
    let bottom = laid(&mut view).bottom;
    assert!(bottom > 500.0, "the page is taller than the window");
    assert!(
        (view.scroll - (bottom - 500.0)).abs() < 0.01,
        "clamped to the end"
    );
    let last = laid(&mut view).sections.last().expect("a section").card;
    assert!(
        last.y + last.h <= view.scroll + 500.0,
        "the last section is on screen at the end of the scroll"
    );

    // The last section's agent row, pressed where it is shown.
    let agent = rect_of(&mut view, Target::Row(RowId::TerminalAgent));
    assert!(press(&mut view, agent));
    assert_eq!(view.take_wants(), [WelcomeWant::Agents]);
}

/// **Three lines a notch**, the step the document editor already uses.
#[test]
fn a_wheel_notch_moves_three_lines() {
    let mut view = page();
    view.set_viewport(1000.0, 400.0);
    View::scroll_by(&mut view, MouseScrollDelta::LineDelta(0.0, -1.0));
    let expected = theme::text_metrics::DOC_BODY.line_height * WHEEL_LINES;
    assert!(
        (view.scroll - expected).abs() < 0.01,
        "moved {}",
        view.scroll
    );
}

#[test]
fn focus_scrolls_the_focused_control_into_view() {
    let mut view = page();
    view.set_viewport(760.0, 400.0);
    let order = view.focus_order();
    for _ in 0..order.len() {
        assert!(key(&mut view, NamedKey::Tab, ModifiersState::empty()));
    }
    let focus = view.focus.expect("focused");
    let rect = rect_of(&mut view, focus);
    assert!(rect.y >= view.scroll && rect.y + rect.h <= view.scroll + 400.0);
}

// --------------------------------------------------- drawing and geometry

fn capture(view: &mut WelcomeView) -> inspect::Snapshot {
    let mut canvas = Scene::new();
    let root = Affine::IDENTITY;
    let (w, h) = view.viewport;
    let frame = inspect::Frame::begin(1, root, koru_draw::scene::rect(0.0, 0.0, w, h));
    view.draw_page(&mut canvas, root);
    frame.finish()
}

#[test]
fn what_is_drawn_is_exactly_what_can_be_pressed() {
    let mut view = page();
    view.set_done(["first-document"]);
    let _ = capture(&mut view);
    let hits = laid(&mut view).hits.clone();
    assert_eq!(view.drawn, hits);
    // And again while scrolled, where a drift would show first.
    view.set_viewport(760.0, 500.0);
    View::scroll_by(&mut view, MouseScrollDelta::LineDelta(0.0, -5.0));
    let _ = capture(&mut view);
    let hits = laid(&mut view).hits.clone();
    assert_eq!(view.drawn, hits);
}

#[test]
fn the_inspector_shows_one_named_tree_with_no_loose_primitives() {
    let mut view = page();
    let snapshot = capture(&mut view);
    assert!(!snapshot.truncated);
    let roots: Vec<&str> = snapshot
        .records
        .iter()
        .filter(|r| r.parent.is_none())
        .map(|r| r.name)
        .collect();
    assert_eq!(roots, ["KoruWelcome"], "one root, everything under it");
    let page = snapshot
        .records
        .iter()
        .position(|r| r.name == "KoruWelcome")
        .expect("root");
    let loose: Vec<&str> = snapshot
        .records
        .iter()
        .filter(|r| r.parent == Some(page) && r.kind == "primitive" && r.name != "fill")
        .map(|r| r.name)
        .collect();
    assert!(
        loose.is_empty(),
        "loose primitives under the page: {loose:?}"
    );
    let count = |name: &str| snapshot.records.iter().filter(|r| r.name == name).count();
    assert_eq!(count("Welcome header"), 0, "the page has no header for now");
    assert_eq!(count("Welcome section"), 3);
    assert_eq!(count("Welcome footer"), 0, "the page has no footer");
    assert_eq!(count("Welcome pair QR"), 1);
    assert_eq!(count("KoruLogo"), 0, "no header, so no mark");
    assert_eq!(count("KoruTextTitle"), 0, "no header, so no greeting");
    assert_eq!(
        count("Welcome row"),
        8,
        "every row of all three sections but the six hidden for now"
    );
    assert_eq!(count("Welcome command"), 1, "the MCP command");
    assert_eq!(count("Welcome agents"), 1, "the MCP row's line of agents");
    assert_eq!(
        count("Welcome shortcuts"),
        0,
        "Learn your way around is off"
    );
    // Every row sits inside a section, and every section inside the page.
    for (i, record) in snapshot.records.iter().enumerate() {
        if record.name == "Welcome row" {
            let parent = &snapshot.records[record.parent.expect("parented")];
            assert!(
                parent.name == "KoruCard",
                "row {i} is under {:?}",
                parent.name
            );
        }
    }
}

#[test]
fn the_column_is_centred_and_capped() {
    let mut view = page();
    view.set_viewport(2400.0, 900.0);
    let column = laid(&mut view).column;
    assert_eq!(column.w, layout::COLUMN_W, "capped");
    assert!(
        (column.x - (2400.0 - layout::COLUMN_W) / 2.0).abs() < 0.5,
        "centred"
    );
    view.set_viewport(300.0, 900.0);
    let narrow = laid(&mut view).column;
    assert!(narrow.w <= 300.0 && narrow.w > 0.0, "{}", narrow.w);
    assert!(narrow.x >= 0.0);
}

#[test]
fn a_narrow_window_wraps_sentences_instead_of_overflowing() {
    let mut view = page();
    view.set_viewport(420.0, 900.0);
    let column = laid(&mut view).column;
    for row in laid(&mut view).sections.iter().flat_map(|s| s.rows.iter()) {
        for run in &row.sentence {
            assert!(
                run.x + run.w <= column.x + column.w + 0.5,
                "{:?}: '{}' runs past the column",
                row.id,
                run.text
            );
        }
    }
}

#[test]
fn the_embedded_logo_is_square() {
    let logo = koru_draw::brand::logo_brush();
    assert_eq!(logo.image.width, logo.image.height);
    assert!(logo.image.width > 0);
}

// ------------------------------------------------- shortcuts from the registry

/// The dictation chord is hidden for now; the other chords are the
/// registry's rather than strings written here.
#[test]
fn the_shortcut_lines_leave_dictation_out_and_come_from_the_registry() {
    let _hints = guide();
    let lines = hints::hint_lines();
    assert!(
        lines.iter().all(|(_, what)| !what.contains("dictate")),
        "{lines:?}"
    );
    let (chord, _) = lines
        .iter()
        .find(|(_, what)| *what == "new tab")
        .expect("the Welcome page teaches a new tab");
    assert_eq!(
        chord,
        &koru_keys::registry::label_of(koru_keys::ShortcutId::NewTab),
        "the chord was written out rather than looked up"
    );
}

/// **Only when guide hints are on** — the key chip beside *Make your first
/// document* goes with them.
#[test]
fn the_shortcut_lines_and_key_chip_go_quiet_when_guide_hints_are_off() {
    let _hints = guide();
    koru_keys::hint::set_shown(koru_keys::Section::Guide, false);
    let quiet = hints::hint_lines();
    let chip = hints::new_document_chord();
    koru_keys::hint::set_shown(koru_keys::Section::Guide, true);
    assert!(quiet.is_empty());
    assert!(chip.is_none());
    assert_eq!(
        hints::new_document_chord().as_deref(),
        Some(koru_keys::registry::label_of(koru_keys::ShortcutId::NewTab).as_str())
    );
}

// ----------------------------------------------------------- build stamp

#[test]
fn an_unpublished_build_still_stamps_when_it_was_built() {
    assert!(koru_channel::build_info().released().is_none());
    let stamp = build_stamp().expect("a build knows when it was built");
    let when = stamp
        .strip_prefix("built ")
        .unwrap_or_else(|| panic!("{stamp}"));
    assert!(chrono::NaiveDateTime::parse_from_str(when, "%Y-%m-%d %H:%M").is_ok());
}

#[test]
fn a_published_build_shows_its_release_time_in_local_time() {
    use chrono::Local;
    let shown = build::stamp(Some("2026-08-08T14:32:11Z"), None).expect("stamped");
    let when = shown
        .strip_prefix("released ")
        .unwrap_or_else(|| panic!("{shown}"));
    let expected = chrono::DateTime::parse_from_rfc3339("2026-08-08T14:32:11Z")
        .unwrap()
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string();
    assert_eq!(when, expected);
    assert_eq!(
        build::stamp(Some("whenever"), None).as_deref(),
        Some("released whenever")
    );
    assert_eq!(build::stamp(None, None), None);
}

/// **The simplified product's Welcome** (koru-features all off): no *Task
/// list*, *Reminder*, *Koru AI* or agent row; *Further* still counts its MCP
/// row.
#[test]
fn hidden_surfaces_rows_leave_the_page() {
    use super::rows::{RowId, Section};
    koru_features::testing::with(koru_features::Features::ALL_OFF, || {
        let first = Section::FirstMinutes.rows();
        assert!(!first.contains(&RowId::TaskList));
        assert!(!first.contains(&RowId::Reminder));
        assert!(first.contains(&RowId::FirstDocument));
        let further = Section::Further.rows();
        assert_eq!(
            further,
            [RowId::Mcp],
            "the row that does not hang on a hidden surface stays"
        );
        assert!(Section::Further.counts());
        assert!(Section::FirstMinutes.counts());
    });
    assert!(Section::Further.rows().contains(&RowId::TerminalAgent));
    assert!(Section::Further.counts());
}
