// SPDX-License-Identifier: MPL-2.0

//! Reading and writing the files, in the formats that already exist.
//!
//! **No format changes.** `preferences.conf` stays `key=value`, `desktop-v1.json`
//! stays the same JSON object, and the terminal row's grammar is reused for
//! the new roaming `terminal.conf`. An older build must be able to read what
//! this authority writes, because somebody will run one — and a person who
//! downgrades and finds their theme reset has lost something real.
//!
//! Three habits run through every codec here:
//!
//! - **Tolerant read, exact write.** An unknown token is the default, never an
//!   error: a file nobody can parse is a person's preferences gone.
//! - **Keys the registry does not own are carried, not dropped.** The
//!   preferences file still holds `emoji_recents`, `last_export_dir`,
//!   `opened.*` and `docpref.*`, which belong to other owners and relocate in
//!   SP-6b. A whole-body writer that only knew about settings would delete
//!   them on the first save. So a write is read-merge-write, which is also
//!   exactly what makes per-field patches not clobber each other.
//! - **A write publishes by rename.** Every destination is staged beside
//!   itself and renamed over, so a reader finds either the previous complete
//!   file or the next one, never half of either. None of the four current
//!   settings writers does this; all of them are a plain `fs::write`.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use crate::paths::staging_path;
use crate::sections::{GlobalSettings, TerminalSettings};
use crate::values::{
    CanvasBackground, CaretColorChoice, CaretHeightChoice, CaretWidthChoice, CloseBehaviorChoice,
    DensityChoice, DictationGesture, DictationInput, DictationLanguage, DictationShortcut,
    DictationTier, FontFamily, FontSetting, GlobalShortcut, LanguageChoice, PopoutCloseChoice,
    TerminalCursorBlink, TerminalCursorHeight, TerminalCursorWidth, ThemeChoice, ViewModeChoice,
    Zoom,
};

#[cfg(test)]
mod tests;

/// `key = value` lines, tolerant of blanks, comments and a byte-order mark.
///
/// Byte-identical to `koru_window::user_preferences::key_values` and
/// `koru_workspace_host::settings::key_values`: strip a BOM at the start of
/// the file *and* after trimming each line, skip blanks and `#` comments,
/// split on the **first** `=`, trim both halves. A later duplicate wins.
pub fn key_values(text: &str) -> Vec<(&str, &str)> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(|line| line.trim().trim_start_matches('\u{feff}'))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim(), value.trim()))
        .collect()
}

// ------------------------------------------------------- preferences.conf

const PREFERENCES_HEADER: &str =
    "# Koru preferences. These follow you, not a folder and not a machine.";
/// Bumped when a key's *meaning* changes. Nothing branches on the number
/// itself — only on its presence, which is what tells a v2 file from a v1 one.
const PREFERENCES_VERSION: &str = "2";

/// The keys `preferences.conf` holds on this authority's behalf.
///
/// Anything else in the file belongs to somebody else and is carried through a
/// write untouched. `dark` and `version` are handled separately: `dark` is
/// dropped and `version` is rewritten.
///
/// **The checklist, not the mechanism.** The match arms in
/// [`apply_preferences`] and the lines in [`render_preferences`] are the
/// truth; this list is what they are proved against, so a key added to one and
/// forgotten in the other fails a test instead of silently ceasing to persist.
#[cfg(test)]
pub(crate) const OWNED_PREFERENCE_KEYS: &[&str] = &[
    "theme",
    "icon_pack",
    "caret_color",
    "caret_width",
    "caret_height",
    "font",
    "body_font",
    "headings_font",
    "code_font",
    "density",
    "dictation_toggle",
    "dictation_language",
    "dictation_quality",
    "dictation_input",
    "spellcheck_enabled",
    "spellcheck_language",
    "pinned_show_title",
    "zoom",
    "popout_close",
    "view_mode",
    "canvas_bg",
    "emoji_skin_tone",
];

/// What a read of `preferences.conf` recovered, plus what it must carry back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreferencesFile {
    /// Keys this authority does not own, in the order they were found.
    pub carried: Vec<(String, String)>,
}

