// SPDX-License-Identifier: MPL-2.0

//! Round-trip goldens against the formats that are on disk today.
//!
//! Every fixture below is the shape a real build wrote. They are pure string
//! and byte tests — nothing here resolves a path — because the formats are the
//! contract and the disk is not. The one test that does touch a disk is the
//! atomic-replace proof in `crate::authority::tests`, which is what
//! `TESTING.md`'s third exception covers.

use super::*;
use crate::sections::GlobalSettings;

#[test]
fn icon_pack_round_trips_without_changing_other_preferences() {
    for choice in crate::IconPackChoice::ALL {
        let mut settings = GlobalSettings::default();
        let patch = crate::GlobalPatch {
            icon_pack: Some(*choice),
            ..Default::default()
        };
        assert!(patch.touches_roaming());
        patch.apply(&mut settings);
        let written = render_preferences(&settings, &PreferencesFile::default());
        let mut loaded = GlobalSettings::default();
        apply_preferences(&written, &mut loaded);
        assert_eq!(loaded, settings);
    }
    let mut old = GlobalSettings::default();
    apply_preferences("version=2\nicon_pack=future-pack\n", &mut old);
    assert_eq!(old.icon_pack, None, "an unknown pack follows the default");
    let mut json = serde_json::to_value(GlobalSettings::default()).unwrap();
    json.as_object_mut().unwrap().remove("icon_pack");
    let old: GlobalSettings = serde_json::from_value(json).unwrap();
    assert_eq!(old.icon_pack, None);
}

/// Nobody chose a pack: nothing is written, so a later default reaches them.
/// A chosen pack is written even when it is today's default.
#[test]
fn an_unchosen_icon_pack_writes_no_key_and_a_chosen_one_always_does() {
    let settings = GlobalSettings::default();
    let written = render_preferences(&settings, &PreferencesFile::default());
    assert!(!written.contains("icon_pack="), "{written}");
    for choice in crate::IconPackChoice::ALL {
        let settings = GlobalSettings {
            icon_pack: Some(*choice),
            ..GlobalSettings::default()
        };
        let written = render_preferences(&settings, &PreferencesFile::default());
        assert!(written.contains(&format!("icon_pack={}\n", choice.token())));
    }
}

/// A v2 file, as `preferences_body` writes one today.
const V2_FIXTURE: &str = "\
# Koru preferences. These follow you, not a folder and not a machine.
version=2
theme=dark
caret_color=violet
caret_width=4x
caret_height=tall
body_font=lora
density=compact
dictation_toggle=1
dictation_language=fr
dictation_quality=high_accuracy
dictation_input=Yeti Nano
pinned_show_title=1
zoom=1.25
popout_close=restore
view_mode=narrow
canvas_bg=#1e1e28
emoji_recents=1f600,1f44d
emoji_skin_tone=1f3fd
last_export_dir=C:\\Users\\ada\\Documents
";

/// A v1 file: **no `version=` key**, and every font key written out because
/// that is what v1 did on every save.
const V1_FIXTURE: &str = "\
# Koru preferences. These follow you, not a folder and not a machine.
dark=1
theme=light
font=inter
body_font=inter
headings_font=inter
code_font=jetbrainsmono
density=normal
zoom=1.00
";

