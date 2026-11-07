// SPDX-License-Identifier: MPL-2.0

//! The download, as the plan's §10 orders it — and as **S10 and S11** assert
//! it.
//!
//! ```text
//! read_capability → get:range… → hash → install_object → write_association → announce
//! ```
//!
//! Two invariants, and every line below is one of them:
//!
//! - **`association_before_bytes: false`.** The object is installed first and
//!   the Workspace association is written last, so a successful metadata
//!   lookup can never point at partial bytes. The order lives in one Store
//!   call ([`crate::ports::BlobStorePort::install_downloaded`]) rather than in
//!   two, because two is a place for a future caller to get it backwards.
//! - **`resends: 0`.** The staging file's synced length is the resume point,
//!   so a client killed mid-transfer asks for exactly what it does not have.
//!
//! And S11's: a download whose bytes do not hash is **quarantined, never
//! rendered, never associated**. A pending file is not at the object address
//! and carries a suffix nothing reads, so "never rendered" holds for the whole
//! time it exists; on a mismatch it leaves staging altogether, because a
//! resume that appended to known-bad bytes would fail forever.

use koru_cloud_blob::{BlobCode, wire::ReadCapabilityV1};
use koru_store_core::{WorkspaceId, model::BlobHash};

use crate::{
    allowlist::AdmittedUrl,
    engine::BlobEngine,
    ports::{CloudAccessFacts, LocalBlob, TransferError},
    retry::Disposition,
};

/// Where a download ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadOutcome {
    /// The Workspace already had it. No request was made.
    AlreadyLocal,
    /// Installed and associated, in that order, and announced.
    Arrived { received: u64, total: u64 },
}

/// What a read capability grants, once its URL has been admitted.
struct Grant {
    url: AdmittedUrl,
    capability: String,
    blob: LocalBlob,
}

pub async fn run(
    engine: &BlobEngine,
    access: &CloudAccessFacts,
    workspace: WorkspaceId,
    hash: &BlobHash,
) -> Result<DownloadOutcome, TransferError> {
    if engine
        .store()
        .workspace_blob(workspace, hash.clone())
        .await?
        .is_some()
    {
        return Ok(DownloadOutcome::AlreadyLocal);
    }

    // Asked **before** bytes move. Whether this download happens at all was
    // decided by the pass (D260: a request, a document on screen, or a resume);
    // here it decides order among the ones that do.
    let watched = engine.downloads().watching(workspace, hash);

    // **News, and unconditional.** This device already holds these bytes and
    // this Workspace cannot use them, so a gigabyte is about to cross the
    // network for something that is on the disk. The transfer is still
    // correct, and the cross-Workspace association that would make it
    // unnecessary is B1's; saying so is how anybody finds out it is worth
    // having.
    if engine
        .store()
        .object_present(hash.clone())
        .await
        .unwrap_or(false)
    {
        tracing::info!(
            target: "koru_blob_sync",
            event = "cloud.blob.refetching_a_held_object",
            workspace = %workspace,
            hash = %hash,
        );
    }

    // **§8: the peer first, then Cloud.** A LAN transfer costs neither relay
    // bandwidth nor Cloud egress, and both of those are ours to pay for. The
    // attempt is bounded, falls through on anything at all, and — the part that
    // makes it free rather than a gamble — writes into the same staging file,
    // so whatever it managed is what Cloud then resumes from.
    if let PeerAttempt::Arrived { received, total } = peer_first(engine, workspace, hash).await? {
        return Ok(DownloadOutcome::Arrived { received, total });
    }

    // **§6's fourth point, at the download.** The read capability is the first
    // Cloud request this transfer makes, so the gate goes in front of it — and
    // it is asked again rather than trusted from the pass that ordered this
    // blob.
    engine.admit_cloud(workspace).await?;

    let mut grant = grant(engine, access, workspace, hash).await?;
    let total = grant.blob.size;
    let staging = engine.staging().clone();

    // **Never a remembered number.** The length on the disk is the contiguous
    // durable prefix, whatever this process believed before it died.
    let mut offset = {
        let staging = staging.clone();
        let hash = hash.clone();
        blocking(move || staging.synced_length(&hash)).await?
    };
    // A staging file longer than the object is not a resume point; it is a
    // file this build cannot account for, so it goes to quarantine rather than
    // being truncated to fit.
    if offset > total {
        let staging = staging.clone();
        let hash = hash.clone();
        let location = blocking(move || staging.quarantine(&hash)).await?;
        tracing::warn!(
            target: "koru_blob_sync",
            event = "cloud.blob.staging_longer_than_object",
            location = %location,
        );
        offset = 0;
    }

    // **Before the first range, and once.** A device that cannot fit this
    // picture fails visibly and bounded — a stable code a person can read,
    // its neighbours still reconciling — rather than writing towards a full
    // disk and failing on whichever chunk happened to be the last one.
    let fits = {
        let staging = staging.clone();
        let remaining = total.saturating_sub(offset);
        blocking(move || staging.will_fit(remaining, total)).await?
    };
    if !fits {
        return Err(TransferError::InsufficientStorage);
    }

    if total == 0 {
        // No range exists for an empty file. Materialize its staging object,
        // then use the same hash verification and installation as every file.
        let staging = staging.clone();
        let hash = hash.clone();
        blocking(move || staging.append(&hash, 0, &[])).await?;
    }
    let mut received = 0u64;
    while offset < total {
        let want = engine.chunk().min(total - offset);
        let bytes = match engine
            .objects()
            .read_range(grant.url.clone(), grant.capability.clone(), offset, want)
            .await
        {
            Ok(bytes) => bytes,
            Err(error) => match crate::retry::classify(&error) {
                // A read capability is the authorization for as long as it
                // lives (D252), and it is short on purpose. Asking for another
                // one re-runs every authorization axis, which is the point.
                Disposition::Renew => {
                    grant = self::grant(engine, access, workspace, hash).await?;
                    continue;
                }
                _ => return Err(error),
            },
        };
        let written = {
            let staging = staging.clone();
            let hash = hash.clone();
            // Appended, flushed and **synced** before the next range is asked
            // for. The next request is made on the strength of this length.
            blocking(move || staging.append(&hash, offset, &bytes)).await?
        };
        received += written - offset;
        offset = written;
    }

    // At exact length, hash the whole file. `blake3::Hasher` cannot be
    // serialised, so no per-chunk state could survive the kill this test is
    // about; one read at the end is the honest shape (§4.3's reasoning, from
    // the client's side).
    let matched = {
        let staging = staging.clone();
        let hash = hash.clone();
        blocking(move || staging.hashes_to(&hash)).await?
    };
    if !matched {
        // **S11.** Quarantined, never rendered, never associated.
        let staging = staging.clone();
        let quarantined = hash.clone();
        let location = blocking(move || staging.quarantine(&quarantined)).await?;
        // Unconditional: a checksum mismatch is news about somebody's file.
        tracing::warn!(
            target: "koru_blob_sync",
            event = "cloud.blob.download_hash_mismatch",
            workspace = %workspace,
            hash = %hash,
            location = %location,
        );
        return Err(TransferError::Refused(BlobCode::HashMismatch));
    }

    // **The object, and only then the association.** One call, because the
    // order is the invariant.
    let staged = staging.part_path(hash);
    engine
        .store()
        .install_downloaded(workspace, grant.blob.clone(), staged)
        .await?;
    {
        let staging = staging.clone();
        let hash = hash.clone();
        blocking(move || staging.discard(&hash)).await?;
    }

    // After durability, never before.
    engine.downloads().arrived(workspace, hash);
    tracing::debug!(
        target: "koru_blob_sync",
        event = "cloud.blob.downloaded",
        workspace = %workspace,
        watched,
        received,
    );
    Ok(DownloadOutcome::Arrived { received, total })
}

