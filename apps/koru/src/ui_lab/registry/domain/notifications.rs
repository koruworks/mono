// SPDX-License-Identifier: MPL-2.0

//! **`koru-native-notifications`**: the boundary between Koru's reminder
//! scheduler and an operating system's notification service. Koru supplies
//! the title, body, sound and the words of three actions; the system draws
//! the notification — a Windows toast through the package's notifier, a
//! macOS `UNUserNotificationCenter` request through the notification owner
//! app. Nothing here paints, so the record is support with the delivered
//! notification as its boundary.

use super::*;

const OWNER: &str = "koru-native-notifications";
const LIB: &str = "crates/koru-native-notifications/src/lib.rs";
const SOUNDS: &str = "crates/koru-native-notifications/src/sounds.rs";

/// The crate's record.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: OS-owned: the platform adapter registers with the
    // system's notification service, from the Agent's worker; a Linux build
    // carries an adapter that answers every call Unsupported
    record(
        "C-SYSTEM-NOTIFICATIONS",
        "NativeNotifications",
        Key::UiLabSystemNotificationsName,
        Key::UiLabSystemNotificationsPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_native_notifications", LIB),
        [
            (Platform::Windows, Availability::NativeOwned),
            (Platform::MacOs, Availability::NativeOwned),
            (Platform::Linux, Availability::Unavailable),
            (Platform::Android, Availability::Unavailable),
            (Platform::Ios, Availability::Unavailable),
        ],
        &[
            boundary("koru_native_notifications::NativeNotifications"),
            boundary("koru_native_notifications::platform_notifications"),
            boundary("koru_native_notifications::run_notification_owner_app"),
            support("koru_native_notifications::Content"),
            support("koru_native_notifications::ActionLabels"),
            support("koru_native_notifications::MacActionTitles"),
            support("koru_native_notifications::Schedule"),
            support_with(
                "koru_native_notifications::Action",
                &[values(
                    Key::UiLabControlKind,
                    "koru_native_notifications::Action",
                    LIB,
                    Declared::Enum,
                    &[literal("Open"), literal("Snooze"), literal("Handled")],
                )],
            ),
            support_with(
                "koru_native_notifications::ActionSet",
                &[values(
                    Key::UiLabControlKind,
                    "koru_native_notifications::ActionSet",
                    LIB,
                    Declared::Enum,
                    &[literal("Reminder"), literal("OpenOnly")],
                )],
            ),
            support_with(
                "koru_native_notifications::Sound",
                &[values(
                    Key::UiLabControlKind,
                    "koru_native_notifications::Sound",
                    LIB,
                    Declared::Enum,
                    &[literal("Silent"), literal("System"), literal("Bundled")],
                )],
            ),
            support_with(
                "koru_native_notifications::BundledTone",
                &[values(
                    Key::UiLabControlValues,
                    "koru_native_notifications::BundledTone",
                    SOUNDS,
                    Declared::Enum,
                    &[
                        literal("KoruDefault"),
                        literal("Soft"),
                        literal("Chime"),
                        literal("Pulse"),
                        literal("Bright"),
                        literal("Calm"),
                        literal("Drop"),
                        literal("Knock"),
                        literal("Signal"),
                        literal("Urgent"),
                    ],
                )],
            ),
            support("koru_native_notifications::Authorization"),
        ],
    ),
];
