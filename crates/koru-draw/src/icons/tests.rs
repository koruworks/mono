// SPDX-License-Identifier: MPL-2.0

use super::*;

fn fontless_engine() -> crate::text::TextEngine {
    crate::text::TextEngine {
        font_cx: parley::FontContext {
            collection: parley::fontique::Collection::new(parley::fontique::CollectionOptions {
                shared: false,
                system_fonts: false,
            }),
            source_cache: Default::default(),
        },
        layout_cx: Default::default(),
        pills: Default::default(),
        attr_chips: Default::default(),
    }
}

#[test]
fn declarations_catalog_and_aliases_cover_the_same_identities() {
    use std::collections::BTreeSet;
    let declarations: BTreeSet<_> = include_str!("../icons.rs")
        .lines()
        .filter_map(|line| line.strip_prefix("pub const "))
        .filter_map(|line| line.split_once(": char =").map(|(name, _)| name))
        .collect();
    let named: BTreeSet<_> = ALL
        .iter()
        .chain(ALIASES.iter())
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(declarations, named);
    assert_eq!(data::ICON_DEFS.len(), ALL.len());
    assert!(
        data::ICON_DEFS
            .windows(2)
            .all(|w| w[0].codepoint < w[1].codepoint)
    );
    for (name, icon) in ALL {
        assert_eq!(vector_def(icon).expect(name).name, name);
    }
    for (_, alias) in ALIASES {
        assert!(has_vector_icon(alias));
    }
}

#[test]
fn inline_identity_roundtrips_both_states_without_stealing_other_boxes() {
    for (_, icon) in ALL {
        for filled in [false, true] {
            assert_eq!(
                unpack_box_id(pack_box_id(icon, filled)),
                Some((icon, filled))
            );
        }
    }
    for id in [
        0,
        1,
        u64::MAX,
        vector::ICON_BOX_TAG,
        vector::ICON_BOX_TAG | 0xFFFF,
    ] {
        assert_eq!(unpack_box_id(id), None);
    }
    for icon in ['A', '\u{1E000}', '\u{10FFFF}'] {
        assert_eq!(unpack_box_id(pack_box_id(icon, false)), None);
    }
}

#[test]
fn every_icon_parses_and_paints_without_any_fonts_in_both_states() {
    use parley::layout::PositionedLayoutItem;
    use vello::kurbo::{Affine, BezPath, Shape};
    let mut engine = fontless_engine();
    for def in &data::ICON_DEFS {
        assert!(
            !def.stroke_d.is_empty() || !def.fill_d.is_empty(),
            "{}",
            def.name
        );
        for d in [Some(def.stroke_d), Some(def.fill_d), def.active_fill_d]
            .into_iter()
            .flatten()
            .filter(|d| !d.is_empty())
        {
            let path = BezPath::from_svg(d).expect(def.name);
            assert!(!path.elements().is_empty(), "{}", def.name);
            let bounds = path.bounding_box();
            for coordinate in [bounds.x0, bounds.y0, bounds.x1, bounds.y1] {
                assert!(
                    coordinate.is_finite() && (-3.0..=27.0).contains(&coordinate),
                    "{} lies outside the 24-unit icon grid: {bounds:?}",
                    def.name
                );
            }
        }
        for filled in [false, true] {
            let layout = if filled {
                engine.icon_layout_filled(def.codepoint, 20.0)
            } else {
                engine.icon_layout(def.codepoint, 20.0)
            };
            let boxes: Vec<_> = layout.lines().flat_map(|l| l.items()).collect();
            assert_eq!(boxes.len(), 1, "{}", def.name);
            let PositionedLayoutItem::InlineBox(b) = &boxes[0] else {
                panic!("icon became text")
            };
            assert_eq!(unpack_box_id(b.id), Some((def.codepoint, filled)));
            assert_eq!((b.width, b.height), (20.0, 20.0));
            assert_eq!(layout.width(), 20.0);
            let mut scene = vello::Scene::new();
            crate::scene::draw_layout(&mut scene, Affine::IDENTITY, &layout, 0.0, 0.0, None);
            assert!(!scene.encoding().path_tags.is_empty(), "{}", def.name);
        }
    }
}

