// SPDX-License-Identifier: MPL-2.0

use koru_draw::text::TextEngine;
use koru_session_protocol::{
    AttentionState, CellStyle, CellWidth, ColumnIndex, CursorShape, CursorState, GridSize,
    KeyModifiers, MouseButton, MouseEncoding, MouseTrackingMode, RawCommand, RenderDelta,
    RenderSnapshot, RenderUpdate, RowIndex, RowRange, ScrollbackPage, SessionId, StreamSequence,
    TerminalCell, TerminalGeneration, TerminalModes, TerminalRow, ViewportGeneration,
};
use koru_session_view::{
    CellPoint, CellSelection, ClipboardAction, KeyPhase, MAX_PASTE_BYTES, PointerModifiers,
    SessionSurfaceIntent, SessionSurfaceInteraction, TerminalIntent, TerminalKeyEvent,
    TerminalKeyInput, TerminalShortcut, TerminalView,
};
use vello::{
    Scene,
    kurbo::{Affine, Rect},
};

#[derive(Default)]
struct Recorder {
    intents: Vec<TerminalIntent>,
}

impl SessionSurfaceInteraction for Recorder {
    fn dispatch(&mut self, intent: SessionSurfaceIntent) {
        let SessionSurfaceIntent::Terminal(intent) = intent else {
            panic!("terminal view emitted a different surface intent");
        };
        self.intents.push(intent);
    }
}

fn row(index: u64, text: &str, wrapped_to_next: bool) -> TerminalRow {
    let mut cells = Vec::new();
    for (column, character) in text.chars().enumerate() {
        cells.push(TerminalCell {
            column: ColumnIndex(column as u16),
            text: character.to_string(),
            width: CellWidth::One,
            style: CellStyle::default(),
        });
    }
    TerminalRow {
        index: RowIndex(index),
        cells,
        wrapped_to_next,
    }
}

fn snapshot(modes: TerminalModes) -> RenderSnapshot {
    RenderSnapshot {
        session_id: SessionId::mint(),
        terminal_generation: TerminalGeneration::new(1).unwrap(),
        viewport_generation: ViewportGeneration::new(1).unwrap(),
        sequence: StreamSequence::new(1).unwrap(),
        viewport: RowRange {
            first: RowIndex(10),
            count: 5,
        },
        size: GridSize::new(6, 3).unwrap(),
        rows: vec![
            row(10, "hello ", true),
            row(11, "world ", false),
            row(12, "alpha ", false),
            row(13, "beta  ", false),
            row(14, "tail  ", false),
        ],
        cursor: CursorState {
            row: RowIndex(14),
            column: ColumnIndex(0),
            shape: CursorShape::Block,
            blinking: false,
        },
        modes,
        attention: AttentionState::default(),
    }
}

fn view(modes: TerminalModes) -> TerminalView<Recorder> {
    let mut view = TerminalView::new(Recorder::default());
    view.set_visible_rows(RowRange {
        first: RowIndex(12),
        count: 3,
    });
    view.apply(RenderUpdate::Snapshot {
        snapshot: snapshot(modes),
    })
    .unwrap();
    view
}

fn shortcut(shortcut: TerminalShortcut) -> TerminalKeyEvent {
    TerminalKeyEvent {
        input: TerminalKeyInput::Shortcut(shortcut),
        phase: KeyPhase::Pressed,
        repeat: false,
    }
}

#[test]
fn copy_interrupt_paste_and_ime_are_one_policy_boundary() {
    let mut view = view(TerminalModes::default());
    view.set_selection(Some(CellSelection {
        anchor: CellPoint {
            row: RowIndex(12),
            column: ColumnIndex(0),
        },
        focus: CellPoint {
            row: RowIndex(12),
            column: ColumnIndex(4),
        },
    }));
    let copied = view.handle_key(shortcut(TerminalShortcut::CopyOrInterrupt));
    assert_eq!(
        copied.clipboard,
        Some(ClipboardAction::Copy("alpha".into()))
    );
    assert!(view.interaction_mut().intents.is_empty());

    view.set_selection(None);
    view.handle_key(shortcut(TerminalShortcut::CopyOrInterrupt));
    assert!(matches!(
        view.interaction_mut().intents.last(),
        Some(TerminalIntent::Raw(RawCommand::SendKey { key }))
            if key.modifiers.control
    ));
    assert_eq!(
        view.handle_key(shortcut(TerminalShortcut::PasteAlternate))
            .clipboard,
        Some(ClipboardAction::Paste)
    );
    assert!(view.paste("one bulk paste").unwrap());
    assert!(matches!(
        view.interaction_mut().intents.last(),
        Some(TerminalIntent::Raw(RawCommand::PasteText { text })) if text == "one bulk paste"
    ));
    assert!(view.paste(&"x".repeat(MAX_PASTE_BYTES + 1)).is_err());

    assert!(view.ime_preedit("漢"));
    assert_eq!(view.preedit(), Some("漢"));
    assert!(view.ime_commit("漢字"));
    assert_eq!(view.preedit(), None);
    assert!(matches!(
        view.interaction_mut().intents.last(),
        Some(TerminalIntent::Raw(RawCommand::WriteText { text })) if text == "漢字"
    ));
}

