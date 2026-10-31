// SPDX-License-Identifier: MPL-2.0

//! **Every row that existed still behaves the way it did**, now that it lives
//! on a page in a card.
//!
//! A row-by-row list on purpose: a summary test that asserted "the page draws"
//! would pass with a control missing.

use crate::action::FontRole;
use crate::page::Block;
use crate::tests::{
    GLOBALS, flip, goto, is_active, koru, labels, lines, page_lines, press, press_rect, rect_of,
    surface, swatches, switch_on,
};
use crate::{Availability, PageId, Request, Sections, VoiceFacts};
use koru_draw::theme;
use koru_settings::{
    CloseBehaviorChoice, DictationGesture, DictationInput, DictationLanguage, DictationTier,
    GlobalSettings,
};

/// The names every setting is found under. If one disappears, a setting
/// disappeared with it.
const ROWS: [&str; 16] = [
    "Theme",
    "Density",
    "Pinned tabs",
    "App font",
    "Body",
    "Headings",
    "Code",
    "Color",
    "Shape",
    "Page width for new documents",
    "apply to all",
    "Microphone",
    "Speech model",
    "Start Koru when you log in",
    "When the last window closes",
    "Voice input",
];

#[test]
fn every_setting_is_still_drawn_somewhere() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let surface = koru();
    let drawn = lines(&surface);
    for row in ROWS {
        assert!(
            drawn.iter().any(|line| line == row),
            "the `{row}` row is gone: {drawn:?}"
        );
    }
    for role in FontRole::ALL {
        assert!(
            drawn
                .iter()
                .any(|line| line == role.row_label(koru_i18n::Lang::En)),
            "the `{}` picker is gone",
            role.label()
        );
    }
}

#[test]
fn the_first_minutes_link_opens_the_voice_card() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.set_viewport(1000.0, 400.0);
    assert_eq!(surface.page(), PageId::Appearance);
    assert!(surface.scroll_to_voice());
    assert_eq!(surface.page(), PageId::Editor);
    assert!(surface.scroll > 0.0);
    let anchor = surface
        .laid
        .anchors
        .first()
        .map(|(_, y)| *y)
        .expect("the voice anchor");
    // The card's title is at the top of the page area.
    let title_top = anchor - crate::Card::TITLE_H - crate::Card::TITLE_GAP;
    assert!((title_top - surface.scroll - crate::page::TOP).abs() < 0.01);
}

// ------------------------------------------------ the live-global rows persist
//
// Each of these writes the same process global welcome wrote, which is what the
// shell's per-field patch reads.

#[test]
fn the_theme_tiles_move_the_live_theme() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let was = theme::theme_preference();
    let mut surface = koru();
    for (label, expected) in [
        ("Dark", theme::ThemePreference::Dark),
        ("Light", theme::ThemePreference::Light),
        ("System", theme::ThemePreference::System),
    ] {
        assert!(press(&mut surface, label));
        assert_eq!(theme::theme_preference(), expected, "{label}");
        surface.relayout();
        assert!(is_active(&surface, label), "{label} reads as chosen");
    }
    theme::set_theme_preference(was);
}

#[test]
fn the_caret_shape_moves_the_live_caret() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let (was_w, was_h) = (theme::caret_width(), theme::caret_height());
    let mut surface = koru();
    assert!(press(&mut surface, "Thick"));
    assert_eq!(theme::caret_width(), theme::CaretWidth::Thick);
    assert!(press(&mut surface, "Tall"));
    assert_eq!(theme::caret_height(), theme::CaretHeight::Tall);
    theme::set_caret_width(was_w);
    theme::set_caret_height(was_h);
}

/// The swatches carry no words, so they are pressed by their rect.
#[test]
fn the_caret_color_swatches_move_the_live_color() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let was = theme::caret_color();
    let mut surface = koru();
    surface.show_page(PageId::Appearance);
    surface.relayout();
    let rects = swatches(&surface, "Color");
    assert_eq!(rects.len(), theme::CaretColor::ALL.len());
    for (index, color) in theme::CaretColor::ALL.into_iter().enumerate() {
        assert!(press_rect(&mut surface, rects[index]));
        assert_eq!(theme::caret_color(), color);
        surface.relayout();
    }
    theme::set_caret_color(was);
}

