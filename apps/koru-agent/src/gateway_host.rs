// SPDX-License-Identifier: MPL-2.0

//! Supervision of the Koru API Gateway.
//!
//! The gateway is a worker child of this same executable —
//! `koru-agent.exe --worker gateway` — spawned and watched through
//! `koru-agent-worker`, exactly as the session host is. This file is the
//! **supervisor adapter**: what one gateway child is, how it is proved ready,
//! how its health is checked, and how it is asked to stop. The *role* — what
//! the child does once it is running — is `gateway_worker.rs`, and the pair
//! mirrors `session_host.rs` ↔ `session_worker.rs` deliberately, because two
//! roles written the same way are two roles one person can hold in their head.
//!
//! It replaces `service.rs::ensure_gateway`, which spawned a sibling
//! `koru-api-gateway.exe` behind a process-wide ensure lock, adopted whatever
//! it found on the root, and stopped it again when the private bridge did not
//! belong to this launch. The supervisor owns every one of those jobs now, and
//! that sibling no longer exists — Plan 7 deleted the binary. The name survives
//! only in `koru_update::exe`, so an update can still ask an installed
//! pre-cutover one to stop and move its file aside.
//!
//! ## Startup never waits for the gateway
//!
//! [`start`] returns as soon as the supervision thread exists; the first child
//! is spawned on that thread. There is deliberately **no** equivalent of
//! `session_host::Supervisor::start`'s blocking twelve-second first-child wait.
//! The sessions host earns that wait — a Terminal tab cannot be routed to a
//! host that is not there — and the gateway does not, because a person holding
//! a dictation chord must not wait behind a Store that is opening. Readiness is
//! awaited only by [`Supervisor::ensure`], only on a worker thread, and never
//! by startup or the winit loop.
//!
//! ## The three fencing properties, held apart
//!
//! They are easy to conflate and only the third protects the shutdown request
//! itself.
//!
//! 1. **An owner-initiated service stop is deliberately not
//!    generation-fenced.** [`Supervisor::stop_for_quit`] and
//!    [`Supervisor::hold_and_stop_for_update`] mean *this service should not be
//!    running*, and stopping whatever child is current is the correct outcome
//!    even if it replaced the one the caller observed. Neither takes a
//!    generation.
//! 2. **A role operation carrying a per-child handle is fenced.**
//!    [`Supervisor::issue`] hands out [`GatewayChild`] — the launch id this
//!    Agent minted and the bridge capability it registered — only for the
//!    generation it was asked about, and refuses once that child is gone. It is
//!    **not** an HTTP stop authority; see the next rule, which is what actually
//!    protects the shutdown request.
//! 3. **The graceful-stop adapter is bound to its child at the moment it
//!    acts.** The gateway's authority and the session host's are *not*
//!    analogous, and treating them as though they were is the trap. The session
//!    host's stop carries a per-child `stop_authority`, so a replacement
//!    refuses a predecessor's request. The gateway has no such thing:
//!    `POST /v1/shutdown` authorizes the `gateway.control` **scope** and checks
//!    no per-child secret, and the helper this file replaces discovered
//!    whichever gateway owned the root without comparing launch identity. So a
//!    delayed callback started for child A could discover child B and shut down
//!    a healthy replacement. [`ask_the_checked_child_to_stop`] is the answer,
//!    and its signature is the guarantee: `discover` is an `FnOnce`, so the
//!    hello that was checked is arithmetically the hello the client is built
//!    from, and authority cannot be reached for a second time. See its own
//!    documentation for the order and for what is provable when.
//!
//! Whatever a stop returns names the operation and the generation it describes
//! ([`GatewayStop`]), so a later snapshot cannot stand in for this request's
//! completion.
//!
//! ## Why a process global
//!
//! [`current`] is a `OnceLock`, for the reason `session_host`'s
//! `LIFECYCLE_AUTHORITY` is one: the supervision is created in `run` and its
//! consumers are free functions on worker threads (`service::store_client`,
//! `service::run_sync_control`, `service::open_gateway_docs`) that no handle
//! reaches. It is installed once, after the device network — whose
//! `expect_gateway` every launch calls — and before the sync control worker
//! that ensures on every attempt.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use koru_agent_worker::machine::State;
use koru_agent_worker::supervisor::{
    self as worker, CurrentChild, Generation, LaunchPlan, StaleGeneration, StatusReport,
    StopOutcome, WorkerEventKind, WorkerHandle, WorkerSpec,
};

use crate::Paths;

/// Everything that can go wrong on the way to a serving gateway.
///
/// The codes are what a shell branches on and what a log line carries; they
/// are unchanged from the sibling-launch era so no consumer has to learn a new
/// vocabulary because ownership moved.
#[derive(Debug)]
pub enum GatewayServiceError {
    NotFound(String),
    Launch(String),
    Unavailable(String),
    Incompatible(String),
}