/// Overlay `preferences.conf` onto a section, and collect what to carry back.
///
/// The **v1 auto-written font migration** lives here and is the subtlest thing
/// in the crate. A file with no `version=` key was written by a build that
/// emitted every font key on every save, so a font key in one is not evidence
/// of a choice — it is evidence that somebody once pressed save. A v1 key
/// carrying what v1 wrote by default is therefore ignored, and the role falls
/// back to "follow the app". Without this, a default written out once froze
/// that default forever, and did: nobody saw a new default font until their
/// file was deleted.
pub fn apply_preferences(text: &str, into: &mut GlobalSettings) -> PreferencesFile {
    let pairs = key_values(text);
    let v1 = !pairs.iter().any(|(key, _)| *key == "version");
    let auto_written = |key: &str, value: &str| {
        v1 && match key {
            "font" | "body_font" | "headings_font" => value == "inter",
            "code_font" => value == "jetbrainsmono",
            _ => false,
        }
    };

    let mut file = PreferencesFile::default();
    for (key, value) in pairs {
        if auto_written(key, value) {
            continue;
        }
        match key {
            // Read for its presence only, and always rewritten.
            "version" => {}
            // Pre-theme builds wrote `dark=0|1` on every save, so it was not a
            // choice — especially `dark=0`. Ignored on read, dropped on write.
            "dark" => {}
            "theme" => into.theme = ThemeChoice::parse(value),
            "icon_pack" => into.icon_pack = crate::IconPackChoice::parse(value),
            "caret_color" => into.caret_color = CaretColorChoice::parse(value),
            "caret_width" => into.caret_width = CaretWidthChoice::parse(value),
            "caret_height" => into.caret_height = CaretHeightChoice::parse(value),
            "font" => into.ui_font = FontSetting::Chosen(FontFamily::parse(value)),
            "body_font" => into.body_font = FontSetting::Chosen(FontFamily::parse(value)),
            "headings_font" => into.headings_font = FontSetting::Chosen(FontFamily::parse(value)),
            "code_font" => into.code_font = FontSetting::Chosen(FontFamily::parse(value)),
            "density" => into.density = DensityChoice::parse(value),
            "dictation_toggle" => into.dictation_gesture = DictationGesture::parse(value),
            "dictation_language" => into.dictation_language = DictationLanguage::parse(value),
            "dictation_quality" => into.dictation_tier = DictationTier::parse(value),
            "dictation_input" => into.dictation_input = DictationInput::parse(value),
            "spellcheck_enabled" => into.spellcheck_enabled = value != "0" && value != "false",
            "spellcheck_language" => into.spellcheck_language = LanguageChoice::parse(value),
            "pinned_show_title" => into.pinned_show_title = value == "1" || value == "true",
            "zoom" => into.zoom = Zoom::parse_or_default(value),
            "popout_close" => into.popout_close = PopoutCloseChoice::parse(value),
            "view_mode" => into.default_view_mode = ViewModeChoice::parse(value),
            "canvas_bg" => into.default_canvas_background = CanvasBackground::parse(value),
            "emoji_skin_tone" => into.emoji_skin_tone = value.to_owned(),
            // Somebody else's. `emoji_recents`, `last_export_dir`, `opened.*`,
            // `docpref.*`, `hide_completed`, `show_archived` all land here and
            // survive every write this crate makes.
            other => file.carried.push((other.to_owned(), value.to_owned())),
        }
    }
    file
}

