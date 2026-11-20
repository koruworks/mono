// SPDX-License-Identifier: MPL-2.0

use super::*;
use winit::event::Ime;

#[test]
fn native_target_separates_support_from_visible_caret_and_geometry() {
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let target = view.ime_target().unwrap();
    assert!(View::ime(
        &mut view,
        &mut ws,
        target.id,
        &Ime::Preedit("日本".into(), None)
    ));
    let hidden = view.ime_target().unwrap();
    assert_eq!(hidden.id, target.id);
    assert!(hidden.cursor_area.is_none());
    view.editor.set_viewport(420.0, 700.0);
    assert_eq!(view.ime_target().unwrap().id, target.id);
    assert!(View::ime(&mut view, &mut ws, target.id, &Ime::Disabled));
    assert!(view.ime_target().unwrap().cursor_area.is_some());
    view.editor.view.active = false;
    assert!(view.ime_target().is_none());
}

#[test]
fn native_search_refreshes_only_authored_commit_and_focus_fences_body_events() {
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let body = view.ime_target().unwrap().id;
    view.key(&mut ws, &ch("f"), Some("f"), koru_keys::PRIMARY);
    let find = view.ime_target().unwrap().id;
    assert_ne!(body, find);
    assert!(!View::ime(
        &mut view,
        &mut ws,
        body,
        &Ime::Commit("wrong target".into())
    ));
    let refreshes = view.search.refreshes();
    View::ime(
        &mut view,
        &mut ws,
        find,
        &Ime::Preedit("日本".into(), Some((3, 3))),
    );
    assert_eq!(view.search.refreshes(), refreshes);
    View::ime(&mut view, &mut ws, find, &Ime::Preedit(String::new(), None));
    assert_eq!(view.search.refreshes(), refreshes);
    View::ime(&mut view, &mut ws, find, &Ime::Commit("日本語".into()));
    assert_eq!(view.search.refreshes(), refreshes + 1);
    assert_eq!(
        view.search.field_mut(FieldId::Find).unwrap().raw_text(),
        "日本語"
    );
}

#[test]
fn reopening_same_document_mints_a_new_native_input_lifetime() {
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let old = view.ime_target().unwrap().id;
    let root = view.editor.body.root.unwrap();
    let mut reopened = BuiltInDocument::open(&ws, root);
    reopened.editor.view.active = true;
    let new = reopened.ime_target().unwrap().id;
    assert_ne!(new.owner, old.owner);
    let before = ws.version();
    assert!(!View::ime(
        &mut reopened,
        &mut ws,
        old,
        &Ime::Commit("late".into())
    ));
    assert_eq!(ws.version(), before);
}

#[test]
fn native_find_and_replace_have_distinct_targets_and_authored_values() {
    let (mut ws, mut view) = document();
    view.key(&mut ws, &ch("h"), Some("h"), koru_keys::PRIMARY);
    let find = view.ime_target().unwrap().id;
    View::ime(&mut view, &mut ws, find, &Ime::Commit("find".into()));
    view.key(
        &mut ws,
        &Key::Named(NamedKey::Tab),
        None,
        ModifiersState::empty(),
    );
    let replace = view.ime_target().unwrap().id;
    assert_ne!(find, replace);
    let refreshes = view.search.refreshes();
    View::ime(
        &mut view,
        &mut ws,
        replace,
        &Ime::Commit("replacement".into()),
    );
    assert_eq!(view.search.refreshes(), refreshes);
    assert_eq!(
        view.search.field_mut(FieldId::Find).unwrap().raw_text(),
        "find"
    );
    assert_eq!(
        view.search.field_mut(FieldId::Replace).unwrap().raw_text(),
        "replacement"
    );
    assert!(!View::ime(
        &mut view,
        &mut ws,
        find,
        &Ime::Commit("late".into())
    ));
}

#[test]
fn native_targets_refuse_read_only_and_nontext_rows() {
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    view.editor.view.read_only = true;
    assert!(view.ime_target().is_none());
    view.editor.view.read_only = false;
    let root = view.editor.body.root.unwrap();
    for node in [
        Node::divider().unwrap(),
        Node::image(
            koru_core::node::BlobHash("00".repeat(32)),
            "image/png",
            Some(16),
            Some(16),
        )
        .unwrap(),
        Node::table("| A |\n| --- |\n| B |").unwrap(),
    ] {
        let id = ws.insert(Parent::Node(root), node).unwrap();
        view.editor.body.rebuild(&ws);
        view.editor.body.focus_on(id);
        view.editor.reflow();
        assert!(view.ime_target().is_none());
    }
}
