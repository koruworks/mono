// SPDX-License-Identifier: MPL-2.0

//! **The workspace window's container** (`apps/koru/src/mainview`): the
//! caption band and tab strip, the sidebar lane, the pane splits, and every
//! menu, prompt, modal and floating card the container stacks over the
//! active tab. `MainView` itself is the app's `koru_workspace_host::View`:
//! routing, and the order a frame goes down in, recorded as support. The
//! container's orchestration modules — sessions, runs, canvas chains,
//! reminders' drains, navigation — are excluded by reason in the module
//! baseline. `registry/MANIFEST.md` lists how deeply each file was read.

use super::*;

const AGENT_CARDS: &str = "apps/koru/src/mainview/agent_cli_updates.rs";
const CONTEXT_MENU: &str = "apps/koru/src/mainview/context_menu.rs";
const DELEGATE_MODAL: &str = "apps/koru/src/mainview/delegate/modal.rs";
const DELEGATE_PAINT: &str = "apps/koru/src/mainview/delegate/paint.rs";
const DICTATION: &str = "apps/koru/src/mainview/dictation.rs";
const DRAW: &str = "apps/koru/src/mainview/draw.rs";
const ERROR_DIALOG: &str = "apps/koru/src/mainview/error_dialog.rs";
const FILE_PREVIEW: &str = "apps/koru/src/mainview/file_preview.rs";
const FONT_PICKER: &str = "apps/koru/src/mainview/font_picker.rs";
const IMPORT_DIALOG: &str = "apps/koru/src/mainview/import_dialog.rs";
const LINK_DEVICE: &str = "apps/koru/src/mainview/link_device.rs";
const MAINVIEW: &str = "apps/koru/src/mainview/mod.rs";
const MOVE_TO: &str = "apps/koru/src/mainview/move_to.rs";
const NEW_DOCUMENT: &str = "apps/koru/src/mainview/new_document.rs";
const PANES: &str = "apps/koru/src/mainview/panes.rs";
const PROFILE_WAIT: &str = "apps/koru/src/mainview/profile_wait.rs";
const QUICK_SWITCH: &str = "apps/koru/src/mainview/quick_switch.rs";
const RECORDING: &str = "apps/koru/src/mainview/recording/paint.rs";
const REMINDER_CREATION: &str = "apps/koru/src/mainview/reminder_creation.rs";
const SNAPSHOT: &str = "apps/koru/src/mainview/snapshot_prompt.rs";
const UPDATE_UI: &str = "apps/koru/src/mainview/update_ui.rs";
const ZOOM: &str = "apps/koru/src/mainview/zoomlabel.rs";