#[test]
fn wrapped_wide_and_click_selection_follow_terminal_cells() {
    let mut view = view(TerminalModes::default());
    view.set_selection(Some(CellSelection {
        anchor: CellPoint {
            row: RowIndex(10),
            column: ColumnIndex(0),
        },
        focus: CellPoint {
            row: RowIndex(11),
            column: ColumnIndex(4),
        },
    }));
    assert_eq!(view.selected_text().as_deref(), Some("helloworld"));

    let word = view.pointer_press(
        CellPoint {
            row: RowIndex(12),
            column: ColumnIndex(2),
        },
        MouseButton::Left,
        PointerModifiers::default(),
        2,
    );
    assert!(word.redraw);
    assert_eq!(view.selected_text().as_deref(), Some("alpha"));
    view.pointer_press(
        CellPoint {
            row: RowIndex(13),
            column: ColumnIndex(2),
        },
        MouseButton::Left,
        PointerModifiers::default(),
        3,
    );
    assert_eq!(view.selected_text().as_deref(), Some("beta"));

    let identity = {
        let model = view.model().unwrap();
        (
            model.session_id(),
            model.terminal_generation(),
            model.viewport_generation(),
            model.sequence(),
        )
    };
    let mut wide = row(12, "      ", false);
    wide.cells[0].text = "界".into();
    wide.cells[0].width = CellWidth::Two;
    wide.cells[1].text.clear();
    wide.cells[1].width = CellWidth::Zero;
    view.apply(RenderUpdate::Delta {
        delta: RenderDelta {
            session_id: identity.0,
            terminal_generation: identity.1,
            viewport_generation: identity.2,
            sequence: identity.3.next().unwrap(),
            rows: vec![wide],
            cursor: None,
            modes: None,
            attention: None,
        },
    })
    .unwrap();
    view.pointer_press(
        CellPoint {
            row: RowIndex(12),
            column: ColumnIndex(1),
        },
        MouseButton::Left,
        PointerModifiers::default(),
        1,
    );
    assert_eq!(view.selected_text().as_deref(), Some("界"));
}

#[test]
fn mouse_reporting_preserves_shift_local_selection() {
    let modes = TerminalModes {
        mouse_reporting: true,
        mouse_tracking: MouseTrackingMode::Drag,
        mouse_encoding: MouseEncoding::Sgr,
        ..TerminalModes::default()
    };
    let mut view = view(modes);
    let point = CellPoint {
        row: RowIndex(12),
        column: ColumnIndex(1),
    };
    view.pointer_press(point, MouseButton::Left, PointerModifiers::default(), 1);
    assert!(matches!(
        view.interaction_mut().intents.last(),
        Some(TerminalIntent::Raw(RawCommand::SendMouse { .. }))
    ));
    assert!(view.selected_text().is_none());
    let shifted = PointerModifiers {
        shift: true,
        ..PointerModifiers::default()
    };
    assert!(view.pointer_drag(point, shifted).handled);
    assert!(
        view.pointer_release(point, MouseButton::Left, shifted)
            .handled
    );

    let before = view.interaction_mut().intents.len();
    let local = view.pointer_press(point, MouseButton::Left, shifted, 1);
    assert!(local.redraw);
    assert_eq!(view.interaction_mut().intents.len(), before);
    assert_eq!(view.selected_text().as_deref(), Some("l"));
}

