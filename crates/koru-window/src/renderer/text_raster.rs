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

/// Apply once to a freshly composed, physical-pixel window scene, immediately
/// before rendering. Never apply to reusable logical-pixel scene fragments.
///
/// Vello folds appended scenes' transforms into each glyph run, so this is the
/// point at which 0.5 means half a device pixel at every DPI and document zoom.
/// Move only glyphs: borders, images, layout, wrapping and hit targets stay put.
/// The native frame loops rebuild this scene before every call; shifting cached
/// fragments instead would accumulate the phase across redraws.
pub fn prepare_window_text(scene: &mut vello::Scene) {
    prepare(scene, koru_draw::text_rendering::current());
}

fn prepare(scene: &mut vello::Scene, settings: koru_settings::TextRendering) {
    let encoding = scene.encoding_mut();
    for run in &mut encoding.resources.glyph_runs {
        run.transform.translation[0] += settings.offset;
        run.transform.translation[1] += settings.offset;
        run.hint = settings.hinting;
    }
    encoding.set_glyph_coverage(settings.gamma, settings.cutoff);
}

#[cfg(test)]
mod tests;
