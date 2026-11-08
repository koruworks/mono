// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn every_pack_keeps_identity_and_has_independent_cached_geometry() {
    use vello::kurbo::Shape;
    for (i, expected) in ICON_DEFS.iter().enumerate() {
        let mut addresses = Vec::new();
        for pack in IconPack::ALL {
            let def = &pack.defs()[i];
            assert_eq!(
                (def.codepoint, def.name),
                (expected.codepoint, expected.name)
            );
            let paths = parsed(pack, i);
            addresses.push(std::ptr::from_ref(paths));
            for path in [&paths.stroke, &paths.fill, &paths.active_fill]
                .into_iter()
                .flatten()
            {
                let bounds = path.bounding_box();
                assert!(
                    [bounds.x0, bounds.y0, bounds.x1, bounds.y1]
                        .into_iter()
                        .all(|v| v.is_finite() && (-3.0..=27.0).contains(&v)),
                    "{} {}",
                    pack.label(),
                    def.name
                );
            }
            for filled in [false, true] {
                let mut scene = Scene::new();
                draw_vector_icon_for(
                    pack,
                    &mut scene,
                    Affine::IDENTITY,
                    def.codepoint,
                    filled,
                    Rect::new(0.0, 0.0, 20.0, 20.0),
                    Color::WHITE,
                );
                assert!(!scene.encoding().path_tags.is_empty());
            }
        }
        addresses.sort();
        addresses.dedup();
        assert_eq!(addresses.len(), IconPack::ALL.len());
    }
}

#[test]
fn remix_preserves_its_outline_and_filled_artwork() {
    let def = vector_def_for(IconPack::Remix, crate::icons::HOUSE).unwrap();
    assert_eq!(def.source, "Remix");
    assert!(
        def.stroke_d.is_empty(),
        "Remix line icons are filled outlines"
    );
    assert!(!def.fill_d.is_empty());
    assert_ne!(def.active_fill_d.unwrap(), def.fill_d);
    assert_eq!(IconPack::parse("remix"), IconPack::Remix);
    assert_eq!(IconPack::Remix.token(), "remix");
}

#[test]
fn warmed_paths_keep_the_same_cached_allocation() {
    let addresses: Vec<_> = (0..ICON_DEFS.len())
        .map(|i| std::ptr::from_ref(parsed(IconPack::Phosphor, i)))
        .collect();
    let start = std::time::Instant::now();
    for _ in 0..1000 {
        for (i, address) in addresses.iter().enumerate() {
            assert_eq!(
                *address,
                std::ptr::from_ref(std::hint::black_box(parsed(IconPack::Phosphor, i)))
            );
        }
    }
    eprintln!(
        "{} warmed parsed-path retrievals: {:?}",
        ICON_DEFS.len() * 1000,
        start.elapsed()
    );
}
