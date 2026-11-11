// SPDX-License-Identifier: MPL-2.0

//! **The model picker, pinned before it moves** (Bots P1.8 picker plan
//! §4.4, S0): for each state below, every inspector record but its `source`,
//! the whole semantics tree and a grid of what each point is — the frame's
//! hit target, the cursor, the tooltip and the part a press reads — and, over
//! scripted sessions, the keyboard's every step and every command in order.
//! The goldens were recorded from the code before the extraction; see
//! `crate::pins` for how a pin reports a change.

use std::sync::mpsc::Receiver;

use super::*;
use crate::ChatSurface;
use crate::pins::{Grid, Pins, drain, records, rect, semantics};
use crate::reader::{Command, Reader};
use koru_ai_core::{ChatId, InferenceAttemptId, ModelSelection, ReasoningLevel};
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

/// **Every part the picker has**: Claude and OpenCode ready, Claude's *Opus*
/// with three levels and Off (its own default *Medium*), Fast and a 1M
/// window; two older Claude models; OpenCode's two groups; *Sonnet*
/// starred; the **+** offered; a new Claude chat on *Opus*, not made yet;
/// a reader whose commands the pin reads.
fn base() -> (ChatSurface, Receiver<Command>) {
    let mut surface = ChatSurface::new();
    surface.lang = Lang::En;
    surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
    let (reader, commands, _updates) = Reader::test_pair();
    surface.reader = Some(reader);
    surface.settings = Some(settings(
        &[("agent-claude", "Claude"), ("agent-opencode", "OpenCode")],
        None,
    ));
    for id in ["agent-claude", "agent-opencode"] {
        surface.agent_status.insert(agent(id), AgentStatus::Ready);
    }
    let mut opus = model("opus", "claude", Some("Opus 4.7"), None);
    opus.reasoning = Some(ReasoningControl::Levels {
        values: vec![
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
        ],
        default: Some(ReasoningLevel::Medium),
        supports_off: true,
    });
    opus.fast = Some(koru_ai_wire::AgentModelFast {
        on_by_default: false,
    });
    opus.context = Some(koru_ai_wire::AgentModelContext {
        id: "opus[1m]".into(),
        tokens: 1_000_000,
    });
    let mut old = model("claude-2", "claude", Some("Claude 2"), None);
    old.legacy = true;
    let mut older = model("claude-1", "claude", Some("Claude 1"), Some("The first"));
    older.legacy = true;
    surface.agent_models.insert(
        agent("agent-claude"),
        AgentModels::Listed(vec![
            model(
                "default",
                "claude",
                Some("Default (recommended)"),
                Some("Opus 4.7 · Most capable"),
            ),
            opus,
            model(
                "sonnet",
                "claude",
                Some("Sonnet 4.6"),
                Some("Fast and able"),
            ),
            old,
            older,
        ]),
    );
    surface.agent_models.insert(
        agent("agent-opencode"),
        AgentModels::Listed(vec![
            model("opencode/beam-free", "opencode", Some("Beam Free"), None),
            model("deepseek/v4-pro", "deepseek", None, None),
        ]),
    );
    surface.set_favorite_models(vec![FavoriteModel::new("claude", "sonnet")]);
    surface.offer_agent_settings(true);
    on_agent(&mut surface, "agent-claude", "opus");
    surface.unmade_chat = surface.transcript.as_ref().map(|t| t.chat.id);
    surface.composer_rect();
    (surface, commands)
}