/// Every record of the container, in navigator order.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: starts the whole window: MainView is built by the
    // app's Host over a live workspace, the Agent link and the Store
    record(
        "H-MAIN-VIEW",
        "MainView",
        Key::UiLabMainViewName,
        Key::UiLabMainViewPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(KORU, "koru::mainview", MAINVIEW),
        DESKTOP_PHONES_PARTIAL,
        &[
            support("koru::mainview::MainView"),
            painter("koru::mainview::MainView::draw_frame"),
            support_with(
                "koru::mainview::LiftedViewer",
                &[values(
                    Key::UiLabControlKind,
                    "koru::mainview::LiftedViewer",
                    MAINVIEW,
                    Declared::Enum,
                    &[
                        literal("FileImage"),
                        literal("Document"),
                        literal("AiPane"),
                        literal("AiTab"),
                    ],
                )],
            ),
            support("koru::mainview::ToastPayload"),
            support("koru::mainview::Landing"),
        ],
    ),
    // Construction: pure geometry from koru-shell-chrome's lane solver
    // over the container's state; drawn with the shared icon buttons
    record(
        "C-LANE",
        "KoruLane",
        Key::UiLabLaneName,
        Key::UiLabLanePurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", DRAW),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::mainview::MainView::draw_lane"),
            painter("koru::mainview::MainView::draw_lane_controls"),
            support("koru::mainview::MainView::leading"),
            support("koru::mainview::MainView::lane_controls"),
        ],
    ),
    // Construction: needs the container: the caption band reads the open
    // tabs, the window's state and the store's reachability
    record(
        "C-APP-CAPTION",
        "draw_caption",
        Key::UiLabAppCaptionName,
        Key::UiLabAppCaptionPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", DRAW),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::mainview::MainView::draw_caption"),
            painter("koru::mainview::MainView::draw_caption_band"),
            painter("koru::mainview::MainView::draw_group_strip"),
            painter("koru::mainview::MainView::draw_window_buttons"),
            painter("koru::mainview::MainView::draw_cards"),
            painter("koru::mainview::panes::TabGroup::draw_strip"),
            support("koru::mainview::tab::Tab"),
            support("koru::mainview::tab::TabView"),
        ],
    ),
    // Construction: pure: the pane tree and a drag's split intent
    record(
        "C-PANE-SPLITS",
        "PaneTree",
        Key::UiLabPaneSplitsName,
        Key::UiLabPaneSplitsPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", PANES),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::mainview::panes::draw_split_preview"),
            support("koru::mainview::panes::PaneTree"),
            support("koru::mainview::panes::TabGroup"),
            support("koru::mainview::panes::SplitIntent"),
        ],
    ),
    // Construction: pure: a zoom percentage and its fade
    record(
        "C-APP-ZOOM-FLASH",
        "ZoomLabel",
        Key::UiLabAppZoomFlashName,
        Key::UiLabAppZoomFlashPurpose,
        Family::Controls,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", ZOOM),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::mainview::MainView::draw_zoom_label"),
            support("koru::mainview::zoomlabel::ZoomLabel"),
        ],
    ),
    // Construction: pure: the window's open tabs, most recently used first,
    // as the container builds them; Ctrl+Tab stands it up over the scrim
    record(
        "C-QUICK-SWITCH",
        "QuickSwitch",
        Key::UiLabQuickSwitchName,
        Key::UiLabQuickSwitchPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", QUICK_SWITCH),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::mainview::quick_switch::QuickSwitch::draw"),
            support("koru::mainview::quick_switch::QuickSwitch"),
            support("koru::mainview::quick_switch::Entry"),
        ],
    ),
    // Construction: pure: the host's ErrorReport and its actions
    record(
        "C-ERROR-DIALOG",
        "ErrorDialog",
        Key::UiLabErrorDialogName,
        Key::UiLabErrorDialogPurpose,
        Family::Controls,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", ERROR_DIALOG),
        DESKTOP_PHONES_PARTIAL,
        &[
            painter("koru::mainview::error_dialog::ErrorDialog"),
            painter("koru::mainview::MainView::draw_error"),
            support_with(
                "koru::mainview::error_dialog::Outcome",
                &[values(
                    Key::UiLabControlState,
                    "koru::mainview::error_dialog::Outcome",
                    ERROR_DIALOG,
                    Declared::Enum,
                    &[
                        literal("Keep"),
                        literal("Close"),
                        literal("Copy"),
                        literal("Act"),
                    ],
                )],
            ),
        ],
    ),
    // Construction: fetches the file through the gateway client and saves
    // or opens it through the system on request
    record(
        "C-CHAT-FILE-PREVIEW",
        "FilePreview",
        Key::UiLabChatFilePreviewName,
        Key::UiLabChatFilePreviewPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &["FilePreview"],
        source(KORU, "koru::mainview", FILE_PREVIEW),
        DESKTOP_NOT_ON_PHONES,
        &[painter("koru::mainview::file_preview::FilePreview")],
    ),
    // Construction: asks the Agent for a pairing code and draws its QR
    record(
        "C-LINK-DEVICE-PANEL",
        "LinkDevice",
        Key::UiLabLinkDevicePanelName,
        Key::UiLabLinkDevicePanelPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &["QRFloatingPanel"],
        source(KORU, "koru::mainview", LINK_DEVICE),
        DESKTOP_PHONES_PARTIAL,
        &[
            painter("koru::mainview::link_device::LinkDevice"),
            painter("koru::mainview::MainView::draw_link_device"),
            support("koru::mainview::link_device::Tab"),
            support("koru::mainview::link_device::Outcome"),
        ],
    ),
    // Construction: needs the host's provider readiness and agent catalog;
    // submitting starts a gateway worker (delegate/job.rs)
    record(
        "C-DELEGATE-FORM",
        "Delegation",
        Key::UiLabDelegateFormName,
        Key::UiLabDelegateFormPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &["KoruDelegateForm"],
        source(KORU, "koru::mainview", DELEGATE_PAINT),
        DESKTOP_ONLY,
        &[
            painter("koru::mainview::delegate::paint::draw"),
            support("koru::mainview::delegate::modal::Delegation"),
            support_with(
                "koru::mainview::delegate::modal::Phase",
                &[values(
                    Key::UiLabControlState,
                    "koru::mainview::delegate::modal::Phase",
                    DELEGATE_MODAL,
                    Declared::Enum,
                    &[
                        literal("Editing"),
                        literal("Sending"),
                        literal("Scheduling"),
                        literal("Done"),
                        literal("Scheduled"),
                        literal("Refused"),
                    ],
                )],
            ),
        ],
    ),
    // Construction: pure setup state; Record starts the screen recorder
    record(
        "C-RECORDING-SETUP",
        "recording::modal::Modal",
        Key::UiLabRecordingSetupName,
        Key::UiLabRecordingSetupPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", RECORDING),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::mainview::recording::paint::draw"),
            painter("koru::mainview::recording::paint::draw_countdown"),
            support("koru::mainview::recording::modal::Modal"),
        ],
    ),
    // Construction: rows from the Agent's coding-agent CLI update stages
    record(
        "C-AGENT-CLI-CARDS",
        "Cards",
        Key::UiLabAgentCliCardsName,
        Key::UiLabAgentCliCardsPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", AGENT_CARDS),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::mainview::MainView::draw_agent_cli_cards"),
            support("koru::mainview::agent_cli_updates::Cards"),
            support("koru::mainview::agent_cli_updates::Card"),
        ],
    ),
    // Construction: lists the system's installed font families
    record(
        "C-FONT-PICKER",
        "FontPickerSurface",
        Key::UiLabFontPickerName,
        Key::UiLabFontPickerPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", FONT_PICKER),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::mainview::font_picker::FontPickerSurface"),
            painter("koru::mainview::MainView::draw_font_picker"),
            support("koru::mainview::font_picker::Action"),
        ],
    ),
    // Construction: pure: an import preview the host prepared
    record(
        "C-IMPORT-DIALOG",
        "ImportDialog",
        Key::UiLabImportDialogName,
        Key::UiLabImportDialogPurpose,
        Family::Controls,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", IMPORT_DIALOG),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::mainview::import_dialog::ImportDialog"),
            support_with(
                "koru::mainview::import_dialog::Outcome",
                &[values(
                    Key::UiLabControlState,
                    "koru::mainview::import_dialog::Outcome",
                    IMPORT_DIALOG,
                    Declared::Enum,
                    &[literal("Keep"), literal("Choose")],
                )],
            ),
        ],
    ),
    // Construction: pure: the workspace's documents as a destination tree
    record(
        "C-MOVE-TO",
        "MoveToSurface",
        Key::UiLabMoveToName,
        Key::UiLabMoveToPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", MOVE_TO),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::mainview::move_to::MoveToSurface"),
            support("koru::mainview::move_to::Destination"),
            support("koru::mainview::move_to::WorkspaceTree"),
        ],
    ),
    // Construction: pure: a text field each, on the shared dialog frame or
    // in place
    record(
        "C-NAME-PROMPTS",
        "NewDocument",
        Key::UiLabNamePromptsName,
        Key::UiLabNamePromptsPurpose,
        Family::Controls,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", NEW_DOCUMENT),
        DESKTOP_PHONES_PARTIAL,
        &[
            painter("koru::mainview::new_document::NewDocument"),
            painter("koru::mainview::snapshot_prompt::SnapshotPrompt"),
            painter("koru::mainview::workspace_rename::WorkspaceRename"),
            painter("koru::mainview::MainView::draw_new_list"),
            support_with(
                "koru::mainview::snapshot_prompt::Mode",
                &[values(
                    Key::UiLabControlMode,
                    "koru::mainview::snapshot_prompt::Mode",
                    SNAPSHOT,
                    Declared::Enum,
                    &[literal("Save"), literal("Rename")],
                )],
            ),
        ],
    ),
    // Construction: rows from the Agent's reminder state; the editor's
    // reminder requests are drained into it by mainview/reminders.rs
    record(
        "C-REMINDER-EDITOR",
        "reminder_creation::Surface",
        Key::UiLabReminderEditorName,
        Key::UiLabReminderEditorPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", REMINDER_CREATION),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::mainview::reminder_creation::Surface"),
            painter("koru::mainview::reminder_management::Management"),
            painter("koru::mainview::reminder_shell::Shell"),
            painter("koru::mainview::reminders_view::RemindersView"),
        ],
    ),
    // Construction: rows from the Agent's update feed
    record(
        "C-UPDATE-PROMPTS",
        "draw_update_banner",
        Key::UiLabUpdatePromptsName,
        Key::UiLabUpdatePromptsPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", UPDATE_UI),
        desktop_and(Availability::NativePartial, Availability::NotApplicable),
        &[
            painter("koru::mainview::MainView::draw_update_banner"),
            support("koru::mainview::update_ui::Action"),
        ],
    ),
    // Construction: pure geometry over the dictation state the Agent sends
    record(
        "C-DICTATION-STRIP",
        "Listening",
        Key::UiLabDictationStripName,
        Key::UiLabDictationStripPurpose,
        Family::Controls,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", DICTATION),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::mainview::dictation::Listening"),
            support("koru::mainview::dictation::Geometry"),
            support("koru::mainview::dictation::Metrics"),
        ],
    ),
    // Construction: pure: the Profile the window is waiting for
    record(
        "C-PROFILE-WAIT",
        "WaitPane",
        Key::UiLabProfileWaitName,
        Key::UiLabProfileWaitPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", PROFILE_WAIT),
        DESKTOP_NOT_ON_PHONES,
        &[painter("koru::mainview::profile_wait::WaitPane")],
    ),
    // Construction: the container's menus are koru-ui's Menu over the
    // app's own action vocabulary, opened by the container
    record(
        "C-APP-MENUS",
        "ContextMenu",
        Key::UiLabAppMenusName,
        Key::UiLabAppMenusPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mainview", CONTEXT_MENU),
        DESKTOP_PHONES_PARTIAL,
        &[
            painter("koru::mainview::MainView::draw_menu"),
            painter("koru::mainview::MainView::draw_place_chooser"),
            painter("koru::mainview::MainView::draw_launcher"),
            support("koru::mainview::context_menu::ContextMenu"),
            support("koru::mainview::context_menu::MenuAction"),
            support("koru::mainview::context_menu::MenuSubject"),
            support("koru::mainview::launcher::LauncherAsk"),
            support("koru::mainview::launcher::NewKind"),
        ],
    ),
];
