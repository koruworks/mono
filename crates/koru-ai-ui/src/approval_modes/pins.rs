// SPDX-License-Identifier: MPL-2.0

//! **The approval-mode list, pinned before it moves** (Bots P1.8 picker plan
//! §4.4, S0): for each state below, every inspector record but its `source`,
//! the list's semantics and a grid of what each point is — the row a press
//! or the pointer finds, the hover part, and the footer's *Change default*
//! — and, over scripted sessions, every key's effect and every command in
//! order. See `crate::pins` for how a pin reports a change.

use std::sync::mpsc::Receiver;

use super::*;
use crate::ChatSurface;
use crate::pins::{Grid, Pins, drain, records, rect, semantics};
use crate::reader::Reader;
use koru_ai_core::{ChatId, InferenceAttemptId, ModelSelection, ReasoningChoice};
use koru_i18n::Lang;

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

/// Claude Code's five and one Koru has no words for.
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

fn codex_modes() -> ListedModes {
    ListedModes {
        modes: vec![
            mode("ask", None, None),
            mode("auto-review", None, None),
            mode("full-access", None, None),
        ],
        default: Some("ask".into()),
    }
}

/// A new Claude chat, not made yet, whose listing carries its modes, with a
/// reader whose commands the pin reads.
fn base() -> (ChatSurface, Receiver<Command>) {
    let mut surface = ChatSurface::new();
    surface.lang = Lang::En;
    surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
    let (reader, commands, _updates) = Reader::test_pair();
    surface.reader = Some(reader);
    surface
        .agent_modes
        .insert(agent("agent-claude"), claude_modes());
    surface
        .agent_modes
        .insert(agent("agent-codex"), codex_modes());
    on_agent(&mut surface, "agent-claude");
    surface.unmade_chat = surface.transcript.as_ref().map(|t| t.chat.id);
    surface.composer_rect();
    (surface, commands)
}

fn on_agent(surface: &mut ChatSurface, id: &str) {
    let mut transcript = crate::unmade_transcript(ModelSelection {
        provider: agent(id),
        requested_model: koru_ai_core::AGENT_DEFAULT_MODEL.into(),
        default_reasoning: ReasoningChoice::Default,
    });
    transcript.chat.id = ChatId::new();
    surface.transcript = Some(transcript);
}

fn centre(rect: Rect) -> (f32, f32) {
    (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
}

// --- The states ------------------------------------------------------------

/// Open, the pointer on *Ask first* and the keyboard on *Plan*.
fn open() -> ChatSurface {
    let (mut surface, _) = base();
    surface.approval_mode_open = true;
    surface.approval_mode_focus = Some(2);
    let row = surface.approval_mode_row_rect(0).expect("the first row");
    surface.pointer = (row.x + 20.0, row.y + row.h * 0.5);
    surface
}

/// The keyboard on the footer's *Change default*, the pointer off the list.
fn footer_focused() -> ChatSurface {
    let (mut surface, _) = base();
    surface.approval_mode_open = true;
    surface.approval_mode_focus = Some(surface.approval_mode_rows().len());
    surface.pointer = (0.0, 0.0);
    surface
}

/// A Codex chat in *Full access*, the pointer on the footer's button.
fn codex_full() -> ChatSurface {
    let (mut surface, _) = base();
    on_agent(&mut surface, "agent-codex");
    surface.transcript.as_mut().unwrap().chat.agent_mode = Some("full-access".into());
    surface.approval_mode_open = true;
    let button = surface.approval_mode_default_rect().expect("the footer");
    surface.pointer = centre(button);
    surface
}

/// What a press at `x`, `y` reads first, in the order
/// [`ChatSurface::approval_mode_mouse_down`] asks.
fn part_at(surface: &ChatSurface, x: f32, y: f32) -> String {
    if surface
        .approval_mode_default_rect()
        .is_some_and(|button| button.contains(x, y))
    {
        return "change-default".into();
    }
    if let Some(index) = surface.approval_mode_row_at(x, y) {
        return format!("row {index}");
    }
    if surface
        .approval_mode_button()
        .is_some_and(|chip| chip.contains(x, y))
    {
        return "chip".into();
    }
    if surface
        .approval_mode_panel()
        .is_some_and(|panel| panel.contains(x, y))
    {
        "panel".into()
    } else {
        "outside".into()
    }
}

fn pin_state(pins: &mut Pins, name: &str, mut surface: ChatSurface, golden: [&str; 3]) {
    use koru_draw::inspect;
    let panel = surface.approval_mode_panel().expect("open");
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0),
    );
    let mut scene = Scene::new();
    surface.draw_approval_mode_picker(&mut scene, Affine::IDENTITY);
    let snapshot = capture.finish();
    let line = format!(
        "panel {} footer {:?} default {:?} chip {:?}\n",
        rect(panel),
        surface.approval_mode_footer_rect().map(rect),
        surface.approval_mode_default_rect().map(rect),
        surface.approval_mode_button().map(rect),
    );
    pins.check(
        &format!("{name}.records"),
        &(line + &records(&snapshot)),
        golden[0],
    );
    let (specs, frame) = surface.approval_mode_frame().expect("open");
    let rows: String = specs
        .iter()
        .map(|spec| {
            format!(
                "row {:?} icon {:#x} title {:?} help {:?} full={} current={} lit={}\n",
                spec.key,
                spec.icon as u32,
                spec.title,
                spec.help,
                spec.full_access,
                spec.current,
                spec.lit
            )
        })
        .collect();
    pins.check(
        &format!("{name}.semantics"),
        &format!(
            "diagnostics {:?}\n{rows}{}",
            frame.diagnostics,
            semantics(&frame.semantics())
        ),
        golden[1],
    );
    let grid = Grid::over(panel, 12.0, 6.0, |x, y| {
        format!(
            "{} | hit {:?} | hover {:?}",
            part_at(&surface, x, y),
            frame.hit_at(x, y),
            surface.approval_mode_hover_at(x, y),
        )
    });
    pins.check(&format!("{name}.grid"), &grid, golden[2]);
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