fn on_agent(surface: &mut ChatSurface, id: &str, model: &str) {
    let mut transcript = crate::unmade_transcript(ModelSelection {
        provider: agent(id),
        requested_model: model.into(),
        default_reasoning: ReasoningChoice::Default,
    });
    transcript.chat.id = ChatId::new();
    surface.transcript = Some(transcript);
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

/// The frame a press reads: the tree laid out with no pointer on it.
fn frame_of(surface: &ChatSurface) -> koru_draw::el::Frame {
    let geometry = surface.picker_geometry().expect("open");
    elements::layout(
        &elements::tree(&surface.picker_spec(), elements::Painted::default()),
        geometry.panel,
        FrameCx::root(),
    )
}

fn centre(rect: Rect) -> (f32, f32) {
    (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
}

/// Where the element keyed `key` is, in the frame a press reads.
fn at_key(surface: &ChatSurface, key: koru_draw::el::Key) -> (f32, f32) {
    centre(frame_of(surface).node(key).expect("drawn").rect)
}

/// Where list row `index` is drawn.
fn at_row(surface: &ChatSurface, index: usize) -> (f32, f32) {
    let frame = frame_of(surface);
    let row = frame
        .named(elements::MODEL_ROW)
        .nth(index)
        .expect("a row")
        .rect;
    (row.x + 20.0, row.y + row.h * 0.5)
}

/// The list row whose label is `label`.
fn row_labelled(surface: &ChatSurface, label: &str) -> usize {
    surface
        .picker_spec()
        .list
        .iter()
        .position(|row| row.label == label)
        .unwrap_or_else(|| panic!("no row {label}"))
}

// --- The states ------------------------------------------------------------

/// Everything shown: the rail on Claude's list, the pointer on *Opus*.
fn everything() -> ChatSurface {
    let (mut surface, _) = base();
    assert!(surface.open_model_picker());
    surface.select_rail_entry(RailEntry::Agent(agent("agent-claude")));
    let (x, y) = at_row(&surface, 1);
    surface.pointer = (x, y);
    surface
}

/// The chat has begun: no rail, the keyboard on the strip.
fn begun() -> ChatSurface {
    let mut surface = everything();
    begin(&mut surface);
    surface.picker.focus = PickerFocus::Effort;
    surface
}

/// A query across every agent, the pointer on the first found row.
fn searching() -> ChatSurface {
    let (mut surface, _) = base();
    assert!(surface.open_model_picker());
    for ch in "o".chars() {
        let text = ch.to_string();
        surface.picker_key(
            &WinitKey::Character(text.clone().into()),
            Some(&text),
            ModifiersState::empty(),
        );
    }
    let (x, y) = at_row(&surface, 1);
    surface.pointer = (x, y);
    surface
}

/// *Legacy models* expanded, the keyboard on the *Fast* pill.
fn legacy_open() -> ChatSurface {
    let mut surface = everything();
    let legacy = row_labelled(&surface, "Legacy models (2)");
    surface.choose_picker_row(legacy);
    surface.picker.focus = PickerFocus::Fast;
    surface.pointer = (0.0, 0.0);
    surface
}

/// Twenty Claude models scrolled down, the pointer on the scrollbar.
fn scrolled() -> ChatSurface {
    let (mut surface, _) = base();
    if let Some(AgentModels::Listed(models)) = surface.agent_models.get_mut(&agent("agent-claude"))
    {
        models.extend((0..20).map(|n| {
            model(
                &format!("model-{n:02}"),
                "claude",
                Some(&format!("Model {n:02}")),
                None,
            )
        }));
    }
    assert!(surface.open_model_picker());
    surface.select_rail_entry(RailEntry::Agent(agent("agent-claude")));
    surface.picker_scroll(-7.0);
    let geometry = surface.picker_geometry().unwrap();
    surface.pointer = (
        geometry.list.x + geometry.list.w - 3.0,
        geometry.list.y + 20.0,
    );
    surface.picker.focus = PickerFocus::List;
    surface
}

/// Ten agents: the rail's second page, the keyboard on the rail.
fn paged_rail() -> ChatSurface {
    let (mut surface, _) = base();
    let agents = [
        ("agent-claude", "Claude"),
        ("agent-opencode", "OpenCode"),
        ("agent-grok", "Grok"),
        ("agent-kimi", "Kimi"),
        ("agent-codex", "Codex"),
        ("agent-copilot", "Copilot"),
        ("agent-qwen", "Qwen"),
        ("agent-antigravity", "Antigravity"),
        ("agent-muse", "Muse"),
        ("agent-devin", "Devin"),
    ];
    surface.settings = Some(settings(&agents, Some("agent-claude")));
    for (id, _) in agents {
        surface.agent_status.insert(agent(id), AgentStatus::Ready);
    }
    surface
        .agent_status
        .insert(agent("agent-kimi"), AgentStatus::NotInstalled);
    assert!(surface.open_model_picker());
    let (x, y) = at_key(&surface, elements::RAIL_NEXT_KEY);
    surface.picker_mouse_down(x, y);
    surface.picker.focus = PickerFocus::Rail;
    surface
}

/// A begun Ollama chat: a built-in provider's own list, no rail.
fn builtin() -> ChatSurface {
    let (mut surface, _) = base();
    surface.ollama_models = vec!["qwen3".into(), "llama3".into()];
    on_agent(&mut surface, AiProviderId::OLLAMA, "llama3");
    begin(&mut surface);
    surface.unmade_chat = None;
    assert!(surface.open_model_picker());
    let (x, y) = at_row(&surface, 0);
    surface.pointer = (x, y);
    surface
}

/// What a press at `x`, `y` reads first, in the order
/// [`ChatSurface::picker_mouse_down`] asks.
fn part_at(surface: &ChatSurface, frame: &koru_draw::el::Frame, x: f32, y: f32) -> &'static str {
    let geometry = surface.picker_geometry().unwrap();
    if !geometry.panel.contains(x, y) {
        return if surface.chat_model_button().contains(x, y) {
            "chip"
        } else {
            "outside"
        };
    }
    if frame
        .scrollbar(elements::LIST_KEY)
        .is_some_and(|bar| bar.in_gutter(x, y))
    {
        return "gutter";
    }
    let hit = frame.hit_at(x, y);
    if hit.is_some()
        && (hit == Some(elements::RAIL_PREVIOUS_KEY)
            || hit == Some(elements::RAIL_NEXT_KEY)
            || hit == Some(elements::MANAGE_KEY)
            || surface
                .picker_visible_rail()
                .iter()
                .any(|entry| hit == Some(elements::mark_key(entry))))
    {
        return "rail";
    }
    let inside = |rect: Option<Rect>| rect.is_some_and(|rect| rect.contains(x, y));
    if geometry.refresh.contains(x, y) {
        "refresh"
    } else if geometry.search.contains(x, y) {
        "search"
    } else if inside(geometry.strip) {
        "strip"
    } else if inside(geometry.fast) {
        "fast"
    } else if inside(geometry.context) {
        "context"
    } else if geometry.list.contains(x, y) {
        "list"
    } else {
        "panel"
    }
}

/// The three pins of one state: its records, its semantics and its grid.
fn pin_state(pins: &mut Pins, name: &str, mut surface: ChatSurface, golden: [&str; 3]) {
    use koru_draw::inspect;
    let geometry = surface.picker_geometry().expect("open");
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0),
    );
    let mut scene = Scene::new();
    surface.draw_model_picker(&mut scene, Affine::IDENTITY);
    let snapshot = capture.finish();
    let geometry_line = format!(
        "panel {} rail {} search {} field {} refresh {} list {} footer {:?} strip {:?} context {:?} fast {:?}\n",
        rect(geometry.panel),
        rect(geometry.rail),
        rect(geometry.search),
        rect(geometry.field),
        rect(geometry.refresh),
        rect(geometry.list),
        geometry.footer.map(rect),
        geometry.strip.map(rect),
        geometry.context.map(rect),
        geometry.fast.map(rect),
    );
    pins.check(
        &format!("{name}.records"),
        &(geometry_line + &records(&snapshot)),
        golden[0],
    );

    let frame = frame_of(&surface);
    pins.check(
        &format!("{name}.semantics"),
        &format!(
            "diagnostics {:?}\n{}",
            frame.diagnostics,
            semantics(&frame.semantics())
        ),
        golden[1],
    );

    let grid = Grid::over(geometry.panel, 12.0, 6.0, |x, y| {
        let tooltip = surface
            .picker_tooltip_at(&geometry, x, y)
            .map(|tip| format!("{:#x} {:?} {}", tip.key, tip.label, rect(tip.anchor)));
        format!(
            "{} | hit {:?} | cursor {:?} | tooltip {:?}",
            part_at(&surface, &frame, x, y),
            frame.hit_at(x, y),
            surface.picker_cursor(x, y),
            tooltip,
        )
    });
    pins.check(&format!("{name}.grid"), &grid, golden[2]);
}

