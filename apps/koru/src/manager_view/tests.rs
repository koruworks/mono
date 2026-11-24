// SPDX-License-Identifier: MPL-2.0

//! Geometry tests for the close-choice dialog.
//!
//! Pure arithmetic, like the tab menu's: the dialog is laid out without a
//! viewport, so the hit test and the drawing cannot disagree about where a
//! button is.

use super::*;
use koru_core::WorkspaceId;
use koru_core::node::{Kind, Node};
use koru_core::schema::Parent;
use koru_workspace_host::{ImportPreview, PasteTarget, PastedImage};

/// A viewport big enough that the dialog never clamps, so the arithmetic is
/// about the dialog rather than about the window.
const VIEWPORT: (f32, f32) = (800.0, 600.0);

#[test]
fn the_panel_is_centred_in_the_viewport() {
    let d = CloseDialog::new();
    let p = d.panel(VIEWPORT);
    assert!((p.x - (VIEWPORT.0 - p.w) * 0.5).abs() < 0.5);
    assert!((p.y - (VIEWPORT.1 - p.h) * 0.5).abs() < 0.5);
}

#[test]
fn the_two_buttons_sit_side_by_side_without_overlapping() {
    let d = CloseDialog::new();
    let (restore, close) = d.buttons(VIEWPORT);
    assert_eq!(restore.y, close.y);
    assert_eq!(restore.h, close.h);
    assert_eq!(restore.w, close.w);
    assert!(close.x >= restore.x + restore.w, "buttons overlap");
    // A hair of gap between them, not flush — there is a `DLG_BTN_GAP`.
    assert!(close.x - (restore.x + restore.w) >= DLG_BTN_GAP - 0.5);
}

#[test]
fn each_button_hit_tests_back_to_itself() {
    let d = CloseDialog::new();
    let (restore, close) = d.buttons(VIEWPORT);
    let rc = |r: Rect| (r.x + r.w * 0.5, r.y + r.h * 0.5);
    assert_eq!(
        d.hit_at(VIEWPORT, rc(restore).0, rc(restore).1),
        Some(CloseHit::Restore)
    );
    assert_eq!(
        d.hit_at(VIEWPORT, rc(close).0, rc(close).1),
        Some(CloseHit::Close)
    );
}

#[test]
fn the_remember_checkbox_hits_across_its_whole_row() {
    let d = CloseDialog::new();
    let r = d.remember_rect(VIEWPORT);
    // The box itself.
    assert_eq!(
        d.hit_at(VIEWPORT, r.x + r.w * 0.5, r.y + r.h * 0.5),
        Some(CloseHit::Remember)
    );
    // The label beside it, almost at the panel's right edge — the whole row is
    // the target, the way a native checkbox's label is.
    let p = d.panel(VIEWPORT);
    assert_eq!(
        d.hit_at(VIEWPORT, p.x + p.w - DLG_PAD, r.y + r.h * 0.5),
        Some(CloseHit::Remember)
    );
}

#[test]
fn a_point_outside_the_panel_hits_nothing() {
    let d = CloseDialog::new();
    let p = d.panel(VIEWPORT);
    // Above the panel.
    assert_eq!(d.hit_at(VIEWPORT, p.x + p.w * 0.5, p.y - 1.0), None);
    // In the panel's padding band, between the title and the buttons.
    let (restore, _) = d.buttons(VIEWPORT);
    assert_eq!(d.hit_at(VIEWPORT, p.x + DLG_PAD, restore.y - 2.0), None);
}

/// Regression: the shell drains `pane.view`, which is a `ManagerView` (and, for
/// a popped-out document, the `DocumentPane` it wraps). These wrappers once
/// forwarded the `/image` and copy hooks but **not** the paste hooks
/// (`take_wants_paste_image`, `take_wants_paste_rich`, `paste_images`), so those
/// fell back to the trait defaults (`None` / no-op): a paste the editor had armed
/// never reached the document, and image paste was silently dropped. Arming the
/// request on the inner document and asserting the wrapper returns it guards
/// against that omission coming back.
#[test]
fn document_pane_and_manager_view_forward_the_paste_hooks() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = koru_workspace::document(&mut ws, "spec.md").unwrap();
    let anchor = ws
        .insert(Parent::Node(doc), Node::paragraph("anchor").unwrap())
        .unwrap();
    let mut pane = DocumentPane::open(&ws, doc, PopoutClose::default());

    // `DocumentPane` forwards the drain rather than swallowing it (the bug).
    pane.doc.editor.view.wants_paste_image = Some((anchor, Vec::new()));
    assert_eq!(
        View::take_wants_paste_image(&mut pane),
        Some(PasteTarget {
            anchor,
            replacing: Vec::new(),
        }),
        "DocumentPane must forward take_wants_paste_image"
    );
    pane.doc.editor.view.wants_paste_rich = Some((anchor, Vec::new(), "hello".into()));
    assert_eq!(
        View::take_wants_paste_rich(&mut pane),
        Some((
            PasteTarget {
                anchor,
                replacing: Vec::new(),
            },
            "hello".into(),
        )),
        "DocumentPane must forward take_wants_paste_rich"
    );

    // The `ManagerView::Document` arm forwards too.
    pane.doc.editor.view.wants_paste_image = Some((anchor, Vec::new()));
    pane.doc.editor.view.wants_paste_rich = Some((anchor, Vec::new(), "hello".into()));
    let mut mv = ManagerView::Document(pane);
    assert_eq!(
        View::take_wants_paste_image(&mut mv),
        Some(PasteTarget {
            anchor,
            replacing: Vec::new(),
        }),
        "ManagerView::Document must forward take_wants_paste_image"
    );
    assert_eq!(
        View::take_wants_paste_rich(&mut mv),
        Some((
            PasteTarget {
                anchor,
                replacing: Vec::new(),
            },
            "hello".into(),
        )),
        "ManagerView::Document must forward take_wants_paste_rich"
    );

    // And `paste_images` actually inserts through the wrapper.
    let img = PastedImage {
        hash: "deadbeef".into(),
        mime: "image/png".into(),
        width: Some(1),
        height: Some(1),
        name: None,
    };
    View::paste_images(
        &mut mv,
        &mut ws,
        Some(PasteTarget {
            anchor,
            replacing: Vec::new(),
        }),
        &[img],
    );
    let ManagerView::Document(pane) = &mv else {
        unreachable!();
    };
    assert!(
        pane.doc
            .editor
            .body
            .rows
            .iter()
            .any(|r| r.kind == Kind::Image),
        "paste_images through the wrapper must insert an image node"
    );
}

