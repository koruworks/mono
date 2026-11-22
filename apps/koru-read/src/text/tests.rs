// SPDX-License-Identifier: MPL-2.0

use super::*;

const SIZE: f64 = 10.0;
const ADVANCE: f64 = 5.0;

/// The glyphs of `text` set upright from `(x, y)`, each half an em wide and
/// touching the next — so the letters of a word have no gap between them.
fn word(text: &str, x: f64, y: f64) -> Vec<Placed> {
    text.chars()
        .enumerate()
        .map(|(i, c)| Placed {
            orientation: Orientation::Upright,
            u: x + i as f64 * ADVANCE,
            v: y,
            advance: ADVANCE,
            size: SIZE,
            text: c.to_string(),
        })
        .collect()
}

/// Where the glyph after `text`, set at `x`, would start.
fn after(text: &str, x: f64) -> f64 {
    x + text.chars().count() as f64 * ADVANCE
}

#[test]
fn words_positioned_apart_get_a_space_and_touching_letters_do_not() {
    let mut glyphs = word("Hello", 0.0, 700.0);
    // A third of an em of white space: a word break.
    glyphs.extend(word("world", after("Hello", 0.0) + 3.3, 700.0));
    // A tenth of an em: kerning, not a break.
    glyphs.extend(word(
        "ly",
        after("Hello", 0.0) + 3.3 + after("world", 0.0) + 1.0,
        700.0,
    ));
    assert_eq!(assemble(&glyphs), "Hello worldly");
}

#[test]
fn a_drawn_space_is_kept_once() {
    let mut glyphs = word("a b", 0.0, 700.0);
    // And a positioned gap after a drawn space adds nothing more.
    glyphs.extend(word("c", after("a b", 0.0) + 4.0, 700.0));
    assert_eq!(assemble(&glyphs), "a b c");
}

/// Content streams draw in any order; reading order is top to bottom, then
/// left to right.
#[test]
fn lines_are_read_top_to_bottom_whatever_order_they_were_drawn_in() {
    let mut glyphs = word("third", 0.0, 660.0);
    glyphs.extend(word("first", 0.0, 700.0));
    glyphs.extend(word("right", 200.0, 680.0));
    glyphs.extend(word("second", 0.0, 680.0));
    assert_eq!(assemble(&glyphs), "first\nsecond right\nthird");
}

#[test]
fn a_superscript_stays_on_its_line() {
    let mut glyphs = word("E=mc", 0.0, 700.0);
    let mut two = word("2", after("E=mc", 0.0), 703.3);
    two[0].size = 7.0;
    glyphs.extend(two);
    glyphs.extend(word("next", 0.0, 686.0));
    assert_eq!(assemble(&glyphs), "E=mc2\nnext");
}

/// Text that is filled and then stroked arrives twice, at the same place.
#[test]
fn a_glyph_drawn_twice_in_place_is_kept_once() {
    let mut glyphs = word("bold", 0.0, 700.0);
    glyphs.extend(word("bold", 0.2, 700.0));
    assert_eq!(assemble(&glyphs), "bold");
    // The same letter twice in a row, a letter apart, is two letters.
    assert_eq!(assemble(&word("aa", 0.0, 700.0)), "aa");
}

#[test]
fn turned_text_reads_after_upright_text_in_its_own_direction() {
    let mut glyphs = word("upright", 0.0, 700.0);
    // A label running up the page at x = 50: its baseline frame is (y, -x).
    for (i, c) in "side".chars().enumerate() {
        glyphs.push(Placed {
            orientation: Orientation::Left,
            u: 100.0 + i as f64 * ADVANCE,
            v: -50.0,
            advance: ADVANCE,
            size: SIZE,
            text: c.to_string(),
        });
    }
    assert_eq!(assemble(&glyphs), "upright\nside");
}