/// **The tree is the control's** (S2b): the one field the pins leave out,
/// each record's construction site, says the picker's elements are now
/// built in `koru-agent-config-ui`.
#[test]
fn the_pickers_elements_are_built_by_the_control() {
    use koru_draw::inspect;
    let mut surface = everything();
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0),
    );
    let mut scene = Scene::new();
    surface.draw_model_picker(&mut scene, Affine::IDENTITY);
    let snapshot = capture.finish();
    let elements: Vec<_> = snapshot
        .records
        .iter()
        .filter(|record| record.kind == "element")
        .collect();
    assert!(elements.len() > 20, "{}", elements.len());
    for record in elements {
        assert!(
            record.source.contains("koru-agent-config-ui"),
            "{} is built at {}",
            record.name,
            record.source
        );
    }
}

macro_rules! golden {
    ($name:literal) => {
        [
            include_str!(concat!("pins/", $name, ".records.txt")),
            include_str!(concat!("pins/", $name, ".semantics.txt")),
            include_str!(concat!("pins/", $name, ".grid.txt")),
        ]
    };
}

/// **What each state draws, declares and hits** — its inspector records but
/// `source`, its semantics and its grid — as it did before the move.
#[test]
fn every_state_draws_declares_and_hits_as_before() {
    let mut pins = Pins::default();
    pin_state(&mut pins, "everything", everything(), golden!("everything"));
    pin_state(&mut pins, "begun", begun(), golden!("begun"));
    pin_state(&mut pins, "searching", searching(), golden!("searching"));
    pin_state(
        &mut pins,
        "legacy-open",
        legacy_open(),
        golden!("legacy-open"),
    );
    pin_state(&mut pins, "scrolled", scrolled(), golden!("scrolled"));
    pin_state(&mut pins, "paged-rail", paged_rail(), golden!("paged-rail"));
    pin_state(&mut pins, "builtin", builtin(), golden!("builtin"));
    pins.finish();
}

