// SPDX-License-Identifier: MPL-2.0

//! The toolbar's three settings and the list they open, driven through the
//! window.
//!
//! **The theme is process-wide**, so every test here that changes it holds
//! the binary's theme lock for as long as it runs and puts the theme back
//! before letting go ([`ThemeGuard`]). The language is the Lab's own value
//! and is never written to the process: these tests change it freely.

use super::super::tests::{
    SIZE, SMALLEST, assert_sound, centre, draw, drawn, english, field, frame, named, press, record,
    story, tab, type_text,
};
use super::*;

const NONE: ModifiersState = ModifiersState::empty();

/// The theme lock, and the theme to go back to when the test ends.
struct ThemeGuard {
    before: &'static Theme,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for ThemeGuard {
    fn drop(&mut self) {
        themes::set_active(self.before);
    }
}

fn theme_guard() -> ThemeGuard {
    let lock = crate::tests::lock_theme();
    let before = themes::active();
    themes::set_active(&themes::LIGHT_THEME);
    ThemeGuard {
        before,
        _lock: lock,
    }
}

fn key(lab: &mut Lab, engine: &mut TextEngine, key: NamedKey) -> bool {
    named(lab, engine, key, NONE)
}

/// Open `setting`'s list with the pointer.
fn open(lab: &mut Lab, engine: &mut TextEngine, setting: Setting) {
    assert!(press(lab, engine, setting.key()));
    assert_eq!(
        lab.menu.as_ref().map(|open| open.owner),
        Some(setting.key())
    );
}

/// Where the open list draws the option whose keyword is `part`.
fn option_at(lab: &Lab, part: &str) -> (f32, f32) {
    let open = lab.menu.as_ref().expect("a list is open");
    let slot = open
        .choices
        .iter()
        .position(|choice| choice.part == part)
        .unwrap_or_else(|| panic!("the list has no `{part}`"));
    open.menu.item_rect(SIZE, slot).centre()
}

/// Open `setting`'s list and press the option `part`.
fn pick(lab: &mut Lab, engine: &mut TextEngine, setting: Setting, part: &str) {
    open(lab, engine, setting);
    let (x, y) = option_at(lab, part);
    assert!(lab.click(engine, x, y, false));
    lab.mouse_up();
    assert!(lab.menu.is_none(), "choosing closes the list");
    draw(lab, engine, SIZE);
}

/// The three settings are in the toolbar, before Reset, each showing the
/// value in force and each as wide as its widest option.
#[test]
fn the_toolbar_holds_the_three_settings() {
    let _theme = theme_guard();
    english(|| {
        let (lab, _) = drawn(SIZE);
        let stops = lab.laid.as_ref().unwrap().stops();
        assert_eq!(
            stops[..4],
            [
                Setting::Theme.key(),
                Setting::Width.key(),
                Setting::Language.key(),
                RESET
            ]
        );
        let toolbar = frame(&lab).rect_of("Toolbar").unwrap();
        let mut right = 0.0;
        for setting in Setting::ALL {
            let rect = lab.rect(setting.key()).unwrap();
            assert!(rect.y >= toolbar.y && rect.y + rect.h <= toolbar.y + toolbar.h);
            assert!(rect.x >= right, "{setting:?} overlaps the one before it");
            assert_eq!(rect.h, Dropdown::H);
            right = rect.x + rect.w;
        }
        assert!(right <= lab.rect(RESET).unwrap().x);
        let value = |setting: Setting| lab.chosen(setting.key()).unwrap().label;
        assert_eq!(value(Setting::Theme), "Light");
        assert_eq!(value(Setting::Width), "Full width");
        assert_eq!(value(Setting::Language), "English");
        // The themes are the ones the drawing crate registers, each once;
        // the widths are the presets; the languages are the eight, each
        // named in itself.
        let parts = |setting: Setting| -> Vec<&str> {
            lab.choices(setting.key())
                .iter()
                .map(|choice| choice.part)
                .collect()
        };
        let registered: Vec<&str> = themes::registered().iter().map(|t| t.key).collect();
        assert_eq!(parts(Setting::Theme), registered);
        assert!(registered.starts_with(&["light", "dark"]));
        assert_eq!(parts(Setting::Width), ["fit", "320", "480", "640", "800"]);
        let tags: Vec<&str> = koru_i18n::Lang::ALL.iter().map(|lang| lang.tag()).collect();
        assert_eq!(parts(Setting::Language), tags);
        let endonyms: Vec<String> = lab
            .choices(Setting::Language.key())
            .into_iter()
            .map(|choice| choice.label)
            .collect();
        assert_eq!(endonyms[7], "日本語");
        for setting in Setting::ALL {
            let chosen = lab
                .choices(setting.key())
                .iter()
                .filter(|choice| choice.selected)
                .count();
            assert_eq!(chosen, 1, "{setting:?}");
        }
    });
}

