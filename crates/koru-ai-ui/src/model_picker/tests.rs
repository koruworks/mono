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
        context: None,
        fast: None,
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
        let (w, h) = size(composer, rail_height(2, false));
        assert!(w <= MAX_W && w <= composer.max(RAIL_W + 160.0));
        let geometry = geometry(Rect::new(10.0, 20.0, w, h), true, true, false);
        assert_eq!(geometry.rail.w, RAIL_W);
        assert!(geometry.search.x >= geometry.rail.x + RAIL_W);
        assert!(geometry.refresh.x + geometry.refresh.w <= geometry.search.x + geometry.search.w);
        let footer = geometry.footer.unwrap();
        let context = geometry.context.unwrap();
        assert!(geometry.list.y + geometry.list.h <= context.y + 0.5);
        assert!(
            (context.y + context.h - footer.y).abs() < 0.5,
            "right above it"
        );
        assert_eq!(geometry.visible_rows(), LIST_ROWS);
        assert!(geometry.reset().unwrap().x + 24.0 <= footer.x + footer.w);
    }
    assert_eq!(size(1000.0, 0.0).0, MAX_W);
    // Never shorter than the rail needs room for.
    let (_, rail_bound) = size(360.0, rail_height(8, true));
    assert!(rail_bound >= rail_height(8, true));
}

/// D13: one size — search, six rows, the Context row and the level's
/// footer — whatever the agent lists: a short list leaves room, a long one
/// scrolls, and a model without either row gives its room to the list.
#[test]
fn the_picker_keeps_one_height_from_agent_to_agent() {
    let (_, h) = size(360.0, rail_height(2, false));
    let rows = LIST_ROWS as f32 * ROW_H;
    assert!((h - (PAD * 2.0 + SEARCH_H + rows + CONTEXT_H + FOOTER_H)).abs() < 0.5);

    let mut surface = surface();
    let mut heights = Vec::new();
    for (id, model) in [
        ("agent-claude", "sonnet"),
        ("agent-opencode", "deepseek/v4-pro"),
    ] {
        on_agent(&mut surface, id, model, ReasoningChoice::Default);
        open(&mut surface);
        heights.push(surface.picker_geometry().unwrap().panel.h);
        surface.close_picker();
    }
    assert_eq!(heights[0], heights[1], "lists of different lengths");
    assert_eq!(heights[0], h);

    let levels = geometry(Rect::new(0.0, 0.0, 360.0, h), true, false, false);
    assert!(levels.visible_rows() >= LIST_ROWS, "no Context row");
    let bare = geometry(Rect::new(0.0, 0.0, 360.0, h), false, false, false);
    assert!(bare.visible_rows() > LIST_ROWS, "neither row: more rows");
}

/// Claude's list as the runtime folds it (koru-ai D10): *Opus* carries its
/// 1M window as a context choice.
fn with_opus_context(surface: &mut ChatSurface) {
    if let Some(AgentModels::Listed(models)) = surface.agent_models.get_mut(&agent("agent-claude"))
    {
        models[1].context = Some(koru_ai_wire::AgentModelContext {
            id: "opus[1m]".into(),
            tokens: 1_000_000,
        });
    }
}

