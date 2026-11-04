// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::ChatSurface;
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
                steers: false,
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
    let mut surface = ChatSurface::new();
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
        let (w, h) = size(composer, rail_height(2, 0));
        assert!(w <= MAX_W && w <= composer.max(RAIL_W + 160.0));
        let geometry = geometry(Rect::new(10.0, 20.0, w, h), true, true, false);
        assert_eq!(geometry.rail.w, RAIL_W);
        assert!(geometry.search.x >= geometry.rail.x + RAIL_W);
        assert!(geometry.refresh.x + geometry.refresh.w <= geometry.search.x + geometry.search.w);
        let footer = geometry.footer.unwrap();
        let strip = geometry.strip.unwrap();
        let context = geometry.context.unwrap();
        assert!(geometry.list.y + geometry.list.h <= footer.y + 0.5);
        assert!(strip.y + strip.h <= context.y, "the pills under the strip");
        assert!(context.y + context.h <= footer.y + footer.h);
        assert!((geometry.list.h - LIST_ROWS as f32 * ROW_H).abs() < 0.5);
        assert!(strip.x + strip.w <= footer.x + footer.w);
    }
    assert_eq!(size(1000.0, 0.0).0, MAX_W);
    // Never shorter than the rail needs room for.
    let (_, rail_bound) = size(360.0, rail_height(8, 1));
    assert!(rail_bound >= rail_height(8, 1));
}

/// The rail's marks are never flush against the content beside them: the
/// search row, the list, and every footer row start [`RAIL_CONTENT_GAP`]
/// right of the rail, and the picker keeps its one height.
#[test]
fn the_content_column_keeps_a_gap_from_the_rail() {
    assert!(RAIL_CONTENT_GAP >= 8.0);
    let (w, h) = size(360.0, rail_height(3, 1));
    let geometry = geometry(Rect::new(10.0, 20.0, w, h), true, true, true);
    let rail_end = geometry.rail.x + geometry.rail.w;
    let footer = geometry.footer.unwrap();
    for column in [geometry.search, geometry.list, footer] {
        assert!((column.x - (rail_end + RAIL_CONTENT_GAP)).abs() < 0.01);
        assert!(column.x + column.w <= geometry.panel.x + geometry.panel.w);
    }
    // The footer's parts start 8 in, level with a row's name.
    for part in [geometry.strip, geometry.fast].map(Option::unwrap) {
        assert!((part.x - (rail_end + RAIL_CONTENT_GAP + 8.0)).abs() < 0.01);
        assert!(part.x + part.w <= geometry.panel.x + geometry.panel.w - 12.0 + 0.01);
    }
    assert_eq!(h, size(360.0, rail_height(3, 1)).1, "the height is fixed");
    assert_eq!(
        size(360.0, 0.0).1,
        size(280.0, 0.0).1,
        "the gap takes width, never height"
    );
}

