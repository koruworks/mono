// SPDX-License-Identifier: MPL-2.0

//! **The reader's release table: what is converted, what the conversion must
//! produce, and what is published.**
//!
//! Voice's table (`voice/table.rs`) pins an upstream archive and the zip built
//! from it. The reader's model has one more link, and the table pins every
//! one of them, so a changed byte anywhere is a refusal at the step that
//! produced it rather than a different model in somebody's cache:
//!
//! | Link | Pinned here as | Checked by |
//! | --- | --- | --- |
//! | PaddlePaddle's official inference files, at a commit | [`WeightFile`] (size, SHA-256) | `read-model convert`, before converting |
//! | the tools that convert them | `crates/koru-release/read-model/requirements.lock` (version and SHA-256 per wheel) | `pip install --require-hashes` |
//! | the three files the conversion writes | [`ConvertedFile`] (size, SHA-256) | `convert` after converting; `package` before zipping |
//! | the zip that is published | [`ReadModel::archive_size`], [`ReadModel::archive_sha256`] | `package`, and `publish` without any file at all |
//!
//! **Why the archive's size is pinned, when voice's is not.** Voice can
//! rebuild its zip on any machine from a public download, so its size is
//! measured from the zip on disk and one pinned number is enough. The reader's
//! zip can only be rebuilt by running the conversion, which runs on one kind of
//! machine (Windows x64, see `convert.rs`) and is not what a nightly does. A
//! nightly therefore has to name the archive's key *and* its length from this
//! table alone, ask the origin, and find it already there.
//!
//! **An empty digest means unpinned**, as in voice's table: `convert` then
//! prints the measured row to paste, and `package` and `publish` refuse.
//!
//! ## When this table changes
//!
//! Only when the model does: a new upstream commit, a new converter in the
//! lock, or a change to what goes into the zip. Then `version` is raised, the
//! conversion is run (`read-model convert`, twice, from `--clean`), and the
//! measured digests are pasted here. A nightly never converts.

/// One file of PaddlePaddle's official inference format, at a pinned commit of
/// its Hugging Face repository.
#[derive(Copy, Clone, Debug)]
pub struct WeightFile {
    /// Which of the two models it belongs to, and the directory it is saved
    /// under in the work directory: `det` or `rec`.
    pub part: &'static str,
    /// `PaddlePaddle/PP-OCRv6_small_det` — PaddlePaddle's own organisation.
    pub repository: &'static str,
    /// The full commit, so the URL names immutable bytes.
    pub commit: &'static str,
    pub file: &'static str,
    pub size: u64,
    /// Ours: the `.pdiparams` digests are the LFS object ids Hugging Face
    /// publishes; the `.json` and `.yml` digests were taken by downloading
    /// them at the same commit (`docs/features/ocr/MEASUREMENTS.md` §1).
    pub sha256: &'static str,
}

impl WeightFile {
    /// The download URL, at the pinned commit.
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repository, self.commit, self.file
        )
    }
}

/// One file the conversion writes, and therefore one file inside the zip.
#[derive(Copy, Clone, Debug)]
pub struct ConvertedFile {
    /// The name in the work directory's `models/` and inside the zip.
    pub name: &'static str,
    pub size: u64,
    /// Empty means unpinned; see the module note.
    pub sha256: &'static str,
}