/// D10: a model with a longer window shows a Context row — *Auto* and *1M*
/// — and choosing *1M* gives the chat that window's id as its model; the
/// list keeps one *Opus*, current either way. A model without one shows no
/// row.
#[test]
fn the_context_row_chooses_which_id_the_chat_takes() {
    let mut surface = surface();
    let (reader, commands, _updates) = crate::reader::Reader::test_pair();
    surface.reader = Some(reader);
    with_opus_context(&mut surface);

    on_agent(
        &mut surface,
        "agent-claude",
        "sonnet",
        ReasoningChoice::Default,
    );
    open(&mut surface);
    assert!(surface.picker_context().is_none(), "sonnet: none");
    assert!(surface.picker_geometry().unwrap().context.is_none());
    surface.close_picker();

    on_agent(
        &mut surface,
        "agent-claude",
        "opus",
        ReasoningChoice::Default,
    );
    let chat = surface.transcript.as_ref().unwrap().chat.id;
    surface.unmade_chat = Some(chat);
    open(&mut surface);
    let geometry = surface.picker_geometry().unwrap();
    let row = geometry.context.expect("opus has a Context row");
    assert_eq!(context::window_label(koru_i18n::Lang::En, 1_000_000), "1M");
    while commands.try_recv().is_ok() {}

    surface.picker.focus = PickerFocus::Context;
    assert!(press(&mut surface, NamedKey::ArrowRight));
    let model = |surface: &ChatSurface| {
        surface
            .transcript
            .as_ref()
            .unwrap()
            .chat
            .current_model
            .requested_model
            .clone()
    };
    assert_eq!(model(&surface), "opus[1m]");
    assert!(surface.picker_context().unwrap().chosen_long);
    let rows = surface.picker_rows();
    let opus = rows
        .iter()
        .position(|row| *row == Row::Model("opus".into()))
        .unwrap();
    let entry = surface.picker_entry().unwrap();
    assert!(
        surface.picker_row_current(&entry, &rows[opus]),
        "still Opus"
    );
    assert!(
        !rows.contains(&Row::Model("opus[1m]".into())),
        "listed once"
    );

    // A press on *Auto* takes the model's own id again.
    let (_, control) = surface.context_control(row, 1_000_000);
    let auto = control.segments[0];
    assert_eq!(
        surface.picker_mouse_down(auto.x + 2.0, auto.y + 2.0),
        Some(true)
    );
    assert_eq!(model(&surface), "opus");
}

