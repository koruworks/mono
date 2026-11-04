// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::ChatSurface;
use crate::reader::Reader;
use koru_ai_core::{ChatId, InferenceAttemptId, ModelSelection, ReasoningChoice};
use koru_i18n::Lang;
use std::time::Duration;
use vello::Scene;
use vello::kurbo::Affine;

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
    let mut surface = ChatSurface::new();
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
    let row = surface.approval_mode_row_rect(full_access).unwrap();
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

/// **The chip comes once the agent's modes do** — even when the first
/// listing came without them. The session host reads an agent's modes with
/// its probe, seconds after it starts, and answers its models from disk at
/// once; the window used to ask once, as it opened, and showed no chip until
/// the model picker asked again. Now the shown chat's agent is asked again,
/// a little later, until its modes come or the asks run out.
#[test]
fn the_chip_comes_when_the_modes_come_after_the_models() {
    let (mut surface, commands) = surface();
    surface.agent_modes.clear();
    let claude = agent("agent-claude");
    let start = std::time::Instant::now();
    let asks = |commands: &std::sync::mpsc::Receiver<Command>| {
        let mut asked = 0;
        while let Ok(command) = commands.recv_timeout(Duration::from_millis(20)) {
            if matches!(command, Command::DiscoverModels(ref of) if *of == agent("agent-claude")) {
                asked += 1;
            }
        }
        asked
    };
    let answer = |surface: &mut ChatSurface, modes: Vec<AgentModeView>| {
        surface.apply_update(crate::reader::Update::AgentModels(
            koru_ai_wire::AgentModelsResponse {
                provider: agent("agent-claude"),
                models: Vec::new(),
                modes,
                default_mode: Some("default".into()),
                levels_per_turn: false,
            },
        ));
    };

    assert!(surface.modes_deadline().is_some(), "owed at once");
    surface.follow_agent_modes(start);
    assert_eq!(asks(&commands), 1);
    surface.follow_agent_modes(start);
    assert_eq!(asks(&commands), 0, "not while the ask is out");
    // The host has not probed yet: models, no modes.
    answer(&mut surface, Vec::new());
    assert!(surface.approval_mode_button().is_none());
    surface.follow_agent_modes(start + Duration::from_millis(500));
    assert_eq!(asks(&commands), 0, "not again at once");
    let next = surface.modes_deadline().expect("asked again later");
    assert!(next >= start + MODES_ASK_AGAIN);
    surface.follow_agent_modes(start + MODES_ASK_AGAIN);
    assert_eq!(asks(&commands), 1);
    // Probed: the modes come, the chip with them, and nothing more is asked.
    answer(&mut surface, claude_modes().modes);
    assert!(surface.approval_mode_button().is_some());
    assert_eq!(mode_of(&surface).as_deref(), Some("default"));
    assert!(surface.modes_deadline().is_none());
    surface.follow_agent_modes(start + MODES_ASK_AGAIN * 4);
    assert_eq!(asks(&commands), 0);
    assert!(surface.agent_modes.contains_key(&claude));
}

/// An agent that offers no modes is asked a few times, then left alone; and
/// a chat that is not an agent's owes nothing.
#[test]
fn an_agent_with_no_modes_is_not_asked_forever() {
    let (mut surface, commands) = surface();
    surface.agent_modes.clear();
    let start = std::time::Instant::now();
    for ask in 0..u32::from(MODES_ASKS) + 3 {
        surface.follow_agent_modes(start + MODES_ASK_AGAIN * ask);
        surface.apply_update(crate::reader::Update::AgentModels(
            koru_ai_wire::AgentModelsResponse {
                provider: agent("agent-claude"),
                models: Vec::new(),
                modes: Vec::new(),
                default_mode: None,
                levels_per_turn: false,
            },
        ));
    }
    let mut asked = 0;
    while let Ok(command) = commands.recv_timeout(Duration::from_millis(20)) {
        if matches!(command, Command::DiscoverModels(_)) {
            asked += 1;
        }
    }
    assert_eq!(asked, usize::from(MODES_ASKS));
    assert!(surface.modes_deadline().is_none());

    surface
        .transcript
        .as_mut()
        .unwrap()
        .chat
        .current_model
        .provider = AiProviderId::parse(AiProviderId::OPENAI).unwrap();
    surface.modes_asked.clear();
    assert!(surface.modes_deadline().is_none());
}

