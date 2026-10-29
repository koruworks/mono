// SPDX-License-Identifier: MPL-2.0

//! **The gate**: ELEMENTS.final §5.3's four fixtures, which decide whether
//! the in-house solver is enough before anything is built on it.
//!
//! 1. The pilot workspace row matches the Workspaces page's P0 baseline at
//!    both widths, with one trailing-lane allocation in every state.
//! 2. Capped weighted fills redistribute; a deficit reports overflow and
//!    keeps every minimum.
//! 3. Wrapped text follows its column's width and a persistent weight
//!    change; the two-line profile row keeps its 44-unit minimum.
//! 4. A scroll viewport measures unbounded content, clamps its offset, and
//!    rejects a Fill it cannot honour.
//!
//! Each runs under every registered theme: layout must not move with the
//! theme, and paint must take each theme's colours.

use super::*;
use crate::geometry::Rect;
use crate::inspect;
use crate::theme::{metrics, roles, themes};

/// A fixed-size leaf: a tile, a lane.
struct Fixed(f32, f32);

impl Leaf for Fixed {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.0, self.1)
    }
}

/// The P1 fixtures' stand-ins carry no meaning of their own.
fn leaf(name: &'static str, adapter: Fixed) -> El {
    super::leaf(name, adapter).decorative()
}

/// Every theme × contrast this build registers.
fn contexts() -> Vec<FrameCx> {
    themes::registered()
        .iter()
        .flat_map(|theme| Contrast::ALL.map(|contrast| FrameCx::new(theme, contrast)))
        .collect()
}

fn lay(tree: &El, area: Rect, cx: FrameCx) -> Frame {
    let mut engine = TextEngine::new();
    let mut cache = TextCache::default();
    tree.layout(&mut engine, &mut cache, area, cx)
}

fn near(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() < 0.01
        && (a.y - b.y).abs() < 0.01
        && (a.w - b.w).abs() < 0.01
        && (a.h - b.h).abs() < 0.01
}

#[track_caller]
fn assert_rect(frame: &Frame, name: &str, want: Rect) {
    let got = frame.rect_of(name).unwrap_or_else(|| panic!("no {name}"));
    assert!(near(got, want), "{name}: {got:?}, want {want:?}");
}

// ------------------------------------------------- 1. the pilot workspace row

/// The workspace row as the page draws it today: a neutral row with the lit
/// plate 4 inside it, the content another 4 inside the plate; the tile and
/// name as the primary, 8 apart; the lane 4 after the name.
fn workspace_row(name: &str, current: bool) -> El {
    col("Workspaces page").child(
        row("Workspace row")
            .min_h(metrics::ROW_MIN)
            .pad_x(Space::S1)
            .child(
                row("Workspace plate")
                    .fill()
                    .pad_x(Space::S1)
                    .gap(Space::S1)
                    .bg(Surface::Hover)
                    .radius(Radius::Metric(metrics::WORKSPACE_GEAR_RADIUS))
                    .child(
                        row("Workspace primary action")
                            .fill()
                            .gap(Space::S2)
                            .child(
                                leaf(
                                    "Workspace tile",
                                    Fixed(
                                        metrics::WORKSPACE_TILE.value,
                                        metrics::WORKSPACE_TILE.value,
                                    ),
                                )
                                .align_self(Align::Centre),
                            )
                            .child(
                                text(name)
                                    .named("Workspace name")
                                    .role(roles::ROW)
                                    .ink(if current {
                                        Ink::Primary
                                    } else {
                                        Ink::Secondary
                                    })
                                    .emphasis(current)
                                    .fill()
                                    .truncate(),
                            ),
                    )
                    .child(
                        leaf("Manage gear", Fixed(metrics::TRAILING_LANE_W.value, 0.0))
                            .w(metrics::TRAILING_LANE_W),
                    ),
            ),
    )
}