// --- The transcripts -------------------------------------------------------

/// The picker's state and the chat's, on one line.
fn state_line(surface: &ChatSurface) -> String {
    let chat = surface.transcript.as_ref().map(|transcript| {
        format!(
            "{}/{} {:?} mode={:?} fast={:?}",
            transcript.chat.current_model.provider.as_str(),
            transcript.chat.current_model.requested_model,
            transcript.chat.current_model.default_reasoning,
            transcript.chat.agent_mode,
            transcript.chat.agent_fast,
        )
    });
    format!(
        "open={} focus={:?} rail={:?} page={} hl={:?} off={} q={:?} legacy={} refreshing={} | chat={:?} status={:?} note={:?}",
        surface.chat_model_open,
        surface.picker.focus,
        surface.picker.rail,
        surface.picker.rail_offset,
        surface.picker.highlight,
        surface.picker.offset,
        surface.picker.query.raw_text(),
        surface.picker.legacy_open,
        surface.picker.refreshing.len(),
        chat,
        surface.status,
        surface.copy_feedback.label(),
    )
}

/// What the host is asked for since the last step.
fn requests(surface: &mut ChatSurface) -> String {
    format!(
        "favorites={:?} settings={}",
        surface.take_favorite_models_request(),
        surface.take_agent_settings_request()
    )
}

