// SPDX-License-Identifier: MPL-2.0

//! **The workspace app's host, and the other app crates that draw**: the
//! `Host` the workspace app's `main` hands to `koru_workspace_host::run`;
//! the standalone document app (`apps/koru-document`), whose
//! `BuiltInDocument` is also what the workspace app's `Leaf` wraps; and the
//! Agent (`apps/koru-agent`), whose search window, voice windows and tray
//! are its only UI. The other crates under `apps/` draw nothing, or own
//! only an operating-system surface; `registry/MANIFEST.md` says which.

use super::*;

const DOCUMENT: &str = "koru-document";
const AGENT: &str = "koru-agent";

const AGENT_APP: &str = "apps/koru-agent/src/app.rs";
const AGENT_TRAY: &str = "apps/koru-agent/src/tray.rs";
const AGENT_VOICE: &str = "apps/koru-agent/src/voice_window.rs";
const DOC_BUBBLE: &str = "apps/koru-document/src/bubble.rs";
const DOC_LIB: &str = "apps/koru-document/src/lib.rs";
const DOC_MENU: &str = "apps/koru-document/src/menu.rs";
const DOC_SEARCH: &str = "apps/koru-document/src/search.rs";
const MAIN: &str = "apps/koru/src/main.rs";

/// Every record of the host and the companion apps, in navigator order.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: starts the whole app: the Host reads settings, finds a
    // Profile, connects to the Agent and the Store and opens windows; the
    // Lab is dispatched before it and never builds one
    record(
        "H-APP-HOST",
        "Manager",
        Key::UiLabAppHostName,
        Key::UiLabAppHostPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(KORU, "koru", MAIN),
        DESKTOP_SHARED,
        &[support("koru::Manager")],
    ),
    // Construction: open(ws, root) is in-memory over the host's resident
    // document; the window app itself is koru_workspace_host::run
    record(
        "C-DOCUMENT-WINDOW",
        "BuiltInDocument",
        Key::UiLabDocumentWindowName,
        Key::UiLabDocumentWindowPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(DOCUMENT, "koru_document", DOC_LIB),
        DESKTOP_SHARED,
        &[
            painter("koru_document::BuiltInDocument"),
            painter("koru_document::BuiltInDocument::draw_confirm"),
            painter("koru_document::BuiltInDocument::draw_lifted_viewer"),
            painter("koru_document::draw::draw_chrome"),
            painter("koru_document::caption::draw"),
            support("koru_document::caption::Hit"),
            support("koru_document::draw::WindowChrome"),
            support("koru_document::ViewerLift"),
        ],
    ),
    // Construction: pure geometry over the selection's state
    record(
        "C-FORMAT-BUBBLE",
        "bubble::draw",
        Key::UiLabFormatBubbleName,
        Key::UiLabFormatBubblePurpose,
        Family::Controls,
        Kind::AppLocal,
        None,
        &[],
        source(DOCUMENT, "koru_document", DOC_BUBBLE),
        DESKTOP_SHARED,
        &[
            painter("koru_document::bubble::draw"),
            painter("koru_document::bubble::draw_tooltip"),
            support_with(
                "koru_document::bubble::Button",
                &[values(
                    Key::UiLabControlKind,
                    "koru_document::bubble::Button",
                    DOC_BUBBLE,
                    Declared::Enum,
                    &[
                        literal("BlockType"),
                        literal("List"),
                        literal("Cmd"),
                        literal("More"),
                    ],
                )],
            ),
            support("koru_document::bubble::hit"),
        ],
    ),
    // Construction: pure: rows built from the document's state
    record(
        "C-DOCUMENT-MENUS",
        "menu::Menu",
        Key::UiLabDocumentMenusName,
        Key::UiLabDocumentMenusPurpose,
        Family::Controls,
        Kind::AppLocal,
        None,
        &[],
        source(DOCUMENT, "koru_document", DOC_MENU),
        DESKTOP_SHARED,
        &[
            painter("koru_document::menu::Menu"),
            support_with(
                "koru_document::menu::Trailing",
                &[values(
                    Key::UiLabControlKind,
                    "koru_document::menu::Trailing",
                    DOC_MENU,
                    Declared::Enum,
                    &[
                        literal("None"),
                        literal("Check"),
                        literal("Toggle"),
                        literal("Note"),
                        literal("Swatches"),
                    ],
                )],
            ),
            support("koru_document::menu::Row"),
            painter("koru_document::page_menu::draw_width_icon"),
            painter("koru_document::page_menu::draw_button"),
            support("koru_document::page_menu::Choice"),
            support("koru_document::blockmenu::BlockChoice"),
            support("koru_document::window_menu::Choice"),
            support("koru_document::context_menu::Choice"),
        ],
    ),
    // Construction: pure: two TextInputs over a TextEngine; matching runs
    // over the open document in memory
    record(
        "C-FIND-REPLACE",
        "LocalSearch",
        Key::UiLabFindReplaceName,
        Key::UiLabFindReplacePurpose,
        Family::Controls,
        Kind::AppLocal,
        None,
        &[],
        source(DOCUMENT, "koru_document", DOC_SEARCH),
        DESKTOP_SHARED,
        &[painter("koru_document::search::LocalSearch")],
    ),
    // Construction: the Agent's own window loop (koru_window::run), and a
    // SearchSurface that starts two store reader threads
    record(
        "C-AGENT-SEARCH-WINDOW",
        "app::Agent",
        Key::UiLabAgentSearchWindowName,
        Key::UiLabAgentSearchWindowPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(AGENT, "koru_agent", AGENT_APP),
        DESKTOP_SHARED,
        &[painter("koru_agent::app::Agent")],
    ),
    // Construction: pure views over the dictation coordinator's phase; the
    // windows are the Agent's
    record(
        "C-AGENT-VOICE-WINDOWS",
        "VoiceWindowHost",
        Key::UiLabAgentVoiceWindowsName,
        Key::UiLabAgentVoiceWindowsPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(AGENT, "koru_agent", AGENT_VOICE),
        DESKTOP_SHARED,
        &[
            painter("koru_agent::voice_window::VoiceWindowHost"),
            painter("koru_agent::voice_window::VoiceWindowHost::draw"),
        ],
    ),
    // Construction: OS-owned: tray-icon on Windows and macOS, ksni on
    // Linux, each on the platform's own thread; a stand-in elsewhere
    record(
        "C-AGENT-TRAY",
        "tray::Tray",
        Key::UiLabAgentTrayName,
        Key::UiLabAgentTrayPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(AGENT, "koru_agent", AGENT_TRAY),
        [
            (Platform::Windows, Availability::NativeOwned),
            (Platform::MacOs, Availability::NativeOwned),
            (Platform::Linux, Availability::NativeOwned),
            (Platform::Android, Availability::NotAudited),
            (Platform::Ios, Availability::NotAudited),
        ],
        &[
            boundary("koru_agent::tray::Tray"),
            support("koru_agent::tray::VoiceMenu"),
            support_with(
                "koru_agent::tray::Shortcut",
                &[values(
                    Key::UiLabControlState,
                    "koru_agent::tray::Shortcut",
                    AGENT_TRAY,
                    Declared::Enum,
                    &[literal("Unknown"), literal("Absent"), literal("Chord")],
                )],
            ),
        ],
    ),
];