/// Choosing a theme redraws the whole window in it: the chrome and the
/// story alike, from layouts shaped again and fields built again holding
/// what they held. Nothing but this process's own theme is changed.
#[test]
fn choosing_a_theme_redraws_everything_in_it() {
    let _theme = theme_guard();
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        press(&mut lab, &mut engine, field());
        type_text(&mut lab, &mut engine, " now");
        let typed = story(&lab).label().to_owned();
        lab.set_field(&mut engine, SEARCH, "button");
        draw(&mut lab, &mut engine, SIZE);
        assert!(
            frame(&lab)
                .nodes
                .iter()
                .all(|node| node.cx.theme.key == "light")
        );
        assert!(!lab.cache.is_empty());

        pick(&mut lab, &mut engine, Setting::Theme, "dark");
        assert!(std::ptr::eq(themes::active(), &themes::DARK_THEME));
        assert_eq!(lab.chosen(Setting::Theme.key()).unwrap().label, "Dark");
        // Every element of the next frame resolved under the new theme: the
        // caption, the toolbar, the navigator, the page and the properties.
        let frame = frame(&lab);
        assert!(frame.nodes.len() > 50);
        for node in &frame.nodes {
            assert_eq!(
                node.cx.theme.key, "dark",
                "`{}` kept the old theme",
                node.name
            );
        }
        for name in ["Caption", "Toolbar", "Navigator", "Preview", "Properties"] {
            assert!(frame.named(name).next().is_some(), "{name}");
        }
        // What was typed is still there, in fields that were built again.
        assert_eq!(story(&lab).label(), typed);
        assert_eq!(lab.field(field()).unwrap().raw_text(), typed);
        assert_eq!(lab.query(), "button");
        assert_eq!(story(&lab).presses(), 0);
        assert!(lab.take_appearance_changed());
        assert!(!lab.take_appearance_changed(), "told once");
        assert_sound(&lab, "the dark theme");

        // Choosing the theme in force changes nothing and throws nothing
        // away.
        let held = lab.cache.len();
        pick(&mut lab, &mut engine, Setting::Theme, "dark");
        assert!(!lab.take_appearance_changed());
        assert!(lab.cache.len() >= held);
        // Reset is the story's: the theme stays.
        assert!(press(&mut lab, &mut engine, RESET));
        assert!(std::ptr::eq(themes::active(), &themes::DARK_THEME));

        pick(&mut lab, &mut engine, Setting::Theme, "light");
        assert!(std::ptr::eq(themes::active(), &themes::LIGHT_THEME));
        assert!(
            super::super::tests::frame(&lab)
                .nodes
                .iter()
                .all(|node| node.cx.theme.key == "light")
        );
    });
}

/// The layout cache is emptied by a theme change, so no layout shaped with
/// the old theme's brush can be handed to a frame of the new one.
#[test]
fn a_theme_change_drops_every_shaped_layout() {
    let _theme = theme_guard();
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        assert!(!lab.cache.is_empty());
        assert!(lab.set_theme(&mut engine, &themes::DARK_THEME));
        assert!(lab.cache.is_empty());
        assert!(!lab.set_theme(&mut engine, &themes::DARK_THEME));
        draw(&mut lab, &mut engine, SIZE);
        assert!(!lab.cache.is_empty());
    });
}