impl GatewayServiceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "gateway_not_found",
            Self::Launch(_) => "gateway_launch_failed",
            Self::Unavailable(_) => "gateway_unreachable",
            Self::Incompatible(_) => "gateway_incompatible",
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            Self::NotFound(_) => "Koru API Gateway was not found.".into(),
            Self::Launch(_) => "Koru API Gateway could not be started.".into(),
            Self::Unavailable(_) => "Koru API Gateway is not answering.".into(),
            Self::Incompatible(_) => "Koru API Gateway is incompatible with this build.".into(),
        }
    }
}

impl std::fmt::Display for GatewayServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(message)
            | Self::Launch(message)
            | Self::Unavailable(message)
            | Self::Incompatible(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for GatewayServiceError {}

// ------------------------------------------------------------ the per-child handle

/// What the role's closures need to reach and stop **their own** child, and
/// nothing else's.
///
/// Handed out only by [`Supervisor::issue`], which is the generation fence
/// (fencing property 2). It carries no stop token: the gateway is stopped over
/// its authenticated public API, which authorizes a scope rather than a child,
/// and the session host's per-Agent-run cleanup authority is deliberately never
/// given to it — a gateway holding that could end every terminal on the
/// machine (ADR-278).
#[derive(Debug)]
pub struct GatewayChild {
    /// This child's identity. The published `GatewayHello.supervisor_launch_id`
    /// must equal it before anything trusts the endpoint.
    pub launch_id: String,
    segment: String,
    root: PathBuf,
}

impl GatewayChild {
    fn discover(
        &self,
    ) -> Result<koru_api_discovery::GatewayHello, koru_api_discovery::DiscoveryError> {
        discover_at(&self.segment, &self.root)
    }

    fn probe(&self) -> Result<(), koru_api_discovery::DiscoveryError> {
        koru_api_discovery::probe_at(&self.segment, &self.root)
    }
}

/// The supervisor's own greeting: control-scoped, because every question it
/// asks a gateway is a lifecycle question. It is deliberately *not* the token
/// any Store or catalog work uses — reusing a control credential for data makes
/// every workspace refresh a 403.
fn discover_at(
    segment: &str,
    root: &Path,
) -> Result<koru_api_discovery::GatewayHello, koru_api_discovery::DiscoveryError> {
    koru_api_discovery::connect_at(
        segment,
        root,
        &koru_api_discovery::ClientHello {
            product: "koru-agent-supervisor".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            segment: segment.into(),
            api_version: 1,
            requested_scopes: vec![koru_api_client::CONTROL_SCOPE.into()],
            grant: None,
        },
    )
}

// --------------------------------------------------------- the checked stop

/// What one attempt to ask a child to stop found. **`Stale` means nobody was
/// asked**, which is the whole point of the type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StopAsk {
    /// The checked child accepted the request.
    Asked,
    /// Nothing is published at this root; there is nothing to ask.
    NotRunning,
    /// Something else owns the root now — a replacement, or a gateway this
    /// Agent did not launch. **No shutdown was sent.**
    Stale {
        expected: String,
        found: Option<String>,
    },
    /// Discovery or the request itself failed.
    Failed(String),
}

/// **Fencing property 3, as one function.** Ask exactly the child whose stop
/// began, or ask nobody.
///
/// In order, and the order is the property:
///
/// 1. discover a control-scoped `GatewayHello`;
/// 2. require its `supervisor_launch_id` to equal the launch id of the child
///    whose stop began;
/// 3. build the client from **that same checked hello** — `Client::from_gateway`
///    carries no renewal closure and so cannot re-acquire authority for another
///    launch;
/// 4. return a stale result if the launch differs, having sent nothing.
///
/// `discover` is an `FnOnce` on purpose. *What is forbidden is checking A and
/// then reaching for authority again*, and a closure that can only be called
/// once cannot do it — the rule is held by the signature rather than by a
/// comment somebody has to keep reading. A hello captured and checked before
/// replacement is equally acceptable, and is expressed by passing a `discover`
/// that returns one already in hand.
///
/// **The hazard is now reachable, and is asserted end to end.** Plan 5's
/// correction C2 moved the graceful request off the supervisor's control
/// thread, so this callback can genuinely be released after its child was
/// killed on the stop deadline and an aborted update resumed into a healthy
/// replacement — which is precisely what
/// `worker_process.rs::a_released_stop_callback_never_reaches_the_replacement`
/// arranges with real processes.
///
/// **The invariant is that no authorized shutdown *executes* on the
/// replacement**, not that nothing reaches it. A replacement that reuses its
/// predecessor's address may legitimately receive a stale request and reject
/// it, and that rejection is a correct outcome;
/// `a_stop_callback_released_after_its_child_died_executes_nothing_on_the_replacement`
/// models exactly that, in both shapes — a callback paused before discovery,
/// and one that had already captured the checked hello.
pub(crate) fn ask_the_checked_child_to_stop<Discover, Shutdown>(
    expected_launch: &str,
    discover: Discover,
    shutdown: Shutdown,
) -> StopAsk
where
    Discover:
        FnOnce() -> Result<koru_api_discovery::GatewayHello, koru_api_discovery::DiscoveryError>,
    Shutdown: FnOnce(koru_api_discovery::GatewayHello) -> Result<(), String>,
{
    let hello = match discover() {
        Ok(hello) => hello,
        Err(koru_api_discovery::DiscoveryError::NotRunning(_)) => return StopAsk::NotRunning,
        Err(error) => return StopAsk::Failed(error.to_string()),
    };
    if hello.supervisor_launch_id.as_deref() != Some(expected_launch) {
        return StopAsk::Stale {
            expected: expected_launch.to_owned(),
            found: hello.supervisor_launch_id,
        };
    }
    match shutdown(hello) {
        Ok(()) => StopAsk::Asked,
        Err(error) => StopAsk::Failed(error),
    }
}

