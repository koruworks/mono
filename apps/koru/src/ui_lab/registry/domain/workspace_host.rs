// SPDX-License-Identifier: MPL-2.0

//! **`koru-workspace-host`**: the reusable surfaces the workspace app's host
//! paints — first-run setup and the account chooser — its window and view
//! contracts, its screen-reader semantics, native child placement, keyboard
//! ownership, and the two boundaries the operating system owns, its menus
//! and its file dialogs, a record each, written from the source they name.
//! The 21,000-line event loop is orchestration and is recorded as the host
//! contract, not as a component. `registry/MANIFEST.md` lists the files
//! read, the modules excluded and why.
//!
//! **Every path here is a string literal on purpose.** The isolation scan in
//! `ui_lab/tests.rs` refuses this crate's run, app and startup paths in Lab
//! code; this file is on its `RECORD_FILES` list, where only string literals
//! are exempt, so a record may spell what it records while the file still
//! names nothing it could call.

use super::*;

const OWNER: &str = "koru-workspace-host";

const ACCESSIBILITY: &str = "crates/koru-workspace-host/src/accessibility.rs";
const ACCOUNT_UI: &str = "crates/koru-workspace-host/src/account_ui.rs";
const DIALOGS: &str = "crates/koru-workspace-host/src/dialogs.rs";
const LIB: &str = "crates/koru-workspace-host/src/lib.rs";
const MENUS: &str = "crates/koru-workspace-host/src/menus.rs";
const NATIVE_SURFACE: &str = "crates/koru-workspace-host/src/native_surface.rs";
const ONBOARDING: &str = "crates/koru-workspace-host/src/onboarding.rs";
const ROUTING: &str = "crates/koru-workspace-host/src/routing.rs";
const SWITCHER: &str = "crates/koru-workspace-host/src/account_ui/switcher.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: pure: Default builds its own TextEngine, text inputs and
    // emoji and color panel; every sign-in, account and Profile step leaves
    // as an Action the host performs
    record(
        "C-ONBOARDING",
        "OnboardingView",
        Key::UiLabOnboardingName,
        Key::UiLabOnboardingPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[
            "CodeBoxes",
            "PasswordField",
            "EmailAddressField",
            "EmailPasswordField",
            "NameField",
            "ProfileIcon",
            "ProfileNameField",
            "WorkspaceIcon",
            "WorkspaceNameField",
        ],
        source(OWNER, "koru_workspace_host::onboarding", ONBOARDING),
        DESKTOP_SHARED,
        &[
            painter("koru_workspace_host::onboarding::OnboardingView"),
            painter("koru_workspace_host::onboarding::OnboardingView::draw"),
            support("koru_workspace_host::onboarding::OnboardingView::set_viewport"),
            support("koru_workspace_host::onboarding::OnboardingView::set_embedded"),
            support("koru_workspace_host::onboarding::OnboardingView::set_embedded_visible_area"),
            support("koru_workspace_host::onboarding::OnboardingView::set_custom_caption"),
            support("koru_workspace_host::onboarding::OnboardingView::set_maximized"),
            support("koru_workspace_host::onboarding::OnboardingView::caption_action_at"),
            support("koru_workspace_host::onboarding::OnboardingView::take_action"),
            support("koru_workspace_host::onboarding::OnboardingView::take_telemetry"),
            support("koru_workspace_host::onboarding::OnboardingView::show_account_state"),
            support("koru_workspace_host::onboarding::OnboardingView::show_account_profiles"),
            support("koru_workspace_host::onboarding::OnboardingView::show_profile"),
            support("koru_workspace_host::onboarding::OnboardingView::mouse_down"),
            support("koru_workspace_host::onboarding::OnboardingView::mouse_move"),
            support("koru_workspace_host::onboarding::OnboardingView::mouse_up"),
            support("koru_workspace_host::onboarding::OnboardingView::scroll_by"),
            support("koru_workspace_host::onboarding::OnboardingView::key"),
            support("koru_workspace_host::onboarding::OnboardingView::cursor_icon"),
            support("koru_workspace_host::onboarding::OnboardingView::focused_form_rect"),
            support("koru_workspace_host::onboarding::OnboardingView::embedded_content_height"),
            support("koru_workspace_host::onboarding::OnboardingView::caret_blink_deadline"),
            support("koru_workspace_host::onboarding::OnboardingView::accessibility_semantics"),
            support("koru_workspace_host::onboarding::OnboardingView::accessibility_focus"),
            support("koru_workspace_host::onboarding::OnboardingView::accessibility_action"),
            support("koru_workspace_host::onboarding::OnboardingView::semantics_revision"),
            support("koru_workspace_host::onboarding::OnboardingView::window_command_bounds"),
            support("koru_workspace_host::onboarding::Action"),
            support("koru_workspace_host::onboarding::Telemetry"),
            support_with(
                "koru_workspace_host::onboarding::Purpose",
                &[values(
                    Key::UiLabControlKind,
                    "koru_workspace_host::onboarding::Purpose",
                    ONBOARDING,
                    Declared::Enum,
                    &[literal("Work"), literal("School"), literal("Personal")],
                )],
            ),
            support("koru_workspace_host::onboarding::accessibility::parts"),
        ],
    ),
    // Construction: pure: Default; the rows are the host's AccountState
    record(
        "C-ACCOUNT-SWITCHER",
        "AccountSwitcher",
        Key::UiLabAccountSwitcherName,
        Key::UiLabAccountSwitcherPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_workspace_host::account_ui::switcher", SWITCHER),
        DESKTOP_SHARED,
        &[
            painter("koru_workspace_host::account_ui::switcher::AccountSwitcher"),
            painter("koru_workspace_host::account_ui::switcher::AccountSwitcher::draw"),
            support("koru_workspace_host::account_ui::switcher::AccountSwitcher::click"),
            support("koru_workspace_host::account_ui::switcher::AccountSwitcher::key"),
            support("koru_workspace_host::account_ui::switcher::AccountSwitcher::scroll"),
        ],
    ),
    // Construction: pure projections of the Agent's account state; nothing
    // here opens a browser, the network, a keyring or the store
    record(
        "H-ACCOUNT-STATE",
        "account_ui::project",
        Key::UiLabAccountStateName,
        Key::UiLabAccountStatePurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_workspace_host::account_ui", ACCOUNT_UI),
        DESKTOP_SHARED,
        &[
            support("koru_workspace_host::account_ui::project"),
            support("koru_workspace_host::account_ui::project_for_lang"),
            support("koru_workspace_host::account_ui::Surface"),
            support_with(
                "koru_workspace_host::account_ui::SurfaceKind",
                &[values(
                    Key::UiLabControlState,
                    "koru_workspace_host::account_ui::SurfaceKind",
                    ACCOUNT_UI,
                    Declared::Enum,
                    &[
                        literal("AccountCreation"),
                        literal("WaitingForDevice"),
                        literal("WaitingForBrowser"),
                        literal("SignedOut"),
                        literal("Finishing"),
                        literal("SignedIn"),
                        literal("NotConfigured"),
                        literal("Unavailable"),
                        literal("AccessRevoked"),
                        literal("Reauthenticate"),
                        literal("DeviceVerification"),
                        literal("Persistence"),
                        literal("Collision"),
                        literal("ErasedAccount"),
                        literal("OlderPeer"),
                    ],
                )],
            ),
            support("koru_workspace_host::account_ui::Action"),
            support("koru_workspace_host::account_ui::CommandSubmissionId"),
            support("koru_workspace_host::account_ui::CommandSubmissionId::next"),
            support("koru_workspace_host::account_ui::CommandFailure"),
            support("koru_workspace_host::account_ui::CommandAdmission"),
            support("koru_workspace_host::account_ui::state_for_profile"),
            support("koru_workspace_host::account_ui::not_configured"),
            support("koru_workspace_host::account_ui::profile_connection_pending"),
            support("koru_workspace_host::account_ui::command_failure_surface"),
            support("koru_workspace_host::account_ui::is_credential_repair_code"),
            support("koru_workspace_host::account_ui::actionable_failure_code"),
            support("koru_workspace_host::account_ui::selection_result_rows"),
        ],
    ),
    // Construction: starts the whole workspace app: run and run_with_args
    // read settings, find a Profile, connect to the Agent and the Store and
    // open windows; the Lab is dispatched before it and may never call it.
    // View and Host are traits the application implements
    record(
        "H-VIEW-HOST",
        "View",
        Key::UiLabViewHostName,
        Key::UiLabViewHostPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_workspace_host", LIB),
        DESKTOP_SHARED,
        &[
            support("koru_workspace_host::View"),
            support("koru_workspace_host::Host"),
            support("koru_workspace_host::run"),
            support("koru_workspace_host::run_with_args"),
            support("koru_workspace_host::ImeTarget"),
            support("koru_workspace_host::ImeTargetId"),
            support("koru_workspace_host::ChildDocumentRequest"),
            support_with(
                "koru_workspace_host::TabPresence",
                &[values(
                    Key::UiLabControlState,
                    "koru_workspace_host::TabPresence",
                    LIB,
                    Declared::Enum,
                    &[literal("Pinned"), literal("Open"), literal("Absent")],
                )],
            ),
            support("koru_workspace_host::PaneRequest"),
            support("koru_workspace_host::NavigationTarget"),
            support("koru_workspace_host::ZoomIntent"),
            support("koru_workspace_host::ErrorReport"),
            support("koru_workspace_host::ErrorAction"),
            support("koru_workspace_host::RichBlock"),
            support("koru_workspace_host::RichTextBlock"),
            support("koru_workspace_host::RichTextBlock::from_text"),
            support("koru_workspace_host::RichTextRun"),
            support("koru_workspace_host::RichTextLink"),
            support("koru_workspace_host::RichTableBlock"),
            support("koru_workspace_host::RichTextKind"),
            support("koru_workspace_host::RichListSpec"),
            support("koru_workspace_host::PastedImage"),
            support("koru_workspace_host::PasteTarget"),
            support("koru_workspace_host::ImageTicket"),
            support("koru_workspace_host::ImagePlace"),
            support("koru_workspace_host::ImageMipSide"),
            support("koru_workspace_host::app::Wake"),
            support("koru_workspace_host::app::WindowDrag"),
            support("koru_workspace_host::app::WindowDrag::begin"),
            support("koru_workspace_host::app::tasks_debug_log"),
        ],
    ),
    // Construction: pure values; the composition and mini-app hosts in
    // apps/koru name seven of them to publish their declared controls
    record(
        "H-WORKSPACE-ACCESSIBILITY",
        "Occurrence",
        Key::UiLabWorkspaceAccessibilityName,
        Key::UiLabWorkspaceAccessibilityPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_workspace_host::accessibility", ACCESSIBILITY),
        DESKTOP_SHARED,
        &[
            host_only("koru_workspace_host::accessibility::ControlPart"),
            host_only("koru_workspace_host::accessibility::Occurrence"),
            host_only("koru_workspace_host::accessibility::SemanticNode"),
            host_only("koru_workspace_host::accessibility::ViewSlot"),
            host_only("koru_workspace_host::accessibility::WindowSlot"),
            host_only("koru_workspace_host::accessibility::ObjectKey"),
            host_only("koru_workspace_host::accessibility::Revision"),
            support("koru_workspace_host::accessibility::Occurrence::locator"),
            support("koru_workspace_host::accessibility::Slots"),
            support("koru_workspace_host::accessibility::part"),
            support("koru_workspace_host::accessibility::identity::LOCATOR_PREFIX"),
            support("koru_workspace_host::accessibility::WindowSemantics"),
            support("koru_workspace_host::accessibility::Projection"),
            support("koru_workspace_host::accessibility::projection::build"),
            support("koru_workspace_host::accessibility::Scope"),
            support("koru_workspace_host::accessibility::Runs"),
            support("koru_workspace_host::accessibility::TextBody"),
            support("koru_workspace_host::accessibility::PaneAccessibility"),
            support("koru_workspace_host::accessibility::PaneAccessibility::new"),
            support("koru_workspace_host::accessibility::Stage"),
            support("koru_workspace_host::accessibility::Poll"),
            support("koru_workspace_host::accessibility::Resolved"),
            support("koru_workspace_host::accessibility::Counters"),
            support("koru_workspace_host::accessibility::Redaction"),
            support("koru_workspace_host::accessibility::dump::render"),
            support("koru_workspace_host::accessibility::dump::armed"),
            support("koru_workspace_host::accessibility::CaptionGeometry"),
            support("koru_workspace_host::accessibility::window_commands"),
            support("koru_workspace_host::accessibility::ui_lang"),
        ],
    ),
    // Construction: needs a live window: SurfaceHost wraps the host's
    // window, and reconcile places a view's native child in it
    record(
        "H-NATIVE-SURFACE",
        "NativeSurface",
        Key::UiLabNativeSurfaceName,
        Key::UiLabNativeSurfacePurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_workspace_host::native_surface", NATIVE_SURFACE),
        DESKTOP_SHARED,
        &[
            support("koru_workspace_host::native_surface::NativeSurface"),
            support("koru_workspace_host::native_surface::Placement"),
            support("koru_workspace_host::native_surface::SurfaceHost"),
            support("koru_workspace_host::native_surface::SurfaceHost::window"),
            support("koru_workspace_host::native_surface::SurfaceHost::waker"),
            support("koru_workspace_host::native_surface::WindowGeometry"),
            support("koru_workspace_host::native_surface::PhysicalRect"),
            support("koru_workspace_host::native_surface::SurfaceError"),
            support("koru_workspace_host::native_surface::to_physical"),
            support("koru_workspace_host::native_surface::inset_for_band"),
            support("koru_workspace_host::native_surface::reconcile"),
            support("koru_workspace_host::native_surface::BackgroundIntent"),
        ],
    ),
    // Construction: on the main thread only: Popup::new builds a native menu
    // (it panics off the main thread) and MenuBar::install seats the Mac's
    // menu bar; elsewhere both are stand-ins that show nothing
    record(
        "C-SYSTEM-MENUS",
        "menus::Popup",
        Key::UiLabSystemMenusName,
        Key::UiLabSystemMenusPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_workspace_host", MENUS),
        [
            (Platform::Windows, Availability::NativeOwned),
            (Platform::MacOs, Availability::NativeOwned),
            (Platform::Linux, Availability::Unavailable),
            (Platform::Android, Availability::NotAudited),
            (Platform::Ios, Availability::NotAudited),
        ],
        &[
            boundary("koru_workspace_host::menus::Popup"),
            boundary("koru_workspace_host::menus::Popup::new"),
            boundary("koru_workspace_host::menus::Popup::show"),
            boundary("koru_workspace_host::menubar::MenuBar"),
            boundary("koru_workspace_host::menubar::MenuBar::install"),
            support("koru_workspace_host::menus::Popup::tag_of"),
            support("koru_workspace_host::menus::MenuId"),
            support("koru_workspace_host::menus::install"),
            support("koru_workspace_host::menus::ingest"),
            support("koru_workspace_host::menus::drain"),
            support("koru_workspace_host::menubar::MenuBar::command_for"),
            support("koru_workspace_host::menubar::Command"),
        ],
    ),
    // Construction: blocks on a native modal dialog owned by the window that
    // asked
    record(
        "C-FILE-DIALOGS",
        "dialogs::pick_open_file",
        Key::UiLabFileDialogsName,
        Key::UiLabFileDialogsPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_workspace_host::dialogs", DIALOGS),
        [
            (Platform::Windows, Availability::NativeOwned),
            (Platform::MacOs, Availability::NativeOwned),
            (Platform::Linux, Availability::NativeOwned),
            (Platform::Android, Availability::NotAudited),
            (Platform::Ios, Availability::NotAudited),
        ],
        &[
            boundary("koru_workspace_host::dialogs::pick_open_file"),
            boundary("koru_workspace_host::dialogs::pick_open_files"),
            boundary("koru_workspace_host::dialogs::pick_save_file"),
            boundary("koru_workspace_host::dialogs::pick_folder"),
            support("koru_workspace_host::dialogs::FileFilter"),
        ],
    ),
    // Construction: pure values
    record(
        "H-KEYBOARD-ROUTING",
        "routing::Blockers",
        Key::UiLabKeyboardRoutingName,
        Key::UiLabKeyboardRoutingPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_workspace_host::routing", ROUTING),
        DESKTOP_SHARED,
        &[
            support("koru_workspace_host::routing::Blockers"),
            support("koru_workspace_host::routing::Blockers::is_blocking"),
            support("koru_workspace_host::routing::Blockers::editable"),
            support("koru_workspace_host::routing::ModalKind"),
            support("koru_workspace_host::routing::PromptKind"),
            support("koru_workspace_host::routing::FieldId"),
            support("koru_workspace_host::routing::TabId"),
            support("koru_workspace_host::routing::EditableTarget"),
            support("koru_workspace_host::routing::EditableTarget::accepts_text"),
        ],
    ),
];
