// SPDX-License-Identifier: MPL-2.0

//! **The resumable downloader, which is the one genuinely new mechanism in
//! Plan 1.2.**
//!
//! `koru_update::controller::download_verified` is the shape: an HTTPS-only
//! redirect policy, a `.part` file, a cancel flag polled between chunks, and a
//! size cross-check before a byte is trusted. Everything below is what it does
//! not do, and each gap is a real one — an update package is 60 MB and a model
//! is ten times that, so a connection dropped at 80% is the difference between
//! an annoyance and a feature nobody can install on a train.
//!
//! ## The four gaps, closed
//!
//! **A `.part` keyed by the digest, so it survives a restart.** The update
//! path names its part file after the package *and the process id*, and deletes
//! it on any failure — so a download that fails twice downloads everything
//! twice. Here the name is the digest, which is the only thing that identifies
//! the bytes independently of the mirror they came from, the process that asked
//! or the run it happened in.
//!
//! **`If-Range` with `Content-Range` validation, and explicit 200 / 206 / 416
//! handling** — never trust in `Accept-Ranges`. The status is what says whether
//! a range was honoured, and `Accept-Ranges: bytes` on a response is a claim
//! about the resource that a proxy, a CDN edge or a rewriting middlebox is free
//! to make and then not keep. The three cases are:
//!
//! | status | what it means | what happens |
//! | --- | --- | --- |
//! | `206` | the range was served | `Content-Range` must start at exactly the byte we have, and its total must be the signed size — otherwise the reply is refused rather than appended |
//! | `200` | the range was ignored, or `If-Range` did not match | the partial is discarded and the file is written from zero |
//! | `416` | our partial is longer than the resource now is | the partial is discarded and the whole request is made again without a range |
//!
//! **The 200 case is the one worth stating twice.** A server that answers a
//! ranged request with the whole body and a client that appends produces a file
//! of the right length only by coincidence and of the wrong content always —
//! and the failure surfaces as a digest mismatch after the whole download,
//! which is exactly where nobody looks for a resume bug.
//!
//! **One full re-hash of the completed file.** A streaming `Sha256` cannot be
//! resumed: its state is not in the part file and cannot be reconstructed from
//! it without reading every byte, which is the re-hash. So the hash the update
//! path accumulates while downloading is not available here, and pretending
//! otherwise — hashing only the bytes this run fetched — installs corruption
//! that verifies. The re-hash is [`crate::ModelDescriptor::verify_archive`]'s,
//! run by the store after the fetch returns, and
//! [`InstallStage::Verifying`](crate::InstallStage::Verifying) exists because
//! it is minutes of work on a slow disk.
//!
//! **Progress coalesced** to at most one event per 250 ms or per whole
//! percent, by the `progress::Coalescer` beside this module. The update path
//! emits one per 64 KiB chunk, which over 650 MB is roughly ten thousand
//! events.
//!
//! ## What this does not do
//!
//! It does not verify. A source produces bytes at a path; the signature is
//! checked before it is called and the size and digest after. Splitting it that
//! way is what stops a second source ever being written with a weaker check in
//! it — and it is why a `Content-Length` disagreeing with the signed size is
//! refused here as an *early* failure rather than as the verification.

use crate::descriptor::ModelDescriptor;
use crate::error::{ModelError, ModelErrorCode};
use crate::progress::Coalescer;
use crate::source::ModelSource;
use crate::store::{InstallProgress, InstallStage};
use reqwest::blocking::{Client, Response};
// `Accept-Ranges` is deliberately absent: the module note says why the status
// is what a resume branches on, and a header imported here is a header
// somebody would eventually branch on.
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, ETAG, IF_RANGE, LAST_MODIFIED, RANGE};
use reqwest::{StatusCode, redirect};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use url::Url;

