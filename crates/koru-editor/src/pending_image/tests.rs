// SPDX-License-Identifier: MPL-2.0

//! The room a loading image holds: where it opens, and that the picture which
//! replaces it lands in exactly that room.

use koru_core::WorkspaceId;
use koru_core::node::Node;
use koru_core::schema::Parent;

use super::*;

const W: f32 = 900.0;

fn doc(lines: &[&str]) -> (WorkspaceDoc, Body, Vec<NodeId>, TextEngine) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let ids = lines
        .iter()
        .map(|line| {
            ws.insert(Parent::Root, Node::paragraph(*line).unwrap())
                .unwrap()
        })
        .collect();
    let body = Body::new(&ws);
    (ws, body, ids, TextEngine::new())
}

fn lay_out(body: &mut Body, pending: &[PendingImage], engine: &mut TextEngine) {
    reserve(body, pending, W);
    rowlayout::reflow_all(engine, body, W, 0.0);
}

fn y_of(body: &Body, id: NodeId) -> f32 {
    body.rows[body.index_of(id).unwrap()].y
}

#[test]
fn the_picture_lands_in_the_room_its_placeholder_held() {
    let (mut ws, mut body, ids, mut engine) = doc(&["one", "two"]);
    let mut pending = Vec::new();
    begin(
        &mut pending,
        &ws,
        &body,
        true,
        1,
        Place::After(ids[0]),
        Instant::now(),
    );
    assert!(size(&mut pending, 1, &[(200, 100)]));
    lay_out(&mut body, &pending, &mut engine);
    let band = body.reserved[0].clone();
    let block = band.blocks().next().unwrap();
    let two_with_placeholder = y_of(&body, ids[1]);
    assert!(block.y > y_of(&body, ids[0]));
    assert!(two_with_placeholder > block.y + block.h);

    // The image lands: the placeholder goes, the row comes.
    end(&mut pending, 1).unwrap();
    let image = ws
        .insert_after(
            ids[0],
            Node::image(BlobHash("h".into()), "image/png", Some(200), Some(100)).unwrap(),
        )
        .unwrap();
    body.rebuild(&ws);
    lay_out(&mut body, &pending, &mut engine);
    assert!(body.reserved.is_empty());
    let row = &body.rows[body.index_of(image).unwrap()];
    assert_eq!(row.y, block.y, "the picture is where its block was");
    assert_eq!(row.height, block.h);
    assert_eq!(
        y_of(&body, ids[1]),
        two_with_placeholder,
        "and nothing below it moved"
    );
}

#[test]
fn a_replacement_opens_above_its_first_row_and_a_drop_after_the_last() {
    let (ws, mut body, ids, mut engine) = doc(&["one", "two"]);
    let mut pending = Vec::new();
    let now = Instant::now();
    begin(
        &mut pending,
        &ws,
        &body,
        true,
        1,
        Place::Before(ids[1]),
        now,
    );
    begin(&mut pending, &ws, &body, true, 2, Place::End, now);
    lay_out(&mut body, &pending, &mut engine);
    let index = body.index_of(ids[1]).unwrap();
    assert_eq!(body.reserved[0].before_row, index);
    assert_eq!(body.reserved[1].before_row, body.rows.len());
    let end = body.reserved[1].blocks().next().unwrap();
    assert!(end.y > y_of(&body, ids[1]));
    assert!(
        rowlayout::content_height(&body) >= end.y + end.h,
        "the page scrolls to a placeholder after the last row"
    );
    // Unsized until the header is read: one modest block.
    assert_eq!(body.reserved[1].blocks().count(), 1);
    assert_eq!(end.w, UNSIZED.0 as f32);
}

#[test]
fn a_deleted_anchor_hands_the_band_to_the_line_before_it() {
    let (mut ws, mut body, ids, mut engine) = doc(&["one", "two", "three"]);
    let mut pending = Vec::new();
    begin(
        &mut pending,
        &ws,
        &body,
        true,
        1,
        Place::After(ids[1]),
        Instant::now(),
    );
    ws.delete(ids[1]).unwrap();
    body.rebuild(&ws);
    lay_out(&mut body, &pending, &mut engine);
    assert_eq!(
        body.reserved[0].before_row,
        body.index_of(ids[0]).unwrap() + 1
    );
}

#[test]
fn nothing_loading_is_nothing_reserved_and_no_frame() {
    let (ws, mut body, ids, mut engine) = doc(&["one"]);
    let mut pending = Vec::new();
    begin(
        &mut pending,
        &ws,
        &body,
        true,
        1,
        Place::After(ids[0]),
        Instant::now(),
    );
    assert!(animating(&pending, false));
    assert!(!animating(&pending, true), "Reduce Motion rests the pulse");
    lay_out(&mut body, &pending, &mut engine);
    assert_eq!(body.reserved.len(), 1);
    end(&mut pending, 1);
    lay_out(&mut body, &pending, &mut engine);
    assert!(body.reserved.is_empty());
    assert!(!animating(&pending, false));
}

#[test]
fn the_caret_mark_notices_the_caret_leaving() {
    let (_ws, mut body, ids, _engine) = doc(&["one", "two"]);
    body.focus_on(ids[0]);
    let before = CaretMark::of(&body, true);
    assert_eq!(before, CaretMark::of(&body, true));
    body.focus_on(ids[1]);
    assert_ne!(before, CaretMark::of(&body, true));
    // Typing on the same line is the doc view's test, which drives real keys.
}
