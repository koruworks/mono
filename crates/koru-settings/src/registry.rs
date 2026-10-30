// SPDX-License-Identifier: MPL-2.0

//! The closed registry: every setting this authority knows, and nothing else.
//!
//! **Closed is the point.** The alternative — a property bag, or a settings
//! table, or a CRDT — makes "what settings are there?" a question you answer by
//! reading whatever happens to be on disk, and makes adding one a thing that
//! can happen by accident at a call site. Here a setting exists because there
//! is a row for it, the row names its section, its storage class and the file
//! it lands in, and `registry/tests.rs` fails if a row and its patch field
//! disagree.
//!
//! The rows are frozen from the SP-6 plan, which is itself frozen from the
//! 2026-08-31 survey. The plan document is the checklist: a row here that is
//! not in the plan, or a plan entry with no row, is a bug in one of the two.

use crate::sections::{Section, StorageClass};

#[cfg(test)]
mod tests;

/// Which file a setting's bytes land in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Backing {
    /// The roaming `preferences.conf`.
    PreferencesConf,
    /// The roaming `terminal.conf`.
    TerminalConf,
    /// The device-local `desktop-v1.json`.
    DesktopJson,
    /// `ai.db` plus the OS vault, behind the AI worker. Not this crate's file.
    AiAuthority,
}

impl Backing {
    pub fn token(self) -> &'static str {
        match self {
            Self::PreferencesConf => "preferences.conf",
            Self::TerminalConf => "terminal.conf",
            Self::DesktopJson => "desktop-v1.json",
            Self::AiAuthority => "ai.db + OS vault",
        }
    }
}

/// The kind of value a setting holds, for documentation and for the API shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    /// A closed set of tokens.
    Token,
    /// True or false.
    Flag,
    /// A bounded number.
    Number,
    /// Free text the authority validates the shape of.
    Text,
    /// A font role: a family, or the explicit "follow the app" state.
    Font,
    /// A value that may be absent, and whose absence is meaningful.
    Nullable,
}

/// One setting.
#[derive(Clone, Copy, Debug)]
pub struct SettingSpec {
    /// The field name in the section type and in a patch body. **Not** the
    /// on-disk key: those are a migration concern and several differ.
    pub field: &'static str,
    pub section: Section,
    pub storage: StorageClass,
    pub backing: Backing,
    pub kind: ValueKind,
    /// What it means, and any behavior that is not obvious from the type.
    pub doc: &'static str,
}

