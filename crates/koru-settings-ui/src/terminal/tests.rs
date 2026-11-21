// SPDX-License-Identifier: MPL-2.0

//! The Terminal page: the terminal's own menu vocabulary, as cards, patched
//! per field.

use super::*;
use crate::tests::{GLOBALS, flip, is_active, labels, page_lines, press_on, switch_on};
use crate::{Availability, PageId, Sections, SettingsLinks, SettingsSurface};
use koru_i18n::{Key, Lang, t};
use koru_settings::Zoom;

/// Koru's surface on its Terminal page, with a link that records patches.
fn terminal() -> SettingsSurface {
    let mut surface =
        SettingsSurface::with_links(Sections::koru(), SettingsLinks::inert(false, true));
    surface.set_viewport(1000.0, 700.0);
    surface.show_page(PageId::Terminal);
    surface.relayout();
    surface
}

/// Stop following the editor caret, so the cursor's own rows are drawn, and
/// forget the patch that did it.
fn own_cursor(surface: &mut SettingsSurface) {
    let label = t(surface.lang, Key::SettingsMatchTheEditorCaret);
    assert!(flip(surface, label));
    surface.links.take_outstanding_terminal();
    surface.show_page(PageId::Terminal);
    surface.relayout();
}

#[test]
fn every_terminal_row_is_drawn() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = terminal();
    own_cursor(&mut surface);
    let drawn = page_lines(&surface);
    for label in [
        "Theme",
        "Font",
        t(Lang::En, Key::SettingsZoom),
        t(Lang::En, Key::TerminalCursor),
        "Match the editor caret",
        t(Lang::En, Key::TerminalCursorColor),
        t(Lang::En, Key::TerminalCursorBlink),
        "Shape",
    ] {
        assert!(
            drawn.iter().any(|line| line == label),
            "the `{label}` row is gone: {drawn:?}"
        );
    }
    let pressable = labels(&surface);
    for label in ["Follow app", "Light", "Dark"] {
        assert!(pressable.iter().any(|word| word == label), "{label}");
    }
}

/// **Mono only.** A proportional face in a grid renderer is a broken window,
/// not a preference, and `TerminalPatch::validate` refuses one — so offering it
/// would be offering a choice the authority will not take.
#[test]
fn the_font_row_offers_only_families_the_authority_accepts() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let surface = terminal();
    let offered: Vec<_> = mono_families().collect();
    assert_eq!(offered.len(), 3);
    // The Font row's own options — *System* is also a theme, so the page's
    // words would answer for the wrong row.
    let font = surface
        .laid
        .dropdowns
        .iter()
        .find(|d| {
            d.options
                .iter()
                .any(|(a, _)| matches!(a, crate::action::Action::TerminalFont(_)))
        })
        .expect("the font dropdown");
    let words: Vec<&str> = font.options.iter().map(|(_, o)| o.label.as_str()).collect();
    for family in FontFamily::ALL {
        let label = FontChoice::parse(family.token()).label();
        assert_eq!(
            words.contains(&label),
            family.is_mono(),
            "{label}: offered exactly when it is a terminal face"
        );
    }
    for family in offered {
        let patch = koru_settings::TerminalPatch {
            font: Some(koru_settings::FontSetting::Chosen(family)),
            ..Default::default()
        };
        assert!(patch.validate().is_ok(), "{family:?}");
    }
}

/// Every zoom the stepper offers is one the registry will accept.
#[test]
fn every_zoom_step_is_one_the_registry_accepts() {
    assert_eq!(accepted_zooms().count(), ZOOM_STEPS.len());
    for factor in ZOOM_STEPS {
        let patch = koru_settings::TerminalPatch {
            zoom: Some(factor),
            ..Default::default()
        };
        assert!(patch.validate().is_ok(), "{factor}");
    }
}