/// `workspaces.txt`, "width=256 … names=normal state=rest", row ws:0002:
/// `row 54 571 258 26`, `tile 62 574 20 20`, `name 90 571 190 26`,
/// `lane 284 571 20 26`; and at width=600 the panel is 584 wide.
#[test]
fn the_workspace_row_matches_the_p0_baseline_at_both_widths() {
    for (panel_w, name_w, lane_x) in [(258.0, 190.0, 284.0), (584.0, 516.0, 610.0)] {
        for cx in contexts() {
            for current in [false, true] {
                let tree = workspace_row("Client work", current);
                let frame = lay(&tree, Rect::new(54.0, 571.0, panel_w, 26.0), cx);
                let key = cx.theme.key;
                assert_rect(
                    &frame,
                    "Workspace row",
                    Rect::new(54.0, 571.0, panel_w, 26.0),
                );
                assert_rect(&frame, "Workspace tile", Rect::new(62.0, 574.0, 20.0, 20.0));
                assert_rect(
                    &frame,
                    "Workspace name",
                    Rect::new(90.0, 571.0, name_w, 26.0),
                );
                assert_rect(&frame, "Manage gear", Rect::new(lane_x, 571.0, 20.0, 26.0));
                // The lit plate is the row inset by 4, as `draw_page` fills it.
                assert_rect(
                    &frame,
                    "Workspace plate",
                    Rect::new(58.0, 571.0, panel_w - 8.0, 26.0),
                );
                assert!(
                    frame.diagnostics.is_empty(),
                    "{key}: {:?}",
                    frame.diagnostics
                );
            }
        }
    }
}

/// The row is 26 because of its minimum, not because the area said so: in a
/// hugging column its content (a 20 tile, an 18 line) would be shorter.
#[test]
fn the_row_minimum_is_what_makes_it_26() {
    let tree = workspace_row("Example", false);
    let frame = lay(&tree, Rect::new(0.0, 0.0, 258.0, 200.0), FrameCx::root());
    assert_eq!(frame.rect_of("Workspace row").unwrap().h, 26.0);
}

/// **One lane, one allocation, in every state.** Current changes the name's
/// weight, a persistent variant measured before layout; hot and focused are
/// paint-only. None of them moves the lane, the tile or the name's box.
#[test]
fn the_trailing_lane_keeps_its_allocation_across_states() {
    let area = Rect::new(54.0, 571.0, 258.0, 26.0);
    let baseline = lay(&workspace_row("Client work", false), area, FrameCx::root());
    let current = lay(&workspace_row("Client work", true), area, FrameCx::root());
    for name in [
        "Manage gear",
        "Workspace tile",
        "Workspace name",
        "Workspace plate",
    ] {
        assert!(
            near(
                baseline.rect_of(name).unwrap(),
                current.rect_of(name).unwrap()
            ),
            "{name} moved when the row became current"
        );
    }
}

/// A name too long for its box is elided to it; a short one is not.
#[test]
fn a_long_name_is_truncated_to_its_box() {
    let long = "Quarterly planning for the European expansion programme";
    let frame = lay(
        &workspace_row(long, false),
        Rect::new(0.0, 0.0, 258.0, 26.0),
        FrameCx::root(),
    );
    let node = frame.named("Workspace name").next().unwrap();
    let layout = node.text.as_ref().unwrap();
    assert!(layout.width() <= node.rect.w + 0.01, "{}", layout.width());
    assert_eq!(node.lines(), Some(1));
}

// ---------------------------------------------- 2. capped fills and deficits

fn fills(weights: &[(f32, Option<f32>, Option<f32>)]) -> El {
    row("Fills").children(weights.iter().map(|&(weight, min, max)| {
        let mut el = spacer().named("Fill").fill_weight(weight);
        if let Some(min) = min {
            el = el.min_w(Length::Px(min));
        }
        if let Some(max) = max {
            el = el.max_w(Length::Px(max));
        }
        el
    }))
}

fn widths(frame: &Frame) -> Vec<f32> {
    frame.named("Fill").map(|n| n.rect.w).collect()
}

/// §5.1's two worked examples, weights, and redistribution past a cap.
#[test]
fn weighted_fills_share_what_is_left_and_respect_their_caps() {
    let area = Rect::new(0.0, 0.0, 100.0, 10.0);
    for cx in contexts() {
        let cases: [(&[(f32, Option<f32>, Option<f32>)], &[f32]); 4] = [
            (&[(1.0, None, Some(20.0)), (1.0, None, None)], &[20.0, 80.0]),
            (
                &[(1.0, Some(10.0), None), (1.0, Some(30.0), None)],
                &[40.0, 60.0],
            ),
            (&[(1.0, None, None), (3.0, None, None)], &[25.0, 75.0]),
            (
                &[
                    (1.0, None, Some(10.0)),
                    (1.0, None, None),
                    (1.0, None, None),
                ],
                &[10.0, 45.0, 45.0],
            ),
        ];
        for (spec, want) in cases {
            let frame = lay(&fills(spec), area, cx);
            let got = widths(&frame);
            assert_eq!(got.len(), want.len());
            for (g, w) in got.iter().zip(want) {
                assert!((g - w).abs() < 0.01, "{spec:?}: {got:?}, want {want:?}");
            }
            assert!(!frame.nodes[0].overflow);
        }
    }
}