/// How long a model download may take before the client gives up. Thirty
/// minutes, the same as `download_verified`'s, and for the same reason: a
/// shorter one is a failure on a slow link rather than a protection against
/// anything.
const TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// 64 KiB, matching `koru_update::sha256_file` and `download_verified`. It is
/// the read size, not the progress interval — those were the same number in the
/// update path, which is the whole of that bug.
const CHUNK: usize = 64 * 1024;

/// What a resumed request has to prove the resource has not changed under it.
///
/// Persisted beside the `.part` because it has to survive the process, which is
/// the point of a part file keyed by digest. `url` is recorded for diagnosis
/// only: a partial fetched from one mirror is perfectly resumable from another,
/// since what identifies the bytes is the digest.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ResumeState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_modified: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

impl ResumeState {
    fn read(path: &Path) -> ResumeState {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn write(&self, path: &Path) {
        // Best effort: losing this costs a re-download, and failing an install
        // over it would cost the download as well.
        if let Ok(bytes) = serde_json::to_vec(self) {
            let _ = fs::write(path, bytes);
        }
    }

    /// What `If-Range` carries. An ETag is preferred because `Last-Modified`
    /// has one-second resolution, and a resource rewritten inside its own
    /// second is indistinguishable from one that was not.
    fn validator(&self) -> Option<&str> {
        self.etag.as_deref().or(self.last_modified.as_deref())
    }
}

/// Whether an attempt failed in a way a second attempt could fix.
enum Attempt {
    Done,
    /// The partial was discarded; ask again with no range. Reached by `416`
    /// and by a `206` whose validator changed under us.
    StartOver,
}

/// **Fetches a model archive over HTTPS, resuming what a previous run left.**
pub struct HttpSource {
    client: Client,
}

impl HttpSource {
    /// The same redirect policy `download_verified` uses: at most five hops and
    /// never off HTTPS, with the debug-build loopback exception `koru-update`
    /// and [`crate::catalog`] both already carry. Copying the *exception* is
    /// the point — a stricter rule here would make the fixture server
    /// unbuildable and would be relaxed under pressure into something looser
    /// than this.
    pub fn new() -> Result<HttpSource, ModelError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(TIMEOUT)
            .redirect(redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 || !scheme_allowed(attempt.url()) {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|e| {
                ModelErrorCode::DownloadFailed.with(format!("could not build an HTTP client: {e}"))
            })?;
        Ok(HttpSource { client })
    }