#[test]
fn the_zoom_stepper_moves_one_step_and_stops_at_the_ends() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = terminal();
    let stepper = |surface: &SettingsSurface| {
        surface
            .laid
            .blocks
            .iter()
            .find_map(|block| match block {
                crate::page::Block::Card(card) => card.rows.iter().find_map(|r| match &r.widget {
                    crate::page::Widget::Stepper {
                        control,
                        less,
                        more,
                        ..
                    } => Some((*control, *less, *more)),
                    _ => None,
                }),
                _ => None,
            })
            .expect("the zoom stepper")
    };
    let (control, less, more) = stepper(&surface);
    assert!(less.is_some() && more.is_some(), "100% is mid-scale");
    assert!(crate::tests::press_rect(&mut surface, control.more));
    assert_eq!(
        surface.links.take_outstanding_terminal().unwrap().zoom,
        Some(1.1)
    );
    surface.adopt_terminal(&TerminalSettings {
        zoom: Zoom::new(1.5).unwrap(),
        ..TerminalSettings::default()
    });
    surface.relayout();
    let (_, _, more) = stepper(&surface);
    assert!(more.is_none(), "150% is the end of the scale");
}

/// A press patches its own field and names no other — which is what makes two
/// windows changing two different settings both land.
#[test]
fn a_press_patches_one_field_and_names_no_other() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = terminal();
    assert!(press_on(&mut surface, PageId::Terminal, "Dark"));
    let patch = surface
        .links
        .take_outstanding_terminal()
        .expect("a patch went out");
    assert_eq!(patch.theme, Some(ThemeChoice::Dark));
    assert_eq!(patch.font, None);
    assert_eq!(patch.zoom, None);
    assert_eq!(patch.cursor_blink, None);
    assert_eq!(patch.composer_default, None);
    assert_eq!(
        surface.terminal.theme,
        ThemeChoice::Dark,
        "applied at once, before the reply"
    );
}

/// **The switch is the two follow values**, not a setting of its own: on
/// exactly when width and height both follow the app.
#[test]
fn match_the_editor_caret_is_the_two_follow_values() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = terminal();
    assert!(
        switch_on(&mut surface, "Match the editor caret"),
        "the default"
    );
    assert!(
        !page_lines(&surface).iter().any(|l| l == "Shape"),
        "no shape of its own while it follows"
    );

    let (was_w, was_h) = (theme::caret_width(), theme::caret_height());
    theme::set_caret_width(theme::CaretWidth::Thick);
    theme::set_caret_height(theme::CaretHeight::Tall);
    assert!(flip(&mut surface, "Match the editor caret"));
    let patch = surface.links.take_outstanding_terminal().unwrap();
    // Off gives it the caret it already had, so nothing changes on screen.
    assert_eq!(patch.cursor_blink, None, "blink is its own row");
    assert_eq!(patch.cursor_width, Some(TerminalCursorWidth::Thick));
    assert_eq!(patch.cursor_height, Some(TerminalCursorHeight::Tall));
    assert_eq!(patch.cursor_color, None, "color has no follow value");
    assert!(!matches_caret(&surface.terminal));

    assert!(flip(&mut surface, "Match the editor caret"));
    let patch = surface.links.take_outstanding_terminal().unwrap();
    assert_eq!(patch.cursor_width, Some(TerminalCursorWidth::Application));
    assert_eq!(patch.cursor_height, Some(TerminalCursorHeight::Application));
    assert!(matches_caret(&surface.terminal));
    theme::set_caret_width(was_w);
    theme::set_caret_height(was_h);
}

#[test]
fn every_cursor_property_patches_its_own_field() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = terminal();
    own_cursor(&mut surface);

    // Blink is a switch now; the default (follow the app) blinks, so the
    // first flip turns it off.
    assert!(crate::tests::flip(
        &mut surface,
        t(Lang::En, Key::TerminalCursorBlink)
    ));
    assert_eq!(
        surface
            .links
            .take_outstanding_terminal()
            .unwrap()
            .cursor_blink,
        Some(TerminalCursorBlink::Off)
    );

    let color = crate::tests::swatches(&surface, t(Lang::En, Key::TerminalCursorColor));
    let amber = CaretColorChoice::ALL
        .iter()
        .position(|c| *c == CaretColorChoice::Amber)
        .unwrap();
    assert!(crate::tests::press_rect(&mut surface, color[amber]));
    assert_eq!(
        surface
            .links
            .take_outstanding_terminal()
            .unwrap()
            .cursor_color,
        Some(CaretColorChoice::Amber)
    );

    assert!(press_on(
        &mut surface,
        PageId::Terminal,
        t(Lang::En, Key::TerminalCursor4x)
    ));
    assert_eq!(
        surface
            .links
            .take_outstanding_terminal()
            .unwrap()
            .cursor_width,
        Some(TerminalCursorWidth::X4)
    );

    assert!(press_on(
        &mut surface,
        PageId::Terminal,
        t(Lang::En, Key::TerminalCursorShort)
    ));
    assert_eq!(
        surface
            .links
            .take_outstanding_terminal()
            .unwrap()
            .cursor_height,
        Some(TerminalCursorHeight::Short)
    );
}

