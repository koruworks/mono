// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn mini_app_explicit_invalidation_rearms_child_sizing_without_hidden_layout() {
    let (mut app, _) = super::super::tests::app();
    let size = app.tasks_viewport.expect("visible tasks have been sized");
    View::invalidate(&mut app);
    assert!(app.tasks_viewport.is_none());
    app.layout();
    assert_eq!(app.tasks_viewport, Some(size));
}

fn update(app: &MiniApp, swapped: bool) -> UpdateEnvelope {
    let mut root = app
        .effective_definition()
        .envelope
        .surface
        .components
        .remove(0);
    root.arrangement = Some(
        if swapped {
            "document-first"
        } else {
            "tasks-first"
        }
        .into(),
    );
    UpdateEnvelope {
        version: "1.0".into(),
        update: koru_ai_core::mini_apps::ComponentUpdate {
            surface_id: app.refinement_target().surface_name(),
            components: vec![root],
        },
    }
}

#[test]
fn mini_app_revision_tracks_effective_bindings_and_arrangement_only() {
    let mut app = MiniApp::fixture(WorkspaceId::new(), 41).unwrap();
    let initial = app.refinement_target();
    let before = app.effective_definition();
    app.select_tasks(app.tasks.target());
    assert_eq!(initial, app.refinement_target());
    let envelope = update(&app, true);
    assert!(app.refine(initial, &envelope).is_ok());
    assert_eq!(app.declaration_revision(), 1);
    assert!(app.refine(initial, &envelope).is_err());
    assert!(app.refine(app.refinement_target(), &envelope).is_ok());
    assert_eq!(app.declaration_revision(), 1);
    assert_eq!(
        before.task_selection(),
        app.effective_definition().task_selection()
    );
    app.select_tasks(RailTarget::Bucket(Bucket::Completed));
    assert_eq!(app.declaration_revision(), 2);
    assert_eq!(
        app.effective_definition().task_selection(),
        Some(&TaskSelection::Bucket("completed".into()))
    );
    let target = app.refinement_target();
    app.request_document(DocumentId::new(), None);
    assert_eq!(
        target,
        app.refinement_target(),
        "loads have an independent generation"
    );
    let mut invalid = update(&app, false);
    invalid.update.components[0]
        .children
        .as_mut()
        .unwrap()
        .pop();
    let before = app.effective_definition();
    assert!(app.refine(target, &invalid).is_err());
    assert_eq!(before, app.effective_definition());
    assert_ne!(
        app.refinement_target().surface_id,
        MiniApp::fixture(app.workspace, 41)
            .unwrap()
            .refinement_target()
            .surface_id
    );
}

#[test]
fn mini_app_swap_retains_native_composition_selection_scroll_and_undo_without_reflow() {
    use winit::event::Ime;
    let (mut app, mut doc) = super::super::tests::app();
    doc.enable_undo();
    let revision = app.declaration_revision();
    assert!(app.land(&doc, app.document.unwrap()));
    assert_eq!(
        revision,
        app.declaration_revision(),
        "same document reload is not a binding change"
    );
    let task_owner = &app.tasks as *const TasksView;
    let leaf_owner = app.leaf().unwrap() as *const Leaf;
    let leaf = app.leaf_mut().unwrap();
    leaf.view_mut().active = true;
    leaf.body_mut().rows[0].select_range_now(2, 1);
    let row_id = leaf.body().rows[0].id;
    let original = doc
        .read(row_id)
        .unwrap()
        .content
        .as_text()
        .unwrap()
        .to_owned();
    let target = View::ime_target(&app).unwrap().id;
    assert!(View::ime(
        &mut app,
        &mut doc,
        target,
        &Ime::Preedit("日本".into(), Some((3, 3)))
    ));
    app.leaf_mut().unwrap().view_mut().scroll = 17.0;
    let selection = app.leaf().unwrap().body().rows[0].directed_range();
    let preedit = app.leaf().unwrap().body().rows[0].text.clone();
    let reflows = app.leaf().unwrap().view().reflows;
    let cursor_before = View::ime_target(&app).unwrap().cursor_area;
    let old_origin = app.regions.document.x;
    let before = doc.version();
    let envelope = update(&app, true);
    app.refine(app.refinement_target(), &envelope).unwrap();
    assert_eq!(&app.tasks as *const TasksView, task_owner);
    assert_eq!(app.leaf().unwrap() as *const Leaf, leaf_owner);
    assert_eq!(View::ime_target(&app).unwrap().id, target);
    if let Some(before) = cursor_before {
        let after = View::ime_target(&app).unwrap().cursor_area.unwrap();
        assert_eq!(after.x - before.x, app.regions.document.x - old_origin);
        assert_eq!(after.y, before.y);
    }
    assert_eq!(
        app.leaf().unwrap().body().rows[0].directed_range(),
        selection
    );
    assert_eq!(app.leaf().unwrap().body().rows[0].text, preedit);
    assert_eq!(app.leaf().unwrap().view().scroll, 17.0);
    assert_eq!(app.leaf().unwrap().view().reflows, reflows);
    assert_eq!(doc.version(), before);
    assert_eq!(app.declaration_revision(), revision + 1);
    assert!(View::ime(
        &mut app,
        &mut doc,
        target,
        &Ime::Commit("日本".into())
    ));
    assert_eq!(
        app.declaration_revision(),
        revision + 1,
        "authored content is not a declaration revision"
    );
    assert!(doc.undo().unwrap());
    assert_eq!(
        doc.read(row_id).unwrap().content.as_text().unwrap(),
        original
    );
}
