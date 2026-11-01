// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{ChatSurface, SurfaceMode};
use koru_ai_core::{ChatId, ModelSelection, ReasoningLevel};
use koru_ai_wire::{AgentModelView, AgentProviderView, ProviderSettingsResponse};

fn agent(id: &str) -> AiProviderId {
    AiProviderId::parse(id).unwrap()
}

fn settings(agents: &[(&str, &str)], selected: Option<&str>) -> ProviderSettingsResponse {
    ProviderSettingsResponse {
        selected_provider: selected.map(agent),
        openai: koru_ai_wire::OpenAiSettingsView {
            model: None,
            reasoning: ReasoningChoice::Default,
            credential_configured: false,
        },
        ollama: koru_ai_wire::OllamaSettingsView {
            endpoint: "http://localhost:11434".into(),
            model: None,
            reasoning: ReasoningChoice::Default,
        },
        agents: agents
            .iter()
            .map(|(id, label)| AgentProviderView {
                id: agent(id),
                label: (*label).into(),
            })
            .collect(),
    }
}

fn model(id: &str, group: &str, name: Option<&str>, description: Option<&str>) -> AgentModelView {
    AgentModelView {
        id: id.into(),
        group: group.into(),
        name: name.map(Into::into),
        description: description.map(Into::into),
        group_name: None,
        legacy: false,
        reasoning: None,
    }
}

/// A surface with Claude and OpenCode offered, both ready, and lists for
/// both: Claude's flat, OpenCode's in two groups.
fn surface() -> ChatSurface {
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
    surface.settings = Some(settings(
        &[("agent-claude", "Claude"), ("agent-opencode", "OpenCode")],
        None,
    ));
    for id in ["agent-claude", "agent-opencode"] {
        surface.agent_status.insert(agent(id), AgentStatus::Ready);
    }
    surface.agent_models.insert(
        agent("agent-claude"),
        AgentModels::Listed(vec![
            model(
                "default",
                "claude",
                Some("Default (recommended)"),
                Some("Opus 4.7 · Most capable"),
            ),
            model("opus", "claude", Some("Opus 4.7"), None),
            model(
                "sonnet",
                "claude",
                Some("Sonnet 4.6"),
                Some("Fast and able"),
            ),
        ]),
    );
    surface.agent_models.insert(
        agent("agent-opencode"),
        AgentModels::Listed(vec![
            model("opencode/beam-free", "opencode", Some("Beam Free"), None),
            model("deepseek/v4-pro", "deepseek", None, None),
        ]),
    );
    surface.composer_rect();
    surface
}

fn open(surface: &mut ChatSurface) {
    assert!(surface.open_model_picker());
}

fn type_query(surface: &mut ChatSurface, text: &str) {
    for ch in text.chars() {
        let key = WinitKey::Character(ch.to_string().into());
        surface.picker_key(&key, Some(&ch.to_string()), ModifiersState::empty());
    }
}

fn press(surface: &mut ChatSurface, key: NamedKey) -> bool {
    surface
        .picker_key(&WinitKey::Named(key), None, ModifiersState::empty())
        .unwrap()
}

fn on_agent(surface: &mut ChatSurface, id: &str, model: &str, reasoning: ReasoningChoice) {
    let mut transcript = crate::unmade_transcript(ModelSelection {
        provider: agent(id),
        requested_model: model.into(),
        default_reasoning: reasoning,
    });
    transcript.chat.id = ChatId::new();
    surface.transcript = Some(transcript);
}

#[test]
fn the_geometry_fits_narrow_wide_and_middling_composers() {
    for composer in [280.0, 360.0, 1000.0] {
        let (w, h) = size(composer, 4, rail_height(2, false), true);
        assert!(w <= MAX_W && w <= composer.max(RAIL_W + 160.0));
        let geometry = geometry(Rect::new(10.0, 20.0, w, h), true);
        assert_eq!(geometry.rail.w, RAIL_W);
        assert!(geometry.search.x >= geometry.rail.x + RAIL_W);
        assert!(geometry.refresh.x + geometry.refresh.w <= geometry.search.x + geometry.search.w);
        let footer = geometry.footer.unwrap();
        assert!(geometry.list.y + geometry.list.h <= footer.y + 0.5);
        assert_eq!(geometry.visible_rows(), 4);
        assert!(geometry.reset().unwrap().x + 24.0 <= footer.x + footer.w);
    }
    assert_eq!(size(1000.0, 40, 0.0, false).0, MAX_W);
    // The list never shows more than nine rows, and never fewer than the
    // rail needs room for.
    let (_, tall) = size(360.0, 40, 0.0, false);
    assert!((tall - (SEARCH_H + VISIBLE_ROWS as f32 * ROW_H + PAD * 2.0)).abs() < 0.5);
    let (_, rail_bound) = size(360.0, 1, rail_height(8, true), false);
    assert!(rail_bound >= rail_height(8, true));
}

