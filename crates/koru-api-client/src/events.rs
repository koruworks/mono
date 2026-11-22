// SPDX-License-Identifier: MPL-2.0

//! `client.events()` — the events namespace: publish, read, subscribe.
//!
//! The bus is cross-process by construction: every process that can open a
//! [`crate::Client`] — koru.exe, the Agent, koru-mcp, an external tool — uses
//! exactly this surface, riding the same gateway session and renewal the
//! store calls do. The model (`koru_events`) validates before a byte moves;
//! the worker validates again on the other side, because the wire is a
//! public boundary.
//!
//! **Delivery semantics, plainly:** [`EventsClient::on`] is at-least-once
//! *within the process lifetime* — a background thread tails the SSE stream,
//! resumes reconnects gap-free through its cursor, and loses its place only
//! when the process dies. Anything needing durable resumption uses
//! [`EventsClient::read`] with a cursor the caller persists. The handler runs
//! on the subscription's thread, one delivery at a time and in cursor order;
//! forwarding into a UI loop follows the house "send first, wake second"
//! rule, and a handler panic kills only its subscription.

use std::io::{BufRead as _, BufReader};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use koru_events::wire::{
    ActivityPage, AppendReceipt, ConsentBody, EventPage, EventsStatus, MAX_ACTIVITY_LIMIT,
    StoredEvent,
};
use koru_events::{Draft, EmitContext, Envelope};
use koru_store_core::errors::{Code, Error, Result};

use crate::Client;

/// One application run, one context: the session id every event this process
/// sends carries. Process-global on purpose — two `Client`s in one window are
/// still one session.
fn process_context() -> &'static EmitContext {
    static CONTEXT: OnceLock<EmitContext> = OnceLock::new();
    CONTEXT.get_or_init(EmitContext::user_session)
}

/// `KORU_EVENTS_DEBUG`'s sender half — free when off.
fn debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| koru_env::switch(koru_env::name::KORU_EVENTS_DEBUG))
}

/// A journal position. Totally ordered per journal; `0` is "from the top".
pub type Cursor = u64;

/// What a subscription or read is interested in. Minimal for v1: one name
/// prefix (`"document."` matches `document.*`), one workspace. Consumers
/// filter finer in the handler; widening this later is additive.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub name_prefix: Option<String>,
    pub workspace: Option<String>,
}

impl Filter {
    pub fn all() -> Self {
        Self::default()
    }

    pub fn names(prefix: impl Into<String>) -> Self {
        Self {
            name_prefix: Some(prefix.into()),
            workspace: None,
        }
    }

    pub fn workspace(mut self, workspace: impl ToString) -> Self {
        self.workspace = Some(workspace.to_string());
        self
    }

    fn query(&self, after: Cursor, limit: usize) -> String {
        let mut query = format!("/v1/events?after={after}&limit={limit}");
        if let Some(prefix) = &self.name_prefix {
            query.push_str(&format!("&name={}", urlencode(prefix)));
        }
        if let Some(workspace) = &self.workspace {
            query.push_str(&format!("&workspace={}", urlencode(workspace)));
        }
        query
    }
}

/// What a send answers: the id that will dedupe forever, and where the event
/// landed. A duplicate is success — the journal already had it.
#[derive(Clone, Debug)]
pub struct Receipt {
    pub event_id: koru_events::EventId,
    pub cursor: Cursor,
    pub duplicate: bool,
}

pub struct EventsClient<'a> {
    pub(crate) client: &'a Client,
}

