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
        let (w, h) = size(composer, rail_height(2, 0));
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
        assert!((geometry.list.h - LIST_ROWS as f32 * ROW_H).abs() < 0.5);
        assert!(geometry.reset().unwrap().x + 24.0 <= footer.x + footer.w);
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
    let context = geometry.context.unwrap();
    let fast = geometry.fast.unwrap();
    for column in [geometry.search, geometry.list, footer, context, fast] {
        assert!((column.x - (rail_end + RAIL_CONTENT_GAP)).abs() < 0.01);
        assert!(column.x + column.w <= geometry.panel.x + geometry.panel.w);
    }
    assert_eq!(h, size(360.0, rail_height(3, 1)).1, "the height is fixed");
    assert_eq!(
        size(360.0, 0.0).1,
        size(280.0, 0.0).1,
        "the gap takes width, never height"
    );
}

/// D13: one size — search, six rows, the Context row and the level's
/// footer — whatever the agent lists: a short list leaves room, a long one
/// scrolls, and a model without either row gives its room to the list.
#[test]
fn the_picker_keeps_one_height_from_agent_to_agent() {
    let (_, h) = size(360.0, rail_height(2, 0));
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
    assert!(levels.list.h >= rows - 0.5, "no Context row");
    let bare = geometry(Rect::new(0.0, 0.0, 360.0, h), false, false, false);
    assert!(bare.list.h > rows + ROW_H - 0.5, "neither row: more rows");
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

/// **A begun chat keeps its agent in a search too** (D67): another
/// agent's models are found and shown, and cannot be chosen — as on that
/// agent's own list.
#[test]
fn a_search_in_a_begun_chat_finds_other_agents_and_keeps_its_own() {
    let mut surface = surface();
    on_agent(
        &mut surface,
        "agent-claude",
        "sonnet",
        ReasoningChoice::Default,
    );
    begin(&mut surface);
    open(&mut surface);
    type_query(&mut surface, "beam");
    let rows = surface.picker_rows();
    assert_eq!(rows.len(), 2);
    assert!(surface.picker_choosable(&rows).iter().all(|can| !can));
    assert_eq!(surface.picker.highlight, None);
    let geometry = surface.picker_geometry().unwrap();
    let row = surface.picker_row_rect(&geometry, 1).unwrap();
    surface.picker_mouse_down(row.x + 20.0, row.y + row.h * 0.5);
    let model = &surface.transcript.as_ref().unwrap().chat.current_model;
    assert_eq!(model.provider, agent("agent-claude"), "the chat keeps it");
    assert_eq!(model.requested_model, "sonnet");
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
    surface.picker.focus = PickerFocus::Slider;
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
    assert!(!surface.picker_rows().contains(&Row::LegacyMore));
}

/// Enter or Right on the row opens the flyout beside it; Up and Down walk
/// it, Left closes it, Enter chooses — the older model is the chat's, the
/// flyout closes and the picker stays open (D66).
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
    let geometry = surface.picker_geometry().unwrap();
    let parent = surface.picker_row_rect(&geometry, 3).unwrap();
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
    assert!(surface.chat_model_open, "the picker stays open");
    assert!(surface.picker.legacy.is_none(), "the flyout closes");
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
/// rows. Rows, the rail, ↻, the panel's foot and the *Legacy models ›*
/// flyout show the arrow; the search field shows the I-beam.
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
    for index in 0..3 {
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
    assert!(surface.open_legacy_menu(3));
    let flyout = surface
        .picker
        .legacy
        .as_ref()
        .unwrap()
        .panel(surface.menu_viewport());
    assert_eq!(
        at(&mut surface, flyout.x + 8.0, flyout.y + 8.0),
        CursorIcon::Default,
        "the flyout"
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
    let frame = elements::layout(&elements::tree(&spec, &mut |_| None), geometry.panel, cx);
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
/// here as the record the frame is held to.
fn arithmetic_geometry(panel: Rect, levels: bool, context: bool, fast: bool) -> Geometry {
    let inner_x = panel.x + RAIL_W + RAIL_CONTENT_GAP;
    let inner_w = (panel.w - RAIL_W - RAIL_CONTENT_GAP - PAD).max(1.0);
    let mut bottom = panel.y + panel.h - PAD;
    let mut below = |on: bool, h: f32| {
        on.then(|| {
            bottom -= h;
            Rect::new(inner_x, bottom, inner_w, h)
        })
    };
    let footer = below(levels, FOOTER_H);
    let context = below(context, CONTEXT_H);
    let fast = below(fast, FAST_H);
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
/// rail, search box, field, ↻, list and the three rows under it, for narrow,
/// middling and wide panels with every combination of rows — pixel for
/// pixel.
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
    assert!(geometry.fast.is_some() && geometry.context.is_some() && geometry.footer.is_some());
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
    for part in [
        elements::SEARCH_ROW,
        elements::LIST,
        elements::FAST_ROW,
        elements::CONTEXT_ROW,
        elements::EFFORT_FOOTER,
    ] {
        assert_eq!(records[one(part)].parent, Some(content), "{part}");
    }
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
        &elements::tree(&spec, &mut |_| None),
        geometry.panel,
        koru_draw::el::FrameCx::root(),
    );
    assert_eq!(frame.diagnostics, [], "{:?}", frame.diagnostics);
    let buttons: Vec<_> = frame
        .semantics()
        .into_iter()
        .filter(|node| node.role == Role::Button)
        .collect();
    // Favorites, Claude, OpenCode, the + and ↻.
    assert_eq!(buttons.len(), surface.picker_rail().len() + 2);
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
        &elements::tree(&spec, &mut |_| None),
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
