// SPDX-License-Identifier: MPL-2.0

//! **The reader: PaddleOCR's PP-OCRv6 small models, run by `oar-ocr` on ONNX
//! Runtime, behind `koru_read::Reader`** (OCR TECHNICAL D1, D5, D6).
//!
//! This is the one crate allowed to name the product it adapts, and it is
//! where the reader token [`READER_TOKEN`] (`"pp-ocr"`) is declared — the
//! contract crate names only the text reader.
//!
//! ## What is here, and what needs the runtime
//!
//! | | runtime? |
//! | --- | --- |
//! | [`READER_TOKEN`], [`reader_id`] | no |
//! | [`settings`]: the thread count, one line per call, the detection settings Plan 0.1 measured | no |
//! | [`model`]: the model directory's three files and the dictionary's shape | no |
//! | [`lines`]: recognized lines into reading order, and the confidence | no |
//! | [`artifacts`]: the pinned ONNX Runtime static library the runtime links | no |
//! | [`load`]: build the pipeline and hand back a `Reader` | **yes** |
//!
//! With `runtime` off — every ordinary workspace build — [`load`] answers
//! `unsupported` and nothing native is compiled or linked. With it on, on a
//! target that has no pinned ONNX Runtime library (Windows on ARM, Linux
//! aarch64), the same: [`RECOGNITION_BUILT`] says which build this is.
//!
//! ## The settings, and where they come from
//!
//! Every number is Plan 0.1's (`docs/features/ocr/MEASUREMENTS.md`): one text
//! line per recognizer call (four peaked at 1,473 MiB and was not faster; one
//! peaks at 504 MiB a page, inside the worker's 1 GiB ceiling), `oar-ocr`'s
//! own PP-OCRv6 small detection settings written out because its copy is
//! private, four ONNX Runtime threads by default, and `oar-ocr`'s own thread
//! pool bounded to the same number so a backlog cannot take the machine.

use koru_read::{FailureCode, ReadError, Reader, ReaderId};

pub mod artifacts;
pub mod lines;
pub mod model;
pub mod settings;

#[cfg(all(
    feature = "runtime",
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        target_os = "macos"
    )
))]
mod engine;

pub use model::{DictionaryError, ModelDir, ModelError};
pub use settings::Settings;

/// The token written into every reading this reader produces.
pub const READER_TOKEN: &str = "pp-ocr";

/// [`READER_TOKEN`] as the contract's checked type.
pub fn reader_id() -> ReaderId {
    ReaderId::parse(READER_TOKEN).expect("the reader token is a valid token")
}

/// Whether this build can recognize at all: compiled with `runtime`, on a
/// target with a pinned ONNX Runtime library. A build that cannot still reads
/// text layers; it never offers recognition.
pub const RECOGNITION_BUILT: bool = cfg!(all(
    feature = "runtime",
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        target_os = "macos"
    )
));

/// What the reader says about itself, printed and logged, never parsed.
pub const VERSION: &str = "PP-OCRv6 small, oar-ocr 0.10.2, ONNX Runtime 1.27.1";

/// **Load the models in `model` and hand back a reader.**
///
/// Checks the directory and the dictionary first ([`ModelDir::check`]), then
/// builds both ONNX Runtime sessions — the slow part, a few hundred
/// milliseconds and about 50 MiB. A model that is missing, malformed or will
/// not load answers [`FailureCode::NeedsReader`]: the reader is not usable here
/// until it is installed again. A build without the runtime answers
/// [`FailureCode::Unsupported`].
pub fn load(model: &ModelDir, settings: &Settings) -> Result<Box<dyn Reader>, ReadError> {
    #[cfg(all(
        feature = "runtime",
        any(
            all(target_os = "windows", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
            target_os = "macos"
        )
    ))]
    {
        let dictionary = model.check().map_err(|error| ReadError {
            code: FailureCode::NeedsReader,
            message: error.to_string(),
        })?;
        let reader = engine::PaddleReader::load(model, dictionary, settings)?;
        Ok(Box::new(reader))
    }
    #[cfg(not(all(
        feature = "runtime",
        any(
            all(target_os = "windows", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
            target_os = "macos"
        )
    )))]
    {
        let _ = (model, settings);
        Err(ReadError {
            code: FailureCode::Unsupported,
            message: "this build was made without recognition".into(),
        })
    }
}

#[cfg(test)]
mod tests;