/// D10: once an agent chat has begun, its window is shown, not changed —
/// as its level is.
#[test]
fn the_context_is_read_only_once_the_chat_has_begun() {
    let mut surface = surface();
    with_opus_context(&mut surface);
    on_agent(
        &mut surface,
        "agent-claude",
        "opus[1m]",
        ReasoningChoice::Default,
    );
    let transcript = surface.transcript.as_mut().unwrap();
    let chat = transcript.chat.id;
    transcript.messages.push(koru_ai_core::Message {
        id: koru_ai_core::MessageId::new(),
        chat_id: chat,
        ordinal: 0,
        role: koru_ai_core::ChatRole::User,
        state: koru_ai_core::MessageState::Complete,
        markdown: "Plan the launch".into(),
        origin_message_id: None,
        provider: None,
        requested_model: None,
        resolved_model: None,
        error_class: None,
        created_at: koru_ai_core::Timestamp::from_millis(1),
        completed_at: None,
    });
    surface.lang = koru_i18n::Lang::En;
    open(&mut surface);
    let choice = surface.picker_context().expect("still shown");
    assert!(choice.chosen_long);
    assert!(surface.choose_context(false));
    assert_eq!(
        surface
            .transcript
            .as_ref()
            .unwrap()
            .chat
            .current_model
            .requested_model,
        "opus[1m]"
    );
    assert_eq!(
        surface.status.as_deref(),
        Some(t(koru_i18n::Lang::En, Key::AiModelContextLockedAfterStart))
    );
}
// D12: the picker opens upward, above the composer and end-aligned with
/// the chip, as the workspace and tool-approval panels do — and below the
/// chip only when the window has no room above.
#[test]
fn the_picker_opens_above_the_composer_unless_there_is_no_room() {
    let mut surface = surface();
    on_agent(
        &mut surface,
        "agent-claude",
        "sonnet",
        ReasoningChoice::Default,
    );
    // A chat under way: the composer at the window's foot.
    surface.pending_user = Some("Plan the launch".into());
    surface.composer_rect();
    open(&mut surface);
    let panel = surface.picker_geometry().unwrap().panel;
    let chip = surface.chat_model_button();
    let composer = surface.composer_hit_rect;
    assert!(panel.y + panel.h <= composer.y, "above the composer");
    assert!(
        (panel.x + panel.w - (chip.x + chip.w)).abs() < 0.5,
        "end-aligned"
    );
    surface.close_picker();

    // A composer near the window's top has no room above it: the picker
    // hangs from the chip instead.
    surface.composer_hit_rect = Rect::new(20.0, 60.0, 600.0, 100.0);
    assert!(!surface.fits_above_composer(panel.h));
    open(&mut surface);
    let low = surface.picker_geometry().unwrap().panel;
    let chip = surface.chat_model_button();
    assert!(low.y >= chip.y + chip.h, "below the chip");
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
    assert_eq!(
        surface.picker_row_label(&claude, &Row::Default),
        "Default (recommended)",
        "the agent's own default entry"
    );

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

/// **A model row raises no tooltip** (D25, the user: *don't show
/// tooltips*): the row already reads the model's name. A rail mark, an icon
/// whose agent is named nowhere else on screen, keeps its one.
#[test]
fn a_model_row_raises_no_tooltip_and_a_rail_mark_keeps_its_name() {
    let mut surface = surface();
    open(&mut surface);
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-claude")));
    let geometry = surface.picker_geometry().unwrap();
    for visible in 0..3 {
        let row = geometry.row(visible);
        assert!(
            surface
                .picker_tooltip_at(&geometry, row.x + 20.0, row.y + row.h * 0.5)
                .is_none(),
            "row {visible} raised a tooltip"
        );
    }
    let rail = surface.picker_rail();
    let mark = rail_rects(geometry.rail, rail.len(), surface.rail_has_divider(&rail))[0];
    assert!(
        surface
            .picker_tooltip_at(&geometry, mark.x + mark.w * 0.5, mark.y + mark.h * 0.5)
            .is_some(),
        "the mark is named on hover"
    );
}

/// Claude with two older models it marks as such.
fn with_legacy(surface: &mut ChatSurface) {
    if let Some(AgentModels::Listed(models)) = surface.agent_models.get_mut(&agent("agent-claude"))
    {
        let mut old = model("claude-2", "claude", Some("Claude 2"), None);
        old.legacy = true;
        models.push(old);
        let mut older = model("claude-1", "claude", Some("Claude 1"), Some("The first"));
        older.legacy = true;
        models.push(older);
    }
}

/// **Rung 6: older models live behind *Legacy models ›***, at the foot of
/// the list; a search shows them inline after the others, tagged.
#[test]
fn legacy_models_sit_behind_one_row_and_a_search_shows_them_inline() {
    let mut surface = surface();
    with_legacy(&mut surface);
    open(&mut surface);
    let claude = RailEntry::Agent(agent("agent-claude"));
    surface.picker.rail = Some(claude.clone());
    assert_eq!(
        surface.picker_rows(),
        [
            Row::Default,
            Row::Model("opus".into()),
            Row::Model("sonnet".into()),
            Row::LegacyMore,
        ]
    );
    assert_eq!(
        surface.picker_row_label(&claude, &Row::LegacyMore),
        "Legacy models"
    );
    type_query(&mut surface, "claude-");
    let rows = surface.picker_rows();
    assert_eq!(
        rows,
        [Row::Model("claude-2".into()), Row::Model("claude-1".into())],
        "inline while searching"
    );
    assert!(surface.row_is_legacy(&claude, &rows[0]));
    // Today's agents mark nothing: no Legacy row.
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-opencode")));
    surface.picker.query.set_text(&mut surface.engine, "");
    assert!(!surface.picker_rows().contains(&Row::LegacyMore));
}

/// Enter or Right on the row opens the flyout beside it; Up and Down walk
/// it, Left closes it, Enter chooses — the older model is the chat's.
#[test]
fn the_legacy_flyout_opens_walks_closes_and_chooses() {
    let mut surface = surface();
    with_legacy(&mut surface);
    open(&mut surface);
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-claude")));
    surface.picker.focus = PickerFocus::List;
    surface.picker.highlight = Some(3);
    press(&mut surface, NamedKey::ArrowRight);
    let menu = surface.picker.legacy.as_ref().expect("Right opens it");
    assert_eq!(menu.items().len(), 2);
    let parent = surface.picker_geometry().unwrap().row(3);
    let panel = menu.panel(surface.menu_viewport());
    assert!(
        panel.x >= parent.x + parent.w - 0.5 || panel.x + panel.w <= parent.x + 0.5,
        "beside the row, never over it"
    );
    press(&mut surface, NamedKey::ArrowLeft);
    assert!(surface.picker.legacy.is_none(), "Left closes it");
    press(&mut surface, NamedKey::Enter);
    assert!(surface.picker.legacy.is_some(), "Enter on the row opens it");
    press(&mut surface, NamedKey::ArrowDown);
    press(&mut surface, NamedKey::Enter);
    assert!(!surface.chat_model_open);
    assert_eq!(
        surface
            .transcript
            .as_ref()
            .unwrap()
            .chat
            .current_model
            .requested_model,
        "claude-1"
    );
}

/// At the viewport's right edge the flyout opens to the row's left.
#[test]
fn the_legacy_flyout_flips_left_at_the_viewport_edge() {
    let mut engine = TextEngine::new();
    let items = vec![LegacyItem {
        id: "claude-2".into(),
        label: "Claude 2".into(),
        current: false,
    }];
    let parent = Rect::new(500.0, 100.0, 280.0, ROW_H);
    let flipped = Menu::as_submenu(&mut engine, parent, items.clone(), (800.0, 600.0));
    assert!(flipped.panel((800.0, 600.0)).x + flipped.panel((800.0, 600.0)).w <= parent.x + 0.5);
    let roomy = Menu::as_submenu(&mut engine, parent, items, (1600.0, 600.0));
    assert!(roomy.panel((1600.0, 600.0)).x >= parent.x + parent.w - 0.5);
}

/// D11: a model that offers Fast mode shows a Fast row — *Off* and *On* —
/// reading the model's default until the chat chooses; choosing sets the
/// chat's Fast mode for its next message, also once the chat has begun. A
/// model without Fast mode shows no row, and both rows fit with the list.
#[test]
fn the_fast_row_shows_only_when_the_model_offers_it_and_stays_changeable() {
    let mut surface = surface();
    let (reader, commands, _updates) = crate::reader::Reader::test_pair();
    surface.reader = Some(reader);
    with_opus_context(&mut surface);
    if let Some(AgentModels::Listed(models)) = surface.agent_models.get_mut(&agent("agent-claude"))
    {
        models[1].fast = Some(koru_ai_wire::AgentModelFast {
            on_by_default: false,
        });
    }

    on_agent(
        &mut surface,
        "agent-claude",
        "sonnet",
        ReasoningChoice::Default,
    );
    open(&mut surface);
    assert!(surface.picker_fast().is_none());
    assert!(surface.picker_geometry().unwrap().fast.is_none());

    on_agent(
        &mut surface,
        "agent-claude",
        "opus",
        ReasoningChoice::Default,
    );
    surface.unmade_chat = None;
    let geometry = surface.picker_geometry().unwrap();
    let (fast, context) = (geometry.fast.unwrap(), geometry.context.unwrap());
    assert!(fast.y + fast.h <= context.y, "Fast sits above Context");
    assert!(geometry.list.y + geometry.list.h <= fast.y);
    assert!(!surface.picker_fast().unwrap().on, "the model's default");

    // The chat has begun: Fast mode still changes, for the next message.
    let transcript = surface.transcript.as_mut().unwrap();
    let chat = transcript.chat.id;
    transcript.messages.push(koru_ai_core::Message {
        id: koru_ai_core::MessageId::new(),
        chat_id: chat,
        ordinal: 0,
        role: koru_ai_core::ChatRole::User,
        state: koru_ai_core::MessageState::Complete,
        markdown: "Go".into(),
        origin_message_id: None,
        provider: None,
        requested_model: None,
        resolved_model: None,
        error_class: None,
        created_at: koru_ai_core::Timestamp::from_millis(1),
        completed_at: None,
    });
    while commands.try_recv().is_ok() {}
    assert!(surface.choose_fast(true));
    assert!(surface.picker_fast().unwrap().on);
    assert!(matches!(
        commands.recv_timeout(std::time::Duration::from_millis(50)).unwrap(),
        crate::reader::Command::UpdateAgentSettings(_, request)
            if request.fast == Some(true) && request.mode.is_none()
    ));
    // Right and Left on the focused row choose On and Off.
    surface.picker.focus = PickerFocus::Fast;
    assert!(press(&mut surface, NamedKey::ArrowLeft));
    assert!(!surface.picker_fast().unwrap().on);
}

/// **Codex is on the rail again, with its models, levels and modes** (the
/// user, 2026-11-02): the AI service offers it like any agent, its catalog
/// is asked for with the others, and a Codex chat then has the three
/// approval modes Koru gives it, *Ask for approval* first and the default.
#[test]
fn codex_is_offered_with_its_models_levels_and_modes() {
    let mut surface = surface();
    surface.apply_update(crate::reader::Update::Settings(settings(
        &[
            ("agent-claude", "Claude"),
            ("agent-codex", "Codex"),
            ("agent-opencode", "OpenCode"),
        ],
        None,
    )));
    surface
        .agent_status
        .insert(agent("agent-codex"), AgentStatus::Ready);
    assert!(
        surface
            .picker_rail()
            .contains(&RailEntry::Agent(agent("agent-codex")))
    );
    let mut leveled = model("gpt-5.5", "codex", Some("GPT-5.5"), None);
    leveled.reasoning = Some(ReasoningControl::Levels {
        values: vec![ReasoningLevel::Low, ReasoningLevel::XHigh],
        default: Some(ReasoningLevel::Low),
        supports_off: true,
    });
    let mode = |id: &str| koru_ai_wire::AgentModeView {
        id: id.into(),
        name: None,
        description: None,
    };
    surface.apply_update(crate::reader::Update::AgentModels(
        koru_ai_wire::AgentModelsResponse {
            provider: agent("agent-codex"),
            models: vec![leveled],
            modes: vec![mode("ask"), mode("auto-review"), mode("full-access")],
            default_mode: Some("ask".into()),
        },
    ));
    open(&mut surface);
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-codex")));
    assert!(
        surface
            .picker_rows()
            .contains(&Row::Model("gpt-5.5".into()))
    );

    on_agent(
        &mut surface,
        "agent-codex",
        "gpt-5.5",
        ReasoningChoice::Default,
    );
    let rows = surface.approval_mode_rows();
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["ask", "auto-review", "full-access"]
    );
    assert_eq!(
        rows[0].title,
        t(Lang::En, Key::AiAgentModeAskApproval),
        "Koru's own words"
    );
    assert!(rows[2].full_access);
    assert_eq!(surface.current_approval_mode().as_deref(), Some("ask"));
}