impl EventsClient<'_> {
    /// Capability-gated like `ai()`: false against a gateway that opened no
    /// events worker.
    pub fn available(&self) -> bool {
        self.client
            .capabilities()
            .iter()
            .any(|capability| capability == "events")
    }

    /// Publish one event. Blocking; `Ok` means the journal committed it.
    /// The envelope is validated here first so a mistake fails at the call
    /// site with a nameable reason, not as a rejected row in a receipt.
    pub fn send(&self, event: impl Into<Draft>) -> Result<Receipt> {
        let envelope = event.into().into_envelope(process_context());
        self.send_envelope(&envelope)
    }

    /// Publish an already-built envelope — the retry path: a resend under the
    /// same `event_id` is deduplicated, never doubled.
    pub fn send_envelope(&self, envelope: &Envelope) -> Result<Receipt> {
        koru_events::validate(envelope)
            .map_err(|violation| Error::new(Code::InvalidRequest, violation.to_string()))?;
        let receipt: AppendReceipt = self.client.store().post_json(
            "/v1/events",
            &koru_events::wire::EventBatch {
                events: vec![envelope.clone()],
            },
        )?;
        if let Some(rejected) = receipt.rejected.first() {
            return Err(Error::new(
                Code::InvalidRequest,
                format!("{}: {}", rejected.code, rejected.message),
            ));
        }
        if debug_enabled() {
            eprintln!(
                "[Koru][events] sent name={} id={} occurred_at_ms={}{}",
                envelope.name,
                envelope.event_id,
                envelope.occurred_at_ms,
                if receipt.duplicates > 0 {
                    " duplicate"
                } else {
                    ""
                },
            );
        }
        Ok(Receipt {
            event_id: envelope.event_id,
            cursor: receipt.last_seq,
            duplicate: receipt.duplicates > 0,
        })
    }

    /// Publish a batch in one round trip, order preserved. The raw receipt
    /// comes back whole: per-event rejections are data here, not an error,
    /// because the rest of the batch landed.
    pub fn send_batch(&self, events: Vec<Draft>) -> Result<AppendReceipt> {
        let ctx = process_context();
        let envelopes: Vec<Envelope> = events
            .into_iter()
            .map(|draft| draft.into_envelope(ctx))
            .collect();
        self.client.store().post_json(
            "/v1/events",
            &koru_events::wire::EventBatch { events: envelopes },
        )
    }

    /// One page of the journal after a cursor — the durable-consumer pull
    /// API. The caller owns and persists the cursor; replaying from any past
    /// one is the at-least-once story.
    pub fn read(&self, after: Option<Cursor>, filter: &Filter, limit: usize) -> Result<EventPage> {
        self.client
            .store()
            .get_json(&filter.query(after.unwrap_or(0), limit.clamp(1, 1_000)))
    }

    /// **One subject's activity, bounded and paged** — the read behind a
    /// task's Activity pane (SPEC §9). Never a journal scan: the worker seeks
    /// an index on the subject, so the cost is the page.
    ///
    /// Deliberately *not* [`read`](Self::read) with a filter. That one filters
    /// a journal walk by name and workspace and answers whole envelopes; this
    /// one is addressed by subject and answers the projection a pane draws —
    /// no device id, no profile id, no session. Widening `Filter` to carry a
    /// subject would have made the expensive shape reachable from the pane's
    /// call site, which is the mistake SPEC §9 is written against.
    ///
    /// Authorization is the gateway bearer this client already rides — the one
    /// trusted user with peered devices (SPEC §10). There is no task scope.
    ///
    /// `after: None` starts at the beginning of what the journal still holds.
    /// The page carries `retention_days`, which is what lets a caller tell
    /// *nothing happened* from *it happened longer ago than this device keeps*.
    pub fn activity(
        &self,
        subject: koru_events::SubjectKind,
        id: &str,
        after: Option<Cursor>,
        limit: usize,
    ) -> Result<ActivityPage> {
        let after = after.unwrap_or(0);
        let limit = (limit as u32).clamp(1, MAX_ACTIVITY_LIMIT);
        self.client.store().get_json(&format!(
            "/v1/events/activity?kind={}&id={}&after={after}&limit={limit}",
            subject.token(),
            urlencode(id),
        ))
    }

    /// The worker's own counters — what it is doing, never content.
    pub fn status(&self) -> Result<EventsStatus> {
        self.client.store().get_json("/v1/events/status")
    }

    /// Record this device's telemetry answer — the only thing that allows an
    /// export (the opt-in contract, ADR-176-consenton's 2026-10-11 amendment).
    /// Blocking: call it off the UI thread. A `no` deletes whatever was
    /// queued; a `yes` starts from an empty queue, so nothing recorded before
    /// it is ever uploaded. Answers with what the worker now holds.
    pub fn set_telemetry_consent(&self, granted: bool) -> Result<ConsentBody> {
        self.client.store().put_json(
            "/v1/events/telemetry/consent",
            &ConsentBody {
                granted,
                consent_version: koru_events::telemetry::DEFAULT_CONSENT_VERSION.to_string(),
            },
        )
    }

    /// Live tail on a named background thread. `from: None` starts at "now";
    /// `Some(cursor)` replays forward first. See the module doc for the
    /// delivery semantics. Dropping the [`Subscription`] signals the thread
    /// and detaches; [`Subscription::close`] joins it.
    pub fn on(
        &self,
        from: Option<Cursor>,
        filter: Filter,
        handler: impl FnMut(StoredEvent) + Send + 'static,
    ) -> Result<Subscription> {
        self.on_with_reconnect(from, filter, handler, || {})
    }

    /// As `on`, with one content-free invalidation after a failed stream
    /// reconnects. Quiet, healthy stream renewals do not invoke it.
    pub fn on_with_reconnect(
        &self,
        from: Option<Cursor>,
        filter: Filter,
        mut handler: impl FnMut(StoredEvent) + Send + 'static,
        mut reconnected: impl FnMut() + Send + 'static,
    ) -> Result<Subscription> {
        let start = match from {
            Some(cursor) => cursor,
            None => self.status()?.last_seq,
        };
        let store = self.client.store().clone();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let thread_stop = std::sync::Arc::clone(&stop);
        let join = std::thread::Builder::new()
            .name("koru-events".into())
            .spawn(move || {
                let mut cursor = start;
                let mut backoff = std::time::Duration::from_millis(250);
                let mut interrupted = false;
                while !thread_stop.load(Ordering::Relaxed) {
                    if interrupted
                        && let Ok(status) =
                            store.get_public_json::<EventsStatus>("/v1/events/status")
                    {
                        if status.last_seq < cursor {
                            cursor = status.last_seq;
                        }
                    }
                    match tail_once(
                        &store,
                        &filter,
                        cursor,
                        &thread_stop,
                        &mut handler,
                        &mut || {
                            if interrupted {
                                reconnected();
                                interrupted = false;
                            }
                        },
                    ) {
                        Ok(reached) => {
                            cursor = reached;
                            // A stream that ended at its deadline is routine;
                            // reconnect immediately and quietly.
                            backoff = std::time::Duration::from_millis(250);
                        }
                        Err(_) if thread_stop.load(Ordering::Relaxed) => break,
                        Err(_) => {
                            interrupted = true;
                            // The send_with_retry engine already renewed what
                            // it could; what is left is a store restarting.
                            std::thread::sleep(backoff);
                            backoff = (backoff * 2).min(std::time::Duration::from_secs(5));
                        }
                    }
                }
            })
            .map_err(|error| Error::new(Code::Internal, error.to_string()))?;
        Ok(Subscription {
            stop,
            join: Some(join),
        })
    }
}