#[test]
fn historical_output_does_not_relayout_and_bottom_recovers_once() {
    let mut view = view(TerminalModes::default());
    assert!(view.scroll_lines(5));
    let requested = RowRange {
        first: RowIndex(7),
        count: 3,
    };
    assert_eq!(
        view.interaction_mut().intents.last(),
        Some(&TerminalIntent::RequestScrollback { range: requested })
    );
    assert!(!view.scroll_lines(0));
    let model = view.model().unwrap();
    let page = ScrollbackPage {
        session_id: model.session_id(),
        terminal_generation: model.terminal_generation(),
        viewport_generation: model.viewport_generation(),
        sequence: model.sequence(),
        range: requested,
        rows: vec![
            row(7, "seven ", false),
            row(8, "eight ", false),
            row(9, "nine  ", false),
        ],
        has_earlier: true,
        has_later: true,
    };
    let identity = (
        model.session_id(),
        model.terminal_generation(),
        model.viewport_generation(),
        model.sequence(),
    );
    view.apply_scrollback_page(page).unwrap();

    let mut engine = TextEngine::new();
    let mut scene = Scene::new();
    let first = view.draw(
        &mut engine,
        &mut scene,
        Affine::IDENTITY,
        Rect::new(0.0, 0.0, 600.0, 60.0),
        1.0,
    );
    assert_eq!(first.rows_shaped, 3);
    view.apply(RenderUpdate::Delta {
        delta: RenderDelta {
            session_id: identity.0,
            terminal_generation: identity.1,
            viewport_generation: identity.2,
            sequence: identity.3.next().unwrap(),
            rows: vec![row(12, "news  ", false)],
            cursor: None,
            modes: None,
            attention: None,
        },
    })
    .unwrap();
    let mut scene = Scene::new();
    let second = view.draw(
        &mut engine,
        &mut scene,
        Affine::IDENTITY,
        Rect::new(0.0, 0.0, 600.0, 60.0),
        1.0,
    );
    assert_eq!(second.rows_shaped, 0);
    view.return_to_bottom();
    assert!(matches!(
        view.interaction_mut().intents.last(),
        Some(TerminalIntent::SetViewport { .. })
    ));
}

#[test]
fn scrolling_requests_only_the_missing_contiguous_history_span() {
    let mut view = view(TerminalModes::default());
    assert!(view.scroll_lines(3));
    assert_eq!(
        view.interaction_mut().intents.last(),
        Some(&TerminalIntent::RequestScrollback {
            range: RowRange {
                first: RowIndex(9),
                count: 1,
            },
        })
    );
    let count = view.interaction_mut().intents.len();
    assert!(!view.scroll_lines(0));
    assert_eq!(view.interaction_mut().intents.len(), count);
}

#[test]
fn resize_and_focus_are_coalesced_or_mode_gated() {
    let mut view = view(TerminalModes {
        focus_reporting: true,
        ..TerminalModes::default()
    });
    let size = GridSize::new(100, 40).unwrap();
    let rows = RowRange {
        first: RowIndex(20),
        count: 40,
    };
    assert!(view.resize_terminal(size, rows, 2).unwrap());
    assert!(!view.resize_terminal(size, rows, 2).unwrap());
    assert_eq!(
        view.interaction_mut().intents,
        vec![TerminalIntent::Raw(RawCommand::Resize { size })],
        "the Host's resize snapshot owns a tail-following viewport"
    );

    view.interaction_mut().intents.clear();
    assert!(view.scroll_lines(1));
    view.interaction_mut().intents.clear();
    let scrollback_size = GridSize::new(90, 30).unwrap();
    let scrollback_rows = RowRange {
        first: RowIndex(19),
        count: 30,
    };
    assert!(
        view.resize_terminal(scrollback_size, scrollback_rows, 2)
            .unwrap()
    );
    assert_eq!(
        view.interaction_mut().intents,
        vec![
            TerminalIntent::Raw(RawCommand::Resize {
                size: scrollback_size,
            }),
            TerminalIntent::SetViewport {
                rows: scrollback_rows,
                columns: scrollback_size.columns,
                overscan_rows: 2,
            },
        ],
        "an intentional scrollback anchor survives resize"
    );

    view.interaction_mut().intents.clear();
    assert!(view.focus_changed(false));
    assert!(matches!(
        view.interaction_mut().intents.last(),
        Some(TerminalIntent::Raw(RawCommand::SetFocus { focused: false }))
    ));

    let released = TerminalKeyEvent {
        input: TerminalKeyInput::Stroke(koru_session_protocol::KeyStroke {
            key: koru_session_protocol::TerminalKey::Tab,
            modifiers: KeyModifiers {
                control: true,
                ..KeyModifiers::default()
            },
        }),
        phase: KeyPhase::Released,
        repeat: false,
    };
    assert!(!view.handle_key(released).handled);
}

