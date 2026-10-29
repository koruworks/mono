// SPDX-License-Identifier: MPL-2.0

use koru_session_protocol::{SessionProfileId, SessionProfileKey};
use koru_workspace_host::{Settings, TerminalLook};

use super::super::context_menu::{self, MenuAction, MenuSubject};
use super::super::harness::*;
use super::*;
use crate::terminal_prefs;

/// A profile no other test touches: the looks are process-wide, and tests run
/// in parallel.
fn own_profile() -> SessionProfileKey {
    SessionProfileKey::Custom(SessionProfileId::mint())
}

#[test]
fn an_agent_with_no_look_follows_settings_and_a_chosen_half_overrides_only_itself() {
    let profile = own_profile();
    let settings = terminal_prefs::appearance();
    assert_eq!(terminal_prefs::appearance_for(Some(&profile)), settings);
    assert_eq!(terminal_prefs::appearance_for(None), settings);

    assert!(terminal_prefs::set_agent_look(
        &profile,
        Some(Some("light")),
        None
    ));
    let light = terminal_prefs::appearance_for(Some(&profile));
    assert!(!light.dark, "the theme half is this agent's");
    assert_eq!(
        light.font, settings.font,
        "the font half still follows Settings"
    );
    assert_eq!(light.zoom, settings.zoom, "zoom is not a per-agent choice");

    assert!(terminal_prefs::set_agent_look(
        &profile,
        Some(Some("dark")),
        None
    ));
    assert!(terminal_prefs::appearance_for(Some(&profile)).dark);

    // Back to Settings, and nothing is left filed.
    assert!(terminal_prefs::set_agent_look(&profile, Some(None), None));
    assert_eq!(terminal_prefs::appearance_for(Some(&profile)), settings);
    assert!(!terminal_prefs::agent_looks().contains_key(&terminal_prefs::agent_key(&profile)));
    assert!(
        !terminal_prefs::set_agent_look(&profile, Some(None), None),
        "a choice that changes nothing reports nothing"
    );
}

#[test]
fn a_font_the_build_does_not_offer_reads_as_follow_settings() {
    let profile = own_profile();
    let settings = terminal_prefs::appearance();
    terminal_prefs::set_agent_look(&profile, None, Some(Some("no-such-font")));
    assert_eq!(
        terminal_prefs::appearance_for(Some(&profile)).font,
        settings.font,
        "an unknown token is ignored, not a failure"
    );
    // A proportional family is not a terminal font either.
    terminal_prefs::set_agent_look(&profile, None, Some(Some("inter")));
    assert_eq!(
        terminal_prefs::appearance_for(Some(&profile)).font,
        settings.font
    );
    terminal_prefs::set_agent_look(&profile, None, Some(None));
}

#[test]
fn the_menu_offers_theme_and_font_on_a_session_tab_only() {
    let with = context_menu::tab_items_for(false, false, false, false, None, true);
    let theme = with.iter().position(|a| *a == MenuAction::TerminalTheme);
    let font = with.iter().position(|a| *a == MenuAction::TerminalFont);
    assert!(theme.is_some() && font == theme.map(|i| i + 1), "{with:?}");
    assert_eq!(
        with.last(),
        Some(&MenuAction::PopOut),
        "the window verb stays last"
    );
    let without = context_menu::tab_items_for(true, false, false, false, None, false);
    assert!(!without.contains(&MenuAction::TerminalTheme));
    assert!(!without.contains(&MenuAction::TerminalFont));
    assert!(with.contains(&MenuAction::SessionTerminate));
    assert!(!without.contains(&MenuAction::SessionTerminate));
}

#[test]
fn the_flyouts_list_light_and_dark_and_only_the_monospaced_families() {
    let theme = context_menu::submenu(MenuAction::TerminalTheme).expect("a flyout");
    assert!(theme.contains(&MenuAction::TerminalThemeIs(None)));
    assert!(theme.contains(&MenuAction::TerminalThemeIs(Some("light"))));
    assert!(theme.contains(&MenuAction::TerminalThemeIs(Some("dark"))));

    let font = context_menu::submenu(MenuAction::TerminalFont).expect("a flyout");
    assert!(font.contains(&MenuAction::TerminalFontIs(None)));
    let families: Vec<_> = font
        .iter()
        .filter_map(|action| match action {
            MenuAction::TerminalFontIs(Some(token)) => Some(*token),
            _ => None,
        })
        .collect();
    assert!(!families.is_empty());
    for token in families {
        assert!(
            koru_settings::FontFamily::ALL
                .iter()
                .any(|family| family.is_mono() && family.token() == token),
            "{token} is a monospaced family"
        );
    }
}

#[test]
fn the_current_choice_is_checked_in_its_flyout() {
    context_menu::set_terminal_look_snapshot((Some("dark".into()), None));
    assert!(
        MenuAction::TerminalThemeIs(Some("dark"))
            .label()
            .contains('\u{2713}')
    );
    assert!(
        !MenuAction::TerminalThemeIs(Some("light"))
            .label()
            .contains('\u{2713}')
    );
    assert!(
        MenuAction::TerminalFontIs(None)
            .label()
            .contains('\u{2713}'),
        "following Settings is the current font"
    );
    assert!(
        MenuAction::TerminalThemeIs(None)
            .label()
            .contains("Default")
    );
    context_menu::set_terminal_look_snapshot((None, None));
}

/// The whole path: a menu choice on one agent's tab, kept for every tab of that
/// agent, written into the profile file at capture, and read back at restore —
/// and a different agent unaffected.
#[test]
fn a_choice_on_a_tab_is_kept_per_agent_and_survives_a_restart() {
    let (mut v, _workspace) = container();
    assert!(v.open_session_tab_for_profile(0, None));
    let tab = v.tabs().active();
    let profile = v.session_profile_of_tab(tab).expect("a session tab");
    let other = own_profile();

    assert!(v.run_menu_action_for_test(
        MenuSubject::Tab(tab),
        MenuAction::TerminalThemeIs(Some("light"))
    ));
    assert!(v.run_menu_action_for_test(MenuSubject::Tab(tab), MenuAction::TerminalFontIs(None)));
    assert_eq!(
        terminal_prefs::agent_look(&profile).0.as_deref(),
        Some("light")
    );
    assert_eq!(
        terminal_prefs::agent_look(&other),
        (None, None),
        "another agent is unaffected"
    );

    // A document tab has no terminal to style.
    assert!(!v.run_menu_action_for_test(
        MenuSubject::Tab(0),
        MenuAction::TerminalThemeIs(Some("dark"))
    ));

    // Captured into the profile file…
    let mut settings = Settings::default();
    v.capture_session(&mut settings);
    let filed = settings
        .terminal_looks
        .get(&terminal_prefs::agent_key(&profile))
        .cloned();
    assert_eq!(
        filed,
        Some(TerminalLook {
            theme: Some("light".into()),
            font: None
        })
    );

    // …and read back by a window that has never heard of it.
    terminal_prefs::set_agent_look(&profile, Some(None), None);
    assert_eq!(terminal_prefs::agent_look(&profile), (None, None));
    let ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("create");
    let workspace = koru_workspace::Workspace::new(ws);
    let mut fresh = MainView::new();
    fresh.restore_session(&workspace, &settings);
    assert_eq!(
        terminal_prefs::agent_look(&profile).0.as_deref(),
        Some("light")
    );
    terminal_prefs::set_agent_look(&profile, Some(None), None);
}
