// SPDX-License-Identifier: MPL-2.0

use koru_session_protocol::{
    ErrorCode, KeyModifiers, KeyStroke, MouseAction, MouseButton, MouseEncoding, MouseTrackingMode,
    TerminalKey, TerminalModes, TerminalMouseEvent,
};

/// Encode one protocol key against the emulator's live mode state.
pub fn encode_key(key: &KeyStroke, modes: TerminalModes) -> Result<Vec<u8>, ErrorCode> {
    let modifier = xterm_modifier(key.modifiers);
    let mut bytes = match &key.key {
        TerminalKey::Character { text } => encode_character(text, key.modifiers)?,
        TerminalKey::Enter => vec![b'\r'],
        TerminalKey::Tab if key.modifiers.shift => b"\x1b[Z".to_vec(),
        TerminalKey::Tab => vec![b'\t'],
        TerminalKey::Backspace => vec![0x7f],
        TerminalKey::Escape => vec![0x1b],
        TerminalKey::ArrowUp => cursor_key(b'A', modes.application_cursor, modifier),
        TerminalKey::ArrowDown => cursor_key(b'B', modes.application_cursor, modifier),
        TerminalKey::ArrowRight => cursor_key(b'C', modes.application_cursor, modifier),
        TerminalKey::ArrowLeft => cursor_key(b'D', modes.application_cursor, modifier),
        TerminalKey::Home => cursor_key(b'H', modes.application_cursor, modifier),
        TerminalKey::End => cursor_key(b'F', modes.application_cursor, modifier),
        TerminalKey::PageUp => csi_tilde(5, modifier),
        TerminalKey::PageDown => csi_tilde(6, modifier),
        TerminalKey::Insert => csi_tilde(2, modifier),
        TerminalKey::Delete => csi_tilde(3, modifier),
        TerminalKey::Function { number } => function(*number, modifier)?,
    };
    if key.modifiers.alt && !bytes.starts_with(&[0x1b]) {
        bytes.insert(0, 0x1b);
    }
    Ok(bytes)
}

/// Encode a whole paste as one write, adding delimiters only for the live mode.
pub fn encode_paste(text: &str, modes: TerminalModes) -> Vec<u8> {
    if !modes.bracketed_paste {
        return text.as_bytes().to_vec();
    }
    let mut bytes = Vec::with_capacity(text.len() + 12);
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}

pub fn encode_focus(focused: bool, modes: TerminalModes) -> Option<&'static [u8]> {
    modes.focus_reporting.then_some(if focused {
        b"\x1b[I".as_slice()
    } else {
        b"\x1b[O".as_slice()
    })
}

/// Encode a mouse report only when the terminal negotiated a mode that wants it.
/// Coordinates too large for a legacy encoding are dropped instead of wrapped.
pub fn encode_mouse(
    event: TerminalMouseEvent,
    modes: TerminalModes,
) -> Result<Option<Vec<u8>>, ErrorCode> {
    event.validate()?;
    let tracking = if modes.mouse_tracking == MouseTrackingMode::Off && modes.mouse_reporting {
        MouseTrackingMode::Click
    } else {
        modes.mouse_tracking
    };
    if !tracks(event, tracking) {
        return Ok(None);
    }

    let button = button_code(event, modes.mouse_encoding);
    let bytes = match modes.mouse_encoding {
        MouseEncoding::Sgr => {
            let final_byte = if event.action == MouseAction::Release {
                'm'
            } else {
                'M'
            };
            format!(
                "\x1b[<{button};{};{}{final_byte}",
                u32::from(event.column) + 1,
                u32::from(event.row) + 1
            )
            .into_bytes()
        }
        MouseEncoding::Legacy => {
            let Some(x) = event
                .column
                .checked_add(33)
                .and_then(|value| u8::try_from(value).ok())
            else {
                return Ok(None);
            };
            let Some(y) = event
                .row
                .checked_add(33)
                .and_then(|value| u8::try_from(value).ok())
            else {
                return Ok(None);
            };
            let code = u8::try_from(button + 32).map_err(|_| ErrorCode::InvalidRequest)?;
            vec![0x1b, b'[', b'M', code, x, y]
        }
        MouseEncoding::Utf8 => {
            let mut bytes = b"\x1b[M".to_vec();
            push_utf8_codepoint(&mut bytes, button + 32)?;
            push_utf8_codepoint(&mut bytes, u32::from(event.column) + 33)?;
            push_utf8_codepoint(&mut bytes, u32::from(event.row) + 33)?;
            bytes
        }
    };
    Ok(Some(bytes))
}