#[test]
fn the_removed_input_area_switch_cannot_change_the_default() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = terminal();
    assert!(
        !page_lines(&surface)
            .iter()
            .any(|line| line == t(Lang::En, Key::SettingsSeparateInputArea))
    );
    assert!(surface.links.take_outstanding_terminal().is_none());
}

/// The page draws what the authority says. A change another window committed
/// arrives through the link and repaints.
#[test]
fn a_section_from_another_window_repaints_the_rows() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = terminal();
    assert!(surface.adopt_terminal(&TerminalSettings {
        zoom: Zoom::new(1.25).unwrap(),
        ..TerminalSettings::default()
    }));
    surface.relayout();
    assert!(is_active(&surface, "125%"));
    assert!(!is_active(&surface, "100%"));
}

/// Zoom is compared as it persists, never as it floats: `0.1 * 9` and the
/// `0.90` read back out of the file are different `f32`s.
#[test]
fn a_zoom_read_back_off_the_wire_still_reads_as_chosen() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = terminal();
    surface.adopt_terminal(&TerminalSettings {
        zoom: Zoom::new(0.1_f32 * 9.0).unwrap(),
        ..TerminalSettings::default()
    });
    surface.relayout();
    assert!(is_active(&surface, "90%"));
}

/// All eight, and none of them empty.
#[test]
fn every_terminal_row_has_a_label_in_every_language() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    for lang in Lang::ALL {
        let mut surface = terminal();
        surface.set_lang(lang);
        own_cursor(&mut surface);
        assert!(
            page_lines(&surface)
                .iter()
                .all(|line| !line.trim().is_empty()),
            "{lang:?} left a row blank"
        );
        assert!(
            labels(&surface)
                .iter()
                .all(|label| !label.trim().is_empty()),
            "{lang:?} left a control blank"
        );
    }
}

/// The Terminal page owes the same read-only presentation the others do.
#[test]
fn the_terminal_page_is_told_why_its_rows_are_read_only() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = terminal();
    surface.set_availability(Availability::Down);
    surface.relayout();
    let reason = t(Lang::En, Key::SettingsUnavailableReadOnly);
    assert_eq!(
        page_lines(&surface)
            .iter()
            .filter(|line| *line == reason)
            .count(),
        1,
        "said once, first thing on the page"
    );
}

/// **The constructor the standalone Terminal used**, kept until it is removed:
/// it links both sections itself.
#[test]
fn the_terminal_constructor_links_both_sections() {
    let surface =
        SettingsSurface::with_links(Sections::terminal(), SettingsLinks::inert(true, true));
    assert_eq!(surface.sections(), Sections::terminal());
    assert!(surface.links().holds_global());
    assert!(surface.links().holds_terminal());
}

/// While the cursor matches the editor caret it takes the caret's colour as
/// well as its shape, so Colour and Shape fold away with it; Blink stays.
#[test]
fn matching_the_editor_caret_folds_colour_and_shape_away() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = terminal();
    surface.show_page(PageId::Terminal);
    surface.relayout();
    let drawn = page_lines(&surface);
    assert!(drawn.iter().any(|l| l == "Match the editor caret"));
    assert!(
        drawn
            .iter()
            .any(|l| l == t(Lang::En, Key::TerminalCursorBlink))
    );
    assert!(
        !drawn
            .iter()
            .any(|l| l == t(Lang::En, Key::TerminalCursorColor))
    );
    assert!(!drawn.iter().any(|l| l == "Shape"));
    own_cursor(&mut surface);
    let drawn = page_lines(&surface);
    assert!(
        drawn
            .iter()
            .any(|l| l == t(Lang::En, Key::TerminalCursorColor))
    );
    assert!(drawn.iter().any(|l| l == "Shape"));
}