/// The table.
///
/// Global's roaming half first, then Global's device-local half, then
/// Terminal, then the AI facade — which is the order the plan freezes them in.
pub const REGISTRY: &[SettingSpec] = &[
    SettingSpec {
        field: "icon_pack",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Token,
        doc: "In-app icon artwork: Phosphor, Tabler, Lucide or Remix. Absent until the \
              person chooses one, which follows the build's default artwork. Applied \
              immediately.",
    },
    // ------------------------------------ Global, roaming: preferences.conf
    SettingSpec {
        field: "theme",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Token,
        doc: "Light, dark, or follow the system. A pre-theme build's `dark=` \
              is ignored on read and never written: it was on every save, so \
              it was a habit rather than a choice.",
    },
    SettingSpec {
        field: "caret_color",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Token,
        doc: "The text caret's color.",
    },
    SettingSpec {
        field: "caret_width",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Token,
        doc: "The text caret's width. `x4`/`x8` are read for compatibility \
              and written as `4x`/`8x`.",
    },
    SettingSpec {
        field: "caret_height",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Token,
        doc: "The text caret's height, as a fraction of the line.",
    },
    SettingSpec {
        field: "ui_font",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Font,
        doc: "The interface typeface. `follow_default` is an explicit state \
              and persists as no key at all — never as today's default \
              spelled out, which is what froze the default for everyone who \
              had ever saved.",
    },
    SettingSpec {
        field: "body_font",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Font,
        doc: "The document body typeface. Follow-the-app as above.",
    },
    SettingSpec {
        field: "headings_font",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Font,
        doc: "The headings typeface. Follow-the-app as above.",
    },
    SettingSpec {
        field: "code_font",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Font,
        doc: "The code-block typeface. Follow-the-app as above, and \
              deliberately not the terminal's font.",
    },
    SettingSpec {
        field: "density",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Token,
        doc: "How much space the interface leaves around things.",
    },
    SettingSpec {
        field: "zoom",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Number,
        doc: "The interface scale, 0.5 to 2.0. Out of range is ignored rather \
              than clamped — the guarded-geometry stance.",
    },
    SettingSpec {
        field: "default_view_mode",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Token,
        doc: "The view mode a document opens in when it has no override of \
              its own. A document's own override is `docpref.*` and stays \
              with the host: it is per-document state, not a preference.",
    },
    SettingSpec {
        field: "default_canvas_background",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Nullable,
        doc: "The canvas background a document opens with. `auto` follows the \
              theme; a color is `#rrggbb` and always opaque.",
    },
    SettingSpec {
        field: "pinned_show_title",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Flag,
        doc: "Whether a pinned window keeps its title bar.",
    },
    SettingSpec {
        field: "popout_close",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Token,
        doc: "What closing a popped-out document does. `restore_tab` \
              persists as `restore`.",
    },
    SettingSpec {
        field: "emoji_skin_tone",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Text,
        doc: "The skin-tone modifier the emoji picker applies. The picker's \
              *recents* are derived state and are not a setting: they move to \
              the host's profile-slot file in SP-6b.",
    },
    SettingSpec {
        field: "dictation_gesture",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Token,
        doc: "Push-to-talk or toggle. Persists as `dictation_toggle=0|1`; \
              only `1` means toggle, matching all three current readers.",
    },
    SettingSpec {
        field: "dictation_language",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Nullable,
        doc: "The language dictation transcribes, or automatic. The authority \
              validates a tag's shape and never resolves it: the closed set \
              of languages is the consumer's.",
    },
    SettingSpec {
        field: "dictation_tier",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Token,
        doc: "Which speech model quality tier runs. Persists as \
              `dictation_quality=`.",
    },
    SettingSpec {
        field: "dictation_input",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Nullable,
        doc: "Which microphone dictation opens. The device name **roams** — \
              ADR-252's recorded reasoning, kept deliberately even though a \
              device belongs to a machine.",
    },
    SettingSpec {
        field: "spellcheck_enabled",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Flag,
        doc: "Whether misspelled words are underlined while typing. On by \
              default (spell-check plan D7); a language with no dictionary \
              degrades to quietly-off at the consumer, never here.",
    },
    SettingSpec {
        field: "spellcheck_language",
        section: Section::Global,
        storage: StorageClass::Roaming,
        backing: Backing::PreferencesConf,
        kind: ValueKind::Nullable,
        doc: "The language spelling is checked in, or automatic — the \
              profile's locale. The authority validates a tag's shape and \
              never resolves it: the closed set of dictionaries is the \
              consumer's.",
    },
    // ------------------------------ Global, device-local: desktop-v1.json
    SettingSpec {
        field: "close_behavior",
        section: Section::Global,
        storage: StorageClass::DeviceLocal,
        backing: Backing::DesktopJson,
        kind: ValueKind::Token,
        doc: "What closing the last window does: ask, go to the tray, or quit.",
    },
    SettingSpec {
        field: "global_shortcut",
        section: Section::Global,
        storage: StorageClass::DeviceLocal,
        backing: Backing::DesktopJson,
        kind: ValueKind::Text,
        doc: "The chord that raises global search. Currently writerless — no \
              surface changes it — and the registry defines it anyway, \
              because a value the app reads and nothing can write is the hole \
              this authority exists to close.",
    },
    SettingSpec {
        field: "global_dictation_enabled",
        section: Section::Global,
        storage: StorageClass::DeviceLocal,
        backing: Backing::DesktopJson,
        kind: ValueKind::Flag,
        doc: "Whether global dictation is enabled. It defaults off and moves \
              only through an acknowledged settings write after the Voice \
              notice has been accepted.",
    },
    SettingSpec {
        field: "global_dictation_shortcut",
        section: Section::Global,
        storage: StorageClass::DeviceLocal,
        backing: Backing::DesktopJson,
        kind: ValueKind::Text,
        doc: "The requested global Dictation chord. Agent owns registration \
              and reports a fallback or conflict separately.",
    },
    SettingSpec {
        field: "start_on_login",
        section: Section::Global,
        storage: StorageClass::DeviceLocal,
        backing: Backing::DesktopJson,
        kind: ValueKind::Flag,
        doc: "Whether Koru starts with the session. **The OS side effect must \
              succeed before the value commits**, exactly as today, so this \
              field is refused unless a host has installed a \
              `StartOnLoginPort`. A stored `true` that the registry never got \
              is a lie the person cannot see.",
    },
    SettingSpec {
        field: "reminder_start_on_login_explained",
        section: Section::Global,
        storage: StorageClass::DeviceLocal,
        backing: Backing::DesktopJson,
        kind: ValueKind::Flag,
        doc: "Whether this installation has recorded the one-time reminder \
              explanation for starting Koru with the session. It is local \
              shell state, not reminder data and does not roam.",
    },
    SettingSpec {
        field: "automatic_update_checks",
        section: Section::Global,
        storage: StorageClass::DeviceLocal,
        backing: Backing::DesktopJson,
        kind: ValueKind::Flag,
        doc: "Whether the Agent checks for updates on its own schedule. \
              **Moved here from `koru-update`'s `state.json`**, which stays \
              that crate's runtime state; the value is seeded from it once. \
              Its default is `true` on both sides: a bool that defaulted the \
              other way would silently stop updating every existing install.",
    },
    SettingSpec {
        field: "delegate_temporary_root",
        section: Section::Global,
        storage: StorageClass::DeviceLocal,
        backing: Backing::DesktopJson,
        kind: ValueKind::Nullable,
        doc: "Where Koru makes a folder for a delegated run that belongs to no \
              project. `null` — and an absent key — mean the platform's \
              temporary directory, which is the only place the default is \
              spelled (`paths::delegate_temporary_base`). Device-local \
              because it names a directory on *this* machine: a path that \
              roamed would send a run to a folder the other computer does not \
              have. Whether the folder can be written to is settled where it \
              is chosen, not here — see ADR-365.",
    },
    SettingSpec {
        field: "recording",
        section: Section::Global,
        storage: StorageClass::DeviceLocal,
        backing: Backing::DesktopJson,
        kind: ValueKind::Nullable,
        doc: "The internal session recorder's last-used modal values \
              (SESSIONRECORD): preset token, custom width/height, frame rate, \
              cursor, and the folder the last clip went to. Written whole on \
              every Record; absent means first use, which is the modal's own \
              default set. Device-local because the folder names a directory \
              on this machine — the delegate_temporary_root rule.",
    },
    // ------------------------------------- Terminal, roaming: terminal.conf
    SettingSpec {
        field: "theme",
        section: Section::Terminal,
        storage: StorageClass::Roaming,
        backing: Backing::TerminalConf,
        kind: ValueKind::Token,
        doc: "The terminal's theme.",
    },
    SettingSpec {
        field: "font",
        section: Section::Terminal,
        storage: StorageClass::Roaming,
        backing: Backing::TerminalConf,
        kind: ValueKind::Font,
        doc: "The terminal typeface — mono-restricted, and its own value \
              rather than Global's `code_font`. A proportional family is \
              refused rather than silently replaced.",
    },
    SettingSpec {
        field: "zoom",
        section: Section::Terminal,
        storage: StorageClass::Roaming,
        backing: Backing::TerminalConf,
        kind: ValueKind::Number,
        doc: "The terminal's scale, 0.5 to 2.0.",
    },
    SettingSpec {
        field: "cursor_color",
        section: Section::Terminal,
        storage: StorageClass::Roaming,
        backing: Backing::TerminalConf,
        kind: ValueKind::Token,
        doc: "The terminal cursor's color.",
    },
    SettingSpec {
        field: "cursor_width",
        section: Section::Terminal,
        storage: StorageClass::Roaming,
        backing: Backing::TerminalConf,
        kind: ValueKind::Token,
        doc: "The terminal cursor's width; `application` follows the app's \
              caret.",
    },
    SettingSpec {
        field: "cursor_height",
        section: Section::Terminal,
        storage: StorageClass::Roaming,
        backing: Backing::TerminalConf,
        kind: ValueKind::Token,
        doc: "The terminal cursor's height; `application` follows the app's \
              caret.",
    },
    SettingSpec {
        field: "cursor_blink",
        section: Section::Terminal,
        storage: StorageClass::Roaming,
        backing: Backing::TerminalConf,
        kind: ValueKind::Token,
        doc: "Whether the terminal cursor blinks; `application` follows the \
              app.",
    },
    SettingSpec {
        field: "composer_default",
        section: Section::Terminal,
        storage: StorageClass::Roaming,
        backing: Backing::TerminalConf,
        kind: ValueKind::Flag,
        doc: "Whether a **new** terminal window opens with its composer \
              showing. Whether an existing window's composer is open is that \
              window's furniture and stays its `input_area` row.",
    },
    // --------------------------------------------- AI, delegated: ai.db
    SettingSpec {
        field: "selected_provider",
        section: Section::Ai,
        storage: StorageClass::Delegated,
        backing: Backing::AiAuthority,
        kind: ValueKind::Nullable,
        doc: "Which provider answers. Absent until somebody has chosen one — \
              `ai.db` deliberately has no default row.",
    },
    SettingSpec {
        field: "openai_model",
        section: Section::Ai,
        storage: StorageClass::Delegated,
        backing: Backing::AiAuthority,
        kind: ValueKind::Nullable,
        doc: "The OpenAI model. `null` clears it.",
    },
    SettingSpec {
        field: "openai_reasoning",
        section: Section::Ai,
        storage: StorageClass::Delegated,
        backing: Backing::AiAuthority,
        kind: ValueKind::Token,
        doc: "How much reasoning OpenAI is asked for.",
    },
    SettingSpec {
        field: "openai_credential_configured",
        section: Section::Ai,
        storage: StorageClass::Delegated,
        backing: Backing::AiAuthority,
        kind: ValueKind::Flag,
        doc: "**Read-only.** Whether a key is in the OS vault. The key itself \
              is write-only and does not pass through this authority in \
              either direction; the vault stays `koru-ai-provider`'s.",
    },
    SettingSpec {
        field: "ollama_endpoint",
        section: Section::Ai,
        storage: StorageClass::Delegated,
        backing: Backing::AiAuthority,
        kind: ValueKind::Text,
        doc: "Where the local Ollama server is. Validated to an http/https \
              URL with a host and no userinfo, query or fragment — the same \
              shape `ai.db`'s repository enforces, checked here so the \
              authority refuses before the worker has to.",
    },
    SettingSpec {
        field: "ollama_model",
        section: Section::Ai,
        storage: StorageClass::Delegated,
        backing: Backing::AiAuthority,
        kind: ValueKind::Nullable,
        doc: "The Ollama model. `null` clears it.",
    },
    SettingSpec {
        field: "ollama_reasoning",
        section: Section::Ai,
        storage: StorageClass::Delegated,
        backing: Backing::AiAuthority,
        kind: ValueKind::Token,
        doc: "How much reasoning Ollama is asked for.",
    },
];