/// Render `preferences.conf`.
///
/// **A chosen family is always written, even when it equals today's default.**
/// The pre-SP-6 writer omitted a key whose value matched the role default,
/// which meant "I picked Inter" and "I never chose" produced identical bytes —
/// so a later default change silently moved somebody who had deliberately
/// chosen. Follow-the-app is now the *only* thing that omits a key.
pub fn render_preferences(settings: &GlobalSettings, file: &PreferencesFile) -> String {
    let mut out = String::with_capacity(768);
    out.push_str(PREFERENCES_HEADER);
    out.push('\n');
    out.push_str("version=");
    out.push_str(PREFERENCES_VERSION);
    out.push('\n');

    let mut line = |key: &str, value: &str| {
        out.push_str(key);
        out.push('=');
        out.push_str(value);
        out.push('\n');
    };

    line("theme", settings.theme.token());
    line("icon_pack", settings.icon_pack.token());
    line("caret_color", settings.caret_color.token());
    line("caret_width", settings.caret_width.token());
    line("caret_height", settings.caret_height.token());
    for (key, setting) in [
        ("font", settings.ui_font),
        ("body_font", settings.body_font),
        ("headings_font", settings.headings_font),
        ("code_font", settings.code_font),
    ] {
        if let Some(family) = setting.family() {
            line(key, family.token());
        }
    }
    line("density", settings.density.token());
    line("dictation_toggle", settings.dictation_gesture.token());
    line("dictation_language", settings.dictation_language.token());
    line("dictation_quality", settings.dictation_tier.token());
    line("dictation_input", settings.dictation_input.token());
    line(
        "spellcheck_enabled",
        if settings.spellcheck_enabled {
            "1"
        } else {
            "0"
        },
    );
    line("spellcheck_language", settings.spellcheck_language.token());
    line(
        "pinned_show_title",
        if settings.pinned_show_title { "1" } else { "0" },
    );
    line("zoom", &format!("{:.2}", settings.zoom.get()));
    line("popout_close", settings.popout_close.token());
    line("view_mode", settings.default_view_mode.token());
    line("canvas_bg", &settings.default_canvas_background.token());
    line("emoji_skin_tone", &settings.emoji_skin_tone);

    for (key, value) in &file.carried {
        line(key, value);
    }
    out
}

// -------------------------------------------------------- desktop-v1.json

/// The keys `desktop-v1.json` holds on this authority's behalf. The checklist
/// [`render_desktop`] is proved against; see [`OWNED_PREFERENCE_KEYS`].
#[cfg(test)]
pub(crate) const OWNED_DESKTOP_KEYS: &[&str] = &[
    "close_behavior",
    "global_shortcut",
    "global_dictation_enabled",
    "global_dictation_shortcut",
    "start_on_login",
    "reminder_start_on_login_explained",
    "automatic_checks",
    "delegate_temporary_root",
    "recording",
];

/// What a read of `desktop-v1.json` recovered, plus what it must carry back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DesktopFile {
    /// Keys this authority does not own — today `global_search_height`, which
    /// is geometry and leaves for a `windows.conf` row in SP-6b.
    pub carried: BTreeMap<String, serde_json::Value>,
    /// Whether the file said anything about automatic update checks.
    ///
    /// `false` here is what triggers the one-time migration read of
    /// `koru-update`'s `state.json`: absent is not the same as `false`, and
    /// treating it as `false` would silently stop updating every install.
    pub had_automatic_checks: bool,
}

/// Overlay `desktop-v1.json` onto a section.
///
/// Total, like the reader it replaces: a corrupt or unreadable file yields the
/// defaults rather than an error, and the next write repairs it.
pub fn apply_desktop(bytes: &[u8], into: &mut GlobalSettings) -> DesktopFile {
    let Ok(serde_json::Value::Object(object)) = serde_json::from_slice(bytes) else {
        return DesktopFile::default();
    };
    let mut file = DesktopFile::default();
    for (key, value) in object {
        match key.as_str() {
            "close_behavior" => {
                if let Some(text) = value.as_str() {
                    into.close_behavior = CloseBehaviorChoice::parse(text);
                }
            }
            "global_shortcut" => {
                if let Some(text) = value.as_str()
                    && !text.trim().is_empty()
                {
                    into.global_shortcut = GlobalShortcut::new(text);
                }
            }
            "global_dictation_enabled" => {
                if let Some(flag) = value.as_bool() {
                    into.global_dictation_enabled = Some(flag);
                }
            }
            "global_dictation_shortcut" => {
                if let Some(text) = value.as_str()
                    && !text.trim().is_empty()
                {
                    into.global_dictation_shortcut = DictationShortcut::parse_or_default(text);
                }
            }
            "start_on_login" => {
                if let Some(flag) = value.as_bool() {
                    into.start_on_login = flag;
                }
            }
            "reminder_start_on_login_explained" => {
                if let Some(flag) = value.as_bool() {
                    into.reminder_start_on_login_explained = flag;
                }
            }
            "automatic_checks" => {
                if let Some(flag) = value.as_bool() {
                    into.automatic_update_checks = flag;
                    file.had_automatic_checks = true;
                }
            }
            // A blank string reads as absent: the consumer's fallback is the
            // platform's temporary directory either way, and a file that had
            // somehow been left with `""` should behave like one that never
            // named a folder rather than like one naming the current
            // directory.
            "delegate_temporary_root" => {
                if let Some(text) = value.as_str()
                    && !text.trim().is_empty()
                {
                    into.delegate_temporary_root = Some(text.to_owned());
                }
            }
            // Read tolerantly: a value this build does not understand —
            // a future field, a hand-edit — degrades to the defaults rather
            // than failing the whole load.
            "recording" => {
                if let Ok(prefs) = serde_json::from_value::<crate::RecordingPrefs>(value) {
                    into.recording = prefs;
                }
            }
            _ => {
                file.carried.insert(key, value);
            }
        }
    }
    file
}

