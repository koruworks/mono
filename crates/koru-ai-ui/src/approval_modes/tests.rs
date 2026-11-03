// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::reader::Reader;
use crate::{ChatSurface, SurfaceMode};
use koru_ai_core::{ChatId, InferenceAttemptId, ModelSelection, ReasoningChoice};
use koru_i18n::Lang;
use std::time::Duration;

fn agent(id: &str) -> AiProviderId {
    AiProviderId::parse(id).unwrap()
}

fn mode(id: &str, name: Option<&str>, description: Option<&str>) -> AgentModeView {
    AgentModeView {
        id: id.into(),
        name: name.map(Into::into),
        description: description.map(Into::into),
    }
}

/// Claude Code 0.76.0's five, as its listing carries them, and one Koru
/// has no words for.
fn claude_modes() -> ListedModes {
    ListedModes {
        modes: vec![
            mode("default", Some("Manual"), None),
            mode("acceptEdits", Some("Accept edits"), None),
            mode("plan", Some("Plan"), None),
            mode("auto", Some("Auto"), None),
            mode("bypassPermissions", Some("Bypass permissions"), None),
            mode(
                "dontAsk",
                Some("Don't ask"),
                Some("Never asks; refuses instead"),
            ),
        ],
        default: Some("default".into()),
    }
}

/// A surface on a new Claude chat whose listing carries its modes, with a
/// reader whose commands the test reads.
fn surface() -> (ChatSurface, std::sync::mpsc::Receiver<Command>) {
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    surface.lang = Lang::En;
    surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
    let (reader, commands, _updates) = Reader::test_pair();
    surface.reader = Some(reader);
    surface
        .agent_modes
        .insert(agent("agent-claude"), claude_modes());
    let mut transcript = crate::unmade_transcript(ModelSelection {
        provider: agent("agent-claude"),
        requested_model: koru_ai_core::AGENT_DEFAULT_MODEL.into(),
        default_reasoning: ReasoningChoice::Default,
    });
    transcript.chat.id = ChatId::new();
    surface.transcript = Some(transcript);
    surface.composer_rect();
    (surface, commands)
}

fn shift_tab(surface: &mut ChatSurface) -> bool {
    surface.key(&Key::Named(NamedKey::Tab), None, ModifiersState::SHIFT)
}

fn chosen(commands: &std::sync::mpsc::Receiver<Command>) -> Option<String> {
    match commands.recv_timeout(Duration::from_millis(50)).ok()? {
        Command::UpdateAgentSettings(_, request) => request.mode,
        _ => None,
    }
}

fn mode_of(surface: &ChatSurface) -> Option<String> {
    surface.current_approval_mode()
}

/// **The agent's modes in Koru's words** (D7): Claude's five worded and
/// iconed, *Full access* marked, and one Koru does not know shown with the
/// agent's own name and description and a neutral icon.
#[test]
fn known_modes_are_worded_and_the_rest_keep_the_agents_own() {
    let rows = rows(Lang::En, &agent("agent-claude"), &claude_modes());
    let titles: Vec<_> = rows.iter().map(|row| row.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Ask first",
            "Accept edits",
            "Plan",
            "Auto",
            "Full access",
            "Don't ask"
        ]
    );
    assert_eq!(rows[0].help.as_deref(), Some("Asks before every change."));
    assert_eq!(rows[0].icon, icons::QUESTION);
    assert!(rows[4].full_access && rows[4].icon == icons::WARNING);
    assert!(rows.iter().filter(|row| row.full_access).count() == 1);
    assert_eq!(rows[5].help.as_deref(), Some("Never asks; refuses instead"));
    assert_eq!(rows[5].icon, icons::FADERS);

    // Codex's three, worded as Koru gives them; OpenCode's personas.
    let codex = ListedModes {
        modes: vec![
            mode("ask", None, None),
            mode("auto-review", None, None),
            mode("full-access", None, None),
        ],
        default: Some("ask".into()),
    };
    let codex_rows = super::rows(Lang::En, &agent("agent-codex"), &codex);
    let titles: Vec<_> = codex_rows.iter().map(|row| row.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Ask for approval", "Approve for me", "Full access"]
    );
    assert!(codex_rows[2].full_access);
    let opencode = ListedModes {
        modes: vec![mode("build", None, None), mode("plan", None, None)],
        default: Some("build".into()),
    };
    let titles: Vec<_> = super::rows(Lang::En, &agent("agent-opencode"), &opencode)
        .into_iter()
        .map(|row| row.title)
        .collect();
    assert_eq!(titles, ["Build", "Plan"]);
}