/// **The composer's toolbar for a Claude chat** holds, left to right, the
/// spaces the chat may read, the approval-mode chip naming its mode, the
/// model chip and the send button — each where it is drawn and hit, none
/// over another. No globe: web search is an OpenAI model's.
#[test]
fn a_claude_chats_toolbar_holds_spaces_mode_model_and_send() {
    let (mut surface, _commands) = surface();
    let mut scene = vello::Scene::new();
    surface.draw(&mut scene, vello::kurbo::Affine::IDENTITY);
    assert_eq!(surface.tool_access_label(), "0 spaces");
    assert_eq!(
        surface
            .current_approval_row()
            .map(|row| row.title)
            .as_deref(),
        Some("Ask first")
    );
    assert!(surface.web_search_button().is_none());
    let toolbar = surface.composer_toolbar_rect();
    let spaces = surface.tool_access_button();
    let mode = surface.approval_mode_button().expect("the mode chip");
    let model = surface.chat_model_button();
    let send = surface.send_button();
    assert_eq!(spaces.x, toolbar.x, "the spaces start the row");
    for (left, right) in [(spaces, mode), (mode, model), (model, send)] {
        assert!(left.w > 0.0 && right.w > 0.0);
        assert!(left.x + left.w <= right.x, "{left:?} runs into {right:?}");
    }
    assert!(
        mode.w >= surface.approval_mode_chip_w.min(toolbar.w * 0.4),
        "the chip is as wide as its words"
    );
}

/// The open list with the pointer on *Ask first* and the keyboard on
/// *Plan*: a lit row each way, the ✓ on the current mode, and *Full access*
/// in the notice ink.
fn open_list() -> ChatSurface {
    let (mut surface, _commands) = surface();
    surface.approval_mode_open = true;
    surface.approval_mode_focus = Some(2);
    let row = surface.approval_mode_row_rect(0).expect("the first row");
    surface.pointer = (row.x + 20.0, row.y + row.h * 0.5);
    surface
}

/// The frame node at `index`'s first descendant named `name`.
fn descendant<'a>(
    frame: &'a koru_draw::el::Frame,
    index: usize,
    name: &str,
) -> Option<&'a koru_draw::el::Node> {
    let under = |mut at: usize| loop {
        match frame.nodes[at].parent {
            Some(parent) if parent == index => return true,
            Some(parent) => at = parent,
            None => return false,
        }
    };
    frame
        .nodes
        .iter()
        .enumerate()
        .find(|(at, node)| node.name == name && under(*at))
        .map(|(_, node)| node)
}

/// **The rows on elements sit where the arithmetic put them** (D105). Each
/// row is 46 high and 4 inside the panel; its icon starts 10 in and 8 down,
/// its title 34 in and 5 down, its line 2 under the title, both elided 30
/// short of the row's end; the ✓'s lane ends 10 short of it. The panel is
/// the rows and 4 round them.
#[test]
fn every_row_sits_where_the_arithmetic_put_it() {
    let surface = open_list();
    let panel = surface.approval_mode_panel().expect("open");
    let rows = surface.approval_mode_rows();
    let (_, frame) = surface.approval_mode_frame().expect("open");
    let near = |a: f32, b: f32, what: &str| assert!((a - b).abs() < 0.01, "{what}: {a}, not {b}");
    near(
        panel.h,
        rows.len() as f32 * MODE_ROW_H + PANEL_PAD * 2.0,
        "panel height",
    );
    let profile = koru_draw::theme::Profile::Desktop;
    near(elements::PANEL_INSET.token.px(profile), PANEL_PAD, "inset");
    near(elements::ROW_HEIGHT.value, MODE_ROW_H, "row");
    let line = koru_draw::theme::role::row().metrics.line_height;
    let rows_at: Vec<usize> = frame
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.name == elements::MODE_ROW)
        .map(|(at, _)| at)
        .collect();
    assert_eq!(rows_at.len(), rows.len());
    for (index, (at, mode)) in rows_at.iter().zip(&rows).enumerate() {
        let rect = frame.nodes[*at].rect;
        let what = |part: &str| format!("{} {part}", mode.id);
        near(rect.x, panel.x + PANEL_PAD, &what("x"));
        near(
            rect.y,
            panel.y + PANEL_PAD + index as f32 * MODE_ROW_H,
            &what("y"),
        );
        near(rect.w, panel.w - PANEL_PAD * 2.0, &what("w"));
        near(rect.h, MODE_ROW_H, &what("h"));
        let icon = descendant(&frame, *at, elements::MODE_ICON).unwrap().rect;
        near(icon.x, rect.x + 10.0, &what("icon x"));
        near(icon.y + 8.0, rect.y + 8.0, &what("icon y"));
        near(icon.h, 8.0 + 12.0, &what("icon box"));
        let title = descendant(&frame, *at, elements::MODE_TITLE).unwrap().rect;
        near(title.x, rect.x + 34.0, &what("title x"));
        near(title.y, rect.y + 5.0, &what("title y"));
        near(
            title.x + title.w,
            rect.x + rect.w - 30.0,
            &what("title end"),
        );
        if mode.help.is_some() {
            let help = descendant(&frame, *at, elements::MODE_HELP).unwrap().rect;
            near(help.x, rect.x + 34.0, &what("help x"));
            near(help.y, rect.y + 5.0 + line + 2.0, &what("help y"));
            near(help.x + help.w, rect.x + rect.w - 30.0, &what("help end"));
        }
        let lane = descendant(&frame, *at, elements::CHECK_LANE).unwrap().rect;
        near(lane.x + lane.w, rect.x + rect.w - 10.0, &what("lane end"));
    }
    // The ✓ on the current mode only, at its lane's end, in the row's middle.
    let checks: Vec<_> = frame.named(elements::CHECK).collect();
    assert_eq!(checks.len(), 1);
    let current = surface.approval_mode_row_rect(0).unwrap();
    let check = checks[0].rect;
    near(check.x + check.w, current.x + current.w - 10.0, "✓ end");
    near(
        check.y + check.h * 0.5,
        current.y + current.h * 0.5,
        "✓ middle",
    );
}