/// Render `desktop-v1.json`.
///
/// Pretty-printed with no trailing newline, which is what `to_vec_pretty`
/// produces and therefore what is on disk today.
pub fn render_desktop(settings: &GlobalSettings, file: &DesktopFile) -> Vec<u8> {
    let mut object = serde_json::Map::new();
    for (key, value) in &file.carried {
        object.insert(key.clone(), value.clone());
    }
    object.insert(
        "close_behavior".into(),
        settings.close_behavior.token().into(),
    );
    object.insert(
        "global_shortcut".into(),
        settings.global_shortcut.as_str().into(),
    );
    if let Some(enabled) = settings.global_dictation_enabled {
        object.insert("global_dictation_enabled".into(), enabled.into());
    }
    object.insert(
        "global_dictation_shortcut".into(),
        settings.global_dictation_shortcut.as_str().into(),
    );
    object.insert("start_on_login".into(), settings.start_on_login.into());
    object.insert(
        "reminder_start_on_login_explained".into(),
        settings.reminder_start_on_login_explained.into(),
    );
    object.insert(
        "automatic_checks".into(),
        settings.automatic_update_checks.into(),
    );
    // Written only when a folder was chosen. An absent key is what "follow
    // this machine's temporary directory" is spelled as, and writing `null`
    // would make the two states look different on disk when they are not.
    if let Some(root) = &settings.delegate_temporary_root {
        object.insert("delegate_temporary_root".into(), root.as_str().into());
    }
    // Written only once a recording has settled: absent is what "first use"
    // is spelled as, and a first-use file should not grow keys it has never
    // needed.
    if settings.recording != crate::RecordingPrefs::default() {
        object.insert(
            "recording".into(),
            serde_json::to_value(&settings.recording).expect("recording prefs always serialize"),
        );
    }
    serde_json::to_vec_pretty(&serde_json::Value::Object(object))
        .expect("a settings object always serializes")
}

/// Read `automatic_checks` out of `koru-update`'s `state.json`.
///
/// **The one-time migration, and the only time this authority looks at that
/// file.** Everything else in `state.json` — the last check time, the etags,
/// the cached package — is `koru-update`'s runtime state and stays there. Two
/// writers sharing one JSON file, each writing the whole body, is exactly the
/// clobber this packet exists to remove; so the *preference* moves out and the
/// *state* stays.
///
/// Absent, unreadable or malformed means "no opinion", and the section's own
/// default of `true` stands — which is also `koru-update`'s default.
pub fn read_update_automatic_checks(bytes: &[u8]) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value.get("automatic_checks")?.as_bool()
}

// ----------------------------------------------------------- terminal.conf

const TERMINAL_HEADER: &str = "# Koru Terminal appearance. This follows you, not a machine.";
const TERMINAL_VERSION: &str = "1";

/// Overlay `terminal.conf`.
pub fn apply_terminal(text: &str, into: &mut TerminalSettings) {
    for (key, value) in key_values(text) {
        match key {
            "version" => {}
            "theme" => into.theme = ThemeChoice::parse(value),
            "font" => {
                // A file that names a proportional family is treated as
                // follow-the-app rather than obeyed: the API refuses one, so a
                // file holding one was hand-edited, and a grid rendered in
                // Lora is not what that person meant.
                let family = FontFamily::parse(value);
                into.font = if family.is_mono() {
                    FontSetting::Chosen(family)
                } else {
                    FontSetting::FollowDefault
                };
            }
            "zoom" => into.zoom = Zoom::parse_or_default(value),
            "cursor_color" => into.cursor_color = CaretColorChoice::parse(value),
            "cursor_width" => into.cursor_width = TerminalCursorWidth::parse(value),
            "cursor_height" => into.cursor_height = TerminalCursorHeight::parse(value),
            "cursor_blink" => into.cursor_blink = TerminalCursorBlink::parse(value),
            "composer_default" => into.composer_default = !matches!(value, "0" | "false"),
            _ => {}
        }
    }
}

