// SPDX-License-Identifier: MPL-2.0

//! **Application compositions**: whole surfaces assembled from controls —
//! the shared ones in `koru-ui` here, the domain crates' and the apps' own as
//! their batches register them. Each record was written from the module it
//! names; `registry/MANIFEST.md` lists what was read.

use super::*;
use koru_i18n::Key;

const fn ui(module: &'static str, file: &'static str) -> Source {
    Source {
        owner: "koru-ui",
        module,
        file,
    }
}

#[allow(clippy::too_many_arguments)]
const fn record(
    catalog_id: &'static str,
    public_name: &'static str,
    name: Key,
    purpose: Key,
    kind: Kind,
    catalog_family: Option<&'static str>,
    identities: &'static [&'static str],
    source: Source,
    platforms: [(Platform, Availability); 5],
    apis: &'static [Api],
) -> Entry {
    Entry {
        catalog_id,
        public_name,
        name,
        purpose,
        family: Family::Compositions,
        kind,
        catalog_family,
        identities,
        source,
        platforms,
        recipes: &[],
        apis,
    }
}

const fn composed(symbol: &'static str) -> Api {
    plain(symbol, Story::NoStoryYet)
}

const fn support(symbol: &'static str) -> Api {
    plain(symbol, Story::Nonvisual)
}

const CAPTION: &str = "crates/koru-ui/src/caption.rs";
const WELCOME: &str = "crates/koru-ui/src/welcome.rs";
const MEDIA_VIEWER: &str = "crates/koru-ui/src/media_viewer.rs";

/// The viewer's own labels for its stages and toolbar actions, and its
/// wheel behaviours: the host picks which actions the toolbar offers.
const MEDIA_VIEWER_OPTIONS: [OptionSet; 3] = [
    values(
        Key::UiLabControlKind,
        "koru_ui::media_viewer::Stage",
        MEDIA_VIEWER,
        Declared::Enum,
        &[
            labelled("Diagram", Key::ViewerDiagram),
            labelled("Bitmap", Key::ViewerPicture),
        ],
    ),
    values(
        Key::UiLabControlValues,
        "koru_ui::media_viewer::Action",
        MEDIA_VIEWER,
        Declared::Enum,
        &[
            labelled("ZoomOut", Key::ViewerZoomOut),
            labelled("ZoomIn", Key::ViewerZoomIn),
            labelled("Fit", Key::ViewerFit),
            labelled("ActualSize", Key::ViewerActualSize),
            labelled("Copy", Key::ViewerCopy),
            labelled("SaveAs", Key::ViewerSaveAs),
            labelled("OpenExternally", Key::ViewerOpenExternally),
            labelled("Close", Key::ViewerClose),
        ],
    ),
    values(
        Key::UiLabControlValues,
        "koru_ui::media_viewer::WheelAxis",
        MEDIA_VIEWER,
        Declared::Enum,
        &[
            literal("AsTurned"),
            literal("Sideways"),
            literal("UpAndDown"),
        ],
    ),
];

/// Every composition record, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    record(
        "C-CAPTION",
        "KoruCaption",
        Key::UiLabCaptionName,
        Key::UiLabCaptionPurpose,
        Kind::Widget,
        Some("C-CHROME"),
        &[],
        ui("koru_ui::caption", CAPTION),
        DESKTOP_NOT_ON_PHONES,
        &[
            plain_with(
                "koru_ui::caption::draw",
                &[values(
                    Key::UiLabControlStyle,
                    "koru_ui::caption::MenuVisual",
                    CAPTION,
                    Declared::Enum,
                    &[
                        labelled("Static", Key::UiLabOptionStatic),
                        labelled("Morph", Key::UiLabOptionMorph),
                        labelled("Pin", Key::UiLabOptionPin),
                    ],
                )],
                Story::NoStoryYet,
            ),
            composed("koru_ui::caption::draw_direct_controls"),
            support("koru_ui::caption::State"),
            support("koru_ui::caption::Action"),
            support("koru_ui::caption::controls"),
            support("koru_ui::caption::direct_hit"),
            support("koru_ui::caption::reserved_width"),
        ],
    ),
    record(
        "C-WELCOME",
        "KoruWelcome",
        Key::UiLabWelcomeName,
        Key::UiLabWelcomePurpose,
        Kind::Widget,
        Some("C-CHROME"),
        &["WelcomeFailure", "WelcomeCloudNote"],
        ui("koru_ui::welcome", WELCOME),
        DESKTOP_ANDROID_PARTIAL,
        &[
            plain_with(
                "koru_ui::welcome::Layout",
                &[values(
                    Key::UiLabControlMode,
                    "koru_ui::welcome::Layout",
                    WELCOME,
                    Declared::Functions,
                    &[
                        labelled("new", Key::UiLabOptionWithAccounts),
                        labelled("without_accounts", Key::UiLabOptionWithoutAccounts),
                    ],
                )],
                Story::Nonvisual,
            ),
            composed("koru_ui::welcome::draw"),
            composed("koru_ui::welcome::draw_without_accounts"),
            composed("koru_ui::welcome::draw_backdrop"),
            support("koru_ui::welcome::CloudNote"),
            support("koru_ui::welcome::Failure"),
            support("koru_ui::welcome::Action"),
        ],
    ),
    record(
        "C-LAUNCHER",
        "KoruLauncher",
        Key::UiLabLauncherName,
        Key::UiLabLauncherPurpose,
        Kind::Widget,
        None,
        &["KoruLauncher"],
        ui("koru_ui::launcher", "crates/koru-ui/src/launcher.rs"),
        DESKTOP_NOT_ON_PHONES,
        &[
            composed("koru_ui::launcher::Launcher"),
            support("koru_ui::launcher::Tile"),
            support("koru_ui::launcher::Outcome"),
            support("koru_ui::launcher::Semantics"),
        ],
    ),
    record(
        "C-MEDIA-VIEWER",
        "KoruMediaViewer",
        Key::UiLabMediaViewerName,
        Key::UiLabMediaViewerPurpose,
        Kind::Widget,
        Some("C-OVERLAY"),
        &["KoruMediaViewer"],
        ui(
            "koru_ui::media_viewer",
            "crates/koru-ui/src/media_viewer.rs",
        ),
        DESKTOP_NOT_ON_PHONES,
        &[
            plain_with(
                "koru_ui::media_viewer::MediaViewer",
                &MEDIA_VIEWER_OPTIONS,
                Story::NoStoryYet,
            ),
            support("koru_ui::media_viewer::ViewerImage"),
            support("koru_ui::media_viewer::ActionSet"),
            support("koru_ui::media_viewer::ViewerOptions"),
            support("koru_ui::media_viewer::ViewerSemantics"),
            support("koru_ui::media_viewer::Outcome"),
        ],
    ),
    record(
        "C-PAGE-SHELL",
        "PageShell",
        Key::UiLabPageShellName,
        Key::UiLabPageShellPurpose,
        Kind::Support,
        Some("C-LAYOUT"),
        &[],
        ui("koru_ui::page_shell", "crates/koru-ui/src/page_shell.rs"),
        DESKTOP_NOT_ON_PHONES,
        &[support("koru_ui::page_shell::PageShell")],
    ),
];
