// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::sync::Arc;
use vello::{
    Glyph, Scene,
    kurbo::{Affine, Rect},
    peniko::{Blob, Color, Fill, FontData},
};

#[test]
fn coverage_changes_only_glyph_edges_on_light_and_dark_backgrounds() {
    let mut context = crate::renderer::context_with(crate::renderer::default_backends());
    let Some(id) = pollster::block_on(context.device(None)) else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let handle = &context.devices[id];
    if handle.adapter().get_info().device_type == wgpu::DeviceType::Cpu {
        eprintln!("skipping: software adapter");
        return;
    }
    let mut renderer = crate::renderer::build_renderer(&handle.device).unwrap();
    for (ink, background) in [(Color::BLACK, Color::WHITE), (Color::WHITE, Color::BLACK)] {
        let mut original = Scene::new();
        // Identical fractional rectangles: an ordinary shape on the left,
        // and the glyph-color command on the right. No fonts or disk needed.
        for x in [2.25, 18.25] {
            original.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                ink,
                None,
                &Rect::new(x, 2.25, x + 10.0, 12.25),
            );
        }
        let encoding = original.encoding_mut();
        encoding.draw_tags.pop();
        encoding.draw_data.pop();
        encoding.encode_glyph_brush(ink, 0.75);
        let data = encoding.draw_data.len() - 3;
        let mut adjusted = original.clone();
        adjusted.encoding_mut().draw_data[data + 1] = 1.75_f32.to_bits();
        adjusted.encoding_mut().draw_data[data + 2] = 0.15_f32.to_bits();
        let normal =
            crate::offscreen::raster_with(&mut renderer, handle, &original, 32, 16, background)
                .unwrap();
        let filtered =
            crate::offscreen::raster_with(&mut renderer, handle, &adjusted, 32, 16, background)
                .unwrap();
        let pixel =
            |image: &crate::offscreen::Rgba, x: usize, y: usize| image.bytes[(y * 32 + x) * 4];
        for y in 0..16 {
            for x in 0..16 {
                assert_eq!(
                    pixel(&normal, x, y),
                    pixel(&filtered, x, y),
                    "ordinary shapes must not change"
                );
            }
        }
        assert_eq!(
            pixel(&normal, 22, 7),
            pixel(&filtered, 22, 7),
            "solid glyph interiors and brush opacity must not change"
        );
        assert_ne!(
            pixel(&normal, 18, 7),
            pixel(&filtered, 18, 7),
            "partial glyph coverage must change"
        );
        assert_eq!(
            pixel(&filtered, 28, 12),
            pixel(&filtered, 31, 15),
            "coverage below cutoff must disappear into the background"
        );

        // Gamma 0.0: any positive coverage at or above cutoff snaps to full
        // solid coverage (matching the interior), while zero-coverage
        // background pixels stay untouched even when cutoff is 0.0.
        let mut zero_gamma = original.clone();
        zero_gamma.encoding_mut().draw_data[data + 1] = 0.0_f32.to_bits();
        zero_gamma.encoding_mut().draw_data[data + 2] = 0.0_f32.to_bits();
        let solid =
            crate::offscreen::raster_with(&mut renderer, handle, &zero_gamma, 32, 16, background)
                .unwrap();
        assert_eq!(
            pixel(&solid, 31, 15),
            pixel(&normal, 31, 15),
            "zero-coverage background must stay untouched at gamma 0.0"
        );
        assert_eq!(
            pixel(&solid, 18, 7),
            pixel(&normal, 22, 7),
            "partial edge coverage must snap to full interior ink at gamma 0.0"
        );
    }
}