/// **No implicit shrinking.** Fixed children and a Fill's minimum that do
/// not fit keep their sizes, the stack says it overflowed, and nothing goes
/// negative.
#[test]
fn a_deficit_reports_overflow_and_keeps_every_minimum() {
    let tree = row("Deficit")
        .gap(Space::S2)
        .child(spacer().named("A").w(Length::Px(60.0)))
        .child(spacer().named("B").w(Length::Px(50.0)))
        .child(spacer().named("C").fill().min_w(Length::Px(10.0)));
    for cx in contexts() {
        let frame = lay(&tree, Rect::new(0.0, 0.0, 100.0, 10.0), cx);
        assert!(frame.nodes[0].overflow, "the row reports its overflow");
        assert_eq!(frame.rect_of("A").unwrap().w, 60.0);
        assert_eq!(frame.rect_of("B").unwrap().w, 50.0);
        assert_eq!(frame.rect_of("C").unwrap().w, 10.0, "the minimum holds");
        assert!(
            frame
                .nodes
                .iter()
                .all(|n| n.rect.w >= 0.0 && n.rect.h >= 0.0)
        );
        // Start-anchored: the overflow runs off the end, in order.
        assert_eq!(frame.rect_of("C").unwrap().x, 60.0 + 8.0 + 50.0 + 8.0);
    }
}

/// Cross-axis: Stretch by default, Start/Centre/End at natural size, an
/// explicit size wins over Stretch and sits at the start.
#[test]
fn cross_alignment_follows_the_defaults() {
    let tree = row("Row")
        .h(Length::Px(40.0))
        .child(leaf("Stretched", Fixed(10.0, 10.0)))
        .child(leaf("Centred", Fixed(10.0, 10.0)).align_self(Align::Centre))
        .child(leaf("Ended", Fixed(10.0, 10.0)).align_self(Align::End))
        .child(leaf("Explicit", Fixed(10.0, 10.0)).h(Length::Px(16.0)));
    let frame = lay(&tree, Rect::new(0.0, 0.0, 200.0, 40.0), FrameCx::root());
    assert_rect(&frame, "Stretched", Rect::new(0.0, 0.0, 10.0, 40.0));
    assert_rect(&frame, "Centred", Rect::new(10.0, 15.0, 10.0, 10.0));
    assert_rect(&frame, "Ended", Rect::new(20.0, 30.0, 10.0, 10.0));
    assert_rect(&frame, "Explicit", Rect::new(30.0, 0.0, 10.0, 16.0));
}

// --------------------------------- 3. wrapping, weight, the two-line profile row

fn paragraph(content: &str, emphasis: bool) -> El {
    col("Column").child(
        text(content)
            .named("Paragraph")
            .role(roles::ROW)
            .emphasis(emphasis)
            .wrap(),
    )
}

/// Wrapped text breaks at its column's width, and its height is its lines.
#[test]
fn wrapped_text_follows_its_column_width() {
    let content = "Every profile on this machine, one slide to the right, with its own workspaces.";
    let line = roles::ROW.style().metrics.line_height;
    for cx in contexts() {
        let mut previous = 0;
        for width in [600.0, 300.0, 160.0, 90.0] {
            let frame = lay(
                &paragraph(content, false),
                Rect::new(0.0, 0.0, width, 400.0),
                cx,
            );
            let node = frame.named("Paragraph").next().unwrap();
            let lines = node.lines().unwrap();
            assert!(lines >= previous, "narrower never means fewer lines");
            assert!(
                (node.rect.h - lines as f32 * line).abs() < 0.5,
                "at {width}: {lines} lines in {}",
                node.rect.h
            );
            previous = lines;
        }
        assert!(previous > 1, "it wraps at 90");
    }
}

/// A persistent weight change is measured, not painted over: SemiBold is
/// wider, so at a fixed width it can need more lines, and its layout is a
/// different one.
#[test]
fn a_persistent_weight_change_is_measured() {
    let content = "Client work for Müller & Söhne GmbH";
    let mut engine = TextEngine::new();
    let mut cache = TextCache::default();
    let cx = FrameCx::root();
    let natural = |emphasis: bool, engine: &mut TextEngine, cache: &mut TextCache| {
        let tree = row("Row").child(
            text(content)
                .named("Name")
                .role(roles::ROW)
                .emphasis(emphasis)
                .hug_w(),
        );
        let frame = tree.layout(engine, cache, Rect::new(0.0, 0.0, 600.0, 26.0), cx);
        frame.rect_of("Name").unwrap().w
    };
    let regular = natural(false, &mut engine, &mut cache);
    let misses = cache.misses();
    let bold = natural(true, &mut engine, &mut cache);
    assert!(
        bold > regular,
        "SemiBold {bold} is wider than Medium {regular}"
    );
    assert!(cache.misses() > misses, "the weight is part of the shape");
}

