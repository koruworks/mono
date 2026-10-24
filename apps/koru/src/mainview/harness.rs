// SPDX-License-Identifier: MPL-2.0

//! The fixtures every container test builds on.
//!
//! Shared rather than repeated because the tests now sit in four files — beside
//! the module each covers — and a container is four lines to stand up.
//!
//! **Nothing here touches disk.** `Workspace::new` is in-memory; only
//! `Session::open` reaches a directory, and only `App::new` calls it.

use koru_core::node::Node;
use koru_core::schema::WorkspaceDoc;
use koru_core::{NodeId, WorkspaceId};
use koru_workspace::Workspace;
use winit::keyboard::{Key, ModifiersState};

use super::MainView;
use koru_workspace_host::View;

/// The explore pane's toggle wears Shift, because the plain primary chord is
/// the editor's align-centre — see `mainview::keys::resolve`.
pub(super) use koru_keys::{NONE, PRIMARY, PRIMARY_ALT, PRIMARY_SHIFT};

pub(super) fn ch(c: &str) -> Key {
    Key::Character(c.into())
}

/// **A dictation controller somebody has agreed with.**
///
/// A controller with no consent refuses before it looks at anything else — the
/// microphone half of Plan 2.5's gate — so a test about routing, anchoring or
/// discarding has to start from one that has been told. `Granted::recorded` is
/// the in-memory grant the consent step itself holds between the button and
/// the write, so this is the product's own path rather than a test seam, and
/// nothing here touches the machine's real consent record.
pub(super) fn consenting_dictation() -> koru_dictation::Dictation {
    let mut controller = koru_dictation::Dictation::new();
    controller.set_consent(Some(koru_voice_consent::Granted::recorded(
        koru_voice_consent::Terms {
            model: "parakeet-tdt-v3".into(),
            version: "1.2.0".into(),
            licence: "Apache-2.0".into(),
            attribution: "k2-fsa, Apache-2.0".into(),
            download_bytes: 648_291_840,
        },
        1,
    )));
    controller
}

/// A container over an in-memory workspace, sized to a comfortable window.
///
/// Opens as the app does: one welcome tab, and no permanent tab behind it.
pub(super) fn container() -> (MainView, Workspace) {
    let ws = WorkspaceDoc::create(WorkspaceId::new(), "t").expect("create");
    let workspace_id = ws.id().expect("workspace id");
    let workspace = Workspace::new(ws);
    let mut v = MainView::new();
    // **Not the machine's setting.** `MainView::new` reads the OS's
    // animation preference, and a suite that followed it would pass or fail
    // with Windows' *Animation effects* switch. The one test about reduced
    // motion sets the flag itself.
    v.reduced_motion = false;
    v.set_viewport(1200.0, 700.0);
    v.set_pane_context(
        Vec::new(),
        koru_core::ProfileId::new(),
        Vec::new(),
        Some(workspace_id),
    );
    (v, workspace)
}

/// …and one showing a document, which is what most tests want: a leaf they can
/// type into and click rows in.
///
/// This used to open the permanent Workspace tab. There is no such tab now, and
/// a document is what a container holds — so the tests that ask for "a leaf"
/// get the kind of leaf the app actually opens. Tabs are `[Welcome, Notes]`
/// with `Notes` showing at index 1, which is the same index the outline tab had.
pub(super) fn on_document() -> (MainView, Workspace) {
    let (mut v, mut w) = container();
    let doc = a_document(&mut w.ws, "Notes");
    v.open_node(&w.ws, doc);
    // Opening one is how the app gets a leaf, but the fixture should be as
    // inert as the old `on_workspace` was — that one *selected* a tab which
    // already existed and recorded nothing. A test that asserts on recents
    // should start from an empty list.
    v.recents.clear();
    (v, w)
}

pub(super) fn a_document(ws: &mut WorkspaceDoc, title: &str) -> NodeId {
    koru_workspace::document(ws, title).expect("document")
}

/// Pin the active tab. In-memory tabs have no `DocumentId`, and `TabStrip::pin`
/// refuses those, so stamp one first.
pub(super) fn pin_active_document(v: &mut MainView) -> bool {
    let i = v.tabs().active();
    if let Some(tab) = v.tabs_mut().get_mut(i)
        && tab.document.is_none()
    {
        tab.document = Some(koru_core::DocumentId::new());
    }
    v.pin_tab(i)
}

/// The `y`, in the content rect's own space, of **body** row `n`.
///
/// A document tab draws no chrome above its rows any more, so the offset is
/// the top margin plus the *title* — which is row 0, and set on the heading
/// ramp rather than at body size. Derived rather than spelled out at each call
/// site, so the next person changing either does not have to find eight magic
/// numbers; and derived from the ramp rather than from a number that happens to
/// match it today.
pub(super) fn row_y(n: usize) -> f32 {
    use koru_draw::theme;
    let title = theme::type_step(Some(theme::TITLE_LEVEL));
    theme::MARGIN_TOP
        + title.size * title.line_height
        + theme::TITLE_GAP_BELOW
        + n as f32 * theme::ROW_H
        + 6.0
}

/// Put `n` paragraphs into the document `v` is showing, so there is something
/// to click, scroll and hover over.
///
/// Rows used to go into `Parent::Root` — the outline — because the container's
/// permanent tab showed that realm. A container shows documents now, so a row
/// belongs to the document being shown, and a test that put one in the outline
/// would be filling a tree nothing is looking at.
pub(super) fn fill(v: &MainView, ws: &mut WorkspaceDoc, n: usize) -> NodeId {
    use koru_core::node::Node;
    use koru_core::schema::Parent;
    let doc = v.document_root().expect("a document tab is showing");
    for i in 0..n {
        ws.insert(
            Parent::Node(doc),
            Node::paragraph(format!("row {i}")).unwrap(),
        )
        .expect("insert");
    }
    doc
}

/// Stand in for the shell answering a draft's request for a real document.
///
/// **Promotion is a round trip now.** A draft asks the shell
/// (`PaneRequest::CreateDocument`) and keeps taking keystrokes until the answer
/// arrives; its text is carried across at the swap. There is no shell in these
/// tests, so this plays its part.
///
/// **It replaces the document rather than adding to it**, because that is what
/// `Workspace::open_document` does: a store-backed `ws` holds exactly one
/// document, whose root is the one `show_active_document` finds. An earlier
/// version of this helper inserted a second root into the existing `ws` and the
/// swap picked the wrong one — the helper was wrong, not the code it stands in
/// for, which is worth recording because it cost a revert.
pub(super) fn shell_answers_the_draft(v: &mut MainView, ws: &mut WorkspaceDoc) -> NodeId {
    assert!(
        matches!(
            View::take_pane_request(v),
            Some(koru_workspace_host::PaneRequest::CreateDocument(_))
        ),
        "the draft did not ask the shell for a document"
    );
    // What `Workspace::create_document` leaves behind: a fresh document holding
    // a titled root and one empty line.
    *ws = WorkspaceDoc::create(WorkspaceId::new(), "t").expect("create");
    let root = ws
        .insert(koru_core::schema::Parent::Root, Node::document("").unwrap())
        .unwrap();
    ws.insert(
        koru_core::schema::Parent::Node(root),
        Node::paragraph("").unwrap(),
    )
    .unwrap();
    // `None`: this harness stands in for a workspace with no manifest, where a
    // tab carries no `DocumentId` either.
    v.show_active_document(ws, None);
    // The swap lands on the next key, where the document is available mutably.
    v.key(
        ws,
        &Key::Named(winit::keyboard::NamedKey::Shift),
        None,
        ModifiersState::empty(),
    );
    root
}
