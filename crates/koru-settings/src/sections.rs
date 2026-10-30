// SPDX-License-Identifier: MPL-2.0

//! The three sections, and the per-field patch that changes one.
//!
//! **A patch names fields, never a whole section.** Today's writers serialize
//! the entire body and write it, so two windows that change two different
//! settings at the same time race and one loses a value it never touched —
//! survey note 4. Here a patch carries `Option` per field: a field the patch
//! does not mention is read back from disk and written out unchanged, so
//! different fields never clobber and the same field is simply
//! last-committed-wins.

use serde::{Deserialize, Serialize};

use crate::values::{
    AiProvider, CanvasBackground, CaretColorChoice, CaretHeightChoice, CaretWidthChoice,
    CloseBehaviorChoice, DensityChoice, DictationGesture, DictationInput, DictationLanguage,
    DictationShortcut, DictationTier, Field, FontSetting, GlobalShortcut, IconPackChoice,
    LanguageChoice, PopoutCloseChoice, ReasoningChoice, TerminalCursorBlink, TerminalCursorHeight,
    TerminalCursorWidth, ThemeChoice, ViewModeChoice, Zoom, validate_endpoint, validate_model,
    validate_skin_tone,
};

#[cfg(test)]
mod tests;

/// Which section a setting belongs to — *who consumes it*.
///
/// Deliberately independent of [`StorageClass`], which is *where it lives*.
/// The two axes were conflated before this crate: "device-local" and "not a
/// Koru setting" were the same answer, which is how the terminal's appearance
/// ended up in the geometry file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    Global,
    Ai,
    Terminal,
}

impl Section {
    pub const ALL: &'static [Self] = &[Self::Global, Self::Ai, Self::Terminal];

    pub fn token(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Ai => "ai",
            Self::Terminal => "terminal",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|section| section.token() == text)
    }
}

/// Where a setting is stored — *whether it follows the person or the machine*.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageClass {
    /// Follows the person. Today: the roaming config directory.
    Roaming,
    /// Belongs to this machine and does not travel.
    DeviceLocal,
    /// Held by an authority this crate faces rather than a file it owns.
    Delegated,
}

// ---------------------------------------------------------------- Global

/// The session recorder's last-used modal values (SESSIONRECORD) — an
/// internal dev tool's prefill and nothing more.
///
/// Device-local like [`GlobalSettings::delegate_temporary_root`]: the folder
/// names a directory on *this* machine, and the rest travels with it because
/// a recording rig is a machine, not a person. Written whole — the modal
/// always settles the set at once — and read tolerantly: a value this build
/// does not understand degrades to the default, never to a failed load.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct RecordingPrefs {
    /// The preset token — `current`, `youtube`, `tiktok`, `square`,
    /// `portrait`, `web`, `custom`. Empty means first use: *Current window
    /// size*, which is the modal's own default.
    pub preset: String,
    /// The last custom pair typed, kept whatever preset was chosen so
    /// *Custom…* never opens empty.
    pub custom_width: Option<u32>,
    pub custom_height: Option<u32>,
    /// 60 or 30. `None` is first use — 60, the modal's default.
    pub frame_rate: Option<u32>,
    /// Whether the mouse cursor is in the clip. `None` is first use — on.
    pub cursor: Option<bool>,
    /// The folder the last clip went to. `None` asks for the Videos folder.
    pub folder: Option<String>,
}