fn tracks(event: TerminalMouseEvent, mode: MouseTrackingMode) -> bool {
    match (mode, event.action) {
        (MouseTrackingMode::Off, _) => false,
        (_, MouseAction::Press | MouseAction::Release) => true,
        (MouseTrackingMode::Drag, MouseAction::Move) => event.button.is_some(),
        (MouseTrackingMode::Motion, MouseAction::Move) => true,
        (MouseTrackingMode::Click, MouseAction::Move) => false,
    }
}

fn button_code(event: TerminalMouseEvent, encoding: MouseEncoding) -> u32 {
    let base = match (event.action, event.button) {
        (MouseAction::Release, _) if encoding != MouseEncoding::Sgr => 3,
        (_, Some(MouseButton::Left)) => 0,
        (_, Some(MouseButton::Middle)) => 1,
        (_, Some(MouseButton::Right)) => 2,
        (_, Some(MouseButton::WheelUp)) => 64,
        (_, Some(MouseButton::WheelDown)) => 65,
        (MouseAction::Move, None) => 3,
        (_, None) => 3,
    };
    base + u32::from(event.modifiers.shift) * 4
        + u32::from(event.modifiers.alt) * 8
        + u32::from(event.modifiers.control || event.modifiers.primary) * 16
        + u32::from(event.action == MouseAction::Move) * 32
}

fn push_utf8_codepoint(bytes: &mut Vec<u8>, value: u32) -> Result<(), ErrorCode> {
    let character = char::from_u32(value).ok_or(ErrorCode::InvalidRequest)?;
    let mut encoded = [0; 4];
    bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
    Ok(())
}

fn encode_character(text: &str, modifiers: KeyModifiers) -> Result<Vec<u8>, ErrorCode> {
    if text.is_empty() {
        return Err(ErrorCode::InvalidRequest);
    }
    if modifiers.control || modifiers.primary {
        let mut chars = text.chars();
        let value = chars.next().ok_or(ErrorCode::InvalidRequest)?;
        if chars.next().is_none() && value.is_ascii() {
            let byte = value as u8;
            let control = match byte {
                b' ' | b'@'..=b'_' => byte & 0x1f,
                b'a'..=b'z' => (byte - b'a') + 1,
                b'?' => 0x7f,
                _ => return Ok(text.as_bytes().to_vec()),
            };
            return Ok(vec![control]);
        }
    }
    Ok(text.as_bytes().to_vec())
}

fn xterm_modifier(modifiers: KeyModifiers) -> u8 {
    1 + u8::from(modifiers.shift)
        + 2 * u8::from(modifiers.alt)
        + 4 * u8::from(modifiers.control || modifiers.primary)
}

fn cursor_key(final_byte: u8, application: bool, modifier: u8) -> Vec<u8> {
    if modifier == 1 {
        return vec![0x1b, if application { b'O' } else { b'[' }, final_byte];
    }
    format!("\x1b[1;{modifier}{}", final_byte as char).into_bytes()
}

fn csi_tilde(number: u8, modifier: u8) -> Vec<u8> {
    if modifier == 1 {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{modifier}~").into_bytes()
    }
}