#[test]
fn the_density_row_moves_the_live_density() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let was = theme::density();
    let mut surface = koru();
    assert!(press(&mut surface, "Comfortable"));
    assert_eq!(theme::density(), theme::Density::Comfortable);
    assert!(press(&mut surface, "Compact"));
    assert_eq!(theme::density(), theme::Density::Compact);
    theme::set_density(was);
}

/// Each font role is its own dropdown; choosing in one moves that role only.
#[test]
fn every_font_picker_moves_its_own_role_and_no_other() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let before: Vec<_> = FontRole::ALL.iter().map(|role| role.current()).collect();
    let mut surface = koru();
    surface.show_page(PageId::Appearance);
    let wanted = theme::FontChoice::Lora;
    for (index, role) in FontRole::ALL.into_iter().enumerate() {
        surface.relayout();
        let field = surface.laid.dropdowns[index].field;
        assert!(press_rect(&mut surface, field));
        let open = surface.open.as_ref().expect("the list is open");
        let item = open
            .menu
            .items()
            .iter()
            .position(|option| option.label == wanted.label())
            .expect("every role offers every family");
        let (x, y) = open.menu.item_rect(surface.viewport, item).centre();
        assert!(surface.mouse_down(x, y));
        assert_eq!(role.current(), wanted, "{role:?}");
        for (other, was) in FontRole::ALL.into_iter().zip(&before).skip(index + 1) {
            assert_eq!(other.current(), *was, "{other:?} moved with {role:?}");
        }
    }
    for (role, was) in FontRole::ALL.into_iter().zip(before) {
        role.set(was);
    }
}

/// Font options are set in their own face — the point of a font picker.
#[test]
fn every_font_option_is_set_in_its_own_face() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.show_page(PageId::Appearance);
    surface.relayout();
    for (_, option) in &surface.laid.dropdowns[0].options {
        let family = theme::FontChoice::ALL
            .iter()
            .find(|f| f.label() == option.label)
            .expect("a family");
        assert_eq!(option.font_stack, Some(family.stack()));
    }
}

#[test]
fn the_page_width_row_moves_the_live_default_view_mode() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let was = theme::default_view_mode();
    let mut surface = koru();
    assert!(press(&mut surface, "Narrow"));
    assert_eq!(theme::default_view_mode(), theme::ViewMode::Narrow);
    assert_eq!(surface.view_mode(), theme::ViewMode::Narrow);
    theme::set_default_view_mode(was);
}

/// The row follows the live default, not a stale copy of `Settings`.
#[test]
fn the_page_width_row_follows_the_live_default_not_a_stale_field() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let was = theme::default_view_mode();
    theme::set_default_view_mode(theme::ViewMode::Page);
    let mut surface = koru();
    surface.adopt_preferences(
        theme::ViewMode::Full,
        None,
        false,
        CloseBehaviorChoice::default(),
    );
    surface.show_page(PageId::Editor);
    surface.relayout();
    assert!(
        is_active(&surface, "Page"),
        "a stale Full copy must not restyle the row"
    );
    assert!(!is_active(&surface, "Full"));
    theme::set_default_view_mode(was);
}

/// The page background is a Page concept: in Narrow or Full it would be a
/// control that does nothing, so it is absent rather than dead.
#[test]
fn the_page_background_is_offered_only_in_page_mode() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let was = theme::default_view_mode();
    let mut surface = koru();
    surface.show_page(PageId::Editor);
    theme::set_default_view_mode(theme::ViewMode::Page);
    surface.relayout();
    assert_eq!(swatches(&surface, "Page background").len(), 1);
    theme::set_default_view_mode(theme::ViewMode::Full);
    surface.relayout();
    assert!(swatches(&surface, "Page background").is_empty());
    theme::set_default_view_mode(was);
}

// -------------------------------------------------------- the host-drained rows

#[test]
fn the_pinned_tabs_row_moves_the_value_the_host_captures() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    assert!(press(&mut surface, "Icon and title"));
    assert!(surface.pinned_show_title());
    assert!(press(&mut surface, "Icon only"));
    assert!(!surface.pinned_show_title());
}

#[test]
fn the_closing_row_asks_the_host_to_record_the_choice() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    assert!(press(&mut surface, "Quit"));
    assert_eq!(
        surface.take_requests(),
        vec![Request::SetCloseBehavior(CloseBehaviorChoice::Quit)]
    );
    assert_eq!(surface.close_behavior(), CloseBehaviorChoice::Quit);
    // Choosing what is already chosen asks for nothing.
    assert!(press(&mut surface, "Quit"));
    assert!(surface.take_requests().is_empty());
    // And the sentence under the row says what the choice means.
    surface.relayout();
    assert!(
        page_lines(&surface)
            .iter()
            .any(|line| line.starts_with("Koru quits"))
    );
}