#[test]
fn a_v2_file_round_trips_through_the_authority() {
    let mut settings = GlobalSettings::default();
    let file = apply_preferences(V2_FIXTURE, &mut settings);

    assert_eq!(settings.theme, ThemeChoice::Dark);
    assert_eq!(settings.caret_color, CaretColorChoice::Violet);
    assert_eq!(settings.caret_width, CaretWidthChoice::X4);
    assert_eq!(settings.caret_height, CaretHeightChoice::Tall);
    assert_eq!(settings.body_font, FontSetting::Chosen(FontFamily::Lora));
    assert_eq!(settings.density, DensityChoice::Compact);
    assert_eq!(settings.dictation_gesture, DictationGesture::Toggle);
    assert_eq!(
        settings.dictation_language,
        DictationLanguage::Tag("fr".into())
    );
    assert_eq!(settings.dictation_tier, DictationTier::HighAccuracy);
    assert_eq!(
        settings.dictation_input,
        DictationInput::Named("Yeti Nano".into())
    );
    assert!(settings.pinned_show_title);
    assert_eq!(settings.zoom.get(), 1.25);
    assert_eq!(settings.popout_close, PopoutCloseChoice::RestoreTab);
    assert_eq!(settings.default_view_mode, ViewModeChoice::Narrow);
    assert_eq!(
        settings.default_canvas_background,
        CanvasBackground::Color([0x1e, 0x1e, 0x28])
    );
    assert_eq!(settings.emoji_skin_tone, "1f3fd");

    // Write it back and read it again: every value survives, and so does
    // everything the authority does not own.
    let written = render_preferences(&settings, &file);
    let mut again = GlobalSettings::default();
    let file_again = apply_preferences(&written, &mut again);
    assert_eq!(again, settings);
    assert_eq!(file_again, file);
}

#[test]
fn absent_font_keys_mean_follow_the_app_and_stay_absent() {
    // The load-bearing one. Three roles are absent from the v2 fixture.
    let mut settings = GlobalSettings::default();
    let file = apply_preferences(V2_FIXTURE, &mut settings);
    assert_eq!(settings.ui_font, FontSetting::FollowDefault);
    assert_eq!(settings.headings_font, FontSetting::FollowDefault);
    assert_eq!(settings.code_font, FontSetting::FollowDefault);
    assert_eq!(settings.body_font, FontSetting::Chosen(FontFamily::Lora));

    let written = render_preferences(&settings, &file);
    assert!(!written.contains("\nfont="));
    assert!(!written.contains("headings_font="));
    assert!(!written.contains("code_font="));
    assert!(written.contains("body_font=lora"));
}

#[test]
fn a_v1_files_auto_written_font_keys_are_not_choices() {
    // v1 wrote every font key on every save, so a v1 key carrying what v1
    // wrote *by default* is a habit rather than a choice. Honoring it froze
    // the default forever: nobody saw a new default font until their file was
    // deleted.
    let mut settings = GlobalSettings::default();
    apply_preferences(V1_FIXTURE, &mut settings);
    assert_eq!(settings.ui_font, FontSetting::FollowDefault);
    assert_eq!(settings.body_font, FontSetting::FollowDefault);
    assert_eq!(settings.headings_font, FontSetting::FollowDefault);
    assert_eq!(settings.code_font, FontSetting::FollowDefault);
    // Everything else in a v1 file is an ordinary value and is kept.
    assert_eq!(settings.theme, ThemeChoice::Light);
    assert_eq!(settings.density, DensityChoice::Normal);
}

#[test]
fn a_v1_font_key_that_is_not_the_v1_default_is_a_real_choice() {
    let text = "font=poppins\ncode_font=sourcecodepro\nbody_font=inter\n";
    let mut settings = GlobalSettings::default();
    apply_preferences(text, &mut settings);
    assert_eq!(settings.ui_font, FontSetting::Chosen(FontFamily::Poppins));
    assert_eq!(
        settings.code_font,
        FontSetting::Chosen(FontFamily::SourceCodePro)
    );
    // `inter` in a v1 file is still the auto-written value, so it is not one.
    assert_eq!(settings.body_font, FontSetting::FollowDefault);
}

#[test]
fn a_v2_font_key_holding_the_old_default_is_a_choice() {
    // The migration is v1-only. Once `version=` is present, every font key in
    // the file was written because somebody chose it.
    let text = "version=2\nfont=inter\ncode_font=jetbrainsmono\n";
    let mut settings = GlobalSettings::default();
    apply_preferences(text, &mut settings);
    assert_eq!(settings.ui_font, FontSetting::Chosen(FontFamily::Inter));
    assert_eq!(
        settings.code_font,
        FontSetting::Chosen(FontFamily::JetBrainsMono)
    );
}