/// Render `terminal.conf`.
pub fn render_terminal(settings: &TerminalSettings) -> String {
    let mut out = String::with_capacity(256);
    out.push_str(TERMINAL_HEADER);
    out.push('\n');
    out.push_str("version=");
    out.push_str(TERMINAL_VERSION);
    out.push('\n');
    let mut line = |key: &str, value: &str| {
        out.push_str(key);
        out.push('=');
        out.push_str(value);
        out.push('\n');
    };
    line("theme", settings.theme.token());
    if let Some(family) = settings.font.family() {
        line("font", family.token());
    }
    line("zoom", &format!("{:.2}", settings.zoom.get()));
    line("cursor_color", settings.cursor_color.token());
    line("cursor_width", settings.cursor_width.token());
    line("cursor_height", settings.cursor_height.token());
    line("cursor_blink", settings.cursor_blink.token());
    line(
        "composer_default",
        if settings.composer_default { "1" } else { "0" },
    );
    out
}

/// Seed the Terminal section from this machine's `windows.conf` row.
///
/// **Once, and only when `terminal.conf` does not exist yet.** The appearance
/// half of that row is being promoted from device-local to roaming; the
/// geometry half — position, size, maximized, `input_area` — stays exactly
/// where it is and belongs to the window, not to the person.
///
/// `input_area` seeds `composer_default`, which is the one place a per-window
/// value becomes a default for new windows: it is the best evidence available
/// of what this person expects a terminal to look like when it opens.
pub fn seed_terminal_from_window_row(text: &str) -> Option<TerminalSettings> {
    let row = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim(), value.trim()))
        .find(|(key, _)| *key == "terminal")
        .map(|(_, value)| value)?;

    let mut settings = TerminalSettings::default();
    for field in row.split(';') {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "theme" => settings.theme = ThemeChoice::parse(value),
            "font" => {
                let family = FontFamily::parse(value);
                if family.is_mono() {
                    settings.font = FontSetting::Chosen(family);
                }
            }
            "zoom" => settings.zoom = Zoom::parse_or_default(value),
            "cursor_color" => settings.cursor_color = CaretColorChoice::parse(value),
            "cursor_width" => settings.cursor_width = TerminalCursorWidth::parse(value),
            "cursor_height" => settings.cursor_height = TerminalCursorHeight::parse(value),
            "cursor_blink" => settings.cursor_blink = TerminalCursorBlink::parse(value),
            // Default-true inversion, matching the terminal's own parser.
            "input_area" => settings.composer_default = !matches!(value, "0" | "false"),
            _ => {}
        }
    }
    Some(settings)
}

// -------------------------------------------------------------------- I/O

/// Read a file, treating "absent" and "unreadable" the same way.
///
/// Total on purpose: a preferences file that will not open is a first run as
/// far as anything above here is concerned, and the next write repairs it.
pub fn read_optional(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

/// Write `bytes` to `destination` by staging beside it and renaming over.
///
/// **This is the atomic-replace proof `TESTING.md`'s third exception covers.**
/// A reader finds either the previous complete file or the next one. The
/// staging name carries the process id so two hosts cannot collide, and it is
/// removed on any failure so a crashed write does not leave litter that looks
/// like a settings file.
pub fn write_atomic(destination: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staging = staging_path(destination);
    let result = (|| {
        let mut file = std::fs::File::create(&staging)?;
        file.write_all(bytes)?;
        // The bytes are on the disk before the name points at them. Without
        // this the rename can be durable while the contents are not, which is
        // an empty settings file after a power loss.
        file.sync_all()?;
        drop(file);
        std::fs::rename(&staging, destination)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    result
}
