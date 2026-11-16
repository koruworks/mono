// SPDX-License-Identifier: MPL-2.0

//! **`koru-settings-ui`**: its surfaces, controls and support, a record each,
//! written from the source they name. `registry/MANIFEST.md` lists the
//! files read, the modules excluded and why, and what building each
//! surface needs.

use super::*;

const OWNER: &str = "koru-settings-ui";

const ACTION: &str = "crates/koru-settings-ui/src/action.rs";
const LIB: &str = "crates/koru-settings-ui/src/lib.rs";
const PAGE: &str = "crates/koru-settings-ui/src/page.rs";
const STATE: &str = "crates/koru-settings-ui/src/state.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: new(sections) is pure apart from reading theme process
    // globals: SettingsLinks::disabled() (never dials), a TextEngine and
    // TextInput, theme::default_view_mode and default_canvas_bg;
    // with_links(sections, links) adopts a link that runs its own worker, and
    // for_terminal(segment, root) opens one with SettingsLinks::open. The live
    // preference rows read and, on press, write koru_draw::theme process
    // globals, so pressing a control in a story changes the app's theme state.
    record(
        "C-SETTINGS-SURFACE",
        "SettingsSurface",
        Key::UiLabSettingsPagesName,
        Key::UiLabSettingsPagesPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_settings_ui", LIB),
        [
            (Platform::Windows, Availability::Existing),
            (Platform::MacOs, Availability::SharedSourceUnverified),
            (Platform::Linux, Availability::SharedSourceUnverified),
            (Platform::Android, Availability::Unavailable),
            (Platform::Ios, Availability::Unavailable),
        ],
        &[
            painter("koru_settings_ui::SettingsSurface"),
            painter_with(
                "koru_settings_ui::SettingsSurface::new",
                &[values(
                    Key::UiLabControlMode,
                    "koru_settings_ui::Sections",
                    LIB,
                    Declared::Functions,
                    &[literal("koru"), literal("terminal")],
                )],
            ),
            painter("koru_settings_ui::SettingsSurface::with_links"),
            painter("koru_settings_ui::SettingsSurface::for_terminal"),
            painter("koru_settings_ui::SettingsSurface::draw"),
            painter("koru_settings_ui::SettingsSurface::draw_host_section"),
            support_with(
                "koru_settings_ui::SettingsSurface::show_page",
                &[values(
                    Key::UiLabControlValues,
                    "koru_settings_ui::PageId",
                    PAGE,
                    Declared::Enum,
                    &[
                        labelled("Appearance", Key::SettingsAppearance),
                        labelled("Editor", Key::SettingsEditor),
                        labelled("Reminders", Key::ReminderTitle),
                        labelled("Terminal", Key::SettingsTerminal),
                        labelled("System", Key::SettingsSystem),
                        labelled("AgentProviders", Key::SettingsAgentProviders),
                    ],
                )],
            ),
            support("koru_settings_ui::SettingsSurface::page"),
            support("koru_settings_ui::SettingsSurface::set_lang"),
            support("koru_settings_ui::SettingsSurface::set_viewport"),
            support("koru_settings_ui::SettingsSurface::relayout"),
            support("koru_settings_ui::SettingsSurface::scroll_by"),
            support("koru_settings_ui::SettingsSurface::mouse_move"),
            support("koru_settings_ui::SettingsSurface::mouse_down"),
            support("koru_settings_ui::SettingsSurface::mouse_up"),
            support("koru_settings_ui::SettingsSurface::key"),
            support("koru_settings_ui::SettingsSurface::cursor_icon"),
            support("koru_settings_ui::SettingsSurface::focus_search"),
            support("koru_settings_ui::SettingsSurface::dismiss_menu"),
            support("koru_settings_ui::SettingsSurface::poll"),
            support_with(
                "koru_settings_ui::SettingsSurface::set_availability",
                &[values(
                    Key::UiLabControlState,
                    "koru_settings_ui::Availability",
                    STATE,
                    Declared::Enum,
                    &[
                        labelled("Live", Key::UiLabOptionConnected),
                        labelled("Down", Key::UiLabOptionDisconnected),
                    ],
                )],
            ),
            support("koru_settings_ui::SettingsSurface::adopt_global"),
            support("koru_settings_ui::SettingsSurface::adopt_terminal"),
            support("koru_settings_ui::SettingsSurface::take_requests"),
            support("koru_settings_ui::SettingsSurface::notice"),
            support("koru_settings_ui::SettingsSurface::host_section_rect"),
            support("koru_settings_ui::SettingsSurface::set_host_section_height"),
            support("koru_settings_ui::Sections"),
            support("koru_settings_ui::Sections::koru"),
            support("koru_settings_ui::Sections::terminal"),
            support("koru_settings_ui::Sections::pages"),
            support("koru_settings_ui::PageId"),
            support("koru_settings_ui::PageId::label"),
            support("koru_settings_ui::PageGroup"),
            support_with(
                "koru_settings_ui::Request",
                &[values(
                    Key::UiLabControlValues,
                    "koru_settings_ui::Request",
                    ACTION,
                    Declared::Enum,
                    &[
                        literal("MoreFonts"),
                        literal("ApplyLayoutToAllDocuments"),
                        literal("SetPinnedShowTitle"),
                        literal("CycleCanvasBackground"),
                        literal("SetCloseBehavior"),
                        literal("SetRenderer"),
                        literal("SetVoiceLanguage"),
                        literal("SetVoiceInput"),
                        literal("SetVoiceTier"),
                        literal("SetVoiceGesture"),
                        literal("SetSpellcheckEnabled"),
                        literal("SetSpellcheckLanguage"),
                        literal("ClearPersonalDictionary"),
                        literal("DownloadDictionary"),
                        literal("InstallVoiceModels"),
                        literal("RemoveVoiceModels"),
                        literal("AskVoiceDevices"),
                        literal("RevokeVoiceConsent"),
                        literal("OpenAiSetup"),
                        literal("OpenSampleDocument"),
                        literal("SetTelemetryConsent"),
                        literal("SetAppMenuEntry"),
                        literal("SetAgentEnabled"),
                        literal("RestoreAgentDefaults"),
                        literal("RefreshAgentStatus"),
                        literal("OpenAgentGuide"),
                        literal("InstallAgent"),
                        literal("UpdateAgent"),
                        literal("SetAgentPermission"),
                        literal("SetDefaultAgent"),
                        literal("SetAgentOrder"),
                    ],
                )],
            ),
            support_with(
                "koru_settings_ui::FontRole",
                &[values(
                    Key::UiLabControlKind,
                    "koru_settings_ui::FontRole",
                    ACTION,
                    Declared::Enum,
                    &[
                        literal("App"),
                        literal("Body"),
                        literal("Headings"),
                        literal("Code"),
                        literal("Chat"),
                    ],
                )],
            ),
            support("koru_settings_ui::Availability"),
            support("koru_settings_ui::Notice"),
            support("koru_settings_ui::SettingsSurface::maintenance_deadline"),
            support("koru_settings_ui::SettingsSurface::set_spelling"),
        ],
    ),
];