/// D13: one size — search, six rows and the footer at its tallest (D76) —
/// whatever the agent lists: a short list leaves room, a long one scrolls,
/// and a model without a footer gives its room to the list.
#[test]
fn the_picker_keeps_one_height_from_agent_to_agent() {
    let (_, h) = size(360.0, rail_height(2, 0));
    let rows = LIST_ROWS as f32 * ROW_H;
    assert!((h - (PAD * 2.0 + SEARCH_H + rows + FOOTER_H)).abs() < 0.5);

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
    assert!(levels.list.h >= rows - 0.5, "the strip and its readout");
    let toggles = geometry(Rect::new(0.0, 0.0, 360.0, h), false, true, true);
    assert!(
        toggles.list.h >= rows + ROW_H - 0.5,
        "pills alone: a row more"
    );
    let bare = geometry(Rect::new(0.0, 0.0, 360.0, h), false, false, false);
    assert!(
        bare.list.h > rows + ROW_H * 2.0 - 0.5,
        "no footer: more rows"
    );
    assert!(bare.footer.is_none() && bare.strip.is_none());
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

/// D10, D76: a model with a longer window shows a *1M context* pill, and
/// turning it on gives the chat that window's id as its model; the list
/// keeps one *Opus*, current either way. A model without one shows no
/// pill.
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
    assert!(
        surface.picker_geometry().unwrap().footer.is_none(),
        "no footer"
    );
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
    let pill = geometry.context.expect("opus has a context pill");
    assert_eq!(context::window_label(koru_i18n::Lang::En, 1_000_000), "1M");
    assert_eq!(
        context::pill_label(koru_i18n::Lang::En, 1_000_000),
        "1M context"
    );
    while commands.try_recv().is_ok() {}

    surface.picker.focus = PickerFocus::Context;
    assert!(press(&mut surface, NamedKey::Space));
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

    // A press on the pill takes the model's own id again.
    assert_eq!(
        surface.picker_mouse_down(pill.x + pill.w * 0.5, pill.y + pill.h * 0.5),
        Some(true)
    );
    assert_eq!(model(&surface), "opus");
    assert_eq!(surface.picker.focus, PickerFocus::Context);
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
    let plain = rail_rects(rail, 3, &[]);
    let divided = rail_rects(rail, 3, &[0]);
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

/// **A model row is one line** (D43 taken back): Claude's *Default* and
/// *Sonnet* carry a description and *Opus* none, and every row is as tall
/// as a heading, one under the next.
#[test]
fn every_model_row_is_one_line_whatever_its_description() {
    let mut surface = surface();
    open(&mut surface);
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-claude")));
    assert_eq!(
        surface.picker_rows(),
        [
            Row::Default,
            Row::Model("opus".into()),
            Row::Model("sonnet".into())
        ]
    );
    let geometry = surface.picker_geometry().unwrap();
    for index in 0..3 {
        let row = surface.picker_row_rect(&geometry, index).unwrap();
        assert_eq!(row.h, ROW_H);
        assert_eq!(row.y, geometry.list.y + index as f32 * ROW_H);
    }
}

/// **A long list scrolls to its end**: the wheel stops with the last model
/// whole at the foot, the keyboard scrolls the highlight into view, and a
/// press chooses the row drawn under it.
#[test]
fn a_long_list_scrolls_to_its_last_model() {
    let mut surface = surface();
    surface.agent_models.insert(
        agent("agent-claude"),
        AgentModels::Listed(
            (0..12)
                .map(|n| {
                    model(
                        &format!("m{n}"),
                        "claude",
                        Some(&format!("Model {n}")),
                        Some("Said under its name"),
                    )
                })
                .collect(),
        ),
    );
    open(&mut surface);
    let geometry = surface.picker_geometry().unwrap();
    let rows = surface.picker_rows();
    let last = rows.len() - 1;
    while surface.picker_scroll(-1.0) {}
    let shown = surface
        .picker_row_rect(&geometry, last)
        .expect("the last row is in view");
    assert!(shown.y + shown.h <= geometry.list.y + geometry.list.h + 0.5);
    assert!(!surface.picker_scroll(-1.0), "no further");
    assert!(surface.picker_scrollbar(&geometry).is_some());

    while surface.picker_scroll(1.0) {}
    assert_eq!(surface.picker.offset, 0);
    surface.picker.focus = PickerFocus::List;
    surface.picker.highlight = Some(0);
    for _ in 0..last {
        press(&mut surface, NamedKey::ArrowDown);
    }
    assert_eq!(surface.picker.highlight, Some(last));
    assert!(surface.picker_row_rect(&geometry, last).is_some());

    let row = surface.picker_row_rect(&geometry, last).unwrap();
    surface.picker_mouse_down(row.x + 20.0, row.y + row.h - 2.0);
    assert_eq!(
        surface
            .transcript
            .as_ref()
            .unwrap()
            .chat
            .current_model
            .requested_model,
        format!("m{}", last - 1),
        "the row under the press: the last model (row 0 is the agent's Default)"
    );
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
        readout(&stated, ReasoningChoice::Default, Lang::En),
        ["Effort: High (default)", "High (default)", "High"]
    );
    assert_eq!(
        readout(
            &stated,
            ReasoningChoice::Level(ReasoningLevel::High),
            Lang::En
        )[0],
        "Effort: High (default)",
        "the level shown is the model's own, however it was chosen"
    );
    assert_eq!(
        readout(
            &stated,
            ReasoningChoice::Level(ReasoningLevel::Low),
            Lang::En
        ),
        ["Effort: Low", "Low"],
        "no (default) on another level"
    );
    assert_eq!(
        readout(&unstated, ReasoningChoice::Default, Lang::En),
        ["Effort: Default", "Default"]
    );
    assert_eq!(
        readout(
            &unstated,
            ReasoningChoice::Level(ReasoningLevel::Low),
            Lang::En
        )[0],
        "Effort: Low"
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

/// A row `entry` lists, as a search finds it (D67).
fn found(entry: &RailEntry, row: Row) -> Row {
    Row::Found(entry.clone(), Box::new(row))
}

/// The chat's first message: it has begun, and keeps its agent.
fn begin(surface: &mut ChatSurface) {
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
}

/// §9.4 and S3: the query narrows the list — under the agent's heading
/// now, as it spans every agent (D67); an agent in the rail with no match
/// is dimmed; a model is still found by its description; nothing found
/// says so.
#[test]
fn typing_searches_and_dims_the_agents_without_a_match() {
    let mut surface = surface();
    surface.lang = Lang::En;
    open(&mut surface);
    let claude = RailEntry::Agent(agent("agent-claude"));
    surface.picker.rail = Some(claude.clone());
    type_query(&mut surface, "son");
    assert_eq!(surface.picker.query.raw_text(), "son");
    assert_eq!(
        surface.picker_rows(),
        [
            Row::EntryHeading(claude.clone()),
            found(&claude, Row::Model("sonnet".into()))
        ]
    );
    assert!(surface.agent_matches_query(&agent("agent-claude"), "son"));
    assert!(!surface.agent_matches_query(&agent("agent-opencode"), "son"));
    type_query(&mut surface, " able");
    assert_eq!(
        surface.picker_rows(),
        [
            Row::EntryHeading(claude.clone()),
            found(&claude, Row::Model("sonnet".into()))
        ],
        "by description, which no row shows"
    );
    type_query(&mut surface, "x");
    assert_eq!(
        surface.picker_rows(),
        [Row::Note("No matching models".into())]
    );
}

/// **A search spans every agent** (D67, the user: *search models should be
/// across all models, not just the scoped selection*): what it finds in
/// each agent on the rail, in the rail's order, under a heading with the
/// agent's mark and name — only agents with a match. Up and Down walk
/// across the groups and never land on a heading; no rail mark reads as
/// the list's meanwhile; a result from another agent is chosen as that
/// agent's own list would choose it. Esc brings the selected agent's list
/// back.
#[test]
fn a_search_spans_every_agent_under_a_heading_each() {
    let mut surface = surface();
    surface.lang = Lang::En;
    open(&mut surface);
    let claude = RailEntry::Agent(agent("agent-claude"));
    let opencode = RailEntry::Agent(agent("agent-opencode"));
    surface.select_rail_entry(claude.clone());
    assert!(surface.picker_spec().marks.iter().any(|mark| mark.selected));

    type_query(&mut surface, "beam");
    assert_eq!(
        surface.picker_rows(),
        [
            Row::EntryHeading(opencode.clone()),
            found(&opencode, Row::Model("opencode/beam-free".into()))
        ],
        "found outside the selected agent; Claude has no match, no heading"
    );
    assert_eq!(surface.picker.highlight, Some(1), "past the heading");
    assert_eq!(
        surface.picker_row_label(&claude, &Row::EntryHeading(opencode.clone())),
        "OpenCode"
    );
    assert_eq!(
        surface.picker_row_label(&claude, &surface.picker_rows()[1]),
        "Beam Free"
    );
    assert!(
        surface
            .picker_spec()
            .marks
            .iter()
            .all(|mark| !mark.selected),
        "no mark is the list's while searching"
    );

    press(&mut surface, NamedKey::Escape);
    type_query(&mut surface, "o");
    let rows = surface.picker_rows();
    assert_eq!(
        rows[0],
        Row::EntryHeading(claude.clone()),
        "the rail's order"
    );
    let second = rows
        .iter()
        .position(|row| *row == Row::EntryHeading(opencode.clone()))
        .expect("both agents match");
    assert!(second > 1);
    let mut walked = vec![surface.picker.highlight.unwrap()];
    for _ in 0..rows.len() {
        press(&mut surface, NamedKey::ArrowDown);
        walked.push(surface.picker.highlight.unwrap());
    }
    assert!(
        walked
            .iter()
            .all(|index| matches!(rows[*index], Row::Found(..))),
        "never on a heading"
    );
    assert!(walked.contains(&(second + 1)), "across into the next group");
    assert_eq!(*walked.last().unwrap(), rows.len() - 1);

    // The last row is OpenCode's: choosing it makes OpenCode the chat's.
    assert_eq!(
        rows[rows.len() - 1],
        found(&opencode, Row::Model("deepseek/v4-pro".into()))
    );
    assert!(press(&mut surface, NamedKey::Enter));
    let model = &surface.transcript.as_ref().unwrap().chat.current_model;
    assert_eq!(model.provider, agent("agent-opencode"));
    assert_eq!(model.requested_model, "deepseek/v4-pro");
    assert!(surface.picker_row_current(&claude, &rows[rows.len() - 1]));

    assert!(press(&mut surface, NamedKey::Escape), "clears the query");
    assert_eq!(
        surface.picker_rows()[0],
        Row::Default,
        "the selected agent's own list again"
    );
    assert!(surface.picker_spec().marks.iter().any(|mark| mark.selected));
}

/// **A begun chat searches only its own agent** (the user: *once the chat
/// is running, hide the model rail since we don't support switching
/// models*): another agent's models are not found, and the chat's own are
/// its own list narrowed — no heading over them, as there is no rail to
/// tell them apart from. Its agent's models are shown, not chosen: a live
/// agent session is not moved to another model.
#[test]
fn a_search_in_a_begun_chat_looks_only_at_its_own_agent() {
    let mut surface = surface();
    surface.lang = Lang::En;
    on_agent(
        &mut surface,
        "agent-claude",
        "sonnet",
        ReasoningChoice::Default,
    );
    begin(&mut surface);
    open(&mut surface);
    type_query(&mut surface, "beam");
    assert_eq!(
        surface.picker_rows(),
        [Row::Note("No matching models".into())],
        "OpenCode's Beam Free is not found"
    );
    press(&mut surface, NamedKey::Escape);
    type_query(&mut surface, "o");
    let rows = surface.picker_rows();
    assert!(
        rows.iter()
            .all(|row| matches!(row, Row::Default | Row::Model(_))),
        "Claude's own rows, no heading and nothing found elsewhere: {rows:?}"
    );
    assert!(rows.contains(&Row::Model("opus".into())));
    assert!(surface.picker_choosable(&rows).iter().all(|can| !can));
    assert_eq!(surface.picker.highlight, None);
    let geometry = surface.picker_geometry().unwrap();
    let row = surface.picker_row_rect(&geometry, 1).unwrap();
    surface.picker_mouse_down(row.x + 20.0, row.y + row.h * 0.5);
    let model = &surface.transcript.as_ref().unwrap().chat.current_model;
    assert_eq!(model.provider, agent("agent-claude"), "the chat keeps it");
    assert_eq!(model.requested_model, "sonnet");
}

/// **A begun agent chat moves between its own agent's models, from its
/// next message, where the agent's host takes a model with each one**: the
/// rail stays hidden and every other agent stays out of reach, the agent's
/// own rows are choosable, and a choice is the chat's model update — the
/// level kept where the new model offers it (D18). While a reply runs, a
/// note says the model is the next message's. Where the host takes no
/// model, the rows only show the one the chat started on.
#[test]
fn a_begun_agent_chat_moves_between_its_own_agents_models_where_its_host_takes_one() {
    use crate::reader::Command;
    let mut surface = surface();
    surface.lang = Lang::En;
    let (reader, commands, _updates) = crate::reader::Reader::test_pair();
    surface.reader = Some(reader);
    on_agent(
        &mut surface,
        "agent-claude",
        "opus",
        ReasoningChoice::Default,
    );
    begin(&mut surface);
    let updates = |commands: &std::sync::mpsc::Receiver<Command>| -> Vec<ModelSelection> {
        std::iter::from_fn(|| commands.try_recv().ok())
            .filter_map(|command| match command {
                Command::UpdateModel(_, request) => Some(request.model),
                _ => None,
            })
            .collect()
    };

    // A host that takes no model per message: shown, not chosen.
    assert!(surface.model_locked());
    open(&mut surface);
    let rows = surface.picker_rows();
    assert!(surface.picker_choosable(&rows).iter().all(|can| !can));
    let sonnet = rows
        .iter()
        .position(|row| *row == Row::Model("sonnet".into()))
        .unwrap();
    surface.choose_picker_row(sonnet);
    assert!(updates(&commands).is_empty());
    surface.close_picker();

    // One that does: the agent's own rows are choosable, the rail still
    // hidden.
    surface.models_per_turn.insert(agent("agent-claude"));
    assert!(!surface.model_locked());
    open(&mut surface);
    assert!(surface.picker_rail_hidden());
    assert_eq!(
        surface.picker_rail(),
        [RailEntry::Agent(agent("agent-claude"))]
    );
    let rows = surface.picker_rows();
    let choosable = surface.picker_choosable(&rows);
    for (row, can) in rows.iter().zip(&choosable) {
        assert_eq!(*can, matches!(row, Row::Default | Row::Model(_)), "{row:?}");
    }
    assert!(surface.choose_picker_row(sonnet));
    assert!(surface.chat_model_open, "chosen, still open (D66)");
    let sent = updates(&commands);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].provider, agent("agent-claude"), "the same agent");
    assert_eq!(sent[0].requested_model, "sonnet");
    assert!(
        surface.copy_feedback.label().is_none(),
        "no reply runs: no note"
    );

    // Another agent's model is refused: the chat keeps its agent.
    surface.choose_agent_model(agent("agent-opencode"), "deepseek/v4-pro".into());
    assert!(updates(&commands).is_empty());
    // The model it is on already: nothing to send.
    surface.choose_agent_model(agent("agent-claude"), "opus".into());
    assert!(updates(&commands).is_empty());

    // While a reply runs: the choice is the next message's, and says so.
    surface.attempt = Some(koru_ai_core::InferenceAttemptId::new());
    surface.choose_agent_model(agent("agent-claude"), "sonnet".into());
    assert_eq!(updates(&commands).len(), 1);
    let note = surface.copy_feedback.label().unwrap_or_default().to_owned();
    assert_eq!(note, "Sonnet 4.6 · applies to your next message");
}

