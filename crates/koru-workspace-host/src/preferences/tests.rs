// SPDX-License-Identifier: MPL-2.0

use super::*;

use koru_settings::{DensityChoice, TerminalCursorWidth};

/// **What a window shows before it has heard anything.**
///
/// The registry's defaults and `Settings::default()` are two spellings of one
/// answer, and they have to agree exactly: the first frame is drawn from the
/// second, and the authority — when it answers at all — answers with the first.
/// A drift would show as the window quietly restyling itself a moment after it
/// opened, for somebody who had never chosen anything.
#[test]
fn the_startup_defaults_are_the_registrys_defaults() {
    let mut settings = Settings::default();
    assert!(
        !adopt(&GlobalSettings::default(), &mut settings),
        "a fresh window is already showing what the authority would tell it"
    );
    assert_eq!(settings, Settings::default());
}

/// The corollary: with nothing moved, there is nothing to say. This is what
/// keeps the idle tick from patching a value nobody touched — the write storm
/// the old whole-body writer had by construction.
#[test]
fn an_untouched_window_sends_no_patch() {
    let authoritative = GlobalSettings::default();
    let settings = Settings::default();
    assert!(patch(&authoritative, &settings).is_empty());
}

/// **A patch names the field that moved and nothing else.** That is the whole
/// of what makes two windows changing two settings both land: the authority
/// re-reads under its lock and folds only what it was given.
#[test]
fn a_patch_names_only_what_moved() {
    let authoritative = GlobalSettings::default();
    let settings = Settings {
        theme: ThemePreference::Dark,
        ..Settings::default()
    };
    let patch = patch(&authoritative, &settings);
    assert_eq!(patch.theme, Some(ThemeChoice::Dark));
    assert!(patch.density.is_none());
    assert!(patch.caret_color.is_none());
    assert!(patch.dictation_gesture.is_none());
    assert!(patch.zoom.is_none());
}

/// Every value a window holds survives the round trip through the section, in
/// both directions. One test rather than eighteen, because the failure mode is
/// a *field* that was forgotten in one of the two conversions — and a forgotten
/// field is a setting that silently stops persisting.
#[test]
fn every_preference_round_trips_through_the_section() {
    let settings = Settings {
        icon_pack: koru_draw::icons::IconPack::Lucide,
        theme: ThemePreference::Light,
        caret_color: CaretColor::Violet,
        caret_width: CaretWidth::Thick,
        caret_height: CaretHeight::Tall,
        font: FontChoice::Poppins,
        body_font: FontChoice::SourceSerif4,
        headings_font: FontChoice::Lora,
        code_font: FontChoice::SourceCodePro,
        density: Density::Comfortable,
        zoom: 1.25,
        view_mode: ViewMode::Page,
        canvas_bg: Some([0x12, 0x34, 0x56, 0xFF]),
        pinned_show_title: true,
        popout_close: crate::settings::PopoutClose::RestoreTab,
        emoji_skin_tone: "medium-dark".into(),
        close_behavior: koru_window::CloseBehavior::Quit,
        dictation_toggle: true,
        dictation_language: koru_voice::Requested::Fixed(koru_voice::VoiceLanguage::PtBr),
        dictation_tier: koru_voice_models::ModelTier::HighAccuracy,
        // **A real device name, with a space and a bracket in it** — hardware
        // is called `Microphone Array (Realtek(R) Audio)`, and the round trip
        // has to survive that rather than a tidy token.
        dictation_input: koru_voice::InputChoice::named("Microphone Array (Realtek(R) Audio)"),
        ..Settings::default()
    };

    // Out: a full patch, since the authority still holds the defaults.
    let mut section = GlobalSettings::default();
    patch(&section, &settings).apply(&mut section);

    // Back in: the same window, wound back to nothing, told what is stored.
    let mut back = Settings::default();
    assert!(adopt(&section, &mut back));
    assert_eq!(back, settings);
}

