// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::mini_apps::{ComponentUpdate, UpdateEnvelope};

fn fixture() -> (MiniApp, Vec<WorkspaceDoc>) {
    let workspace = WorkspaceId::new();
    let declaration = Declaration::parse(model::FIXTURE.as_bytes()).unwrap();
    let documents = declaration
        .document_references()
        .into_iter()
        .map(|id| {
            let mut doc = WorkspaceDoc::create(workspace, "composition").unwrap();
            koru_workspace::document(&mut doc, "Source").unwrap();
            doc.stamp_document_id(DocumentId::from_uuid(id.as_uuid()))
                .unwrap();
            doc
        })
        .collect();
    let mut app = MiniApp::from_declaration(workspace, 90, declaration).unwrap();
    View::set_viewport(&mut app, 1000.0, 600.0);
    (app, documents)
}

#[test]
fn document_selection_lands_only_on_success_and_preserves_every_editor_owner() {
    let (mut app, documents) = fixture();
    let first = documents[0].own_document_id().unwrap();
    assert!(app.land(&documents[0], first));
    app.pending = None;
    let owner = app.leaf().unwrap().native_ime_owner();
    let initial = app.refinement_target();
    let second = documents[1].own_document_id().unwrap();
    app.request_document(second, None);
    assert_eq!(app.document, Some(first));
    assert_eq!(app.refinement_target(), initial);
    assert_eq!(app.leaf().unwrap().native_ime_owner(), owner);
    // A failure clears the request in the native host; it never replaces state.
    app.pending = None;
    assert_eq!(app.document, Some(first));
    for doc in &documents[1..] {
        assert!(app.land(doc, doc.own_document_id().unwrap()));
    }
    assert_eq!(app.retained_documents().count(), documents.len());
    assert!(app.land(&documents[0], first));
    assert_eq!(app.leaf().unwrap().native_ime_owner(), owner);
    assert_eq!(app.inactive.len(), documents.len() - 1);
}

#[test]
fn layout_refinement_retains_editor_and_v2_snapshot_reopens_selected_binding() {
    let (mut app, documents) = fixture();
    let doc = &documents[2];
    assert!(app.land(doc, doc.own_document_id().unwrap()));
    app.pending = None;
    let owner = app.leaf().unwrap().native_ime_owner();
    let target = app.refinement_target();
    let mut components = app.effective_definition().envelope.surface.components;
    components
        .iter_mut()
        .find(|n| n.id == "heading")
        .unwrap()
        .text = Some("Plan together".into());
    let update = UpdateEnvelope {
        version: "1.0".into(),
        update: ComponentUpdate {
            surface_id: target.surface_name(),
            components,
        },
    };
    app.refine(target, &update).unwrap();
    assert_eq!(app.leaf().unwrap().native_ime_owner(), owner);
    assert!(app.refine(target, &update).is_err());
    let saved = app.artifact_snapshot().unwrap();
    assert!(matches!(
        saved,
        koru_ai_core::mini_apps::artifact::SavedEnvelope::Composition(_)
    ));
    let bytes = saved.encode().unwrap();
    let decoded = koru_ai_core::mini_apps::artifact::SavedEnvelope::decode(&bytes).unwrap();
    let reopened = decoded
        .instantiate(
            decoded.workspace(),
            format!("mini-app-{}", uuid::Uuid::new_v4()),
        )
        .unwrap();
    assert_eq!(
        reopened.initial_document().unwrap().as_uuid(),
        doc.own_document_id().unwrap().as_uuid()
    );
    assert_eq!(reopened.document_references().len(), 4);
    assert_eq!(app.artifact_snapshot().unwrap(), saved);
    assert_eq!(
        app.snapshot_cache.borrow().as_ref().unwrap().0,
        app.refinement_target()
    );
}