/// The list is the keyboard's too: Enter, Space or an arrow opens it on the
/// option in force, the arrows step it, Enter chooses, and Escape or Tab
/// closes it without choosing. The keyboard stays on the field throughout.
#[test]
fn a_setting_is_chosen_from_the_keyboard() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let width = Setting::Width.key();
        while lab.focus != Some(width) {
            assert!(tab(&mut lab, &mut engine));
        }
        // The box round the field wears the focus ring.
        let ring = frame(&lab)
            .named("Setting")
            .find(|node| node.state.focused)
            .expect("the focused setting is marked");
        assert!(ring.ring.is_some());

        assert!(key(&mut lab, &mut engine, NamedKey::Enter));
        let open = lab.menu.as_ref().unwrap();
        assert_eq!(open.owner, width);
        assert_eq!(open.menu.hover, Some(0), "opens on the option in force");
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowDown));
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowDown));
        assert_eq!(lab.menu.as_ref().unwrap().menu.hover, Some(2));
        // Every key is the list's while it is open: Tab does not move on.
        assert_eq!(lab.width, PreviewWidth::Fit, "stepping chooses nothing");
        assert!(key(&mut lab, &mut engine, NamedKey::Enter));
        assert!(lab.menu.is_none());
        assert_eq!(lab.width, PreviewWidth::Fixed(480));
        assert_eq!(lab.focus, Some(width), "the keyboard is where it was");

        // An arrow opens it too; Escape and Tab close it and choose nothing.
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowDown));
        assert_eq!(lab.menu.as_ref().unwrap().menu.hover, Some(2));
        assert!(key(&mut lab, &mut engine, NamedKey::End));
        assert!(key(&mut lab, &mut engine, NamedKey::Escape));
        assert!(lab.menu.is_none());
        assert_eq!(lab.width, PreviewWidth::Fixed(480));
        assert!(key(&mut lab, &mut engine, NamedKey::Space));
        assert!(key(&mut lab, &mut engine, NamedKey::Home));
        assert!(key(&mut lab, &mut engine, NamedKey::Tab));
        assert!(lab.menu.is_none());
        assert_eq!(lab.focus, Some(width));
        assert_eq!(lab.width, PreviewWidth::Fixed(480));
        // A held Enter opens once and does not then choose.
        let enter = KeyboardKey::Named(NamedKey::Enter);
        assert!(lab.key_down(&mut engine, &enter, None, NONE, false));
        assert!(!lab.key_down(&mut engine, &enter, None, NONE, true));
        assert!(lab.menu.is_some());
    });
}

/// While a list is open the pointer lights its options, a press outside
/// closes it without pressing what was under it, and the wheel scrolls
/// nothing.
#[test]
fn an_open_list_takes_the_pointer() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        open(&mut lab, &mut engine, Setting::Language);
        let lit = lab.hot;
        let (x, y) = option_at(&lab, "fr");
        assert!(lab.pointer_moved(x, y));
        assert_eq!(lab.menu.as_ref().unwrap().menu.hover, Some(4));
        assert!(!lab.pointer_moved(x + 1.0, y), "the same option");
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(lab.hot, lit, "nothing under the list is lit");

        // The list is drawn inside the window, under its field.
        let panel = lab.menu.as_ref().unwrap().menu.panel(SIZE);
        let field = lab.rect(Setting::Language.key()).unwrap();
        assert!(panel.y >= field.y + field.h);
        assert!(panel.x >= 0.0 && panel.x + panel.w <= SIZE.0);
        assert!(panel.y + panel.h <= SIZE.1);

        let list = frame(&lab).node(LIST).unwrap().viewport.unwrap();
        lab.pointer_moved(list.x + 20.0, list.y + 20.0);
        let scrolled = lab.nav.scroll;
        assert!(!lab.scroll_by(-80.0, NONE));
        assert_eq!(lab.nav.scroll, scrolled);

        let (sx, sy) = centre(&lab, SUBJECT);
        assert!(lab.click(&mut engine, sx, sy, false));
        assert!(lab.menu.is_none());
        assert_eq!(story(&lab).presses(), 0, "the press only closed the list");
        assert_eq!(lab.lang, koru_i18n::Lang::En);
        // Pressing the field again closes its own list.
        open(&mut lab, &mut engine, Setting::Language);
        let (fx, fy) = centre(&lab, Setting::Language.key());
        assert!(lab.click(&mut engine, fx, fy, false));
        assert!(lab.menu.is_none());
        // A resize, or the window losing the keyboard, closes it as well.
        open(&mut lab, &mut engine, Setting::Theme);
        lab.resized(PhysicalSize::new(900, 700), 1.0);
        assert!(lab.menu.is_none());
        draw(&mut lab, &mut engine, SIZE);
        open(&mut lab, &mut engine, Setting::Theme);
        lab.focus_changed(false);
        assert!(lab.menu.is_none());
    });
}

