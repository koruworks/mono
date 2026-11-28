// SPDX-License-Identifier: MPL-2.0

//! The Windows text rasterization settings selected in the Parley reference.

/// Native window rendering; offscreen exports retain their own sampling policy.
pub fn window_antialiasing() -> vello::AaConfig {
    match koru_draw::text_rendering::current().antialiasing {
        koru_settings::TextAntialiasing::Area => vello::AaConfig::Area,
        koru_settings::TextAntialiasing::Msaa8 => vello::AaConfig::Msaa8,
        koru_settings::TextAntialiasing::Msaa16 => vello::AaConfig::Msaa16,
    }
}

/// `vello_encoding::DrawTag::GLYPH_COLOR.0`, defined in
/// `third-party/patches/vello_encoding.patch`.
const GLYPH_COLOR_TAG: u32 = 0x4c;

/// Apply once to a freshly composed, physical-pixel window scene, immediately
/// before rendering. Never apply to reusable logical-pixel scene fragments.
///
/// Vello folds appended scenes' transforms into each glyph run, so this is the
/// point at which 0.5 means half a device pixel at every DPI and document zoom.
/// Move only glyphs: borders, images, layout, wrapping and hit targets stay put.
/// The native frame loops rebuild this scene before every call; shifting cached
/// fragments instead would accumulate the phase across redraws.
///
/// Light ink rendered during a light theme (dark/black buttons, tooltips,
/// toasts) uses the dark-theme text rendering settings so stems are not thinned
/// away by light-on-white gamma; dark ink on a light surface in dark theme
/// mirrors the rule.
pub fn prepare_window_text(scene: &mut vello::Scene) {
    prepare_themed(
        scene,
        koru_draw::text_rendering::current_for(false),
        koru_draw::text_rendering::current_for(true),
        koru_draw::theme::is_dark(),
    );
}

#[cfg(test)]
fn prepare(scene: &mut vello::Scene, settings: koru_settings::TextRendering) {
    prepare_themed(scene, settings, settings, false);
}

fn prepare_themed(
    scene: &mut vello::Scene,
    light: koru_settings::TextRendering,
    dark: koru_settings::TextRendering,
    dark_theme: bool,
) {
    let primary = if dark_theme { dark } else { light };
    let encoding = scene.encoding_mut();
    for run in &mut encoding.resources.glyph_runs {
        let settings = if encoding
            .draw_tags
            .get(run.stream_offsets.draw_tags)
            .is_some_and(|tag| tag.0 == GLYPH_COLOR_TAG)
        {
            let offset = run.stream_offsets.draw_data;
            let selected = select_run_settings(encoding.draw_data[offset], light, dark, dark_theme);
            let gamma = if selected.gamma.is_finite() {
                selected.gamma.clamp(0.0, 3.0)
            } else {
                1.0
            };
            let cutoff = if selected.cutoff.is_finite() {
                selected.cutoff.clamp(0.0, 0.5)
            } else {
                0.0
            };
            encoding.draw_data[offset + 1] = gamma.to_bits();
            encoding.draw_data[offset + 2] = cutoff.to_bits();
            selected
        } else {
            primary
        };
        run.transform.translation[0] += settings.offset;
        run.transform.translation[1] += settings.offset;
        run.hint = settings.hinting;
    }
}

/// Unpremultiplied sRGB luminance of a packed little-endian premultiplied RGBA8
/// `DrawColor`, or `None` when fully transparent.
fn ink_srgb_luma(rgba: u32) -> Option<f32> {
    let [r, g, b, a] = rgba.to_le_bytes();
    if a == 0 {
        return None;
    }
    let inv_a = 1.0 / f32::from(a);
    let r = (f32::from(r) * inv_a).clamp(0.0, 1.0);
    let g = (f32::from(g) * inv_a).clamp(0.0, 1.0);
    let b = (f32::from(b) * inv_a).clamp(0.0, 1.0);
    Some(0.2126 * r + 0.7152 * g + 0.0722 * b)
}

/// Choose light- or dark-theme raster settings for one solid glyph run.
fn select_run_settings(
    rgba: u32,
    light: koru_settings::TextRendering,
    dark: koru_settings::TextRendering,
    dark_theme: bool,
) -> koru_settings::TextRendering {
    let Some(luma) = ink_srgb_luma(rgba) else {
        return if dark_theme { dark } else { light };
    };
    if !dark_theme && luma >= 0.80 {
        dark
    } else if dark_theme && luma <= 0.25 {
        light
    } else if dark_theme {
        dark
    } else {
        light
    }
}

#[cfg(test)]
mod tests;