/// **Every `View` method the shell calls has to be forwarded by this enum**, or
/// it silently falls back to the trait's default and the feature behind it is
/// dead with nothing to show for it.
///
/// That is exactly how Alt+Arrow and the mouse's thumb buttons did nothing: the
/// container implemented `navigate_history`, and `ManagerView` never passed the
/// call on. The default returns `false`, so there was no error to notice.
#[test]
fn manager_view_forwards_navigate_history_to_the_container() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let a = koru_workspace::document(&mut ws, "A").unwrap();
    let b = koru_workspace::document(&mut ws, "B").unwrap();

    let mut container = crate::mainview::MainView::new();
    View::set_viewport(&mut container, 1200.0, 700.0);
    container.open_node(&ws, a);
    container.open_node(&ws, b);

    let mut mv = ManagerView::Container(container);
    assert_eq!(View::document_root(&mv), Some(b));
    assert!(
        View::navigate_history(&mut mv, &ws, -1),
        "the call has to reach the container"
    );
    assert_eq!(View::document_root(&mv), Some(a));
}

/// Import inspection finishes on a worker and hands its modal back through the
/// shell's `ManagerView`. Falling through to `View`'s default `false` makes the
/// shell report that the preview could not be shown even though `MainView`
/// implements it. Notices use the same shell-to-container boundary.
#[test]
fn manager_view_forwards_import_previews_and_notices_to_the_container() {
    let mut mv = ManagerView::Container(crate::mainview::MainView::new());
    let preview = ImportPreview {
        workspace_name: "Older workspace".into(),
        document_count: 1,
        node_count: 2,
        image_count: 0,
        warnings: Vec::new(),
        rows: Vec::new(),
        identity_conflict: false,
    };

    assert!(View::show_import_preview(&mut mv, preview));
    assert!(View::show_notice(&mut mv, "Export complete".into()));
}

#[test]
fn manager_view_forwards_native_reminder_surfaces_to_the_container() {
    let profile = koru_store_core::ProfileId::new();
    let reminder = koru_store_core::ReminderId::new();
    let intent = koru_workspace_host::ReminderSnoozeIntent {
        profile,
        reminder,
        occurrence: koru_store_core::OccurrenceKey::parse(&format!("occ1-{}", "01".repeat(32)))
            .unwrap(),
        entity: koru_store_core::EntityRef::new(
            "task",
            koru_store_core::WorkspaceId::new(),
            koru_store_core::DocumentId::new(),
            uuid::Uuid::new_v4(),
        )
        .unwrap(),
    };
    let mut view = ManagerView::Container(crate::mainview::MainView::new());

    assert!(View::reminder_snooze(&mut view, intent));
    assert!(View::open_all_reminders(&mut view));
}

/// A popped-out document is one document in one window, so it has nowhere it
/// has been — and must say so rather than panicking or pretending.
#[test]
fn a_popped_out_document_has_no_history_to_walk() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = koru_workspace::document(&mut ws, "A").unwrap();
    let mut mv = ManagerView::Document(DocumentPane::open(&ws, doc, PopoutClose::default()));
    assert!(!View::navigate_history(&mut mv, &ws, -1));
    assert!(!View::navigate_history(&mut mv, &ws, 1));
}

