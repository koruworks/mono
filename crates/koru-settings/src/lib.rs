// SPDX-License-Identifier: MPL-2.0

//! **The Koru settings authority.**
//!
//! Charter: *typed user-editable preferences — the closed registry, defaults,
//! validation, migration, storage classification, file persistence, and change
//! facts. It owns no UI and no window or session state.*
//!
//! Before this crate there were three independent parsers of
//! `preferences.conf` — in `koru-workspace-host`, in `koru-window`, and in
//! `koru-dictation` — plus a fourth reader of `desktop-v1.json` and a fifth of
//! the terminal's row in `windows.conf`. Each had its own idea of what a value
//! meant, none could be pointed at a test directory, and every writer
//! serialized the whole file, so two windows saving at once lost one another's
//! changes. This crate is the one authority; SP-6b deletes the others.
//!
//! ## The two axes
//!
//! [`Section`] is **who consumes a setting** — Global, AI, Terminal.
//! [`StorageClass`] is **where it lives** — roaming, device-local, or held by
//! an authority this crate faces. They are independent, and conflating them is
//! how the terminal's appearance ended up filed with window geometry: the
//! answer to "does it travel?" was being used to answer "is it a setting?".
//!
//! ## What it is not
//!
//! No Store table, no CRDT, no property bag. The backend stays the file-style
//! storage that already exists, because the thing that was broken was
//! ownership, not the format. Cross-device sync is deferred and recorded in
//! `docs/deferred/README.md`.
//!
//! ## Hosting
//!
//! The authority runs inside the API gateway process and is reached over
//! `GET`/`PATCH /v1/settings/{section}`. Reads ride an ordinary read scope;
//! writes take the narrow `settings.write` scope, which is not in the ordinary
//! default set. See ADR-266-settings.

pub mod authority;
pub mod paths;
pub mod registry;
pub mod sections;
pub mod storage;
pub mod values;

#[cfg(test)]
mod dependency;

pub use authority::{PatchError, SettingsAuthority, SettingsEventSink, StartOnLoginPort};
pub use paths::{RUNS_FOLDER, SettingsPaths, delegate_temporary_base};
pub use registry::{Backing, EXEMPTIONS, REGISTRY, SettingSpec, ValueKind, settings_in};
pub use sections::{
    AiPatch, AiSettings, GlobalPatch, GlobalSettings, Invalid, OllamaSection, OpenAiSection,
    RecordingPrefs, Section, SectionValues, StorageClass, TerminalPatch, TerminalSettings,
};
pub use values::{
    AiProvider, CanvasBackground, CaretColorChoice, CaretHeightChoice, CaretWidthChoice,
    CloseBehaviorChoice, DensityChoice, DictationGesture, DictationInput, DictationLanguage,
    DictationShortcut, DictationTier, DocTextSize, DocTextSizeSetting, Field, FontFamily,
    FontSetting, GlobalShortcut, IconPackChoice, LanguageChoice, PopoutCloseChoice,
    ReasoningChoice, TerminalCursorBlink, TerminalCursorHeight, TerminalCursorWidth, ThemeChoice,
    ViewModeChoice, Zoom,
};

/// The bounded wait a window gives the Gateway at startup, in milliseconds.
///
/// **Two seconds** (owner, 2026-09-01). A window asks for its settings off the
/// event thread, renders immediately with the registry's defaults, and swaps
/// in the authoritative section when it lands. If nothing lands inside this
/// budget the window stays on defaults — never a hang, and never a splash
/// screen while somebody's editor waits on a preferences file.
///
/// **Revised 2026-10-30 (owner, DOCFONTSIZE Q-F: "Ok").** "Renders
/// immediately" became "renders within 400 ms": a process's *first* window
/// is held hidden for at most `REVEAL_BUDGET` (400 ms, beside `reveal_pane`
/// in `koru-workspace-host`) waiting for the first answer, so its first
/// visible frame is the person's size, fonts and theme. Past the budget it is
/// shown with the defaults and corrects when the answer lands, as before;
/// this wait is unchanged and still bounds how long the link listens.
///
/// Spelled here rather than in the client so the authority and the client
/// cannot disagree about it; `koru-api-client` reads this constant.
pub const STARTUP_WAIT_MILLIS: u64 = 2_000;

/// The scope a settings **write** requires.
///
/// Not in the ordinary default set (owner, 2026-09-01): an MCP tool or a
/// script cannot silently change what every window looks like. Spelled again
/// in `koru-api-gateway-core`, which this crate does not link; a test there
/// pins the two copies together, the way `CONTROL_SCOPE`'s copies are pinned.
pub const SETTINGS_WRITE_SCOPE: &str = "settings.write";

/// The scope a settings **read** requires. An ordinary scope, in the default set.
pub const SETTINGS_READ_SCOPE: &str = "settings.read";