/// Everything that applies to Koru as a whole, whoever is consuming it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GlobalSettings {
    // Roaming — `preferences.conf`.
    pub theme: ThemeChoice,
    /// `None` follows the build's default artwork, now and later; it is
    /// persisted as no key, like a font role that follows the app.
    #[serde(default)]
    pub icon_pack: Option<IconPackChoice>,
    pub caret_color: CaretColorChoice,
    pub caret_width: CaretWidthChoice,
    pub caret_height: CaretHeightChoice,
    pub ui_font: FontSetting,
    pub body_font: FontSetting,
    pub headings_font: FontSetting,
    pub code_font: FontSetting,
    pub density: DensityChoice,
    pub zoom: Zoom,
    pub default_view_mode: ViewModeChoice,
    pub default_canvas_background: CanvasBackground,
    pub pinned_show_title: bool,
    pub popout_close: PopoutCloseChoice,
    pub emoji_skin_tone: String,
    pub dictation_gesture: DictationGesture,
    pub dictation_language: DictationLanguage,
    pub dictation_tier: DictationTier,
    pub dictation_input: DictationInput,
    pub spellcheck_enabled: bool,
    pub spellcheck_language: LanguageChoice,
    // Device-local — `desktop-v1.json`.
    pub close_behavior: CloseBehaviorChoice,
    pub global_shortcut: GlobalShortcut,
    /// Whether Agent may open a global microphone session and insert into an
    /// external application. Off until the person accepts the Voice notice.
    /// `None` means the first-use notice has never been accepted; `Some`
    /// remembers onboarding independently of the current enabled value.
    pub global_dictation_enabled: Option<bool>,
    /// The person's preferred global Dictation chord. Agent reports the
    /// registered fallback separately; this is intent, not availability.
    pub global_dictation_shortcut: DictationShortcut,
    pub start_on_login: bool,
    pub reminder_start_on_login_explained: bool,
    pub automatic_update_checks: bool,
    /// **Where Koru makes a folder for a delegated run that belongs to no
    /// project** (ADR-365). `None` means the platform's temporary directory —
    /// `%TMP%` on Windows, `/tmp` on Unix — which is
    /// [`crate::paths::delegate_temporary_base`]'s answer and the only place
    /// the default is spelled.
    ///
    /// Device-local because it names a directory on *this* machine: the same
    /// person's other computer has different disks, and a path that travelled
    /// would send a run to a folder that is not there.
    ///
    /// A string rather than a `PathBuf` because it crosses JSON and the wire,
    /// and because an unreadable value has to survive a round trip rather than
    /// fail a load — the consumer falls back to the default instead.
    pub delegate_temporary_root: Option<String>,
    /// The internal session recorder's prefill. Device-local — see the type.
    pub recording: RecordingPrefs,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::default(),
            icon_pack: None,
            caret_color: CaretColorChoice::default(),
            caret_width: CaretWidthChoice::default(),
            caret_height: CaretHeightChoice::default(),
            ui_font: FontSetting::FollowDefault,
            body_font: FontSetting::FollowDefault,
            headings_font: FontSetting::FollowDefault,
            code_font: FontSetting::FollowDefault,
            density: DensityChoice::default(),
            zoom: Zoom::default(),
            default_view_mode: ViewModeChoice::default(),
            default_canvas_background: CanvasBackground::default(),
            pinned_show_title: false,
            popout_close: PopoutCloseChoice::default(),
            emoji_skin_tone: String::new(),
            dictation_gesture: DictationGesture::default(),
            dictation_language: DictationLanguage::default(),
            dictation_tier: DictationTier::default(),
            dictation_input: DictationInput::default(),
            // **On, not off** (spell-check plan D7): every editor has taught
            // people to expect the squiggles, and a locale with no dictionary
            // degrades to quietly-off at the consumer rather than here.
            spellcheck_enabled: true,
            spellcheck_language: LanguageChoice::Automatic,
            close_behavior: CloseBehaviorChoice::default(),
            global_shortcut: GlobalShortcut::default(),
            global_dictation_enabled: None,
            global_dictation_shortcut: DictationShortcut::default(),
            start_on_login: false,
            reminder_start_on_login_explained: false,
            // **True, not `false`.** `koru-update`'s own default is `true`,
            // and a bool that defaulted the other way here would silently stop
            // updating every install whose file predates the move.
            automatic_update_checks: true,
            // **Absent, not `%TMP%` spelled out.** Storing the resolved
            // directory would freeze whatever this machine's temporary
            // directory happened to be the first time Koru wrote the file,
            // and a person who never chose anything would then stop following
            // their own system when it moved.
            delegate_temporary_root: None,
            recording: RecordingPrefs::default(),
        }
    }
}