// -------------------------------------------------------------- stop results

/// Which owner-initiated stop a [`GatewayStop`] describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopOperation {
    /// The Agent is quitting. The supervision ends with it.
    Quit,
    /// An update is about to replace this image. The supervision is parked and
    /// revivable — an aborted install must be able to bring the service back.
    UpdateHandoff,
}

impl std::fmt::Display for StopOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Quit => "quit",
            Self::UpdateHandoff => "update handoff",
        })
    }
}

/// What a stop request came back with.
///
/// **It names the operation and the generation it describes**, so a later
/// snapshot cannot stand in for this request's completion — and, since Plan 5,
/// it says *how the child ended*: graceful, killed on the deadline, refused,
/// nothing there, or never completed. That is [`StopOutcome`], recorded by the
/// supervisor at the moment the stop finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayStop {
    pub operation: StopOperation,
    /// How the child ended, and which child and request it was about.
    pub outcome: StopOutcome,
    /// Where supervision stood when the request returned.
    pub state: State,
}

impl GatewayStop {
    /// The child the supervisor was stopping. `None` when there was none.
    ///
    /// Deliberately *not* a condition on anything: an owner stop is not
    /// generation-fenced (fencing property 1), so what it ends is whichever
    /// child was current when it acted, and this is the record of which one
    /// that was.
    pub fn generation(&self) -> Option<Generation> {
        self.outcome.generation()
    }

    /// Whether supervision reached its stop with no child left running.
    pub fn stopped(&self) -> bool {
        matches!(self.state, State::Stopped)
    }

    /// The gateway shut its Store down on its own terms.
    pub fn was_graceful(&self) -> bool {
        self.outcome.was_graceful()
    }

    /// **Whether the supervision has provably let go of its child.**
    ///
    /// A [`StopOutcome::TimedOut`] fails it: the supervisor still owns a
    /// child, so the image is still in use — **a timeout is not permission to
    /// swap**. Necessary for an update, and deliberately not sufficient: see
    /// [`GatewayStop::store_confirmed_its_shutdown`].
    pub fn image_is_free(&self) -> bool {
        self.stopped() && self.outcome.child_is_gone()
    }

    /// **The only answer an update may install on.**
    ///
    /// The gateway child exits 0 exactly when it joined its Store worker and
    /// found it clean — the role sets that code from `Completion::is_clean`,
    /// which requires a `WorkerExit::Clean` and nothing else. So a graceful
    /// stop with code 0 *is* the Store's own confirmation that its last commit
    /// finished, carried across a process boundary by an exit code.
    ///
    /// Everything else fails closed, and each for a stated reason:
    ///
    /// - a kill on the deadline says the Store was still working;
    /// - `NotRunning` says the child died on its own, and **absence of a child
    ///   is never evidence of a clean Store join**;
    /// - a non-zero code says the child itself reported the Store did not
    ///   confirm;
    /// - `TimedOut` says the image is not even free.
    ///
    /// An install refused here aborts and resumes the service, which is
    /// recoverable. An install that proceeded over an unfinished commit would
    /// not be.
    pub fn store_confirmed_its_shutdown(&self) -> bool {
        self.image_is_free() && matches!(self.outcome, StopOutcome::Graceful { code: Some(0), .. })
    }
}

impl std::fmt::Display for GatewayStop {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "gateway {}: {} (supervision is {:?})",
            self.operation, self.outcome, self.state
        )
    }
}

// ------------------------------------------------------------ the supervision seam

/// The supervisor's side, behind one trait so every decision in this file is
/// provable with a fixture rather than with a real Store.
///
/// It is exactly [`WorkerHandle`]'s public surface for one payload type and
/// adds nothing; the point is that a test can script `Ready → Exited →
/// Backoff → Ready` in microseconds and assert what `ensure` and the stops did
/// about it.
pub(crate) trait Children: Send + Sync + 'static {
    fn status(&self) -> StatusReport;
    fn current(&self) -> Option<CurrentChild<GatewayChild>>;
    fn issue(&self, generation: Generation) -> Result<Arc<GatewayChild>, StaleGeneration>;
    fn hold(&self);
    fn resume(&self);
    fn stop_serving(&self) -> StopOutcome;
    fn stop(&self) -> StopOutcome;
}