#[test]
fn phase_is_physical_and_does_not_change_layout_or_cached_fragments() {
    // Vello inspects the sfnt header even when only encoding a run. A valid
    // empty table directory is enough here: this test never rasterizes glyphs.
    // No font loading, GPU or disk access is needed.
    let font = FontData::new(
        Blob::new(Arc::new(vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])),
        0,
    );
    let mut fragment = Scene::new();
    fragment.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::BLACK,
        None,
        &Rect::new(0.0, 0.0, 40.0, 20.0),
    );
    fragment
        .draw_glyphs(&font)
        .font_size(16.0)
        .transform(Affine::translate((10.0, 20.0)))
        .draw(
            Fill::NonZero,
            [Glyph {
                id: 1,
                x: 2.0,
                y: 14.0,
            }]
            .into_iter(),
        );
    for dpi in [1.0, 1.25, 1.5, 2.0] {
        for _ in 0..2 {
            let mut frame = Scene::new();
            frame.append(&fragment, Some(Affine::scale(dpi)));
            let paths = frame.encoding().transforms.clone();
            prepare(&mut frame, koru_settings::TextRendering::default());
            let run = &frame.encoding().resources.glyph_runs[0];
            let phase = koru_settings::TextRendering::DEFAULT.offset;
            assert_eq!(
                run.transform.translation,
                [10.0 * dpi as f32 + phase, 20.0 * dpi as f32 + phase]
            );
            assert_eq!(run.font_size, 16.0);
            let data = run.stream_offsets.draw_data;
            assert_eq!(
                f32::from_bits(frame.encoding().draw_data[data + 1]),
                koru_settings::TextRendering::DEFAULT.gamma
            );
            assert_eq!(
                f32::from_bits(frame.encoding().draw_data[data + 2]),
                koru_settings::TextRendering::DEFAULT.cutoff
            );
            assert_eq!(
                frame.encoding().draw_data[0],
                fragment.encoding().draw_data[0],
                "shape color is untouched"
            );
            assert_eq!(frame.encoding().resources.glyphs[0].x, 2.0);
            assert_eq!(
                frame.encoding().transforms,
                paths,
                "shape transforms are unchanged"
            );
            if cfg!(target_os = "windows") {
                assert!(run.hint);
            }
        }
    }
    assert_eq!(
        fragment.encoding().resources.glyph_runs[0]
            .transform
            .translation,
        [10.0, 20.0]
    );
    let data = fragment.encoding().resources.glyph_runs[0]
        .stream_offsets
        .draw_data;
    assert_eq!(f32::from_bits(fragment.encoding().draw_data[data + 1]), 1.0);
    assert_eq!(f32::from_bits(fragment.encoding().draw_data[data + 2]), 0.0);
    let mut unhinted = fragment.clone();
    prepare(
        &mut unhinted,
        koru_settings::TextRendering {
            hinting: false,
            offset: 0.0,
            gamma: 1.0,
            cutoff: 0.0,
            ..koru_settings::TextRendering::default()
        },
    );
    assert!(!unhinted.encoding().resources.glyph_runs[0].hint);
    assert_eq!(unhinted.encoding().draw_data, fragment.encoding().draw_data);
}

#[test]
fn light_ink_on_dark_surfaces_in_light_theme_uses_dark_text_rendering() {
    let font = FontData::new(
        Blob::new(Arc::new(vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])),
        0,
    );
    let light = koru_settings::TextRendering {
        hinting: true,
        antialiasing: koru_settings::TextAntialiasing::Msaa16,
        offset: 0.0,
        gamma: 1.75,
        cutoff: 0.05,
    };
    let dark = koru_settings::TextRendering {
        hinting: true,
        antialiasing: koru_settings::TextAntialiasing::Msaa16,
        offset: 0.25,
        gamma: 0.75,
        cutoff: 0.15,
    };
    let draw = |scene: &mut Scene, color: Color| {
        scene.draw_glyphs(&font).font_size(13.0).brush(color).draw(
            Fill::NonZero,
            [Glyph {
                id: 1,
                x: 0.0,
                y: 10.0,
            }]
            .into_iter(),
        );
    };

    // Light theme: ordinary text and placeholders keep light settings;
    // white button labels and faded dark-tooltip labels use dark settings.
    let mut light_scene = Scene::new();
    let lp = &koru_draw::theme::LIGHT;
    draw(&mut light_scene, lp.text);
    draw(&mut light_scene, lp.placeholder);
    draw(&mut light_scene, lp.bg); // Inverted button ("Start using Koru")
    draw(&mut light_scene, lp.on_accent); // Primary accent button
    draw(&mut light_scene, lp.bubble_fg.multiply_alpha(0.5)); // Fading tooltip
    prepare_themed(&mut light_scene, light, dark, false);

    let read = |scene: &Scene, index: usize| {
        let run = &scene.encoding().resources.glyph_runs[index];
        let off = run.stream_offsets.draw_data;
        (
            f32::from_bits(scene.encoding().draw_data[off + 1]),
            f32::from_bits(scene.encoding().draw_data[off + 2]),
            run.transform.translation[0],
        )
    };
    assert_eq!(
        read(&light_scene, 0),
        (light.gamma, light.cutoff, light.offset)
    );
    assert_eq!(
        read(&light_scene, 1),
        (light.gamma, light.cutoff, light.offset)
    );
    assert_eq!(
        read(&light_scene, 2),
        (dark.gamma, dark.cutoff, dark.offset)
    );
    assert_eq!(
        read(&light_scene, 3),
        (dark.gamma, dark.cutoff, dark.offset)
    );
    assert_eq!(
        read(&light_scene, 4),
        (dark.gamma, dark.cutoff, dark.offset)
    );

    // Dark theme mirror: ordinary text and dim text keep dark settings;
    // dark text on light inverted/accent buttons uses light settings.
    let mut dark_scene = Scene::new();
    let dp = &koru_draw::theme::DARK;
    draw(&mut dark_scene, dp.text);
    draw(&mut dark_scene, dp.placeholder);
    draw(&mut dark_scene, dp.bg); // White inverted button in dark theme
    draw(&mut dark_scene, dp.on_accent); // Light accent button in dark theme
    prepare_themed(&mut dark_scene, light, dark, true);

    assert_eq!(read(&dark_scene, 0), (dark.gamma, dark.cutoff, dark.offset));
    assert_eq!(read(&dark_scene, 1), (dark.gamma, dark.cutoff, dark.offset));
    assert_eq!(
        read(&dark_scene, 2),
        (light.gamma, light.cutoff, light.offset)
    );
    assert_eq!(
        read(&dark_scene, 3),
        (light.gamma, light.cutoff, light.offset)
    );
}