/// **What each state draws, declares and hits**, as it did before the move.
#[test]
fn every_state_draws_declares_and_hits_as_before() {
    let mut pins = Pins::default();
    pin_state(&mut pins, "open", open(), golden!("open"));
    pin_state(
        &mut pins,
        "footer-focused",
        footer_focused(),
        golden!("footer-focused"),
    );
    pin_state(&mut pins, "codex-full", codex_full(), golden!("codex-full"));
    pins.finish();
}

// --- The transcripts -------------------------------------------------------

fn state_line(surface: &mut ChatSurface) -> String {
    let chat = surface.transcript.as_ref().map(|transcript| {
        (
            transcript.chat.agent_mode.clone(),
            transcript.chat.current_model.provider.as_str().to_owned(),
        )
    });
    let settings = surface.take_agent_settings_request();
    format!(
        "open={} row={:?} focus={:?} model_open={} | chat={:?} current={:?} note={:?} settings={}",
        surface.approval_mode_open,
        surface.approval_mode_focus,
        surface.focus,
        surface.chat_model_open,
        chat,
        surface.current_approval_mode(),
        surface.copy_feedback.label(),
        settings,
    )
}

/// One key through the whole surface, as the window hands it over.
fn key(
    out: &mut String,
    surface: &mut ChatSurface,
    commands: &Receiver<Command>,
    key: Key,
    mods: ModifiersState,
) {
    let handled = surface.key(&key, None, mods);
    let name = match &key {
        Key::Named(named) => format!("{named:?}"),
        other => format!("{other:?}"),
    };
    out.push_str(&format!(
        "{}{name} -> {handled} {} | {}\n",
        if mods.shift_key() { "Shift+" } else { "" },
        drain(commands),
        state_line(surface)
    ));
}

