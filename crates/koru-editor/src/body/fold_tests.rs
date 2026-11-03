// SPDX-License-Identifier: MPL-2.0

//! A closed toggle hides its body in this view only: the rows go, the nodes
//! stay, the document does not change, and the caret never stays inside.

use super::*;
use koru_core::WorkspaceId;

struct Doc {
    ws: WorkspaceDoc,
    body: Body,
    toggle: NodeId,
    inner: NodeId,
    child: NodeId,
    after: NodeId,
}

/// Title, then `Outer` with body [`a`, `Inner` with body [`b`]], then `after`.
fn doc() -> Doc {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = ws
        .insert(Parent::Root, Node::document("Spec").unwrap())
        .unwrap();
    let toggle = ws
        .insert(Parent::Node(doc), Node::toggle("Outer").unwrap())
        .unwrap();
    let child = ws
        .insert(Parent::Node(toggle), Node::paragraph("a").unwrap())
        .unwrap();
    let inner = ws
        .insert(Parent::Node(toggle), Node::toggle("Inner").unwrap())
        .unwrap();
    ws.insert(Parent::Node(inner), Node::paragraph("b").unwrap())
        .unwrap();
    let after = ws
        .insert(Parent::Node(doc), Node::paragraph("after").unwrap())
        .unwrap();
    let mut body = Body::empty_over(&ws);
    body.root = Some(doc);
    body.flat = true;
    body.title_row = true;
    body.rebuild(&ws);
    Doc {
        ws,
        body,
        toggle,
        inner,
        child,
        after,
    }
}

fn shown(body: &Body) -> Vec<(String, u16)> {
    body.rows
        .iter()
        .skip(1)
        .map(|row| (row.text.clone(), row.depth))
        .collect()
}

#[test]
fn an_open_toggle_indents_its_body_one_step_in_a_flat_document() {
    let d = doc();
    assert_eq!(
        shown(&d.body),
        vec![
            ("Outer".into(), 0),
            ("a".into(), 1),
            ("Inner".into(), 1),
            ("b".into(), 2),
            ("after".into(), 0),
        ]
    );
    assert!(d.body.rows[1].is_toggle());
    assert!(d.body.rows[1].text_x() > d.body.rows[5].text_x());
}

#[test]
fn closing_hides_the_body_without_touching_the_document() {
    let mut d = doc();
    let before = d.ws.version();
    assert!(d.body.set_fold(&d.ws, d.toggle, true));
    assert_eq!(
        shown(&d.body),
        vec![("Outer".into(), 0), ("after".into(), 0)]
    );
    assert!(d.body.rows[1].fold_closed);
    assert_eq!(d.ws.version(), before, "a fold is not an edit");
    assert_eq!(
        d.body.take_fold_intents(),
        vec![crate::folds::FoldIntent {
            node: d.toggle,
            closed: true
        }]
    );
    // Asking again owes nothing.
    assert!(!d.body.set_fold(&d.ws, d.toggle, true));
    assert!(d.body.take_fold_intents().is_empty());
}

#[test]
fn a_nested_fold_survives_its_parent_closing_and_opening() {
    let mut d = doc();
    d.body.set_fold(&d.ws, d.inner, true);
    d.body.set_fold(&d.ws, d.toggle, true);
    d.body.set_fold(&d.ws, d.toggle, false);
    assert_eq!(
        shown(&d.body),
        vec![
            ("Outer".into(), 0),
            ("a".into(), 1),
            ("Inner".into(), 1),
            ("after".into(), 0),
        ],
        "the parent changed only the parent"
    );
}

#[test]
fn a_fold_that_hides_the_caret_moves_it_to_the_toggle_title() {
    let mut d = doc();
    d.body.focus_on(d.child);
    d.body.set_fold(&d.ws, d.toggle, true);
    assert_eq!(d.body.focused_id(), Some(d.toggle));
    // A caret outside the fold stays where it was.
    d.body.set_fold(&d.ws, d.toggle, false);
    d.body.focus_on(d.after);
    d.body.set_fold(&d.ws, d.toggle, true);
    assert_eq!(d.body.focused_id(), Some(d.after));
}