/// **Everything drawn inside the list nests under its `FloatingPanel`**
/// (D105, the user on the inspector: *all the ask-for-approval UI elements
/// should be inside the floating panel*). Drawn with the inspector on,
/// every record — the shadow and the body, each row's plate, icon, words
/// and ✓ — descends from the panel, through the element tree's rows.
#[test]
fn everything_drawn_inside_the_list_nests_under_the_floating_panel() {
    use koru_draw::inspect;
    let mut surface = open_list();
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0),
    );
    let mut scene = Scene::new();
    surface.draw_approval_mode_picker(&mut scene, Affine::IDENTITY);
    let snapshot = capture.finish();
    let records = &snapshot.records;
    let named = |name: &str| {
        records
            .iter()
            .enumerate()
            .filter(|(_, r)| r.name == name)
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    };
    let under = |mut index: usize, ancestor: usize| loop {
        match records[index].parent {
            Some(parent) if parent == ancestor => return true,
            Some(parent) => index = parent,
            None => return false,
        }
    };
    let panels = named("FloatingPanel");
    assert_eq!(panels.len(), 1, "one panel");
    let panel = panels[0];
    assert_eq!(records[panel].parent, None);
    for (index, record) in records.iter().enumerate() {
        if index != panel {
            assert!(
                under(index, panel),
                "{} · {} lies outside the FloatingPanel",
                record.name,
                record.kind
            );
        }
    }
    // The tree: the list, a row a mode, each row's icon and words.
    let list = named(elements::PANEL);
    assert_eq!(list.len(), 1);
    assert_eq!(records[list[0]].parent, Some(panel));
    let rows = named(elements::MODE_ROW);
    assert_eq!(rows.len(), surface.approval_mode_rows().len());
    for row in &rows {
        assert_eq!(records[*row].parent, Some(list[0]));
    }
    for part in [
        elements::MODE_ICON,
        elements::MODE_TITLE,
        elements::MODE_WORDS,
    ] {
        let found = named(part);
        assert_eq!(found.len(), rows.len(), "{part}");
        assert!(
            found
                .iter()
                .all(|at| rows.iter().any(|row| under(*at, *row)))
        );
    }
    assert_eq!(named(elements::CHECK).len(), 1);
    let value = |index: usize, name: &str| {
        records[index]
            .properties
            .iter()
            .find(|property| property.name == name)
            .map(|property| property.value.clone())
            .unwrap_or_default()
    };
    // The pointer's row and the keyboard's row are on the hover plate.
    let lit: Vec<usize> = rows
        .iter()
        .copied()
        .filter(|row| value(*row, "fill colour").starts_with("Surface::Hover"))
        .collect();
    assert_eq!(
        lit,
        vec![rows[0], rows[2]],
        "the pointer's and the keyboard's"
    );
    // *Full access* in the notice ink, the rest in the primary.
    let full = surface
        .approval_mode_rows()
        .iter()
        .position(|row| row.full_access)
        .unwrap();
    for (index, title) in named(elements::MODE_TITLE).into_iter().enumerate() {
        let ink = value(title, "ink");
        if index == full {
            assert!(ink.starts_with("Ink::Notice -> #"), "{ink}");
        } else {
            assert!(ink.starts_with("Ink::Primary -> #"), "{ink}");
        }
    }
}

/// The pointer lights a row and the hover reports it, from the frame's
/// targets; between rows and outside the panel there is none.
#[test]
fn the_pointer_finds_the_row_the_frame_draws() {
    let mut surface = open_list();
    surface.approval_mode_focus = None;
    for index in 0..surface.approval_mode_rows().len() {
        let row = surface.approval_mode_row_rect(index).unwrap();
        assert_eq!(
            surface.approval_mode_hover_at(row.x + 1.0, row.y + 1.0),
            Some((17, index))
        );
        assert_eq!(
            surface.approval_mode_hover_at(row.x + row.w - 1.0, row.y + row.h - 1.0),
            Some((17, index))
        );
    }
    let panel = surface.approval_mode_panel().unwrap();
    assert_eq!(
        surface.approval_mode_hover_at(panel.x + 1.0, panel.y + 1.0),
        None
    );
}