#[test]
fn a_written_file_is_v2_and_carries_no_legacy_dark_key() {
    let mut settings = GlobalSettings::default();
    let file = apply_preferences(V1_FIXTURE, &mut settings);
    let written = render_preferences(&settings, &file);
    assert!(written.starts_with(
        "# Koru preferences. These follow you, not a folder and not a machine.\nversion=2\n"
    ));
    assert!(
        !written.contains("dark="),
        "a pre-theme build's `dark=` was on every save, so it was never a \
         choice; it is dropped rather than migrated"
    );
}

#[test]
fn a_written_file_carries_every_key_the_authority_owns() {
    // The ownership boundary, asserted rather than described: a key in the
    // owned list that the writer forgot would be a setting that silently
    // stopped persisting.
    let settings = GlobalSettings {
        ui_font: FontSetting::Chosen(FontFamily::Poppins),
        body_font: FontSetting::Chosen(FontFamily::Lora),
        headings_font: FontSetting::Chosen(FontFamily::Nunito),
        code_font: FontSetting::Chosen(FontFamily::Mono),
        // A pack that follows the default is absent on purpose, like a font.
        icon_pack: Some(crate::IconPackChoice::Lucide),
        global_dictation_enabled: Some(false),
        // Both nullable device-local keys are given a value: an absent one is
        // absent from the file on purpose, and what this test is about is a
        // writer forgetting a key it was supposed to write.
        delegate_temporary_root: Some("D:/runs".into()),
        // The recorder's key is the same rule: written only once settled, so
        // this fixture settles it.
        recording: crate::RecordingPrefs {
            preset: "youtube".into(),
            folder: Some("D:/videos".into()),
            ..crate::RecordingPrefs::default()
        },
        ..GlobalSettings::default()
    };
    let written = render_preferences(&settings, &PreferencesFile::default());
    let keys: Vec<_> = key_values(&written)
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    for owned in OWNED_PREFERENCE_KEYS {
        assert!(keys.contains(owned), "the writer forgot `{owned}`");
    }
    assert!(keys.contains(&"version"));
    assert!(!keys.contains(&"dark"));

    let desktop = render_desktop(&settings, &DesktopFile::default());
    let value: serde_json::Value = serde_json::from_slice(&desktop).unwrap();
    for owned in OWNED_DESKTOP_KEYS {
        assert!(
            value.get(owned).is_some(),
            "the desktop writer forgot `{owned}`"
        );
    }
}

#[test]
fn the_two_owned_key_lists_do_not_overlap() {
    // One key, one file, one owner. A key in both lists would be a value two
    // writers could disagree about — which is the whole failure this packet
    // exists to remove.
    for key in OWNED_PREFERENCE_KEYS {
        assert!(!OWNED_DESKTOP_KEYS.contains(key), "`{key}` is in two files");
    }
}

#[test]
fn keys_the_authority_does_not_own_survive_a_write() {
    // `emoji_recents` and `last_export_dir` are derived state that shares the
    // file; `opened.*` and `docpref.*` belong to the host. A whole-body writer
    // that only knew about settings would delete all of them.
    let text = format!("{V2_FIXTURE}opened.abc=1\ndocpref.xyz=n|1e1e28\nhide_completed=1\n");
    let mut settings = GlobalSettings::default();
    let file = apply_preferences(&text, &mut settings);
    let written = render_preferences(&settings, &file);
    for carried in [
        "emoji_recents=1f600,1f44d",
        "last_export_dir=C:\\Users\\ada\\Documents",
        "opened.abc=1",
        "docpref.xyz=n|1e1e28",
        "hide_completed=1",
    ] {
        assert!(written.contains(carried), "lost `{carried}`");
    }
}

#[test]
fn an_out_of_range_zoom_is_ignored_rather_than_clamped() {
    for text in ["zoom=9.0\n", "zoom=0.1\n", "zoom=oops\n", "zoom=\n"] {
        let mut settings = GlobalSettings::default();
        apply_preferences(text, &mut settings);
        assert_eq!(settings.zoom, Zoom::default(), "{text:?}");
    }
    let mut settings = GlobalSettings::default();
    apply_preferences("zoom=0.50\n", &mut settings);
    assert_eq!(settings.zoom.get(), 0.5, "the boundary is inclusive");
}

