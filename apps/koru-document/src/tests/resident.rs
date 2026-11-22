// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn background_editor_uses_resident_authority_and_refuses_another_documents_input() {
    let workspace = WorkspaceId::new();
    let mut a = WorkspaceDoc::create(workspace, "A metadata").unwrap();
    let root = koru_workspace::document(&mut a, "A title").unwrap();
    let id = koru_core::DocumentId::new();
    a.stamp_document_id(id).unwrap();
    let mut view = BuiltInDocument::open(&a, root);
    view.editor.view.active = true;
    let mut b = WorkspaceDoc::create(workspace, "B metadata").unwrap();
    koru_workspace::document(&mut b, "B title").unwrap();
    b.stamp_document_id(koru_core::DocumentId::new()).unwrap();
    let before = (a.version(), b.version());
    let caret = view.editor.body.focus;
    assert!(view.native_ime(
        &mut a,
        &winit::event::Ime::Preedit("pending".into(), Some((7, 7)))
    ));
    let selected = view
        .frame_document(&b, |requested| {
            assert_eq!(requested, id);
            Some(&a)
        })
        .unwrap();
    assert!(
        std::ptr::eq(selected, &a),
        "spelling and chrome receive the original resident authority"
    );
    assert_eq!(selected.name().as_deref(), Some("A metadata"));
    assert_eq!(selected.read(root).unwrap(), a.read(root).unwrap());
    assert!(
        view.frame_document(&b, |_| None).is_none(),
        "missing resident never reads the new active document"
    );
    assert!(!view.key(
        &mut b,
        &Key::Character("x".into()),
        Some("x"),
        ModifiersState::empty()
    ));
    assert!(!view.native_ime(&mut b, &winit::event::Ime::Commit("x".into())));
    assert!(view.native_ime_cancel(&mut a));
    assert_eq!(view.editor.body.focus, caret);
    assert!(!View::maintain(
        &mut view,
        &mut b,
        std::time::Instant::now()
    ));
    View::paste_text(&mut view, &mut b, "wrong document");
    assert_eq!((a.version(), b.version()), before);
}