/// **The keyboard alone** (§9.5): Tab and Shift+Tab round every stop, the
/// rail's arrows, the list's, *Legacy models*' Right and Left, the strip's
/// Left and Right, Space and Enter on the pills, typing, Enter, Esc twice —
/// after each key the picker's state, the chat's and what was sent.
#[test]
fn the_keyboard_walks_the_picker_as_before() {
    let (mut surface, commands) = base();
    let mut out = String::new();
    surface.open_model_picker();
    out.push_str(&format!(
        "open -> {} | {}\n",
        drain(&commands),
        state_line(&surface)
    ));
    let named = |key: NamedKey| {
        (
            WinitKey::Named(key),
            None::<String>,
            ModifiersState::empty(),
        )
    };
    let shift = |key: NamedKey| (WinitKey::Named(key), None::<String>, ModifiersState::SHIFT);
    let typed = |ch: char| {
        (
            WinitKey::Character(ch.to_string().into()),
            Some(ch.to_string()),
            ModifiersState::empty(),
        )
    };
    let script = [
        named(NamedKey::Tab),
        named(NamedKey::ArrowDown),
        named(NamedKey::ArrowDown),
        named(NamedKey::ArrowUp),
        named(NamedKey::ArrowUp),
        named(NamedKey::ArrowDown),
        named(NamedKey::ArrowRight),
        named(NamedKey::ArrowDown),
        named(NamedKey::ArrowDown),
        named(NamedKey::ArrowRight),
        named(NamedKey::ArrowDown),
        named(NamedKey::ArrowDown),
        named(NamedKey::ArrowUp),
        named(NamedKey::ArrowUp),
        named(NamedKey::ArrowLeft),
        named(NamedKey::ArrowUp),
        named(NamedKey::ArrowUp),
        named(NamedKey::Enter),
        named(NamedKey::Tab),
        named(NamedKey::ArrowRight),
        named(NamedKey::ArrowRight),
        named(NamedKey::ArrowLeft),
        named(NamedKey::ArrowLeft),
        named(NamedKey::ArrowLeft),
        named(NamedKey::Tab),
        named(NamedKey::Space),
        named(NamedKey::Tab),
        named(NamedKey::Enter),
        named(NamedKey::Tab),
        shift(NamedKey::Tab),
        shift(NamedKey::Tab),
        shift(NamedKey::Tab),
        typed('s'),
        typed('o'),
        typed('n'),
        named(NamedKey::ArrowDown),
        named(NamedKey::ArrowUp),
        named(NamedKey::Enter),
        named(NamedKey::Escape),
        named(NamedKey::Escape),
        named(NamedKey::Escape),
    ];
    for (step, (key, text, mods)) in script.iter().enumerate() {
        let handled = surface.picker_key(key, text.as_deref(), *mods);
        let name = match key {
            WinitKey::Named(named) => format!("{named:?}"),
            other => format!("{other:?}"),
        };
        out.push_str(&format!(
            "{step:02} {}{name} -> {:?} {} {} | {}\n",
            if mods.shift_key() { "Shift+" } else { "" },
            handled,
            drain(&commands),
            requests(&mut surface),
            state_line(&surface)
        ));
    }
    let mut pins = Pins::default();
    pins.check("keyboard", &out, include_str!("pins/keyboard.txt"));
    pins.finish();
}

/// One press, and what came of it.
fn press_at(
    out: &mut String,
    surface: &mut ChatSurface,
    commands: &Receiver<Command>,
    what: &str,
    (x, y): (f32, f32),
) {
    let handled = surface.picker_mouse_down(x, y);
    out.push_str(&format!(
        "{what} @{} -> {:?} {} {} | {}\n",
        rect(Rect::new(x, y, 0.0, 0.0)),
        handled,
        drain(commands),
        requests(surface),
        state_line(surface)
    ));
}

