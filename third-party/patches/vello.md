# Vello 0.8: per-glyph coverage

The three patches (`vello`, `vello_encoding`, `vello_shaders`) travel together.
They retain version 0.8.0 and add a solid glyph draw command carrying coverage
gamma and cutoff. Normal shapes keep their original draw command and cost.

`DrawGlyphs` records a neutral glyph brush (gamma 1, cutoff 0). On a freshly
composed native-window scene, Koru calls `Encoding::set_glyph_coverage` using
each glyph run's existing stream offsets. Appending scenes already relocates
those offsets, so cached fragments remain neutral and no stream is rebuilt.

The GPU fine shader adjusts geometric coverage **before** premultiplied brush
opacity and source-over compositing. The cutoff compares the original coverage;
gamma is `coverage.powf(gamma)`. Fully covered interiors and zero coverage avoid
the power operation. CPU coarse/draw-leaf/fine paths understand the same command.
The command adds two words per solid glyph run, and two per affected tile command.

This is deliberately not the reference editor's transparent-page postprocess:
that experiment assumes a white background and also filters other transparent
marks. The production path works over arbitrary backgrounds, preserves text
opacity and clip/paint order, and does not filter shapes, images or clip masks.
Color emoji and non-solid glyph brushes retain upstream behavior.

The Vello and encoding patches also remove obsolete `single_use_lifetimes`
lint expectations that produce warnings on the current Rust toolchain.

Regression coverage lives beside `koru-window::renderer::text_raster`: scene
append/DPI/caching checks and a GPU pixel test comparing shapes, partial glyph
coverage, solid interiors and brush opacity over both black and white. The
settings crate tests range rejection, old sections and preference round trips.

Remove these patches when upstream exposes a glyph-coverage transfer with the
same semantics. A version bump must verify draw-tag bit layouts, glyph stream
offsets, GPU and CPU command decoding, all three antialiasing variants, and
neutral export behavior. Never patch the Cargo registry cache.