#[test]
fn vertical_columns_read_right_to_left_and_top_to_bottom() {
    let column = |text: &str, x: f64| -> Vec<Placed> {
        text.chars()
            .enumerate()
            .map(|(i, c)| Placed {
                orientation: Orientation::Vertical,
                u: -(700.0 - i as f64 * SIZE),
                v: x,
                advance: SIZE,
                size: SIZE,
                text: c.to_string(),
            })
            .collect()
    };
    let mut glyphs = column("二行", 100.0);
    glyphs.extend(column("一行", 120.0));
    assert_eq!(assemble(&glyphs), "一行\n二行");
}

#[test]
fn nothing_assembles_to_nothing() {
    assert_eq!(assemble(&[]), "");
}

/// No mapping is held against a page; an icon font's private code point or a
/// control character is an ornament, held against nothing (Plan 0.1).
#[test]
fn a_mapping_is_text_unmapped_or_an_ornament() {
    assert_eq!(mapping("a"), Mapping::Text);
    assert_eq!(mapping("fi"), Mapping::Text);
    assert_eq!(mapping("中"), Mapping::Text);
    assert_eq!(mapping(" "), Mapping::Text, "a drawn space is a word break");
    assert_eq!(mapping(""), Mapping::Unmapped);
    assert_eq!(mapping("\u{FFFD}"), Mapping::Unmapped);
    assert_eq!(mapping("\u{0}"), Mapping::Ornament);
    assert_eq!(
        mapping("\u{F041}"),
        Mapping::Ornament,
        "a symbol font's private code point"
    );
    assert_eq!(mapping("a\u{F041}"), Mapping::Text);
}

/// The collector's own arithmetic, without a PDF: a glyph's transform places
/// it, its width spaces it, and an unmapped glyph is counted and dropped.
#[test]
fn the_collector_places_counts_and_measures() {
    let mut collector = Collector::new(612.0 * 792.0);
    // 12-point text: glyph space (1000 units/em) scaled to 12 page units.
    let at = |x: f64| Affine::translate((x, 700.0)) * Affine::scale(12.0 / UNITS_PER_EM);
    let mut x = 72.0;
    for c in ['H', 'i'] {
        collector.place(
            at(x),
            Some(500.0),
            WritingMode::Horizontal,
            Some(BfString::Char(c)),
        );
        x += 6.0;
    }
    // A word gap of a third of an em.
    x += 4.0;
    collector.place(
        at(x),
        Some(500.0),
        WritingMode::Horizontal,
        Some(BfString::String("fi".into())),
    );
    collector.place(at(x + 6.0), None, WritingMode::Horizontal, None);
    collector.place(
        at(x + 12.0),
        Some(500.0),
        WritingMode::Horizontal,
        Some(BfString::Char('\u{FFFD}')),
    );
    // An icon-font glyph: neither a character nor unmapped.
    collector.place(
        at(x + 18.0),
        Some(500.0),
        WritingMode::Horizontal,
        Some(BfString::Char('\u{F0B7}')),
    );
    // A full-page picture's transform maps the unit square onto the page.
    let picture = Affine::scale_non_uniform(612.0, 792.0);
    collector.picture_area += picture.determinant().abs();
    let layer = collector.finish();
    assert_eq!(layer.text, "Hi fi");
    assert_eq!(layer.chars, 4);
    assert_eq!(layer.unmapped, 2);
    assert!((layer.picture_share - 1.0).abs() < 1e-9);
}

/// A glyph turned a quarter to the left is placed in its own frame.
#[test]
fn the_collector_finds_a_glyphs_orientation_from_its_transform() {
    let mut collector = Collector::new(1.0);
    let turned = Affine::translate((50.0, 100.0))
        * Affine::rotate(std::f64::consts::FRAC_PI_2)
        * Affine::scale(10.0 / UNITS_PER_EM);
    collector.place(
        turned,
        Some(500.0),
        WritingMode::Horizontal,
        Some(BfString::Char('x')),
    );
    let placed = &collector.glyphs[0];
    assert_eq!(placed.orientation, Orientation::Left);
    assert!((placed.u - 100.0).abs() < 1e-9 && (placed.v + 50.0).abs() < 1e-9);
    assert!((placed.size - 10.0).abs() < 1e-9 && (placed.advance - 5.0).abs() < 1e-9);
}
