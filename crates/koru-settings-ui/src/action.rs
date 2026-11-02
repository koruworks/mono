// SPDX-License-Identifier: MPL-2.0

//! **What a press means, and what the host is asked to do about it.**
//!
//! Two vocabularies and not one, because the two answer different questions.
//! [`Action`] is *which control was pressed* — private, produced by the layout
//! pass and consumed by [`SettingsSurface::press`](crate::SettingsSurface).
//! [`Request`] is *what only the host can do* — public, drained by the host,
//! and deliberately small: a surface that could install a voice model, write
//! `renderer.conf` or revoke a consent record would be a surface that had to
//! link the crates that own those things, and then it could not be hosted by
//! three different applications.

use koru_draw::theme::FontChoice;
use koru_settings::{
    CaretColorChoice, CaretHeightChoice, CaretWidthChoice, CloseBehaviorChoice, DensityChoice,
    DictationGesture, DictationInput, DictationLanguage, DictationTier, FontFamily, IconPackChoice,
    LanguageChoice, TerminalCursorBlink, TerminalCursorHeight, TerminalCursorWidth, ThemeChoice,
    ViewModeChoice,
};

/// Which of the four font roles a chip belongs to.
///
/// The order is welcome's, and it is the order the roles are drawn in: app,
/// body, headings, code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontRole {
    App,
    Body,
    Headings,
    Code,
}

impl FontRole {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 4] = [Self::App, Self::Body, Self::Headings, Self::Code];

    /// The label welcome drew, carried across unchanged.
    #[cfg(test)]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::App => "App font",
            Self::Body => "Body font",
            Self::Headings => "Headings font",
            Self::Code => "Code font",
        }
    }

    /// The row's name inside its card — *Document fonts* already says what
    /// the three document roles are fonts of.
    pub(crate) fn row_label(self, lang: koru_i18n::Lang) -> &'static str {
        match self {
            Self::App => koru_i18n::t(lang, koru_i18n::Key::SettingsAppFont),
            Self::Body => koru_i18n::t(lang, koru_i18n::Key::SettingsBody),
            Self::Headings => koru_i18n::t(lang, koru_i18n::Key::SettingsHeadings),
            Self::Code => koru_i18n::t(lang, koru_i18n::Key::SettingsCode),
        }
    }

    /// The font chosen for this role: what its list ticks, never the one
    /// being previewed.
    pub(crate) fn current(self) -> FontChoice {
        koru_draw::theme::chosen_font(self.slot())
    }

    /// The theme global this role sets.
    pub fn slot(self) -> koru_draw::theme::FontSlot {
        use koru_draw::theme::FontSlot;
        match self {
            Self::App => FontSlot::Ui,
            Self::Body => FontSlot::Body,
            Self::Headings => FontSlot::Headings,
            Self::Code => FontSlot::Code,
        }
    }

    pub(crate) fn set(self, family: FontChoice) {
        match self {
            Self::App => koru_draw::theme::set_ui_font(family),
            Self::Body => koru_draw::theme::set_body_font(family),
            Self::Headings => koru_draw::theme::set_headings_font(family),
            Self::Code => koru_draw::theme::set_code_font(family),
        }
    }
}

/// One control, named by what it sets.
pub(crate) enum Action {
    // ------------------------------------------------------------- Global
    Theme(ThemeChoice),
    CaretColor(CaretColorChoice),
    CaretWidth(CaretWidthChoice),
    CaretHeight(CaretHeightChoice),
    ViewMode(ViewModeChoice),
    CanvasBackground,
    ApplyLayoutToAll,
    Density(DensityChoice),
    IconPack(IconPackChoice),
    PinnedShowTitle(bool),
    Font(FontRole, FontChoice),
    /// A document body size, in px, from `DocTextSize::ALL`.
    DocTextSize(u8),
    /// The base colour — the window's frame — for the light (`false`) or
    /// dark (`true`) theme:
    /// `0xRRGGBB`, or `None` for the theme's own.
    BaseColor(bool, Option<u32>),
    /// A font role set to exactly this — the row for an installed family
    /// that is not installed here keeps it.
    FontSetting(FontRole, koru_settings::FontSetting),
    /// *More fonts…*: the picker over every installed family, for a role.
    MoreFonts(FontRole),
    VoiceLanguage(DictationLanguage),
    VoiceInput(DictationInput),
    VoiceTier(DictationTier),
    VoiceGesture(DictationGesture),
    SpellcheckEnabled(bool),
    SpellcheckLanguage(LanguageChoice),
    ClearPersonalDictionary,
    DownloadDictionary,
    /// *Install…* or *Remove*, whichever the reading calls for.
    VoiceModels,
    RevokeVoiceConsent,
    /// The renderer passthrough. The value is the **host's** token: this crate
    /// never names a `RendererBackend`, because `renderer.conf` is exempt from
    /// the authority (ADR-266) and belongs to `koru-window`.
    Renderer(String),
    CloseBehavior(CloseBehaviorChoice),
    StartOnLogin(bool),
    /// *Share anonymous usage data*: the value the switch moves to.
    TelemetryConsent(bool),
    /// *Preview in sample document*, at the right of *Document fonts*.
    PreviewSampleDocument,