#[test]
fn a_byte_order_mark_and_comments_and_blank_lines_are_tolerated() {
    let text = "\u{feff}# a comment\n\n  theme = dark  \n\u{feff}density=compact\nnot a pair\n";
    let mut settings = GlobalSettings::default();
    apply_preferences(text, &mut settings);
    assert_eq!(settings.theme, ThemeChoice::Dark);
    assert_eq!(settings.density, DensityChoice::Compact);
}

#[test]
fn an_unknown_token_is_the_default_rather_than_a_failure() {
    // A file nobody can read is a person's preferences lost, so nothing here
    // returns an error.
    let text = "theme=chartreuse\ncaret_width=enormous\npopout_close=maybe\n";
    let mut settings = GlobalSettings::default();
    apply_preferences(text, &mut settings);
    assert_eq!(settings.theme, ThemeChoice::System);
    assert_eq!(settings.caret_width, CaretWidthChoice::Medium);
    assert_eq!(settings.popout_close, PopoutCloseChoice::Ask);
}

#[test]
fn the_caret_width_read_and_write_spellings_are_asymmetric_on_purpose() {
    assert_eq!(CaretWidthChoice::parse("x4"), CaretWidthChoice::X4);
    assert_eq!(CaretWidthChoice::parse("4x"), CaretWidthChoice::X4);
    assert_eq!(
        CaretWidthChoice::X4.token(),
        "4x",
        "`x4` is read for compatibility and never written"
    );
}

// ------------------------------------------------------- desktop-v1.json

#[test]
fn desktop_json_round_trips_and_carries_what_it_does_not_own() {
    let bytes = br#"{
      "close_behavior": "quit",
      "global_shortcut": "Ctrl+Alt+K",
      "global_dictation_enabled": false,
      "global_dictation_shortcut": "Ctrl+Alt+Space",
      "global_search_height": 900,
      "start_on_login": true,
      "reminder_start_on_login_explained": true,
      "automatic_checks": false
    }"#;
    let mut settings = GlobalSettings::default();
    let file = apply_desktop(bytes, &mut settings);
    assert_eq!(settings.close_behavior, CloseBehaviorChoice::Quit);
    assert_eq!(settings.global_shortcut.as_str(), "Ctrl+Alt+K");
    assert_eq!(settings.global_dictation_enabled, Some(false));
    assert_eq!(
        settings.global_dictation_shortcut.as_str(),
        "Ctrl+Alt+Space"
    );
    assert!(settings.start_on_login);
    assert!(settings.reminder_start_on_login_explained);
    assert!(!settings.automatic_update_checks);
    assert!(file.had_automatic_checks);

    let written = render_desktop(&settings, &file);
    let value: serde_json::Value = serde_json::from_slice(&written).unwrap();
    assert_eq!(value["close_behavior"], "quit");
    assert_eq!(value["global_shortcut"], "Ctrl+Alt+K");
    assert_eq!(value["global_dictation_enabled"], false);
    assert_eq!(value["global_dictation_shortcut"], "Ctrl+Alt+Space");
    assert_eq!(value["start_on_login"], true);
    assert_eq!(value["reminder_start_on_login_explained"], true);
    assert_eq!(value["automatic_checks"], false);
    assert_eq!(
        value["global_search_height"], 900,
        "geometry that shares this file is carried until SP-6b relocates it"
    );

    let mut again = GlobalSettings::default();
    apply_desktop(&written, &mut again);
    assert_eq!(again, settings);
}

#[test]
fn an_absent_desktop_file_is_a_first_run_and_not_an_error() {
    let mut settings = GlobalSettings::default();
    let file = apply_desktop(b"not json at all", &mut settings);
    assert_eq!(settings, GlobalSettings::default());
    assert!(!file.had_automatic_checks);
    assert_eq!(
        apply_desktop(b"{}", &mut settings).had_automatic_checks,
        false
    );
}

