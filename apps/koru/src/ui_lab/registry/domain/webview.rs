// SPDX-License-Identifier: MPL-2.0

//! **`koru-webview`**: the native web surface, one record written from the
//! source it names. The page inside it is the system's web engine's, so the
//! record is a boundary. `registry/MANIFEST.md` lists the files read and what
//! opening one costs.

use super::*;

const OWNER: &str = "koru-webview";

const LIB: &str = "crates/koru-webview/src/lib.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: an engine process: open(parent, bounds, config, wake)
    // needs a live parent window handle, blocks the UI thread while the
    // engine builds its environment (hundreds of milliseconds on Windows),
    // keeps cookies and storage in the caller's data directory, and under
    // no-webview or on Linux answers RuntimeUnavailable
    record(
        "C-WEB-SURFACE",
        "WebSurface",
        Key::UiLabWebSurfaceName,
        Key::UiLabWebSurfacePurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_webview", LIB),
        [
            (Platform::Windows, Availability::NativeOwned),
            (Platform::MacOs, Availability::NativeOwned),
            (Platform::Linux, Availability::Unavailable),
            (Platform::Android, Availability::Unavailable),
            (Platform::Ios, Availability::Unavailable),
        ],
        &[
            boundary("koru_webview::WebSurface"),
            boundary("koru_webview::WebSurface::open"),
            support("koru_webview::WebSurface::set_bounds"),
            support("koru_webview::WebSurface::set_visible"),
            support("koru_webview::WebSurface::focus"),
            support("koru_webview::WebSurface::focus_parent"),
            support("koru_webview::WebSurface::has_focus"),
            support("koru_webview::WebSurface::eval"),
            support("koru_webview::WebSurface::navigate"),
            support("koru_webview::WebSurface::clear_all_browsing_data"),
            support("koru_webview::WebConfig"),
            support_with(
                "koru_webview::Navigation",
                &[values(
                    Key::UiLabControlValues,
                    "koru_webview::Navigation",
                    LIB,
                    Declared::Enum,
                    &[literal("Allow"), literal("External"), literal("Refuse")],
                )],
            ),
            support("koru_webview::WebEvent"),
            support("koru_webview::WebError"),
            support("koru_webview::PhysicalRect"),
            support("koru_webview::Fault"),
            support("koru_webview::Fault::parse"),
            support("koru_webview::host_of"),
            support("koru_webview::js_string_literal"),
            support("koru_webview::debug_enabled"),
        ],
    ),
];