/// **The zoom write storm.** `7 * 0.1f32` and the `0.70` read back out of the
/// file are different `f32`s, so comparing them with `!=` made every idle tick
/// look like a change and patched a value nobody had touched. The comparison is
/// the persisted spelling, which is the only one that is stable.
#[test]
fn a_zoom_that_only_differs_below_the_stored_precision_is_not_a_change() {
    let mut section = GlobalSettings::default();
    section.zoom = koru_settings::Zoom::new(0.7).expect("in range");
    let settings = Settings {
        // What `clamp_zoom`'s snap actually produces for seven steps out.
        zoom: 7.0 * 0.1_f32,
        ..Settings::default()
    };
    assert!(
        patch(&section, &settings).zoom.is_none(),
        "0.70 and 7 x 0.1 are the same stored zoom"
    );

    let moved = Settings {
        zoom: 0.8,
        ..Settings::default()
    };
    assert_eq!(patch(&section, &moved).zoom, Some(0.8));
}

/// **Follow-the-app is the authority's state, never this window's.** A window
/// only ever reports the family it is showing, so a role sitting on the current
/// default is left alone rather than being reported as a choice — and a role
/// the person moved *to* the default is reported, because they chose it.
#[test]
fn a_role_that_follows_the_app_is_not_reported_as_a_choice() {
    let section = GlobalSettings::default();
    let following = Settings::default();
    assert!(patch(&section, &following).headings_font.is_none());

    let chose = Settings {
        headings_font: FontChoice::Lora,
        ..Settings::default()
    };
    assert_eq!(
        patch(&section, &chose).headings_font,
        Some(FontSetting::Chosen(FontFamily::Lora))
    );

    // And the other way: a role the authority says follows the app resolves to
    // *this build's* default, not to whatever was frozen into a file.
    let mut settings = Settings {
        headings_font: FontChoice::Lora,
        ..Settings::default()
    };
    adopt(&section, &mut settings);
    assert_eq!(
        settings.headings_font,
        koru_draw::theme::default_headings_font()
    );
}

/// A caret width a file may carry but no build writes (`4x`, `8x`) still
/// survives both conversions — the tolerant-read asymmetry the authority
/// inherited, seen from this side.
#[test]
fn the_read_only_caret_widths_still_round_trip() {
    for width in [CaretWidth::X4, CaretWidth::X8] {
        let settings = Settings {
            caret_width: width,
            ..Settings::default()
        };
        let mut section = GlobalSettings::default();
        patch(&section, &settings).apply(&mut section);
        let mut back = Settings::default();
        adopt(&section, &mut back);
        assert_eq!(back.caret_width, width, "{width:?}");
    }
}

/// **Sending a patch is not sending it twice.** The link takes its own patch as
/// the current answer, so the next tick sees nothing moved; the authority's
/// reply then replaces it, which is how a refusal corrects rather than
/// compounds. This is the pairing of *this* window's `patch` with the shared
/// link — the link's own contract is proved in `koru-api-client`.
#[test]
fn a_sent_patch_is_not_sent_again_on_the_next_tick() {
    let mut link = koru_api_client::GlobalLink::disabled();
    let settings = Settings {
        density: Density::Compact,
        ..Settings::default()
    };
    let first = patch(link.authoritative(), &settings);
    assert_eq!(first.density, Some(DensityChoice::Compact));
    link.save(first, |patch, section| patch.clone().apply(section));
    assert!(patch(link.authoritative(), &settings).is_empty());
}

/// **`preferences.conf` has one reader and one writer, and it is not this
/// crate.** The migration below is the single exception and names itself; a new
/// `fs::read`/`fs::write` of that file anywhere here would be a fourth parser
/// coming back, which is exactly what SP-6 removed.
#[test]
fn nothing_in_this_crate_opens_the_preferences_file_except_the_one_migration() {
    let settings = include_str!("../settings.rs");
    let reads = settings.matches("PREFERENCES_FILE)").count();
    assert_eq!(
        reads, 1,
        "exactly one read of preferences.conf survives — the relocation of \
         `emoji_recents` and `last_export_dir`, which is deletable once no \
         install predates the move"
    );
    let callers: Vec<&str> = settings
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("let Some(text) = preferences_path()"))
        .collect();
    assert_eq!(
        callers.len(),
        1,
        "and it is `migrate_relocated_keys`, which never writes"
    );
    // Profile-keyed window-state cleanup may write `settings-*.conf`; this guard
    // is specifically about the global `preferences.conf` authority.
    for gone in ["write_preferences", "preferences_body"] {
        assert!(
            !settings.contains(gone),
            "{gone} would be this crate writing settings files again"
        );
    }
}