/// Every language words every mode: none falls back to English by accident.
#[test]
fn every_language_words_the_modes() {
    for lang in Lang::ALL {
        let rows = rows(lang, &agent("agent-claude"), &claude_modes());
        assert!(rows.iter().all(|row| !row.title.is_empty()));
        assert!(rows[..5].iter().all(|row| row.help.is_some()));
    }
}

/// **Shift+Tab steps through every mode but Full access**, wrapping, and
/// from Full access goes back to the first.
#[test]
fn the_cycle_skips_full_access_and_wraps() {
    let rows = rows(Lang::En, &agent("agent-claude"), &claude_modes());
    let next = |current: &str| next_mode(&rows, Some(current)).unwrap().id.clone();
    assert_eq!(next("default"), "acceptEdits");
    assert_eq!(next("auto"), "dontAsk", "past Full access");
    assert_eq!(next("dontAsk"), "default");
    assert_eq!(next("bypassPermissions"), "default");
    assert!(next_mode(&[], None).is_none());
}

/// The chip shows the agent's reported default until a mode is chosen, and
/// there is no chip for a chat whose agent lists no modes.
#[test]
fn the_chip_names_the_current_mode_and_is_absent_without_modes() {
    let (mut surface, _commands) = surface();
    assert_eq!(mode_of(&surface).as_deref(), Some("default"));
    assert!(surface.approval_mode_button().is_some());
    surface.agent_modes.clear();
    assert!(surface.approval_mode_button().is_none());
    assert!(
        !shift_tab(&mut surface),
        "with no modes the key is left as it was"
    );
}

/// **Shift+Tab in the composer** chooses the next mode for the chat and
/// never lands on Full access. No note repeats the mode's name beside the
/// chip (D65): the chip names it.
#[test]
fn shift_tab_chooses_the_next_mode_and_the_chip_names_it() {
    let (mut surface, commands) = surface();
    // The chat is not made yet: the stand-in keeps the choice.
    surface.unmade_chat = surface.transcript.as_ref().map(|t| t.chat.id);
    assert!(shift_tab(&mut surface));
    assert_eq!(mode_of(&surface).as_deref(), Some("acceptEdits"));
    assert_eq!(
        surface.copy_feedback.label(),
        None,
        "the chip's label is the mode's name; no tooltip repeats it"
    );
    assert!(commands.try_recv().is_err(), "nothing reaches the service");
    surface.unmade_chat = None;
    for expected in ["plan", "auto", "dontAsk", "default"] {
        assert!(shift_tab(&mut surface));
        assert_eq!(chosen(&commands).as_deref(), Some(expected));
    }
    assert!(
        surface
            .approval_mode_rows()
            .iter()
            .all(|row| !row.full_access || mode_of(&surface).as_deref() != Some(&row.id))
    );
}

/// **A mode chosen during a reply applies to the next message**, and the
/// note beside the chip says so — whether by Shift+Tab or by a press.
#[test]
fn a_mode_chosen_during_a_reply_says_it_applies_to_the_next_message() {
    let (mut surface, commands) = surface();
    surface.unmade_chat = None;
    surface.attempt = Some(InferenceAttemptId::new());
    assert!(shift_tab(&mut surface));
    assert_eq!(
        surface.copy_feedback.label(),
        Some("Accept edits · applies to your next message")
    );
    assert_eq!(chosen(&commands).as_deref(), Some("acceptEdits"));

    surface.copy_feedback.clear();
    assert!(surface.choose_approval_mode("plan"));
    assert_eq!(
        surface.copy_feedback.label(),
        Some("Plan · applies to your next message")
    );
    // With no reply running, a press says nothing.
    surface.attempt = None;
    surface.copy_feedback.clear();
    assert!(surface.choose_approval_mode("default"));
    assert_eq!(surface.copy_feedback.label(), None);
}

/// **The `@` picker keeps the key**: while it is open, Shift+Tab is the
/// composer's, and the mode stays — the composer is always asked first.
#[test]
fn the_at_picker_keeps_shift_tab() {
    let (mut surface, _commands) = surface();
    surface.key(
        &Key::Character("@".into()),
        Some("@"),
        ModifiersState::empty(),
    );
    assert!(surface.composer.picker_open());
    shift_tab(&mut surface);
    assert_eq!(mode_of(&surface).as_deref(), Some("default"));
}

