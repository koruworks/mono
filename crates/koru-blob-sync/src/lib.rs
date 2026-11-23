// SPDX-License-Identifier: MPL-2.0

//! The client half of `cloud-blob-object-v1`: one transfer engine, shared by
//! every platform that moves blob bytes.
//!
//! What this crate is for, stated so something can be refused entry: the
//! **transfer**. The referenced-blob walk that says which blobs a Workspace
//! actually has, the reconciliation that decides what to do about each one,
//! the two state machines that do it, the allowlist that says where bytes may
//! be sent, and the announcement that tells a view its picture arrived.
//!
//! What is deliberately **not** here, because it is a fact about one platform
//! rather than about a transfer:
//!
//! | | |
//! | --- | --- |
//! | UI, a window, a decoded pixel | never |
//! | an iroh type, a peer, a connection | never — the `PeerBlobs` port is the whole of what this crate knows about a peer |
//! | SQLite, a file layout, a Store implementation | never — ports only |
//! | an Android API, a desktop process, a service | never — composition is the host's |
//!
//! The wire contract is not restated here either: the constants, the stable
//! codes, the `Content-Range` admission and every JSON body come from
//! [`koru_cloud_blob`], so a client and the two services cannot disagree about
//! a field name. This crate carries what only a *client* has to decide.
//!
//! ## The four rules that shape everything below
//!
//! - **Association comes last.** A download installs the object and only then
//!   writes the Workspace association, so a successful metadata lookup can
//!   never point at partial bytes (S10). A mismatch quarantines and publishes
//!   nothing (S11).
//! - **The resume authority is the worker's `HEAD`, never a local guess.** The
//!   staging file's synced length is the truth for a download and the worker's
//!   committed offset is the truth for an upload; two authorities that can
//!   disagree is not a design.
//! - **A control response is not authority to send a person's file anywhere.**
//!   Every returned byte URL passes [`ByteOrigins`] first.
//! - **Nothing walks a Workspace on a timer.** Reconciliation runs on the
//!   [`Trigger`]s the plan's §10 lists and on nothing else.
//!
//! The contract is `docs/cloud-platform/contracts/cloud-blob-object-v1.md`;
//! the decision is ADR-000-blobobjectworker.

pub mod admission;
pub mod allowlist;
pub mod control;
pub mod coverage;
pub mod download;
pub mod engine;
/// Fake ports, for this crate's tests and for the integration tests beside
/// it. Behind a feature so a deployed binary never carries them.
#[cfg(any(test, feature = "test-support"))]
pub mod fakes;
pub mod object;
pub mod ports;
pub mod referenced;
pub mod retry;
pub mod staging;
pub mod status;
pub mod upload;
/// The checked-in `logical-usage-v1` corpus. Test material: see the `vectors`
/// feature.
#[cfg(any(test, feature = "vectors"))]
pub mod vectors;

pub use allowlist::{AdmittedUrl, ByteOrigins, OriginRefused};
pub use control::{BlobControlPlane, HttpControlPlane};
pub use coverage::{
    BlobPresence, CoverageReport, CoverageRequest, CoverageState, CoveredDocuments, DocumentSample,
    DocumentState, SampleDigest, SampledDocument,
};
pub use download::DownloadOutcome;
pub use download::PEER_ATTEMPT_WINDOW;
pub use engine::{
    BlobEngine, BlobFileFacts, DocumentCoverage, DocumentFiles, LiveAuthorization, Pass,
    ReleaseReport, Trigger, WaitingDocument,
};
pub use object::{BlobObjectPlane, HttpObjectPlane};
pub use ports::{
    AlwaysAdmitted, AlwaysPermitted, BlobDownloads, BlobPolicy, BlobPorts, BlobStorePort,
    CloudAccess, CloudAccessFacts, LocalBlob, NoBlobDownloads, NoPeerBlobs, PeerBlobs, PeerChunk,
    PortFuture, TransferError, TransferHold, TransportModes,
};
pub use referenced::{
    DocumentReferences, ReferencedBlob, Walk, WalkDocument, WalkNode, document_blobs,
    document_references, referenced_blobs,
};
pub use retry::{Backoff, Disposition, classify};
pub use staging::StagingArea;
pub use status::{BlobPreflight, BlobTransferStatus, Direction, TransferPath};
pub use upload::UploadOutcome;

/// How long the engine waits after a durable document change before walking
/// the Workspace whose hash set may have moved.
///
/// **This is the number that keeps typing free.** A keystroke does not reach
/// this crate at all — only a *durable* commit does — but a burst of commits
/// during a paste or an import would otherwise be a burst of walks. The value
/// is the document pass's own trailing debounce, so a person who pastes six
/// pictures gets one walk rather than six, and nothing at all happens while
/// they are typing between them.
pub const CHANGE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

// **One transfer at a time**, which is why there is no concurrency constant
// here to tune. The link is the bottleneck rather than the parallelism; a
// queue that drains in order is a queue whose progress a person can read; and
// one chunk in flight is what makes the memory bound a fact rather than an
// average. A device that opened one connection per picture on the first enable
// of a Workspace of several years would be the opposite of all three.

#[cfg(test)]
mod tests;