/// A press on the element keyed `key`, or a line saying it is not drawn.
fn press_key(
    out: &mut String,
    surface: &mut ChatSurface,
    commands: &Receiver<Command>,
    what: &str,
    key: koru_draw::el::Key,
) {
    let found = surface
        .picker_geometry()
        .and_then(|_| frame_of(surface).node(key).map(|node| centre(node.rect)));
    match found {
        Some(at) => press_at(out, surface, commands, what, at),
        None => out.push_str(&format!("{what}: not drawn\n")),
    }
}

/// A press on the row labelled `label`, or a line saying there is none.
fn press_row(
    out: &mut String,
    surface: &mut ChatSurface,
    commands: &Receiver<Command>,
    label: &str,
) {
    let index = surface.picker_geometry().and_then(|_| {
        surface
            .picker_spec()
            .list
            .iter()
            .position(|row| row.label == label)
    });
    match index {
        Some(index) => {
            let at = at_row(surface, index);
            press_at(out, surface, commands, label, at);
        }
        None => out.push_str(&format!("{label}: no row\n")),
    }
}

/// A scripted session of presses: open, another agent's model, back to
/// Claude's, a level, Fast, the window, ↻, a ☆, *Legacy models*, *Default*,
/// a press outside.
fn press_session(out: &mut String, mut surface: ChatSurface, commands: &Receiver<Command>) {
    surface.open_model_picker();
    out.push_str(&format!(
        "open -> {} | {}\n",
        drain(commands),
        state_line(&surface)
    ));
    let opencode = RailEntry::Agent(agent("agent-opencode"));
    let claude = RailEntry::Agent(agent("agent-claude"));
    press_key(
        out,
        &mut surface,
        commands,
        "OpenCode mark",
        elements::mark_key(&opencode),
    );
    press_row(out, &mut surface, commands, "Beam Free");
    press_key(
        out,
        &mut surface,
        commands,
        "Claude mark",
        elements::mark_key(&claude),
    );
    press_row(out, &mut surface, commands, "Opus 4.7");
    for segment in ["effort-high", "effort-medium", "effort-off"] {
        press_key(
            out,
            &mut surface,
            commands,
            segment,
            koru_draw::el::Key::surface(elements::SURFACE, segment),
        );
    }
    press_key(out, &mut surface, commands, "Fast", elements::FAST_KEY);
    press_key(
        out,
        &mut surface,
        commands,
        "Fast again",
        elements::FAST_KEY,
    );
    press_key(
        out,
        &mut surface,
        commands,
        "1M context",
        elements::CONTEXT_KEY,
    );
    press_key(
        out,
        &mut surface,
        commands,
        "refresh",
        elements::REFRESH_KEY,
    );
    surface.agent_models_answered(&agent("agent-claude"));
    out.push_str(&format!("answered | {}\n", state_line(&surface)));
    let star = surface
        .picker_spec()
        .list
        .iter()
        .find(|row| row.label == "Sonnet 4.6")
        .and_then(|row| row.star.as_ref().map(|star| star.key));
    match star {
        Some(key) => press_key(out, &mut surface, commands, "Sonnet star", key),
        None => out.push_str("Sonnet star: not drawn\n"),
    }
    press_row(out, &mut surface, commands, "Legacy models (2)");
    press_row(out, &mut surface, commands, "Claude 1");
    if surface.picker_geometry().is_some() {
        let at = at_row(&surface, 0);
        press_at(out, &mut surface, commands, "first row", at);
    }
    press_key(out, &mut surface, commands, "+", elements::MANAGE_KEY);
    surface.open_model_picker();
    out.push_str(&format!(
        "reopen -> {} | {}\n",
        drain(commands),
        state_line(&surface)
    ));
    let geometry = surface.picker_geometry().unwrap();
    press_at(
        out,
        &mut surface,
        commands,
        "outside",
        (geometry.panel.x - 30.0, geometry.panel.y + 10.0),
    );
}