#[test]
fn the_rail_stacks_its_marks_with_a_divider_after_a_built_in_provider() {
    let rail = Rect::new(0.0, 0.0, RAIL_W, 400.0);
    let plain = rail_rects(rail, 3, false);
    let divided = rail_rects(rail, 3, true);
    assert_eq!(plain[0], divided[0]);
    assert!(divided[1].y - plain[1].y >= RAIL_DIVIDER - 0.5);
    assert!(plain.iter().all(|rect| rect.w == RAIL_BUTTON));
}

/// §9.4: every term must be found, in any of the fields, without case.
#[test]
fn search_terms_all_match_across_label_id_group_and_description() {
    let fields = ["Sonnet 4.6", "sonnet", "Claude", "Fast and able"];
    assert!(matches("", &fields));
    assert!(matches("son", &fields));
    assert!(matches("SON fast", &fields), "terms across fields");
    assert!(matches("claude able", &fields));
    assert!(!matches("son opus", &fields), "every term must match");
}

#[test]
fn the_highlight_steps_over_rows_that_cannot_be_chosen() {
    let choosable = [false, true, false, true];
    assert_eq!(step_highlight(&choosable, None, 1), Some(1));
    assert_eq!(step_highlight(&choosable, Some(1), 1), Some(3));
    assert_eq!(step_highlight(&choosable, Some(3), 1), None);
    assert_eq!(step_highlight(&choosable, Some(3), -1), Some(1));
    assert_eq!(step_highlight(&choosable, None, -1), Some(3));
    assert_eq!(scroll_into_view(12, 0, 9), 4);
    assert_eq!(scroll_into_view(2, 4, 9), 2);
    assert_eq!(scroll_into_view(5, 4, 9), 4);
}

/// §9.3: the level as a word; *(default)* after the model's stated
/// default; *Default* for a model that states none.
#[test]
fn the_footer_reads_the_level_and_says_when_it_is_the_default() {
    let stated = ReasoningControl::Levels {
        values: vec![ReasoningLevel::Low, ReasoningLevel::High],
        default: Some(ReasoningLevel::High),
        supports_off: false,
    };
    let unstated = ReasoningControl::Levels {
        values: vec![ReasoningLevel::Low, ReasoningLevel::High],
        default: None,
        supports_off: false,
    };
    assert_eq!(
        footer_words(&stated, ReasoningChoice::Default, Lang::En),
        ("High".into(), Some("(default)".into()))
    );
    assert_eq!(
        footer_words(&unstated, ReasoningChoice::Default, Lang::En),
        ("Default".into(), None)
    );
    assert_eq!(
        footer_words(
            &unstated,
            ReasoningChoice::Level(ReasoningLevel::Low),
            Lang::En
        ),
        ("Low".into(), None)
    );
}

/// §9.1: the chat's agent; else the default agent from settings; else the
/// first ready one.
#[test]
fn the_rail_opens_on_the_chats_agent_then_the_default_then_the_first_ready() {
    let mut surface = surface();
    surface
        .agent_status
        .insert(agent("agent-claude"), AgentStatus::NotInstalled);
    assert_eq!(
        surface.picker_initial_entry(),
        Some(RailEntry::Agent(agent("agent-opencode"))),
        "the first ready"
    );
    surface.settings = Some(settings(
        &[("agent-claude", "Claude"), ("agent-opencode", "OpenCode")],
        Some("agent-claude"),
    ));
    assert_eq!(
        surface.picker_initial_entry(),
        Some(RailEntry::Agent(agent("agent-claude"))),
        "the default agent from settings"
    );
    on_agent(
        &mut surface,
        "agent-opencode",
        "opencode/beam-free",
        ReasoningChoice::Default,
    );
    assert_eq!(
        surface.picker_initial_entry(),
        Some(RailEntry::Agent(agent("agent-opencode"))),
        "the chat's own"
    );
}