/// **The one model the reader publishes.**
#[derive(Copy, Clone, Debug)]
pub struct ReadModel {
    /// The `ModelId`: a path component, so lowercase. It shares the origin's
    /// `models/` namespace with voice's rows, and a test holds that it never
    /// collides with one.
    pub id: &'static str,
    /// The `ModelVersion`, naming the export as voice's do: our ONNX
    /// conversion, dated by the day its digests were recorded. Raised whenever
    /// any pin below changes.
    pub version: &'static str,
    /// The signed `engine` token the reader's worker dispatches on.
    pub engine: &'static str,
    /// The signed `tier` token. See [`MODEL`] for why this word.
    pub tier: &'static str,
    /// BCP 47 tags in canonical case. See [`MODEL`] for why these.
    pub languages: &'static [&'static str],
    /// Declared, and signed: what a device must be able to give the worker
    /// that loads this model. See [`MODEL`] for the number.
    pub working_set_bytes: u64,
    /// The SPDX identifier, which goes into the signed descriptor.
    pub licence: &'static str,
    /// The key of the licence text in `voice::repack::licence_text`, which
    /// travels inside the zip as `LICENCE.txt`.
    pub licence_file: &'static str,
    /// Who to credit. Signed, and shown in the consent step.
    pub attribution: &'static str,
    /// Copyright and origin lines for `NOTICE.txt`, one per line.
    pub origin_notice: &'static [&'static str],
    pub weights: &'static [WeightFile],
    pub outputs: &'static [ConvertedFile],
    /// The published zip's length. Pinned with its digest; see the module note.
    pub archive_size: u64,
    /// The published zip's SHA-256. Empty means unpinned.
    pub archive_sha256: &'static str,
}

impl ReadModel {
    /// Whether the zip this row publishes has its key and length recorded, so
    /// a publish can name it without any file on this machine.
    pub fn archive_is_pinned(&self) -> bool {
        is_digest(self.archive_sha256) && self.archive_size > 0
    }
}

/// 64 lowercase hexadecimal characters: the only spelling of a pin.
pub fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// **paddle2onnx's version, as the notice inside the zip states it.**
///
/// The pin itself is the lock's; this is the sentence Apache-2.0 §4(b) asks
/// for ("prominent notices stating that You changed the files"), and a test
/// holds it to the lock so the two cannot disagree.
/// The lock line it corresponds to is the same words joined by `==`.
pub const CONVERTER: &str = "paddle2onnx 2.1.0";

/// The detector's commit, written once for its three files.
const DET_COMMIT: &str = "106c97591b235f607453300d9fc8c1cad1b25488";
/// The recognizer's commit.
const REC_COMMIT: &str = "bd619643acac4b9650c040234da8d944476ee3f1";

