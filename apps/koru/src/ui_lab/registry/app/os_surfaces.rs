// SPDX-License-Identifier: MPL-2.0

//! **The app crates that own only an operating-system surface**: the
//! standalone Store (`apps/koru-store-server`), whose notification-area icon
//! and menu, backup dialogs and start-up failure box are the system's own
//! controls, and the macOS notification owner
//! (`apps/koru-notification-owner`), the helper app that owns Koru's
//! notifications on a Mac and titles their actions. Koru draws none of it;
//! each record is the boundary where Koru hands the system its words.
//!
//! The phones embed the Store in the app and run no Store process, and a
//! phone has no notification area (mobile CATCHUP, *Deliberately desktop*),
//! so these are not applicable there.

use super::*;

const STORE: &str = "koru-store-server";
const OWNER: &str = "koru-notification-owner";

const STORE_TRAY: &str = "apps/koru-store-server/src/tray.rs";
const STORE_LOG: &str = "apps/koru-store-server/src/log.rs";
const OWNER_MAIN: &str = "apps/koru-notification-owner/src/main.rs";

/// A Windows-only system surface: `tray-icon` and `MessageBoxW` are linked
/// only there, and every other desktop gets a stand-in that shows nothing.
const WINDOWS_ONLY: [(Platform, Availability); 5] = [
    (Platform::Windows, Availability::NativeOwned),
    (Platform::MacOs, Availability::Unavailable),
    (Platform::Linux, Availability::Unavailable),
    (Platform::Android, Availability::NotApplicable),
    (Platform::Ios, Availability::NotApplicable),
];

/// The OS-owned surfaces, in navigator order.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: OS-owned: a dedicated thread creates the icon and pumps
    // Win32 messages for it; off Windows, `start` answers None
    record(
        "C-STORE-TRAY",
        "tray::Tray",
        Key::UiLabStoreTrayName,
        Key::UiLabStoreTrayPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(STORE, "koru_store_server", STORE_TRAY),
        WINDOWS_ONLY,
        &[
            boundary("koru_store_server::tray::Tray"),
            boundary("koru_store_server::tray::backup_store"),
            support("koru_store_server::tray::attach"),
            support_with(
                "koru_store_server::tray::TrayEvent",
                &[values(
                    Key::UiLabControlKind,
                    "koru_store_server::tray::TrayEvent",
                    STORE_TRAY,
                    Declared::Enum,
                    &[
                        literal("OpenStoreLocation"),
                        literal("BackupStore"),
                        literal("StartOnLogin"),
                        literal("Quit"),
                    ],
                )],
            ),
            support("koru_store_server::tray::run_entry"),
            support("koru_store_server::tray::RUN_ENTRY"),
        ],
    ),
    // Construction: OS-owned: a modal MessageBoxW, only before the Store is
    // serving; KORU_STORE_NO_DIALOG silences it for test harnesses
    record(
        "C-STORE-FAILURE-DIALOG",
        "log::report_fatal",
        Key::UiLabStoreFailureDialogName,
        Key::UiLabStoreFailureDialogPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(STORE, "koru_store_server", STORE_LOG),
        WINDOWS_ONLY,
        &[
            boundary("koru_store_server::log::report_fatal"),
            support("koru_store_server::log::NO_DIALOG"),
        ],
    ),
    // Construction: OS-owned: the signed helper app the Agent asks to
    // register, withdraw and answer for macOS notifications
    record(
        "C-MAC-NOTIFICATION-OWNER",
        "action_titles",
        Key::UiLabMacNotificationOwnerName,
        Key::UiLabMacNotificationOwnerPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(OWNER, "koru_notification_owner", OWNER_MAIN),
        [
            (Platform::Windows, Availability::NotApplicable),
            (Platform::MacOs, Availability::NativeOwned),
            (Platform::Linux, Availability::NotApplicable),
            (Platform::Android, Availability::NotApplicable),
            (Platform::Ios, Availability::NotApplicable),
        ],
        &[
            boundary("koru_notification_owner::main"),
            support("koru_notification_owner::action_titles"),
            support("koru_notification_owner::system_lang"),
        ],
    ),
];
