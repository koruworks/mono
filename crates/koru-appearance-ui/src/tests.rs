// SPDX-License-Identifier: MPL-2.0

use super::*;

const VP: (f32, f32) = (1280.0, 800.0);

fn modal() -> (EmojiColorPanel, TextEngine) {
    let mut engine = TextEngine::new();
    let modal = EmojiColorPanel::new(&mut engine, WorkspaceId::new(), "Research", None, None);
    (modal, engine)
}

fn press(m: &mut EmojiColorPanel, engine: &mut TextEngine, rect: Rect) -> Outcome {
    let (x, y) = rect.centre();
    m.mouse_down(engine, VP, x, y)
}

#[test]
fn the_panel_is_centred_and_stays_on_screen() {
    let (m, _) = modal();
    let p = m.panel(VP);
    assert!((p.x + p.w * 0.5 - VP.0 * 0.5).abs() < 1.0);
    assert!(p.y >= 8.0 && p.y + p.h <= VP.1);

    let tiny = m.panel((240.0, 200.0));
    assert!(tiny.x >= 0.0 && tiny.y >= 0.0);
    assert!(tiny.w <= 240.0 && tiny.h <= 200.0);
}

#[test]
fn every_swatch_hits_back_to_itself() {
    let (m, mut engine) = modal();
    let panel = m.panel(VP);
    let count = theme::workspace_color_names().len() + 1;
    for n in 0..count {
        let r = m.swatch_rect(panel, n).expect("a swatch");
        let (x, y) = r.centre();
        assert_eq!(m.hit_at(&mut engine, VP, x, y), Some(Hit::Swatch(n)));
    }
}

#[test]
fn swatches_store_names_and_the_hollow_swatch_clears() {
    let (mut m, mut engine) = modal();
    let panel = m.panel(VP);
    let named = m.swatch_rect(panel, 3).expect("a swatch");
    assert!(matches!(press(&mut m, &mut engine, named), Outcome::Keep));
    let expected = theme::workspace_color_names()[2];
    assert_eq!(m.color.as_deref(), Some(expected));
    assert!(!m.color.as_deref().unwrap().starts_with('#'));

    let clear = m.swatch_rect(panel, 0).expect("a swatch");
    press(&mut m, &mut engine, clear);
    assert_eq!(m.color, None);
}

#[test]
fn the_shared_picker_surface_lies_inside_the_modal() {
    let (m, _) = modal();
    let panel = m.panel(VP);
    let picker = m.picker_rect(panel);
    assert_eq!(picker.w, emoji_picker::PANEL_W);
    assert!(picker.x >= panel.x && picker.x + picker.w <= panel.x + panel.w);
    assert!(picker.y >= panel.y && picker.y + picker.h <= panel.y + panel.h);
}

#[test]
fn a_compact_modal_keeps_its_choices_scrollable_and_its_actions_visible() {
    for viewport in [(300.0, 360.0), (360.0, 413.0), (600.0, 500.0), VP] {
        let (mut modal, mut engine) = modal();
        modal.draw(&mut Scene::new(), Affine::IDENTITY, &mut engine, viewport);
        let panel = modal.panel(viewport);
        let picker = modal.picker_rect(panel);
        assert!(picker.x >= panel.x && picker.x + picker.w <= panel.x + panel.w);
        let (clear, cancel, save) = modal.buttons(&mut engine, panel);
        for button in [clear, cancel, save] {
            assert!(button.x >= panel.x && button.x + button.w <= panel.x + panel.w);
            assert!(button.y >= panel.y && button.y + button.h <= panel.y + panel.h);
        }
        assert!(clear.y + clear.h <= cancel.y || clear.x + clear.w <= cancel.x);
        modal.scroll_by(MouseScrollDelta::LineDelta(0.0, -1000.0));
        let body = modal.body_rect(&mut engine, panel);
        let category = modal.picker.geometry().category_rect(0).unwrap();
        assert!(picker.y + category.y + category.h - modal.body_scroll <= body.y + body.h);
        // Keyboard movement reveals the color choices again, using the same
        // translated rectangles pointer input reads.
        modal.key(
            &mut engine,
            &Key::Named(NamedKey::ArrowRight),
            None,
            ModifiersState::empty(),
        );
        for index in 0..EmojiColorPanel::colors().len() {
            let swatch = modal.swatch_rect(panel, index).unwrap();
            assert!(swatch.x >= body.x && swatch.x + swatch.w <= body.x + body.w);
            let (x, y) = swatch.centre();
            let y = y - modal.body_scroll;
            assert!(body.contains(x, y));
            assert_eq!(
                modal.hit_at(&mut engine, viewport, x, y),
                Some(Hit::Swatch(index))
            );
        }
        for _ in 0..3 {
            modal.key(
                &mut engine,
                &Key::Named(NamedKey::Tab),
                None,
                ModifiersState::empty(),
            );
        }
        assert_eq!(modal.focus, Focus::Category);
        assert!(picker.y + category.y - modal.body_scroll >= body.y);
        let (x, y) = save.centre();
        assert!(matches!(
            modal.mouse_down(&mut engine, viewport, x, y),
            Outcome::Save { .. }
        ));
    }
}

