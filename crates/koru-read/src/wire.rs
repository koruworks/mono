// SPDX-License-Identifier: MPL-2.0

//! **What the Store daemon's coordinator and the reading worker say to each
//! other** (TECHNICAL §10 "Worker IPC").
//!
//! ```text
//! coordinator -> Request::Status                    first, on every connection
//! worker      -> Event::Status(WorkerStatus)        launch id and protocol checked
//! coordinator -> Request::Read { job, path, mime, from_page,
//!                                max_text_pages, max_read_pages, want }
//! worker      -> Event::Page { job, page, text, source, confidence }   0..n
//!              -> Event::Done { job, status, reason, pages_done }       or
//!                 Event::Failed { job, code }                           exactly one
//! coordinator -> Request::Cancel { job }            at any time
//! ```
//!
//! ## The rules a peer may rely on
//!
//! - **Every `Read` ends with exactly one `Done` or `Failed` for its `job`.** A
//!   `Page` after that is a bug, and the coordinator drops it.
//! - **One job at a time.** A `Read` while another job runs is answered
//!   `Failed { code: Busy }` for the *new* job; the running one continues.
//! - **`Cancel` ends a running job with `Failed { code: Cancelled }`.**
//!   Cancelling a job that has already ended, or was never started, answers
//!   nothing.
//! - **The first message on a connection is `Status`.** The coordinator
//!   compares [`WorkerStatus::supervisor_launch_id`] with the launch it made
//!   and [`WorkerStatus::protocol_version`] with [`PROTOCOL_VERSION`] before it
//!   sends a `Read`, so a stale worker from a previous launch, or one from
//!   another build left behind by an update, is refused by arithmetic.
//!
//! ## A message is a value before it is bytes
//!
//! These travel a `koru-ipc` frame stamped with [`FRAME_VERSION`]; this crate
//! links no socket. The two numbers are different questions — see
//! `koru_voice::wire` for the full argument, which applies here unchanged.
//!
//! ## What never crosses
//!
//! A file's bytes (the worker opens `path` itself, read-only), a file's name
//! (the path is the blob store's content-addressed object, never the person's
//! filename), a workspace, a document or a hash. The worker is a pure service:
//! bytes in, words out, no Store session (D4). Words cross only inside a
//! [`PageText`], whose `Debug` is redacted, and a failure is a code with no
//! message — a message is where a page's words or a path would leak into a log.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::reading::{PageText, ReadingStatus, Reason, Source};

/// **What these messages mean.** Bump when a shape changes in a way an older
/// peer cannot read. 1 is the first.
pub const PROTOCOL_VERSION: u32 = 1;

/// The frame version handed to `koru_ipc::frame`. Changes only if the framing
/// does.
pub const FRAME_VERSION: u8 = 1;

/// One request to read one file, minted by the coordinator.
///
/// Monotonic within a coordinator's run, which is all a correlation id needs:
/// a late answer to a cancelled job carries a number the coordinator has moved
/// past, and is dropped by comparison rather than by vigilance.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub u64);

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

tokens! {
    /// What the coordinator wants from a file.
    Want {
        /// **Only words already in the file**: a PDF's text layer and a
        /// plain-text file. A page that would need recognition is left out,
        /// and the job ends `partial` with no reason. What a computer without
        /// the reader installed asks for — no model, no consent.
        Text => "text",
        /// Words already in the file, **and recognition** where there are
        /// none: pictures, scanned pages, and text layers that fail the
        /// acceptance check. Sent only when the reader is installed.
        Recognize => "recognize",
    }
}

tokens! {
    /// Why a job could not be read **on this computer, this time**.
    ///
    /// Every one of these is a local fact: none is ever written into a reading
    /// document, because another device, or a later attempt, may succeed. The
    /// coordinator records them in its local attempts table for backoff. What
    /// is true on every device — password, damage, a page cap — is a
    /// [`Reason`] on [`Event::Done`] instead.
    FailureCode {
        /// A format this worker cannot decode here (HEIC; recognition on a
        /// platform built without it).
        Unsupported => "unsupported",
        /// `path` could not be opened or read.
        Unreadable => "unreadable",
        /// Recognition was needed and the reader is not installed or did not
        /// verify. The job waits as `needs_reader`.
        NeedsReader => "needs-reader",
        /// Another job is running.
        Busy => "busy",
        /// The coordinator cancelled it.
        Cancelled => "cancelled",
        /// The reader or the worker failed in a way it could report. A crash,
        /// an exceeded memory limit or a page over its time budget reports
        /// nothing: the coordinator sees the process end.
        Internal => "internal",
    }
}