/// Every row in one section.
pub fn settings_in(section: Section) -> impl Iterator<Item = &'static SettingSpec> {
    REGISTRY.iter().filter(move |spec| spec.section == section)
}

/// What this authority deliberately does **not** own.
///
/// Written down rather than left implicit, because "why is this not a setting?"
/// is the question that turns a closed registry back into a property bag. Each
/// entry names the thing and the criterion that excludes it.
pub const EXEMPTIONS: &[(&str, &str)] = &[
    (
        "renderer.conf",
        "Machine-survival state, read before any service exists — if the \
         renderer choice needed the Gateway, a machine that cannot start the \
         renderer could never be told to use a different one. The Settings \
         surface writes it through a narrow authority passthrough rather than \
         through the registry.",
    ),
    (
        "the voice-consent record",
        "A revocable legal record, not a preference. It stays \
         `koru-voice-consent`'s; the Settings surface's revoke row acts \
         through that crate.",
    ),
    (
        "AI secrets",
        "The OS vault's, and write-only. The API returns configured-state and \
         never a value or a vault label.",
    ),
    (
        "window geometry, pinned-on-top, `input_area`, pane and tab state",
        "Per-window furniture and session state. The criterion is whether two \
         windows could reasonably disagree: if they can, it is not a setting.",
    ),
    (
        "`opened.*`, `docpref.*`, `hide_completed`, `show_archived`, `search.conf`",
        "Per-document and per-view state. `opened.*` is misfiled in the \
         preferences file today; that is recorded rather than inherited \
         silently.",
    ),
    (
        "`emoji_recents`, `last_export_dir`",
        "Derived state that happened to share a file with preferences. They \
         move to the host's profile-slot settings file in SP-6b, so the \
         roaming file can have one owner without admitting derived state to \
         the registry.",
    ),
    (
        "`global_search_height`",
        "Geometry, and only ever in the preferences file by adjacency. It \
         moves to a `windows.conf` row under the Agent's existing \
         `global-search` key in SP-6b.",
    ),
];