/// **An agent turned off leaves the rail, and a chat already on it keeps
/// working** (Settings → Coding → Agent providers): a new chat is offered
/// only the agents left on; a chat that began on the hidden agent still has
/// it on its rail, opens on it, and lists its models.
#[test]
fn an_agent_turned_off_leaves_the_rail_but_a_chat_on_it_keeps_it() {
    let mut surface = surface();
    assert!(surface.set_hidden_agents(["opencode".to_owned()]));
    assert!(
        !surface.set_hidden_agents(["opencode".to_owned()]),
        "no change"
    );
    assert_eq!(
        surface.picker_rail(),
        [RailEntry::Agent(agent("agent-claude"))],
        "a new chat is offered only the agents left on"
    );

    on_agent(
        &mut surface,
        "agent-opencode",
        "opencode/beam-free",
        ReasoningChoice::Default,
    );
    let transcript = surface.transcript.as_mut().unwrap();
    let chat = transcript.chat.id;
    transcript.messages.push(koru_ai_core::Message {
        id: koru_ai_core::MessageId::new(),
        chat_id: chat,
        ordinal: 0,
        role: koru_ai_core::ChatRole::User,
        state: koru_ai_core::MessageState::Complete,
        markdown: "hello".into(),
        origin_message_id: None,
        provider: None,
        requested_model: None,
        resolved_model: None,
        error_class: None,
        created_at: koru_ai_core::Timestamp::from_millis(1),
        completed_at: None,
    });
    assert!(
        surface
            .picker_rail()
            .contains(&RailEntry::Agent(agent("agent-opencode"))),
        "the chat keeps its own agent"
    );
    assert_eq!(
        surface.picker_initial_entry(),
        Some(RailEntry::Agent(agent("agent-opencode")))
    );
    open(&mut surface);
    assert!(
        surface
            .picker_rows()
            .contains(&Row::Model("opencode/beam-free".into()))
    );

    assert!(surface.set_hidden_agents(Vec::new()));
    surface.transcript = None;
    assert_eq!(surface.picker_rail().len(), 2, "restore defaults");
}

