// SPDX-License-Identifier: MPL-2.0

//! **The Android app** (`apps/android`, Kotlin and Jetpack Compose over
//! Material 3): its one activity and navigation host, every screen, the
//! reusable composables they share, the theme, and the system surfaces it
//! asks Android or Google Play services to draw. Every file under the app's
//! `ui` folder was read in full, with the three activities and the four
//! services that hand a person to a system surface; `registry/MANIFEST.md`
//! lists the files that draw nothing.
//!
//! The app links no Koru drawing crate: it draws Compose controls in Koru's
//! colors and spacing over the embedded store host, so a record here is the
//! Android counterpart of a desktop record, never the same code. Which one
//! each stands beside is in the manifest.

use super::*;

/// The owner every Android symbol is written under: the app's Gradle
/// namespace.
const OWNER: &str = "app.koru.mobile";

const THEME: &str = "apps/android/app/src/main/java/app/koru/mobile/ui/theme/Theme.kt";
const KORU_APP: &str = "apps/android/app/src/main/java/app/koru/mobile/ui/KoruApp.kt";
const DOCUMENT_EDITOR: &str =
    "apps/android/app/src/main/java/app/koru/mobile/ui/editor/DocumentEditor.kt";
const CLOUD_COPY: &str =
    "apps/android/app/src/main/java/app/koru/mobile/ui/editor/CloudCopyScreen.kt";
const RUN_PRESENTATION: &str =
    "apps/android/app/src/main/java/app/koru/mobile/ui/run/RunPresentation.kt";

const fn android(file: &'static str) -> Source {
    source(OWNER, OWNER, file)
}

const THEME_PREFERENCE: OptionSet = values(
    Key::UiLabControlMode,
    "app.koru.mobile::ui.theme.KoruThemePreference",
    THEME,
    Declared::Enum,
    &[literal("System"), literal("Light"), literal("Dark")],
);

const FONT_CHOICE: OptionSet = values(
    Key::UiLabControlValues,
    "app.koru.mobile::ui.theme.KoruFontChoice",
    THEME,
    Declared::Enum,
    &[
        literal("Inter"),
        literal("Manrope"),
        literal("Sans"),
        literal("Serif"),
        literal("Rounded"),
        literal("Mono"),
    ],
);

const WORKSPACE_ACTION: OptionSet = values(
    Key::UiLabControlKind,
    "app.koru.mobile::ui.WorkspaceAction",
    KORU_APP,
    Declared::Enum,
    &[
        literal("Rename"),
        literal("Archive"),
        literal("Unarchive"),
        literal("Delete"),
        literal("Restore"),
        literal("DebugInfo"),
    ],
);

const WORKSPACE_SORT: OptionSet = values(
    Key::UiLabControlMode,
    "app.koru.mobile::ui.WorkspaceSort",
    KORU_APP,
    Declared::Enum,
    &[
        literal("LastModified"),
        literal("TitleAscending"),
        literal("TitleDescending"),
    ],
);

const DOCUMENT_PROJECTION: OptionSet = values(
    Key::UiLabControlMode,
    "app.koru.mobile::ui.DocumentProjection",
    KORU_APP,
    Declared::Enum,
    &[literal("Documents"), literal("Files")],
);

const CLOUD_COPY_PHASE: OptionSet = values(
    Key::UiLabControlState,
    "app.koru.mobile::ui.editor.CloudCopyPhase",
    CLOUD_COPY,
    Declared::Enum,
    &[
        literal("Loading"),
        literal("Verified"),
        literal("Unavailable"),
        literal("SourceChanged"),
        literal("Removing"),
        literal("OutcomeUnknown"),
        literal("Acknowledged"),
    ],
);

const RUN_GLYPH: OptionSet = values(
    Key::UiLabControlState,
    "app.koru.mobile::ui.run.RunGlyph",
    RUN_PRESENTATION,
    Declared::Enum,
    &[
        literal("Waiting"),
        literal("Running"),
        literal("Attention"),
        literal("Interrupted"),
        literal("Completed"),
        literal("Failed"),
        literal("Cancelled"),
        literal("Unknown"),
    ],
);

