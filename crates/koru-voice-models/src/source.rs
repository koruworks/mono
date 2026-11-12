// SPDX-License-Identifier: MPL-2.0

//! **How bytes get here, in voice's words.**
//!
//! The three sources — [`HttpSource`] (the resumable downloader),
//! [`DirectorySource`] and [`NoNetworkSource`] — are `koru-models`', and so is
//! the generic `koru_models::ModelSource` they implement. This module keeps
//! voice's [`ModelSource`], whose `fetch` takes voice's typed descriptor,
//! because voice's store is built from a `Box<dyn ModelSource>` and its tests
//! write sources of their own against that signature. Each of the three
//! implements both traits; a voice source handed to the generic store is
//! wrapped in [`Typed`], which maps the descriptor back.
//!
//! **Verification is not a source's job.** A source produces bytes at a path;
//! the store checks the signature before it calls one and the size and digest
//! after.

pub use koru_models::source::http;
pub use koru_models::source::{DirectorySource, HttpSource, NoNetworkSource};

use crate::descriptor::ModelDescriptor;
use crate::error::ModelError;
use crate::store::InstallProgress;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use url::Url;

// The suite beside this module predates the extraction and names these
// through `super::*`.
#[cfg(test)]
use crate::{error::ModelErrorCode, store::InstallStage};

/// Somewhere a speech model archive can be fetched from.
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
    /// [`ModelErrorCode::Cancelled`](crate::ModelErrorCode::Cancelled), leaving
    /// nothing a later read would mistake for a complete download.
    fn fetch(
        &self,
        url: &Url,
        descriptor: &ModelDescriptor,
        destination: &Path,
        progress: &mut dyn FnMut(InstallProgress),
        cancel: &AtomicBool,
    ) -> Result<(), ModelError>;
}

/// One body for the three generic sources: map the descriptor and hand over.
fn through_generic(
    source: &dyn koru_models::ModelSource,
    url: &Url,
    descriptor: &ModelDescriptor,
    destination: &Path,
    progress: &mut dyn FnMut(InstallProgress),
    cancel: &AtomicBool,
) -> Result<(), ModelError> {
    source.fetch(url, &descriptor.to_model()?, destination, progress, cancel)
}

impl ModelSource for NoNetworkSource {
    fn fetch(
        &self,
        url: &Url,
        descriptor: &ModelDescriptor,
        destination: &Path,
        progress: &mut dyn FnMut(InstallProgress),
        cancel: &AtomicBool,
    ) -> Result<(), ModelError> {
        through_generic(self, url, descriptor, destination, progress, cancel)
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
        through_generic(self, url, descriptor, destination, progress, cancel)
    }
}

impl ModelSource for HttpSource {
    fn fetch(
        &self,
        url: &Url,
        descriptor: &ModelDescriptor,
        destination: &Path,
        progress: &mut dyn FnMut(InstallProgress),
        cancel: &AtomicBool,
    ) -> Result<(), ModelError> {
        through_generic(self, url, descriptor, destination, progress, cancel)
    }
}

/// **A voice source, as the generic store sees it.** The store calls it with
/// the generic descriptor; it answers with voice's, which a voice source's
/// `fetch` is written against.
pub(crate) struct Typed(pub(crate) Box<dyn ModelSource>);

impl koru_models::ModelSource for Typed {
    fn fetch(
        &self,
        url: &Url,
        descriptor: &koru_models::ModelDescriptor,
        destination: &Path,
        progress: &mut dyn FnMut(InstallProgress),
        cancel: &AtomicBool,
    ) -> Result<(), ModelError> {
        let typed = ModelDescriptor::from_model(descriptor.clone())?;
        self.0.fetch(url, &typed, destination, progress, cancel)
    }
}

#[cfg(test)]
mod tests;
