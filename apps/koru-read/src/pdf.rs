// SPDX-License-Identifier: MPL-2.0

//! **Opening a PDF, and taking its pages' text layers** with `hayro`.
//!
//! What the library does with a file that is not plainly readable, found by
//! reading its source (`hayro-syntax` 0.9.0, `pdf.rs`, `xref.rs`,
//! `crypto/mod.rs`) and held by the tests below:
//!
//! - **Encryption it can say.** `Pdf::new` tries the empty password. A file
//!   with a user password answers `LoadPdfError::Decryption(PasswordProtected)`;
//!   a file encrypted with permissions only (an empty user password, the
//!   common "no printing" PDF) opens and reads. A security handler other than
//!   the standard one — a certificate — is `UnsupportedAlgorithm`, and is as
//!   locked to us as a password, so it is reported the same way. So the
//!   trailer's `/Encrypt` does not have to be read here: the library reads it.
//! - **Damage it repairs when it can.** A broken cross-reference table is
//!   rebuilt by scanning the file; only a file it cannot make a document of
//!   at all is `LoadPdfError::Invalid`, reported as `damaged`. An encryption
//!   dictionary it cannot make sense of is damage too, and so is a file that
//!   opens with no pages (Plan 0.1 met a truncated one that does).
//!
//! The measurement plan (0.1) confirmed the first two on real files.
//!
//! Both are true on every device with these bytes, which is what lets them be
//! written into the synced reading as `failed/password` and `failed/damaged`.

use hayro_interpret::hayro_syntax::{DecryptionError, LoadPdfError, Pdf};
use hayro_interpret::kurbo::{Affine, Rect};
use hayro_interpret::{Context, InterpreterCache, InterpreterSettings, interpret_page};
use koru_read::Reason;

use crate::accept::Layer;
use crate::text::Collector;

/// **A document whose pages can be read for their text layers.** The seam the
/// job reads through, so a test can stand in a page that never finishes.
pub trait Document: Send + Sync {
    fn page_count(&self) -> u32;

    /// Read pages `from..=to` (counting from 1) in order, handing each to
    /// `out` as soon as it is read; stop early when `out` answers `false`.
    fn read_pages(&self, from: u32, to: u32, out: &mut dyn FnMut(u32, Layer) -> bool);
}

/// A PDF `hayro` opened.
pub struct PdfDocument {
    pdf: Pdf,
}

/// Open `bytes` as a PDF, or say why it cannot be read on any device.
pub fn open(bytes: Vec<u8>) -> Result<PdfDocument, Reason> {
    match Pdf::new(bytes) {
        // A file cut short can open with no error and no pages (Plan 0.1 met
        // one). A PDF is a page tree; one without pages is damage.
        Ok(pdf) if pdf.pages().is_empty() => Err(Reason::Damaged),
        Ok(pdf) => Ok(PdfDocument { pdf }),
        Err(LoadPdfError::Decryption(
            DecryptionError::PasswordProtected | DecryptionError::UnsupportedAlgorithm,
        )) => Err(Reason::Password),
        Err(LoadPdfError::Decryption(
            DecryptionError::InvalidEncryption | DecryptionError::MissingIDEntry,
        )) => Err(Reason::Damaged),
        Err(LoadPdfError::Invalid) => Err(Reason::Damaged),
    }
}

impl Document for PdfDocument {
    fn page_count(&self) -> u32 {
        u32::try_from(self.pdf.pages().len()).unwrap_or(u32::MAX)
    }

    fn read_pages(&self, from: u32, to: u32, out: &mut dyn FnMut(u32, Layer) -> bool) {
        let pages = self.pdf.pages();
        // One cache for the run, so a font used on every page is parsed once.
        let cache = InterpreterCache::new();
        // The defaults: substitutes for the standard fonts and the predefined
        // CMaps are compiled in, and a page's annotations (form fields,
        // stamps) are interpreted along with its content.
        let settings = InterpreterSettings::default();
        for number in from.max(1)..=to {
            let Some(page) = usize::try_from(number - 1)
                .ok()
                .and_then(|index| pages.get(index))
            else {
                return;
            };
            let (width, height) = page.render_dimensions();
            let (width, height) = (f64::from(width), f64::from(height));
            // The page as a person sees it: its `/Rotate` applied, y up, so
            // text that reads upright on screen is upright here.
            let transform = Affine::new(page.initial_transform(false).as_coeffs());
            let mut collector = Collector::new(width * height);
            let mut context = Context::new(
                transform,
                Rect::new(0.0, 0.0, width, height),
                &cache,
                self.pdf.xref(),
                settings.clone(),
            );
            interpret_page(page, &mut context, &mut collector);
            if !out(number, collector.finish()) {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests;
