// SPDX-License-Identifier: MPL-2.0

//! **The native platforms' records**: the screens, reusable views and
//! platform adapters the Android app (Kotlin, Compose), the iOS app and its
//! share extension (Swift, SwiftUI and UIKit) and the macOS share extension
//! (Swift, AppKit) draw for themselves. A file per platform, written from
//! the source each record names.
//!
//! **None of this is Rust, and none of it runs in the Lab.** The Lab is a
//! Windows window; a Compose screen or a SwiftUI view cannot be drawn in it,
//! so every visible symbol here is [`Story::PlatformOnly`], what the system
//! itself draws for Koru (a credential sheet, a camera scanner) is a
//! [`Story::Boundary`], and the code beside a view is nonvisual. No native
//! symbol is in an A2UI catalog.
//!
//! **How a native symbol is written.** The record's module is the platform's
//! own name for the code it was read from, and `::` separates it from the
//! declaration, the same way the Rust records separate a crate from an item:
//!
//! - Android: the Gradle namespace `app.koru.mobile`, then the Kotlin package
//!   below it and the declaration (`app.koru.mobile::ui.editor.DocumentViewer`,
//!   `app.koru.mobile::MainActivity` for the package root).
//! - Apple: the Xcode target or `swiftc` module, qualified by its app folder
//!   because the iOS and macOS share extensions are both the module
//!   `KoruShare` (`ios/KoruMobile::DevicesScreen`,
//!   `macos/KoruShare::ShareViewController`).
//!
//! `tests/drift/native.rs` reads every file these records name as text and
//! holds each symbol to a Kotlin or Swift declaration of that name, each
//! option set to the enum's members, and every composable and view those
//! files declare to a record.
//!
//! **Every path is a string literal**, as in the app records: nothing here
//! links or calls native code, and each file is on `ui_lab/tests.rs`'s
//! `RECORD_FILES` list.

use super::domain::{record, source};
use super::*;
use koru_i18n::Key;

mod android;

pub(super) const ANDROID: &[Entry] = android::RECORDS;

/// A Compose screen or a SwiftUI view: drawn only on its own platform.
const fn view(symbol: &'static str) -> Api {
    plain(symbol, Story::PlatformOnly)
}

/// A native view whose production values are listed.
const fn view_with(symbol: &'static str, options: &'static [OptionSet]) -> Api {
    plain_with(symbol, options, Story::PlatformOnly)
}

/// Code beside a native view that draws nothing: state, words, geometry.
const fn helper(symbol: &'static str) -> Api {
    plain(symbol, Story::Nonvisual)
}

/// Nonvisual, with the production values it carries.
const fn helper_with(symbol: &'static str, options: &'static [OptionSet]) -> Api {
    plain_with(symbol, options, Story::Nonvisual)
}

/// What the operating system or a provider draws when Koru asks for it.
const fn system(symbol: &'static str) -> Api {
    plain(symbol, Story::Boundary)
}

/// A native adapter that ships in the Android app. Every other platform has
/// its own record of the same role, or none; this one has no role there.
const ANDROID_APP: [(Platform, Availability); 5] = [
    (Platform::Windows, Availability::NotApplicable),
    (Platform::MacOs, Availability::NotApplicable),
    (Platform::Linux, Availability::NotApplicable),
    (Platform::Android, Availability::Existing),
    (Platform::Ios, Availability::NotApplicable),
];

/// A surface Android or Google Play services draws for the Android app.
const ANDROID_SYSTEM: [(Platform, Availability); 5] = [
    (Platform::Windows, Availability::NotApplicable),
    (Platform::MacOs, Availability::NotApplicable),
    (Platform::Linux, Availability::NotApplicable),
    (Platform::Android, Availability::NativeOwned),
    (Platform::Ios, Availability::NotApplicable),
];
