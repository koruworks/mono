// SPDX-License-Identifier: MPL-2.0

//! **The workspace app's own pages and tab views**: the Welcome page, the
//! Manage tab and its pages, the Trash, the feedback form, the version
//! comparison, the canvas tab and its text dialog, the support chat and its
//! floating button, the recovered-terminal panel, the Reminders settings
//! section, the pop-out window's close question, the Windows Jump List, and
//! the tab views that only host a domain crate's surface. Each record names
//! where the app draws it; `registry/MANIFEST.md` lists how deeply each
//! folder was read and what was left out.

use super::*;

const CANVAS_VIEW: &str = "apps/koru/src/canvas_view.rs";
const DIALOG: &str = "apps/koru/src/canvas_view/dialog.rs";
const DIFF: &str = "apps/koru/src/diff/mod.rs";
const FEEDBACK: &str = "apps/koru/src/feedback.rs";
const MANAGE: &str = "apps/koru/src/manage.rs";
const MANAGE_CLOUD: &str = "apps/koru/src/manage/cloud.rs";
const MANAGE_COPY: &str = "apps/koru/src/manage/cloud/copy.rs";
const MANAGE_DEVICES: &str = "apps/koru/src/manage/devices.rs";
const MANAGE_DIALOGS: &str = "apps/koru/src/manage/dialogs.rs";
const MANAGE_PAGE: &str = "apps/koru/src/manage/page.rs";
const MANAGE_WORKSPACES: &str = "apps/koru/src/manage/workspaces.rs";
const MANAGER_VIEW: &str = "apps/koru/src/manager_view.rs";
const SESSION_OFFER: &str = "apps/koru/src/session_offer.rs";
const SETTINGS_REMINDERS: &str = "apps/koru/src/settings_view/reminders.rs";
const SUPPORT_FAB: &str = "apps/koru/src/support/fab.rs";
const SUPPORT_SESSION: &str = "apps/koru/src/support/session.rs";
const SUPPORT_VIEW: &str = "apps/koru/src/support/view.rs";
const TASKBAR: &str = "apps/koru/src/taskbar.rs";
const TAB_HOSTS: &str = "apps/koru/src/ai_chat_view.rs";
const TRASH_VIEW: &str = "apps/koru/src/trash/view.rs";
const WELCOME: &str = "apps/koru/src/welcome/mod.rs";
const WELCOME_ROWS: &str = "apps/koru/src/welcome/rows.rs";

