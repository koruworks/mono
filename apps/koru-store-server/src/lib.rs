// SPDX-License-Identifier: MPL-2.0

//! Reusable Koru Store server worker and its standalone host.
//!
//! The worker owns Store lifecycle and resources. [`host`] is the optional
//! process adapter used by development, diagnostics, vertical tests, and a
//! future separate deployment. Embedders use [`StoreWorker`] directly.

mod attributes;
mod cli;
mod console;
mod daemon;
mod derived;
mod events_feed;
pub mod host;
pub mod log;
mod projection;
mod replication;
mod root_inspection;
mod search;
mod store_backup;
mod sync_control;
mod systemdoc;
mod tagnames;
mod tasks;
mod tray;

pub use daemon::blob_sync;
pub use daemon::cloud_driver;
pub use daemon::cloud_sync;
/// An in-memory Cloud worker on the real carrier, for a host's tests across
/// crates. Off in every product build.
#[cfg(feature = "test-support")]
pub use daemon::fake_cloud;
pub use daemon::memory;
pub use daemon::{
    INTERNAL_READY_ENV, StandaloneReady, StoreJobs, StoreReply, StoreRootClaim, StoreWorker,
    StoreWorkerClient, StoreWorkerInfo, WorkerConfig, WorkerError, WorkerExit, WorkerHandle,
    WorkerStatus,
};

/// The concrete Store owned by [`StoreWorker`]. Exposed only to its typed
/// in-process task port; callers receive no lock or direct persistent handle.
pub type WorkerStore = koru_store_core::service::Store<
    koru_store_sqlite::SqliteCatalog,
    koru_store_sqlite::SqliteBackend,
>;