fn function(number: u8, modifier: u8) -> Result<Vec<u8>, ErrorCode> {
    let bytes = match number {
        1..=4 if modifier == 1 => vec![0x1b, b'O', b'P' + number - 1],
        1..=4 => format!("\x1b[1;{modifier}{}", (b'P' + number - 1) as char).into_bytes(),
        5 => csi_tilde(15, modifier),
        6 => csi_tilde(17, modifier),
        7 => csi_tilde(18, modifier),
        8 => csi_tilde(19, modifier),
        9 => csi_tilde(20, modifier),
        10 => csi_tilde(21, modifier),
        11 => csi_tilde(23, modifier),
        12 => csi_tilde(24, modifier),
        13..=24 => csi_tilde(number + 12, modifier),
        _ => return Err(ErrorCode::InvalidRequest),
    };
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse(action: MouseAction, button: Option<MouseButton>) -> TerminalMouseEvent {
        TerminalMouseEvent {
            action,
            button,
            column: 4,
            row: 2,
            modifiers: KeyModifiers::default(),
        }
    }

    #[test]
    fn application_cursor_control_keys_and_bracketed_paste_are_mode_aware() {
        let mut modes = TerminalModes::default();
        let arrow = KeyStroke {
            key: TerminalKey::ArrowUp,
            modifiers: KeyModifiers::default(),
        };
        assert_eq!(encode_key(&arrow, modes).unwrap(), b"\x1b[A");
        modes.application_cursor = true;
        assert_eq!(encode_key(&arrow, modes).unwrap(), b"\x1bOA");
        modes.alternate_screen = true;
        assert_eq!(
            encode_key(
                &KeyStroke {
                    key: TerminalKey::Tab,
                    modifiers: KeyModifiers {
                        control: true,
                        ..KeyModifiers::default()
                    },
                },
                modes,
            )
            .unwrap(),
            b"\t",
            "Ctrl+Tab remains terminal input rather than a logical app chord"
        );
        assert_eq!(
            encode_key(
                &KeyStroke {
                    key: TerminalKey::Character { text: "c".into() },
                    modifiers: KeyModifiers {
                        control: true,
                        ..KeyModifiers::default()
                    },
                },
                modes,
            )
            .unwrap(),
            [0x03]
        );
        assert_eq!(
            encode_key(
                &KeyStroke {
                    key: TerminalKey::Character { text: "c".into() },
                    modifiers: KeyModifiers {
                        primary: true,
                        ..KeyModifiers::default()
                    },
                },
                modes,
            )
            .unwrap(),
            [0x03]
        );
        assert_eq!(
            encode_key(
                &KeyStroke {
                    key: TerminalKey::End,
                    modifiers: KeyModifiers {
                        control: true,
                        ..KeyModifiers::default()
                    },
                },
                modes,
            )
            .unwrap(),
            b"\x1b[1;5F"
        );
        assert_eq!(
            encode_key(
                &KeyStroke {
                    key: TerminalKey::Character { text: " ".into() },
                    modifiers: KeyModifiers {
                        control: true,
                        ..KeyModifiers::default()
                    },
                },
                modes,
            )
            .unwrap(),
            [0x00]
        );
        modes.bracketed_paste = true;
        assert_eq!(encode_paste("hello", modes), b"\x1b[200~hello\x1b[201~");
        assert_eq!(encode_focus(true, modes), None);
        modes.focus_reporting = true;
        assert_eq!(encode_focus(true, modes), Some(b"\x1b[I".as_slice()));
        assert_eq!(encode_focus(false, modes), Some(b"\x1b[O".as_slice()));
    }

    #[test]
    fn sgr_mouse_reports_click_drag_motion_and_wheel_without_coordinate_loss() {
        let mut modes = TerminalModes {
            mouse_reporting: true,
            mouse_tracking: MouseTrackingMode::Drag,
            mouse_encoding: MouseEncoding::Sgr,
            ..TerminalModes::default()
        };
        assert_eq!(
            encode_mouse(mouse(MouseAction::Press, Some(MouseButton::Left)), modes).unwrap(),
            Some(b"\x1b[<0;5;3M".to_vec())
        );
        assert_eq!(
            encode_mouse(mouse(MouseAction::Move, None), modes).unwrap(),
            None
        );
        assert_eq!(
            encode_mouse(mouse(MouseAction::Move, Some(MouseButton::Left)), modes).unwrap(),
            Some(b"\x1b[<32;5;3M".to_vec())
        );
        modes.mouse_tracking = MouseTrackingMode::Motion;
        assert_eq!(
            encode_mouse(mouse(MouseAction::Move, None), modes).unwrap(),
            Some(b"\x1b[<35;5;3M".to_vec())
        );
        assert_eq!(
            encode_mouse(
                mouse(MouseAction::Press, Some(MouseButton::WheelDown)),
                modes
            )
            .unwrap(),
            Some(b"\x1b[<65;5;3M".to_vec())
        );
        assert_eq!(
            encode_mouse(mouse(MouseAction::Release, Some(MouseButton::Left)), modes).unwrap(),
            Some(b"\x1b[<0;5;3m".to_vec())
        );
    }

    #[test]
    fn legacy_coordinates_are_dropped_instead_of_wrapped() {
        let modes = TerminalModes {
            mouse_reporting: true,
            mouse_tracking: MouseTrackingMode::Click,
            ..TerminalModes::default()
        };
        let mut event = mouse(MouseAction::Press, Some(MouseButton::Left));
        event.column = 300;
        assert_eq!(encode_mouse(event, modes).unwrap(), None);
    }
}