/// A read capability, its admitted byte URL, and the metadata the association
/// will carry.
///
/// **Issued only for a durable mapping** (§3.5). Possessing a hash is never
/// authority, and neither is having started an upload of one.
async fn grant(
    engine: &BlobEngine,
    access: &CloudAccessFacts,
    workspace: WorkspaceId,
    hash: &BlobHash,
) -> Result<Grant, TransferError> {
    let ReadCapabilityV1 {
        capability,
        byte_url,
        size,
        mime,
        filename,
    } = engine
        .control()
        .read_capability(access.clone(), workspace, hash.clone())
        .await?;
    if !koru_cloud_blob::valid_blob_size(hash.as_str(), size) {
        return Err(TransferError::Refused(BlobCode::LengthMismatch));
    }
    Ok(Grant {
        // A control response is not authority to fetch a person's file from
        // anywhere either: the same clause, read in the other direction.
        url: access
            .byte_origins
            .admit(&byte_url)
            .map_err(TransferError::Origin)?,
        capability,
        blob: LocalBlob {
            hash: hash.clone(),
            size,
            mime,
            filename,
        },
    })
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, TransferError> + Send + 'static,
) -> Result<T, TransferError> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or(Err(TransferError::Local(
            koru_store_core::errors::Code::Internal,
        )))
}

/// How far a peer attempt may run before the pass gives up on it and goes to
/// Cloud.
///
/// **Derived, not chosen**: one capability lifetime (§12, D250). A peer attempt
/// that outlived the window a Cloud request is admitted for would be a stall
/// dressed up as an optimisation, and this is the only other bound in the file
/// measured in the same units.
pub const PEER_ATTEMPT_WINDOW: std::time::Duration =
    std::time::Duration::from_secs(koru_cloud_blob::CAPABILITY_LIFETIME_SECS as u64);

/// Where the peer arm of a download ended.
enum PeerAttempt {
    /// The whole file arrived from a peer, was verified, installed and
    /// announced. Cloud is never asked.
    Arrived { received: u64, total: u64 },
    /// Nothing, or not all of it. Whatever did arrive is on the disk in the
    /// staging file, and Cloud resumes from there rather than from zero.
    FellThrough,
}

