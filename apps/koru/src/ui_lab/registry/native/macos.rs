// SPDX-License-Identifier: MPL-2.0

//! **The macOS share extension** (`apps/macos/KoruShare`, Swift and AppKit):
//! Koru's row in the Mac's share sheet. `koru-release` compiles it with
//! `swiftc` beside the iOS inbox format (`apps/ios/KoruShared/ShareInbox.swift`)
//! into `KoruShare.appex` inside the desktop package; the desktop app itself
//! draws nothing in AppKit, so this is the only macOS-native view.

use super::*;

/// The extension's module, qualified by its app folder: iOS's share
/// extension is the module `KoruShare` too.
const SHARE: &str = "macos/KoruShare";

/// The macOS record.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: a separate process that never opens the Store: it stages
    // the shared items into the channel's inbox and brings Koru forward
    record(
        "C-MACOS-SHARE-EXTENSION",
        "ShareViewController",
        Key::UiLabMacosShareExtensionName,
        Key::UiLabMacosShareExtensionPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        source(
            SHARE,
            SHARE,
            "apps/macos/KoruShare/ShareViewController.swift",
        ),
        MACOS_APP,
        &[view("macos/KoruShare::ShareViewController")],
    ),
];