    // ----------------------------------------------------------- Terminal
    TerminalTheme(ThemeChoice),
    TerminalFont(FontFamily),
    TerminalZoom(f32),
    TerminalCursorColor(CaretColorChoice),
    TerminalCursorWidth(TerminalCursorWidth),
    TerminalCursorHeight(TerminalCursorHeight),
    TerminalCursorBlink(TerminalCursorBlink),
    TerminalComposerDefault(bool),
    /// *Match the editor caret*: `true` sets width and height to
    /// `Application`; `false` gives them the editor caret's current values.
    TerminalMatchCaret(bool),

    // ---------------------------------------------------- Agent providers
    /// An agent's switch: its key, and the value the switch moves to.
    AgentEnabled(String, bool),
    /// *Restore defaults*: every agent on.
    RestoreAgentDefaults,
    /// *Refresh status*: read every agent's status again.
    RefreshAgentStatus,
    /// *Setup guide ↗*: the agent's install or setup page.
    AgentGuide(&'static str),
    /// *Default permission for new chats*: `true` for the *Other agents*
    /// row, and the permission chosen.
    AgentPermission(bool, koru_settings::AgentPermission),
    /// *Default agent*: an agent's key, or `None` for *Auto*.
    DefaultAgent(Option<String>),
}

/// **What the surface cannot do for itself.**
///
/// Everything here is either an effect that belongs to another authority
/// (`renderer.conf`, the voice-consent record, the model cache) or an effect
/// that reaches things this crate must not know about (every open document
/// tab). A host drains these with [`take_requests`](crate::SettingsSurface::take_requests)
/// and performs them in its own vocabulary.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// *More fonts…* on a font list: the container opens the installed-font
    /// picker for this role (it owns the window the picker stands over).
    MoreFonts(FontRole),
    /// *Apply to all documents*. Only the container can reach every open tab.
    ApplyLayoutToAllDocuments,
    /// The canvas swatch was pressed; the host cycles it and hands the new
    /// value back with [`set_canvas_background`](crate::SettingsSurface::set_canvas_background).
    CycleCanvasBackground,
    /// A close-behavior choice for the host to record. The shell holds the
    /// live copy it consults when the moment comes, so a view may not write it.
    SetCloseBehavior(CloseBehaviorChoice),
    /// The renderer passthrough, in the host's own token vocabulary.
    SetRenderer(String),
    SetVoiceLanguage(DictationLanguage),
    SetVoiceInput(DictationInput),
    SetVoiceTier(DictationTier),
    SetVoiceGesture(DictationGesture),
    /// The two spelling preferences live in the host's process globals
    /// (`koru-workspace-host`'s `spelling`), the voice route exactly.
    SetSpellcheckEnabled(bool),
    SetSpellcheckLanguage(LanguageChoice),
    /// Clear the active Profile's synced personal spelling dictionary.
    ClearPersonalDictionary,
    /// Download the spelling dictionary the check would use; see
    /// [`DictionaryRow`](crate::DictionaryRow).
    DownloadDictionary,
    /// Reach a catalog and download several hundred megabytes. Not a view's.
    InstallVoiceModels,
    /// Delete them. Likewise.
    RemoveVoiceModels,
    /// Enumerate microphones. It opens nothing and downloads nothing; what it
    /// cannot be is a call from a view, because it blocks on a socket.
    AskVoiceDevices,
    /// Act through `koru-voice-consent`, which is the record's only owner.
    RevokeVoiceConsent,
    /// Open the AI provider setup the AI surface owns. No Settings row raises
    /// it while the Koru AI card is off the System page; Welcome's *Set up
    /// your AI* still does, through the same host path.
    OpenAiSetup,
    /// Show the sample document — a short read-only page in the person's
    /// document fonts — beside this Settings tab, or bring it forward if it
    /// is already open. Only a host with tabs and panes can; see
    /// [`offer_sample_document`](crate::SettingsSurface::offer_sample_document).
    OpenSampleDocument,
    /// Record this device's telemetry answer. The record is the events
    /// journal's, behind the Gateway (the opt-in contract of 2026-10-11), and
    /// the write must leave the event loop — so it is the host's to perform.
    SetTelemetryConsent(bool),
    /// Turn a coding agent on or off on this machine, by its key. The host
    /// holds the stored list and everything that hides an agent.
    SetAgentEnabled {
        key: String,
        on: bool,
    },
    /// Turn every coding agent on.
    RestoreAgentDefaults,
    /// Read every agent's status again — the host's existing checks, off
    /// the event loop.
    RefreshAgentStatus,
    /// Open an agent's setup guide in the browser: a fixed vendor page.
    OpenAgentGuide(&'static str),
    /// Store the permission new chats start in, on this machine: for the
    /// supported agents, or (`other`) for every other agent.
    SetAgentPermission {
        other: bool,
        permission: koru_settings::AgentPermission,
    },
    /// Store the agent new chats start with, on this machine: a key, or
    /// `None` for *Auto* (the last agent used).
    SetDefaultAgent(Option<String>),
}