fn keyboard_session(out: &mut String, mut surface: ChatSurface, commands: &Receiver<Command>) {
    let plain = ModifiersState::empty();
    for named in [
        NamedKey::Tab,
        NamedKey::Tab,
        NamedKey::Enter,
        NamedKey::ArrowDown,
        NamedKey::ArrowDown,
        NamedKey::ArrowDown,
        NamedKey::ArrowUp,
        NamedKey::Tab,
        NamedKey::ArrowDown,
        NamedKey::ArrowDown,
        NamedKey::Enter,
        NamedKey::Space,
        NamedKey::ArrowUp,
        NamedKey::Tab,
        NamedKey::Tab,
    ] {
        key(out, &mut surface, commands, Key::Named(named), plain);
    }
    for _ in 0..7 {
        key(
            out,
            &mut surface,
            commands,
            Key::Named(NamedKey::Tab),
            ModifiersState::SHIFT,
        );
    }
    for named in [
        NamedKey::Tab,
        NamedKey::Tab,
        NamedKey::Space,
        NamedKey::ArrowUp,
        NamedKey::Enter,
        NamedKey::Tab,
        NamedKey::Tab,
        NamedKey::Enter,
        NamedKey::Escape,
        NamedKey::Enter,
        NamedKey::Tab,
        NamedKey::Enter,
    ] {
        key(out, &mut surface, commands, Key::Named(named), plain);
    }
}

fn press(
    out: &mut String,
    surface: &mut ChatSurface,
    commands: &Receiver<Command>,
    what: &str,
    (x, y): (f32, f32),
) {
    let handled = surface.approval_mode_mouse_down(x, y);
    out.push_str(&format!(
        "{what} @{} -> {handled:?} {} | {}\n",
        rect(Rect::new(x, y, 0.0, 0.0)),
        drain(commands),
        state_line(surface)
    ));
}

fn press_session(out: &mut String, mut surface: ChatSurface, commands: &Receiver<Command>) {
    let chip = centre(surface.approval_mode_button().expect("the chip"));
    press(out, &mut surface, commands, "chip", chip);
    for index in [1, 4, 0] {
        if surface.approval_mode_panel().is_none() {
            press(out, &mut surface, commands, "chip", chip);
        }
        let row = surface.approval_mode_row_rect(index).expect("a row");
        press(
            out,
            &mut surface,
            commands,
            &format!("row {index}"),
            (row.x + 20.0, row.y + 10.0),
        );
    }
    press(out, &mut surface, commands, "chip", chip);
    press(out, &mut surface, commands, "chip again", chip);
    press(out, &mut surface, commands, "chip", chip);
    let panel = surface.approval_mode_panel().expect("open");
    press(
        out,
        &mut surface,
        commands,
        "outside",
        (panel.x - 40.0, panel.y + 10.0),
    );
    press(out, &mut surface, commands, "chip", chip);
    let button = surface.approval_mode_default_rect().expect("the footer");
    press(
        out,
        &mut surface,
        commands,
        "change default",
        centre(button),
    );
}

/// **The mode list's keys and presses, and what they send**: through the
/// whole surface (Tab to the chevron, Enter, arrows, the footer, Esc, and
/// Shift+Tab's cycle) and by the pointer, on a chat not made yet, a made
/// one, and a made one with a reply running.
#[test]
fn every_key_and_press_does_what_it_did() {
    let mut out = String::new();
    out.push_str("# keys, a chat not made yet\n");
    let (surface, commands) = base();
    keyboard_session(&mut out, surface, &commands);
    out.push_str("# keys, a made chat\n");
    let (mut surface, commands) = base();
    surface.unmade_chat = None;
    keyboard_session(&mut out, surface, &commands);
    out.push_str("# presses, a chat not made yet\n");
    let (surface, commands) = base();
    press_session(&mut out, surface, &commands);
    out.push_str("# presses, a made chat\n");
    let (mut surface, commands) = base();
    surface.unmade_chat = None;
    press_session(&mut out, surface, &commands);
    // While a reply runs the split button is Stop and has no menu: the
    // mode moves by Shift+Tab, and the note says it is the next message's.
    out.push_str("# Shift+Tab, a made chat with a reply running\n");
    let (mut surface, commands) = base();
    surface.unmade_chat = None;
    surface.attempt = Some(InferenceAttemptId::new());
    out.push_str(&format!(
        "chip {:?}\n",
        surface.approval_mode_button().map(rect)
    ));
    for _ in 0..3 {
        key(
            &mut out,
            &mut surface,
            &commands,
            Key::Named(NamedKey::Tab),
            ModifiersState::SHIFT,
        );
        surface.copy_feedback.clear();
    }
    let mut pins = Pins::default();
    pins.check("transcript", &out, include_str!("pins/transcript.txt"));
    pins.finish();
}