#[test]
fn the_page_background_asks_the_host_to_cycle_it() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    let was = theme::default_view_mode();
    theme::set_default_view_mode(theme::ViewMode::Page);
    surface.show_page(PageId::Editor);
    surface.relayout();
    let rect = swatches(&surface, "Page background")[0];
    assert!(press_rect(&mut surface, rect));
    assert_eq!(
        surface.take_requests(),
        vec![Request::CycleCanvasBackground]
    );
    surface.set_canvas_background(Some([1, 2, 3, 255]));
    assert_eq!(surface.canvas_background(), Some([1, 2, 3, 255]));
    theme::set_default_view_mode(was);
}

#[test]
fn clearing_the_personal_dictionary_is_a_host_request() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    assert!(press(&mut surface, "Clear…"));
    assert_eq!(
        surface.take_requests(),
        vec![Request::ClearPersonalDictionary]
    );
}

#[test]
fn the_spelling_switch_asks_the_host_and_hides_the_language_while_off() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.set_spelling(
        true,
        koru_settings::LanguageChoice::default(),
        vec![(
            koru_settings::LanguageChoice::default(),
            "Automatic".to_owned(),
        )],
        None,
    );
    assert!(goto(&mut surface, "Automatic"), "the language row is shown");
    assert!(flip(&mut surface, "Check spelling while typing"));
    assert_eq!(
        surface.take_requests(),
        vec![Request::SetSpellcheckEnabled(false)]
    );
    surface.relayout();
    assert!(
        rect_of(&surface, "Automatic").is_none(),
        "no language to choose for a check that is off"
    );
}

// -------------------------------------------------------------------- voice

#[test]
fn every_voice_row_asks_the_host_to_perform_it() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    for (label, expected) in [
        (
            "English",
            Request::SetVoiceLanguage(DictationLanguage::parse("en")),
        ),
        (
            "Yeti Nano",
            Request::SetVoiceInput(DictationInput::parse("Yeti Nano")),
        ),
        (
            "High accuracy",
            Request::SetVoiceTier(DictationTier::HighAccuracy),
        ),
        ("Toggle", Request::SetVoiceGesture(DictationGesture::Toggle)),
        ("Remove", Request::RemoveVoiceModels),
        ("Revoke", Request::RevokeVoiceConsent),
    ] {
        assert!(press(&mut surface, label), "{label}");
        assert_eq!(surface.take_requests(), vec![expected], "{label}");
    }
}

/// The model button is derived from the host's reading, never from a stored
/// flag: the same press installs when nothing is installed.
#[test]
fn the_model_button_installs_when_nothing_is_installed() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    let mut facts = crate::tests::voice_facts();
    facts.model_line = "Not installed — dictation offers the download".to_owned();
    facts.model_action = "Install…".to_owned();
    facts.model_removes = false;
    surface.set_voice_facts(facts);
    assert!(press(&mut surface, "Install…"));
    assert_eq!(surface.take_requests(), vec![Request::InstallVoiceModels]);
}

/// A switched-off installation says so rather than drawing controls that lead
/// nowhere.
#[test]
fn a_switched_off_installation_draws_the_sentence_and_no_controls() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    let mut facts = crate::tests::voice_facts();
    facts.blocked =
        Some("Voice input is switched off on this installation (KORU_VOICE_DISABLED).".to_owned());
    surface.set_voice_facts(facts);
    let drawn = lines(&surface);
    assert!(
        drawn
            .iter()
            .any(|line| line.contains("KORU_VOICE_DISABLED"))
    );
    assert!(
        !goto(&mut surface, "High accuracy"),
        "no quality picker on a switched-off installation"
    );
    assert!(!goto(&mut surface, "Remove"));
}

/// A host that has said nothing about dictation draws no voice card at all.
#[test]
fn a_host_that_says_nothing_about_voice_gets_no_voice_card() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = surface(Sections::koru());
    surface.set_voice_facts(VoiceFacts::default());
    assert!(!lines(&surface).iter().any(|line| line == "Voice input"));
}