#[test]
fn choosing_from_the_shared_picker_updates_the_preview_value() {
    let (mut m, mut engine) = modal();
    let expected_index = m.picker.visible_hits()[0];
    let expected = EmojiPicker::display_value(koru_editor::emoji::catalog()[expected_index].value);
    let panel = m.panel(VP);
    let picker = m.picker_rect(panel);
    let cell = m.picker.cell_rect(0).expect("first shared picker cell");
    let window_cell = Rect::new(picker.x + cell.x, picker.y + cell.y, cell.w, cell.h);

    assert!(matches!(
        press(&mut m, &mut engine, window_cell),
        Outcome::Keep
    ));
    assert!(matches!(m.mouse_up(), Outcome::Keep));
    assert_eq!(m.icon.as_deref(), Some(expected.as_str()));
}

#[test]
fn clear_emoji_is_separate_from_the_shared_catalogue() {
    let mut engine = TextEngine::new();
    let mut m = EmojiColorPanel::new(
        &mut engine,
        WorkspaceId::new(),
        "Research",
        Some("🔬".into()),
        None,
    );
    let panel = m.panel(VP);
    let (clear, _, _) = m.buttons(&mut engine, panel);
    press(&mut m, &mut engine, clear);
    assert_eq!(m.icon, None);
}

#[test]
fn searching_and_categories_are_the_editor_picker_not_a_local_grid() {
    let (mut m, mut engine) = modal();
    m.key(
        &mut engine,
        &Key::Named(NamedKey::Tab),
        None,
        ModifiersState::empty(),
    );
    let before = m.picker.hits.len();
    for ch in "microscope".chars() {
        let text = ch.to_string();
        m.key(
            &mut engine,
            &Key::Character(text.clone().into()),
            Some(&text),
            ModifiersState::empty(),
        );
    }
    assert!(!m.picker.hits.is_empty());
    assert!(m.picker.hits.len() < before);
    assert_eq!(m.picker.category, None, "search spans every category");
}

#[test]
fn escape_cancels_but_enter_chooses_the_highlighted_emoji() {
    let (mut m, mut engine) = modal();
    for _ in 0..2 {
        m.key(
            &mut engine,
            &Key::Named(NamedKey::Tab),
            None,
            ModifiersState::empty(),
        );
    }
    assert!(matches!(
        m.key(
            &mut engine,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty()
        ),
        Outcome::Keep
    ));
    assert!(m.icon.is_some());
    assert!(matches!(
        m.key(
            &mut engine,
            &Key::Named(NamedKey::Escape),
            None,
            ModifiersState::empty()
        ),
        Outcome::Cancel
    ));
}

#[test]
fn tab_reaches_save_instead_of_walking_categories() {
    let (mut m, mut engine) = modal();
    let start = m.picker.category;
    for _ in 0..6 {
        m.key(
            &mut engine,
            &Key::Named(NamedKey::Tab),
            None,
            ModifiersState::empty(),
        );
    }
    assert_eq!(
        m.picker.category, start,
        "Tab is the modal order, not categories"
    );
    assert!(matches!(
        m.key(
            &mut engine,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty()
        ),
        Outcome::Save { .. }
    ));
}