/// The chip opens the list above the composer; a row chooses its mode and
/// closes the list; Escape closes it too; Full access is only ever chosen
/// by a press.
#[test]
fn the_list_opens_upward_and_a_press_chooses() {
    let (mut surface, commands) = surface();
    surface.unmade_chat = None;
    let chip = surface.approval_mode_button().unwrap();
    assert_eq!(
        surface.approval_mode_mouse_down(chip.x + 2.0, chip.y + 2.0),
        Some(true)
    );
    let panel = surface.approval_mode_panel().expect("open");
    assert!(
        panel.y + panel.h <= surface.composer_hit_rect.y,
        "it opens upward"
    );
    let full_access = surface
        .approval_mode_rows()
        .iter()
        .position(|row| row.full_access)
        .unwrap();
    let row = surface.approval_mode_row_rect(panel, full_access);
    assert_eq!(
        surface.approval_mode_mouse_down(row.x + 20.0, row.y + 10.0),
        Some(true)
    );
    assert_eq!(chosen(&commands).as_deref(), Some("bypassPermissions"));
    assert!(surface.approval_mode_panel().is_none());

    surface.approval_mode_mouse_down(chip.x + 2.0, chip.y + 2.0);
    assert!(surface.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty()));
    assert!(surface.approval_mode_panel().is_none());
}

fn codex_modes() -> ListedModes {
    ListedModes {
        modes: vec![
            mode("ask", Some("Ask for approval"), None),
            mode("auto-review", Some("Approve for me"), None),
            mode("full-access", Some("Full access"), None),
        ],
        default: Some("ask".into()),
    }
}

fn opencode_modes() -> ListedModes {
    ListedModes {
        modes: vec![mode("build", None, None), mode("plan", None, None)],
        default: Some("build".into()),
    }
}

/// **The mapping, every agent by every setting** (D39): Claude, Codex and
/// OpenCode as they list their modes, and an agent Koru has no table for
/// that spells full access one of the known ways.
#[test]
fn the_default_maps_onto_every_agents_own_modes() {
    use NewChatPermission::{Auto, Manual, Yolo};
    let map = |agent_id: &str, listed: &ListedModes, supported, other| {
        default_mode(&agent(agent_id), listed, supported, other)
    };
    let claude = claude_modes();
    let codex = codex_modes();
    let opencode = opencode_modes();
    // (agent, its modes, supported setting, other setting, the mode)
    let cases: [(
        &str,
        &ListedModes,
        NewChatPermission,
        NewChatPermission,
        &str,
    ); 12] = [
        ("agent-claude", &claude, Manual, Yolo, "default"),
        ("agent-claude", &claude, Auto, Manual, "auto"),
        ("agent-claude", &claude, Yolo, Manual, "bypassPermissions"),
        ("agent-codex", &codex, Manual, Yolo, "ask"),
        ("agent-codex", &codex, Auto, Manual, "auto-review"),
        ("agent-codex", &codex, Yolo, Manual, "full-access"),
        // OpenCode has no auto mode: it reads the second setting, and has
        // no full-access mode either.
        ("agent-opencode", &opencode, Yolo, Manual, "build"),
        ("agent-opencode", &opencode, Manual, Yolo, "build"),
        ("agent-opencode", &opencode, Auto, Auto, "build"),
        ("agent-opencode", &opencode, Yolo, Yolo, "build"),
        ("agent-claude", &claude, Manual, Manual, "default"),
        ("agent-codex", &codex, Manual, Manual, "ask"),
    ];
    for (agent_id, listed, supported, other, expected) in cases {
        assert_eq!(
            map(agent_id, listed, supported, other).as_deref(),
            Some(expected),
            "{agent_id} {supported:?}/{other:?}"
        );
    }
    // An agent with no table, no auto mode, and a `yolo` mode: the second
    // setting, by the alias.
    let muse = ListedModes {
        modes: vec![mode("normal", None, None), mode("YOLO", None, None)],
        default: Some("normal".into()),
    };
    assert_eq!(
        map("agent-muse", &muse, Manual, Yolo).as_deref(),
        Some("YOLO")
    );
    assert_eq!(
        map("agent-muse", &muse, Yolo, Manual).as_deref(),
        Some("normal"),
        "Manual is the default it reports"
    );
    // Lists none: no mode to start in.
    assert_eq!(map("agent-kimi", &ListedModes::default(), Yolo, Yolo), None);
    // Which agents are the supported ones is decided by their modes.
    assert!(has_auto_mode(&agent("agent-claude"), &claude));
    assert!(has_auto_mode(&agent("agent-codex"), &codex));
    assert!(!has_auto_mode(&agent("agent-opencode"), &opencode));
}