#[test]
fn the_desktop_defaults_match_the_reader_being_replaced() {
    let settings = GlobalSettings::default();
    assert_eq!(settings.close_behavior, CloseBehaviorChoice::Background);
    assert_eq!(settings.global_shortcut.as_str(), "Ctrl+Shift+K");
    assert_eq!(settings.global_dictation_enabled, None);
    assert_eq!(
        settings.global_dictation_shortcut,
        DictationShortcut::default()
    );
    assert!(!settings.start_on_login);
    assert!(!settings.reminder_start_on_login_explained);
}

#[test]
fn an_invalid_stored_dictation_chord_falls_back_before_authority() {
    let mut settings = GlobalSettings::default();
    apply_desktop(
        br#"{"global_dictation_shortcut":"Control+Anything"}"#,
        &mut settings,
    );
    assert_eq!(
        settings.global_dictation_shortcut,
        DictationShortcut::default()
    );
}

#[test]
fn never_enabled_is_absent_but_onboarded_disabled_is_written() {
    let never = render_desktop(&GlobalSettings::default(), &DesktopFile::default());
    let never: serde_json::Value = serde_json::from_slice(&never).unwrap();
    assert!(never.get("global_dictation_enabled").is_none());

    let disabled = GlobalSettings {
        global_dictation_enabled: Some(false),
        ..GlobalSettings::default()
    };
    let disabled = render_desktop(&disabled, &DesktopFile::default());
    let disabled: serde_json::Value = serde_json::from_slice(&disabled).unwrap();
    assert_eq!(disabled["global_dictation_enabled"], false);
}

// ------------------------------------- the automatic-checks migration