/// §9.2: the list for each state of the selected agent.
#[test]
fn the_list_reads_each_agent_state() {
    let mut surface = surface();
    open(&mut surface);
    let claude = RailEntry::Agent(agent("agent-claude"));
    surface.picker.rail = Some(claude.clone());
    assert_eq!(
        surface.picker_rows(),
        [
            Row::Default,
            Row::Model("opus".into()),
            Row::Model("sonnet".into())
        ],
        "a flat list: no heading"
    );
    let (label, more) = surface.picker_row_label(&claude, &Row::Default);
    assert_eq!(
        label, "Default (recommended)",
        "the agent's own default entry"
    );
    assert_eq!(more.as_deref(), Some("Opus 4.7 · Most capable"));

    surface.picker.rail = Some(RailEntry::Agent(agent("agent-opencode")));
    let rows = surface.picker_rows();
    assert!(matches!(&rows[1], Row::Heading(_)), "two groups: headings");
    assert_eq!(rows.len(), 5);

    let note = |key| Row::Note(t(Lang::En, key).to_owned());
    surface.lang = Lang::En;
    surface.picker.rail = Some(claude.clone());
    surface.picker.refreshing.insert(agent("agent-claude"));
    assert_eq!(
        surface.picker_rows(),
        [note(Key::TerminalChecking)],
        "↻ under way"
    );
    surface.picker.refreshing.clear();
    surface
        .agent_models
        .insert(agent("agent-claude"), AgentModels::Failed);
    assert_eq!(
        surface.picker_rows(),
        [note(Key::AiAgentModelsFailed), Row::Default],
        "a failed listing still offers the default"
    );
    surface
        .agent_models
        .insert(agent("agent-claude"), AgentModels::Loading);
    assert_eq!(surface.picker_rows(), [note(Key::AiAgentModelsLoading)]);
    surface
        .agent_status
        .insert(agent("agent-claude"), AgentStatus::NotInstalled);
    assert_eq!(surface.picker_rows(), [note(Key::TerminalNotInstalled)]);

    // Checking with a list kept from the last run: the list, dimmed.
    surface
        .agent_status
        .insert(agent("agent-opencode"), AgentStatus::Checking);
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-opencode")));
    let rows = surface.picker_rows();
    assert_eq!(rows[0], note(Key::TerminalChecking));
    assert!(rows.len() > 1);
    assert!(
        surface.picker_choosable(&rows).iter().all(|can| !can),
        "seen, never chosen"
    );
}

/// §9.4 and S3: the query narrows the selected agent's list; an agent in
/// the rail with no match is dimmed; nothing found says so.
#[test]
fn typing_searches_the_selected_agent_and_dims_the_rest() {
    let mut surface = surface();
    surface.lang = Lang::En;
    open(&mut surface);
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-claude")));
    type_query(&mut surface, "son");
    assert_eq!(surface.picker.query.raw_text(), "son");
    assert_eq!(surface.picker_rows(), [Row::Model("sonnet".into())]);
    assert!(surface.agent_matches_query(&agent("agent-claude"), "son"));
    assert!(!surface.agent_matches_query(&agent("agent-opencode"), "son"));
    type_query(&mut surface, " able");
    assert_eq!(
        surface.picker_rows(),
        [Row::Model("sonnet".into())],
        "by description"
    );
    type_query(&mut surface, "x");
    assert_eq!(
        surface.picker_rows(),
        [Row::Note("No matching models".into())]
    );
}

/// §9.5: type *son*, Down, Enter chooses; Esc clears the query, then
/// closes; Tab walks search → rail → list → ↺ → slider.
#[test]
fn the_keyboard_alone_chooses_a_model_and_moves_through_the_picker() {
    let mut surface = surface();
    open(&mut surface);
    assert_eq!(surface.picker.focus, PickerFocus::Search);
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-claude")));
    type_query(&mut surface, "o");
    // Typing puts the keyboard on the first row the query leaves.
    let rows = surface.picker_rows();
    let first = surface.picker.highlight.unwrap();
    assert!(matches!(rows[first], Row::Default | Row::Model(_)));
    assert!(press(&mut surface, NamedKey::Escape), "clears the query");
    assert!(surface.chat_model_open);
    assert!(surface.picker.query.raw_text().is_empty());
    type_query(&mut surface, "son");
    press(&mut surface, NamedKey::ArrowDown);
    assert!(press(&mut surface, NamedKey::Enter));
    assert!(!surface.chat_model_open, "chosen, and closed");
    let chosen = &surface.transcript.as_ref().unwrap().chat.current_model;
    assert_eq!(chosen.provider, agent("agent-claude"));
    assert_eq!(chosen.requested_model, "sonnet");

    open(&mut surface);
    let order: Vec<PickerFocus> = (0..3)
        .map(|_| {
            press(&mut surface, NamedKey::Tab);
            surface.picker.focus
        })
        .collect();
    assert_eq!(
        order,
        [PickerFocus::Rail, PickerFocus::List, PickerFocus::Search],
        "no footer: sonnet offers no levels here"
    );
    press(&mut surface, NamedKey::Tab);
    press(&mut surface, NamedKey::ArrowDown);
    assert_eq!(
        surface.picker.rail,
        Some(RailEntry::Agent(agent("agent-opencode"))),
        "Down in the rail changes agent"
    );
    assert!(press(&mut surface, NamedKey::Escape));
    assert!(!surface.chat_model_open, "an empty query: Esc closes");
}