/// **Yolo falls back to Manual where full access is not offered** — Claude
/// lists `bypassPermissions` only where it may — and Manual never lands on
/// a mode that runs everything, even one the agent reports as its own.
#[test]
fn yolo_falls_back_where_full_access_is_not_advertised() {
    let mut claude = claude_modes();
    claude.modes.retain(|mode| mode.id != "bypassPermissions");
    assert_eq!(
        default_mode(
            &agent("agent-claude"),
            &claude,
            NewChatPermission::Yolo,
            NewChatPermission::Manual
        )
        .as_deref(),
        Some("default")
    );
    let reckless = ListedModes {
        modes: vec![mode("full-access", None, None), mode("careful", None, None)],
        default: Some("full-access".into()),
    };
    assert_eq!(
        default_mode(
            &agent("agent-grok"),
            &reckless,
            NewChatPermission::Manual,
            NewChatPermission::Manual
        )
        .as_deref(),
        Some("careful")
    );
}

/// **A new chat starts from the setting, not from the last mode used** —
/// the chats list says nothing about it any more — and a Yolo default does
/// start a chat in Full access, because the person chose it. Its first
/// message makes the chat with that mode.
#[test]
fn a_new_chat_starts_from_the_default_not_the_last_mode_used() {
    let (mut surface, commands) = surface();
    let template = surface.transcript.as_ref().unwrap().chat.clone();
    let claude = |mode: Option<&str>| {
        let mut chat = template.clone();
        chat.id = ChatId::new();
        chat.agent_mode = mode.map(Into::into);
        chat
    };
    let model = template.current_model.clone();

    surface.chats = vec![claude(Some("plan")), claude(Some("acceptEdits"))];
    surface.begin_unmade_chat(model.clone());
    assert_eq!(mode_of(&surface).as_deref(), Some("default"), "Manual");

    surface.set_agent_defaults(crate::AgentDefaults {
        permission: NewChatPermission::Auto,
        ..crate::AgentDefaults::default()
    });
    surface.begin_unmade_chat(model.clone());
    assert_eq!(mode_of(&surface).as_deref(), Some("auto"));

    surface.set_agent_defaults(crate::AgentDefaults {
        permission: NewChatPermission::Yolo,
        ..crate::AgentDefaults::default()
    });
    surface.begin_unmade_chat(model);
    assert_eq!(
        surface
            .transcript
            .as_ref()
            .unwrap()
            .chat
            .agent_mode
            .as_deref(),
        Some("bypassPermissions"),
        "an explicit Yolo carries into a new chat"
    );
    assert!(surface.make_unmade_chat());
    assert!(matches!(
        commands.recv_timeout(Duration::from_millis(50)).unwrap(),
        Command::Create(_)
    ));
    assert_eq!(chosen(&commands).as_deref(), Some("bypassPermissions"));
    // Shift+Tab still never steps to it.
    let rows = surface.approval_mode_rows();
    assert_eq!(
        next_mode(&rows, Some("auto")).map(|row| row.id.as_str()),
        Some("dontAsk")
    );
}

