// SPDX-License-Identifier: MPL-2.0

use super::*;

fn fonts() -> &'static [InstalledFont] {
    Box::leak(
        vec![
            InstalledFont {
                family: "Arial".into(),
                monospace: false,
            },
            InstalledFont {
                family: "Cascadia Code".into(),
                monospace: true,
            },
            InstalledFont {
                family: "Consolas".into(),
                monospace: true,
            },
            InstalledFont {
                family: "Gill Sans MT".into(),
                monospace: false,
            },
        ]
        .into_boxed_slice(),
    )
}

fn press(surface: &mut FontPickerSurface, engine: &mut TextEngine, name: NamedKey) -> Action {
    surface.key(engine, &Key::Named(name), None, ModifiersState::empty())
}

fn type_text(surface: &mut FontPickerSurface, engine: &mut TextEngine, text: &str) {
    for c in text.chars() {
        let s = c.to_string();
        surface.key(
            engine,
            &Key::Character(s.clone().into()),
            Some(&s),
            ModifiersState::empty(),
        );
    }
}

fn names(surface: &FontPickerSurface) -> Vec<Option<String>> {
    surface.rows().into_iter().map(|row| row.family).collect()
}

#[test]
fn only_monospaced_families_are_listed_until_all_are_asked_for() {
    let mut engine = TextEngine::new();
    let mut surface = FontPickerSurface::new(&mut engine, fonts(), None);
    assert_eq!(
        names(&surface),
        [None, Some("Cascadia Code".into()), Some("Consolas".into())]
    );
    assert!(matches!(
        press(&mut surface, &mut engine, NamedKey::Tab),
        Action::Keep
    ));
    assert_eq!(
        names(&surface).len(),
        5,
        "the default and all four families"
    );
}

#[test]
fn typing_filters_by_name_and_the_default_row_steps_aside() {
    let mut engine = TextEngine::new();
    let mut surface = FontPickerSurface::new(&mut engine, fonts(), None);
    press(&mut surface, &mut engine, NamedKey::Tab);
    type_text(&mut surface, &mut engine, "gill");
    assert_eq!(names(&surface), [Some("Gill Sans MT".into())]);
    match press(&mut surface, &mut engine, NamedKey::Enter) {
        Action::Choose(family) => assert_eq!(family, "Gill Sans MT"),
        _ => panic!("Enter chooses the first match"),
    }
}

#[test]
fn the_first_row_puts_the_agent_back_on_settings() {
    let mut engine = TextEngine::new();
    let mut surface = FontPickerSurface::new(&mut engine, fonts(), Some("Consolas".into()));
    assert_eq!(surface.list.selected, 2, "it opens on the family in force");
    press(&mut surface, &mut engine, NamedKey::ArrowUp);
    press(&mut surface, &mut engine, NamedKey::ArrowUp);
    assert!(matches!(
        press(&mut surface, &mut engine, NamedKey::Enter),
        Action::UseDefault
    ));
}

#[test]
fn escape_cancels_and_a_missing_match_chooses_nothing() {
    let mut engine = TextEngine::new();
    let mut surface = FontPickerSurface::new(&mut engine, fonts(), None);
    type_text(&mut surface, &mut engine, "zzz");
    assert!(names(&surface).is_empty());
    assert!(matches!(
        press(&mut surface, &mut engine, NamedKey::Enter),
        Action::Keep
    ));
    assert!(matches!(
        press(&mut surface, &mut engine, NamedKey::Escape),
        Action::Cancel
    ));
}

#[test]
fn a_click_on_a_row_chooses_it() {
    let mut engine = TextEngine::new();
    let mut surface = FontPickerSurface::new(&mut engine, fonts(), None);
    surface.set_viewport(400.0, 300.0);
    // The second row, below the header: the first family.
    let y = HEADER_H + ROW_H * 1.5;
    match surface.mouse_down(&mut engine, 100.0, y) {
        Action::Choose(family) => assert_eq!(family, "Cascadia Code"),
        _ => panic!("a click chooses"),
    }
}

fn own_profile() -> koru_session_protocol::SessionProfileKey {
    koru_session_protocol::SessionProfileKey::Custom(koru_session_protocol::SessionProfileId::mint())
}

#[test]
fn a_chosen_family_reaches_that_agents_terminal_and_nobody_elses() {
    let mine = own_profile();
    let other = own_profile();
    let token = crate::terminal_prefs::family_token("JetBrains Mono");
    assert_eq!(token, "family:JetBrains Mono");
    assert!(crate::terminal_prefs::set_agent_look(
        &mine,
        None,
        Some(Some(&token))
    ));
    let stack = crate::terminal_prefs::appearance_for(Some(&mine))
        .custom_stack
        .expect("an installed family");
    assert!(stack.starts_with("JetBrains Mono, "));
    assert_eq!(
        crate::terminal_prefs::appearance_for(Some(&other)).custom_stack,
        None
    );
    crate::terminal_prefs::set_agent_look(&mine, None, Some(None));
}

#[test]
fn a_family_this_machine_does_not_have_follows_settings() {
    let profile = own_profile();
    crate::terminal_prefs::set_agent_look(&profile, None, Some(Some("family:No Such Face 9000")));
    assert_eq!(
        crate::terminal_prefs::appearance_for(Some(&profile)),
        crate::terminal_prefs::appearance()
    );
    crate::terminal_prefs::set_agent_look(&profile, None, Some(None));
}

#[test]
fn the_picker_opens_over_a_terminal_tab_and_closes_on_escape() {
    use crate::mainview::harness::*;
    let (mut v, _workspace) = container();
    assert!(v.open_session_tab_for_profile(0, None));
    let tab = v.tabs().active();
    assert!(v.open_font_picker(tab));
    assert!(v.font_picker.is_some());
    assert!(v.font_picker_key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty()));
    assert!(v.font_picker.is_none());
    // A document tab has no terminal font to choose.
    let (mut v, _workspace) = container();
    assert!(!v.open_font_picker(0));
}