/// Every Android record, in navigator order.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: a Compose theme over the person's stored appearance
    // (SharedPreferences "appearance"); fonts come from the APK's assets
    record(
        "T-ANDROID-THEME",
        "KoruTheme",
        Key::UiLabAndroidThemeName,
        Key::UiLabAndroidThemePurpose,
        Family::Foundations,
        Kind::NativeAdapter,
        None,
        &[],
        android(THEME),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.theme.KoruTheme"),
            view("app.koru.mobile::ui.theme.KoruSpacing"),
            view_with(
                "app.koru.mobile::ui.theme.KoruThemePreference",
                &[THEME_PREFERENCE],
            ),
            view_with("app.koru.mobile::ui.theme.KoruFontChoice", &[FONT_CHOICE]),
            helper("app.koru.mobile::ui.theme.rememberKoruFontFamily"),
            helper("app.koru.mobile::ui.theme.AppearancePreferences"),
            helper("app.koru.mobile::ui.theme.AppearanceStore"),
            helper("app.koru.mobile::ui.EntityTileColors"),
            helper("app.koru.mobile::ui.workspaceTileColor"),
            helper("app.koru.mobile::ui.profileTileColors"),
            helper("app.koru.mobile::ui.profileChipSlot"),
            helper("app.koru.mobile::ui.calloutTint"),
        ],
    ),
    // Construction: the launcher activity builds the app's service container
    // (the embedded store host) and sets the Compose content
    record(
        "C-ANDROID-APP",
        "KoruApp",
        Key::UiLabAndroidAppName,
        Key::UiLabAndroidAppPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android(KORU_APP),
        ANDROID_APP,
        &[
            view("app.koru.mobile::MainActivity"),
            view("app.koru.mobile::ui.KoruApp"),
            helper("app.koru.mobile::ui.Routes"),
            helper("app.koru.mobile::ui.localizedFailure"),
            helper("app.koru.mobile::ui.localizedError"),
        ],
    ),
    // Construction: pure composables over the rows a screen hands them
    record(
        "C-ANDROID-ENTITY-LIST",
        "EntityListScreen",
        Key::UiLabAndroidEntityListName,
        Key::UiLabAndroidEntityListPurpose,
        Family::Controls,
        Kind::NativeAdapter,
        None,
        &[],
        android(KORU_APP),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.EntityListScreen"),
            view("app.koru.mobile::ui.BoxedEntityIcon"),
            view("app.koru.mobile::ui.WorkspaceActionRow"),
            view("app.koru.mobile::ui.EmptyState"),
            view("app.koru.mobile::ui.CreateDialog"),
        ],
    ),
    // Construction: reads the device-link state the view model polls from
    // the store host; the QR button prepares Google Play's code scanner
    record(
        "C-ANDROID-LINK-DEVICE",
        "LinkDeviceScreen",
        Key::UiLabAndroidLinkDeviceName,
        Key::UiLabAndroidLinkDevicePurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android(KORU_APP),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.LinkDeviceScreen"),
            view("app.koru.mobile::ui.IdentityMismatchBanner"),
            view("app.koru.mobile::ui.LinkedDevicesList"),
            view("app.koru.mobile::ui.RenameDeviceDialog"),
            view("app.koru.mobile::ui.QrScanButton"),
            helper("app.koru.mobile::ui.relayStatusLine"),
            helper("app.koru.mobile::ui.deviceEventLabel"),
            helper("app.koru.mobile::ui.isAnotherInstallation"),
            helper("app.koru.mobile::ui.qrBitmap"),
        ],
    ),
    // Construction: the start route, over the profiles the view model reads
    record(
        "C-ANDROID-PROFILES",
        "ProfilesScreen",
        Key::UiLabAndroidProfilesName,
        Key::UiLabAndroidProfilesPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android(KORU_APP),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.ProfilesScreen"),
            view("app.koru.mobile::ui.CloudSyncActionRow"),
            view("app.koru.mobile::ui.RenameProfileDialog"),
        ],
    ),
    // Construction: the update notice is only ever shown by the nightly
    // build with a feed and a key; a check runs on the IO dispatcher
    record(
        "C-ANDROID-UPDATE-NOTICE",
        "UpdateBanner",
        Key::UiLabAndroidUpdateNoticeName,
        Key::UiLabAndroidUpdateNoticePurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android(KORU_APP),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.UpdateBanner"),
            helper("app.koru.mobile::services.UpdateNotice"),
            helper("app.koru.mobile::services.AppUpdates"),
        ],
    ),
    // Construction: pure over the workspace rows; sorting and filtering are
    // the screen's own state
    record(
        "C-ANDROID-WORKSPACES",
        "WorkspacesScreen",
        Key::UiLabAndroidWorkspacesName,
        Key::UiLabAndroidWorkspacesPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android(KORU_APP),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.WorkspacesScreen"),
            view("app.koru.mobile::ui.WorkspaceDebugDialog"),
            view("app.koru.mobile::ui.DebugRow"),
            view("app.koru.mobile::ui.RenameWorkspaceDialog"),
            view("app.koru.mobile::ui.DeleteWorkspaceDialog"),
            view("app.koru.mobile::ui.WorkspaceFilterRow"),
            view("app.koru.mobile::ui.WorkspaceSortRow"),
            helper_with("app.koru.mobile::ui.WorkspaceAction", &[WORKSPACE_ACTION]),
            helper_with("app.koru.mobile::ui.WorkspaceSort", &[WORKSPACE_SORT]),
            helper("app.koru.mobile::ui.workspaceActions"),
            helper("app.koru.mobile::ui.sortWorkspaces"),
            helper("app.koru.mobile::ui.workspaceVisible"),
        ],
    ),
    // Construction: pure over the document rows; a file's details are read
    // from the store host when its sheet opens
    record(
        "C-ANDROID-DOCUMENTS",
        "DocumentsScreen",
        Key::UiLabAndroidDocumentsName,
        Key::UiLabAndroidDocumentsPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android(KORU_APP),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.DocumentsScreen"),
            view("app.koru.mobile::ui.FileSheetDetails"),
            view("app.koru.mobile::ui.FileWhereabouts"),
            view("app.koru.mobile::ui.RenameDocumentDialog"),
            helper_with(
                "app.koru.mobile::ui.DocumentProjection",
                &[DOCUMENT_PROJECTION],
            ),
            helper("app.koru.mobile::ui.documentVisible"),
        ],
    ),
    // Construction: pure over the blob engine's transfer report
    record(
        "C-ANDROID-FILE-TRANSFERS",
        "BlobTransferPanel",
        Key::UiLabAndroidFileTransfersName,
        Key::UiLabAndroidFileTransfersPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android("apps/android/app/src/main/java/app/koru/mobile/ui/BlobTransferPanel.kt"),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.BlobTransferPanel"),
            view("app.koru.mobile::ui.TransferCount"),
            helper("app.koru.mobile::ui.releaseSentence"),
            helper("app.koru.mobile::ui.transferSentence"),
            helper("app.koru.mobile::ui.transferExplainer"),
            helper("app.koru.mobile::ui.transferIcon"),
            helper("app.koru.mobile::ui.transferTint"),
        ],
    ),
    // Construction: a projection of the application-owned account worker;
    // the screen holds only unsubmitted profile choices
    record(
        "C-ANDROID-ACCOUNT",
        "AccountScreen",
        Key::UiLabAndroidAccountName,
        Key::UiLabAndroidAccountPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android("apps/android/app/src/main/java/app/koru/mobile/ui/AccountScreen.kt"),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.AccountScreen"),
            view("app.koru.mobile::ui.SignInWithAppleButton"),
            view("app.koru.mobile::ui.CloudStorageSection"),
            helper("app.koru.mobile::ui.localProfileAccountLabel"),
        ],
    ),
    // Construction: pages read through the signed-in account's Cloud client
    record(
        "C-ANDROID-CLOUD-INVENTORY",
        "CloudInventoryScreen",
        Key::UiLabAndroidCloudInventoryName,
        Key::UiLabAndroidCloudInventoryPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android("apps/android/app/src/main/java/app/koru/mobile/ui/CloudInventoryScreen.kt"),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.CloudInventoryScreen"),
            view("app.koru.mobile::ui.InventoryAmount"),
            view("app.koru.mobile::ui.IdentifierDetails"),
        ],
    ),
    // Construction: a detached snapshot the authenticated inspection adapter
    // supplies; the body loads no blob and edits nothing
    record(
        "C-ANDROID-CLOUD-COPY",
        "CloudCopyScreen",
        Key::UiLabAndroidCloudCopyName,
        Key::UiLabAndroidCloudCopyPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android(CLOUD_COPY),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.editor.CloudCopyScreen"),
            view("app.koru.mobile::ui.editor.CloudCopyBody"),
            view("app.koru.mobile::ui.editor.PassiveCloudFile"),
            helper_with(
                "app.koru.mobile::ui.editor.CloudCopyPhase",
                &[CLOUD_COPY_PHASE],
            ),
            helper("app.koru.mobile::ui.editor.CloudCopyViewKey"),
            helper("app.koru.mobile::ui.editor.CloudCopyConfirmation"),
            helper("app.koru.mobile::ui.editor.cloudCopyRemovableNodes"),
            helper("app.koru.mobile::ui.editor.cloudCopyRemovalEnabled"),
            helper("app.koru.mobile::ui.editor.PassiveBlockLayout"),
            helper("app.koru.mobile::ui.editor.passiveBlockLayouts"),
        ],
    ),
    // Construction: draws a presenter's state; the presenter polls the store
    // host only while the screen is started
    record(
        "C-ANDROID-CLOUD-PROGRESS",
        "CloudProgressScreen",
        Key::UiLabAndroidCloudProgressName,
        Key::UiLabAndroidCloudProgressPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android("apps/android/app/src/main/java/app/koru/mobile/ui/CloudProgressScreen.kt"),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.CloudProgressScreen"),
            view("app.koru.mobile::ui.Message"),
            view("app.koru.mobile::ui.WorkspaceRow"),
            helper("app.koru.mobile::ui.count"),
            helper("app.koru.mobile::ui.summaryText"),
            helper("app.koru.mobile::ui.detailText"),
            helper("app.koru.mobile::ui.holdSentence"),
            helper("app.koru.mobile::ui.holdText"),
            helper("app.koru.mobile::ui.waitingText"),
        ],
    ),
    // Construction: opens a document session on the store host; pictures
    // are read through the blob service as they scroll in
    record(
        "C-ANDROID-DOCUMENT-VIEWER",
        "DocumentViewer",
        Key::UiLabAndroidDocumentViewerName,
        Key::UiLabAndroidDocumentViewerPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android(DOCUMENT_EDITOR),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.editor.DocumentViewer"),
            view("app.koru.mobile::ui.editor.DocumentBlockRow"),
            view("app.koru.mobile::ui.editor.Marker"),
            view("app.koru.mobile::ui.editor.RichTextBlock"),
            view("app.koru.mobile::ui.editor.CodeBlock"),
            view("app.koru.mobile::ui.editor.TableBlock"),
            view("app.koru.mobile::ui.editor.BlobImage"),
            view("app.koru.mobile::ui.editor.ReferenceCard"),
            view("app.koru.mobile::ui.editor.ContactCard"),
            helper("app.koru.mobile::ui.editor.runPillLabels"),
            helper("app.koru.mobile::ui.editor.defaultPillLabel"),
            helper("app.koru.mobile::ui.editor.rememberReferenceFallback"),
            helper("app.koru.mobile::ui.editor.referenceFallback"),
            helper("app.koru.mobile::ui.editor.annotatedRichText"),
            helper("app.koru.mobile::ui.editor.parseMarkdownTable"),
        ],
    ),
    // Construction: wraps a platform EditText; every keystroke is handed up
    // as the text before and after
    record(
        "C-ANDROID-RICH-EDITOR",
        "NativeRichEditor",
        Key::UiLabAndroidRichEditorName,
        Key::UiLabAndroidRichEditorPurpose,
        Family::Controls,
        Kind::NativeAdapter,
        None,
        &[],
        android("apps/android/app/src/main/java/app/koru/mobile/ui/editor/NativeRichEditor.kt"),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.editor.NativeRichEditor"),
            helper("app.koru.mobile::ui.editor.RichCoordinator"),
            helper("app.koru.mobile::ui.editor.ReferenceSpan"),
        ],
    ),
    // Construction: reads the run observer's snapshot; activity pages are
    // fetched a window at a time, never on a timer
    record(
        "C-ANDROID-RUN",
        "RunScreen",
        Key::UiLabAndroidRunName,
        Key::UiLabAndroidRunPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android("apps/android/app/src/main/java/app/koru/mobile/ui/run/RunScreen.kt"),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.run.RunScreen"),
            view("app.koru.mobile::ui.run.RunOverviewTab"),
            view("app.koru.mobile::ui.run.RunActivityTab"),
            view("app.koru.mobile::ui.run.RunActivityRow"),
            view("app.koru.mobile::ui.run.SectionTitle"),
            view("app.koru.mobile::ui.run.Row"),
            helper("app.koru.mobile::ui.run.deviceName"),
            helper("app.koru.mobile::ui.run.statusLabel"),
            helper("app.koru.mobile::ui.run.stageLabel"),
            helper("app.koru.mobile::ui.run.attentionLabel"),
            helper("app.koru.mobile::ui.run.ratingLabel"),
            helper("app.koru.mobile::ui.run.scopeLabel"),
            helper("app.koru.mobile::ui.run.tokens"),
            helper("app.koru.mobile::ui.run.stepLabel"),
            helper_with("app.koru.mobile::ui.run.RunGlyph", &[RUN_GLYPH]),
            helper("app.koru.mobile::ui.run.RunStepKind"),
            helper("app.koru.mobile::ui.run.runPillLabel"),
            helper("app.koru.mobile::ui.run.firstLine"),
            helper("app.koru.mobile::ui.run.runOverview"),
        ],
    ),
    // Construction: pure; the licenses screen reads the APK's own assets off
    // the main thread
    record(
        "C-ANDROID-SETTINGS",
        "SettingsScreen",
        Key::UiLabAndroidSettingsName,
        Key::UiLabAndroidSettingsPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android("apps/android/app/src/main/java/app/koru/mobile/ui/SettingsScreen.kt"),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ui.SettingsScreen"),
            view("app.koru.mobile::ui.DiagnosticsScreen"),
            view("app.koru.mobile::ui.OpenSourceLicensesScreen"),
            view("app.koru.mobile::ui.OpenSourceLicenseDocumentScreen"),
            helper("app.koru.mobile::ui.OpenSourceLicenses"),
        ],
    ),
    // Construction: the share activity reads the intent's URI grants and
    // stages each item before the store host imports it
    record(
        "C-ANDROID-SHARE",
        "ShareActivity",
        Key::UiLabAndroidShareName,
        Key::UiLabAndroidSharePurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android("apps/android/app/src/main/java/app/koru/mobile/ShareActivity.kt"),
        ANDROID_APP,
        &[
            view("app.koru.mobile::ShareActivity"),
            view("app.koru.mobile::ui.ShareDestinationScreen"),
        ],
    ),
    // Construction: OS- and provider-owned: Credential Manager's sheet,
    // Google Play services' code scanner, the browser and the system chooser
    record(
        "C-ANDROID-SYSTEM-SURFACES",
        "AndroidQrScanner",
        Key::UiLabAndroidSystemSurfacesName,
        Key::UiLabAndroidSystemSurfacesPurpose,
        Family::Compositions,
        Kind::NativeAdapter,
        None,
        &[],
        android("apps/android/app/src/main/java/app/koru/mobile/services/AndroidQrScanner.kt"),
        ANDROID_SYSTEM,
        &[
            system("app.koru.mobile::services.AndroidGoogleAccountDialog"),
            system("app.koru.mobile::services.AndroidQrScanner"),
            system("app.koru.mobile::services.AndroidBrowserSignInLauncher"),
            system("app.koru.mobile::BrowserSignInReturnActivity"),
            system("app.koru.mobile::ui.openExportedFile"),
        ],
    ),
];
