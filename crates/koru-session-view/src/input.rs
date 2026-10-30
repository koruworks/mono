// SPDX-License-Identifier: MPL-2.0

use koru_session_protocol::{
    ErrorCode, GridSize, KeyModifiers, KeyStroke, MouseAction, MouseButton, RawCommand, RowIndex,
    RowRange, TerminalKey, TerminalMouseEvent,
};

use crate::selection::{line_selection, normalize_point, semantic_selection};
use crate::{
    CellPoint, CellSelection, SessionSurfaceIntent, SessionSurfaceInteraction, TerminalIntent,
    TerminalView,
};

pub const MAX_PASTE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyPhase {
    Pressed,
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalShortcut {
    CopyOrInterrupt,
    CopySelection,
    Paste,
    PasteAlternate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalKeyInput {
    Stroke(KeyStroke),
    Shortcut(TerminalShortcut),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalKeyEvent {
    pub input: TerminalKeyInput,
    pub phase: KeyPhase,
    pub repeat: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardAction {
    Copy(String),
    Paste,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputOutcome {
    pub handled: bool,
    pub redraw: bool,
    pub clipboard: Option<ClipboardAction>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PointerModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub primary: bool,
}

impl From<PointerModifiers> for KeyModifiers {
    fn from(value: PointerModifiers) -> Self {
        Self {
            shift: value.shift,
            control: value.control,
            alt: value.alt,
            primary: value.primary,
        }
    }
}

impl<I: SessionSurfaceInteraction> TerminalView<I> {
    pub fn handle_key(&mut self, event: TerminalKeyEvent) -> InputOutcome {
        if event.phase == KeyPhase::Released {
            return InputOutcome::default();
        }
        match event.input {
            TerminalKeyInput::Stroke(key) => {
                if let Some(redraw) = self.handle_shift_scrollback(&key) {
                    return InputOutcome {
                        handled: true,
                        redraw,
                        ..InputOutcome::default()
                    };
                }
                let returned = !self.follow_tail
                    && key.key == TerminalKey::End
                    && (key.modifiers.control || key.modifiers.primary);
                if returned {
                    self.return_to_bottom();
                }
                self.dispatch_raw(RawCommand::SendKey { key });
                InputOutcome {
                    handled: true,
                    redraw: returned,
                    ..InputOutcome::default()
                }
            }
            TerminalKeyInput::Shortcut(TerminalShortcut::CopyOrInterrupt) => {
                if let Some(text) = self.selected_text().filter(|text| !text.is_empty()) {
                    InputOutcome {
                        handled: true,
                        clipboard: Some(ClipboardAction::Copy(text)),
                        ..InputOutcome::default()
                    }
                } else {
                    self.dispatch_raw(RawCommand::SendKey {
                        key: KeyStroke {
                            key: TerminalKey::Character { text: "c".into() },
                            modifiers: KeyModifiers {
                                control: true,
                                ..KeyModifiers::default()
                            },
                        },
                    });
                    InputOutcome {
                        handled: true,
                        ..InputOutcome::default()
                    }
                }
            }
            TerminalKeyInput::Shortcut(TerminalShortcut::CopySelection) => InputOutcome {
                handled: true,
                clipboard: self
                    .selected_text()
                    .filter(|text| !text.is_empty())
                    .map(ClipboardAction::Copy),
                ..InputOutcome::default()
            },
            TerminalKeyInput::Shortcut(
                TerminalShortcut::Paste | TerminalShortcut::PasteAlternate,
            ) => InputOutcome {
                handled: true,
                clipboard: Some(ClipboardAction::Paste),
                ..InputOutcome::default()
            },
        }
    }

    /// Submit clipboard text through one bounded session command. The runtime
    /// decides from its live emulator state whether bracket delimiters apply.
    pub fn paste(&mut self, text: &str) -> Result<bool, ErrorCode> {
        if text.is_empty() {
            return Ok(false);
        }
        if text.len() > MAX_PASTE_BYTES {
            return Err(ErrorCode::InvalidRequest);
        }
        self.dispatch_raw(RawCommand::PasteText {
            text: text.to_owned(),
        });
        Ok(true)
    }

    pub fn ime_preedit(&mut self, text: &str) -> bool {
        let next = (!text.is_empty()).then(|| text.to_owned());
        if self.preedit == next {
            return false;
        }
        self.preedit = next;
        true
    }

    pub fn ime_commit(&mut self, text: &str) -> bool {
        self.preedit = None;
        if text.is_empty() {
            return false;
        }
        self.dispatch_raw(RawCommand::WriteText {
            text: text.to_owned(),
        });
        true
    }

    pub fn preedit(&self) -> Option<&str> {
        self.preedit.as_deref()
    }

    pub fn focus_changed(&mut self, focused: bool) -> bool {
        let reports_focus = self
            .model
            .as_ref()
            .is_some_and(|model| model.modes.focus_reporting);
        if reports_focus {
            self.dispatch_raw(RawCommand::SetFocus { focused });
        }
        reports_focus
    }

    pub fn resize_terminal(
        &mut self,
        size: GridSize,
        visible_rows: RowRange,
        overscan_rows: u16,
    ) -> Result<bool, ErrorCode> {
        size.validate()?;
        visible_rows.validate()?;
        let state = (size, visible_rows, overscan_rows);
        if self.last_resize == Some(state) {
            return Ok(false);
        }
        self.last_resize = Some(state);
        self.overscan_rows = overscan_rows;
        self.set_visible_rows(visible_rows);
        self.dispatch_raw(RawCommand::Resize { size });
        // A tail-following resize is atomic in the Session Host: after the PTY
        // and emulator resize, the existing render subscription is moved to
        // the new visible range and receives an authoritative snapshot. Do not
        // race that operation with row coordinates captured before ConPTY has
        // repainted. Doing so turns the live subscription into a fixed
        // scrollback viewport, hiding all subsequent output until reattach.
        //
        // A user who is deliberately reading scrollback does need to preserve
        // that explicit anchor after the resize has settled. The backend
        // coalesces this request behind the resize acknowledgement.
        if !self.follow_tail {
            self.interaction.dispatch(SessionSurfaceIntent::Terminal(
                TerminalIntent::SetViewport {
                    rows: visible_rows,
                    columns: size.columns,
                    overscan_rows,
                },
            ));
        }
        Ok(true)
    }

    pub fn pointer_press(
        &mut self,
        point: CellPoint,
        button: MouseButton,
        modifiers: PointerModifiers,
        click_count: u8,
    ) -> InputOutcome {
        if self.should_report_mouse(modifiers) {
            self.pressed_button = Some(button);
            self.report_mouse(MouseAction::Press, Some(button), point, modifiers);
            return InputOutcome {
                handled: true,
                ..InputOutcome::default()
            };
        }
        if button != MouseButton::Left {
            return InputOutcome::default();
        }
        let Some(row) = self.source_row(point.row) else {
            return InputOutcome::default();
        };
        let selection = match click_count.max(1) {
            1 => {
                let point = normalize_point(row, point);
                CellSelection {
                    anchor: point,
                    focus: point,
                }
            }
            2 => semantic_selection(row, point),
            _ => line_selection(
                point.row,
                self.model.as_ref().map_or(0, |model| model.size.columns),
            ),
        };
        self.selection = Some(selection);
        self.selecting = true;
        InputOutcome {
            handled: true,
            redraw: true,
            ..InputOutcome::default()
        }
    }

    pub fn pointer_drag(&mut self, point: CellPoint, modifiers: PointerModifiers) -> InputOutcome {
        if self.pressed_button.is_some() {
            self.report_mouse(MouseAction::Move, self.pressed_button, point, modifiers);
            return InputOutcome {
                handled: true,
                ..InputOutcome::default()
            };
        }
        if !self.selecting {
            return InputOutcome::default();
        }
        let Some(row) = self.source_row(point.row) else {
            return InputOutcome::default();
        };
        let point = normalize_point(row, point);
        let changed = self.selection.as_mut().is_some_and(|selection| {
            let changed = selection.focus != point;
            selection.focus = point;
            changed
        });
        InputOutcome {
            handled: true,
            redraw: changed,
            ..InputOutcome::default()
        }
    }

    pub fn pointer_release(
        &mut self,
        point: CellPoint,
        button: MouseButton,
        modifiers: PointerModifiers,
    ) -> InputOutcome {
        let reporting = self.pressed_button == Some(button);
        if reporting {
            self.report_mouse(MouseAction::Release, Some(button), point, modifiers);
        }
        self.pressed_button = None;
        self.selecting = false;
        InputOutcome {
            handled: reporting,
            ..InputOutcome::default()
        }
    }

    pub fn wheel(
        &mut self,
        point: CellPoint,
        lines: i32,
        modifiers: PointerModifiers,
    ) -> InputOutcome {
        if lines == 0 {
            return InputOutcome::default();
        }
        if self.should_report_mouse(modifiers) {
            let button = if lines > 0 {
                MouseButton::WheelUp
            } else {
                MouseButton::WheelDown
            };
            for _ in 0..lines.unsigned_abs().min(32) {
                self.report_mouse(MouseAction::Press, Some(button), point, modifiers);
            }
            return InputOutcome {
                handled: true,
                ..InputOutcome::default()
            };
        }
        let changed = self.scroll_lines(lines);
        InputOutcome {
            handled: true,
            redraw: changed,
            ..InputOutcome::default()
        }
    }

    fn should_report_mouse(&self, modifiers: PointerModifiers) -> bool {
        !modifiers.shift
            && self
                .model
                .as_ref()
                .is_some_and(|model| model.modes.mouse_reporting)
    }

    fn page_scroll_lines(&self) -> i32 {
        let count = self
            .visible_rows
            .or(self.tail_rows)
            .or_else(|| self.model.as_ref().map(|model| model.viewport))
            .map_or(1, |rows| rows.count.max(1));
        i32::from(count)
    }

    fn handle_shift_scrollback(&mut self, key: &KeyStroke) -> Option<bool> {
        if !key.modifiers.shift
            || key.modifiers.control
            || key.modifiers.alt
            || key.modifiers.primary
            || (self.follow_tail
                && self
                    .model
                    .as_ref()
                    .is_some_and(|model| model.modes.alternate_screen))
        {
            return None;
        }
        let page = self.page_scroll_lines();
        match key.key {
            TerminalKey::PageUp => Some(self.scroll_lines(page)),
            TerminalKey::PageDown => Some(self.scroll_lines(-page)),
            TerminalKey::Home => Some(self.scroll_lines(i32::MAX)),
            TerminalKey::End => {
                let changed = !self.follow_tail;
                if changed {
                    self.return_to_bottom();
                }
                Some(changed)
            }
            _ => None,
        }
    }

    fn report_mouse(
        &mut self,
        action: MouseAction,
        button: Option<MouseButton>,
        point: CellPoint,
        modifiers: PointerModifiers,
    ) {
        let first = self
            .visible_rows
            .or(self.tail_rows)
            .map_or(point.row.0, |rows| rows.first.0);
        let row = point.row.0.saturating_sub(first).min(u64::from(u16::MAX)) as u16;
        self.dispatch_raw(RawCommand::SendMouse {
            event: TerminalMouseEvent {
                action,
                button,
                column: point.column.0,
                row,
                modifiers: modifiers.into(),
            },
        });
    }

    pub(crate) fn dispatch_raw(&mut self, command: RawCommand) {
        self.interaction
            .dispatch(SessionSurfaceIntent::Terminal(TerminalIntent::Raw(command)));
    }

    pub(crate) fn source_row(
        &self,
        index: RowIndex,
    ) -> Option<&koru_session_protocol::TerminalRow> {
        let model = self.model.as_ref()?;
        self.history
            .row(model.terminal_generation, index)
            .or_else(|| model.rows.get(&index))
    }
}