/// One patch of [`GlobalSettings`]. Absent fields are left alone.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GlobalPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_pack: Option<IconPackChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caret_color: Option<CaretColorChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caret_width: Option<CaretWidthChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caret_height: Option<CaretHeightChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_font: Option<FontSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_font: Option<FontSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headings_font: Option<FontSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_font: Option<FontSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub density: Option<DensityChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zoom: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_view_mode: Option<ViewModeChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_canvas_background: Option<CanvasBackground>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_show_title: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub popout_close: Option<PopoutCloseChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emoji_skin_tone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dictation_gesture: Option<DictationGesture>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dictation_language: Option<DictationLanguage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dictation_tier: Option<DictationTier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dictation_input: Option<DictationInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spellcheck_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spellcheck_language: Option<LanguageChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_behavior: Option<CloseBehaviorChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_shortcut: Option<GlobalShortcut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_dictation_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_dictation_shortcut: Option<DictationShortcut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_on_login: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reminder_start_on_login_explained: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic_update_checks: Option<bool>,
    /// `null` puts it back to the platform's temporary directory, which is
    /// why this is a [`Field`] and not an `Option`: *absent*, *null* and *a
    /// path* are three different instructions and only two of them fit in an
    /// `Option`.
    #[serde(skip_serializing_if = "Field::is_unchanged")]
    pub delegate_temporary_root: Field<String>,
    /// The recorder's prefill, written whole on every *Record*. Absent
    /// leaves the stored set alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording: Option<RecordingPrefs>,
}

impl GlobalPatch {
    /// Whether this patch touches anything in `preferences.conf`.
    ///
    /// A Global patch can span two files, and a file nothing changed is a file
    /// that is not rewritten — so a theme change never restages
    /// `desktop-v1.json` and cannot fail on it.
    pub fn touches_roaming(&self) -> bool {
        self.theme.is_some()
            || self.icon_pack.is_some()
            || self.caret_color.is_some()
            || self.caret_width.is_some()
            || self.caret_height.is_some()
            || self.ui_font.is_some()
            || self.body_font.is_some()
            || self.headings_font.is_some()
            || self.code_font.is_some()
            || self.density.is_some()
            || self.zoom.is_some()
            || self.default_view_mode.is_some()
            || self.default_canvas_background.is_some()
            || self.pinned_show_title.is_some()
            || self.popout_close.is_some()
            || self.emoji_skin_tone.is_some()
            || self.dictation_gesture.is_some()
            || self.dictation_language.is_some()
            || self.dictation_tier.is_some()
            || self.dictation_input.is_some()
            || self.spellcheck_enabled.is_some()
            || self.spellcheck_language.is_some()
    }

    /// Whether this patch touches anything in `desktop-v1.json`.
    pub fn touches_device(&self) -> bool {
        self.close_behavior.is_some()
            || self.global_shortcut.is_some()
            || self.global_dictation_enabled.is_some()
            || self.global_dictation_shortcut.is_some()
            || self.start_on_login.is_some()
            || self.reminder_start_on_login_explained.is_some()
            || self.automatic_update_checks.is_some()
            || !self.delegate_temporary_root.is_unchanged()
            || self.recording.is_some()
    }

    pub fn is_empty(&self) -> bool {
        !self.touches_roaming() && !self.touches_device()
    }