#[test]
fn icon_slot_centering_and_inspection_follow_the_root_transform() {
    use vello::kurbo::{Affine, Rect};
    let mut engine = fontless_engine();
    let layout = engine.icon_layout(PLUS, 20.0);
    let slot = crate::geometry::Rect::new(10.0, 30.0, 60.0, 40.0);
    let (x, y) = crate::scene::centre_in(&layout, slot);
    let root = Affine::translate((70.0, 60.0)) * Affine::rotate(0.3) * Affine::scale(1.5);
    let frame =
        crate::inspect::Frame::begin(1, Affine::IDENTITY, Rect::new(-100.0, -100.0, 500.0, 500.0));
    crate::scene::draw_layout(&mut vello::Scene::new(), root, &layout, x, y, None);
    let snapshot = frame.finish();
    let icon = snapshot.records.iter().find(|r| r.name == "icon").unwrap();
    let expected = root.transform_rect_bbox(Rect::new(30.0, 40.0, 50.0, 60.0));
    for (actual, expected) in
        icon.bounds
            .into_iter()
            .zip([expected.x0, expected.y0, expected.x1, expected.y1])
    {
        assert!((f64::from(actual) - expected).abs() < 0.001);
    }
    assert!(
        icon.properties
            .iter()
            .any(|p| p.name == "icon pack" && p.value == ACTIVE_PACK)
    );
    assert!(
        icon.properties
            .iter()
            .any(|p| p.name == "icon source" && p.value == vector_def(PLUS).unwrap().source)
    );
    assert!(!icon.properties.iter().any(|p| p.name == "rendered family"));
}

#[test]
fn unknown_inline_boxes_are_not_painted_as_icons() {
    use parley::{InlineBox, InlineBoxKind};
    let mut engine = fontless_engine();
    let mut builder = engine
        .layout_cx
        .ranged_builder(&mut engine.font_cx, "", 1.0, true);
    builder.push_inline_box(InlineBox {
        id: 42,
        kind: InlineBoxKind::InFlow,
        index: 0,
        width: 20.0,
        height: 20.0,
    });
    let mut layout = builder.build("");
    layout.break_all_lines(None);
    let mut scene = vello::Scene::new();
    crate::scene::draw_layout(
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &layout,
        0.0,
        0.0,
        None,
    );
    assert!(scene.encoding().path_tags.is_empty());
}

#[test]
fn warmed_catalog_lookup_benchmark() {
    let start = std::time::Instant::now();
    for _ in 0..1000 {
        for (_, icon) in ALL {
            std::hint::black_box(vector_def(std::hint::black_box(icon)).unwrap());
        }
    }
    eprintln!(
        "{} warmed icon lookups: {:?}",
        ALL.len() * 1000,
        start.elapsed()
    );
}

/// Every glyph sits in the Private Use Area. One that did not would collide
/// with real text — an icon constant holding `A` would render as an `A`
/// wherever Phosphor happened not to be registered.
#[test]
fn every_icon_is_in_the_private_use_area() {
    for (name, ch) in ALL {
        let cp = ch as u32;
        assert!(
            (0xE000..=0xF8FF).contains(&cp),
            "{name} is U+{cp:04X}, outside the PUA"
        );
    }
}

/// Two icons sharing a codepoint would draw the same glyph, which is the
/// sort of thing nobody notices until a toolbar has two identical buttons.
#[test]
fn no_two_icons_share_a_codepoint() {
    let mut seen: Vec<char> = ALL.iter().map(|(_, c)| *c).collect();
    seen.sort_unstable();
    let before = seen.len();
    seen.dedup();
    assert_eq!(seen.len(), before, "duplicate codepoint among the icons");
}

/// Each scheme a link can carry maps to a distinct, recognisable glyph, and
/// anything unrecognised falls back to the generic link.
#[test]
fn for_scheme_picks_the_right_icon() {
    assert_eq!(for_scheme(Some("https://example.com")), LINK);
    assert_eq!(for_scheme(Some("HTTPS://x")), LINK, "case-insensitive");
    assert_eq!(for_scheme(Some("http://example.com")), LINK_SIMPLE_BREAK);
    assert_eq!(for_scheme(Some("mailto:a@b.com")), ENVELOPE);
    assert_eq!(for_scheme(Some("file:///etc/hosts")), FILE);
    assert_eq!(for_scheme(Some("ftp://host/path")), FILE_ARROW_UP);
    assert_eq!(for_scheme(Some("sftp://host/path")), KEY);
    assert_eq!(for_scheme(Some("ssh://host")), KEY);
    assert_eq!(for_scheme(Some("tel:+155512345")), PHONE);
    assert_eq!(for_scheme(Some("git://host/repo")), GITHUB);
    assert_eq!(
        for_scheme(Some("git+ssh://host/repo")),
        GITHUB,
        "git+ssh is git"
    );
    assert_eq!(for_scheme(Some("spotify:track:1")), LINK, "unknown scheme");
    assert_eq!(for_scheme(None), LINK, "no url at all");
}