/// A new chat begun before its agent listed its modes takes its default when
/// they arrive; one whose mode was chosen meanwhile keeps the choice.
#[test]
fn a_chat_begun_before_its_modes_are_listed_takes_its_default_on_arrival() {
    let (mut surface, _commands) = surface();
    surface.set_agent_defaults(crate::AgentDefaults {
        permission: NewChatPermission::Auto,
        ..crate::AgentDefaults::default()
    });
    let claude = agent("agent-claude");
    let listed = surface.agent_modes.remove(&claude).unwrap();
    let model = surface
        .transcript
        .as_ref()
        .unwrap()
        .chat
        .current_model
        .clone();
    surface.begin_unmade_chat(model.clone());
    assert_eq!(surface.transcript.as_ref().unwrap().chat.agent_mode, None);
    surface.agent_modes.insert(claude.clone(), listed.clone());
    surface.adopt_default_mode(&claude);
    assert_eq!(mode_of(&surface).as_deref(), Some("auto"));

    surface.agent_modes.remove(&claude);
    surface.begin_unmade_chat(model);
    if let Some(transcript) = surface.transcript.as_mut() {
        transcript.chat.agent_mode = Some("plan".into());
    }
    surface.agent_modes.insert(claude.clone(), listed);
    surface.adopt_default_mode(&claude);
    assert_eq!(mode_of(&surface).as_deref(), Some("plan"));
}

/// The registry's chord is the one the chat answers.
#[test]
fn the_registry_chord_is_shift_tab_in_the_chat_layer() {
    let entry = koru_keys::registry::get(koru_keys::ShortcutId::CycleAgentMode);
    assert_eq!(entry.layer, koru_keys::Layer::Chat);
    assert!(
        entry
            .chord
            .matches(&Key::Named(NamedKey::Tab), None, ModifiersState::SHIFT)
    );
    assert_eq!(
        entry.shares_chord_with,
        Some(koru_keys::ShortcutId::Outdent)
    );
}

/// Devin's five, as `devin acp` lists them (D51): *Code* is its default.
fn devin_modes() -> ListedModes {
    ListedModes {
        modes: vec![
            mode(
                "accept-edits",
                Some("Code"),
                Some("Writes code and edits files"),
            ),
            mode("smart", Some("Smart"), Some("Approves what is safe")),
            mode("ask", Some("Ask"), Some("Answers questions; never edits")),
            mode("plan", Some("Plan"), Some("Plans before it acts")),
            mode(
                "bypass",
                Some("Bypass Permissions"),
                Some("Runs without asking"),
            ),
        ],
        default: Some("accept-edits".into()),
    }
}

/// **Devin's modes are read for what they are** (D51): its `ask` answers
/// questions and never edits, so it is not *Ask first* and keeps Devin's
/// own words; its `bypass` is *Full access* — warning colour, never a
/// Shift+Tab stop; *Manual* starts it in `accept-edits`, as it has no mode
/// that asks before an edit; *Auto* is `smart` and *Yolo* is `bypass`.
#[test]
fn devins_modes_map_ask_bypass_and_manual_correctly() {
    use NewChatPermission::{Auto, Manual, Yolo};
    let devin = agent("agent-devin");
    let listed = devin_modes();
    assert_ne!(role_of(&devin, "ask"), Role::Ask, "ask is not Manual");
    assert_eq!(role_of(&devin, "ask"), Role::Other);
    assert_eq!(role_of(&devin, "bypass"), Role::Full);
    assert!(is_full_access(&devin, "bypass"));
    assert_eq!(role_of(&devin, "smart"), Role::Auto);
    assert!(has_auto_mode(&devin, &listed));

    for other in [Manual, Auto, Yolo] {
        assert_eq!(
            default_mode(&devin, &listed, Manual, other).as_deref(),
            Some("accept-edits"),
            "Manual, whatever the other setting"
        );
    }
    assert_eq!(
        default_mode(&devin, &listed, Auto, Manual).as_deref(),
        Some("smart")
    );
    assert_eq!(
        default_mode(&devin, &listed, Yolo, Manual).as_deref(),
        Some("bypass")
    );

    let rows = rows(Lang::En, &devin, &listed);
    let titles: Vec<_> = rows.iter().map(|row| row.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Accept edits", "Auto", "Ask", "Plan", "Full access"]
    );
    assert_eq!(
        rows[2].help.as_deref(),
        Some("Answers questions; never edits"),
        "Devin's own line"
    );
    assert_eq!(
        rows[0].help.as_deref(),
        Some("Writes code and edits files"),
        "Koru's name, Devin's own explanation"
    );
    assert!(rows[4].full_access && rows[4].icon == icons::WARNING);
    assert_eq!(rows.iter().filter(|row| row.full_access).count(), 1);
    // Shift+Tab never steps onto it.
    let mut at = Some("plan");
    for _ in 0..rows.len() * 2 {
        let next = next_mode(&rows, at).unwrap();
        assert!(!next.full_access);
        at = Some(next.id.as_str());
    }
}