impl Children for WorkerHandle<GatewayChild> {
    fn status(&self) -> StatusReport {
        WorkerHandle::status(self)
    }
    fn current(&self) -> Option<CurrentChild<GatewayChild>> {
        WorkerHandle::current(self)
    }
    fn issue(&self, generation: Generation) -> Result<Arc<GatewayChild>, StaleGeneration> {
        WorkerHandle::issue(self, generation)
    }
    fn hold(&self) {
        WorkerHandle::hold(self);
    }
    fn resume(&self) {
        WorkerHandle::resume(self);
    }
    fn stop_serving(&self) -> StopOutcome {
        WorkerHandle::stop_serving(self)
    }
    fn stop(&self) -> StopOutcome {
        WorkerHandle::stop(self)
    }
}

// ---------------------------------------------------------------- the ensure

/// How long a consumer waits for a gateway before being told it is not
/// answering.
///
/// Longer than the ten seconds the sibling launcher gave, because the
/// supervisor may legitimately be inside a restart backoff when the ask
/// arrives, and shorter than the thirty seconds a shell's `EnsureService`
/// socket allows, so the honest refusal arrives rather than a timeout.
const ENSURE_DEADLINE: Duration = Duration::from_secs(20);
const ENSURE_POLL: Duration = Duration::from_millis(25);

/// How long a fresh child has to compose Store, AI and events, bind its
/// loopback listener and publish itself, before the supervisor treats it as
/// unhealthy and replaces it.
///
/// Deliberately far longer than [`ENSURE_DEADLINE`]: telling a caller "not yet"
/// is cheap, and killing a child that was eleven seconds from ready is a
/// restart loop.
const READY_DEADLINE: Duration = Duration::from_secs(60);

/// A cached ensure answer, so one readiness wait and one handshake serve every
/// consumer that asks while it is current.
struct Ensured {
    generation: Generation,
    child: Arc<GatewayChild>,
    hello: koru_api_discovery::GatewayHello,
}

impl Ensured {
    /// Reusable while it describes the serving child and its credential has
    /// comfortably longer than one ask to live.
    ///
    /// **The generation is necessary and not sufficient**, which a process test
    /// found rather than a review: a child's rendezvous closes when it exits,
    /// and the supervisor learns of the exit a moment later when its waiter
    /// reaps the process. In that gap the generation is still current and the
    /// endpoint is already gone, so a cache keyed on the generation alone hands
    /// a consumer the record of a gateway that has stopped answering. The
    /// caller then discovers nothing and has been told everything is fine.
    ///
    /// The liveness check closes it and costs a local connect with no hello, so
    /// re-validating mints no credential. It cannot close the microsecond
    /// between the check and the answer — nothing can, and the uncached path
    /// has the same window — but it turns a window measured in reaping latency
    /// into one measured in a socket call.
    fn still_good(&self, current: Option<Generation>, now_millis: i64) -> bool {
        Some(self.generation) == current
            && self.hello.session.expires_at_millis - now_millis > CREDENTIAL_MARGIN_MILLIS
    }
}

/// How this supervision reaches the world, behind closures so every decision
/// above is provable with fixtures.
struct Boundary<'a> {
    /// Does this child's rendezvous still answer? No hello is sent.
    still_there: &'a (dyn Fn(&GatewayChild) -> bool + Sync),
    /// The full handshake, for a fresh answer.
    discover: &'a (dyn Fn(&GatewayChild) -> Option<koru_api_discovery::GatewayHello> + Sync),
}

impl Boundary<'_> {
    /// What production uses: the real rooted rendezvous.
    fn live() -> Self {
        Self {
            still_there: &|child: &GatewayChild| child.probe().is_ok(),
            discover: &|child: &GatewayChild| child.discover().ok(),
        }
    }
}

const CREDENTIAL_MARGIN_MILLIS: i64 = 60_000;

/// Wall-clock epoch milliseconds, which is the unit the published credential's
/// expiry is written in. Used only to decide whether a cached answer is worth
/// handing out again — never to order anything (PRINCIPLES: clocks are for
/// display, version vectors compare states).
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

struct Shared {
    children: Box<dyn Children>,
    /// The child generation an update hold was taken against — the fence a
    /// later resume checks before deciding whether the prepared child survived.
    held: Mutex<Option<Generation>>,
    /// One coalesced ensure result. The mutex is also the coalescing point:
    /// concurrent callers serialize here, and all but the first find an answer
    /// waiting rather than starting a second readiness wait.
    ///
    /// **Nothing invalidates it explicitly, on purpose.** It is keyed on the
    /// serving child's generation, so a stop, a hold, a crash and a restart all
    /// invalidate it by making that generation not current. A `stop` that
    /// reached in here to clear it would have to wait for whatever ensure is
    /// holding this lock — up to a consumer's whole patience — which is the
    /// quit path queueing behind a question.
    ensured: Mutex<Option<Ensured>>,
    /// Set by [`Supervisor::stop_for_quit`]. An ensure racing a quit must not
    /// resurrect the service the person just asked to end.
    quitting: AtomicBool,
    /// The gateway URL is printed once per Agent run, not once per ensure.
    url_logged: OnceLock<()>,
}

