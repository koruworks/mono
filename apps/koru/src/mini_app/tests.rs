// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_workspace_host::accessibility::{Scope, ViewSlot, WindowSlot};
use winit::event::Ime;

pub(super) fn app() -> (MiniApp, WorkspaceDoc) {
    let workspace = WorkspaceId::new();
    let mut doc = WorkspaceDoc::create(workspace, "fixture").unwrap();
    koru_workspace::document(&mut doc, "Document").unwrap();
    let document = DocumentId::new();
    doc.stamp_document_id(document).unwrap();
    let mut app = MiniApp::fixture(workspace, 7).unwrap();
    View::set_viewport(&mut app, 1000.0, 600.0);
    assert!(app.land(&doc, document));
    (app, doc)
}

#[test]
fn retained_children_use_one_transform_across_narrow_and_swapped_layouts() {
    let (mut app, _) = app();
    let owner = app.leaf().unwrap().native_ime_owner();
    for (width, swapped, focus) in [
        (1000.0, false, Region::Tasks),
        (300.0, false, Region::Tasks),
        (300.0, false, Region::Document),
        (1000.0, true, Region::Document),
    ] {
        app.swapped = swapped;
        app.focus = focus;
        View::set_viewport(&mut app, width, 600.0);
        assert_eq!(app.leaf().unwrap().native_ime_owner(), owner);
        for region in [Region::Tasks, Region::Document] {
            let rect = app.regions.rect(region);
            if rect.w == 0.0 {
                continue;
            }
            let point = (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
            assert_eq!(app.regions.hit(point.0, point.1), Some(region));
            assert_eq!(
                app.regions.local(region, point.0, point.1),
                (rect.w * 0.5, rect.h * 0.5)
            );
            let transformed = app.regions.transform(region, Affine::IDENTITY)
                * vello::kurbo::Point::new(0.0, 0.0);
            assert_eq!(
                (transformed.x as f32, transformed.y as f32),
                (rect.x, rect.y)
            );
        }
    }
}

#[test]
fn search_suggestions_keep_ime_owner_and_preedit_does_not_query() {
    let (mut app, mut doc) = app();
    app.chrome.search_focus = true;
    let target = View::ime_target(&app).unwrap().id;
    app.chrome.menu = Some(koru_ui::Menu::new(
        &mut app.chrome.engine,
        (0.0, 0.0),
        vec![chrome::Choice::Document(DocumentId::new(), "Result".into())],
    ));
    assert_eq!(View::ime_target(&app).unwrap().id, target);
    let before = doc.version();
    assert!(View::ime(
        &mut app,
        &mut doc,
        target,
        &Ime::Preedit("に".into(), Some((3, 3)))
    ));
    assert_eq!(app.chrome.input.raw_text(), "");
    assert_eq!(doc.version(), before);
    assert!(View::ime(
        &mut app,
        &mut doc,
        target,
        &Ime::Commit("日本".into())
    ));
    assert_eq!(app.chrome.input.raw_text(), "日本");
    assert_eq!(doc.version(), before);
}

#[test]
fn stale_document_request_and_failed_landing_keep_existing_editor() {
    let (mut app, doc) = app();
    let old = app.document;
    let owner = app.leaf().unwrap().native_ime_owner();
    app.request_document(DocumentId::new(), None);
    let first = app.pending.unwrap().0;
    app.request_document(DocumentId::new(), None);
    assert_ne!(app.pending.unwrap().0.generation, first.generation);
    assert!(!app.land(&doc, DocumentId::new()));
    assert_eq!(app.document, old);
    assert_eq!(app.leaf().unwrap().native_ime_owner(), owner);
}

#[test]
fn both_children_publish_unique_occurrences_without_changing_widget_identity() {
    let (app, _) = app();
    let _ = Scope::Whole;
    let nodes = app.chrome_semantics(
        WindowSlot::from_raw(1),
        ViewSlot::from_raw(1),
        Rect::new(50.0, 60.0, 1000.0, 600.0),
    );
    let ids = nodes
        .iter()
        .map(|node| node.occurrence.node_id())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids.len(), nodes.len());
    assert!(app.regions.tasks.w > 0.0 && app.regions.document.w > 0.0);
}