/// **The rail lists the agents in the person's order** (D41), and an agent
/// the order does not name follows the ones it does.
#[test]
fn the_rail_follows_the_agents_order() {
    let mut surface = surface();
    surface.settings = Some(settings(
        &[
            ("agent-codex", "Codex"),
            ("agent-claude", "Claude"),
            ("agent-opencode", "OpenCode"),
        ],
        None,
    ));
    let rail = |surface: &ChatSurface| surface.picker_rail();
    assert_eq!(
        rail(&surface),
        [
            RailEntry::Agent(agent("agent-codex")),
            RailEntry::Agent(agent("agent-claude")),
            RailEntry::Agent(agent("agent-opencode"))
        ],
        "no order: the service's"
    );
    assert!(surface.set_agent_order(["opencode".to_owned(), "claude".to_owned()]));
    assert_eq!(
        rail(&surface),
        [
            RailEntry::Agent(agent("agent-opencode")),
            RailEntry::Agent(agent("agent-claude")),
            RailEntry::Agent(agent("agent-codex"))
        ]
    );
    assert!(
        !surface.set_agent_order(["opencode".to_owned(), "claude".to_owned()]),
        "no change"
    );
}

/// **A new chat starts with the default agent** (D40), not the agent last
/// used; *Auto* — no default — keeps the last used, and so does a default
/// that is turned off or not installed on this computer.
#[test]
fn a_new_chat_starts_with_the_default_agent_and_falls_back_to_auto() {
    let mut surface = surface();
    surface.settings = Some(settings(
        &[("agent-claude", "Claude"), ("agent-opencode", "OpenCode")],
        Some("agent-claude"),
    ));
    let started_on = |surface: &mut ChatSurface| {
        surface.transcript = None;
        assert!(surface.create_chat());
        surface
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.current_model.provider.clone())
    };
    assert_eq!(
        started_on(&mut surface),
        Some(agent("agent-claude")),
        "Auto: the last used"
    );

    let opencode = crate::AgentDefaults {
        agent: Some("opencode".into()),
        ..crate::AgentDefaults::default()
    };
    assert!(surface.set_agent_defaults(opencode.clone()));
    assert!(!surface.set_agent_defaults(opencode), "no change");
    assert_eq!(started_on(&mut surface), Some(agent("agent-opencode")));
    surface.transcript = None;
    assert_eq!(
        surface.picker_initial_entry(),
        Some(RailEntry::Agent(agent("agent-opencode"))),
        "the picker opens on it"
    );

    // Turned off: Auto.
    surface.set_hidden_agents(["opencode".to_owned()]);
    assert_eq!(started_on(&mut surface), Some(agent("agent-claude")));
    surface.set_hidden_agents(Vec::new());
    // Not installed: Auto.
    surface
        .agent_status
        .insert(agent("agent-opencode"), AgentStatus::NotInstalled);
    assert_eq!(started_on(&mut surface), Some(agent("agent-claude")));
    surface
        .agent_status
        .insert(agent("agent-opencode"), AgentStatus::Ready);
    assert_eq!(started_on(&mut surface), Some(agent("agent-opencode")));
    // An agent this service does not list: Auto.
    surface.set_agent_defaults(crate::AgentDefaults {
        agent: Some("grok".into()),
        ..crate::AgentDefaults::default()
    });
    assert_eq!(started_on(&mut surface), Some(agent("agent-claude")));
}

