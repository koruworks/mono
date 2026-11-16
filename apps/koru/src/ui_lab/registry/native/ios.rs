// SPDX-License-Identifier: MPL-2.0

//! **The iOS app and its share extension** (`apps/ios`, Swift): the
//! `KoruMobile` target's SwiftUI scene, screens, the three native wrappers
//! COMPONENTS.md names (`KoruPrimaryButton`, `KoruEmptyState`, `KoruRow`),
//! the TextKit block view and the camera scanner it asks VisionKit for; and
//! the `KoruShare` target's UIKit share sheet. Every file that imports
//! SwiftUI or UIKit was read in full; `registry/MANIFEST.md` lists the files
//! that draw nothing.
//!
//! Like Android, the app links no Koru drawing crate: these are the iOS
//! counterparts of desktop records, never the same code.

use super::*;

/// The app target's owner, qualified by its folder.
const APP: &str = "ios/KoruMobile";

const THEME: &str = "apps/ios/KoruMobile/Theme.swift";
const VIEWS: &str = "apps/ios/KoruMobile/Views.swift";
const DEVICES: &str = "apps/ios/KoruMobile/DevicesScreen.swift";
const SETTINGS: &str = "apps/ios/KoruMobile/SettingsScreen.swift";
const LICENSES: &str = "apps/ios/KoruMobile/OpenSourceLicensesScreen.swift";

const fn ios(file: &'static str) -> Source {
    source(APP, APP, file)
}

const CREATOR: OptionSet = values(
    Key::UiLabControlKind,
    "ios/KoruMobile::RootView.Creator",
    VIEWS,
    Declared::Enum,
    &[
        literal("profile"),
        literal("workspace"),
        literal("document"),
    ],
);

const APPEARANCE: OptionSet = values(
    Key::UiLabControlMode,
    "ios/KoruMobile::SettingsScreen.Appearance",
    SETTINGS,
    Declared::Enum,
    &[literal("system"), literal("light"), literal("dark")],
);

const LICENSE_DOCUMENT: OptionSet = values(
    Key::UiLabControlKind,
    "ios/KoruMobile::OpenSourceLicenses.Document",
    LICENSES,
    Declared::Enum,
    &[literal("license"), literal("notice"), literal("thirdParty")],
);