/// A guard against the wrong crate being linked for the terminal's values: this
/// window patches the Global section only, and `TerminalSettings` is somebody
/// else's. Cheap, and it fails loudly if a future edit reaches for the wrong
/// section's type here.
#[test]
fn this_window_never_reaches_for_the_terminal_section() {
    let _ = TerminalCursorWidth::Application;
    assert!(
        !include_str!("../preferences.rs").contains("TerminalPatch"),
        "the Global section is the only one a workspace window owns"
    );
}

/// **What ✕ on the last window does is a preference, not a file this window
/// owns.** It used to be read and written straight to `desktop-v1.json` by
/// whichever window was open, each save serializing the whole body over the
/// others'. It is a registry row now, so it patches like everything else.
#[test]
fn the_close_behavior_moves_through_the_authority() {
    let section = GlobalSettings::default();
    let settings = Settings {
        close_behavior: koru_window::CloseBehavior::Quit,
        ..Settings::default()
    };
    assert_eq!(
        patch(&section, &settings).close_behavior,
        Some(koru_settings::CloseBehaviorChoice::Quit)
    );

    let mut told = Settings::default();
    adopt(
        &GlobalSettings {
            close_behavior: koru_settings::CloseBehaviorChoice::Ask,
            ..GlobalSettings::default()
        },
        &mut told,
    );
    assert_eq!(told.close_behavior, koru_window::CloseBehavior::Ask);
}

/// **`desktop-v1.json` has exactly one writer, and it is the authority.**
///
/// Every key that file holds is a registry row now, and the last thing in it
/// that was not — the search overlay's height — is geometry, and went to the
/// `global-search` row. A write from this crate would revert whatever the
/// authority last committed: the two-writers clobber the packet removes.
#[test]
fn this_crate_never_writes_the_device_local_file() {
    let source = include_str!("../app.rs");
    for gone in [
        "desktop_preferences.save()",
        "save_global_search_height",
        "DesktopPreferences::load",
    ] {
        assert!(
            !source.contains(gone),
            "{gone} would be a second writer of desktop-v1.json"
        );
    }
}

/// **A settings click is saved by the click.** The surface has no Apply button,
/// so the press that moved a value is the moment it has to be committed: a
/// second later is a second in which the app can be closed, and a preference
/// that did not survive that is a preference nobody trusts.
#[test]
fn a_preference_that_moved_is_patched_from_the_press_and_not_only_the_tick() {
    let source = include_str!("../app.rs");
    assert_eq!(
        source.matches("self.patch_settings();").count(),
        4,
        "the maintenance tick, the press that moved a preference, the \
         keystroke that moved one (F6 typography), and the close-behavior \
         choice"
    );
}

/// `capture_preferences` is the inverse of `apply_settings`. Dropping a global
/// from the read-back is how "I chose Page" became "it keeps resetting to Full":
/// the click moved the live default, the cheap persist never named it, and the
/// next poll wrote the stored Full back over it.
#[test]
fn capture_preferences_reads_back_the_layout_globals() {
    let source = include_str!("../app.rs");
    assert!(
        source.contains("settings.view_mode = theme::default_view_mode()"),
        "the default page layout has to round-trip through the cheap persist"
    );
    assert!(
        source.contains("settings.canvas_bg = theme::default_canvas_bg()"),
        "so does the canvas colour that travels with it"
    );
    assert!(
        source.contains("let was_view_mode = theme::default_view_mode()"),
        "the press that chose Page has to notice, or it is only saved on the tick"
    );
    assert!(
        source.contains("let was_canvas_bg = theme::default_canvas_bg()"),
        "same for the canvas swatch"
    );
}
