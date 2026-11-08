// SPDX-License-Identifier: MPL-2.0

//! `KORU_STORE_STATS=1` — where the store's time went.
//!
//! The same shape as `koru-workspace-host`'s `searchdebug.rs` and `koru-core`'s
//! `timing.rs`: one environment variable, read once, and **free when off**. A
//! [`Span`] that is not enabled holds `None` and its `stop` is a branch on an
//! `Option`.
//!
//! ## What it is for
//!
//! Not benchmarking — for that there are the tests in
//! `apps/koru-store-server/tests/perf.rs`, which assert budgets. This is for the
//! question those cannot answer: *on this machine, with this workspace, where
//! did the time actually go.* A budget test says a number is too big; this says
//! which number.
//!
//! ## Metadata only
//!
//! A span carries a static name and a duration. **No document text, no title,
//! no filename** — this output is exactly the kind of thing that ends up pasted
//! into an issue, and the rule that applies to `/v1/debug/*` applies here.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| koru_env::switch(koru_env::name::KORU_STORE_STATS))
}

/// A measured stretch of work.
///
/// ```ignore
/// let span = Span::start("commit");
/// // ...
/// span.stop();
/// ```
///
/// Costs an `Option` and a branch when the flag is off.
pub struct Span {
    name: &'static str,
    at: Option<Instant>,
}

impl Span {
    pub fn start(name: &'static str) -> Self {
        Self {
            name,
            at: enabled().then(Instant::now),
        }
    }

    pub fn stop(self) {
        if let Some(at) = self.at {
            record(self.name, at.elapsed());
        }
    }

    /// Stop and add a note — a count, a size. **Never content.**
    pub fn stop_with(self, note: impl FnOnce() -> String) {
        if let Some(at) = self.at {
            let taken = at.elapsed();
            record(self.name, taken);
            eprintln!("  {} {:>8.2}ms  {}", self.name, ms(taken), note());
        }
    }
}

/// The nine measurements the store is judged on.
///
/// Named constants rather than free strings so a typo cannot create a tenth
/// bucket that nobody notices is empty.
pub mod names {
    pub const EDIT_TO_RECEIVE: &str = "edit -> store receive";
    pub const EDIT_TO_ACK: &str = "edit -> durable ack";
    pub const API_REQUEST: &str = "api request";
    pub const DOCUMENT_OPEN: &str = "document open";
    pub const WORKSPACE_OPEN: &str = "workspace open";
    pub const AUTHORITY_READ: &str = "authority read";
    pub const CHECKPOINT: &str = "checkpoint";
    pub const STORE_STARTUP: &str = "store startup";
    pub const COMMIT: &str = "commit";
    pub const REPLAY: &str = "journal replay";

    pub const ALL: &[&str] = &[
        EDIT_TO_RECEIVE,
        EDIT_TO_ACK,
        API_REQUEST,
        DOCUMENT_OPEN,
        WORKSPACE_OPEN,
        AUTHORITY_READ,
        CHECKPOINT,
        STORE_STARTUP,
        COMMIT,
        REPLAY,
    ];
}

#[derive(Default, Clone, Copy)]
struct Bucket {
    count: u64,
    total: Duration,
    worst: Duration,
}

static BUCKETS: OnceLock<std::sync::Mutex<Vec<(&'static str, Bucket)>>> = OnceLock::new();

fn buckets() -> &'static std::sync::Mutex<Vec<(&'static str, Bucket)>> {
    BUCKETS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

pub fn record(name: &'static str, taken: Duration) {
    if !enabled() {
        return;
    }
    let Ok(mut all) = buckets().lock() else {
        return;
    };
    match all.iter_mut().find(|(other, _)| *other == name) {
        Some((_, bucket)) => {
            bucket.count += 1;
            bucket.total += taken;
            bucket.worst = bucket.worst.max(taken);
        }
        None => all.push((
            name,
            Bucket {
                count: 1,
                total: taken,
                worst: taken,
            },
        )),
    }
}

/// Print what has accumulated, and clear it.
///
/// **ASCII only.** Windows PowerShell 5.1 reads a BOM-less stream as ANSI, and
/// a box-drawing character arrives as three cp1252 bytes — the same trap
/// `koru-core::timing` records for its table.
pub fn report(what: &str) {
    if !enabled() {
        return;
    }
    let Ok(mut all) = buckets().lock() else {
        return;
    };
    if all.is_empty() {
        return;
    }
    all.sort_by(|a, b| b.1.total.cmp(&a.1.total));

    eprintln!();
    eprintln!("{what}");
    eprintln!(
        "  {:<24} {:>6} {:>10} {:>10} {:>10}",
        "what", "n", "total", "mean", "worst"
    );
    eprintln!(
        "  {:-<24} {:->6} {:->10} {:->10} {:->10}",
        "", "", "", "", ""
    );
    for (name, bucket) in all.iter() {
        eprintln!(
            "  {:<24} {:>6} {:>9.2}ms {:>9.2}ms {:>9.2}ms",
            name,
            bucket.count,
            ms(bucket.total),
            ms(bucket.total) / bucket.count as f64,
            ms(bucket.worst),
        );
    }
    all.clear();
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests;