/// §9.3: no control, no footer; ↺ does nothing at *Default*; a stop or ↺
/// sets the chat's level; Left and Right move a stop.
#[test]
fn the_footer_follows_the_chats_model_and_sets_its_level() {
    let mut surface = surface();
    let levels = ReasoningControl::Levels {
        values: vec![
            ReasoningLevel::Low,
            ReasoningLevel::High,
            ReasoningLevel::Max,
        ],
        default: None,
        supports_off: false,
    };
    if let Some(AgentModels::Listed(models)) = surface.agent_models.get_mut(&agent("agent-claude"))
    {
        models[1].reasoning = Some(levels);
    }
    on_agent(
        &mut surface,
        "agent-claude",
        "sonnet",
        ReasoningChoice::Default,
    );
    open(&mut surface);
    assert!(
        surface.picker_geometry().unwrap().footer.is_none(),
        "sonnet: none"
    );
    surface.close_picker();

    on_agent(
        &mut surface,
        "agent-claude",
        "opus",
        ReasoningChoice::Default,
    );
    surface.unmade_chat = surface.transcript.as_ref().map(|t| t.chat.id);
    open(&mut surface);
    assert!(surface.picker_geometry().unwrap().footer.is_some());
    assert!(surface.reset_level(), "nothing to reset at Default");
    let level = |surface: &ChatSurface| {
        surface
            .transcript
            .as_ref()
            .unwrap()
            .chat
            .current_model
            .default_reasoning
    };
    assert_eq!(level(&surface), ReasoningChoice::Default);
    surface.picker.focus = PickerFocus::Slider;
    press(&mut surface, NamedKey::ArrowRight);
    assert_eq!(level(&surface), ReasoningChoice::Level(ReasoningLevel::Low));
    press(&mut surface, NamedKey::ArrowRight);
    assert_eq!(
        level(&surface),
        ReasoningChoice::Level(ReasoningLevel::High)
    );
    surface.picker.focus = PickerFocus::Reset;
    press(&mut surface, NamedKey::Enter);
    assert_eq!(level(&surface), ReasoningChoice::Default, "↺");
}

/// ↻ (D13): the selected agent's list rebuilt; *Checking…* until an answer
/// lands; disabled for an agent that is not installed.
#[test]
fn refresh_reads_checking_until_the_answer_lands() {
    let mut surface = surface();
    let (reader, commands, _updates) = crate::reader::Reader::test_pair();
    surface.reader = Some(reader);
    open(&mut surface);
    while commands.try_recv().is_ok() {}
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-claude")));
    assert!(surface.refresh_picker_agent());
    assert!(matches!(
        commands.try_recv().unwrap(),
        crate::reader::Command::RefreshModels(provider) if provider == agent("agent-claude")
    ));
    assert!(surface.picker.refreshing.contains(&agent("agent-claude")));
    assert!(surface.refresh_picker_agent());
    assert!(
        commands.try_recv().is_err(),
        "a second ↻ waits for the first"
    );
    surface.agent_models_answered(&agent("agent-claude"));
    assert!(surface.picker.refreshing.is_empty());

    surface
        .agent_status
        .insert(agent("agent-claude"), AgentStatus::NotInstalled);
    assert!(!surface.refresh_enabled(&agent("agent-claude")));
}

/// The wheel and a click: the list scrolls, and a press on a row chooses
/// it; a press outside closes the picker.
#[test]
fn a_click_on_a_row_chooses_it_and_one_outside_closes() {
    let mut surface = surface();
    open(&mut surface);
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-claude")));
    let geometry = surface.picker_geometry().unwrap();
    let row = geometry.row(1);
    assert_eq!(
        surface.picker_mouse_down(row.x + 20.0, row.y + row.h * 0.5),
        Some(true)
    );
    assert!(!surface.chat_model_open);
    assert_eq!(
        surface
            .transcript
            .as_ref()
            .unwrap()
            .chat
            .current_model
            .requested_model,
        "opus"
    );
    open(&mut surface);
    assert_eq!(surface.picker_mouse_down(1.0, 1.0), Some(true));
    assert!(!surface.chat_model_open);
}
