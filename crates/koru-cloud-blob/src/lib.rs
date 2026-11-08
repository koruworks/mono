// SPDX-License-Identifier: MPL-2.0

//! The `cloud-blob-object-v1` wire contract, in one place.
//!
//! What this crate is for: the facts the control API, the object worker and
//! (at rung 7) a third-party storage adapter must agree on exactly — the
//! derived bounds, the stable error vocabulary, the shape of a `Content-Range`
//! admission, the object key an immutable locator names, and the JSON of every
//! route. It holds no I/O, no database, no signing material and no policy that
//! belongs to one side: a clause only lives here if *both* services would
//! otherwise have to spell it, which is the test that keeps it from becoming a
//! bucket.
//!
//! The capability itself is not here. It lives in
//! [`koru_account_session::cloud::blob`], beside the sync credential and the
//! ring that signs it, because that crate owns the purpose-isolated signing
//! seam. This crate carries the *bindings* — which upload, which workspace,
//! which incarnation, which epoch — because those are a comparison against the
//! world the request arrived in, which is neither service's secret.
//!
//! The contract is [`docs/cloud-platform/contracts/cloud-blob-object-v1.md`],
//! approved 2026-09-17, and its vectors are read by tests through
//! [`vectors`] rather than retyped.

pub mod binding;
pub mod code;
pub mod locator;
pub mod range;
/// The checked-in contract vectors. Test material: see the `vectors` feature.
#[cfg(any(test, feature = "vectors"))]
pub mod vectors;
pub mod wire;

pub use binding::{BindingWorld, check_bindings};
pub use code::{BlobCode, Deferral};
pub use locator::{ObjectLocator, object_key, staging_name};
pub use range::{ContentRange, RangeOutcome, admit_range, parse_content_range};

/// The networks Koru promises to *finish* on — a congested train, a busy café,
/// a weak rural signal (D250). SI bytes, because the decision is spelled "16
/// kB/s". It is not a speed promise: 2 GiB at this rate takes a day and a half,
/// resumed across days. It is the promise that progress is possible at all.
pub const SLOWEST_SUPPORTED_LINK_BYTES_PER_SECOND: u64 = 16_000;

/// `CAPABILITY_LIFETIME`, in whole seconds (D250). Read from the other end it
/// is how long bytes may still be moving after a person turns Cloud off, and
/// for reads it is the revocation latency the split costs (D252).
pub const CAPABILITY_LIFETIME_SECS: i64 =
    koru_account_session::cloud::blob::BLOB_CAPABILITY_DEFAULT_SECONDS;

/// The margin between what one capability must carry and what the slowest
/// promised link can deliver inside its life.
pub const SAFETY: u64 = 2;

/// One bounded chunk. Derived, not chosen: see the assertion below. A client
/// never hard-codes it — allocation returns it — so a later provider can raise
/// it without touching an installed app.
pub const CHUNK: u64 = 2 * 1024 * 1024;

/// **There is one size ceiling and it is the Store's** (D248). The Cloud limit
/// is derived from it rather than kept beside it, so raising the local limit
/// raises the Cloud limit in the same commit.
pub const MAX_BLOB_SIZE: u64 = koru_store_core::ports::BLOB_UPLOAD_BODY_LIMIT;

pub use koru_account_session::cloud::blob::EMPTY_BLOB_HASH;

/// A verified length can be zero only for the address of the empty file.
pub fn valid_blob_size(hash: &str, size: u64) -> bool {
    size <= MAX_BLOB_SIZE && (size != 0 || hash == EMPTY_BLOB_HASH)
}

/// A nonterminal session must outlive the longest interruption a person expects
/// to resume across — a laptop shut for a weekend — and must not outlive the
/// operator's willingness to hold staging bytes. Every acknowledged chunk
/// extends the deadline, so a session that is progressing is never reclaimed.
pub const RESUME_WINDOW_MILLIS: i64 = 7 * 24 * 60 * 60 * 1_000;

/// Staging is reclaimed strictly after the session that owns it expires, so a
/// session can never lose its bytes while it is still resumable.
pub const STAGING_RECLAIM_MILLIS: i64 = RESUME_WINDOW_MILLIS + 24 * 60 * 60 * 1_000;

// A device on a link slower than `chunk / capability_lifetime` can never
// complete a single chunk, however many times it retries. Two numbers in two
// files is how the recovery budget and the checkpoint trigger came to
// contradict each other (D243), so they contradict each other here instead,
// at compile time.
const _: () = assert!(
    CHUNK * SAFETY <= SLOWEST_SUPPORTED_LINK_BYTES_PER_SECOND * CAPABILITY_LIFETIME_SECS as u64,
    "a promised link cannot deliver one chunk inside one capability's life"
);
const _: () = assert!(
    MAX_BLOB_SIZE == koru_store_core::ports::BLOB_UPLOAD_BODY_LIMIT,
    "the Cloud ceiling is the Store ceiling, never a second number"
);
const _: () = assert!(STAGING_RECLAIM_MILLIS > RESUME_WINDOW_MILLIS);
const _: () = assert!(CHUNK < MAX_BLOB_SIZE);

/// The header a `HEAD` answers with: the synced length of the session's staging
/// file, which **is** the contiguous durable prefix and the authority for a
/// resume. The worker needs no database to answer it.
pub const COMMITTED_OFFSET_HEADER: &str = "x-koru-committed-offset";

/// The stable §7 code a refusal carries, repeated in a header beside the JSON
/// body.
///
/// **Because a response with no body still has to name its code.** HTTP
/// forbids a body on the answer to a `HEAD`, and the resume authority (§4.1)
/// *is* a `HEAD` — so the `BlobErrorV1` a refusal writes is stripped by the
/// server before it reaches the client, which is then left with a bare status.
/// `401` is two codes and `403` is four, so a client guessing from the status
/// sends a person to check the wrong thing: this is exactly how a wrong
/// capability issuer was read as a clock problem for hours. The header
/// survives the stripping, and every refusal on every route sets it so that no
/// route has to remember to.
pub const BLOB_CODE_HEADER: &str = "x-koru-blob-code";

/// The authentication scheme on every byte route — deliberately not `Bearer`.
/// An account token sent to a byte route, or a capability sent to a control
/// route, must fail as a malformed credential rather than be tried as the other
/// kind: at rung 7 one of these two travels to a third party.
pub const CAPABILITY_SCHEME: &str = "Capability";

/// The scheme the object worker authenticates *itself* with when it reports a
/// verified object to the control authority. A third distinct word, for the
/// same reason the second one exists.
pub const WORKER_SCHEME: &str = "Worker";

/// The report route on the API, named here so the worker and the API cannot
/// disagree about it.
pub const OBJECT_REPORTS_PATH: &str = "/v1/internal/object-reports";

#[cfg(test)]
mod tests;