#[test]
fn adopting_the_hosts_answer_owes_no_intent_and_only_folds_toggles() {
    let mut d = doc();
    assert!(
        d.body
            .adopt_folds(&d.ws, HashSet::from([d.inner]))
    );
    assert!(d.body.take_fold_intents().is_empty());
    assert!(!shown(&d.body).iter().any(|(text, _)| text == "b"));
    // A paragraph is not a toggle and cannot be folded.
    assert!(!d.body.set_fold(&d.ws, d.after, true));
}

#[test]
fn the_line_appended_after_a_toggle_is_prose() {
    let mut d = doc();
    let id = d.body.append_row(&mut d.ws, Some(d.toggle)).unwrap();
    assert_eq!(d.ws.kind_of(id), Some(Kind::Paragraph));
}

fn caret_at(d: &mut Doc, id: NodeId, at: usize) {
    d.body.focus_on(id);
    d.body.focused_mut().unwrap().place_caret(at);
}

#[test]
fn enter_in_an_open_title_starts_the_body_and_in_a_closed_one_the_next_line() {
    // The split itself, at a byte the layout-free test cannot place a caret
    // on; `split_focused` routes a toggle title here.
    let mut d = doc();
    assert!(d.body.split_toggle_title(&mut d.ws, d.toggle, 3, false));
    let first = d.ws.children(Parent::Node(d.toggle)).unwrap()[0];
    assert_eq!(d.body.focused_id(), Some(first));
    assert_eq!(d.ws.kind_of(first), Some(Kind::Paragraph));
    assert_eq!(d.body.rows[1].text, "Out");
    assert_eq!(d.body.focused().unwrap().text, "er");

    d.body.set_fold(&d.ws, d.toggle, true);
    assert!(d.body.split_toggle_title(&mut d.ws, d.toggle, 3, true));
    let line = d.body.focused_id().unwrap();
    assert_eq!(d.ws.parent(line).unwrap(), d.ws.parent(d.toggle).unwrap());
    assert_eq!(
        d.ws.sibling_index(line).unwrap(),
        d.ws.sibling_index(d.toggle).unwrap() + 1
    );

    // And Enter on the title row goes there.
    let toggle = d.toggle;
    caret_at(&mut d, toggle, 0);
    assert!(d.body.split_focused(&mut d.ws));
    assert_ne!(d.body.focused_id(), Some(toggle));
}

#[test]
fn enter_on_the_empty_last_body_line_steps_out_below_the_toggle() {
    let mut d = doc();
    let empty = d
        .ws
        .insert(Parent::Node(d.toggle), Node::paragraph("").unwrap())
        .unwrap();
    d.body.rebuild(&d.ws);
    caret_at(&mut d, empty, 0);
    assert!(d.body.split_focused(&mut d.ws));
    assert_eq!(d.ws.parent(empty).unwrap(), d.ws.parent(d.toggle).unwrap());
    assert_eq!(
        d.ws.sibling_index(empty).unwrap(),
        d.ws.sibling_index(d.toggle).unwrap() + 1
    );
}

#[test]
fn backspace_at_the_title_start_unwraps_without_losing_anything() {
    let mut d = doc();
    let toggle = d.toggle;
    caret_at(&mut d, toggle, 0);
    assert!(d.body.merge(&mut d.ws, true).is_some());
    assert_eq!(d.ws.kind_of(d.toggle), Some(Kind::Paragraph));
    assert_eq!(
        shown(&d.body)
            .into_iter()
            .map(|(text, _)| text)
            .collect::<Vec<_>>(),
        vec!["Outer", "a", "Inner", "b", "after"]
    );
    assert_eq!(d.body.focused_id(), Some(d.toggle));
}
