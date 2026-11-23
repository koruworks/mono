# Font test

Run `koru.bat font-test` from the repository root. It builds and opens the
standalone comparison editor; it does not open a Koru profile or store.
Arguments after the command go to the editor, for example:

```bat
koru.bat font-test --font=Inter --coverage-gamma=1.75 --coverage-cutoff=0.05
```

The sample uses Quiet.md's Feishu theme metrics, initially with Segoe UI.
Use **F3/F4** for Koru fonts, **F5** for heading weight, **F6** for hinting,
**F7** for antialiasing, **F8** for physical offset, **F9** for coverage gamma,
**F10** for cutoff, and **F11** to reset coverage. The title shows each value.
`--font-report` reports the resolved faces. `--fonts-dir=PATH` overrides the
default repository `assets/fonts` directory.

**Ctrl + wheel** or **Ctrl + plus/minus** magnifies the rendered pixels from
1× to 32× without filtering or laying the text out again. Drag to pan while
magnified. **Ctrl + 0** or **Escape** returns to editing at 1×.

This is the Parley v0.11.1 `vello_editor` comparison, with our sample, font
choices and pixel inspector. It uses the monorepo's Parley, Vello and winit
dependencies. Its experimental coverage pass operates on the sample texture;
Koru's application renderer instead adjusts glyph coverage before compositing.
Identical numbers therefore need visual comparison, especially in dark themes.

Upstream source is fetched from commit
`eea3503dd6cf17130cbb07348e0ff2c918300e94` through
[`fetch.manifest`](../../third-party/fetch.manifest), never vendored.
`build.rs` copies those three files into Cargo's output directory and applies
[`parley-font-test.patch`](../../third-party/patches/parley-font-test.patch).
The new sample and inspector modules live in `src/`. Upstream portions and
their patch retain Parley's **Apache-2.0 OR MIT** license and copyright; the
new Koru modules are MPL-2.0. License texts are already included in
[`licensing`](../../licensing/README.md).
