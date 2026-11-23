// SPDX-License-Identifier: MPL-2.0

use super::*;

/// The first-run card's ten.
const TEN: [&str; 10] = ["🌱", "📓", "💼", "🎓", "🏠", "🚀", "🧠", "🎨", "📚", "🎯"];

#[test]
fn cells_wrap_to_the_width_and_height_follows_the_lines() {
    let row = EmojiRow::new(&TEN);
    // Everything on one line where it fits.
    let wide = row.width();
    assert_eq!(row.columns(wide), 10);
    assert_eq!(row.lines(Rect::new(0.0, 0.0, wide, 0.0)).len(), 1);
    // Four a line in 140 (4 × 28 + 3 × 8 = 136): ten on three lines.
    let rect = Rect::new(10.0, 20.0, 140.0, row.height(140.0));
    assert_eq!(row.columns(rect.w), 4);
    let lines = row.lines(rect);
    assert_eq!(
        lines.iter().map(|(first, _)| *first).collect::<Vec<_>>(),
        vec![0, 4, 8]
    );
    let cells = row.cells(rect);
    assert_eq!(cells.len(), 10);
    for (first, line) in &lines {
        assert_eq!(cells[*first].y, line.y);
    }
    for cell in &cells {
        assert!(cell.x >= rect.x && cell.x + cell.w <= rect.x + rect.w);
        assert_eq!(cell.w, cell.h, "square cells");
    }
    // In order: along a line, then down to the next.
    for pair in cells.windows(2) {
        assert!(pair[1].y > pair[0].y || (pair[1].y == pair[0].y && pair[1].x > pair[0].x));
    }
    // More lines, more height; the same lines, the same height.
    assert!(row.height(140.0) > row.height(wide));
    assert_eq!(row.height(wide), row.height(wide + 50.0));
    // A host's cell size changes the arithmetic, not its shape.
    let small = EmojiRow::new(&TEN).cell(20.0, 4.0);
    assert_eq!(small.columns(140.0), 6);
    // Narrower than one cell still keeps one a line.
    assert_eq!(row.columns(4.0), 1);
}

#[test]
fn hit_finds_the_cell_on_every_line() {
    let row = EmojiRow::new(&TEN);
    let rect = Rect::new(5.0, 5.0, 140.0, row.height(140.0));
    let cells = row.cells(rect);
    for (index, cell) in cells.iter().enumerate() {
        let (x, y) = cell.centre();
        assert_eq!(row.hit(rect, x, y), Some(index));
    }
    // The gap between two cells, and between two lines, is nothing.
    let first = cells[0];
    assert_eq!(
        row.hit(rect, first.x + first.w + EmojiRow::GAP * 0.5, first.y + 1.0),
        None
    );
    assert_eq!(
        row.hit(rect, first.x + 1.0, first.y + first.h + Swatch::RING_REACH),
        None
    );
}

#[test]
fn the_rings_reach_stays_inside_the_measured_height() {
    let row = EmojiRow::new(&TEN);
    for width in [140.0, 200.0, row.width()] {
        let rect = Rect::new(0.0, 30.0, width, row.height(width));
        let cells = row.cells(rect);
        for cell in &cells {
            assert!(
                cell.y - Swatch::RING_REACH >= rect.y,
                "{width}: a ring reaches above the row"
            );
            assert!(
                cell.y + cell.h + Swatch::RING_REACH <= rect.y + rect.h + 0.001,
                "{width}: a ring reaches below the row"
            );
        }
        let lines = row.lines(rect);
        for pair in lines.windows(2) {
            assert!(pair[1].1.y - (pair[0].1.y + pair[0].1.h) >= 2.0 * Swatch::RING_REACH);
        }
    }
}

fn records(row: EmojiRow<'_>, rect: Rect) -> Vec<inspect::Record> {
    let mut engine = TextEngine::new();
    let mut canvas = Scene::new();
    let root = Affine::IDENTITY;
    let capture = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, 400.0, 200.0));
    row.draw(&mut canvas, root, &mut engine, rect);
    capture.finish().records
}

/// A selection the row does not hold rings no cell; the ring strokes drawn
/// are the focus ring's alone.
#[test]
fn no_selection_rings_no_cell() {
    let rect = Rect::new(0.0, 0.0, 400.0, EmojiRow::new(&TEN).height(400.0));
    let strokes = |records: &[inspect::Record]| {
        records
            .iter()
            .filter(|record| record.name == "stroke")
            .count()
    };
    assert_eq!(
        strokes(&records(EmojiRow::new(&TEN).selected(None), rect)),
        0
    );
    assert_eq!(
        strokes(&records(EmojiRow::new(&TEN).selected(Some(3)), rect)),
        1
    );
    // Out of range is no selection either.
    assert_eq!(
        strokes(&records(EmojiRow::new(&TEN).selected(Some(10)), rect)),
        0
    );
    let cells = records(EmojiRow::new(&TEN).selected(None), rect);
    assert!(
        cells
            .iter()
            .filter(|record| record.name == "KoruEmojiCell")
            .all(|record| record
                .properties
                .iter()
                .any(|p| p.name == "selected / hovered / focused" && p.value.starts_with("false")))
    );
}

#[test]
fn the_row_is_one_inspector_component_with_a_part_per_cell() {
    let row = EmojiRow::new(&TEN).selected(Some(0)).focused(Some(1));
    let rect = Rect::new(10.0, 10.0, 300.0, row.height(300.0));
    let snapshot = records(row, rect);
    let rows: Vec<usize> = snapshot
        .iter()
        .enumerate()
        .filter_map(|(i, r)| (r.name == "KoruEmojiRow").then_some(i))
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(snapshot[rows[0]].kind, "component");
    let cells: Vec<_> = snapshot
        .iter()
        .filter(|r| r.name == "KoruEmojiCell")
        .collect();
    assert_eq!(cells.len(), 10);
    assert!(
        cells
            .iter()
            .all(|c| c.kind == "part" && c.parent == Some(rows[0]))
    );
    assert!(
        cells[0]
            .properties
            .iter()
            .any(|p| p.name == "emoji" && p.value == "🌱")
    );
}