/// **Ctrl+W closes a standalone document window.** There are no tabs here for
/// it to close one of, and Alt+F4 already did this — Ctrl+W not doing it was
/// the odd one out. In the container the chord never arrives: `mainview::keys`
/// claims `w` and closes a tab.
#[test]
fn ctrl_w_closes_a_standalone_document_window() {
    use winit::keyboard::Key;

    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = koru_workspace::document(&mut ws, "A").unwrap();
    let mut mv = ManagerView::Document(DocumentPane::open(&ws, doc, PopoutClose::default()));

    // Ctrl+W arrives as the ASCII control code on Windows; `ctrl_letter`
    // normalises both forms, so the test drives the awkward one.
    assert!(View::key(
        &mut mv,
        &mut ws,
        &Key::Character("\u{17}".into()),
        Some("\u{17}"),
        koru_keys::PRIMARY,
    ));
    assert_eq!(
        View::take_window_request(&mut mv),
        Some(koru_window::WindowRequest::Close)
    );
}

/// Ctrl+Shift+W is deliberately *not* taken: it means "close every tab", and a
/// window with none of them has no answer for it.
#[test]
fn ctrl_shift_w_does_not_close_a_standalone_document_window() {
    use winit::keyboard::{Key, ModifiersState};

    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = koru_workspace::document(&mut ws, "A").unwrap();
    let mut mv = ManagerView::Document(DocumentPane::open(&ws, doc, PopoutClose::default()));

    View::key(
        &mut mv,
        &mut ws,
        &Key::Character("\u{17}".into()),
        Some("\u{17}"),
        koru_keys::PRIMARY.union(ModifiersState::SHIFT),
    );
    assert_eq!(View::take_window_request(&mut mv), None);
}

/// **Every `View` method `MainView` implements is forwarded by
/// `ManagerView`.**
///
/// `ManagerView` forwards the `View` trait by hand, one `match` per method, so
/// a method implemented on `MainView` and not listed again there falls through
/// to the trait's default and is never called. Nothing fails to compile: the
/// default is a real implementation, and `false` is a plausible answer.
///
/// It has bitten six times. The fifth was voice, and it cost an evening: every
/// one of six methods defaulted, so no dictation reached a surface and
/// `show_voice_consent` answered `false` for a view that has a consent step —
/// which the shell then reported as *another question is already on screen*
/// while the screen held no question at all. The sixth was the delegate
/// dialog's *Change…*: the picker opened, a folder was chosen, and
/// `delegate_folder_chosen` defaulted to `false` — so the row kept the folder
/// it had, nothing was stored, and a refused folder said nothing either.
/// Twenty methods were in that state at once (ADR-367).
///
/// **The guard is the whole trait now**, not one keyword at a time. Two
/// keyword-scoped versions of this test stood here — one for voice, one for
/// reminders — and neither could have caught the sixth, because "delegate" was
/// not a word either of them knew. A guard that has to be widened by hand for
/// each new capability is the same list this test exists to stop trusting.
///
/// This reads both files rather than exercising behaviour, deliberately: what
/// goes wrong is a name missing from a list, and that is a fact about the text.
#[test]
fn every_view_method_the_container_implements_is_forwarded() {
    let container = include_str!("../mainview/mod.rs");
    let manager = include_str!("../manager_view.rs");

    // Only what the *trait* declares: `MainView` also has private helpers whose
    // names look the same, and forwarding one of those would not compile.
    let trait_src = include_str!("../../../../crates/koru-workspace-host/src/lib.rs");
    let implemented: Vec<&str> = container
        .lines()
        .filter_map(|line| line.trim().strip_prefix("fn "))
        .filter_map(|rest| rest.split(['(', '<']).next())
        .filter(|name| trait_src.contains(&format!("    fn {name}")))
        .collect();

    assert!(
        implemented.len() > 50,
        "the scan found only {} View methods on MainView, so it is testing almost nothing",
        implemented.len()
    );

    let missing: Vec<&&str> = implemented
        .iter()
        .filter(|name| !manager.contains(&format!("fn {name}")))
        .collect();

    assert!(
        missing.is_empty(),
        "MainView implements {missing:?}, and ManagerView does not forward them — so the trait \
         default answers instead and the method is never called. Add each to \
         `forward_to_the_window!` in manager_view.rs, or a `match` arm of its own where the two \
         windows answer differently."
    );
}

mod accessibility;

#[test]
fn purged_popout_closes_by_document_identity_after_source_is_forgotten() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let root = koru_workspace::document(&mut ws, "Notes").unwrap();
    let document = ws.own_document_id().unwrap_or_else(|| ws.document_id(root));
    let mut pane = DocumentPane::open(&ws, root, PopoutClose::default());
    pane.close = Some(CloseDialog::new());
    pane.wants_restore_tab = true;
    let mut view = ManagerView::Document(pane);
    assert!(!view.quiesce_purged_document(koru_core::DocumentId::new(), None));
    assert!(view.quiesce_purged_document(document, None));
    let ManagerView::Document(pane) = &mut view else {
        unreachable!()
    };
    assert!(pane.closing);
    assert!(pane.close.is_none());
    assert!(
        !pane.wants_restore_tab,
        "retirement must not restore the old tab"
    );
    assert!(matches!(
        view.take_window_request(),
        Some(WindowRequest::Close)
    ));
}