/// **The commands, in order** (§4.1's ordering): the same presses on a chat
/// not made yet, whose stand-in takes each choice; on a made chat, whose
/// service is sent each; and on a begun chat whose host takes a model and a
/// level per message, with a reply running, so each choice says it is the
/// next message's.
#[test]
fn every_choice_sends_what_it_sent_in_the_order_it_did() {
    let mut out = String::new();

    out.push_str("# a chat not made yet\n");
    let (surface, commands) = base();
    press_session(&mut out, surface, &commands);

    out.push_str("# a made chat\n");
    let (mut surface, commands) = base();
    surface.unmade_chat = None;
    press_session(&mut out, surface, &commands);

    out.push_str(
        "# a begun chat whose host takes a model and a level per message, a reply running\n",
    );
    let (mut surface, commands) = base();
    surface.unmade_chat = None;
    begin(&mut surface);
    surface.models_per_turn.insert(agent("agent-claude"));
    surface.levels_per_turn.insert(agent("agent-claude"));
    surface.attempt = Some(InferenceAttemptId::new());
    press_session(&mut out, surface, &commands);

    out.push_str("# a begun chat on an older host\n");
    let (mut surface, commands) = base();
    surface.unmade_chat = None;
    begin(&mut surface);
    press_session(&mut out, surface, &commands);

    let mut pins = Pins::default();
    pins.check("commands", &out, include_str!("pins/commands.txt"));
    pins.finish();
}

/// **The list settles against the chosen model's footer** (§4.1, R1): from
/// a model with a footer, scrolled to the list's foot, choosing one with none
/// gives the list the footer's room, so the list's furthest offset falls —
/// and the offset with it. Settling before the choice is applied would keep
/// the old model's offset, past the new foot.
#[test]
fn a_choice_settles_the_list_against_the_new_models_footer() {
    let (mut surface, commands) = base();
    if let Some(AgentModels::Listed(models)) = surface.agent_models.get_mut(&agent("agent-claude"))
    {
        models.extend((0..12).map(|n| {
            model(
                &format!("model-{n:02}"),
                "claude",
                Some(&format!("Model {n:02}")),
                None,
            )
        }));
    }
    surface.open_model_picker();
    surface.select_rail_entry(RailEntry::Agent(agent("agent-claude")));
    let with_footer = surface.picker_geometry().unwrap().visible_rows();
    surface.picker_scroll(-100.0);
    let rows = surface.picker_rows().len();
    assert_eq!(surface.picker.offset, rows - with_footer, "at the foot");
    // The last model; *Legacy models* is the row after it.
    let last = row_labelled(&surface, "Model 11");
    assert_eq!(last, rows - 2);
    let (x, y) = at_row(&surface, last);
    assert_eq!(surface.picker_mouse_down(x, y), Some(true));
    let chosen = &surface.transcript.as_ref().unwrap().chat.current_model;
    assert_eq!(chosen.requested_model, "model-11", "{}", drain(&commands));
    let without = surface.picker_geometry().unwrap().visible_rows();
    assert!(without > with_footer, "no footer: more rows");
    assert_eq!(
        surface.picker.offset,
        rows - without,
        "settled at the new foot"
    );
    assert_eq!(surface.picker.highlight, Some(last));
}

/// **The control's *Default* is the agent's** (§2.4): the control writes an
/// agent's own default as `picker::DEFAULT_MODEL`, and the chat sends it as
/// the AI service's `AGENT_DEFAULT_MODEL`. They are one id.
#[test]
fn the_controls_default_model_is_the_agents() {
    assert_eq!(cfg::DEFAULT_MODEL, AGENT_DEFAULT_MODEL);
}