pub struct Supervisor {
    shared: Arc<Shared>,
}

static GATEWAY: OnceLock<Supervisor> = OnceLock::new();

/// Start supervising the gateway and install it for this process.
///
/// **Returns immediately.** The first spawn happens on the supervision thread;
/// nothing here opens a Store, reads an image or waits for readiness.
pub fn start(
    paths: &Paths,
    device: koru_device::DeviceId,
    bot_authority: koru_network_bridge::BotDelegateAuthority,
) -> &'static Supervisor {
    install(Supervisor::over(Box::new(worker::start(gateway_spec(
        paths,
        device,
        Some(bot_authority),
    )))))
}

fn install(supervisor: Supervisor) -> &'static Supervisor {
    if GATEWAY.set(supervisor).is_err() {
        tracing::warn!(
            target: "koru::gateway_host",
            "a gateway supervisor was already installed; keeping the first"
        );
    }
    GATEWAY.get().expect("a gateway supervisor")
}

/// The installed supervision, or `None` in a process that never started one —
/// a unit test, or an Agent whose device network did not come up.
pub fn current() -> Option<&'static Supervisor> {
    GATEWAY.get()
}

/// Prove a serving gateway and answer with its published record.
///
/// The free function every consumer uses. Blocking: call it from a worker
/// thread, never from the Agent's answering thread or the winit loop.
///
/// **A process that supervises no gateway can only ask whether one is there.**
/// `koru-agent debug store` is a one-shot command in a process that never
/// called `run`, so it holds no supervision and must not start one — a
/// diagnostic that launches a Store in order to look at it is a diagnostic that
/// changes what it measures. It discovers the gateway the *running* Agent
/// supervises, exactly as any other client would, and says so honestly when
/// there is none. (Before Plan 4 this call could silently spawn a sibling
/// gateway from a CLI invocation.)
pub fn ensure(paths: &Paths) -> Result<koru_api_discovery::GatewayHello, GatewayServiceError> {
    match current() {
        Some(supervisor) => supervisor.ensure(),
        None => discover_without_supervising(paths),
    }
}

fn discover_without_supervising(
    paths: &Paths,
) -> Result<koru_api_discovery::GatewayHello, GatewayServiceError> {
    discover_at(crate::segment(), paths.root()).map_err(|error| match error {
        koru_api_discovery::DiscoveryError::Incompatible { .. } => {
            GatewayServiceError::Incompatible(error.to_string())
        }
        _ => GatewayServiceError::Unavailable(error.to_string()),
    })
}

impl Supervisor {
    fn over(children: Box<dyn Children>) -> Self {
        Self {
            shared: Arc::new(Shared {
                children,
                held: Mutex::new(None),
                ensured: Mutex::new(None),
                quitting: AtomicBool::new(false),
                url_logged: OnceLock::new(),
            }),
        }
    }

    /// What the supervision is doing — the inspectable answer the principles
    /// require, and the one a tray or a status request reads.
    pub fn status(&self) -> StatusReport {
        self.shared.children.status()
    }

    /// **Fencing property 2.** The child's launch identity, handed out only
    /// for the generation the caller minted its operation against, and refused
    /// visibly once that child is gone.
    pub fn issue(&self, generation: Generation) -> Result<Arc<GatewayChild>, StaleGeneration> {
        self.shared.children.issue(generation)
    }

    /// Wait until a gateway child is serving, and answer with what it
    /// published.
    ///
    /// **One handle and one coalesced result serve every consumer.** The first
    /// caller in does the waiting and the handshake; anyone who asks while that
    /// answer still describes the serving child is handed the same one. A
    /// restart changes the generation, which is what invalidates it.
    ///
    /// **An unrelated discovered endpoint is not an adoptable worker.** Nothing
    /// here adopts: the answer is always the record of the child *this* Agent
    /// launched, proved by its launch id, and a foreign endpoint is a reason to
    /// keep waiting rather than a gateway to report.
    pub fn ensure(&self) -> Result<koru_api_discovery::GatewayHello, GatewayServiceError> {
        self.ensure_within(ENSURE_DEADLINE, &Boundary::live())
    }

    /// [`Supervisor::ensure`] with its blocking parts handed in: how long a
    /// consumer waits, and how the world is reached. The seam exists so the
    /// coalescing, the launch-id check and the refusals are provable with
    /// fixtures instead of with a real Store.
    fn ensure_within(
        &self,
        patience: Duration,
        boundary: &Boundary<'_>,
    ) -> Result<koru_api_discovery::GatewayHello, GatewayServiceError> {
        let mut ensured = self
            .shared
            .ensured
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let generation = self.shared.children.status().generation;
        if let Some(cached) = ensured.as_ref()
            && cached.still_good(generation, now_millis())
            && (boundary.still_there)(&cached.child)
        {
            return Ok(cached.hello.clone());
        }
        let (child, hello) = self.wait_for_a_serving_child(patience, boundary)?;
        self.report_gateway_url(&hello);
        *ensured = Some(Ensured {
            generation: child.generation,
            child: child.payload,
            hello: hello.clone(),
        });
        Ok(hello)
    }