#[test]
fn authoritative_resize_snapshot_moves_the_live_tail_without_another_resize() {
    let mut view = view(TerminalModes::default());
    let model = view.model().unwrap();
    let session_id = model.session_id();
    let terminal_generation = model.terminal_generation();
    let sequence = model.sequence().next().unwrap();
    let size = GridSize::new(8, 5).unwrap();
    let viewport = RowRange {
        first: RowIndex(20),
        count: 7,
    };
    let expected_tail = RowRange {
        first: RowIndex(22),
        count: 5,
    };
    view.apply(RenderUpdate::Snapshot {
        snapshot: RenderSnapshot {
            session_id,
            terminal_generation,
            viewport_generation: ViewportGeneration::new(2).unwrap(),
            sequence,
            viewport,
            size,
            rows: (20..27).map(|index| row(index, "tail", false)).collect(),
            cursor: CursorState {
                row: RowIndex(26),
                column: ColumnIndex(0),
                shape: CursorShape::Block,
                blinking: false,
            },
            modes: TerminalModes::default(),
            attention: AttentionState::default(),
        },
    })
    .unwrap();

    assert_eq!(view.rows_for_resize(size.rows), Some(expected_tail));
    assert!(
        !view.resize_terminal(size, expected_tail, 2).unwrap(),
        "painting the acknowledged snapshot must not start a resize loop"
    );
    assert!(view.interaction_mut().intents.is_empty());
}

#[test]
fn ctrl_end_returns_scrollback_to_bottom_and_dispatches_the_key() {
    let mut view = view(TerminalModes::default());
    assert!(view.scroll_lines(2));
    view.interaction_mut().intents.clear();

    let outcome = view.handle_key(TerminalKeyEvent {
        input: TerminalKeyInput::Stroke(koru_session_protocol::KeyStroke {
            key: koru_session_protocol::TerminalKey::End,
            modifiers: KeyModifiers {
                control: true,
                primary: true,
                ..KeyModifiers::default()
            },
        }),
        phase: KeyPhase::Pressed,
        repeat: false,
    });
    assert!(outcome.handled);
    assert!(
        outcome.redraw,
        "returning to live tail redraws the viewport"
    );
    assert!(matches!(
        view.interaction_mut().intents.as_slice(),
        [
            TerminalIntent::SetViewport { .. },
            TerminalIntent::Raw(RawCommand::SendKey { key }),
        ] if key.key == koru_session_protocol::TerminalKey::End && key.modifiers.control
    ));
}

#[test]
fn shift_page_up_page_down_home_and_end_scroll_history() {
    let mut view = view(TerminalModes::default());
    let stroke = |key: koru_session_protocol::TerminalKey| TerminalKeyEvent {
        input: TerminalKeyInput::Stroke(koru_session_protocol::KeyStroke {
            key,
            modifiers: KeyModifiers {
                shift: true,
                ..KeyModifiers::default()
            },
        }),
        phase: KeyPhase::Pressed,
        repeat: false,
    };

    // Shift+PageUp scrolls up by page (3 lines).
    let up = view.handle_key(stroke(koru_session_protocol::TerminalKey::PageUp));
    assert!(up.handled);
    assert!(up.redraw);
    assert!(matches!(
        view.interaction_mut().intents.last(),
        Some(TerminalIntent::RequestScrollback { .. })
    ));

    // Shift+Home jumps to oldest available history.
    let home = view.handle_key(stroke(koru_session_protocol::TerminalKey::Home));
    assert!(home.handled);

    // Shift+PageDown scrolls back down.
    let down = view.handle_key(stroke(koru_session_protocol::TerminalKey::PageDown));
    assert!(down.handled);

    // Shift+End returns to live tail.
    let end = view.handle_key(stroke(koru_session_protocol::TerminalKey::End));
    assert!(end.handled);
    assert!(end.redraw);
    assert!(matches!(
        view.interaction_mut().intents.last(),
        Some(TerminalIntent::SetViewport { .. })
    ));
}