/// **The rail shows until the chat begins, and then it is gone** (the
/// user: *once the chat is running, hide the model rail since we don't
/// support switching models*). A new chat has every agent, *Favorites* and
/// the **+**; from the first message the picker is the chat's own agent's
/// list alone, in the rail's room — the panel keeps its size — with its
/// footer as before, and no press, tooltip, Tab or arrow reaches a rail.
#[test]
fn the_rail_is_hidden_once_the_chat_has_begun() {
    let mut surface = picker_with_everything();
    surface.lang = Lang::En;
    // A new chat: nothing changes.
    assert!(!surface.picker_rail_hidden());
    let before = surface.picker_geometry().unwrap();
    assert_eq!(before.rail.w, RAIL_W);
    assert_eq!(
        surface.picker_rail(),
        [
            RailEntry::Favorites,
            RailEntry::Agent(agent("agent-claude")),
            RailEntry::Agent(agent("agent-opencode")),
        ]
    );
    assert_eq!(surface.picker_marks(), surface.picker_rail());
    let plus = surface.picker_manage_rect(&before).unwrap();
    let mark = rail_rects(before.rail, 3, &[0])[2];
    let spec = surface.picker_spec();
    assert!(!spec.hide_rail && spec.manage && spec.marks.len() == 3);
    let tabs = |surface: &mut ChatSurface| -> Vec<PickerFocus> {
        surface.picker.focus = PickerFocus::Search;
        (0..6)
            .map(|_| {
                press(surface, NamedKey::Tab);
                surface.picker.focus
            })
            .collect()
    };
    assert!(tabs(&mut surface).contains(&PickerFocus::Rail));

    // The first message: the rail, Favorites and the + are gone.
    begin(&mut surface);
    assert!(surface.picker_rail_hidden());
    assert_eq!(
        surface.picker_rail(),
        [RailEntry::Agent(agent("agent-claude"))],
        "the chat's own agent alone"
    );
    assert!(surface.picker_marks().is_empty());
    assert_eq!(
        surface.picker_entry(),
        Some(RailEntry::Agent(agent("agent-claude")))
    );
    let spec = surface.picker_spec();
    assert!(spec.hide_rail && !spec.manage && spec.marks.is_empty());
    let after = surface.picker_geometry().unwrap();
    assert_eq!(after.panel, before.panel, "the picker keeps its size");
    assert_eq!(after.rail.w, 0.0);
    assert!(surface.picker_manage_rect(&after).is_none());
    // The list takes the rail's room: inset from the panel's start as
    // from its end.
    assert!((after.list.x - (after.panel.x + PAD)).abs() < 0.01);
    assert!((after.search.x - (after.panel.x + PAD)).abs() < 0.01);
    assert!(
        (after.list.w - (before.list.w + RAIL_W + RAIL_CONTENT_GAP - PAD)).abs() < 0.01,
        "{} after {}",
        after.list.w,
        before.list.w
    );
    assert!(
        (after.list.x + after.list.w - (before.list.x + before.list.w)).abs() < 0.01,
        "the end where it was"
    );
    // Its footer as before: the level, Fast and the window.
    assert!(after.strip.is_some() && after.fast.is_some() && after.context.is_some());
    assert_eq!(
        surface.picker_rows()[..3],
        [
            Row::Default,
            Row::Model("opus".into()),
            Row::Model("sonnet".into())
        ],
        "Claude's own list"
    );

    // Where the marks were, a press chooses no other agent, and nothing
    // raises a rail mark's tooltip.
    let (x, y) = (mark.x + mark.w * 0.5, mark.y + mark.h * 0.5);
    assert!(surface.picker_tooltip_at(&after, x, y).is_none());
    assert_eq!(surface.picker_mouse_down(x, y), Some(true));
    assert_eq!(
        surface.picker_entry(),
        Some(RailEntry::Agent(agent("agent-claude")))
    );
    assert_ne!(surface.picker.focus, PickerFocus::Rail);
    surface.picker_mouse_down(plus.x + plus.w * 0.5, plus.y + plus.h * 0.5);
    assert!(!surface.take_agent_settings_request(), "no + to press");
    assert!(surface.chat_model_open);

    // Tab walks search → list → the footer, never the rail.
    assert_eq!(
        tabs(&mut surface)[..5],
        [
            PickerFocus::List,
            PickerFocus::Effort,
            PickerFocus::Fast,
            PickerFocus::Context,
            PickerFocus::Search,
        ]
    );
    // The keyboard left on the rail as the chat began is on the list: an
    // arrow does not step to another agent.
    surface.picker.focus = PickerFocus::Rail;
    press(&mut surface, NamedKey::ArrowDown);
    assert_eq!(surface.picker.focus, PickerFocus::List);
    assert_eq!(
        surface.picker_entry(),
        Some(RailEntry::Agent(agent("agent-claude")))
    );
}

/// **A begun chat on a built-in provider keeps its own models, and may
/// move between them** (S1): no rail — so no agent to switch to — its
/// models choosable as before, and a search finding only them.
#[test]
fn a_begun_built_in_chat_has_no_rail_and_still_switches_its_own_models() {
    let mut surface = surface();
    surface.lang = Lang::En;
    surface.ollama_models = vec!["qwen3".into(), "llama3".into()];
    on_agent(
        &mut surface,
        AiProviderId::OLLAMA,
        "llama3",
        ReasoningChoice::Default,
    );
    open(&mut surface);
    assert!(!surface.picker_rail_hidden(), "a new chat keeps its rail");
    assert!(!surface.picker_marks().is_empty());
    surface.close_picker();

    begin(&mut surface);
    open(&mut surface);
    let ollama = RailEntry::Provider(agent(AiProviderId::OLLAMA));
    assert_eq!(surface.picker_rail(), [ollama.clone()]);
    assert!(surface.picker_marks().is_empty());
    assert_eq!(surface.picker_entry(), Some(ollama));
    let rows = surface.picker_rows();
    assert_eq!(
        rows,
        [Row::Model("qwen3".into()), Row::Model("llama3".into())],
        "Ollama's own models, in their sorted order"
    );
    assert!(
        surface.picker_choosable(&rows).iter().all(|can| *can),
        "a built-in provider's chat moves between its own models"
    );
    type_query(&mut surface, "qw");
    assert_eq!(surface.picker_rows(), [Row::Model("qwen3".into())]);
    assert_eq!(surface.picker.highlight, Some(0));
    press(&mut surface, NamedKey::Escape);
    type_query(&mut surface, "sonnet");
    assert_eq!(
        surface.picker_rows(),
        [Row::Note("No matching models".into())],
        "Claude's Sonnet is not found"
    );
}

/// A press on a rail mark while searching shows that agent's list: the
/// query, which spans every agent, is cleared (D67).
#[test]
fn a_rail_mark_pressed_while_searching_clears_the_query() {
    let mut surface = surface();
    open(&mut surface);
    type_query(&mut surface, "beam");
    let geometry = surface.picker_geometry().unwrap();
    let rail = surface.picker_rail();
    let mark = rail_rects(geometry.rail, rail.len(), &rail_dividers(&rail))[1];
    surface.picker_mouse_down(mark.x + mark.w * 0.5, mark.y + mark.h * 0.5);
    assert!(surface.picker.query.raw_text().is_empty());
    assert_eq!(
        surface.picker_entry(),
        Some(RailEntry::Agent(agent("agent-opencode")))
    );
    assert_eq!(surface.picker_rows().len(), 5, "OpenCode's own list");
}

/// §9.5: type *son*, Down, Enter chooses and the picker stays open (D66);
/// Esc clears the query, then closes; Tab walks search → rail → list → ↺ →
/// slider.
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
    assert!(matches!(rows[first], Row::Found(..)), "past the heading");
    assert!(press(&mut surface, NamedKey::Escape), "clears the query");
    assert!(surface.chat_model_open);
    assert!(surface.picker.query.raw_text().is_empty());
    type_query(&mut surface, "son");
    press(&mut surface, NamedKey::ArrowDown);
    assert!(press(&mut surface, NamedKey::Enter));
    assert!(surface.chat_model_open, "chosen, and still open");
    let chosen = &surface.transcript.as_ref().unwrap().chat.current_model;
    assert_eq!(chosen.provider, agent("agent-claude"));
    assert_eq!(chosen.requested_model, "sonnet");
    assert!(press(&mut surface, NamedKey::Escape), "clears the query");
    assert!(press(&mut surface, NamedKey::Escape), "closes");
    assert!(!surface.chat_model_open);

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

/// §9.3, D76: no control, no footer; Left and Right on the strip move a
/// segment; the model's own level is the chat's *Default* — a press on it
/// is the way back, with no ↺.
#[test]
fn the_footer_follows_the_chats_model_and_sets_its_level() {
    let mut surface = surface();
    let levels = ReasoningControl::Levels {
        values: vec![
            ReasoningLevel::Low,
            ReasoningLevel::High,
            ReasoningLevel::Max,
        ],
        default: Some(ReasoningLevel::High),
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
    let level = |surface: &ChatSurface| {
        surface
            .transcript
            .as_ref()
            .unwrap()
            .chat
            .current_model
            .default_reasoning
    };
    let high = ReasoningChoice::Level(ReasoningLevel::High);
    assert_eq!(level(&surface), ReasoningChoice::Default);
    assert!(surface.choose_level(high), "already the model's own");
    assert_eq!(level(&surface), ReasoningChoice::Default);
    surface.picker.focus = PickerFocus::Effort;
    press(&mut surface, NamedKey::ArrowRight);
    assert_eq!(level(&surface), ReasoningChoice::Level(ReasoningLevel::Max));
    press(&mut surface, NamedKey::ArrowLeft);
    assert_eq!(
        level(&surface),
        ReasoningChoice::Default,
        "stepping onto the model's own level is its Default"
    );
    press(&mut surface, NamedKey::ArrowLeft);
    assert_eq!(level(&surface), ReasoningChoice::Level(ReasoningLevel::Low));
    // A press on the model's own level is the way back to it.
    let at = surface.picker_segment_rect(high).unwrap();
    assert_eq!(
        surface.picker_mouse_down(at.x + at.w * 0.5, at.y + at.h * 0.5),
        Some(true)
    );
    assert_eq!(level(&surface), ReasoningChoice::Default);
    assert_eq!(surface.picker.focus, PickerFocus::Effort);
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

/// A press on a row chooses it and the picker stays open on it (D66);
/// only a press outside closes the picker.
#[test]
fn a_click_on_a_row_chooses_it_and_one_outside_closes() {
    let mut surface = surface();
    open(&mut surface);
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-claude")));
    let geometry = surface.picker_geometry().unwrap();
    let row = surface.picker_row_rect(&geometry, 1).unwrap();
    assert_eq!(
        surface.picker_mouse_down(row.x + 20.0, row.y + row.h * 0.5),
        Some(true)
    );
    assert!(surface.chat_model_open, "still open, for the footer");
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
    let entry = surface.picker_entry().unwrap();
    let rows = surface.picker_rows();
    assert!(
        surface.picker_row_current(&entry, &rows[1]),
        "the ✓ is on it"
    );
    assert_eq!(surface.picker.highlight, Some(1));
    assert_eq!(surface.picker_mouse_down(1.0, 1.0), Some(true));
    assert!(!surface.chat_model_open, "a press outside closes it");
}

/// **The footer follows the model chosen with the picker open** (D66):
/// choosing a model with levels brings the level's footer, and the Context
/// row, at once; choosing one without takes them away — the ✓ moving each
/// time, the chosen row kept in view.
#[test]
fn choosing_a_model_keeps_the_picker_open_and_its_footer_follows() {
    let mut surface = surface();
    with_opus_context(&mut surface);
    if let Some(AgentModels::Listed(models)) = surface.agent_models.get_mut(&agent("agent-claude"))
    {
        models[1].reasoning = Some(ReasoningControl::Levels {
            values: vec![ReasoningLevel::Low, ReasoningLevel::High],
            default: None,
            supports_off: false,
        });
    }
    on_agent(
        &mut surface,
        "agent-claude",
        "sonnet",
        ReasoningChoice::Default,
    );
    surface.unmade_chat = surface.transcript.as_ref().map(|t| t.chat.id);
    open(&mut surface);
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-claude")));
    let before = surface.picker_geometry().unwrap();
    assert!(before.footer.is_none() && before.context.is_none());

    let opus = surface.picker_row_rect(&before, 1).unwrap();
    surface.picker_mouse_down(opus.x + 20.0, opus.y + opus.h * 0.5);
    assert!(surface.chat_model_open);
    let after = surface.picker_geometry().unwrap();
    assert!(after.footer.is_some(), "opus: the level's footer");
    assert!(after.context.is_some(), "opus: the Context row");
    let entry = surface.picker_entry().unwrap();
    let rows = surface.picker_rows();
    assert!(surface.picker_row_current(&entry, &rows[1]));
    assert!(!surface.picker_row_current(&entry, &rows[2]));
    let highlight = surface.picker.highlight.unwrap();
    assert!(
        surface.picker_row_rect(&after, highlight).is_some(),
        "in view"
    );

    // The level is set right after, in the same open picker.
    surface.picker.focus = PickerFocus::Effort;
    press(&mut surface, NamedKey::ArrowRight);
    assert_eq!(
        surface
            .transcript
            .as_ref()
            .unwrap()
            .chat
            .current_model
            .default_reasoning,
        ReasoningChoice::Level(ReasoningLevel::Low)
    );

    surface.picker.focus = PickerFocus::List;
    surface.picker.highlight = Some(2);
    assert!(press(&mut surface, NamedKey::Enter), "sonnet");
    let back = surface.picker_geometry().unwrap();
    assert!(back.footer.is_none() && back.context.is_none());
    assert!(surface.chat_model_open);
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
        let row = surface.picker_row_rect(&geometry, visible).unwrap();
        assert!(
            surface
                .picker_tooltip_at(&geometry, row.x + 20.0, row.y + row.h * 0.5)
                .is_none(),
            "row {visible} raised a tooltip"
        );
    }
    let rail = surface.picker_rail();
    let mark = rail_rects(geometry.rail, rail.len(), &rail_dividers(&rail))[0];
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

/// **Rung 6: older models live under *Legacy models (N)***, at the foot of
/// the list, collapsed (M4); a search shows them inline after the others,
/// tagged.
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
            Row::LegacyToggle,
        ]
    );
    assert_eq!(
        surface.picker_row_label(&claude, &Row::LegacyToggle),
        "Legacy models (2)"
    );
    type_query(&mut surface, "claude-");
    let rows = surface.picker_rows();
    assert_eq!(
        rows,
        [
            Row::EntryHeading(claude.clone()),
            found(&claude, Row::Model("claude-2".into())),
            found(&claude, Row::Model("claude-1".into()))
        ],
        "inline while searching"
    );
    assert!(surface.row_is_legacy(&claude, &rows[1]));
    // Today's agents mark nothing: no Legacy row.
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-opencode")));
    surface.picker.query.set_text(&mut surface.engine, "");
    assert!(!surface.picker_rows().contains(&Row::LegacyToggle));
}