/// The preview is as wide as its setting says while the column has the
/// room, and as wide as the column when it has not. Reset leaves it.
#[test]
fn the_preview_width_is_the_stages() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let hugging = lab.rect(STAGE).unwrap();
        let room = lab.page_room(SIZE.0);
        assert!(hugging.w < 320.0 && room > 480.0, "{hugging:?} in {room}");

        for (part, expected) in [("320", 320.0), ("480", 480.0), ("800", room)] {
            pick(&mut lab, &mut engine, Setting::Width, part);
            let stage = lab.rect(STAGE).unwrap();
            assert_eq!(stage.w, expected, "{part}");
            // The component keeps its own width, in the middle of the stage.
            let subject = lab.rect(SUBJECT).unwrap();
            assert_eq!(
                subject.w,
                hugging.w - 2.0 * STAGE_INSET.token.px(Profile::Desktop)
            );
            let centre = |rect: Rect| rect.x + rect.w * 0.5;
            assert!((centre(subject) - centre(stage)).abs() < 0.5);
            let page = frame(&lab).node(PAGE).unwrap().rect;
            assert!(stage.x >= page.x && stage.x + stage.w <= page.x + page.w);
            assert_sound(&lab, part);
        }
        // Reset is the story's.
        press(&mut lab, &mut engine, SUBJECT);
        assert!(press(&mut lab, &mut engine, RESET));
        assert_eq!(lab.width, PreviewWidth::Fixed(800));
        // Another record is shown at the same width.
        lab.show(&mut engine, record("T-RADIUS"));
        lab.show(&mut engine, record(registry::BUTTON));
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(lab.rect(STAGE).unwrap().w, room);

        pick(&mut lab, &mut engine, Setting::Width, "fit");
        assert_eq!(lab.rect(STAGE).unwrap().w, hugging.w);

        // In the smallest window every preset is wider than the column.
        lab.set_width(PreviewWidth::Fixed(320));
        draw(&mut lab, &mut engine, SMALLEST);
        let narrow = lab.page_room(SMALLEST.0);
        assert!(narrow < 320.0);
        assert_eq!(lab.rect(STAGE).unwrap().w, narrow);
        assert_sound(&lab, "a preset wider than the column");
    });
}

/// Choosing a language shows the whole window in it from the next frame:
/// what is read, what the search finds, and a story nobody has changed. A
/// story that was changed keeps what it was given. Nothing outside the Lab
/// is told.
#[test]
fn choosing_a_language_shows_the_lab_in_it() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        assert_eq!(lab.lang, koru_i18n::Lang::En);
        assert_eq!(story(&lab).label(), "Save changes");
        for lang in koru_i18n::Lang::ALL {
            pick(&mut lab, &mut engine, Setting::Language, lang.tag());
            assert_eq!(lab.lang, lang);
            assert_eq!(
                lab.take_appearance_changed(),
                lang != koru_i18n::Lang::En,
                "{lang:?}"
            );
            assert_sound(&lab, lang.tag());
            // The untouched sample is in the new language.
            assert_eq!(
                story(&lab).label(),
                koru_i18n::t(lang, koru_i18n::Key::UiLabButtonSampleLabel)
            );
            assert_eq!(lab.field(field()).unwrap().raw_text(), story(&lab).label());
            // The window is titled and its controls named in it.
            assert_eq!(
                lab.window().title,
                koru_i18n::t(lang, koru_i18n::Key::UiLabTitle)
            );
            // The value shown is the language's own name, whatever is read.
            assert_eq!(
                lab.chosen(Setting::Language.key()).unwrap().label,
                lang.endonym()
            );
            draw(&mut lab, &mut engine, SMALLEST);
            assert_sound(&lab, lang.tag());
            draw(&mut lab, &mut engine, SIZE);
        }
        // This thread's own language was never touched: only the Lab's.
        assert_eq!(koru_i18n::current(), koru_i18n::Lang::En);

        // A search finds a record by its name in the language now shown.
        pick(&mut lab, &mut engine, Setting::Language, "ja");
        let japanese = koru_i18n::t(koru_i18n::Lang::Ja, record("T-SPACE").name);
        let english_name = koru_i18n::t(koru_i18n::Lang::En, record("T-SPACE").name);
        assert_ne!(japanese, english_name);
        press(&mut lab, &mut engine, SEARCH);
        assert!(lab.ime(&mut engine, ImeEvent::Commit { text: japanese }));
        assert!(lab.nav.rows().contains(&Row::Entry(record("T-SPACE"))));
        // Changing the language reads the gallery again: the same words no
        // longer find it by name once the names are English.
        pick(&mut lab, &mut engine, Setting::Language, "en");
        assert_eq!(lab.query(), japanese, "the search is kept");
        assert!(!lab.nav.rows().contains(&Row::Entry(record("T-SPACE"))));

        // A story that was changed keeps what it was given.
        lab.set_field(&mut engine, SEARCH, "");
        lab.set_field(&mut engine, field(), "Send");
        press(&mut lab, &mut engine, SUBJECT);
        pick(&mut lab, &mut engine, Setting::Language, "de");
        assert_eq!(story(&lab).label(), "Send");
        assert_eq!(story(&lab).presses(), 1);
        // And Reset then goes back to the sample it opened with.
        assert!(press(&mut lab, &mut engine, RESET));
        assert_eq!(story(&lab).presses(), 0);
    });
}