#[test]
fn narrow_controls_remain_reachable_and_use_the_same_frame_as_the_editor() {
    let (mut app, _) = fixture();
    for (w, h) in [(1000.0, 600.0), (360.0, 600.0), (360.0, 320.0)] {
        View::set_viewport(&mut app, w, h);
        let surface = app.composition.as_ref().unwrap();
        assert!(
            surface.frame.diagnostics.is_empty(),
            "{:?}",
            surface.frame.diagnostics
        );
        assert!(app.regions.document.w > 0.0 && app.regions.document.h > 0.0);
        for name in ["travel", "next", "restaurants", "notes"] {
            let key = surface.key(name);
            assert!(surface.frame.node(key).unwrap().focusable);
        }
        let keys: Vec<_> = ["travel", "next", "restaurants", "notes"]
            .iter()
            .map(|id| surface.key(id))
            .collect();
        for key in keys {
            app.composition.as_mut().unwrap().reveal(key);
            app.layout();
            let surface = app.composition.as_ref().unwrap();
            let rect = surface.frame.node(key).unwrap().hit_region().unwrap();
            assert!(rect.w > 0.0 && rect.h > 0.0);
            assert_eq!(
                surface
                    .frame
                    .hit_at(rect.x + rect.w * 0.5, rect.y + rect.h * 0.5),
                Some(key)
            );
        }
    }
}

#[test]
fn explicit_fill_stretches_across_centered_parent_and_shares_row_space() {
    let (mut app, _) = fixture();
    let surface = app.composition.as_mut().unwrap();
    let mut envelope = surface.declaration.envelope.clone();
    for node in &mut envelope.surface.components {
        if node.id == "root" {
            node.layout.as_mut().unwrap().align = Some(model::Align::Center);
        }
        if ["travel", "next", "restaurants", "notes"].contains(&node.id.as_str()) {
            node.layout = Some(model::Layout {
                width: Some(model::Sizing::Fill),
                ..Default::default()
            });
        }
    }
    surface.declaration = Declaration::validate(envelope).unwrap();
    surface.dirty = true;
    app.layout();
    let surface = app.composition.as_ref().unwrap();
    let navigation = surface.frame.node(surface.key("navigation")).unwrap().rect;
    assert!(navigation.w > 900.0);
    let button = surface.frame.node(surface.key("travel")).unwrap().rect;
    assert!(button.w > 200.0);
}

#[test]
fn inactive_adoption_never_resizes_or_replaces_the_retained_leaf() {
    let (mut app, documents) = fixture();
    assert!(app.land(&documents[0], documents[0].own_document_id().unwrap()));
    let owner = app.leaf().unwrap().native_ime_owner();
    let viewport = app.leaf().unwrap().view().viewport;
    assert!(app.land(&documents[1], documents[1].own_document_id().unwrap()));
    app.adopt_cheap(&documents[0]);
    let (_, leaf) = app
        .inactive
        .iter()
        .find(|(_, l)| l.native_ime_owner() == owner)
        .unwrap();
    assert_eq!(leaf.view().viewport, viewport);
    View::set_viewport(&mut app, 400.0, 600.0);
    let (_, leaf) = app
        .inactive
        .iter()
        .find(|(_, l)| l.native_ime_owner() == owner)
        .unwrap();
    assert_eq!(leaf.view().viewport, viewport);
}

#[test]
fn appearance_invalidation_refreshes_cached_editor_once_at_the_same_viewport() {
    let (mut app, documents) = fixture();
    let first = documents[0].own_document_id().unwrap();
    let second = documents[1].own_document_id().unwrap();
    assert!(app.land(&documents[0], first));
    let leaf = app.leaf_mut().unwrap();
    let owner = leaf.native_ime_owner();
    let viewport = leaf.view().viewport;
    let caret = leaf.caret_spot();
    let scroll = leaf.view().scroll;
    let reflows = leaf.view().reflows;
    let document_version = documents[0].version();
    assert!(app.land(&documents[1], second));
    View::invalidate(&mut app);
    View::invalidate(&mut app);
    let (_, cached) = app.inactive.iter().find(|(id, _)| *id == first).unwrap();
    assert_eq!(cached.view().reflows, reflows, "hidden owner never reflows");
    assert_eq!(cached.view().viewport, viewport);
    assert!(app.inactive_invalidated.contains(&first));
    assert!(app.land(&documents[0], first));
    let leaf = app.leaf().unwrap();
    assert_eq!(leaf.native_ime_owner(), owner);
    assert_eq!(leaf.view().viewport, viewport);
    assert_eq!(leaf.caret_spot(), caret);
    assert_eq!(leaf.view().scroll, scroll);
    assert_eq!(leaf.view().reflows, reflows + 1);
    assert_eq!(documents[0].version(), document_version);
    assert!(!app.inactive_invalidated.contains(&first));
    assert!(app.land(&documents[1], second));
    assert!(app.land(&documents[0], first));
    assert_eq!(
        app.leaf().unwrap().view().reflows,
        reflows + 1,
        "clean cache hit does not reshape again"
    );
}

