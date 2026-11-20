// SPDX-License-Identifier: MPL-2.0

//! **The workspace app's mini app** (`apps/koru/src/mini_app`): the native
//! host both A2UI catalogs are drawn by. The fixed task/document catalog's
//! root, list selector and document search are this module's own; its task
//! collection is the Tasks view (`tasks_view.rs`); its document host, which
//! the composition catalog shares, is the retained document a landed
//! reference becomes. The composition catalog's other node kinds are drawn
//! through the element layer and the shared button, recorded at their
//! owners. `registry/MANIFEST.md` lists what was read.

use super::*;

const CHROME: &str = "apps/koru/src/mini_app/chrome.rs";
const COMPOSITION: &str = "apps/koru/src/mini_app/composition.rs";
const MINI_APP: &str = "apps/koru/src/mini_app.rs";

/// Every record of the mini app, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: new and fixture are pure (state, a TextEngine and an
    // empty element frame); from_declaration asks for the referenced
    // document, which the container's child loader serves; Save and Pin
    // start a gateway worker lazily on the first send
    record(
        "C-MINI-APP",
        "MiniApp",
        Key::UiLabMiniAppName,
        Key::UiLabMiniAppPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mini_app", MINI_APP),
        A2UI_DESKTOP,
        &[
            component("koru::mini_app::MiniApp", "TaskDocument"),
            host_only("koru::mini_app::MiniApp::from_declaration"),
            host_only("koru::mini_app::MiniApp::fixture"),
            host_only("koru::mini_app::MiniApp::refine"),
            host_painter("koru::mini_app::MiniApp::draw_surface"),
            support("koru::mini_app::MiniApp::new"),
            support("koru::mini_app::chrome::Chrome"),
            support_with(
                "koru::mini_app::Region",
                &[values(
                    Key::UiLabControlMode,
                    "koru::mini_app::Region",
                    MINI_APP,
                    Declared::Enum,
                    &[
                        labelled("Tasks", Key::A11yTasksHeading),
                        labelled("Document", Key::CloudTabLabelDocument),
                    ],
                )],
            ),
            support("koru::mini_app::Regions"),
            support("koru::mini_app::MiniApp::focus_region"),
            support("koru::mini_app::MiniApp::catch_up"),
            support("koru::mini_app::MiniApp::default_title"),
            support("koru::mini_app::MiniApp::effective_definition"),
            support("koru::mini_app::MiniApp::refinement_target"),
            support("koru::mini_app::MiniApp::chrome_semantics"),
            support("koru::mini_app::MiniApp::chrome_accessibility_action"),
            support("koru::mini_app::MiniApp::save_label"),
            support("koru::mini_app::MiniApp::request_artifact_save"),
            support("koru::mini_app::MiniApp::pin_label"),
            support("koru::mini_app::MiniApp::request_pin"),
        ],
    ),
    // Construction: part of the mini app's chrome; the lists it offers are
    // the container's list table, pushed in with set_lists
    record(
        "C-TASK-LIST-SELECTOR",
        "TaskListSelector",
        Key::UiLabTaskListSelectorName,
        Key::UiLabTaskListSelectorPurpose,
        Family::Controls,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mini_app", CHROME),
        A2UI_DESKTOP,
        &[
            component(
                "koru::mini_app::chrome::Chrome::open_selector",
                "TaskListSelector",
            ),
            host_only("koru::mini_app::MiniApp::set_lists"),
            support("koru::mini_app::MiniApp::select_tasks"),
            support_with(
                "koru::mini_app::chrome::Choice",
                &[values(
                    Key::UiLabControlKind,
                    "koru::mini_app::chrome::Choice",
                    CHROME,
                    Declared::Enum,
                    &[
                        literal("Query"),
                        literal("Document"),
                        literal("Page"),
                        literal("Status"),
                    ],
                )],
            ),
        ],
    ),
    // Construction: typing starts the search: changed_query builds a
    // koru-search-ui EntitySearch on the gateway root (two store reader
    // threads) and polls it every 40 ms while results are pending
    record(
        "C-DOCUMENT-SEARCH",
        "DocumentSearch",
        Key::UiLabDocumentSearchFieldName,
        Key::UiLabDocumentSearchFieldPurpose,
        Family::Controls,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mini_app", CHROME),
        A2UI_DESKTOP,
        &[
            component(
                "koru::mini_app::chrome::Chrome::changed_query",
                "DocumentSearch",
            ),
            host_only("koru::mini_app::chrome::Chrome::poll"),
            support("koru::mini_app::chrome::Chrome::deadline"),
            support("koru::mini_app::chrome::Chrome::accept_result"),
        ],
    ),
    // Construction: a landed document becomes a Leaf over the container's
    // resident document; earlier ones stay cached, drawing nothing
    record(
        "C-DOCUMENT-HOST",
        "DocumentHost",
        Key::UiLabDocumentHostName,
        Key::UiLabDocumentHostPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mini_app", MINI_APP),
        A2UI_DESKTOP,
        &[
            component("koru::mini_app::MiniApp::land", "DocumentHost"),
            support("koru::mini_app::MiniApp::request_document"),
            support("koru::mini_app::MiniApp::reactivate"),
            support("koru::mini_app::MiniApp::retained_documents"),
            support("koru::mini_app::MiniApp::leaf"),
            support("koru::mini_app::MiniApp::focused_leaf"),
            support("koru::mini_app::MiniApp::owns"),
        ],
    ),
    // Construction: pure: the validated declaration, its own TextEngine and
    // element frame; it never names a document's contents
    record(
        "C-COMPOSITION-HOST",
        "composition::Surface",
        Key::UiLabCompositionHostName,
        Key::UiLabCompositionHostPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::mini_app", COMPOSITION),
        A2UI_DESKTOP,
        &[
            host_only("koru::mini_app::composition::Surface"),
            host_only("koru::mini_app::composition::Surface::layout"),
            host_painter("koru::mini_app::MiniApp::draw_composition"),
            host_only("koru::mini_app::composition::spacing"),
            host_only("koru::mini_app::composition::text_role"),
            host_only("koru::mini_app::composition::ink_role"),
            host_only("koru::mini_app::composition::radius_role"),
            support("koru::mini_app::composition::Surface::references"),
            support("koru::mini_app::composition::Surface::scroll"),
            support("koru::mini_app::MiniApp::composition_semantics"),
            support("koru::mini_app::MiniApp::composition_accessibility_action"),
        ],
    ),
];