/// The Profiles page's two-line row: a 24 tile centred in 44, the name over
/// its detail as one centred block, the lane at the end. The block is the
/// two line heights (18 + 14 = 32) centred in 44.
fn profile_row(name: &str, detail: &str) -> El {
    col("Profiles page").child(
        row("Profile row")
            .min_h(metrics::PROFILE_ROW_MIN)
            .pad_x(Space::S2)
            .gap(Space::S1)
            .child(
                row("Profile primary action")
                    .fill()
                    .gap(Space::S2)
                    .items_center()
                    .child(leaf(
                        "Profile tile",
                        Fixed(metrics::PROFILE_TILE.value, metrics::PROFILE_TILE.value),
                    ))
                    .child(
                        col("Profile words")
                            .fill()
                            .child(text(name).named("Profile name").role(roles::ROW).truncate())
                            .child(
                                text(detail)
                                    .named("Profile detail")
                                    .role(roles::HINT)
                                    .ink(Ink::Secondary)
                                    .truncate(),
                            ),
                    ),
            )
            .child(
                leaf("Manage gear", Fixed(metrics::TRAILING_LANE_W.value, 0.0))
                    .w(metrics::TRAILING_LANE_W),
            ),
    )
}

/// `profiles.txt`, "width=256 … names=normal state=rest", row profile:0102
/// at y 556: tile at +8/+10, 24 square; lane `282 556 20 44`; the detail's
/// box starts 24 below the row's top. Today's name box is 16 high inside
/// that block with its 18-unit line overflowing it by one either side; the
/// element block gives the name its own 18, one unit lower — a difference
/// P5 settles, recorded in ELEMENTS-RESULTS.md.
#[test]
fn the_two_line_profile_row_keeps_its_minimum_and_centres_its_block() {
    for cx in contexts() {
        let frame = lay(
            &profile_row("dev-data", "On this computer"),
            Rect::new(52.0, 556.0, 258.0, 300.0),
            cx,
        );
        assert_rect(&frame, "Profile row", Rect::new(52.0, 556.0, 258.0, 44.0));
        assert_rect(&frame, "Profile tile", Rect::new(60.0, 566.0, 24.0, 24.0));
        assert_rect(&frame, "Manage gear", Rect::new(282.0, 556.0, 20.0, 44.0));
        let words = frame.rect_of("Profile words").unwrap();
        assert_eq!(words.h, 32.0, "two line heights");
        assert_eq!(words.y, 562.0, "centred: (44 - 32) / 2");
        assert_eq!(words.x, 92.0, "8 after the tile");
        assert_eq!(
            words.x + words.w,
            278.0,
            "4 before the lane, as `trailing.x - 4`"
        );
        assert_eq!(frame.rect_of("Profile detail").unwrap().y, 580.0);
    }
}

// ----------------------------------------------------- 4. the scroll viewport

fn list(rows: usize, offset: f32) -> El {
    col("Page").child(
        scroll_y("List")
            .h(Length::Runtime(104.0, "the page's list band"))
            .offset(offset)
            .children((0..rows).map(|_| {
                row("List row")
                    .min_h(metrics::ROW_MIN)
                    .child(text("Row").role(roles::ROW).fill().truncate())
            })),
    )
}

/// Content measures at unbounded height; the offset is clamped to it; rows
/// outside the viewport keep their rectangles (revealable) and every row is
/// clipped to the viewport.
#[test]
fn a_scroll_viewport_measures_unbounded_content_and_clamps_its_offset() {
    let area = Rect::new(54.0, 545.0, 258.0, 104.0);
    for cx in contexts() {
        for (offset, clamped) in [(-5.0, 0.0), (52.0, 52.0), (5000.0, 40.0 * 26.0 - 104.0)] {
            let frame = lay(&list(40, offset), area, cx);
            let viewport = frame.named("List").next().unwrap();
            assert_eq!(
                viewport.scroll,
                Some((40.0 * 26.0, clamped)),
                "offset {offset}"
            );
            let rows: Vec<_> = frame.named("List row").collect();
            assert_eq!(rows.len(), 40, "every row is laid out, seen or not");
            for (i, node) in rows.iter().enumerate() {
                assert_eq!(node.rect.y, 545.0 + 26.0 * i as f32 - clamped);
                assert_eq!(node.clip, Some(viewport.rect));
            }
        }
    }
}