/// Revoking is a promise being withdrawn, so the row exists only while there is
/// one to withdraw.
#[test]
fn the_revoke_row_is_absent_without_a_consent_record() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    let mut facts = crate::tests::voice_facts();
    facts.consent_granted = false;
    surface.set_voice_facts(facts);
    assert!(!goto(&mut surface, "Revoke"));
}

// ------------------------------------------------------------- start on login

/// The one Global row this surface patches itself: it has no process global and
/// no window mirror, so there is nothing else that could carry it.
#[test]
fn start_on_login_patches_the_field_and_nothing_else() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = crate::SettingsSurface::with_links(
        Sections::koru(),
        crate::SettingsLinks::inert(true, false),
    );
    surface.set_viewport(1000.0, 700.0);
    surface.set_voice_facts(crate::tests::voice_facts());
    surface.set_renderer(crate::tests::renderer_row());
    assert!(flip(&mut surface, "Start Koru when you log in"));
    assert!(surface.global.start_on_login, "applied at once");
    let outstanding = surface
        .links
        .take_outstanding_global()
        .expect("the patch was sent");
    assert_eq!(outstanding.start_on_login, Some(true));
    assert!(
        !outstanding.touches_roaming(),
        "a start-on-login patch never restages preferences.conf"
    );
}

/// The switch draws what the authority says, not what this window last pressed.
#[test]
fn start_on_login_draws_the_authoritative_value() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    assert!(!switch_on(&mut surface, "Start Koru when you log in"));
    surface.adopt_global(&GlobalSettings {
        start_on_login: true,
        ..GlobalSettings::default()
    });
    assert!(switch_on(&mut surface, "Start Koru when you log in"));
}

/// Dimmed, but still there: a read-only surface shows the last authoritative
/// values rather than blanking or guessing.
#[test]
fn a_read_only_surface_still_shows_its_values() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.adopt_global(&GlobalSettings {
        start_on_login: true,
        ..GlobalSettings::default()
    });
    surface.set_availability(Availability::Down);
    assert!(switch_on(&mut surface, "Start Koru when you log in"));
}

/// A missing dictionary says what is missing and offers the download; while
/// it runs there is nothing to press.
#[test]
fn a_missing_dictionary_offers_its_download() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.set_dictionary(Some(crate::DictionaryRow {
        line: "The English (US) dictionary is missing (0.6 MB).".to_owned(),
        action: Some("Download".to_owned()),
    }));
    assert!(press(&mut surface, "Download"));
    assert_eq!(surface.take_requests(), vec![Request::DownloadDictionary]);

    surface.set_dictionary(Some(crate::DictionaryRow {
        line: "Downloading the English (US) dictionary — 0.3 of 0.6 MB".to_owned(),
        action: None,
    }));
    assert!(lines(&surface).iter().any(|l| l.starts_with("Downloading")));
    assert!(!goto(&mut surface, "Download"));

    surface.set_dictionary(None);
    assert!(!lines(&surface).iter().any(|l| l == "Dictionary"));
}

/// *Apply to all* is a link ending the page-width sentence: pressing it asks
/// the host, and the link then says it was done.
#[test]
fn the_apply_to_all_link_asks_the_host_and_then_says_so() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    assert!(press(&mut surface, "apply to all"));
    assert_eq!(
        surface.take_requests(),
        vec![Request::ApplyLayoutToAllDocuments]
    );
    surface.relayout();
    assert!(
        page_lines(&surface)
            .iter()
            .any(|l| l == "applied to all documents")
    );
}

/// The personal dictionary's row says how many words it holds.
#[test]
fn the_personal_dictionary_counts_its_words() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.show_page(PageId::Editor);
    for (words, line) in [
        (Some(0), "Empty."),
        (Some(1), "1 word you've told Koru to accept."),
        (Some(12), "12 words you've told Koru to accept."),
    ] {
        surface.set_personal_dictionary_words(words);
        surface.relayout();
        assert!(page_lines(&surface).iter().any(|l| l == line), "{line}");
    }
    assert!(labels(&surface).iter().any(|l| l == "Clear…"));
}

/// The voice shortcut's chord sits in a key chip beside the row's name.
#[test]
fn the_voice_shortcut_wears_its_chord_as_a_key_chip() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.show_page(PageId::Editor);
    surface.relayout();
    let chip = surface.laid.blocks.iter().find_map(|block| match block {
        Block::Card(card) => card
            .rows
            .iter()
            .find(|row| row.label == "Shortcut")
            .and_then(|row| row.chip.clone()),
        _ => None,
    });
    assert!(chip.is_some_and(|chord: String| !chord.is_empty()));
}