/// What the coordinator asks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// Read one file.
    Read {
        job: JobId,
        /// The blob store's object for these bytes — content-addressed, never
        /// the person's filename. Opened read-only, by the worker.
        path: PathBuf,
        /// The MIME type the manifest recorded. The worker trusts the bytes
        /// over it where they disagree.
        mime: String,
        /// The first page to read, counting from 1 — where a `partial` reading
        /// resumes. 1 for a fresh job, and for every picture and plain file.
        from_page: u32,
        /// At most this many pages have their text layer taken.
        /// [`crate::TEXT_PAGE_CAP`] ordinarily; `None` when a person asked for
        /// every page (*Read all pages*).
        max_text_pages: Option<u32>,
        /// At most this many pages are sent to the reader.
        /// [`crate::READ_PAGE_CAP`] ordinarily; `None` when a person asked for
        /// every page.
        max_read_pages: Option<u32>,
        want: Want,
    },
    /// What are you doing, and who launched you? Answered with
    /// [`Event::Status`]. Starts nothing.
    Status,
    /// Stop this job.
    Cancel { job: JobId },
}

/// What the worker answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// One page's words, as soon as they exist — so a long file becomes
    /// findable page by page, and a worker killed on page 41 has already
    /// delivered forty.
    ///
    /// A page with no words may be sent with empty `text`, as progress; it is
    /// never written into a document.
    Page {
        job: JobId,
        /// Counting from 1. A picture and a plain file are page 1.
        page: u32,
        text: PageText,
        /// `text-layer`, `recognized` or `plain`; never `mixed`.
        source: Source,
        /// `0..=100` for a recognized page; `None` for text.
        confidence: Option<u8>,
    },
    /// The job is over, with an outcome that is true on every device.
    Done {
        job: JobId,
        /// `read`, `empty`, `partial` or `failed`.
        status: ReadingStatus,
        /// With `failed`: `password` or `damaged`. With `partial`: `page-cap`,
        /// or `None` when pages were left because they need the reader.
        reason: Option<Reason>,
        /// The highest page number the worker reached. Where a capped reading
        /// stopped, and what the coordinator writes as `pages_done` for a
        /// `partial`.
        pages_done: u32,
    },
    /// The job is over and nothing about it is true on every device.
    Failed { job: JobId, code: FailureCode },
    /// The answer to [`Request::Status`].
    Status(WorkerStatus),
}

impl Event {
    /// The job this is about, or `None` for a status report.
    pub fn job(&self) -> Option<JobId> {
        match self {
            Event::Page { job, .. } | Event::Done { job, .. } | Event::Failed { job, .. } => {
                Some(*job)
            }
            Event::Status(_) => None,
        }
    }

    /// Whether this ends its job.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Event::Done { .. } | Event::Failed { .. })
    }
}

/// The version of [`WorkerBootstrap`]. A mismatch is refused, not interpreted:
/// the Store and the worker ship in one package and are never mixed, so a
/// disagreement means the wrong executable was started.
pub const BOOTSTRAP_VERSION: u32 = 1;

/// **What the supervising Store writes on the worker's stdin**, as one line of
/// JSON, before anything else (TECHNICAL §10 "Worker launch").
///
/// The worker is started as `koru-read-worker --root <store dir> --supervised`
/// through `koru-agent-worker`, which puts it in its memory-limited job and
/// only then writes this line; the worker reads it with
/// `koru_agent_worker::read_bootstrap_line` before it allocates anything worth
/// counting, and the pipe left open behind it is the parent-death signal.
///
/// Here, in the contract, so the coordinator can write one without linking the
/// worker. Only ids cross: no path, no credential (the root is on the command
/// line, and the endpoint is derived from it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerBootstrap {
    /// [`BOOTSTRAP_VERSION`] as the Store writes it.
    pub bootstrap_version: u32,
    /// Which launch this is. Echoed as [`WorkerStatus::supervisor_launch_id`]
    /// in the answer to the connection's first `Status`, which is how the
    /// coordinator tells the process it launched from any other.
    pub launch_id: String,
    /// The channel segment (`stable`, `nightly`, …). Every endpoint name
    /// carries it, so a worker of another channel would bind a name the Store
    /// never dials; refused at start instead.
    pub channel: String,
}

/// **What the worker says about itself.**
///
/// Every field is an id, a count, a version or a flag — no path, no file's
/// name, no word. It is what `GET /v1/debug/readings` reports about the worker
/// and how the coordinator recognises the process it launched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub worker_version: String,
    /// [`PROTOCOL_VERSION`] as this worker speaks it.
    pub protocol_version: u32,
    pub process_id: u32,
    /// The `launch_id` the supervising Store passed in the bootstrap line, or
    /// `None` for a worker somebody started by hand.
    pub supervisor_launch_id: Option<String>,
    /// Whether this worker can recognize pictures now: built with recognition,
    /// and the reader installed and verified. `false` throughout M1.
    pub recognition: bool,
    /// The job running now, if any.
    pub job: Option<JobId>,
    /// Pages finished since this process started.
    pub pages_read: u64,
}

#[cfg(test)]
mod tests;