/// Every record of the app's pages and tab views, in navigator order.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: pure state over the workspace's name; its rows hand
    // wants (import, files, feedback, MCP copy) to the container
    record(
        "C-APP-WELCOME",
        "WelcomeView",
        Key::UiLabAppWelcomeName,
        Key::UiLabAppWelcomePurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &["KoruWelcome"],
        source(KORU, "koru::welcome", WELCOME),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::welcome::WelcomeView"),
            painter("koru::welcome::draw::draw_page"),
            support("koru::welcome::layout::measure"),
            support("koru::welcome::Target"),
            support("koru::welcome::WelcomeWant"),
            support("koru::welcome::rows::RowId"),
            support_with(
                "koru::welcome::rows::Section",
                &[values(
                    Key::UiLabControlKind,
                    "koru::welcome::rows::Section",
                    WELCOME_ROWS,
                    Declared::Enum,
                    &[
                        literal("FirstMinutes"),
                        literal("Devices"),
                        literal("Further"),
                    ],
                )],
            ),
        ],
    ),
    // Construction: needs the host's profiles controller and the shared
    // koru-workspaces-ui controller; prompts and actions leave as Requests
    record(
        "C-MANAGE",
        "ManageView",
        Key::UiLabManageName,
        Key::UiLabManagePurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &["KoruManage"],
        source(KORU, "koru::manage", MANAGE),
        DESKTOP_PHONES_PARTIAL,
        &[
            painter("koru::manage::ManageView"),
            painter("koru::manage::ManageView::paint_page"),
            painter("koru::manage::ManageView::paint_head"),
            painter("koru::manage::nav::paint_nav"),
            painter("koru::manage::dialogs::paint_dialog"),
            painter("koru::manage::new_profile::paint_new_profile"),
            painter("koru::manage::profile_page::paint::paint_profile_page"),
            painter("koru::manage::sign_in::paint_sign_in"),
            painter("koru::manage::recovery::paint_recovery_sections"),
            support("koru::manage::Request"),
            support_with(
                "koru::manage::page::Page",
                &[values(
                    Key::UiLabControlKind,
                    "koru::manage::page::Page",
                    MANAGE_PAGE,
                    Declared::Enum,
                    &[
                        literal("Profile"),
                        literal("NewProfile"),
                        literal("Devices"),
                        literal("Cloud"),
                    ],
                )],
            ),
            support_with(
                "koru::manage::dialogs::Dialog",
                &[values(
                    Key::UiLabControlKind,
                    "koru::manage::dialogs::Dialog",
                    MANAGE_DIALOGS,
                    Declared::Enum,
                    &[
                        literal("CloudFile"),
                        literal("SignOut"),
                        literal("Unlink"),
                        literal("UnlinkDevice"),
                        literal("Remove"),
                        literal("Offline"),
                        literal("AddGoogle"),
                    ],
                )],
            ),
            support("koru::manage::accessibility::semantics"),
        ],
    ),
    // Construction: a drawing over koru-workspaces-ui's controller, which
    // reads the workspace catalog; the section itself is layout and paint
    record(
        "C-MANAGE-WORKSPACES",
        "WorkspacesSection",
        Key::UiLabManageWorkspacesName,
        Key::UiLabManageWorkspacesPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::manage", MANAGE_WORKSPACES),
        DESKTOP_PHONES_PARTIAL,
        &[
            painter("koru::manage::workspaces::paint_workspaces_section"),
            support("koru::manage::workspaces::WorkspacesSection"),
            support("koru::manage::workspaces::WorkspaceRow"),
            support_with(
                "koru::manage::workspaces::ListKind",
                &[values(
                    Key::UiLabControlKind,
                    "koru::manage::workspaces::ListKind",
                    MANAGE_WORKSPACES,
                    Declared::Enum,
                    &[literal("Active"), literal("Archived"), literal("Trash")],
                )],
            ),
            support("koru::manage::SharedWorkspaces"),
        ],
    ),
    // Construction: rows from the host's linked-device list and relay state
    record(
        "C-MANAGE-DEVICES",
        "DevicesLaid",
        Key::UiLabManageDevicesName,
        Key::UiLabManageDevicesPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::manage", MANAGE_DEVICES),
        DESKTOP_PHONES_PARTIAL,
        &[
            painter("koru::manage::devices::paint_devices_page"),
            painter("koru::manage::devices::history::paint_history"),
            support("koru::manage::devices::DevicesLaid"),
            support("koru::manage::devices::platform_badge"),
            support("koru::manage::DeviceRequest"),
        ],
    ),
    // Construction: account, subscription and per-profile sync rows from
    // the Agent's account state; the inventory and copy pages read a
    // Cloud listing the host fetched
    record(
        "C-MANAGE-CLOUD",
        "CloudLaid",
        Key::UiLabManageCloudName,
        Key::UiLabManageCloudPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::manage", MANAGE_CLOUD),
        DESKTOP_ANDROID_PARTIAL,
        &[
            painter("koru::manage::cloud::paint_cloud_page"),
            painter("koru::manage::cloud::progress::disclosure"),
            painter("koru::manage::cloud::inventory::paint_inventory"),
            support("koru::manage::cloud::CloudLaid"),
            support("koru::manage::cloud::copy::Presentation"),
            support_with(
                "koru::manage::cloud::copy::Phase",
                &[values(
                    Key::UiLabControlState,
                    "koru::manage::cloud::copy::Phase",
                    MANAGE_COPY,
                    Declared::Enum,
                    &[
                        literal("Unavailable"),
                        literal("Loading"),
                        literal("Ready"),
                        literal("Removing"),
                        literal("SourceChanged"),
                        literal("OutcomeUnknown"),
                        literal("Acknowledged"),
                    ],
                )],
            ),
        ],
    ),
    // Construction: pure: the rows are the host's trashed entries
    record(
        "C-TRASH-PAGE",
        "TrashView",
        Key::UiLabTrashPageName,
        Key::UiLabTrashPagePurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::trash", TRASH_VIEW),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::trash::TrashView"),
            painter("koru::trash::TrashView::paint"),
            support("koru::trash::Request"),
            support("koru::trash::Entry"),
        ],
    ),
    // Construction: pure form state; Send validates and posts on a worker
    record(
        "C-FEEDBACK-PAGE",
        "FeedbackView",
        Key::UiLabFeedbackPageName,
        Key::UiLabFeedbackPagePurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::feedback", FEEDBACK),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::feedback::FeedbackView"),
            support_with(
                "koru::feedback::Kind",
                &[values(
                    Key::UiLabControlKind,
                    "koru::feedback::Kind",
                    FEEDBACK,
                    Declared::Enum,
                    &[literal("Bug"), literal("Idea"), literal("General")],
                )],
            ),
            support_with(
                "koru::feedback::Frequency",
                &[values(
                    Key::UiLabControlValues,
                    "koru::feedback::Frequency",
                    FEEDBACK,
                    Declared::Enum,
                    &[literal("Once"), literal("Sometimes"), literal("EveryTime")],
                )],
            ),
            support_with(
                "koru::feedback::Importance",
                &[values(
                    Key::UiLabControlValues,
                    "koru::feedback::Importance",
                    FEEDBACK,
                    Declared::Enum,
                    &[
                        literal("Nice"),
                        literal("Significant"),
                        literal("Regular"),
                        literal("Blocking"),
                    ],
                )],
            ),
        ],
    ),
    // Construction: pure: two versions' rows, laid out on open
    record(
        "C-VERSION-COMPARE",
        "DiffView",
        Key::UiLabVersionCompareName,
        Key::UiLabVersionComparePurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::diff", DIFF),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::diff::DiffView"),
            support("koru::diff::Comparison"),
            support("koru::diff::tab_icon"),
        ],
    ),
    // Construction: hosts koru-canvas-ui's CanvasSurface over the canvas
    // document; requests are performed against that document
    record(
        "C-CANVAS-TAB",
        "CanvasView",
        Key::UiLabCanvasTabName,
        Key::UiLabCanvasTabPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::canvas_view", CANVAS_VIEW),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::canvas_view::CanvasView"),
            painter("koru::canvas_view::CanvasView::draw_rename"),
            painter("koru::canvas_view::agents::AgentMenu"),
            support("koru::canvas_view::decor::Decor"),
            support("koru::canvas_view::CanvasAsk"),
            support("koru::canvas_view::accessibility::semantics"),
        ],
    ),
    // Construction: pure: a TextArea and the dialog's geometry
    record(
        "C-CANVAS-TEXT-DIALOG",
        "TextDialog",
        Key::UiLabCanvasTextDialogName,
        Key::UiLabCanvasTextDialogPurpose,
        Family::Controls,
        Kind::AppLocal,
        None,
        &["KoruCanvasTextDialog"],
        source(KORU, "koru::canvas_view", DIALOG),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::canvas_view::dialog::TextDialog"),
            painter("koru::canvas_view::dialog::TextDialog::draw"),
            support("koru::canvas_view::dialog::geometry"),
            support_with(
                "koru::canvas_view::dialog::DialogMode",
                &[values(
                    Key::UiLabControlMode,
                    "koru::canvas_view::dialog::DialogMode",
                    DIALOG,
                    Declared::Enum,
                    &[literal("NewCard"), literal("Edit")],
                )],
            ),
            support_with(
                "koru::canvas_view::dialog::Outcome",
                &[values(
                    Key::UiLabControlState,
                    "koru::canvas_view::dialog::Outcome",
                    DIALOG,
                    Declared::Enum,
                    &[literal("Keep"), literal("Save"), literal("Cancel")],
                )],
            ),
        ],
    ),
    // Construction: hosts koru-webview's WebSurface, which needs a live
    // parent window and starts the system web engine
    record(
        "C-SUPPORT-VIEW",
        "SupportView",
        Key::UiLabSupportViewName,
        Key::UiLabSupportViewPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::support", SUPPORT_VIEW),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::support::view::SupportView"),
            painter("koru::support::view::SupportView::draw"),
            support("koru::support::view::SupportView::native_surface"),
            support("koru::support::surface::HostedWebSurface"),
            support_with(
                "koru::support::session::State",
                &[values(
                    Key::UiLabControlState,
                    "koru::support::session::State",
                    SUPPORT_SESSION,
                    Declared::Enum,
                    &[
                        literal("Idle"),
                        literal("Creating"),
                        literal("Loading"),
                        literal("Booting"),
                        literal("Live"),
                        literal("Failed"),
                        literal("Closed"),
                    ],
                )],
            ),
            support_with(
                "koru::support::session::Mode",
                &[values(
                    Key::UiLabControlMode,
                    "koru::support::session::Mode",
                    SUPPORT_SESSION,
                    Declared::Enum,
                    &[
                        literal("Open"),
                        literal("Fab"),
                        literal("Quiet"),
                        literal("Ended"),
                    ],
                )],
            ),
        ],
    ),
    // Construction: pure; its right-click opens a native popup menu, which
    // must be built on the main thread
    record(
        "C-SUPPORT-FAB",
        "FabView",
        Key::UiLabSupportFabName,
        Key::UiLabSupportFabPurpose,
        Family::Controls,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::support", SUPPORT_FAB),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::support::fab::FabView"),
            painter("koru::support::fab::FabView::draw"),
            support("koru::support::fab::SIZE"),
        ],
    ),
    // Construction: pure: a name, an optional folder and whether the
    // session can resume
    record(
        "C-SESSION-OFFER",
        "Offer",
        Key::UiLabSessionOfferName,
        Key::UiLabSessionOfferPurpose,
        Family::Controls,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::session_offer", SESSION_OFFER),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::session_offer::Offer"),
            painter("koru::session_offer::Offer::draw"),
            support_with(
                "koru::session_offer::Choice",
                &[values(
                    Key::UiLabControlKind,
                    "koru::session_offer::Choice",
                    SESSION_OFFER,
                    Declared::Enum,
                    &[literal("Resume"), literal("StartNew")],
                )],
            ),
        ],
    ),
    // Construction: pure layout over the host's reminder preferences;
    // pressing a control emits an intent the controller saves
    record(
        "C-REMINDER-SETTINGS",
        "ReminderSurface",
        Key::UiLabReminderSettingsName,
        Key::UiLabReminderSettingsPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::settings_view", SETTINGS_REMINDERS),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru::settings_view::reminders::ReminderSurface"),
            painter("koru::settings_view::reminders::ReminderSurface::draw"),
            support("koru::settings_view::reminders::CardSpec"),
        ],
    ),
    // Construction: pure; opened by a pop-out document window's close
    // request when the preference says to ask
    record(
        "C-POPOUT-CLOSE-DIALOG",
        "CloseDialog",
        Key::UiLabPopoutCloseName,
        Key::UiLabPopoutClosePurpose,
        Family::Controls,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::manager_view", MANAGER_VIEW),
        DESKTOP_ONLY,
        &[
            painter("koru::manager_view::CloseDialog"),
            painter("koru::manager_view::CloseDialog::draw"),
            support_with(
                "koru::manager_view::CloseChoice",
                &[values(
                    Key::UiLabControlKind,
                    "koru::manager_view::CloseChoice",
                    MANAGER_VIEW,
                    Declared::Enum,
                    &[literal("Restore"), literal("Close")],
                )],
            ),
            support("koru::manager_view::CloseHit"),
        ],
    ),
    // Construction: the two window kinds the app's Host opens; each holds
    // a MainView or a BuiltInDocument over the host's workspace
    record(
        "H-MANAGER-VIEW",
        "ManagerView",
        Key::UiLabManagerViewName,
        Key::UiLabManagerViewPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(KORU, "koru::manager_view", MANAGER_VIEW),
        DESKTOP_PHONES_PARTIAL,
        &[
            support("koru::manager_view::ManagerView"),
            support("koru::manager_view::DocumentPane"),
        ],
    ),
    // Construction: each view wraps a domain surface (see each crate's
    // record for what building that surface starts); the app adds only a
    // clip, a translate and, for a terminal, a status line
    record(
        "H-APP-TAB-HOSTS",
        "AiChatView",
        Key::UiLabAppTabHostsName,
        Key::UiLabAppTabHostsPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(KORU, "koru", TAB_HOSTS),
        DESKTOP_NOT_ON_PHONES,
        &[
            support("koru::ai_chat_view::AiChatView"),
            support("koru::search::SearchSurface"),
            support("koru::search::SearchSurface::draw_modal"),
            support("koru::settings_view::SettingsView"),
            support("koru::socials_view::SocialsView"),
            support("koru::run_view::RunView"),
            support("koru::result_view::ResultView"),
            support("koru::result_document::ResultDocument"),
            support("koru::session_tab::SessionView"),
            painter("koru::session_tab::draw_label"),
        ],
    ),
    // Construction: Windows only: writes the shell's Jump List through
    // COM; elsewhere every call is a stand-in
    record(
        "C-JUMP-LIST",
        "taskbar::install_jump_list",
        Key::UiLabJumpListName,
        Key::UiLabJumpListPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::taskbar", TASKBAR),
        [
            (Platform::Windows, Availability::NativeOwned),
            (Platform::MacOs, Availability::Unavailable),
            (Platform::Linux, Availability::Unavailable),
            (Platform::Android, Availability::Unavailable),
            (Platform::Ios, Availability::Unavailable),
        ],
        &[
            boundary("koru::taskbar::install_jump_list"),
            support("koru::taskbar::prepare"),
            support_with(
                "koru::taskbar::Activation",
                &[values(
                    Key::UiLabControlKind,
                    "koru::taskbar::Activation",
                    TASKBAR,
                    Declared::Enum,
                    &[
                        literal("NewDocument"),
                        literal("OpenDocument"),
                        literal("Open"),
                        literal("Search"),
                    ],
                )],
            ),
        ],
    ),
];