/// Advanced is folded to one line saying what is in it; *Show* unfolds it and
/// *Hide* folds it again.
#[test]
fn advanced_folds_to_a_summary_and_unfolds_on_show() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.show_page(PageId::System);
    surface.relayout();
    assert!(page_lines(&surface).iter().any(
        |l| l.starts_with("Graphics renderer (") && l.contains("Nothing here needs attention")
    ));
    assert!(press(&mut surface, "Show"));
    surface.relayout();
    assert!(labels(&surface).iter().any(|l| l == "Vulkan"));
    assert!(press(&mut surface, "Hide"));
    surface.relayout();
    assert!(!labels(&surface).iter().any(|l| l == "Vulkan"));
    assert!(surface.take_requests().is_empty(), "folding writes nothing");
}

// ---------------------------------------------------------------- privacy

/// **The withdrawal the opt-in contract requires** (2026-10-11): a switch on
/// the System page whenever the host has a record to show, which moves at once
/// and leaves the write to the host.
#[test]
fn the_usage_data_switch_shows_the_record_and_asks_the_host_to_change_it() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    let label = "Share anonymous usage data";
    assert!(
        !lines(&surface).iter().any(|line| line == "Privacy"),
        "no record read, no Privacy card"
    );

    assert!(surface.set_telemetry_consent(Some(true)));
    assert!(!surface.set_telemetry_consent(Some(true)), "nothing moved");
    assert!(switch_on(&mut surface, label));
    assert!(flip(&mut surface, label));
    assert_eq!(
        surface.take_requests(),
        vec![Request::SetTelemetryConsent(false)],
        "withdrawing is the host's to perform, off the event loop"
    );
    assert!(
        !switch_on(&mut surface, label),
        "the switch moves under the hand"
    );

    // Unanswered draws off, and turning it on asks for a yes.
    surface.set_telemetry_consent(Some(false));
    assert!(flip(&mut surface, label));
    assert_eq!(
        surface.take_requests(),
        vec![Request::SetTelemetryConsent(true)]
    );

    // A host whose record says something else puts it back.
    assert!(switch_on(&mut surface, label), "moved to on under the hand");
    surface.set_telemetry_consent(Some(false));
    assert!(
        !switch_on(&mut surface, label),
        "and back to what the record holds"
    );
}

#[test]
fn the_usage_data_switch_lives_on_the_system_page_and_is_found_by_search() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.set_telemetry_consent(Some(false));
    surface.show_page(PageId::System);
    surface.relayout();
    assert!(
        page_lines(&surface).iter().any(|line| line == "Privacy"),
        "the card is titled Privacy, on the System page"
    );
    assert_eq!(
        crate::tests::switch(&surface, "Share anonymous usage data").map(|(_, on)| on),
        Some(false)
    );
    for word in ["privacy", "telemetry", "usage"] {
        assert!(
            PageId::System.matches(koru_i18n::Lang::En, word),
            "a search for `{word}` keeps the System page"
        );
    }
}

// ---------------------------------------------------------------- app menu

/// Linux's *Show Koru in the app menu*: drawn only when the host has a reading,
/// moved at once, and written by the host, whose reading puts it back if the
/// file did not change.
#[test]
fn the_app_menu_switch_shows_the_file_and_asks_the_host_to_change_it() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    let label = "Show Koru in the app menu";
    surface.show_page(PageId::System);
    surface.relayout();
    assert_eq!(
        crate::tests::switch(&surface, label),
        None,
        "no app menu on this platform, no row"
    );

    assert!(surface.set_app_menu_entry(Some(false)));
    assert!(!surface.set_app_menu_entry(Some(false)), "nothing moved");
    surface.relayout();
    assert!(!switch_on(&mut surface, label));
    assert!(flip(&mut surface, label));
    assert_eq!(
        surface.take_requests(),
        vec![Request::SetAppMenuEntry(true)]
    );
    assert!(
        switch_on(&mut surface, label),
        "the switch moves under the hand"
    );

    // The file was not written: the host's reading puts it back.
    surface.set_app_menu_entry(Some(false));
    assert!(!switch_on(&mut surface, label));
    for word in ["app menu", "launcher", "dock"] {
        assert!(
            PageId::System.matches(koru_i18n::Lang::En, word),
            "a search for `{word}` keeps the System page"
        );
    }
}