    fn wait_for_a_serving_child(
        &self,
        patience: Duration,
        boundary: &Boundary<'_>,
    ) -> Result<(CurrentChild<GatewayChild>, koru_api_discovery::GatewayHello), GatewayServiceError>
    {
        if self
            .shared
            .held
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
        {
            return Err(GatewayServiceError::Unavailable(
                "an update is holding the gateway".into(),
            ));
        }
        let deadline = Instant::now() + patience;
        // A parked supervision is revived once per ask, never in a loop: an
        // aborted update, or an exhausted budget somebody is asking about
        // again, is a decision to serve — repeating it every 25 ms is not.
        let mut revived = false;
        loop {
            let status = self.shared.children.status();
            match status.state {
                State::Ready => {
                    if let Some(child) = self.shared.children.current()
                        && let Some(hello) = (boundary.discover)(&child.payload)
                    {
                        // **An unrelated discovered endpoint is not an
                        // adoptable worker.** A record naming another launch —
                        // a predecessor still dying, or a gateway nobody here
                        // started — is this child not being ready yet, and is
                        // never an answer.
                        if hello.supervisor_launch_id.as_deref()
                            == Some(child.payload.launch_id.as_str())
                        {
                            koru_ipc::ipc_debug!(
                                "gateway: up, instance={} api={}",
                                hello.instance_id,
                                hello.api_version
                            );
                            return Ok((child, hello));
                        }
                    }
                }
                State::Exhausted | State::Stopped if !revived => {
                    revived = true;
                    if !self.shared.quitting.load(Ordering::Acquire) {
                        self.shared.children.resume();
                    }
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(self.why_not(status));
            }
            std::thread::sleep(ENSURE_POLL);
        }
    }

    /// The honest refusal, in the vocabulary consumers already branch on.
    fn why_not(&self, status: StatusReport) -> GatewayServiceError {
        let last = status
            .last_error
            .unwrap_or_else(|| format!("the gateway is {:?}", status.state));
        match status.state {
            State::Exhausted => GatewayServiceError::Launch(format!(
                "the gateway could not be started after {} restarts: {last}",
                status.respawns
            )),
            _ => GatewayServiceError::Unavailable(last),
        }
    }

    fn report_gateway_url(&self, gateway: &koru_api_discovery::GatewayHello) {
        if self.shared.url_logged.set(()).is_ok() {
            println!("[Koru API Gateway] listening on {}", gateway.api_endpoint);
            println!(
                "[Koru API Gateway] docs: {}/docs",
                gateway.api_endpoint.trim_end_matches('/')
            );
        }
    }

    /// Enter the update window and stop the child inside it.
    ///
    /// **Fencing property 1: not generation-fenced.** The update means *this
    /// service must not be running while its image is replaced*, and stopping
    /// whatever child is current is the correct outcome even if it replaced the
    /// one the caller observed. The hold suppresses restarts first, so the
    /// stop is not racing a respawn.
    ///
    /// The supervision is parked and revivable — [`Supervisor::resume`] is the
    /// aborted install's way back. Blocking; call it from a lifecycle worker.
    pub fn hold_and_stop_for_update(&self) -> GatewayStop {
        *self
            .shared
            .held
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = self.shared.children.status().generation;
        self.shared.children.hold();
        let outcome = self.shared.children.stop_serving();
        GatewayStop {
            operation: StopOperation::UpdateHandoff,
            outcome,
            state: self.shared.children.status().state,
        }
    }

    /// The aborted update's way back: leave the window and let supervision
    /// replace or revive the child.
    ///
    /// The held generation is checked rather than assumed — a child that died
    /// inside the window is reported as gone rather than silently replaced
    /// without a word, which is the same courtesy `session_host::cancel` pays.
    pub fn resume(&self) {
        let held = self
            .shared
            .held
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(generation) = held
            && let Err(stale) = self.shared.children.issue(generation)
        {
            tracing::info!(
                target: "koru::gateway_host",
                %stale,
                "the held gateway child is gone; resuming with a fresh one"
            );
        }
        self.shared.children.resume();
    }

    /// Stop the gateway for good, on the way out.
    ///
    /// **Fencing property 1 again**, and the Job Object is the fallback: a
    /// child that will not answer its graceful request is killed on the stop
    /// deadline, and anything still alive when this process exits dies with the
    /// job. Blocking; call it from a lifecycle worker, never from winit.
    ///
    /// **The fallback has to stay a fallback.** The winit thread does not wait
    /// for this — the person asked to quit — but the *process* does, briefly,
    /// after its loop has ended: `app::finish_quit_stop`. Without that the
    /// stop thread races the exit and loses, so every ordinary quit killed the
    /// Store a few milliseconds into a shutdown it had agreed to, and there
    /// was nothing left alive to record the outcome. It is bounded past the
    /// stop deadline, so both endings fit.
    pub fn stop_for_quit(&self) -> GatewayStop {
        self.shared.quitting.store(true, Ordering::Release);
        let outcome = self.shared.children.stop();
        GatewayStop {
            operation: StopOperation::Quit,
            outcome,
            state: self.shared.children.status().state,
        }
    }
}

// ------------------------------------------------------------------ the spec

/// The role's whole knowledge, spelled as the worker crate's closures.
fn gateway_spec(
    paths: &Paths,
    device: koru_device::DeviceId,
    bot_authority: Option<koru_network_bridge::BotDelegateAuthority>,
) -> WorkerSpec<GatewayChild> {
    let segment: String = crate::segment().into();
    let root = paths.root().to_path_buf();
    let store_dir = paths.store_dir();
    let launch_segment = segment.clone();
    let launch_root = root.clone();

    let mut spec = WorkerSpec::new(
        "gateway",
        move |generation: Generation| {
            // **Once per Agent run**, at the first spawn: a gateway on this
            // root that this Agent did not launch owns the rendezvous, and the
            // child we are about to start would refuse before it opened a
            // database. Nothing this supervision launched can be answering
            // here — the machine reaps a child before it spawns its successor
            // — so whatever is found is a leftover from an older Agent or a
            // hand-started standalone host. This is *not* the deleted
            // adopt-and-stop-an-unbridged-gateway loop, which ran on every
            // ensure and could stop a healthy child of our own; it runs once,
            // on the supervision thread, when we provably have no child.
            if generation == Generation(1) {
                clear_a_gateway_this_agent_did_not_launch(&launch_segment, &launch_root);
            }
            let launch_id = uuid::Uuid::new_v4().to_string();
            let bootstrap = koru_network_bridge::LaunchBootstrap {
                device_id: Some(device.to_storage()),
                bot_delegate: bot_authority.clone(),
                bots_engine: crate::bots_host::authority()
                    .map(koru_network_bridge::BotEngineAuthority::new),
                launch_id: launch_id.clone(),
                bridge_version: koru_network_bridge::BRIDGE_VERSION,
                channel: launch_segment.clone(),
                root_fingerprint: koru_network_bridge::fingerprint(
                    store_dir.to_string_lossy().as_bytes(),
                ),
                // **This image, from `current_exe()`** — never a sibling path.
                // A role of our own executable proves its own image, which is
                // what makes "the Agent launched this exact binary" mean
                // something once the two are one file.
                installation_fingerprint: own_image_fingerprint()?,
                capability: koru_network_bridge::BridgeCapability::generate(),
                // The session host started before this in `run`, so its narrow
                // lifecycle secret already exists. A checkout with no serving
                // session host publishes none, and the gateway then serves no
                // workspace-lifecycle operation. It is deliberately not the
                // authority that stops the host.
                session_lifecycle: crate::session_host::lifecycle_authority()
                    .map(koru_network_bridge::SessionLifecycleAuthority::new),
            };
            // **Registered before the spawn**, so a child cannot hand shake
            // before the Agent knows to expect it, and a late handshake from a
            // predecessor finds this generation's capability instead of its
            // own and is refused.
            crate::network::current()
                .ok_or_else(|| "the Agent device network is not running".to_owned())?
                .expect_gateway(&bootstrap);
            let bootstrap = serde_json::to_vec(&bootstrap)
                .map_err(|error| format!("could not encode the gateway bootstrap: {error}"))?;
            Ok(LaunchPlan {
                args: vec!["--root".into(), launch_root.clone().into_os_string()],
                bootstrap,
                payload: GatewayChild {
                    launch_id,
                    segment: launch_segment.clone(),
                    root: launch_root.clone(),
                },
            })
        },
        // **Readiness is today's discovery poll, including its launch-id
        // check.** The `Incompatible` branch `ensure_gateway` returned to its
        // caller is this: a rendezvous belonging to another launch is not this
        // child being ready, it is this child not being ready yet.
        |child: &GatewayChild| {
            // Probe first. A probe sends no hello, so a poll every 25 ms does
            // not mint a control credential the caller immediately discards.
            child.probe().map_err(|error| error.to_string())?;
            let found = child.discover().map_err(|error| error.to_string())?;
            if found.supervisor_launch_id.as_deref() != Some(child.launch_id.as_str()) {
                return Err("the gateway rendezvous belongs to a different launch instance".into());
            }
            Ok(())
        },
    );
    spec.ready_deadline = READY_DEADLINE;
    spec.health = Some(Box::new(|child: &GatewayChild| {
        // **Tolerant on purpose.** A health failure kills and replaces the
        // child, and one busy named-pipe accept is not evidence that a Store
        // died. Three tries on the health thread, which is not a thread that
        // answers anybody.
        let mut last = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(250));
            }
            match child.probe() {
                Ok(()) => return Ok(()),
                Err(error) => last = error.to_string(),
            }
        }
        Err(last)
    }));
    spec.health_interval = Duration::from_secs(30);
    spec.stop_deadline = Duration::from_secs(10);
    spec.graceful_stop = Some(Box::new(|child: &GatewayChild| {
        match ask_the_checked_child_to_stop(
            &child.launch_id,
            || child.discover(),
            |hello| {
                koru_api_client::Client::from_gateway(hello)
                    .map_err(|error| error.to_string())?
                    .store()
                    .shutdown()
                    .map_err(|error| error.to_string())
            },
        ) {
            StopAsk::Asked => Ok(()),
            // Nothing published, or somebody else's gateway is. Either way this
            // child was asked nothing, and the supervisor's kill — which
            // targets *our* child's job and nobody else's — is the right next
            // step. The message is the record of why.
            StopAsk::NotRunning => Err("the gateway published nothing to ask".into()),
            StopAsk::Stale { expected, found } => Err(format!(
                "the gateway rendezvous belongs to launch {} rather than {expected}; nothing was asked to stop",
                found.as_deref().unwrap_or("an unsupervised gateway")
            )),
            StopAsk::Failed(error) => Err(error),
        }
    }));
    spec.on_event = Some(Box::new(|event| match &event.kind {
        // News, and unconditional: rare, consequential, and about the service
        // a person's documents live behind.
        WorkerEventKind::Ready => tracing::info!(
            target: "koru::gateway_host",
            generation = %event.generation,
            "the gateway child is serving"
        ),
        // Never reported: this role sets no memory limit. Spelled out rather
        // than wildcarded so a new event is a compile error here, as it was.
        WorkerEventKind::MemoryLimitReached { limit_bytes } => tracing::error!(
            target: "koru::gateway_host",
            generation = %event.generation,
            limit_bytes,
            "the gateway child reached its memory limit"
        ),
        WorkerEventKind::Exited { code } => tracing::warn!(
            target: "koru::gateway_host",
            generation = %event.generation,
            code = code.unwrap_or(-1),
            "the gateway child exited"
        ),
        WorkerEventKind::RestartScheduled { delay } => tracing::warn!(
            target: "koru::gateway_host",
            delay_ms = delay.as_millis() as u64,
            "a gateway restart is scheduled"
        ),
        WorkerEventKind::SpawnFailed { error } => tracing::error!(
            target: "koru::gateway_host",
            %error,
            "the gateway child could not be started"
        ),
        WorkerEventKind::Exhausted => tracing::error!(
            target: "koru::gateway_host",
            "the gateway restart budget is exhausted"
        ),
        WorkerEventKind::Held => tracing::info!(
            target: "koru::gateway_host", "gateway restarts are held for an update"
        ),
        WorkerEventKind::Resumed => tracing::info!(
            target: "koru::gateway_host", "gateway supervision resumed"
        ),
        WorkerEventKind::Stopped => tracing::info!(
            target: "koru::gateway_host",
            generation = %event.generation,
            "the gateway child was stopped"
        ),
        // Routine and per-poll: the transcript, not the news.
        WorkerEventKind::ProbeFailed { error } | WorkerEventKind::HealthFailed { error } => {
            koru_ipc::ipc_debug!("gateway: probe failed: {error}");
        }
    }));
    spec
}