#[test]
fn stale_cached_document_and_new_viewport_settle_in_one_active_reflow() {
    let (mut app, mut documents) = fixture();
    let first = documents[0].own_document_id().unwrap();
    let second = documents[1].own_document_id().unwrap();
    assert!(app.land(&documents[0], first));
    let owner = app.leaf().unwrap().native_ime_owner();
    let reflows = app.leaf().unwrap().view().reflows;
    let old_viewport = app.leaf().unwrap().view().viewport;
    assert!(app.land(&documents[1], second));
    let root = koru_workspace::document_root(&documents[0]).unwrap();
    documents[0]
        .set_text(root, "Changed while inactive")
        .unwrap();
    app.adopt_cheap(&documents[0]);
    View::set_viewport(&mut app, 420.0, 700.0);
    View::invalidate(&mut app);
    let (_, cached) = app.inactive.iter().find(|(id, _)| *id == first).unwrap();
    assert!(cached.body().is_stale());
    assert_eq!(cached.view().reflows, reflows);
    assert_eq!(cached.view().viewport, old_viewport);
    assert!(app.land(&documents[0], first));
    let leaf = app.leaf().unwrap();
    assert_eq!(leaf.native_ime_owner(), owner);
    assert_eq!(leaf.view().reflows, reflows + 1);
    assert_eq!(
        leaf.view().viewport,
        (app.regions.document.w, app.regions.document.h)
    );
    assert!(!leaf.body().is_stale());
    assert!(
        leaf.body()
            .rows
            .iter()
            .any(|row| row.text == "Changed while inactive")
    );
}

#[test]
fn cached_native_ime_owner_can_cancel_but_never_commit_to_another_document() {
    use winit::event::Ime;
    let (mut app, mut documents) = fixture();
    let first = documents[0].own_document_id().unwrap();
    assert!(app.land(&documents[0], first));
    app.leaf_mut().unwrap().view_mut().active = true;
    let target = View::ime_target(&app).unwrap().id;
    let before = documents[0].version();
    assert!(View::ime(
        &mut app,
        &mut documents[0],
        target,
        &Ime::Preedit("compose".into(), Some((7, 7)))
    ));
    assert!(app.land(&documents[1], documents[1].own_document_id().unwrap()));
    let second_before = documents[1].version();
    assert!(!View::ime(
        &mut app,
        &mut documents[1],
        target,
        &Ime::Commit("wrong owner".into())
    ));
    assert!(View::ime(
        &mut app,
        &mut documents[0],
        target,
        &Ime::Disabled
    ));
    assert_eq!(documents[0].version(), before);
    assert_eq!(documents[1].version(), second_before);
}

#[test]
fn column_controls_scroll_and_long_labels_keep_their_full_accessible_name() {
    let (mut app, _) = fixture();
    let surface = app.composition.as_mut().unwrap();
    let mut envelope = surface.declaration.envelope.clone();
    let long = "Long document title ".repeat(10);
    for node in &mut envelope.surface.components {
        if node.id == "navigation" {
            node.component = "Column".into();
        }
        if node.id == "restaurants" {
            node.text = Some(long.clone());
        }
    }
    surface.declaration = Declaration::validate(envelope).unwrap();
    surface.dirty = true;
    View::set_viewport(&mut app, 360.0, 320.0);
    let surface = app.composition.as_ref().unwrap();
    let key = surface.key("restaurants");
    let node = surface.frame.node(key).unwrap();
    assert_eq!(node.label, Some(el::Name::Text(long)));
    assert!(node.rect.w <= 360.0);
    assert!(surface.frame.nodes.iter().any(|node| node.scroll.is_some()));
    assert!(app.regions.document.h >= ActionButton::H * 2.0);
}