/// Four rows in a 104 band do not scroll.
#[test]
fn a_short_list_does_not_scroll() {
    let frame = lay(
        &list(4, 30.0),
        Rect::new(0.0, 0.0, 258.0, 104.0),
        FrameCx::root(),
    );
    assert_eq!(
        frame.named("List").next().unwrap().scroll,
        Some((104.0, 0.0))
    );
}

/// **An unresolvable Fill is said, not guessed.** A vertical Fill inside
/// scrolled content has no finite height to share; the frame names the
/// element and the axis, and the row keeps its minimum.
#[test]
fn a_fill_in_unbounded_content_is_a_diagnostic() {
    let tree = scroll_y("List")
        .h(Length::Runtime(104.0, "band"))
        .child(row("Greedy").fill().min_h(metrics::ROW_MIN));
    let frame = lay(&tree, Rect::new(0.0, 0.0, 258.0, 104.0), FrameCx::root());
    assert_eq!(
        frame.diagnostics,
        vec![Diagnostic {
            element: "Greedy",
            axis: Some(Axis::Vertical),
            message: "a Fill child needs a finite height, and scrolled content has none",
        }]
    );
    assert_eq!(frame.rect_of("Greedy").unwrap().h, 26.0);
}

// ------------------------------------------------- themes and the text cache

/// The colours a paint resolved, by inspector record name.
fn painted(tree: &El, cx: FrameCx, cache: &mut TextCache) -> Vec<(String, String)> {
    let mut engine = TextEngine::new();
    let area = Rect::new(54.0, 571.0, 258.0, 26.0);
    let frame = tree.layout(&mut engine, cache, area, cx);
    let mut scene = vello::Scene::new();
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        crate::scene::rect(0.0, 0.0, 1200.0, 700.0),
    );
    frame.paint(tree, &mut scene, Affine::IDENTITY, &mut engine);
    capture
        .finish()
        .records
        .into_iter()
        .flat_map(|record| {
            record
                .properties
                .into_iter()
                .filter(|p| matches!(p.name, "color" | "foreground"))
                .map(move |p| (record.name.to_string(), p.value))
        })
        .collect()
}

/// **The contrast axis is real, and so is the cache's respect for it.** The
/// fixture theme's two tables paint the row in different colours, each the
/// table's own; and the row's text is shaped again under the other contrast
/// rather than handed back from the first.
#[test]
fn the_fixture_themes_contrasts_paint_their_own_tables_and_reshape_text() {
    let tree = workspace_row("Client work", true);
    let mut cache = TextCache::default();
    let standard = FrameCx::new(&themes::FIXTURE_THEME, Contrast::Standard);
    let high = FrameCx::new(&themes::FIXTURE_THEME, Contrast::High);

    let first = painted(&tree, standard, &mut cache);
    let shaped = cache.misses();
    let again = painted(&tree, standard, &mut cache);
    assert_eq!(first, again);
    assert_eq!(cache.misses(), shaped, "same theme and contrast: reused");

    let other = painted(&tree, high, &mut cache);
    assert!(
        cache.misses() > shaped,
        "another contrast: shaped again, not reused"
    );
    assert_ne!(first, other, "the two tables paint differently");

    let plate = |painted: &[(String, String)]| {
        painted
            .iter()
            .find(|(name, _)| name == "fill")
            .map(|(_, c)| c.clone())
            .expect("the plate's fill")
    };
    for (painted, cx) in [(&first, standard), (&other, high)] {
        assert_eq!(
            plate(painted),
            inspect::color(Surface::Hover.of(cx.palette())),
            "the plate is the {:?} table's hover",
            cx.contrast
        );
        let ink = inspect::color(Ink::Primary.of(cx.palette()));
        assert!(
            painted.iter().any(|(_, c)| *c == ink),
            "the current name is the {:?} table's primary ink",
            cx.contrast
        );
    }
}

/// Every registered theme lays the pilot row out identically: a theme is
/// colour and nothing else.
#[test]
fn no_theme_moves_anything() {
    let area = Rect::new(54.0, 571.0, 258.0, 26.0);
    let tree = workspace_row("Client work", true);
    let reference = lay(
        &tree,
        area,
        FrameCx::new(&themes::LIGHT_THEME, Contrast::Standard),
    );
    for cx in contexts() {
        let frame = lay(&tree, area, cx);
        for (a, b) in reference.nodes.iter().zip(&frame.nodes) {
            assert!(
                near(a.rect, b.rect),
                "{} moved under {}",
                a.name,
                cx.theme.key
            );
        }
    }
}