    /// One request, and everything that follows from its status.
    #[allow(clippy::too_many_arguments)]
    fn attempt(
        &self,
        url: &Url,
        descriptor: &ModelDescriptor,
        part: &Path,
        meta: &Path,
        progress: &mut dyn FnMut(InstallProgress),
        cancel: &AtomicBool,
    ) -> Result<Attempt, ModelError> {
        let total = descriptor.size;
        let mut have = part.metadata().map(|m| m.len()).unwrap_or(0);
        if have > total {
            // Longer than the thing it claims to be. It cannot be a prefix of
            // the signed bytes, so nothing is lost by starting again — and
            // resuming from it would ask for a range past the end and get a
            // 416 anyway.
            discard(part, meta);
            have = 0;
        }
        let mut state = ResumeState::read(meta);

        let mut request = self.client.get(url.clone());
        if have > 0 {
            request = request.header(RANGE, format!("bytes={have}-"));
            if let Some(validator) = state.validator() {
                request = request.header(IF_RANGE, validator);
            }
        }
        let mut response = request.send().map_err(|e| {
            ModelErrorCode::DownloadFailed.with(format!("{url} could not be reached: {e}"))
        })?;

        let served_etag = header(&response, ETAG);
        let served_modified = header(&response, LAST_MODIFIED);

        let append = match response.status() {
            StatusCode::PARTIAL_CONTENT => {
                // A server that changed the resource and still answered 206 is
                // broken, and appending to a stale prefix is how that becomes
                // silent corruption. Discard and ask again without a range.
                if let (Some(known), Some(served)) = (state.etag.as_deref(), served_etag.as_deref())
                    && known != served
                {
                    discard(part, meta);
                    return Ok(Attempt::StartOver);
                }
                let range = header(&response, CONTENT_RANGE).ok_or_else(|| {
                    ModelErrorCode::DownloadFailed
                        .with(format!("{url} answered 206 with no Content-Range"))
                })?;
                let (start, declared_total) = parse_content_range(&range).ok_or_else(|| {
                    ModelErrorCode::DownloadFailed.with(format!(
                        "{url} answered an unreadable Content-Range: {range:?}"
                    ))
                })?;
                if start != have {
                    return Err(ModelErrorCode::DownloadFailed.with(format!(
                        "{url} answered a range starting at {start}, not at {have}"
                    )));
                }
                if let Some(declared) = declared_total
                    && declared != total
                {
                    return Err(ModelErrorCode::DownloadFailed.with(format!(
                        "{url} says the resource is {declared} bytes, the descriptor says {total}"
                    )));
                }
                true
            }
            StatusCode::OK => {
                // Either the server ignores ranges, or `If-Range` did not
                // match. Both mean the same thing: what is on disk is not a
                // prefix of what is arriving, and appending would produce a
                // file of the right length and the wrong content.
                if let Some(length) =
                    header(&response, CONTENT_LENGTH).and_then(|value| value.parse::<u64>().ok())
                    && length != total
                {
                    return Err(ModelErrorCode::SizeMismatch.with(format!(
                        "{url} declares {length} bytes, the descriptor says {total}"
                    )));
                }
                have = 0;
                false
            }
            StatusCode::RANGE_NOT_SATISFIABLE => {
                discard(part, meta);
                return Ok(Attempt::StartOver);
            }
            status => {
                return Err(ModelErrorCode::DownloadFailed.with(format!("{url} answered {status}")));
            }
        };

        if let Some(parent) = part.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(part)?;
        // **The truncation happens before the new validator is recorded, and
        // the order is load-bearing.** Written the other way round, a process
        // that died in between would leave a `.part` holding the *old*
        // resource's bytes beside a `.part.json` naming the *new* resource's
        // `ETag` — and the next run would resume with an `If-Range` the server
        // honours, appending new bytes to a stale prefix. The whole download
        // would then verify as the wrong length or hash the wrong thing, and
        // the cause would be a crash nobody reproduced.
        if append {
            file.seek(SeekFrom::Start(have))?;
        } else {
            file.set_len(0)?;
            file.sync_all()?;
            file.seek(SeekFrom::Start(0))?;
        }

        // Recorded before the body arrives rather than after it, because a run
        // that dies mid-stream still has to leave a `.part` and a validator
        // that describe each other.
        if !append {
            state = ResumeState {
                etag: served_etag,
                last_modified: served_modified,
                url: Some(url.to_string()),
            };
            state.write(meta);
        } else if state.etag.is_none() && served_etag.is_some() {
            state.etag = served_etag;
            state.write(meta);
        }

        let mut coalescer = Coalescer::new();
        progress(InstallProgress {
            stage: InstallStage::Downloading,
            received: have,
            total,
        });
        coalescer.emitted(have, total);

        let mut received = have;
        let mut buffer = vec![0u8; CHUNK];
        let outcome = loop {
            if cancel.load(Ordering::Acquire) {
                break Err(ModelErrorCode::Cancelled.with("the download was cancelled"));
            }
            let read = match response.read(&mut buffer) {
                Ok(0) => break Ok(()),
                Ok(read) => read,
                Err(e) => {
                    // A connection that ended early. The bytes already written
                    // are a real prefix and are kept — that is what makes the
                    // next attempt a resume rather than a restart.
                    break Err(ModelErrorCode::DownloadFailed
                        .with(format!("{url} stopped after {received} bytes: {e}")));
                }
            };
            received = received
                .checked_add(read as u64)
                .ok_or_else(|| ModelErrorCode::DownloadFailed.with("byte-count overflow"))?;
            if received > total {
                // Refused before it is written: a server sending more than the
                // signed size is not serving these bytes, and letting it fill a
                // disk while we find that out is the failure nobody budgets for.
                break Err(ModelErrorCode::SizeMismatch
                    .with(format!("{url} sent more than the signed {total} bytes")));
            }
            file.write_all(&buffer[..read])?;
            if coalescer.wants(received, total) {
                progress(InstallProgress {
                    stage: InstallStage::Downloading,
                    received,
                    total,
                });
            }
        };
        // Whatever happened, what was written is written: a partial that is not
        // on the disk is not a resume.
        file.flush()?;
        file.sync_all()?;
        drop(file);
        outcome?;

        if received != total {
            return Err(ModelErrorCode::DownloadFailed.with(format!(
                "{url} delivered {received} of {total} bytes; the partial is kept for a resume"
            )));
        }
        progress(InstallProgress {
            stage: InstallStage::Downloading,
            received,
            total,
        });
        Ok(Attempt::Done)
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
        if !scheme_allowed(url) {
            return Err(ModelErrorCode::DownloadFailed.with(format!("{url} must use HTTPS")));
        }
        if cancel.load(Ordering::Acquire) {
            return Err(ModelErrorCode::Cancelled.with("cancelled before the request was made"));
        }
        let parent = destination.parent().ok_or_else(|| {
            ModelErrorCode::Io.with(format!("{} has no parent", destination.display()))
        })?;
        fs::create_dir_all(parent)?;
        let (part, meta) = part_paths(parent, descriptor);

        // Two attempts, never more: the second exists for the one case a first
        // cannot answer — a partial the server has outlived — and a loop would
        // be a retry policy, which belongs to the store that knows about
        // mirrors rather than to one request.
        for _ in 0..2 {
            match self.attempt(url, descriptor, &part, &meta, progress, cancel)? {
                Attempt::Done => {
                    // The archive appears complete only at this instant, which
                    // is what stops a later read mistaking a partial for one.
                    if destination.exists() {
                        fs::remove_file(destination)?;
                    }
                    fs::rename(&part, destination)?;
                    let _ = fs::remove_file(&meta);
                    return Ok(());
                }
                Attempt::StartOver => continue,
            }
        }
        Err(ModelErrorCode::DownloadFailed.with(format!("{url} could not serve the whole archive")))
    }
}

/// The `.part` and its resume state, both named by the digest so that a
/// restart, a different mirror and a different process all find the same one.
fn part_paths(parent: &Path, descriptor: &ModelDescriptor) -> (PathBuf, PathBuf) {
    let digest = descriptor.digest.as_str();
    (
        parent.join(format!("{digest}.part")),
        parent.join(format!("{digest}.part.json")),
    )
}

fn discard(part: &Path, meta: &Path) {
    let _ = fs::remove_file(part);
    let _ = fs::remove_file(meta);
}

fn header(response: &Response, name: reqwest::header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// `bytes 40-99/100` → `(40, Some(100))`; `bytes 40-99/*` → `(40, None)`.
///
/// Returns `None` for anything it cannot read, and the caller refuses rather
/// than guessing — see the module note about `Accept-Ranges`.
fn parse_content_range(value: &str) -> Option<(u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (start, _end) = range.trim().split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let total = match total.trim() {
        "*" => None,
        value => Some(value.parse::<u64>().ok()?),
    };
    Some((start, total))
}

/// HTTPS, or loopback in a debug build. The same rule in the same words as
/// [`crate::catalog`]'s and `koru_update::client`'s.
fn scheme_allowed(url: &Url) -> bool {
    if url.scheme() == "https" {
        return true;
    }
    cfg!(debug_assertions) && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
}

#[cfg(test)]
mod tests;
