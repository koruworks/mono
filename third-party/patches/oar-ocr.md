# Koru patch: oar-ocr uses ZIP 9

`oar-ocr` 0.10.2 (Apache-2.0) is fetched from crates.io, checked against
[`../fetch.manifest`](../fetch.manifest), and patched with
[`oar-ocr.patch`](oar-ocr.patch). The workspace substitutes this source through
`[patch.crates-io]`; the upstream source itself is not committed.

The patch changes only the ZIP dependency requirement from 8.6 to 9.0 in both
upstream manifests. ZIP 8.6 triggers RUSTSEC-2026-0367, -0368, -0369 and -0429.
The maintainer approved this remedy during OCR integration on 2026-11-15.
No advisory is ignored and no recognition behavior changes.

The only upstream ZIP use is the Indra language-model `.npz` loader, which
Koru's PaddleOCR pipeline does not call. Its named-entry reader compiles with
ZIP 9; the existing `default-features = false` and `deflate-flate2` selection
are preserved. Remove this patch when an upstream release adopts a fixed ZIP
dependency, after checking recognition and the advisory gate again.