/// **PP-OCRv6 small: one detector and one multilingual recognizer**, in one
/// download (OCR TECHNICAL D1).
///
/// The three choices the descriptor signs that are ours rather than the
/// model's:
///
/// - **`tier: standard`.** A descriptor's tier is "the feature's quality
///   choice for this model — never a model name", so it cannot be `small`,
///   which is PaddleOCR's name for the size of these weights (the series is
///   tiny, small, medium). The reader offers one model and no quality choice;
///   `standard` names that one choice without promising a size, and leaves
///   room beside it if a second reader is ever offered (EXPERIENCE rung 10).
/// - **`languages: und-Latn, zh, ja`.** The recognizer reads characters, not
///   languages: its card claims 50 languages, its alphabet is 18,708
///   characters, and its accuracy table reports printed and handwritten
///   English, simplified and traditional Chinese, and Japanese. The product
///   says it reads "Latin-script languages, Chinese and Japanese" (EXPERIENCE,
///   decisions table) and Settings states that in one line. `und-Latn` is BCP
///   47's tag for *any language, in Latin script*, which is exactly the claim
///   and renders as exactly that phrase; `zh` without a script covers both
///   the simplified and traditional columns; `ja` is Japanese, whose kana
///   are in the alphabet. Counted in the converted `rec_dict.txt`: 452 Latin
///   letters, 15,704 Han characters, 86 hiragana and 94 katakana, and no
///   Hangul, Arabic, Cyrillic, Devanagari or Thai. It also holds 76 Greek
///   letters; Greek is not claimed, because nobody measured it and the
///   product names it among rung 8's scripts. Listing fifty languages would claim per-language
///   support nobody measured and could not be rendered as that one line; the
///   eight interface languages would claim Arabic, which it does not read.
///   Rung 8's scripts arrive as more tags of the same kind (`und-Cyrl`, `ko`).
/// - **`working_set_bytes: 1 GiB`.** The worker that loads this model runs
///   under a 1 GiB commit limit, and the measured lifetime peak of a
///   long-lived recognition process is 594 MiB (MEASUREMENTS §3, one text line
///   per call). The field is what a device must be able to give before it
///   downloads, and what the worker is allowed is the honest answer; the
///   measurement is the evidence that the limit holds.
pub const MODEL: ReadModel = ReadModel {
    id: "pp-ocrv6-small",
    version: "onnx-2026-11-11",
    engine: "pp-ocr",
    tier: "standard",
    languages: &["und-Latn", "zh", "ja"],
    working_set_bytes: 1024 * 1024 * 1024,
    licence: "Apache-2.0",
    licence_file: "spdx/Apache-2.0.txt",
    attribution: "PaddlePaddle Authors, PP-OCRv6 small text detector and recognizer (Apache-2.0); \
                  converted to ONNX by Koru",
    origin_notice: &[
        "PP-OCRv6 small: a text detector (PP-OCRv6_small_det) and a",
        "multilingual text recognizer (PP-OCRv6_small_rec), from PaddleOCR.",
        "Copyright (c) PaddlePaddle Authors.",
        "Licensed under the Apache License, Version 2.0, reproduced in LICENCE.txt.",
        "Project: https://github.com/PaddlePaddle/PaddleOCR",
        "Model cards: https://huggingface.co/PaddlePaddle/PP-OCRv6_small_det",
        "             https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec",
    ],
    weights: &[
        WeightFile {
            part: "det",
            repository: "PaddlePaddle/PP-OCRv6_small_det",
            commit: DET_COMMIT,
            file: "inference.json",
            size: 229_307,
            sha256: "89240f689a4a77aad75ef55a8df0a15c8e1d4980a327d17e58f24bbadde5aeab",
        },
        WeightFile {
            part: "det",
            repository: "PaddlePaddle/PP-OCRv6_small_det",
            commit: DET_COMMIT,
            file: "inference.pdiparams",
            size: 9_817_888,
            sha256: "5043d4ccc8d63402ccea8feefcee4db57077431a873e78d2191836a178a492da",
        },
        WeightFile {
            part: "det",
            repository: "PaddlePaddle/PP-OCRv6_small_det",
            commit: DET_COMMIT,
            file: "inference.yml",
            size: 885,
            sha256: "193f435274bf9f0b5f71a929bbfbcf148282df7e633b34e7c373e8f44741b516",
        },
        WeightFile {
            part: "rec",
            repository: "PaddlePaddle/PP-OCRv6_small_rec",
            commit: REC_COMMIT,
            file: "inference.json",
            size: 208_004,
            sha256: "f0bf53c853937a917affdd74467472167727f8ab0f0f7bded01c4a16c27e46e6",
        },
        WeightFile {
            part: "rec",
            repository: "PaddlePaddle/PP-OCRv6_small_rec",
            commit: REC_COMMIT,
            file: "inference.pdiparams",
            size: 21_074_618,
            sha256: "406e1e689c9a7fbb04178007a3fe10cf852afe7bf8bb3bc6dbb9d532b13bd907",
        },
        WeightFile {
            part: "rec",
            repository: "PaddlePaddle/PP-OCRv6_small_rec",
            commit: REC_COMMIT,
            file: "inference.yml",
            size: 150_579,
            sha256: "ab078671bb49f06228eadccd34f1bb501e157f7a047095ffb943ba81512c77d1",
        },
    ],
    outputs: &[
        ConvertedFile {
            name: "det.onnx",
            size: 9_929_594,
            sha256: "56a00e6c881f00924ad09e4e477e05b132574570da52068c852e66e5065a3fbc",
        },
        ConvertedFile {
            name: "rec.onnx",
            size: 21_159_418,
            sha256: "4692bf837097b85a5e4c07f5a5a4200ded4587ec67c509cf6753e4ff33c0f17e",
        },
        ConvertedFile {
            name: "rec_dict.txt",
            size: 74_947,
            sha256: "b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d",
        },
    ],
    // Measured by `read-model package` from two conversions run from
    // `--clean` (2026-11-11), which produced the same zip byte for byte.
    archive_size: 26_567_146,
    archive_sha256: "fd74b109e507a244739b2036b1c89dce3f20ba1078164401137e21407f513266",
};
