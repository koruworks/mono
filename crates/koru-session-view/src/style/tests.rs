// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn terminal_palette_is_owned_by_the_captured_style() {
    let appearance = TerminalAppearance {
        dark: false,
        font: FontChoice::SourceCodePro,
        custom_stack: None,
        zoom: 1.25,
    };
    let epoch = appearance.epoch(1.5);
    let light = TerminalStyle {
        epoch,
        metrics: TextMetrics::new(14.0, 20.0),
        stack: appearance.font.stack(),
        cell_width: 8.0,
        cell_height: 20.0,
    };
    let dark = TerminalStyle {
        epoch: StyleEpoch {
            dark: true,
            ..epoch
        },
        ..light
    };
    assert_eq!(epoch.font, FontChoice::SourceCodePro);
    assert_eq!(f32::from_bits(epoch.zoom_bits), 1.25);
    assert_eq!(f32::from_bits(epoch.scale_bits), 1.5);
    let defaults = CellStyle::default();
    assert_eq!(
        light.resolve(defaults).background,
        light.palette().background
    );
    assert_eq!(dark.resolve(defaults).background, dark.palette().background);
    assert_ne!(light.resolve(defaults), dark.resolve(defaults));
    for index in 0..16 {
        let cell = CellStyle {
            foreground: TerminalColor::Indexed { index },
            ..defaults
        };
        assert_eq!(
            light.resolve(cell).foreground,
            light.palette().ansi[index as usize]
        );
        assert_eq!(
            dark.resolve(cell).foreground,
            dark.palette().ansi[index as usize]
        );
    }
}

#[test]
fn a_named_family_replaces_the_stack_the_font_choice_would_give() {
    let mut engine = koru_draw::text::TextEngine::new();
    let appearance = TerminalAppearance {
        dark: true,
        font: FontChoice::JetBrainsMono,
        custom_stack: Some("Fira Code, monospace"),
        zoom: 1.0,
    };
    let style = TerminalStyle::capture_epoch(&mut engine, appearance.epoch(1.0));
    assert_eq!(style.stack, "Fira Code, monospace");
    let plain = TerminalAppearance {
        custom_stack: None,
        ..appearance
    };
    assert_ne!(
        TerminalStyle::capture_epoch(&mut engine, plain.epoch(1.0)).stack,
        style.stack
    );
    assert_ne!(
        appearance.epoch(1.0),
        plain.epoch(1.0),
        "choosing a family is a style change, so the rows are laid out again"
    );
}