/// **The older models expand in the same list** (M4, the user's design):
/// a click on *Legacy models (2)* puts them under it — no popover, no
/// flyout — and a second click takes them away; Enter does the same, and
/// Right expands and Left collapses. Up and Down walk into them, Enter
/// chooses one — the chat's model now, the picker still open (D66) — and
/// its ✓ is in the list, untagged.
#[test]
fn the_legacy_row_expands_and_collapses_in_the_list() {
    let mut surface = surface();
    with_legacy(&mut surface);
    open(&mut surface);
    let claude = RailEntry::Agent(agent("agent-claude"));
    surface.picker.rail = Some(claude.clone());
    let collapsed = surface.picker_rows();
    let toggle = collapsed.len() - 1;
    assert_eq!(collapsed[toggle], Row::LegacyToggle);

    // A click expands it, in place.
    let geometry = surface.picker_geometry().unwrap();
    let row = surface.picker_row_rect(&geometry, toggle).unwrap();
    surface.picker_mouse_down(row.x + 20.0, row.y + row.h * 0.5);
    assert!(surface.chat_model_open);
    assert_eq!(
        &surface.picker_rows()[toggle..],
        [
            Row::LegacyToggle,
            Row::Model("claude-2".into()),
            Row::Model("claude-1".into()),
        ]
    );
    let spec = surface.picker_spec();
    assert_eq!(
        spec.list[toggle].legacy,
        Some(true),
        "its caret points down"
    );
    assert!(spec.list[toggle + 1].tag.is_none(), "no tag under the row");
    // A second click collapses it.
    surface.picker_mouse_down(row.x + 20.0, row.y + row.h * 0.5);
    assert_eq!(surface.picker_rows(), collapsed);
    assert_eq!(surface.picker_spec().list[toggle].legacy, Some(false));

    // The keyboard: Enter expands, Left collapses, Right expands.
    surface.picker.focus = PickerFocus::List;
    surface.picker.highlight = Some(toggle);
    press(&mut surface, NamedKey::Enter);
    assert!(surface.picker.legacy_open, "Enter expands");
    press(&mut surface, NamedKey::ArrowLeft);
    assert!(!surface.picker.legacy_open, "Left collapses");
    assert_eq!(surface.picker.highlight, Some(toggle));
    press(&mut surface, NamedKey::ArrowRight);
    assert!(surface.picker.legacy_open, "Right expands");
    // Down walks into the older models, and Enter chooses one.
    press(&mut surface, NamedKey::ArrowDown);
    press(&mut surface, NamedKey::ArrowDown);
    assert_eq!(surface.picker.highlight, Some(toggle + 2));
    press(&mut surface, NamedKey::Enter);
    assert!(surface.chat_model_open, "the picker stays open");
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
    assert!(surface.picker_row_current(&claude, &Row::Model("claude-1".into())));

    // Opened again on a chat whose model is an older one, the row is
    // expanded and the ✓ is in view.
    surface.close_picker();
    open(&mut surface);
    assert!(surface.picker.legacy_open);
    let current = surface.picker.highlight.expect("on the current model");
    assert_eq!(
        surface.picker_rows()[current],
        Row::Model("claude-1".into())
    );
    let geometry = surface.picker_geometry().unwrap();
    assert!(surface.picker_row_rect(&geometry, current).is_some());
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
    assert!(fast.x + fast.w <= context.x, "Fast first, in one row");
    assert_eq!(fast.y, context.y);
    assert!(geometry.list.y + geometry.list.h <= fast.y);
    assert!(geometry.strip.is_none(), "opus offers no levels here");
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
    // Space or Enter on the focused pill toggles it; so does a press.
    surface.picker.focus = PickerFocus::Fast;
    assert!(press(&mut surface, NamedKey::Space));
    assert!(!surface.picker_fast().unwrap().on);
    assert!(press(&mut surface, NamedKey::Enter));
    assert!(surface.picker_fast().unwrap().on);
    assert!(
        surface.picker.query.raw_text().is_empty(),
        "Space on a pill is not typed"
    );
    let fast = surface.picker_geometry().unwrap().fast.unwrap();
    surface.picker_mouse_down(fast.x + fast.w * 0.5, fast.y + fast.h * 0.5);
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
            levels_per_turn: false,
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
    let marks = rail_rects(geometry.rail, rail.len(), &rail_dividers(&rail));
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

/// **The arrow over the picker, the I-beam only in its search** (D44): the
/// picker lies over the conversation, whose text gave its I-beam to the
/// rows. Rows, *Legacy models*, the rail, ↻ and the panel's foot show the
/// arrow; the search field shows the I-beam.
#[test]
fn the_picker_shows_the_arrow_and_its_search_the_ibeam() {
    use winit::window::CursorIcon;
    let mut surface = surface();
    with_legacy(&mut surface);
    open(&mut surface);
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-claude")));
    let geometry = surface.picker_geometry().unwrap();
    let at = |surface: &mut ChatSurface, x: f32, y: f32| {
        surface.pointer = (x, y);
        surface.cursor_icon()
    };
    let field = search_field(&geometry);
    assert_eq!(
        at(&mut surface, field.x + 12.0, field.y + field.h * 0.5),
        CursorIcon::Text
    );
    for index in 0..4 {
        let row = surface.picker_row_rect(&geometry, index).unwrap();
        assert_eq!(
            at(&mut surface, row.x + 20.0, row.y + row.h * 0.5),
            CursorIcon::Default,
            "row {index}"
        );
    }
    let rail = surface.picker_rail();
    let mark = rail_rects(geometry.rail, rail.len(), &rail_dividers(&rail))[0];
    assert_eq!(
        at(&mut surface, mark.x + 4.0, mark.y + 4.0),
        CursorIcon::Default
    );
    let refresh = geometry.refresh;
    assert_eq!(
        at(&mut surface, refresh.x + 4.0, refresh.y + 4.0),
        CursorIcon::Default
    );
    let panel = geometry.panel;
    assert_eq!(
        at(
            &mut surface,
            panel.x + panel.w * 0.5,
            panel.y + panel.h - 3.0
        ),
        CursorIcon::Default,
        "the foot"
    );
}

/// **The rail sits on the window rail's ground** (D45): the shared token
/// the app's own left rail is drawn in — the base colour for the theme on
/// screen, or the theme's ground — and not the panel's white or charcoal.
#[test]
fn the_rail_is_the_colour_of_the_windows_rail() {
    use koru_draw::el::FrameCx;
    use koru_draw::theme::{Radius, Surface};
    let cx = FrameCx::root();
    assert_eq!(cx.surface(Surface::Ground), theme::base_fill());
    let p = theme::p();
    assert_ne!(
        cx.surface(Surface::Ground),
        p.panel_bg,
        "the rail reads apart from the list"
    );
    // The fill lies inside the panel, where the rail is.
    let mut surface = surface();
    open(&mut surface);
    let geometry = surface.picker_geometry().unwrap();
    assert_eq!(geometry.rail.x, geometry.panel.x);
    assert_eq!(geometry.rail.h, geometry.panel.h);
    assert!(geometry.rail.x + geometry.rail.w <= geometry.list.x + 0.5);
    // It is the rail column's own background (M2): inside the panel's
    // hairline on its start, top and foot, rounded at its start corners as
    // the panel is inside that hairline.
    let spec = surface.picker_spec();
    let frame = elements::layout(
        &elements::tree(&spec, elements::Painted::default()),
        geometry.panel,
        cx,
    );
    let column = frame.named(elements::RAIL_COLUMN).next().unwrap();
    assert_eq!(column.look.bg, Some(Surface::Ground));
    let hairline = koru_draw::theme::BORDER_W as f32;
    assert_same_rect(
        column.rect,
        Rect::new(
            geometry.rail.x + hairline,
            geometry.rail.y + hairline,
            geometry.rail.w - hairline,
            geometry.rail.h - hairline * 2.0,
        ),
        "the ground",
    );
    assert_eq!(
        Radius::Metric(elements::GROUND_RADIUS).px(koru_draw::theme::Profile::Desktop, 1.0, 1.0),
        theme::overlay::popover().radius - hairline
    );
}

/// The primary modifier and D, which starred a model until D47 was taken
/// back.
fn favorite_key(surface: &mut ChatSurface) -> bool {
    let primary = if cfg!(target_os = "macos") {
        ModifiersState::SUPER
    } else {
        ModifiersState::CONTROL
    };
    surface
        .picker_key(&WinitKey::Character("d".into()), Some("d"), primary)
        .unwrap()
}

/// **A ☆ stars a model, and a second press takes the star away** (D46): a
/// press on the row's ☆ stars it without choosing it, the whole list is
/// handed to the host to store, and the host's answer is what the picker
/// then shows. The agent's *Default* and a built-in provider's models have
/// no ☆.
#[test]
fn a_press_on_the_star_toggles_the_favorite_and_asks_the_host_to_store_it() {
    let mut surface = surface();
    open(&mut surface);
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-claude")));
    let geometry = surface.picker_geometry().unwrap();
    let entry = surface.picker_entry().unwrap();
    assert_eq!(surface.row_favorite(&entry, &Row::Default), None);
    assert_eq!(
        surface.row_favorite(
            &RailEntry::Provider(agent("openai")),
            &Row::Model("gpt".into())
        ),
        None
    );

    let sonnet = surface.picker_row_rect(&geometry, 2).unwrap();
    let star = star_rect(sonnet);
    let (x, y) = (star.x + star.w * 0.5, star.y + star.h * 0.5);
    assert!(sonnet.contains(x, y));
    assert_eq!(surface.picker_mouse_down(x, y), Some(true));
    assert!(surface.chat_model_open, "the star does not choose the row");
    assert!(surface.is_favorite(&agent("agent-claude"), "sonnet"));
    let asked = surface
        .take_favorite_models_request()
        .expect("the host is asked");
    assert_eq!(asked, [FavoriteModel::new("claude", "sonnet")]);
    assert_eq!(surface.take_favorite_models_request(), None, "asked once");

    // The host stores it and hands it back: nothing moves.
    assert!(!surface.set_favorite_models(asked));
    surface.picker_mouse_down(x, y);
    assert!(!surface.is_favorite(&agent("agent-claude"), "sonnet"));
    assert_eq!(surface.take_favorite_models_request(), Some(Vec::new()));
}

/// **No key stars a model** (D47 taken back by the user: *click on star
/// only*): the primary modifier and D, from the search field or the list,
/// leaves the highlighted row's star alone, and no shortcut is registered
/// for it.
#[test]
fn the_primary_modifier_and_d_does_not_star_the_highlighted_row() {
    let chord = koru_keys::Chord::new(koru_keys::On::Letter('d'), koru_keys::PRIMARY);
    assert!(
        koru_keys::REGISTRY
            .iter()
            .filter(|shortcut| shortcut.layer == koru_keys::Layer::Chat)
            .all(|shortcut| shortcut.chord != chord),
        "the chat layer has no Ctrl+D"
    );
    let mut surface = surface();
    open(&mut surface);
    surface.picker.rail = Some(RailEntry::Agent(agent("agent-claude")));
    surface.picker.highlight = Some(1);
    for focus in [PickerFocus::Search, PickerFocus::List] {
        surface.picker.focus = focus;
        favorite_key(&mut surface);
        assert!(!surface.is_favorite(&agent("agent-claude"), "opus"));
        assert_eq!(surface.take_favorite_models_request(), None);
    }
}

/// **Favorites leads the rail and lists every agent's starred models**
/// (D46), in the rail's agent order, each with its own agent; choosing one
/// makes that agent and model the chat's. A query searches every agent, as
/// from any entry (D67), and finds a model by its agent's name too.
#[test]
fn favorites_lists_models_across_agents_and_chooses_agent_and_model() {
    let mut surface = surface();
    assert!(
        !surface.picker_rail().contains(&RailEntry::Favorites),
        "no favorite: no entry"
    );
    surface.set_favorite_models(vec![
        FavoriteModel::new("opencode", "deepseek/v4-pro"),
        FavoriteModel::new("claude", "sonnet"),
        FavoriteModel::new("claude", "opus"),
    ]);
    let rail = surface.picker_rail();
    assert_eq!(rail[0], RailEntry::Favorites);
    assert_eq!(rail_dividers(&rail), [0], "set off from the agents");

    open(&mut surface);
    assert_ne!(
        surface.picker_entry(),
        Some(RailEntry::Favorites),
        "it is not where the picker opens"
    );
    surface.select_rail_entry(RailEntry::Favorites);
    assert_eq!(
        surface.picker_rows(),
        [
            Row::Favorite(agent("agent-claude"), "sonnet".into()),
            Row::Favorite(agent("agent-claude"), "opus".into()),
            Row::Favorite(agent("agent-opencode"), "deepseek/v4-pro".into()),
        ]
    );
    let entry = RailEntry::Favorites;
    let first = Row::Favorite(agent("agent-claude"), "sonnet".into());
    assert_eq!(surface.picker_row_label(&entry, &first), "Sonnet 4.6");
    assert_eq!(
        surface.row_favorite(&entry, &first),
        Some((agent("agent-claude"), "sonnet".to_owned())),
        "its star takes it off"
    );

    type_query(&mut surface, "opencode deep");
    let opencode = RailEntry::Agent(agent("agent-opencode"));
    assert_eq!(
        surface.picker_rows(),
        [
            Row::EntryHeading(opencode.clone()),
            found(&opencode, Row::Model("deepseek/v4-pro".into()))
        ]
    );
    press(&mut surface, NamedKey::Enter);
    assert!(surface.chat_model_open, "chosen, still open (D66)");
    let model = &surface.transcript.as_ref().unwrap().chat.current_model;
    assert_eq!(model.provider, agent("agent-opencode"));
    assert_eq!(model.requested_model, "deepseek/v4-pro");

    // Opened again, the favorite reads as the chat's.
    open(&mut surface);
    surface.select_rail_entry(RailEntry::Favorites);
    let rows = surface.picker_rows();
    assert!(surface.picker_row_current(&RailEntry::Favorites, &rows[2]));
    assert!(!surface.picker_row_current(&RailEntry::Favorites, &rows[0]));
}

/// **A turned-off agent's favorites are hidden, not lost** (D46): they
/// leave *Favorites* — and the last one takes the entry off the rail —
/// while the list the host stores keeps them, so turning the agent back on
/// brings them back.
#[test]
fn a_turned_off_agents_favorite_is_hidden_and_kept() {
    let mut surface = surface();
    let favorites = vec![
        FavoriteModel::new("claude", "opus"),
        FavoriteModel::new("opencode", "opencode/beam-free"),
    ];
    surface.set_favorite_models(favorites.clone());
    surface.set_hidden_agents(["opencode".to_owned()]);
    assert_eq!(
        surface.picker_favorites(),
        [(agent("agent-claude"), "opus".to_owned())]
    );
    surface.set_hidden_agents(["opencode".to_owned(), "claude".to_owned()]);
    assert!(surface.picker_favorites().is_empty());
    assert!(!surface.picker_rail().contains(&RailEntry::Favorites));
    assert_eq!(surface.favorite_models, favorites, "kept");
    // Starring another model keeps the hidden ones in what is stored.
    surface.set_hidden_agents(["opencode".to_owned()]);
    surface.toggle_favorite(&agent("agent-claude"), "sonnet");
    let stored = surface.take_favorite_models_request().unwrap();
    assert!(stored.contains(&FavoriteModel::new("opencode", "opencode/beam-free")));
    surface.set_hidden_agents(Vec::new());
    assert_eq!(surface.picker_favorites().len(), 3);
}

/// **Unstarring the last favorite under *Favorites* leaves the picker on
/// an agent** (D46): the entry is gone, and the keyboard is on a row that
/// is there.
#[test]
fn unstarring_the_last_favorite_leaves_favorites_gracefully() {
    let mut surface = surface();
    surface.set_favorite_models(vec![FavoriteModel::new("claude", "opus")]);
    open(&mut surface);
    surface.select_rail_entry(RailEntry::Favorites);
    surface.picker.focus = PickerFocus::List;
    assert_eq!(surface.picker.highlight, Some(0));
    let geometry = surface.picker_geometry().unwrap();
    let star = star_rect(surface.picker_row_rect(&geometry, 0).unwrap());
    surface.picker_mouse_down(star.x + star.w * 0.5, star.y + star.h * 0.5);
    assert!(!surface.picker_rail().contains(&RailEntry::Favorites));
    assert!(matches!(surface.picker_entry(), Some(RailEntry::Agent(_))));
    let rows = surface.picker_rows();
    let highlight = surface.picker.highlight.expect("on a row");
    assert!(highlight < rows.len() && rows[highlight].choosable());
}

/// One press adds a favorite at the end, the next takes it out where it
/// was.
#[test]
fn a_star_toggles_into_and_out_of_the_list_in_place() {
    let opus = FavoriteModel::new("claude", "opus");
    let sonnet = FavoriteModel::new("claude", "sonnet");
    let both = toggled(std::slice::from_ref(&opus), sonnet.clone());
    assert_eq!(both, [opus.clone(), sonnet.clone()], "added at the end");
    assert_eq!(toggled(&both, opus), [sonnet], "taken out where it was");
}

/// The picker's parts as they were placed before its chrome was an element
/// tree: the arithmetic `geometry` and `rail_rects` held until D64, kept
/// here as the record the frame is held to — the footer as D76 places it:
/// the divider, 10, the strip, 8, the pills' row and 8, its parts 8 in
/// from the content column's sides, a wordless pill its inset and icon.
fn arithmetic_geometry(panel: Rect, levels: bool, context: bool, fast: bool) -> Geometry {
    let inner_x = panel.x + RAIL_W + RAIL_CONTENT_GAP;
    let inner_w = (panel.w - RAIL_W - RAIL_CONTENT_GAP - PAD).max(1.0);
    let mut bottom = panel.y + panel.h - PAD;
    let footer_h = if levels {
        Some(FOOTER_H)
    } else if context || fast {
        Some(FOOTER_TOGGLES_H)
    } else {
        None
    };
    let footer = footer_h.map(|h| {
        bottom -= h;
        Rect::new(inner_x, bottom, inner_w, h)
    });
    let part_x = inner_x + 8.0;
    let strip = levels.then(|| Rect::new(part_x, bottom + 11.0, inner_w - 16.0, STRIP_H));
    let pills_y = bottom + 11.0 + if levels { STRIP_H + 8.0 } else { 0.0 };
    let fast_w = 10.0 * 2.0 + 12.0 + 4.0;
    let fast_rect = fast.then(|| Rect::new(part_x, pills_y, fast_w, PILL_H));
    let context = context.then(|| {
        let x = if fast { part_x + fast_w + 6.0 } else { part_x };
        Rect::new(x, pills_y, 20.0, PILL_H)
    });
    let fast = fast_rect;
    let search = Rect::new(inner_x, panel.y + PAD + 4.0, inner_w, SEARCH_H - 8.0);
    let refresh = Rect::new(
        search.x + search.w - 28.0,
        search.y + 2.0,
        26.0,
        search.h - 4.0,
    );
    let list_top = panel.y + PAD + SEARCH_H;
    let rows = ((bottom - list_top) / ROW_H).floor().max(0.0);
    Geometry {
        panel,
        rail: Rect::new(panel.x, panel.y, RAIL_W, panel.h),
        search,
        field: Rect::new(
            search.x,
            search.y,
            (search.w - refresh.w - 4.0).max(1.0),
            search.h,
        ),
        refresh,
        list: Rect::new(inner_x, list_top, inner_w, rows * ROW_H),
        footer,
        strip,
        context,
        fast,
    }
}

fn arithmetic_rail_rects(rail: Rect, count: usize, dividers: &[usize]) -> Vec<Rect> {
    let x = rail.x + (rail.w - RAIL_BUTTON) * 0.5;
    let mut y = rail.y + PAD + 4.0;
    (0..count)
        .map(|index| {
            let rect = Rect::new(x, y, RAIL_BUTTON, RAIL_BUTTON);
            y += RAIL_BUTTON + RAIL_GAP;
            if dividers.contains(&index) {
                y += RAIL_DIVIDER;
            }
            rect
        })
        .collect()
}

fn assert_same_rect(found: Rect, expected: Rect, what: &str) {
    for (a, b) in [
        (found.x, expected.x),
        (found.y, expected.y),
        (found.w, expected.w),
        (found.h, expected.h),
    ] {
        assert!((a - b).abs() < 0.01, "{what}: {found:?}, was {expected:?}");
    }
}

/// **The element frame puts every part where the arithmetic did** (D64):
/// rail, search box, field, ↻, list and the footer's parts under it, for
/// narrow, middling and wide panels with every combination of parts —
/// pixel for pixel.
#[test]
fn the_element_frame_places_the_picker_where_it_always_was() {
    for composer in [280.0, 360.0, 1000.0] {
        let (w, h) = size(composer, rail_height(3, 1));
        for (x, y) in [(10.0, 20.0), (123.5, 401.25)] {
            let panel = Rect::new(x, y, w, h);
            for levels in [false, true] {
                for context in [false, true] {
                    for fast in [false, true] {
                        let found = geometry(panel, levels, context, fast);
                        let was = arithmetic_geometry(panel, levels, context, fast);
                        let case = format!("{w}x{h} levels {levels} context {context} fast {fast}");
                        assert_same_rect(found.panel, was.panel, &case);
                        assert_same_rect(found.rail, was.rail, &format!("rail, {case}"));
                        assert_same_rect(found.search, was.search, &format!("search, {case}"));
                        assert_same_rect(found.field, was.field, &format!("field, {case}"));
                        assert_same_rect(found.refresh, was.refresh, &format!("↻, {case}"));
                        assert_same_rect(found.list, was.list, &format!("list, {case}"));
                        for (part, a, b) in [
                            ("footer", found.footer, was.footer),
                            ("strip", found.strip, was.strip),
                            ("context", found.context, was.context),
                            ("fast", found.fast, was.fast),
                        ] {
                            assert_eq!(a.is_some(), b.is_some(), "{part}, {case}");
                            if let (Some(a), Some(b)) = (a, b) {
                                assert_same_rect(a, b, &format!("{part}, {case}"));
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The rail's marks and dividers sit where they always did, and the rail
/// column's own height is the room [`rail_height`] asks the panel for.
#[test]
fn the_rail_column_places_its_marks_where_they_always_were() {
    use koru_draw::el::col;
    let rail = Rect::new(10.0, 20.0, RAIL_W, 600.0);
    for (count, dividers) in [
        (0, vec![]),
        (1, vec![]),
        (3, vec![]),
        (4, vec![0]),
        (5, vec![0, 1]),
        (9, vec![0, 1]),
    ] {
        let found = rail_rects(rail, count, &dividers);
        let was = arithmetic_rail_rects(rail, count, &dividers);
        assert_eq!(found.len(), was.len());
        for (index, (a, b)) in found.into_iter().zip(was).enumerate() {
            assert_same_rect(
                a,
                b,
                &format!("mark {index} of {count}, dividers {dividers:?}"),
            );
        }
        let column = elements::rail_column(
            (0..count).map(|index| (elements::placeholder_mark(), dividers.contains(&index))),
        );
        let frame = elements::layout(
            &col("Probe").child(column),
            Rect::new(0.0, 0.0, RAIL_W, 2_000.0),
            koru_draw::el::FrameCx::root(),
        );
        let natural = frame.rect_of(elements::RAIL).unwrap().h;
        assert!(
            (natural - rail_height(count, dividers.len())).abs() < 0.01,
            "{count} marks, {} dividers: {natural}",
            dividers.len()
        );
    }
    // The tokens say what the constants say.
    let profile = koru_draw::theme::Profile::Desktop;
    assert_eq!(elements::CONTENT_GAP.token.px(profile), RAIL_CONTENT_GAP);
    assert_eq!(elements::PANEL_INSET.token.px(profile), PAD);
    assert_eq!(elements::MARK_GAP.token.px(profile), RAIL_GAP);
    assert_eq!(
        elements::DIVIDER_INSET.token.px(profile) * 2.0 + koru_draw::theme::BORDER_W as f32,
        RAIL_DIVIDER
    );
}

/// The open picker with every part showing: *Favorites* and its divider,
/// two agents, the **+**, the list with a favorite's row, and the Fast,
/// Context and level rows under it.
fn picker_with_everything() -> ChatSurface {
    let mut surface = surface();
    let (reader, _commands, _updates) = crate::reader::Reader::test_pair();
    surface.reader = Some(reader);
    with_opus_context(&mut surface);
    if let Some(AgentModels::Listed(models)) = surface.agent_models.get_mut(&agent("agent-claude"))
    {
        models[1].fast = Some(koru_ai_wire::AgentModelFast {
            on_by_default: false,
        });
        models[1].reasoning = Some(ReasoningControl::Levels {
            values: vec![ReasoningLevel::Low, ReasoningLevel::High],
            default: None,
            supports_off: false,
        });
    }
    surface.set_favorite_models(vec![FavoriteModel::new("claude", "sonnet")]);
    surface.offer_agent_settings(true);
    on_agent(
        &mut surface,
        "agent-claude",
        "opus",
        ReasoningChoice::Default,
    );
    surface.unmade_chat = surface.transcript.as_ref().map(|t| t.chat.id);
    open(&mut surface);
    surface
}

/// **Everything drawn inside the picker nests under its *Popover surface***
/// (D64, the user: *all primitives that draw inside the popover models
/// should be inside the popover surface*). Drawn with the inspector on, every
/// record — fills, strokes, text lines, icons, the search field's
/// `KoruTextInput`, the ☆ and the **+** — descends from the surface, through
/// the element tree's rail column and content column.
#[test]
fn everything_drawn_inside_the_picker_nests_under_the_popover_surface() {
    use koru_draw::inspect;
    let mut surface = picker_with_everything();
    let geometry = surface.picker_geometry().unwrap();
    assert!(geometry.fast.is_some() && geometry.context.is_some() && geometry.strip.is_some());
    // The pointer on *Opus*, so its ☆ and plate are drawn too.
    let row = surface.picker_row_rect(&geometry, 1).unwrap();
    surface.pointer = (row.x + 20.0, row.y + row.h * 0.5);

    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0),
    );
    let mut scene = Scene::new();
    surface.draw_model_picker(&mut scene, Affine::IDENTITY);
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
    let one = |name: &str| {
        let found = named(name);
        assert_eq!(found.len(), 1, "one {name}: {found:?}");
        found[0]
    };
    let under = |mut index: usize, ancestor: usize| loop {
        match records[index].parent {
            Some(parent) if parent == ancestor => return true,
            Some(parent) => index = parent,
            None => return false,
        }
    };

    let shell = one("Popover surface");
    assert_eq!(records[shell].parent, None);
    for (index, record) in records.iter().enumerate() {
        if index != shell {
            assert!(
                under(index, shell),
                "{} · {} lies outside the Popover surface",
                record.name,
                record.kind
            );
        }
    }

    // The tree: the picker, its rail column and its content column.
    let picker = one(elements::PICKER);
    let rail = one(elements::RAIL);
    let column = one(elements::RAIL_COLUMN);
    let content = one(elements::CONTENT);
    assert_eq!(records[rail].parent, Some(picker));
    assert_eq!(records[column].parent, Some(rail));
    assert_eq!(records[content].parent, Some(picker));
    for mark in named(elements::MARK) {
        assert_eq!(
            records[mark].parent,
            Some(column),
            "a mark is in the rail's column"
        );
    }
    assert_eq!(named(elements::MARK).len(), surface.picker_rail().len());
    assert!(!named("Rail divider").is_empty(), "Favorites is set off");
    let plus = named(elements::MANAGE_NAME);
    assert!(!plus.is_empty() && plus.iter().all(|record| under(*record, rail)));
    for part in [elements::SEARCH_ROW, elements::LIST, elements::FOOTER] {
        assert_eq!(records[one(part)].parent, Some(content), "{part}");
    }
    // The footer's parts: the strip and its segments, the pills, the
    // readout — each its own record inside the footer.
    let footer = one(elements::FOOTER);
    for part in [
        elements::EFFORT_STRIP,
        elements::FAST_PILL,
        elements::CONTEXT_PILL,
        elements::READOUT,
    ] {
        assert!(under(one(part), footer), "{part}");
    }
    let strip = one(elements::EFFORT_STRIP);
    let segments = named(elements::EFFORT_SEGMENT);
    assert_eq!(segments.len(), 2, "Low and High");
    assert!(
        segments
            .iter()
            .all(|segment| records[*segment].parent == Some(strip))
    );
    let field = one(elements::SEARCH_FIELD);
    assert!(under(field, content));
    assert!(
        named("KoruTextInput")
            .iter()
            .any(|input| under(*input, field)),
        "the search field draws its input inside its element"
    );
    // The leaves' own drawing sits inside them: the list's text and ☆.
    let list = one(elements::LIST);
    assert!(
        records
            .iter()
            .enumerate()
            .any(|(i, r)| r.kind == "primitive" && under(i, list)),
        "the rows' paint is the list's"
    );
    // Every element record is where the frame put it.
    let rail_record = &records[rail];
    assert_eq!(
        rail_record.bounds,
        [
            f64::from(geometry.rail.x),
            f64::from(geometry.rail.y),
            f64::from(geometry.rail.x + geometry.rail.w),
            f64::from(geometry.rail.y + geometry.rail.h),
        ]
    );
}

/// The picker's tree is valid as composed (ELEMENTS.final §4): every mark,
/// the **+** and ↻ a named button with a stable key, no key twice, and no
/// layout diagnostic.
#[test]
fn the_pickers_tree_has_named_buttons_and_no_diagnostics() {
    use koru_draw::el::{Name, Role};
    let surface = picker_with_everything();
    let geometry = surface.picker_geometry().unwrap();
    let spec = surface.picker_spec();
    let frame = elements::layout(
        &elements::tree(&spec, elements::Painted::default()),
        geometry.panel,
        koru_draw::el::FrameCx::root(),
    );
    assert_eq!(frame.diagnostics, [], "{:?}", frame.diagnostics);
    // Every node of the tree, depth first: the strip's segments are its
    // group's children.
    fn every(nodes: Vec<koru_draw::el::SemNode>, out: &mut Vec<koru_draw::el::SemNode>) {
        for mut node in nodes {
            let children = std::mem::take(&mut node.children);
            out.push(node);
            every(children, out);
        }
    }
    let mut all = Vec::new();
    every(frame.semantics(), &mut all);
    let buttons: Vec<_> = all
        .into_iter()
        .filter(|node| node.role == Role::Button)
        .collect();
    // Favorites, Claude, OpenCode, the + and ↻; Low and High under the
    // strip's group; Fast and 1M context.
    assert_eq!(buttons.len(), surface.picker_rail().len() + 2 + 2 + 2);
    assert!(
        buttons
            .iter()
            .any(|button| { button.name == Some(Name::Key(koru_i18n::Key::AiEffortHigh)) })
    );
    assert!(
        buttons
            .iter()
            .any(|button| { button.name == Some(Name::Key(koru_i18n::Key::AiModelFast)) })
    );
    assert!(
        frame.semantics().iter().any(|node| node.role == Role::Group
            && node.name == Some(Name::Key(koru_i18n::Key::AiEffortGroup))),
        "the strip is a group named Effort"
    );
    assert_eq!(
        buttons[0].name,
        Some(Name::Key(koru_i18n::Key::AiPickerFavorites))
    );
    assert_eq!(buttons[1].name, Some(Name::Text("Claude".into())));
    assert_eq!(buttons[1].selected, Some(true), "the chat's agent");
    assert!(
        buttons
            .iter()
            .any(|button| { button.name == Some(Name::Key(koru_i18n::Key::AiPickerManageAgents)) })
    );
    assert!(
        buttons
            .iter()
            .any(|button| { button.name == Some(Name::Key(koru_i18n::Key::AiModelsRefresh)) })
    );
    // The marks are what the input reads: each one's rectangle is a rail
    // rectangle, in the rail's order.
    let rail = surface.picker_rail();
    let rects = rail_rects(geometry.rail, rail.len(), &rail_dividers(&rail));
    for (entry, rect) in rail.iter().zip(rects) {
        let node = frame
            .node(elements::mark_key(entry))
            .expect("a mark per entry");
        assert_same_rect(node.rect, rect, &format!("{entry:?}"));
        assert_eq!(
            frame.hit_at(rect.x + rect.w * 0.5, rect.y + rect.h * 0.5),
            Some(elements::mark_key(entry))
        );
    }
    let plus = surface.picker_manage_rect(&geometry).unwrap();
    assert_same_rect(
        frame.node(elements::MANAGE_KEY).unwrap().rect,
        plus,
        "the +",
    );
}

/// **A begun chat's picker has no rail in its tree** (the user: *once the
/// chat is running, hide the model rail*): drawn with the inspector on,
/// there is no rail, rail column, mark or **+** record; the content column
/// is the picker's only child, still inside the *Popover surface*, and
/// fills the panel. Its semantics name ↻, the levels and the pills — no
/// mark — and the tree has no layout diagnostic.
#[test]
fn a_begun_chats_picker_tree_has_no_rail() {
    use koru_draw::el::{Name, Role};
    use koru_draw::inspect;
    let mut surface = picker_with_everything();
    begin(&mut surface);
    let geometry = surface.picker_geometry().unwrap();

    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0),
    );
    let mut scene = Scene::new();
    surface.draw_model_picker(&mut scene, Affine::IDENTITY);
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
    let one = |name: &str| {
        let found = named(name);
        assert_eq!(found.len(), 1, "one {name}: {found:?}");
        found[0]
    };
    let under = |mut index: usize, ancestor: usize| loop {
        match records[index].parent {
            Some(parent) if parent == ancestor => return true,
            Some(parent) => index = parent,
            None => return false,
        }
    };
    for gone in [
        elements::RAIL,
        elements::RAIL_COLUMN,
        elements::MARK,
        elements::MANAGE_NAME,
        "Rail divider",
    ] {
        assert!(named(gone).is_empty(), "no {gone}");
    }
    let shell = one("Popover surface");
    for (index, record) in records.iter().enumerate() {
        if index != shell {
            assert!(
                under(index, shell),
                "{} · {} lies outside the Popover surface",
                record.name,
                record.kind
            );
        }
    }
    let picker = one(elements::PICKER);
    let content = one(elements::CONTENT);
    assert_eq!(records[content].parent, Some(picker));
    let panel = geometry.panel;
    assert_eq!(
        records[content].bounds,
        [
            f64::from(panel.x),
            f64::from(panel.y),
            f64::from(panel.x + panel.w),
            f64::from(panel.y + panel.h),
        ],
        "the content fills the panel"
    );
    for part in [elements::SEARCH_ROW, elements::LIST, elements::FOOTER] {
        assert_eq!(records[one(part)].parent, Some(content), "{part}");
    }

    let spec = surface.picker_spec();
    let frame = elements::layout(
        &elements::tree(&spec, elements::Painted::default()),
        panel,
        FrameCx::root(),
    );
    assert_eq!(frame.diagnostics, [], "{:?}", frame.diagnostics);
    fn every(nodes: Vec<koru_draw::el::SemNode>, out: &mut Vec<koru_draw::el::SemNode>) {
        for mut node in nodes {
            let children = std::mem::take(&mut node.children);
            out.push(node);
            every(children, out);
        }
    }
    let mut all = Vec::new();
    every(frame.semantics(), &mut all);
    let buttons: Vec<_> = all
        .into_iter()
        .filter(|node| node.role == Role::Button)
        .collect();
    // ↻; Low and High; Fast and 1M context. No mark, no +.
    assert_eq!(buttons.len(), 5, "{buttons:?}");
    assert_eq!(
        buttons[0].name,
        Some(Name::Key(koru_i18n::Key::AiModelsRefresh))
    );
    assert!(buttons.iter().all(|button| {
        button.name != Some(Name::Key(koru_i18n::Key::AiPickerManageAgents))
            && button.name != Some(Name::Key(koru_i18n::Key::AiPickerFavorites))
            && button.name != Some(Name::Text("Claude".into()))
    }));
    for entry in [
        RailEntry::Favorites,
        RailEntry::Agent(agent("agent-claude")),
        RailEntry::Agent(agent("agent-opencode")),
    ] {
        assert!(
            frame.node(elements::mark_key(&entry)).is_none(),
            "{entry:?}"
        );
    }
}

/// **The list is rows of elements** (M1, approved by the user 2026-11-03):
/// each row a `Model row` in the one scroll element, whole rows from the
/// list's top where the arithmetic put them; each ☆ a press target of its
/// own where [`star_rect`] put it, over its row; the current model's ✓ the
/// 14-unit `IconSize::S` (it was 13), ending 2 before the ☆.
#[test]
fn the_list_is_rows_of_elements_in_the_shared_scroll_element() {
    let surface = picker_with_everything();
    let geometry = surface.picker_geometry().unwrap();
    let spec = surface.picker_spec();
    let frame = elements::layout(
        &elements::tree(&spec, elements::Painted::default()),
        geometry.panel,
        FrameCx::root(),
    );
    assert_eq!(frame.diagnostics, [], "{:?}", frame.diagnostics);
    let list = frame.node(elements::LIST_KEY).unwrap();
    assert_eq!(list.viewport, Some(geometry.list));
    let rows: Vec<_> = frame.named(elements::MODEL_ROW).collect();
    assert_eq!(rows.len(), surface.picker_rows().len());
    let mut stars = 0;
    for (index, node) in rows.iter().enumerate().take(geometry.visible_rows()) {
        let expected = Rect::new(
            geometry.list.x,
            geometry.list.y + index as f32 * ROW_H,
            geometry.list.w,
            ROW_H,
        );
        assert_same_rect(node.rect, expected, &format!("row {index}"));
        assert_eq!(surface.picker_row_rect(&geometry, index), Some(node.rect));
        if let Some(star) = &spec.list[index].star {
            stars += 1;
            let at = frame.node(star.key).unwrap().rect;
            assert_same_rect(at, star_rect(node.rect), &format!("☆ of row {index}"));
            assert_eq!(
                frame.hit_at(at.x + at.w * 0.5, at.y + at.h * 0.5),
                Some(star.key),
                "the ☆ is pressed over its row"
            );
            assert_eq!(
                frame.hit_at(node.rect.x + 20.0, node.rect.y + 5.0),
                spec.list[index].key,
                "the rest of the row is the row's"
            );
        }
    }
    assert!(stars > 0, "the agent's models can be starred");
    let check = frame.named(elements::CHECK).next().expect("the current ✓");
    assert_eq!(check.rect.w, 14.0, "IconSize::S");
    let current = spec.list.iter().position(|row| row.current).unwrap();
    let star = frame
        .node(spec.list[current].star.as_ref().unwrap().key)
        .unwrap()
        .rect;
    assert!((check.rect.x + check.rect.w + 2.0 - star.x).abs() < 0.01);
}

/// **The list's scrollbar is the shared one** (M1): the frame's, over the
/// rows shown; a press in its gutter takes the thumb and a drag moves the
/// list a whole row at a time; the thumb is lit while it is dragged.
#[test]
fn the_lists_scrollbar_drags_by_whole_rows() {
    let mut surface = surface();
    surface.agent_models.insert(
        agent("agent-claude"),
        AgentModels::Listed(
            (0..20)
                .map(|n| {
                    model(
                        &format!("m{n}"),
                        "claude",
                        Some(&format!("Model {n}")),
                        None,
                    )
                })
                .collect(),
        ),
    );
    open(&mut surface);
    let geometry = surface.picker_geometry().unwrap();
    let bar = surface.picker_scrollbar(&geometry).expect("it overflows");
    assert_eq!(
        Some(bar),
        koru_ui::scrollbar::ScrollbarGeom::for_content(
            geometry.list,
            surface.picker_rows().len() as f32 * ROW_H,
            0.0
        )
    );
    let x = bar.gutter_x + bar.gutter_w * 0.5;
    assert_eq!(
        surface.picker_mouse_down(x, bar.thumb_top + 2.0),
        Some(true)
    );
    assert_eq!(surface.picker.scroll_drag, Some(2.0));
    assert_eq!(surface.picker.offset, 0);
    let travel = bar.track_bottom - bar.track_top - bar.thumb_h;
    surface.picker_mouse_move(x, bar.thumb_top + 2.0 + travel);
    assert_eq!(
        surface.picker.offset,
        surface.picker_rows().len() - geometry.visible_rows()
    );
    surface.picker_mouse_move(x, bar.thumb_top + 2.0 + travel * 0.5);
    let middle = surface.picker.offset;
    assert!(middle > 0 && middle < surface.picker_rows().len() - geometry.visible_rows());
}

/// **The strip shows the levels the model offers, and no others** (D76): a
/// segment each, in the model's order, its short word shown and its full
/// word its name; the chat's level raised; equal columns filling the
/// strip. A model with Off has an Off segment; *Ultra* is no level any
/// model offers yet, so none shows.
#[test]
fn the_strip_shows_exactly_the_models_levels() {
    let mut surface = surface();
    surface.lang = Lang::En;
    if let Some(AgentModels::Listed(models)) = surface.agent_models.get_mut(&agent("agent-claude"))
    {
        models[1].reasoning = Some(ReasoningControl::Levels {
            values: vec![
                ReasoningLevel::Low,
                ReasoningLevel::Medium,
                ReasoningLevel::High,
                ReasoningLevel::XHigh,
                ReasoningLevel::Max,
            ],
            default: Some(ReasoningLevel::XHigh),
            supports_off: false,
        });
        models[2].reasoning = Some(ReasoningControl::Levels {
            values: vec![ReasoningLevel::Low, ReasoningLevel::High],
            default: None,
            supports_off: true,
        });
    }
    let words = |surface: &ChatSurface| {
        let footer = surface.picker_footer().expect("a footer");
        footer
            .segments
            .iter()
            .map(|segment| (segment.label.clone(), segment.name, segment.selected))
            .collect::<Vec<_>>()
    };
    on_agent(
        &mut surface,
        "agent-claude",
        "opus",
        ReasoningChoice::Default,
    );
    open(&mut surface);
    assert_eq!(
        words(&surface),
        [
            (String::from("Low"), Key::AiEffortLow, false),
            (String::from("Med"), Key::AiEffortMedium, false),
            (String::from("High"), Key::AiEffortHigh, false),
            (String::from("XH"), Key::AiEffortXHigh, true),
            (String::from("Max"), Key::AiEffortMax, false),
        ],
        "the model's own level is raised while the chat is on Default"
    );
    let strip = surface.picker_geometry().unwrap().strip.unwrap();
    let rects: Vec<Rect> = surface
        .picker_footer()
        .unwrap()
        .segments
        .iter()
        .map(|segment| surface.picker_segment_rect(segment.choice).unwrap())
        .collect();
    for (index, rect) in rects.iter().enumerate() {
        assert!((rect.w - rects[0].w).abs() < 0.01, "equal columns");
        assert!((rect.h - SEGMENT_H).abs() < 0.01, "28 tall");
        if index > 0 {
            let gap = rect.x - (rects[index - 1].x + rects[index - 1].w);
            assert!((gap - 2.0).abs() < 0.01, "2 between");
        }
    }
    assert!(
        (rects[0].x - (strip.x + 2.0)).abs() < 0.01,
        "2 inside the strip"
    );
    let last = rects[rects.len() - 1];
    assert!((last.x + last.w - (strip.x + strip.w - 2.0)).abs() < 0.01);
    assert!((strip.h - STRIP_H).abs() < 0.01);
    surface.close_picker();

    on_agent(
        &mut surface,
        "agent-claude",
        "sonnet",
        ReasoningChoice::Level(ReasoningLevel::High),
    );
    open(&mut surface);
    assert_eq!(
        words(&surface),
        [
            (String::from("Off"), Key::AiEffortOff, false),
            (String::from("Low"), Key::AiEffortLow, false),
            (String::from("High"), Key::AiEffortHigh, true),
        ]
    );
    assert!(
        surface
            .picker_footer()
            .unwrap()
            .readout
            .first()
            .is_some_and(|readout| readout == "Effort: High"),
        "no (default): the model states none"
    );
}

/// **A pill shows only when the model offers it, and a footer only when it
/// offers a part** (D76): levels alone are the strip and the readout;
/// Fast alone is its pill and no strip; a model with neither has no
/// footer and no divider.
#[test]
fn the_pills_and_the_footer_show_only_what_the_model_offers() {
    let mut surface = surface();
    surface.lang = Lang::En;
    with_opus_context(&mut surface);
    if let Some(AgentModels::Listed(models)) = surface.agent_models.get_mut(&agent("agent-claude"))
    {
        // Sonnet: Fast and nothing else; the default entry: levels only.
        models[2].fast = Some(koru_ai_wire::AgentModelFast {
            on_by_default: true,
        });
        models[0].reasoning = Some(ReasoningControl::Levels {
            values: vec![ReasoningLevel::Low, ReasoningLevel::High],
            default: Some(ReasoningLevel::Low),
            supports_off: false,
        });
    }
    let parts = |surface: &ChatSurface| {
        let footer = surface.picker_footer();
        (
            footer.as_ref().is_some_and(|footer| footer.levels),
            footer.as_ref().and_then(|footer| {
                footer
                    .fast
                    .as_ref()
                    .map(|pill| (pill.label.clone(), pill.on))
            }),
            footer
                .as_ref()
                .and_then(|footer| footer.context.as_ref().map(|pill| pill.label.clone())),
            footer.map(|footer| footer.readout.first().cloned()),
        )
    };

    on_agent(
        &mut surface,
        "agent-claude",
        "sonnet",
        ReasoningChoice::Default,
    );
    open(&mut surface);
    assert_eq!(
        parts(&surface),
        (false, Some((String::from("Fast"), true)), None, Some(None)),
        "Fast alone: its pill, on by the model's default; no strip, no readout"
    );
    let geometry = surface.picker_geometry().unwrap();
    assert!(geometry.strip.is_none() && geometry.context.is_none());
    assert!((geometry.footer.unwrap().h - FOOTER_TOGGLES_H).abs() < 0.01);
    assert!(geometry.fast.is_some());
    surface.close_picker();

    on_agent(
        &mut surface,
        "agent-claude",
        "opus",
        ReasoningChoice::Default,
    );
    open(&mut surface);
    assert_eq!(
        parts(&surface),
        (false, None, Some(String::from("1M context")), Some(None)),
        "a longer window alone: its pill"
    );
    surface.close_picker();

    on_agent(
        &mut surface,
        "agent-claude",
        "default",
        ReasoningChoice::Default,
    );
    open(&mut surface);
    assert_eq!(
        parts(&surface),
        (
            true,
            None,
            None,
            Some(Some(String::from("Effort: Low (default)")))
        ),
        "levels alone: the strip and the readout"
    );
    let geometry = surface.picker_geometry().unwrap();
    assert!((geometry.footer.unwrap().h - FOOTER_H).abs() < 0.01);
    assert!(geometry.fast.is_none() && geometry.context.is_none());
    surface.close_picker();

    on_agent(
        &mut surface,
        "agent-opencode",
        "deepseek/v4-pro",
        ReasoningChoice::Default,
    );
    open(&mut surface);
    assert_eq!(
        parts(&surface),
        (false, None, None, None),
        "nothing offered"
    );
    let geometry = surface.picker_geometry().unwrap();
    assert!(geometry.footer.is_none(), "no footer, no divider");
    let spec = surface.picker_spec();
    let frame = elements::layout(
        &elements::tree(&spec, elements::Painted::default()),
        geometry.panel,
        FrameCx::root(),
    );
    assert!(frame.named(elements::FOOTER).next().is_none());
    assert!(frame.named("Divider").next().is_none());
}

/// **The footer's measurements** (D76, the design *A1a*): 87 at its tallest
/// — the divider, 10, the 32 strip, 8, the 28 pills' row, 8 and the content
/// column's 4 under it, 12 from the panel's foot — where the Fast row, the
/// Context row and the slider's footer took 124 (30 + 30 + 64); 47 for
/// pills alone. A pill is 28 tall and as wide as its words.
#[test]
fn the_footer_is_the_designs_size() {
    assert_eq!(FOOTER_H, 87.0);
    assert_eq!(FOOTER_TOGGLES_H, 47.0);
    let (w, h) = size(360.0, rail_height(2, 0));
    assert_eq!(h, PAD * 2.0 + SEARCH_H + LIST_ROWS as f32 * ROW_H + 87.0);
    let geometry = geometry(Rect::new(0.0, 0.0, w, h), true, true, true);
    let footer = geometry.footer.unwrap();
    let strip = geometry.strip.unwrap();
    let fast = geometry.fast.unwrap();
    assert!((footer.y + footer.h - (h - PAD)).abs() < 0.01);
    assert!((strip.y - (footer.y + 1.0 + 10.0)).abs() < 0.01);
    assert!((fast.y - (strip.y + strip.h + 8.0)).abs() < 0.01);
    assert!((fast.h - PILL_H).abs() < 0.01);
    assert!(
        (h - (fast.y + fast.h) - 12.0).abs() < 0.01,
        "12 from the foot"
    );
    assert!(
        (w - (strip.x + strip.w) - 12.0).abs() < 0.01,
        "12 from the end"
    );

    let mut surface = picker_with_everything();
    surface.lang = Lang::En;
    let geometry = surface.picker_geometry().unwrap();
    let (fast, context) = (geometry.fast.unwrap(), geometry.context.unwrap());
    assert!(
        context.w > fast.w,
        "*1M context* has more words than *Fast*"
    );
    assert!((context.x - (fast.x + fast.w + 6.0)).abs() < 0.01);
}