    /// Refuse anything the files or the consumers could not hold.
    ///
    /// Validation runs before a single byte is staged, so a rejected patch
    /// leaves the disk exactly as it was.
    pub fn validate(&self) -> Result<(), Invalid> {
        if let Some(zoom) = self.zoom
            && Zoom::new(zoom).is_none()
        {
            return Err(Invalid::new(
                "zoom",
                "a zoom factor is a finite number between 0.5 and 2.0",
            ));
        }
        if let Some(tone) = &self.emoji_skin_tone {
            validate_skin_tone(tone).map_err(|why| Invalid::new("emoji_skin_tone", why))?;
        }
        if let Some(language) = &self.dictation_language {
            language
                .validate()
                .map_err(|why| Invalid::new("dictation_language", why))?;
        }
        if let Some(language) = &self.spellcheck_language {
            language
                .validate()
                .map_err(|why| Invalid::new("spellcheck_language", why))?;
        }
        if let Some(input) = &self.dictation_input {
            input
                .validate()
                .map_err(|why| Invalid::new("dictation_input", why))?;
        }
        if let Some(shortcut) = &self.global_shortcut {
            shortcut
                .validate()
                .map_err(|why| Invalid::new("global_shortcut", why))?;
        }
        if let Some(shortcut) = &self.global_dictation_shortcut {
            shortcut
                .validate()
                .map_err(|why| Invalid::new("global_dictation_shortcut", why))?;
        }
        // **Only that it is a path at all.** Whether it exists and whether it
        // can be written to are questions about a machine, and they are asked
        // where the folder is chosen — before the setting is ever sent — so
        // that a person hears about an unusable folder while they are looking
        // at the picker rather than minutes later when a run will not start.
        if let Field::Set(root) = &self.delegate_temporary_root
            && root.trim().is_empty()
        {
            return Err(Invalid::new(
                "delegate_temporary_root",
                "a working folder is a path, or null for the temporary directory",
            ));
        }
        // The recorder's prefill is held to the modal's own rules, so a
        // hand-edit cannot stage a size or rate the encoder would refuse.
        if let Some(recording) = &self.recording {
            if let Some(folder) = &recording.folder
                && folder.trim().is_empty()
            {
                return Err(Invalid::new(
                    "recording",
                    "a recording folder is a path, or absent for the Videos folder",
                ));
            }
            if let Some(rate) = recording.frame_rate
                && !matches!(rate, 30 | 60)
            {
                return Err(Invalid::new(
                    "recording",
                    "a recording frame rate is 30 or 60",
                ));
            }
            for dim in [recording.custom_width, recording.custom_height]
                .into_iter()
                .flatten()
            {
                if !(160..=8192).contains(&dim) {
                    return Err(Invalid::new(
                        "recording",
                        "a custom recording size is between 160 and 8192 pixels",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Fold this patch onto a section read from disk.
    pub fn apply(self, current: &mut GlobalSettings) {
        macro_rules! set {
            ($($field:ident),+ $(,)?) => {
                $( if let Some(value) = self.$field { current.$field = value; } )+
            };
        }
        if let Some(pack) = self.icon_pack {
            current.icon_pack = Some(pack);
        }
        set!(
            theme,
            caret_color,
            caret_width,
            caret_height,
            ui_font,
            body_font,
            headings_font,
            code_font,
            density,
            default_view_mode,
            default_canvas_background,
            pinned_show_title,
            popout_close,
            dictation_gesture,
            dictation_tier,
            spellcheck_enabled,
            close_behavior,
            start_on_login,
            reminder_start_on_login_explained,
            automatic_update_checks,
        );
        if let Some(zoom) = self.zoom.and_then(Zoom::new) {
            current.zoom = zoom;
        }
        if let Some(tone) = self.emoji_skin_tone {
            current.emoji_skin_tone = tone;
        }
        if let Some(language) = self.dictation_language {
            current.dictation_language = language;
        }
        if let Some(language) = self.spellcheck_language {
            current.spellcheck_language = language;
        }
        if let Some(input) = self.dictation_input {
            current.dictation_input = input;
        }
        if let Some(shortcut) = self.global_shortcut {
            current.global_shortcut = shortcut;
        }
        if let Some(shortcut) = self.global_dictation_shortcut {
            current.global_dictation_shortcut = shortcut;
        }
        if let Some(enabled) = self.global_dictation_enabled {
            current.global_dictation_enabled = Some(enabled);
        }
        self.delegate_temporary_root
            .apply(&mut current.delegate_temporary_root);
        if let Some(recording) = self.recording {
            current.recording = recording;
        }
    }
}

// -------------------------------------------------------------- Terminal

/// Koru Terminal's appearance.
///
/// **Roaming**, which is a change: these lived in this machine's `windows.conf`
/// row, beside the geometry, purely because the terminal had one file. How a
/// terminal looks is a preference about the person, not about the monitor it
/// happened to be opened on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalSettings {
    pub theme: ThemeChoice,
    /// Mono-restricted, and deliberately **not** Global's `code_font`: a code
    /// block in a document and a terminal grid are different surfaces, and
    /// somebody who wants a wide face for one rarely wants it for the other.
    pub font: FontSetting,
    pub zoom: Zoom,
    pub cursor_color: CaretColorChoice,
    pub cursor_width: TerminalCursorWidth,
    pub cursor_height: TerminalCursorHeight,
    pub cursor_blink: TerminalCursorBlink,
    /// Whether a *new* window opens with its composer showing.
    ///
    /// The default only. Whether the composer is open in a window that already
    /// exists is furniture, and stays that window's `input_area` row.
    pub composer_default: bool,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::default(),
            font: FontSetting::FollowDefault,
            zoom: Zoom::default(),
            cursor_color: CaretColorChoice::default(),
            cursor_width: TerminalCursorWidth::default(),
            cursor_height: TerminalCursorHeight::default(),
            // **`On`, not the token parser's `Application`.** Those are two
            // different defaults answering two different questions: an
            // unrecognized token in a file falls back to *follow the
            // application*, while a person who has never chosen gets Koru's
            // product default — a visible cursor, as an editing affordance.
            // `koru_session_view::TerminalCursorPreferences` says the same
            // thing, and the appearance tests (written for Koru Terminal, kept by
            // koru.exe's session tabs) hold the two together: a terminal that
            // has heard nothing from the authority and one told the authority
            // holds nothing must blink alike.
            cursor_blink: TerminalCursorBlink::On,
            composer_default: true,
        }
    }
}

/// One patch of [`TerminalSettings`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<FontSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zoom: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_color: Option<CaretColorChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_width: Option<TerminalCursorWidth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_height: Option<TerminalCursorHeight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_blink: Option<TerminalCursorBlink>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub composer_default: Option<bool>,
}

impl TerminalPatch {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn validate(&self) -> Result<(), Invalid> {
        if let Some(zoom) = self.zoom
            && Zoom::new(zoom).is_none()
        {
            return Err(Invalid::new(
                "zoom",
                "a zoom factor is a finite number between 0.5 and 2.0",
            ));
        }
        // The one validation that refuses rather than falling back: a
        // proportional face in a grid renderer is a broken window, not a
        // preference, so it is rejected at the door instead of silently
        // becoming the default and leaving somebody wondering.
        if let Some(FontSetting::Chosen(family)) = self.font
            && !family.is_mono()
        {
            return Err(Invalid::new(
                "font",
                "a terminal font is monospaced: jetbrainsmono, sourcecodepro or mono",
            ));
        }
        Ok(())
    }

    pub fn apply(self, current: &mut TerminalSettings) {
        macro_rules! set {
            ($($field:ident),+ $(,)?) => {
                $( if let Some(value) = self.$field { current.$field = value; } )+
            };
        }
        set!(
            theme,
            font,
            cursor_color,
            cursor_width,
            cursor_height,
            cursor_blink,
            composer_default,
        );
        if let Some(zoom) = self.zoom.and_then(Zoom::new) {
            current.zoom = zoom;
        }
    }
}

// -------------------------------------------------------------------- AI

/// The AI section — **a facade, not a store.**
///
/// The values live in `ai.db` behind the AI worker, and the secrets live in
/// the OS vault behind `koru-ai-provider`. Both authorities are unchanged.
/// This type is the shape the facade presents; the mapping onto the worker is
/// the Gateway's, because the worker handle is the Gateway's.
///
/// **A secret never appears here, in either direction.** The only thing said
/// about the OpenAI key is whether one is configured.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AiSettings {
    pub selected_provider: Option<AiProvider>,
    pub openai: OpenAiSection,
    pub ollama: OllamaSection,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenAiSection {
    pub model: Option<String>,
    pub reasoning: ReasoningChoice,
    /// Whether a key is in the vault. **Read-only, and never a value.**
    pub credential_configured: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OllamaSection {
    pub endpoint: String,
    pub model: Option<String>,
    pub reasoning: ReasoningChoice,
}

impl Default for OllamaSection {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:11434".to_owned(),
            model: None,
            reasoning: ReasoningChoice::default(),
        }
    }
}

/// One patch of [`AiSettings`].
///
/// The nullable fields use [`Field`] so that clearing a model is expressible:
/// `"openai_model": null` clears it, and omitting the key leaves it alone.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AiPatch {
    #[serde(skip_serializing_if = "Field::is_unchanged")]
    pub selected_provider: Field<AiProvider>,
    #[serde(skip_serializing_if = "Field::is_unchanged")]
    pub openai_model: Field<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_reasoning: Option<ReasoningChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ollama_endpoint: Option<String>,
    #[serde(skip_serializing_if = "Field::is_unchanged")]
    pub ollama_model: Field<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ollama_reasoning: Option<ReasoningChoice>,
}