#[test]
fn arrows_choose_a_color_from_the_swatch_row() {
    let (mut m, mut engine) = modal();
    assert_eq!(m.color, None);
    m.key(
        &mut engine,
        &Key::Named(NamedKey::ArrowRight),
        None,
        ModifiersState::empty(),
    );
    assert_eq!(m.color.as_deref(), Some(theme::workspace_color_names()[0]));
}

#[test]
fn buttons_are_ordered_and_a_press_outside_cancels() {
    let (mut m, mut engine) = modal();
    let panel = m.panel(VP);
    let (clear, cancel, save) = m.buttons(&mut engine, panel);
    assert!(clear.x + clear.w <= cancel.x);
    assert!(cancel.x + cancel.w <= save.x);
    assert!(save.x + save.w <= panel.x + panel.w - PAD + 0.01);
    assert!(matches!(
        m.mouse_down(&mut engine, VP, 4.0, 4.0),
        Outcome::Cancel
    ));
}

#[test]
fn the_contents_are_drawn_inside_the_floating_panel() {
    let (mut m, mut engine) = modal();
    let mut canvas = Scene::new();
    let root = Affine::IDENTITY;
    let capture = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, VP.0, VP.1));
    m.draw(&mut canvas, root, &mut engine, VP);
    let snapshot = capture.finish();
    let records = &snapshot.records;
    let component = records
        .iter()
        .position(|r| r.name == "EmojiColorPanel")
        .expect("the component");
    let panel = records
        .iter()
        .position(|r| r.name == "FloatingPanel")
        .expect("its panel");
    assert_eq!(records[panel].parent, Some(component));
    let inside = |mut at: Option<usize>| {
        while let Some(i) = at {
            if i == panel {
                return true;
            }
            at = records[i].parent;
        }
        false
    };
    // Everything painted after the body — preview, swatches, picker, buttons —
    // hangs under the panel rather than beside it.
    let later: Vec<_> = records.iter().skip(panel + 1).collect();
    assert!(!later.is_empty());
    for record in later {
        assert!(inside(record.parent), "{} escaped the panel", record.name);
    }
}

/// The emoji list spans the same column as the COLOR and EMOJI labels: it
/// starts at their left edge and its scrollbar ends at the right one.
#[test]
fn the_emoji_list_fills_the_labels_column() {
    let (m, _) = modal();
    let panel = m.panel(VP);
    let picker = m.picker_rect(panel);
    assert_eq!(picker.x, panel.x + PAD, "aligned with the labels");
    let geometry = m.picker.geometry();
    assert_eq!(geometry.grid_rect().x, 0.0);
    assert_eq!(geometry.search_rect().x, 0.0);
    assert_eq!(geometry.search_rect().w, picker.w);
    let track = geometry.scrollbar_rect();
    assert!(
        (track.x + track.w - picker.w).abs() < 0.01,
        "ends at the right edge"
    );
}

/// The search is the shared text input, the list is one `EmojiList`, each
/// emoji is an `Emoji` component inside it, and the swatches are one
/// `ColorPickerRow`.
#[test]
fn the_picker_is_made_of_named_components() {
    let (mut m, mut engine) = modal();
    let mut canvas = Scene::new();
    let root = Affine::IDENTITY;
    let capture = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, VP.0, VP.1));
    m.draw(&mut canvas, root, &mut engine, VP);
    let records = capture.finish().records;
    let named = |name: &str| records.iter().filter(|r| r.name == name).count();
    assert_eq!(named("KoruTextInput"), 1, "the search field");
    assert_eq!(named("EmojiList"), 1);
    assert_eq!(named("ColorPickerRow"), 1);
    let list = records.iter().position(|r| r.name == "EmojiList").unwrap();
    let emoji: Vec<_> = records.iter().filter(|r| r.name == "Emoji").collect();
    assert!(!emoji.is_empty());
    assert!(emoji.iter().all(|r| r.parent == Some(list)));
}