/// **A + ends the agent rail and opens Settings on Agent providers** (the
/// user, 2026-11-02): only where the host offers that page, one square after
/// the last mark; it raises no tooltip and is named for a screen reader
/// instead; a click closes the picker and asks the host.
#[test]
fn the_rails_plus_asks_the_host_for_agent_providers() {
    let mut surface = surface();
    open(&mut surface);
    let geometry = surface.picker_geometry().unwrap();
    assert_eq!(surface.picker_manage_rect(&geometry), None, "not offered");

    surface.offer_agent_settings(true);
    let geometry = surface.picker_geometry().unwrap();
    let plus = surface
        .picker_manage_rect(&geometry)
        .expect("the + is drawn");
    let rail = surface.picker_rail();
    let marks = rail_rects(geometry.rail, rail.len(), surface.rail_has_divider(&rail));
    let last = marks.last().unwrap();
    assert!(plus.y > last.y + last.h - 0.5, "after the last mark");
    assert!(geometry.rail.contains(plus.x + 1.0, plus.y + plus.h - 1.0));
    assert!(
        surface
            .picker_tooltip_at(&geometry, plus.x + plus.w * 0.5, plus.y + plus.h * 0.5)
            .is_none(),
        "no tooltip"
    );
    surface.lang = Lang::En;
    assert_eq!(surface.picker_manage_name(), "Manage agent providers");

    assert!(!surface.take_agent_settings_request());
    assert_eq!(
        surface.picker_mouse_down(plus.x + plus.w * 0.5, plus.y + plus.h * 0.5),
        Some(true)
    );
    assert!(!surface.chat_model_open, "the picker closes");
    assert!(surface.take_agent_settings_request());
    assert!(!surface.take_agent_settings_request(), "asked once");
}