/// Every iOS record, in navigator order.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: static values over the system's own colors and fonts
    record(
        "T-IOS-THEME",
        "KoruSpace",
        Key::UiLabIosThemeName,
        Key::UiLabIosThemePurpose,
        Family::Foundations,
        Kind::NativeAdapter,
        None,
        &[],
        ios(THEME),
        IOS_APP,
        &[
            view("ios/KoruMobile::KoruSpace"),
            view("ios/KoruMobile::KoruColor"),
            view("ios/KoruMobile::KoruType"),
            helper("ios/KoruMobile::KoruEntityColor"),
        ],
    ),
    // Construction: pure: a title key and an action
    record(
        "C-IOS-PRIMARY-BUTTON",
        "KoruPrimaryButton",
        Key::UiLabIosPrimaryButtonName,
        Key::UiLabIosPrimaryButtonPurpose,
        Family::Controls,
        Kind::NativeAdapter,
        Some("C-BUTTON"),
        &[],
        ios(THEME),
        IOS_APP,
        &[view("ios/KoruMobile::KoruPrimaryButton")],
    ),
    // Construction: pure: a title key
    record(
        "C-IOS-EMPTY-STATE",
        "KoruEmptyState",
        Key::UiLabIosEmptyStateName,
        Key::UiLabIosEmptyStatePurpose,
        Family::Controls,
        Kind::NativeAdapter,
        None,
        &[],
        ios(THEME),
        IOS_APP,
        &[view("ios/KoruMobile::KoruEmptyState")],
    ),
    // Construction: pure: wraps the content it is given
    record(
        "C-IOS-ROW",
        "KoruRow",
        Key::UiLabIosRowName,
        Key::UiLabIosRowPurpose,
        Family::Controls,
        Kind::NativeAdapter,
        None,
        &[],
        ios(THEME),
        IOS_APP,
        &[view("ios/KoruMobile::KoruRow")],
    ),
    // Construction: the scene builds the app model over the native store
    // services and drains the share inbox when it becomes active
    record(
        "C-IOS-APP",
        "RootView",
        Key::UiLabIosAppName,
        Key::UiLabIosAppPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        ios(VIEWS),
        IOS_APP,
        &[
            view("ios/KoruMobile::KoruMobileApp"),
            view_with("ios/KoruMobile::RootView", &[CREATOR]),
            view("ios/KoruMobile::ProfileBadge"),
            view("ios/KoruMobile::CreationSheet"),
            helper("ios/KoruMobile::Route"),
            helper("ios/KoruMobile::localizedError"),
        ],
    ),
    // Construction: pure over the rows a screen hands it
    record(
        "C-IOS-ENTITY-LIST",
        "EntityList",
        Key::UiLabIosEntityListName,
        Key::UiLabIosEntityListPurpose,
        Family::Controls,
        Kind::NativeAdapter,
        None,
        &[],
        ios(VIEWS),
        IOS_APP,
        &[view("ios/KoruMobile::EntityList")],
    ),
    // Construction: reads the app model's visible workspaces
    record(
        "C-IOS-WORKSPACES",
        "WorkspaceList",
        Key::UiLabIosWorkspacesName,
        Key::UiLabIosWorkspacesPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        ios(VIEWS),
        IOS_APP,
        &[
            view("ios/KoruMobile::WorkspaceList"),
            view("ios/KoruMobile::WorkspaceDebugSheet"),
        ],
    ),
    // Construction: reads the app model's visible documents
    record(
        "C-IOS-DOCUMENTS",
        "DocumentList",
        Key::UiLabIosDocumentsName,
        Key::UiLabIosDocumentsPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        ios(VIEWS),
        IOS_APP,
        &[view("ios/KoruMobile::DocumentList")],
    ),
    // Construction: opens a document session through the app model;
    // pictures are observed through the blob service
    record(
        "C-IOS-DOCUMENT-VIEW",
        "DocumentView",
        Key::UiLabIosDocumentViewName,
        Key::UiLabIosDocumentViewPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        ios(VIEWS),
        IOS_APP,
        &[
            view("ios/KoruMobile::DocumentView"),
            view("ios/KoruMobile::BlockView"),
            view("ios/KoruMobile::TableBlock"),
            view("ios/KoruMobile::BlobImage"),
            helper("ios/KoruMobile::listMarkerLabel"),
            helper("ios/KoruMobile::romanNumeral"),
            helper("ios/KoruMobile::documentMarkerLane"),
            helper("ios/KoruMobile::documentBlockFont"),
            helper("ios/KoruMobile::italicized"),
            helper("ios/KoruMobile::documentHeadingSpaceAbove"),
            helper("ios/KoruMobile::documentTextAlignment"),
            helper("ios/KoruMobile::parseMarkdownTable"),
            helper("ios/KoruMobile::isSeparatorCell"),
        ],
    ),
    // Construction: wraps a TextKit 2 UITextView; edits leave as commands
    record(
        "C-IOS-RICH-TEXT",
        "NativeRichTextEditor",
        Key::UiLabIosRichTextName,
        Key::UiLabIosRichTextPurpose,
        Family::Controls,
        Kind::NativeAdapter,
        None,
        &[],
        ios("apps/ios/KoruMobile/NativeRichTextEditor.swift"),
        IOS_APP,
        &[view("ios/KoruMobile::NativeRichTextEditor")],
    ),
    // Construction: reads the link state and devices the app model polls;
    // a device's history is fetched when its sheet opens
    record(
        "C-IOS-DEVICES",
        "DevicesScreen",
        Key::UiLabIosDevicesName,
        Key::UiLabIosDevicesPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        ios(DEVICES),
        IOS_APP,
        &[
            view("ios/KoruMobile::DevicesScreen"),
            view("ios/KoruMobile::LinkedDeviceRow"),
            view("ios/KoruMobile::PairingOfferView"),
            view("ios/KoruMobile::DeviceHistoryScreen"),
            view("ios/KoruMobile::DeviceEventRow"),
            helper("ios/KoruMobile::relativeTime"),
        ],
    ),
    // Construction: OS-owned: VisionKit's live camera scanner
    record(
        "C-IOS-QR-SCANNER",
        "QRScanSheet",
        Key::UiLabIosQrScannerName,
        Key::UiLabIosQrScannerPurpose,
        Family::Controls,
        Kind::NativeAdapter,
        None,
        &[],
        ios(DEVICES),
        IOS_SYSTEM,
        &[system("ios/KoruMobile::QRScanSheet")],
    ),
    // Construction: pure; the appearance is an @AppStorage value and the
    // licenses screen reads the bundle off the main thread
    record(
        "C-IOS-SETTINGS",
        "SettingsScreen",
        Key::UiLabIosSettingsName,
        Key::UiLabIosSettingsPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        ios(SETTINGS),
        IOS_APP,
        &[
            view_with("ios/KoruMobile::SettingsScreen", &[APPEARANCE]),
            view("ios/KoruMobile::OpenSourceLicensesScreen"),
            view("ios/KoruMobile::OpenSourceLicenseDocumentScreen"),
            view("ios/KoruMobile::DiagnosticsView"),
            helper("ios/KoruMobile::KoruAppearanceModifier"),
            helper("ios/KoruMobile::koruAppearance"),
            helper_with("ios/KoruMobile::OpenSourceLicenses", &[LICENSE_DOCUMENT]),
        ],
    ),
    // Construction: one staged inbox entry; importing goes through the app
    // model's file service
    record(
        "C-IOS-SHARE-DESTINATION",
        "ShareDestinationSheet",
        Key::UiLabIosShareDestinationName,
        Key::UiLabIosShareDestinationPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        ios("apps/ios/KoruMobile/ShareDestinationScreen.swift"),
        IOS_APP,
        &[view("ios/KoruMobile::ShareDestinationSheet")],
    ),
    // Construction: a separate process that never opens the Store: it reads
    // the destinations snapshot and stages into the App Group inbox
    record(
        "C-IOS-SHARE-EXTENSION",
        "ShareViewController",
        Key::UiLabIosShareExtensionName,
        Key::UiLabIosShareExtensionPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        source(
            "ios/KoruShare",
            "ios/KoruShare",
            "apps/ios/KoruShare/ShareViewController.swift",
        ),
        IOS_APP,
        &[view("ios/KoruShare::ShareViewController")],
    ),
];