/// This executable's fingerprint, read **once per Agent run**.
///
/// A hundred megabytes off the disk is not work to repeat on every restart, and
/// it is not work for a thread that answers a person: this runs on the
/// supervision control thread, at the first spawn, when no child exists and
/// nothing else is waiting on that thread. Every later generation finds it
/// already computed.
fn own_image_fingerprint() -> Result<String, String> {
    static FINGERPRINT: OnceLock<Result<String, String>> = OnceLock::new();
    FINGERPRINT
        .get_or_init(|| {
            let exe = std::env::current_exe()
                .map_err(|error| format!("could not find this executable: {error}"))?;
            let image = std::fs::read(&exe)
                .map_err(|error| format!("could not fingerprint {}: {error}", exe.display()))?;
            Ok(koru_network_bridge::fingerprint(image))
        })
        .clone()
}

/// Stop a gateway that owns this root and that this Agent did not launch, and
/// wait, bounded, for it to go.
///
/// The sessions worker's `stop_stale` with one difference: it runs on the
/// supervision thread rather than on startup, because startup does not wait for
/// the gateway. A leftover that will not go is left alone — the child that
/// follows refuses to bind, which is a visible failure with an honest reason,
/// and is better than an Agent that hangs before it has a window.
fn clear_a_gateway_this_agent_did_not_launch(segment: &str, root: &Path) {
    let Ok(found) = discover_at(segment, root) else {
        return;
    };
    tracing::warn!(
        target: "koru::gateway_host",
        instance = %found.instance_id,
        "a gateway this Agent did not launch owns this root; asking it to stop"
    );
    let asked = koru_api_client::Client::from_gateway(found)
        .map_err(|error| error.to_string())
        .and_then(|client| client.store().shutdown().map_err(|error| error.to_string()));
    if let Err(error) = asked {
        tracing::warn!(target: "koru::gateway_host", %error, "the leftover gateway refused to stop");
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while koru_api_discovery::probe_at(segment, root).is_ok() {
        if Instant::now() >= deadline {
            tracing::warn!(
                target: "koru::gateway_host",
                "the leftover gateway did not stop within 10 seconds"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests;