// --------------------------------------------------------------- the catalog
//
// **Invalidate on a committed fact; fetch when a surface opens.** No cache, no
// poller, no second copy of the catalog — UIREFACTOR R3. A window subscribes
// once, and thereafter a menu about to open asks whether what it holds is
// stale and refetches through `client.store()` if it is.

/// Which half of the catalog a fact touched.
///
/// Two, because there are two routes a surface refetches: `GET /v1/profiles`
/// and `GET /v1/workspaces` (`?include_deleted=true` where a manager needs the
/// lifecycle states). A finer split would be a cache invalidation scheme, and
/// this is deliberately not one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum CatalogChange {
    Profiles,
    Workspaces,
}

/// Which half — if either — an event name invalidates.
///
/// Name-prefixed rather than enumerated, so a registry row added later
/// invalidates without a second table to keep in step. `workspace.*` covers the
/// Store's `workspace.catalog.*` catalog facts and the app's own
/// `workspace.created`/`observed`/`deleted`; `profile.*` covers the profile
/// rows. Everything else — documents, search, voice, terminal — answers `None`,
/// which is how a busy bus stays cheap for a menu that only cares about two
/// lists.
pub fn catalog_change(event_name: &str) -> Option<CatalogChange> {
    if event_name.starts_with("profile.") {
        Some(CatalogChange::Profiles)
    } else if event_name.starts_with("workspace.") {
        Some(CatalogChange::Workspaces)
    } else {
        None
    }
}

/// What a window holds: two flags, set by the subscription's thread and taken
/// by whatever is about to draw a menu.
///
/// **The flag is sticky until it is taken.** A surface that is closed when the
/// event arrives still finds it set when it opens, which is the whole point of
/// invalidate-on-event/fetch-on-open: a fact must not be missed because nobody
/// was looking.
#[derive(Debug, Default)]
pub struct CatalogFlags {
    profiles: AtomicBool,
    workspaces: AtomicBool,
}

impl CatalogFlags {
    fn cell(&self, which: CatalogChange) -> &AtomicBool {
        match which {
            CatalogChange::Profiles => &self.profiles,
            CatalogChange::Workspaces => &self.workspaces,
        }
    }

    /// Mark one half stale. Public because a window that mutated the catalog
    /// itself may invalidate its own view without waiting for the round trip —
    /// and because a reconnect that lost its place is honestly reported by
    /// invalidating both.
    pub fn invalidate(&self, which: CatalogChange) {
        self.cell(which).store(true, Ordering::Relaxed);
    }

    /// Is this half stale? Does not clear it — for a surface deciding whether
    /// to show a quiet refreshing state.
    pub fn is_stale(&self, which: CatalogChange) -> bool {
        self.cell(which).load(Ordering::Relaxed)
    }