#[test]
fn automatic_checks_defaults_to_true_on_both_sides_of_the_move() {
    // The trap: serde's default for a `bool` is `false`, and a file written
    // before the preference moved has no key at all. Defaulting the wrong way
    // silently stops updating every install that already exists.
    assert!(GlobalSettings::default().automatic_update_checks);
    assert_eq!(
        read_update_automatic_checks(br#"{"last_seen_build":44}"#),
        None
    );
    assert_eq!(
        read_update_automatic_checks(br#"{"automatic_checks":false}"#),
        Some(false)
    );
    assert_eq!(
        read_update_automatic_checks(br#"{"automatic_checks":true}"#),
        Some(true)
    );
    assert_eq!(read_update_automatic_checks(b"corrupt"), None);
    assert_eq!(
        read_update_automatic_checks(br#"{"automatic_checks":"yes"}"#),
        None,
        "a value of the wrong type is no opinion, not a false"
    );
}

#[test]
fn absent_in_desktop_json_is_not_the_same_as_false() {
    // `had_automatic_checks` is the whole migration trigger: it distinguishes
    // "this install has never expressed a preference" from "this install
    // turned it off".
    let mut settings = GlobalSettings::default();
    let file = apply_desktop(br#"{"automatic_checks":false}"#, &mut settings);
    assert!(file.had_automatic_checks);
    assert!(!settings.automatic_update_checks);

    let mut settings = GlobalSettings::default();
    let file = apply_desktop(br#"{"close_behavior":"quit"}"#, &mut settings);
    assert!(!file.had_automatic_checks);
    assert!(settings.automatic_update_checks);
}

// ---------------------------------------------------------- terminal.conf

#[test]
fn terminal_settings_round_trip() {
    let settings = TerminalSettings {
        theme: ThemeChoice::Dark,
        font: FontSetting::Chosen(FontFamily::SourceCodePro),
        zoom: Zoom::new(1.4).unwrap(),
        cursor_color: CaretColorChoice::Violet,
        cursor_width: TerminalCursorWidth::X4,
        cursor_height: TerminalCursorHeight::Tall,
        cursor_blink: TerminalCursorBlink::On,
        composer_default: false,
    };
    let written = render_terminal(&settings);
    let mut again = TerminalSettings::default();
    apply_terminal(&written, &mut again);
    assert_eq!(again, settings);
}

#[test]
fn a_terminal_font_that_is_not_monospaced_is_not_obeyed() {
    // The API refuses one, so a file holding one was hand-edited — and a grid
    // rendered in Lora is not what that person meant.
    let mut settings = TerminalSettings::default();
    apply_terminal("font=lora\n", &mut settings);
    assert_eq!(settings.font, FontSetting::FollowDefault);

    let mut settings = TerminalSettings::default();
    apply_terminal("font=mono\n", &mut settings);
    assert_eq!(settings.font, FontSetting::Chosen(FontFamily::Mono));
}

#[test]
fn the_terminal_section_seeds_once_from_this_machines_window_row() {
    let windows = "\
# Where Koru's windows are on THIS machine.
profile | default = x=10;y=20;w=1200;h=800;maximized=0;pinned=0
terminal = x=-1200;y=80;w=1100;h=720;maximized=1;input_area=0;theme=dark;\
font=sourcecodepro;zoom=1.4;cursor_color=violet;cursor_width=4x;\
cursor_height=tall;cursor_blink=on
global-search = x=0;y=0;w=800;h=400
";
    let seeded = seed_terminal_from_window_row(windows).expect("a terminal row");
    assert_eq!(seeded.theme, ThemeChoice::Dark);
    assert_eq!(
        seeded.font,
        FontSetting::Chosen(FontFamily::SourceCodePro),
        "the appearance half is promoted to roaming"
    );
    assert_eq!(seeded.zoom.get(), 1.4);
    assert_eq!(seeded.cursor_color, CaretColorChoice::Violet);
    assert_eq!(seeded.cursor_width, TerminalCursorWidth::X4);
    assert_eq!(seeded.cursor_height, TerminalCursorHeight::Tall);
    assert_eq!(seeded.cursor_blink, TerminalCursorBlink::On);
    assert!(
        !seeded.composer_default,
        "`input_area` is the best evidence of what this person expects a new \
         window to look like"
    );
}

#[test]
fn seeding_reads_nothing_but_the_terminal_row() {
    assert!(seed_terminal_from_window_row("profile | default = x=1;y=2\n").is_none());
    assert!(seed_terminal_from_window_row("").is_none());
    // Geometry in the row is ignored: it stays the window's.
    let seeded = seed_terminal_from_window_row("terminal = x=5;y=6;w=900;h=600\n").unwrap();
    assert_eq!(seeded, TerminalSettings::default());
}

#[test]
fn the_composer_default_inverts_the_way_the_terminals_own_parser_does() {
    for (value, expected) in [("0", false), ("false", false), ("1", true), ("", true)] {
        let seeded =
            seed_terminal_from_window_row(&format!("terminal = input_area={value}\n")).unwrap();
        assert_eq!(seeded.composer_default, expected, "input_area={value}");
    }
}

/// **The chosen base survives a restart** (ADR-365): written into
/// `desktop-v1.json` and read back out of it.
#[test]
fn a_chosen_delegate_folder_round_trips_through_the_device_file() {
    let chosen = GlobalSettings {
        delegate_temporary_root: Some("D:/koru-runs".into()),
        ..GlobalSettings::default()
    };
    let bytes = render_desktop(&chosen, &DesktopFile::default());

    let mut read_back = GlobalSettings::default();
    apply_desktop(&bytes, &mut read_back);
    assert_eq!(
        read_back.delegate_temporary_root.as_deref(),
        Some("D:/koru-runs"),
        "the folder a person chose is what the next run reads"
    );
}

/// **Nothing chosen writes no key**, and reads back as nothing chosen — the
/// two states are one state on disk, which is what stops a person who never
/// chose anything from being pinned to whatever `%TMP%` was that day.
#[test]
fn no_chosen_delegate_folder_writes_no_key() {
    let bytes = render_desktop(&GlobalSettings::default(), &DesktopFile::default());
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(value.get("delegate_temporary_root").is_none());

    let mut read_back = GlobalSettings::default();
    apply_desktop(&bytes, &mut read_back);
    assert_eq!(read_back.delegate_temporary_root, None);
}

/// A blank stored value is absence, not a folder called nothing.
#[test]
fn a_blank_stored_delegate_folder_reads_as_absent() {
    let bytes = br#"{"delegate_temporary_root": "   "}"#;
    let mut read_back = GlobalSettings::default();
    apply_desktop(bytes, &mut read_back);
    assert_eq!(read_back.delegate_temporary_root, None);
}