impl AiPatch {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn validate(&self) -> Result<(), Invalid> {
        if let Field::Set(model) = &self.openai_model {
            validate_model(model).map_err(|why| Invalid::new("openai_model", why))?;
        }
        if let Field::Set(model) = &self.ollama_model {
            validate_model(model).map_err(|why| Invalid::new("ollama_model", why))?;
        }
        if let Some(endpoint) = &self.ollama_endpoint {
            validate_endpoint(endpoint).map_err(|why| Invalid::new("ollama_endpoint", why))?;
        }
        Ok(())
    }

    /// Fold this patch onto a section read from the AI authority.
    ///
    /// Used to compute what the worker should be told; the worker remains the
    /// thing that persists it.
    pub fn apply(self, current: &mut AiSettings) {
        self.selected_provider.apply(&mut current.selected_provider);
        self.openai_model.apply(&mut current.openai.model);
        self.ollama_model.apply(&mut current.ollama.model);
        if let Some(reasoning) = self.openai_reasoning {
            current.openai.reasoning = reasoning;
        }
        if let Some(reasoning) = self.ollama_reasoning {
            current.ollama.reasoning = reasoning;
        }
        if let Some(endpoint) = self.ollama_endpoint {
            current.ollama.endpoint = endpoint;
        }
    }
}

/// A patch this authority will not apply, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Invalid {
    /// The field that was refused. Names a patch field, never a file key.
    pub field: String,
    pub message: String,
}

impl Invalid {
    pub fn new(field: &str, message: &str) -> Self {
        Self {
            field: field.to_owned(),
            message: message.to_owned(),
        }
    }
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

/// The typed value of one section, for the routes and the client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "section", rename_all = "snake_case")]
pub enum SectionValues {
    Global(GlobalSettings),
    Ai(AiSettings),
    Terminal(TerminalSettings),
}

impl SectionValues {
    pub fn section(&self) -> Section {
        match self {
            Self::Global(_) => Section::Global,
            Self::Ai(_) => Section::Ai,
            Self::Terminal(_) => Section::Terminal,
        }
    }
}

// **There is deliberately no `SectionPatch` union.** An untagged one is
// ambiguous — every patch type has all-optional fields, so `{}` matches the
// first variant — and a tagged one would put the section in the body as well
// as the path, where the two could disagree. The route already knows its
// section from `/v1/settings/{section}`, so it deserializes that section's
// patch type and nothing else can arrive.