    /// **The fetch-on-open call.** Clears the flag and answers whether it was
    /// set, so a menu opening does exactly one refetch per batch of facts and a
    /// menu opening twice with nothing in between does none.
    pub fn take(&self, which: CatalogChange) -> bool {
        self.cell(which).swap(false, Ordering::Relaxed)
    }
}

/// A live catalog subscription and the flags it sets.
///
/// Hold it for as long as the window lives; dropping it stops the tail. The
/// flags outlive it deliberately — they are an `Arc`, so a surface can hold one
/// without holding the subscription.
pub struct CatalogWatch {
    flags: std::sync::Arc<CatalogFlags>,
    subscription: Subscription,
}

impl CatalogWatch {
    pub fn flags(&self) -> std::sync::Arc<CatalogFlags> {
        std::sync::Arc::clone(&self.flags)
    }

    /// Clears and answers, without going through [`Self::flags`].
    pub fn take(&self, which: CatalogChange) -> bool {
        self.flags.take(which)
    }

    /// Stop and wait — bounded by the stream deadline, like
    /// [`Subscription::close`].
    pub fn close(self) {
        self.subscription.close();
    }
}

impl EventsClient<'_> {
    /// Watch the catalog: invalidate on a committed fact, fetch on open.
    ///
    /// Starts at *now*, not at the top of the journal — a window has just
    /// fetched, so replaying history would only tell it to refetch what it
    /// already has. `wake` runs on the subscription's thread, one delivery at a
    /// time; it exists so a window can request a repaint (the house "send
    /// first, wake second" rule) and it must not block. The flag is already set
    /// before `wake` is called, so a window that only ever refetches on open
    /// may pass a closure that does nothing.
    ///
    /// The subscription is unfiltered on purpose: [`Filter`] carries one name
    /// prefix and this needs two, and filtering in the handler costs a string
    /// compare against a stream a desktop machine could not saturate.
    pub fn watch_catalog(
        &self,
        mut wake: impl FnMut(CatalogChange) + Send + 'static,
    ) -> Result<CatalogWatch> {
        let flags = std::sync::Arc::new(CatalogFlags::default());
        let theirs = std::sync::Arc::clone(&flags);
        let subscription = self.on(None, Filter::all(), move |stored| {
            if let Some(change) = catalog_change(stored.envelope.name.as_str()) {
                theirs.invalidate(change);
                wake(change);
            }
        })?;
        Ok(CatalogWatch {
            flags,
            subscription,
        })
    }
}

/// Hold one SSE connection until it ends (deadline, error, or stop), feeding
/// the handler and returning the cursor reached.
fn tail_once(
    store: &koru_store_client::Client,
    filter: &Filter,
    from: Cursor,
    stop: &AtomicBool,
    handler: &mut (impl FnMut(StoredEvent) + Send),
    connected: &mut impl FnMut(),
) -> Result<Cursor> {
    // The deadline outlives the worker's 20 s keep-alive comfortably, so a
    // quiet-but-healthy stream is cut and resumed rather than misread as
    // dead — and it is what bounds `Subscription::close`.
    const STREAM_DEADLINE: std::time::Duration = std::time::Duration::from_secs(90);
    let path = filter
        .query(from, 256)
        .replace("/v1/events?", "/v1/events/stream?");
    let body = store.get_streaming(&path, "text/event-stream", STREAM_DEADLINE)?;
    connected();
    let mut reader = BufReader::new(body);
    let mut cursor = from;
    let mut line = String::new();
    let mut data = String::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(cursor);
        }
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| Error::new(Code::StorageFailed, error.to_string()))?;
        if read == 0 {
            return Ok(cursor); // deadline or clean end; the caller reconnects
        }
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        } else if line.is_empty() && !data.is_empty() {
            // One SSE frame is complete.
            if let Ok(stored) = serde_json::from_str::<StoredEvent>(&data) {
                cursor = stored.seq;
                if debug_enabled() {
                    eprintln!(
                        "[Koru][events] delivered name={} id={} seq={}",
                        stored.envelope.name, stored.envelope.event_id, stored.seq
                    );
                }
                handler(stored);
            }
            data.clear();
        }
        // `id:` lines are redundant with the seq inside the payload, and
        // comment lines (the keep-alive) are ignored by falling through.
    }
}

fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// A live tail. Drop = signal and detach (the thread notices within the
/// stream deadline); `close` = signal and wait.
pub struct Subscription {
    stop: std::sync::Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Subscription {
    /// Stop and wait for the thread — bounded by the stream deadline.
    pub fn close(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Deliberately no join: a Drop that could wait 90 seconds on a quiet
        // stream is a hang nobody chose. `close()` is the waiting variant.
    }
}

#[cfg(test)]
mod tests;
