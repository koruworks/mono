// SPDX-License-Identifier: MPL-2.0

//! **Whether this worker can recognize, and the reader it recognizes with —
//! loaded on the first job that needs it, never at start.**
//!
//! Three things decide whether recognition is available:
//!
//! 1. **the build**: `koru_read_paddle::RECOGNITION_BUILT`, which is false
//!    unless the worker was built with its `recognition` feature on a target
//!    with a pinned ONNX Runtime (Windows x64 today); a build without it
//!    answers a recognition job `unsupported`, a fact about this computer;
//! 2. **the model**: the directory the Store passed as `--model <dir>`, which
//!    must hold the three files with a well-formed dictionary — checked once
//!    at start, which reads 75 KB and loads nothing; a worker started without
//!    one, or with one that does not check, answers `needs-reader`, the
//!    contract's word for a reader that is not installed;
//! 3. **the load**: the first recognition builds both ONNX Runtime sessions
//!    (a few hundred milliseconds, about 50 MiB). A model that fails to load
//!    makes recognition unavailable for the rest of the process, so the next
//!    `Status` says so and the Store stops sending recognition jobs.
//!
//! `Want::Text` never reaches here: a text job loads nothing.
//!
//! ## One reader, lent out
//!
//! The worker runs one job at a time and reads one page at a time, so there
//! is one reader. A page **takes** it, reads on a thread of its own under the
//! page budget, and **gives it back**. A page that runs past its budget is
//! abandoned with the reader still inside it — a native call cannot be
//! stopped — and the next page loads a fresh one; the abandoned-work watchdog
//! and the memory ceiling bound what that costs.

use std::path::PathBuf;
use std::sync::Mutex;

use koru_read::{FailureCode, ReadError, Reader};
use koru_read_paddle::{ModelDir, Settings};

/// Builds a reader. The product's loads the model; a test's hands a fake.
pub type Loader = dyn Fn() -> Result<Box<dyn Reader>, ReadError> + Send + Sync;

struct Slot {
    reader: Option<Box<dyn Reader>>,
    /// A load failed: recognition is gone for this process.
    broken: bool,
    loads: u32,
}

/// The worker's recognition, or its absence.
pub struct Recognition {
    built: bool,
    loader: Option<Box<Loader>>,
    slot: Mutex<Slot>,
}

impl Recognition {
    /// No recognition. `built` says why: a build with recognition and no
    /// model waits for the reader (`needs-reader`); a build without cannot
    /// (`unsupported`).
    pub fn none(built: bool) -> Recognition {
        Recognition {
            built,
            loader: None,
            slot: Mutex::new(Slot {
                reader: None,
                broken: false,
                loads: 0,
            }),
        }
    }

    /// Recognition through `loader`, called on the first page that needs it.
    pub fn with_loader(
        loader: impl Fn() -> Result<Box<dyn Reader>, ReadError> + Send + Sync + 'static,
    ) -> Recognition {
        Recognition {
            loader: Some(Box::new(loader)),
            ..Recognition::none(true)
        }
    }

    /// **The worker's own**, from its `--model` argument and
    /// `KORU_READ_THREADS`. Checks the model directory now; loads nothing.
    pub fn product(model: Option<PathBuf>, settings: Settings) -> Recognition {
        let built = koru_read_paddle::RECOGNITION_BUILT;
        let Some(dir) = model else {
            return Recognition::none(built);
        };
        if !built {
            tracing::info!("a model was given, and this build has no recognition");
            return Recognition::none(false);
        }
        let model = ModelDir::new(dir);
        if let Err(error) = model.check() {
            // The message names one of the model's three files, nothing of a
            // person's.
            tracing::warn!(%error, "the model cannot be used; recognition is unavailable");
            return Recognition::none(true);
        }
        Recognition::with_loader(move || koru_read_paddle::load(&model, &settings))
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether a recognition job can be read here now: what `Status` reports.
    pub fn available(&self) -> bool {
        self.loader.is_some() && !self.slot().broken
    }

    /// Whether this worker was built with recognition: given a model, it could
    /// recognize. What `Status` reports as `recognition_built`, so the Store
    /// offers no download a worker built without it could not use.
    pub fn built(&self) -> bool {
        self.built
    }

    /// What a job that needs recognition is answered when it is not
    /// available.
    pub fn absent(&self) -> FailureCode {
        if self.built {
            FailureCode::NeedsReader
        } else {
            FailureCode::Unsupported
        }
    }

    /// How many times a reader has been built in this process.
    pub fn loads(&self) -> u32 {
        self.slot().loads
    }

    /// **Take the reader**, building it if there is none. The lock is not
    /// held while the model loads, so a `Status` meanwhile is answered at
    /// once.
    pub fn take(&self) -> Result<Box<dyn Reader>, FailureCode> {
        let Some(loader) = &self.loader else {
            return Err(self.absent());
        };
        {
            let mut slot = self.slot();
            if slot.broken {
                return Err(self.absent());
            }
            if let Some(reader) = slot.reader.take() {
                return Ok(reader);
            }
        }
        let started = std::time::Instant::now();
        match loader() {
            Ok(reader) => {
                self.slot().loads += 1;
                tracing::info!(
                    ms = started.elapsed().as_millis() as u64,
                    version = reader.describe().version,
                    "the reader's model was loaded"
                );
                Ok(reader)
            }
            Err(error) => {
                self.slot().broken = true;
                tracing::warn!(
                    code = error.code.as_str(),
                    error = %error.message,
                    "the reader's model could not be loaded; recognition is unavailable"
                );
                Err(match error.code {
                    FailureCode::Unsupported => FailureCode::Unsupported,
                    _ => FailureCode::NeedsReader,
                })
            }
        }
    }

    /// Give the reader back after a page.
    pub fn give_back(&self, reader: Box<dyn Reader>) {
        self.slot().reader = Some(reader);
    }
}

#[cfg(test)]
mod tests;
