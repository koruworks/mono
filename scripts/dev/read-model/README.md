# read-model — PP-OCRv6 small to ONNX, reproducibly

The reader's model files are **our own `paddle2onnx` conversion** of
PaddleOCR's official PP-OCRv6 small weights (OCR TECHNICAL D1). This folder is
that conversion as a script, plus the generator of the synthetic fixtures the
measurements use. Plan 2.2 of the OCR roadmap turns `convert.sh` into the
release tool's command.

## Convert

```bash
bash scripts/dev/read-model/convert.sh --clean
```

Needs Python 3.12 (`py -3.12` on Windows, `python3.12` elsewhere; override
with `PYTHON=...`), `curl`, and `sha256sum` or `shasum`. Everything lands
under `target/ocr-measure/` (override with `READ_MODEL_OUT`), which git
ignores:

| Path | What |
| --- | --- |
| `venv/` | Throwaway Python environment, installed only from `requirements.lock` (`--require-hashes --no-deps`) |
| `weights/{det,rec}/` | The official Paddle inference files, verified by size and SHA-256 |
| `models/det.onnx`, `models/rec.onnx` | The converted detector and recognizer |
| `models/rec_dict.txt` | The recognizer's 18,708 characters, one per line, from `inference.yml` |
| `models/SHA256SUMS` | Digests of the three outputs, also printed at the end |

`--clean` deletes the venv, weights and outputs first. Without it the script
reuses what is there (weights are re-verified every time).

**What it converts.** `PaddlePaddle/PP-OCRv6_small_det` and
`PaddlePaddle/PP-OCRv6_small_rec` on Hugging Face (PaddlePaddle's own
organisation), each at a pinned commit. The recognizer is the single
multilingual one (50 languages, Latin, Chinese and Japanese among them;
18,708 characters). The same files are in PaddleX's bundles on
`paddle-model-ecology.bj.bcebos.com`, with identical digests. The pins are in
`convert.sh`.

**The tools.** paddle2onnx 2.1.0, paddlepaddle 3.0.0.dev20250426 (a nightly,
from PaddlePaddle's own wheel host), onnx 1.17.0, onnxruntime 1.27.0 and
polygraphy 0.53.6 for paddle2onnx's constant folding. `requirements.lock` says
why paddlepaddle is that nightly: it is the one whose Windows build exports
what paddle2onnx 2.1.0's Windows extension imports. The lock is for Windows
x64.

**Determinism.** Two runs from `--clean` on the same machine produce
byte-identical `det.onnx`, `rec.onnx` and `rec_dict.txt`.

## Fixtures

```bash
bash scripts/dev/read-model/make-fixtures.sh --clean
```

Writes `target/ocr-measure/fixtures/` from its own venv (`venv-fixtures/`,
from `fixtures.lock`): rendered English, small-print and Chinese/Japanese
text as PNG/JPEG, a rotated photo-like sign, a photo with no text, a
screenshot, PDFs with a real text layer (standard fonts, a PDF 1.1 file, a
300-page file, a Type3 font with no Unicode mapping), a two-page scanned PDF,
a scan with an invisible OCR text layer, encrypted PDFs (RC4 and AES-256 with
a user password, AES-128 with an owner password only), truncated and damaged
PDFs, random bytes behind a PDF header, a page holding a 20,000 x 20,000
image that compresses to 380 KiB, a page tree that contains itself, and a
form that draws itself inside 200,000 nested `q` operators. `truth.json`
holds the text of every page.

The pictures are drawn with fonts every Windows install has (Arial, Microsoft
YaHei, Yu Gothic); set `FIXTURE_FONT_LATIN`, `FIXTURE_FONT_ZH` and
`FIXTURE_FONT_JA` elsewhere. Everything is seeded and repeatable except the
encrypted files, whose salt and IV pypdf draws at random.

## Measuring

The throwaway measurement binary that reads a folder with these files is not
committed; how it was built and run, and what it found, is in
`docs/features/ocr/MEASUREMENTS.md`.