/// **§8's candidate ordering**: an authorized peer on a direct path, then Cloud.
///
/// Three things make this cheap to be wrong about, which is why it is written
/// this way:
///
/// - it falls through on **anything** — no peer, a peer that stopped, a short
///   read, a mismatched length, the window elapsing;
/// - it writes into the staging file keyed by hash, so the fall-through keeps
///   every byte it managed and Cloud asks only for what is missing;
/// - it verifies the hash before installing, so a peer that served the wrong
///   bytes costs a quarantine rather than a wrong picture.
async fn peer_first(
    engine: &BlobEngine,
    workspace: WorkspaceId,
    hash: &BlobHash,
) -> Result<PeerAttempt, TransferError> {
    // **§6's second point on this arm.** The pass already established that
    // Cloud is admitted, and the ladder makes a peer implied by that — but
    // implied is not asked, and this is the call that makes the peer arm obey
    // the same rule the hydration scheduler does. A refusal is a fall-through,
    // not an error: Cloud is the next candidate.
    if !engine.peer_admitted(workspace).await {
        return Ok(PeerAttempt::FellThrough);
    }
    let staging = engine.staging().clone();
    let deadline = std::time::Instant::now() + PEER_ATTEMPT_WINDOW;
    let mut offset = {
        let staging = staging.clone();
        let hash = hash.clone();
        blocking(move || staging.synced_length(&hash)).await?
    };
    let started_at = offset;
    let mut metadata: Option<(u64, String, Option<String>)> = None;

    loop {
        // **The window bounds the whole attempt, not the gaps between reads.**
        // A host may ask several candidates in turn, each with its own
        // round-trip timeout, so a deadline checked only between iterations is
        // one a wedged peer walks straight past.
        let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
            break;
        };
        let want = engine.chunk();
        let chunk = match tokio::time::timeout(
            remaining,
            engine
                .peers()
                .read_range(workspace, hash.clone(), offset, want),
        )
        .await
        {
            Ok(Ok(Some(chunk))) => chunk,
            // No peer holds it, or none is on a path this device will send
            // bytes over. Not a failure: it is the fall-through.
            Ok(Ok(None)) => break,
            // A peer that went away mid-transfer costs a partial staging file
            // and a fallback, not a stall — and the partial file is the point.
            Ok(Err(_)) => break,
            // The window elapsed. The same answer: keep what landed, ask Cloud.
            Err(_) => break,
        };
        if chunk.total == 0 || chunk.total > koru_cloud_blob::MAX_BLOB_SIZE {
            break;
        }
        match metadata {
            // A peer that changed its mind about the size mid-transfer is one
            // this pass stops listening to.
            Some((total, ..)) if total != chunk.total => break,
            Some(_) => {}
            None => metadata = Some((chunk.total, chunk.mime.clone(), chunk.filename.clone())),
        }
        if chunk.bytes.is_empty() {
            break;
        }
        let written = {
            let staging = staging.clone();
            let hash = hash.clone();
            let bytes = chunk.bytes;
            // Appended, flushed and synced, exactly as the Cloud arm does. The
            // length on the disk is the only resume authority either arm has.
            blocking(move || staging.append(&hash, offset, &bytes)).await?
        };
        if written <= offset {
            break;
        }
        offset = written;
        if offset >= chunk.total {
            break;
        }
    }

    let Some((total, mime, filename)) = metadata else {
        return Ok(PeerAttempt::FellThrough);
    };
    if offset < total {
        // Partial, and kept. Cloud will read `synced_length` and ask for the
        // rest — the 300 MB a peer managed is 300 MB Cloud never sends.
        tracing::debug!(
            target: "koru_blob_sync",
            event = "cloud.blob.peer_attempt_fell_through",
            workspace = %workspace,
            carried = offset.saturating_sub(started_at),
        );
        return Ok(PeerAttempt::FellThrough);
    }

    let matched = {
        let staging = staging.clone();
        let hash = hash.clone();
        blocking(move || staging.hashes_to(&hash)).await?
    };
    if !matched {
        // **S11, on the peer arm.** Quarantined rather than truncated: a resume
        // that appended to known-bad bytes would fail forever, and Cloud must
        // start this one from zero.
        let staging = staging.clone();
        let quarantined = hash.clone();
        let location = blocking(move || staging.quarantine(&quarantined)).await?;
        tracing::warn!(
            target: "koru_blob_sync",
            event = "cloud.blob.peer_download_hash_mismatch",
            workspace = %workspace,
            hash = %hash,
            location = %location,
        );
        return Ok(PeerAttempt::FellThrough);
    }

    let staged = staging.part_path(hash);
    engine
        .store()
        .install_downloaded(
            workspace,
            LocalBlob {
                hash: hash.clone(),
                size: total,
                mime,
                filename,
            },
            staged,
        )
        .await?;
    {
        let staging = staging.clone();
        let hash = hash.clone();
        blocking(move || staging.discard(&hash)).await?;
    }
    engine.used_peer_path(workspace);
    engine.downloads().arrived(workspace, hash);
    tracing::debug!(
        target: "koru_blob_sync",
        event = "cloud.blob.downloaded_from_peer",
        workspace = %workspace,
        received = offset.saturating_sub(started_at),
    );
    Ok(PeerAttempt::Arrived {
        received: offset.saturating_sub(started_at),
        total,
    })
}
