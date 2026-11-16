// SPDX-License-Identifier: MPL-2.0

//! **How bytes get here — the seam, and the two implementations that need no
//! network.**
//!
//! Plan 1.1 defined the interface; **Plan 1.2 wrote the HTTP one**, and it is
//! [`http::HttpSource`], with the resume that
//! `koru_update::controller::download_verified` does not have: a `.part` keyed
//! by digest so it survives a restart, `If-Range` with `Content-Range`
//! validation and explicit 200 / 206 / 416 handling rather than trust in
//! `Accept-Ranges`, and one full re-hash of the completed file — because a
//! streaming `Sha256` cannot be resumed and pretending otherwise installs
//! corruption. That re-hash is the store's, not the source's, for the reason
//! the last paragraph here gives.
//!
//! Two further sources live here, and neither is a placeholder:
//!
//! - [`NoNetworkSource`] is what a build with no downloader linked uses. It
//!   answers [`Offline`](crate::ModelErrorCode::Offline) for every URL. That is
//!   an honest default rather than an `unimplemented!()`: *this build cannot
//!   fetch anything* is a state a caller can report to a person, and a panic is
//!   not.
//! - [`DirectorySource`] installs from bytes somebody already has. It is what
//!   Plan 1.3's end-to-end tests use instead of standing up a server, and it is
//!   also a real product path — `koru_update::controller` keeps `copy_verified`
//!   beside `download_verified` for exactly the same reason, so that a machine
//!   with no route to the internet can still be given an artefact.
//!
//! **Verification is not a source's job.** A source produces bytes at a path;
//! [`ModelStore::install`](crate::ModelStore) checks the signature before it
//! calls one and the size and digest after. Splitting it that way is what stops
//! a second source ever being written with a weaker check in it.

pub mod http;

pub use http::HttpSource;

use crate::descriptor::ModelDescriptor;
use crate::error::{ModelError, ModelErrorCode};
use crate::store::{InstallProgress, InstallStage};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use url::Url;

/// Somewhere a model archive can be fetched from.
///
/// One method, and it is deliberately about *one* URL: trying the mirrors in
/// order is the store's decision, because only the store knows whether a
/// failure was worth retrying elsewhere.
pub trait ModelSource: Send + Sync {
    /// Put the archive named by `url` at `destination`.
    ///
    /// `descriptor` is what the bytes are supposed to be — its `size` is the
    /// progress total and its `digest` is what a resumable implementation keys
    /// its `.part` file by. An implementation may use it to refuse early; it
    /// may not treat having used it as verification.
    ///
    /// `cancel` is polled between chunks and answers
    /// [`ModelErrorCode::Cancelled`], leaving nothing a later read would
    /// mistake for a complete download.
    fn fetch(
        &self,
        url: &Url,
        descriptor: &ModelDescriptor,
        destination: &Path,
        progress: &mut dyn FnMut(InstallProgress),
        cancel: &AtomicBool,
    ) -> Result<(), ModelError>;
}

/// A source that fetches nothing, and says so.
///
/// The default for any build that has not linked a downloader. Everything a
/// person can already do — listing what is installed, loading it, dictating —
/// keeps working; only installing something new is refused, with the code that
/// names the reason.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoNetworkSource;

impl ModelSource for NoNetworkSource {
    fn fetch(
        &self,
        url: &Url,
        _descriptor: &ModelDescriptor,
        _destination: &Path,
        _progress: &mut dyn FnMut(InstallProgress),
        _cancel: &AtomicBool,
    ) -> Result<(), ModelError> {
        Err(ModelErrorCode::Offline.with(format!(
            "this build links no downloader, so {url} cannot be fetched"
        )))
    }
}

/// A source that copies from a local directory, matching on the URL's last path
/// segment.
///
/// The archive for `https://models.koru.do/v1/parakeet-1.2.0.tar.gz` is
/// `<dir>/parakeet-1.2.0.tar.gz`. Matching on the file name rather than on the
/// whole URL is what lets one directory of downloads stand in for a catalog
/// whose hosts have changed — and it costs nothing, because the file name is
/// not what authorises anything: the signature and the digest are, and both are
/// checked by the store regardless of where the bytes came from.
#[derive(Clone, Debug)]
pub struct DirectorySource {
    dir: PathBuf,
}

impl DirectorySource {
    pub fn new(dir: impl Into<PathBuf>) -> DirectorySource {
        DirectorySource { dir: dir.into() }
    }

    /// The local file a URL names, or `None` when the URL has no usable last
    /// segment. Public so a caller can say *the archive is not in that folder*
    /// before starting anything.
    pub fn local_path(&self, url: &Url) -> Option<PathBuf> {
        let name = url.path_segments()?.next_back()?;
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':']) {
            return None;
        }
        Some(self.dir.join(name))
    }
}

impl ModelSource for DirectorySource {
    fn fetch(
        &self,
        url: &Url,
        descriptor: &ModelDescriptor,
        destination: &Path,
        progress: &mut dyn FnMut(InstallProgress),
        cancel: &AtomicBool,
    ) -> Result<(), ModelError> {
        if cancel.load(Ordering::Acquire) {
            return Err(ModelErrorCode::Cancelled.with("cancelled before the copy began"));
        }
        let source = self.local_path(url).ok_or_else(|| {
            ModelErrorCode::DownloadFailed.with(format!("{url} names no file to copy"))
        })?;
        if !source.is_file() {
            return Err(
                ModelErrorCode::DownloadFailed.with(format!("{} is not a file", source.display()))
            );
        }
        progress(InstallProgress {
            stage: InstallStage::Downloading,
            received: 0,
            total: descriptor.size,
        });
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let copied = std::fs::copy(&source, destination).map_err(|e| {
            ModelErrorCode::DownloadFailed.with(format!(
                "could not copy {} to {}: {e}",
                source.display(),
                destination.display()
            ))
        })?;
        progress(InstallProgress {
            stage: InstallStage::Downloading,
            received: copied,
            total: descriptor.size,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests;
