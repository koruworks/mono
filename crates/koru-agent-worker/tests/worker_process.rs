// SPDX-License-Identifier: MPL-2.0

//! Real-process supervision: this test binary re-runs itself as the fake
//! role through the same `dispatch` the Agent will use, so the spawn, the
//! bootstrap pipe, the Job Object, the restart machine and the generation
//! fence are all exercised across genuine process boundaries.
//!
//! `harness = false`: `main` is ours, which is what lets the same binary be
//! the parent, the worker, and the grandchild. No disk is touched — the
//! fake role proves readiness over a loopback TCP socket carrying the
//! launch's one-time token.
//!
//! The machine-level ordering (stop beats backoff, a held stop with no
//! child) is pinned by the unit tests in `src/machine/tests.rs`; what needs
//! real processes is what those orderings *cost* — an acknowledgement that
//! times out, a control-thread join that blocks, a child that outlives its
//! deadline.
//!
//! **The graceful-stop closure is exercised here since Plan 5**, and it has
//! to be: correction C2 moved it off the control thread, which is what lets
//! it outlive the child it was started for. A fake closure is the right
//! instrument for that — what is under test is the *supervisor's* timing and
//! fencing, not any role's request. The role half, the gateway's checked
//! stop, is proved against the real function in
//! `apps/koru-agent/src/gateway_host/tests.rs`.
//!
//! **The sibling program is this same binary, reached as a sibling** (Plan
//! 1.4 of the OCR roadmap). No `koru-*` executable is spawned and nothing is
//! copied: `Program::Sibling` names this binary's own file name, the
//! supervisor resolves it beside `current_exe()` and runs it with the plan's
//! arguments and **no `--worker`** in front — so a supervisor that prepended
//! one would hit `dispatch`'s refusal under the unregistered label the sibling
//! specs use, and never be ready. Path rules are unit-tested in
//! `src/supervisor/tests.rs`.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use koru_agent_worker::machine::{Backoff, RestartBudget, State};
use koru_agent_worker::supervisor::{
    self, Forced, Generation, LaunchPlan, Program, StopOutcome, StopRequest, WorkerEventKind,
    WorkerHandle, WorkerSpec,
};
use koru_agent_worker::{Role, dispatch};

/// The first argument a sibling launch carries. Not a `--worker` role: the
/// sibling is recognised by `main` before the suite would run.
const SIBLING_ARG: &str = "sibling-program";

/// The label sibling specs use. Deliberately **not** a registered role, so a
/// supervisor that ran a sibling as `--worker <label>` would be refused.
const SIBLING_LABEL: &str = "sibling-label";

fn main() {
    if std::env::args().nth(1).as_deref() == Some(SIBLING_ARG) {
        fake_role(Vec::new());
    }
    dispatch(
        std::env::args(),
        &[
            Role {
                name: "fake",
                run: fake_role,
            },
            Role {
                name: "sleeper",
                run: sleeper_role,
            },
        ],
    );

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("scenario-parent-death") {
        parent_death_helper();
    }
    if args.get(1).map(String::as_str) == Some("scenario-sibling-parent-death") {
        supervise_then_die(sibling_spec(true, None, None));
    }

    let mut failures = 0u32;
    let mut check = |name: &str, result: Result<(), String>| match result {
        Ok(()) => println!("PASS {name}"),
        Err(why) => {
            failures += 1;
            println!("FAIL {name}: {why}");
        }
    };

    check("ready_containment_stop", ready_containment_stop());
    check(
        "crash_restart_and_stale_fence",
        crash_restart_and_stale_fence(),
    );
    check(
        "budget_exhausts_and_resume_retries",
        budget_exhausts_and_resume_retries(),
    );
    check(
        "hold_keeps_the_child_and_resume_returns",
        hold_alive_resume(),
    );
    check(
        "a_death_inside_a_hold_waits_for_resume",
        hold_death_resume(),
    );
    check("stop_beats_a_pending_backoff", stop_beats_backoff());
    check(
        "a_serving_stop_parks_and_resume_revives",
        serving_stop_then_revive(),
    );
    check(
        "a_held_stop_after_a_crash_completes_and_resumes",
        held_stop_after_a_crash(),
    );
    check(
        "a_held_stop_during_a_suppressed_backoff_completes",
        held_stop_during_backoff(),
    );
    check(
        "a_stalled_graceful_stop_is_bounded_and_the_loop_keeps_working",
        stalled_graceful_stop_is_bounded(),
    );
    check(
        "an_accepted_stop_the_child_ignores_is_killed_on_the_deadline",
        an_accepted_stop_the_child_ignores_is_killed_on_the_deadline(),
    );
    check(
        "a_released_stop_callback_never_reaches_the_replacement",
        released_callback_never_reaches_the_replacement(),
    );
    check("parent_death_kills_the_worker_tree", parent_death());
    check(
        "dispatch_refuses_bad_worker_arguments",
        dispatch_strictness(),
    );
    check(
        "a_sibling_is_launched_beside_us_contained_and_stopped",
        sibling_launched_contained_stopped(),
    );
    check(
        "a_sibling_dies_with_its_parent",
        parent_death_via("scenario-sibling-parent-death"),
    );
    check(
        "a_sibling_is_released_for_an_update_and_resumes",
        sibling_released_for_update(),
    );
    check(
        "a_missing_sibling_is_a_spawn_failure_that_says_where_it_looked",
        missing_sibling(),
    );
    #[cfg(windows)]
    check(
        "a_background_sibling_runs_below_normal_with_background_io",
        background_sibling(),
    );
    #[cfg(windows)]
    check(
        "a_sibling_over_its_memory_limit_is_ended_and_reported_apart_from_a_crash",
        sibling_memory_limit(),
    );
    #[cfg(windows)]
    check(
        "a_sibling_that_aborts_at_its_memory_limit_is_reported_before_its_exit",
        sibling_memory_limit_abort(),
    );

    if failures > 0 {
        std::process::exit(1);
    }
}

// ------------------------------------------------------------- the roles

/// The supervised child: read the bootstrap line, prove the one-time token
/// over loopback TCP after optionally spawning a grandchild, then wait on stdin —
/// EOF is the parent-death signal.
fn fake_role(_args: Vec<String>) -> ! {
    let mut stdin = std::io::stdin();
    let line = koru_agent_worker::read_bootstrap_line(&mut stdin).expect("bootstrap");
    let text = String::from_utf8(line).expect("utf-8 bootstrap");
    let mut port = 0u16;
    let mut token = String::new();
    let mut grand = false;
    let mut hoard = None;
    for word in text.split_whitespace() {
        if word == BACKGROUND_WORD && !koru_agent_worker::follow_launch_priority() {
            // **Readiness is the proof.** A child asked to follow its launch
            // into background mode that could not answers with a token the
            // parent refuses, so it is never ready.
            token = "not-in-background".into();
            continue;
        }
        if let Some(value) = word.strip_prefix("port=") {
            port = value.parse().expect("port");
        } else if let Some(value) = word.strip_prefix("token=") {
            if token.is_empty() {
                token = value.to_owned();
            }
        } else if let Some(value) = word.strip_prefix("grand=") {
            grand = value == "1";
        } else if let Some(value) = word.strip_prefix("hoard=") {
            hoard = Some(value.to_owned());
        }
    }
    // Finish spawning before advertising readiness. Reporting the token first
    // let the parent's bounded read expire while Windows started the grandchild.
    let grand_pid = if grand {
        Command::new(std::env::current_exe().expect("self"))
            .args(["--worker", "sleeper"])
            .spawn()
            .expect("spawn grandchild")
            .id()
    } else {
        0
    };
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("dial the parent");
    stream
        .write_all(format!("token={token}\ngrand={grand_pid}\n").as_bytes())
        .expect("report complete readiness");
    // Parked until the parent goes away or kills the job. Watched the way
    // every real role watches it, so the unix group containment a real
    // worker gets on parent loss is the one this fake gets too.
    koru_agent_worker::ParentPipe::new(stdin)
        .watch("fake parent", || std::process::exit(0))
        .expect("watch the parent");
    // **Only when asked, and only on the parent's word.** The probe drops the
    // readiness stream once it has read the report, so its end means the
    // parent saw this child ready; the go then comes on a second connection,
    // when the scenario has noted who this child is.
    let _held = hoard.map(|mode| {
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
        let mut go = TcpStream::connect(("127.0.0.1", port)).expect("dial for the go");
        let mut byte = [0u8; 1];
        go.read_exact(&mut byte).expect("the go");
        match mode.as_str() {
            HOARD_AND_ABORT => hoard_until_aborted(),
            _ => hoard_until_refused(),
        }
    });
    loop {
        std::thread::park();
    }
}

/// The bootstrap word that asks the child to follow a below-normal launch
/// into background mode ([`koru_agent_worker::follow_launch_priority`]).
const BACKGROUND_WORD: &str = "background";

/// The bootstrap word for a hoarder that **survives** its refused allocation.
/// Asked for only where a ceiling is enforced.
#[cfg_attr(not(windows), allow(dead_code))]
const HOARD_AND_SURVIVE: &str = "survive";
/// The bootstrap word for a hoarder that **aborts** on it, as an ordinary Rust
/// allocation does — the worker a ceiling usually meets.
const HOARD_AND_ABORT: &str = "abort";

/// Allocate infallibly until the allocator refuses and the runtime aborts the
/// process (printing its one "memory allocation … failed" line to the
/// inherited stderr). Capped like [`hoard_until_refused`].
fn hoard_until_aborted() -> Vec<Vec<u8>> {
    const CHUNK: usize = 8 << 20;
    const CAP: usize = 1 << 30;
    let mut hoard: Vec<Vec<u8>> = Vec::with_capacity(CAP / CHUNK);
    while hoard.len() < CAP / CHUNK {
        hoard.push(Vec::with_capacity(CHUNK));
    }
    std::hint::black_box(&hoard);
    hoard
}

/// Commit memory until an allocation is refused, then **keep running** with
/// what was granted: the child a memory ceiling must still end, because it
/// survives the refusal rather than aborting on it. Capped, so a ceiling that
/// was never applied leaves a parked child and a failed scenario rather than a
/// machine out of memory.
fn hoard_until_refused() -> Vec<Vec<u8>> {
    const CHUNK: usize = 8 << 20;
    const CAP: usize = 1 << 30;
    // Room for every chunk up front, so recording one never allocates.
    let mut hoard: Vec<Vec<u8>> = Vec::with_capacity(CAP / CHUNK);
    while hoard.len() < CAP / CHUNK {
        let mut chunk: Vec<u8> = Vec::new();
        if chunk.try_reserve_exact(CHUNK).is_err() {
            break;
        }
        hoard.push(chunk);
    }
    std::hint::black_box(&hoard);
    hoard
}

/// The grandchild: exists to be collateral in containment checks.
fn sleeper_role(_args: Vec<String>) -> ! {
    std::thread::sleep(Duration::from_secs(300));
    std::process::exit(0)
}

// ------------------------------------------------------- the fake payload

struct FakePayload {
    listener: TcpListener,
    token: String,
    grand_pid: Mutex<Option<u32>>,
}

static TOKEN_MINT: AtomicU64 = AtomicU64::new(1);

fn fake_spec(grand: bool, budget: RestartBudget, backoff: Backoff) -> WorkerSpec<FakePayload> {
    let mut spec = WorkerSpec::new(
        "fake",
        move |generation: Generation| {
            let listener =
                TcpListener::bind("127.0.0.1:0").map_err(|error| format!("bind: {error}"))?;
            listener
                .set_nonblocking(true)
                .map_err(|error| format!("nonblocking: {error}"))?;
            let port = listener
                .local_addr()
                .map_err(|error| format!("addr: {error}"))?
                .port();
            let token = format!(
                "tok-{generation}-{}",
                TOKEN_MINT.fetch_add(1, Ordering::Relaxed)
            );
            let bootstrap =
                format!("port={port} token={token} grand={}", u8::from(grand)).into_bytes();
            Ok(LaunchPlan {
                args: Vec::new(),
                bootstrap,
                payload: FakePayload {
                    listener,
                    token,
                    grand_pid: Mutex::new(None),
                },
            })
        },
        move |payload: &FakePayload| {
            let (stream, _) = payload
                .listener
                .accept()
                .map_err(|error| format!("no connection yet: {error}"))?;
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .map_err(|error| error.to_string())?;
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .map_err(|error| format!("token read: {error}"))?;
            if line.trim() != format!("token={}", payload.token) {
                return Err(format!("wrong token: {}", line.trim()));
            }
            let mut grand = String::new();
            reader
                .read_line(&mut grand)
                .map_err(|error| format!("grandchild read: {error}"))?;
            let pid = grand
                .trim()
                .strip_prefix("grand=")
                .and_then(|value| value.parse::<u32>().ok())
                .ok_or_else(|| "incomplete grandchild report".to_owned())?;
            *payload.grand_pid.lock().unwrap() = (pid != 0).then_some(pid);
            Ok(())
        },
    );
    spec.restart_budget = budget;
    spec.backoff = backoff;
    spec.ready_deadline = Duration::from_secs(10);
    spec
}

fn quick_budget() -> RestartBudget {
    RestartBudget {
        max_restarts: 3,
        window: Duration::from_secs(60),
    }
}

fn quick_backoff() -> Backoff {
    Backoff {
        initial: Duration::from_millis(50),
        max: Duration::from_millis(200),
    }
}

/// This binary's own file name: the sibling every sibling scenario resolves
/// beside `current_exe()`.
fn own_file_name() -> std::ffi::OsString {
    std::env::current_exe()
        .expect("this test binary")
        .file_name()
        .expect("a file name")
        .to_owned()
}

/// The fake, reached as a **sibling program** rather than a role: the same
/// launch as [`fake_spec`], with the sibling's own argv, an unregistered
/// label, and optionally a ceiling and a child that hoards memory.
///
/// `hoard` names the one generation told to hoard, and how it meets the
/// refusal, so the replacement after a memory-limit end is a well-behaved
/// child.
fn sibling_spec(
    grand: bool,
    hoard: Option<(u64, &'static str)>,
    memory_limit: Option<u64>,
) -> WorkerSpec<FakePayload> {
    let mut spec = fake_spec(grand, quick_budget(), quick_backoff());
    let mut inner = std::mem::replace(&mut spec.launch, Box::new(|_| Err("launch moved".into())));
    spec.launch = Box::new(move |generation: Generation| {
        let mut plan = inner(generation)?;
        plan.args = vec![SIBLING_ARG.into()];
        if let Some((hoarder, mode)) = hoard
            && hoarder == generation.0
        {
            plan.bootstrap
                .extend_from_slice(format!(" hoard={mode}").as_bytes());
        }
        Ok(plan)
    });
    spec.role = SIBLING_LABEL;
    spec.program = Program::Sibling(own_file_name());
    spec.memory_limit = memory_limit;
    spec
}

/// Every event a supervision reported, in order, for scenarios that care
/// about what was said and not only where it ended.
type EventLog = Arc<Mutex<Vec<(Generation, WorkerEventKind)>>>;

fn record_events(spec: &mut WorkerSpec<FakePayload>) -> EventLog {
    let log: EventLog = Arc::default();
    let sink = Arc::clone(&log);
    spec.on_event = Some(Box::new(move |event| {
        sink.lock().unwrap().push((event.generation, event.kind));
    }));
    log
}

// ------------------------------------------------------------ the helpers

fn wait_for(what: &str, deadline: Duration, mut done: impl FnMut() -> bool) -> Result<(), String> {
    let until = Instant::now() + deadline;
    while Instant::now() < until {
        if done() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(format!("timed out waiting for {what}"))
}

fn kill_pid(pid: u32) {
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output();
    #[cfg(not(windows))]
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .output();
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .expect("tasklist");
        String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
    }
    #[cfg(not(windows))]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

fn ready_child(
    handle: &WorkerHandle<FakePayload>,
    expect_generation: u64,
) -> Result<supervisor::CurrentChild<FakePayload>, String> {
    wait_for("worker readiness", Duration::from_secs(15), || {
        handle.status().state == State::Ready
            && handle.current().map(|child| child.generation.0) == Some(expect_generation)
    })?;
    handle
        .current()
        .ok_or_else(|| "ready but no current child".to_owned())
}

// ---------------------------------------------------------- the scenarios

fn ready_containment_stop() -> Result<(), String> {
    let handle = supervisor::start(fake_spec(true, quick_budget(), quick_backoff()));
    let child = ready_child(&handle, 1)?;
    handle
        .issue(Generation(1))
        .map_err(|stale| format!("current generation refused: {stale}"))?;

    let grand = wait_for("grandchild report", Duration::from_secs(5), || {
        child.payload.grand_pid.lock().unwrap().is_some()
    })
    .map(|()| child.payload.grand_pid.lock().unwrap().expect("checked"))?;
    if !pid_alive(child.pid) || !pid_alive(grand) {
        return Err("worker or grandchild not alive while ready".into());
    }

    handle.stop();
    if handle.status().state != State::Stopped {
        return Err(format!("stop left state {:?}", handle.status().state));
    }
    wait_for("worker tree death", Duration::from_secs(5), || {
        !pid_alive(child.pid) && !pid_alive(grand)
    })
    .map_err(|_| "the worker tree survived stop — job containment failed".into())
}

fn crash_restart_and_stale_fence() -> Result<(), String> {
    let handle = supervisor::start(fake_spec(false, quick_budget(), quick_backoff()));
    let first = ready_child(&handle, 1)?;
    kill_pid(first.pid);
    let _second = ready_child(&handle, 2)?;

    match handle.issue(Generation(1)) {
        Err(stale) => {
            if stale.current != Some(Generation(2)) {
                return Err(format!("stale refusal names the wrong child: {stale}"));
            }
        }
        Ok(_) => return Err("an operation minted against child 1 reached child 2".into()),
    }
    handle
        .issue(Generation(2))
        .map_err(|stale| format!("the serving generation was refused: {stale}"))?;
    handle.stop();
    Ok(())
}

fn budget_exhausts_and_resume_retries() -> Result<(), String> {
    let budget = RestartBudget {
        max_restarts: 1,
        window: Duration::from_secs(60),
    };
    let handle = supervisor::start(fake_spec(false, budget, quick_backoff()));
    let first = ready_child(&handle, 1)?;
    kill_pid(first.pid);
    let second = ready_child(&handle, 2)?;
    kill_pid(second.pid);
    wait_for("exhaustion", Duration::from_secs(5), || {
        handle.status().state == State::Exhausted
    })?;

    handle.resume();
    ready_child(&handle, 3)?;
    handle.stop();
    Ok(())
}

fn hold_alive_resume() -> Result<(), String> {
    let handle = supervisor::start(fake_spec(false, quick_budget(), quick_backoff()));
    let child = ready_child(&handle, 1)?;
    handle.hold();
    wait_for("hold", Duration::from_secs(5), || {
        handle.status().state == State::Stopping
    })?;
    if !pid_alive(child.pid) {
        return Err("the hold killed a child it was meant to keep".into());
    }
    handle.resume();
    wait_for("resume to ready", Duration::from_secs(5), || {
        handle.status().state == State::Ready
    })?;
    if handle.current().map(|current| current.generation) != Some(Generation(1)) {
        return Err("resume respawned a child that had survived".into());
    }
    handle.stop();
    Ok(())
}

fn hold_death_resume() -> Result<(), String> {
    let handle = supervisor::start(fake_spec(false, quick_budget(), quick_backoff()));
    let child = ready_child(&handle, 1)?;
    handle.hold();
    wait_for("hold", Duration::from_secs(5), || {
        handle.status().state == State::Stopping
    })?;
    kill_pid(child.pid);
    wait_for(
        "the held child's exit report",
        Duration::from_secs(5),
        || handle.status().last_exit.map(|exit| exit.generation) == Some(Generation(1)),
    )?;
    // No restart inside the window: still Stopping, still no child.
    std::thread::sleep(Duration::from_millis(300));
    if handle.status().state != State::Stopping || handle.current().is_some() {
        return Err("a child was restarted inside the update window".into());
    }
    handle.resume();
    ready_child(&handle, 2)?;
    handle.stop();
    Ok(())
}

fn stop_beats_backoff() -> Result<(), String> {
    let slow = Backoff {
        initial: Duration::from_secs(10),
        max: Duration::from_secs(10),
    };
    let handle = supervisor::start(fake_spec(false, quick_budget(), slow));
    let child = ready_child(&handle, 1)?;
    kill_pid(child.pid);
    wait_for("backoff", Duration::from_secs(5), || {
        handle.status().state == State::Backoff
    })?;
    let asked = Instant::now();
    handle.stop();
    if asked.elapsed() > Duration::from_secs(2) {
        return Err("stop waited on the pending backoff instead of beating it".into());
    }
    if handle.status().state != State::Stopped {
        return Err(format!("stop left state {:?}", handle.status().state));
    }
    Ok(())
}

fn serving_stop_then_revive() -> Result<(), String> {
    // The install path: sessions stopped for the swap, the swap aborted,
    // and the service comes back on resume instead of staying suppressed.
    let handle = supervisor::start(fake_spec(false, quick_budget(), quick_backoff()));
    let child = ready_child(&handle, 1)?;
    let outcome = handle.stop_serving();
    // **C3: the answer names the request and the child.** This role supplies
    // no graceful stop, so the honest answer is a forced one — the fake was
    // killed, it did not agree to anything.
    match &outcome {
        StopOutcome::Forced {
            request: StopRequest::Serving,
            generation: Generation(1),
            why: Forced::GracefulRefused(_),
        } => {}
        other => return Err(format!("a serving stop answered {other}")),
    }
    if outcome.was_graceful() {
        return Err("a kill was reported as a graceful stop".into());
    }
    if !outcome.child_is_gone() {
        return Err("a completed stop did not report its child gone".into());
    }
    if handle.status().last_stop.as_ref() != Some(&outcome) {
        return Err("the completed stop was not recorded on the status".into());
    }
    if handle.status().state != State::Stopped {
        return Err(format!(
            "serving stop left state {:?}",
            handle.status().state
        ));
    }
    wait_for("child death", Duration::from_secs(5), || {
        !pid_alive(child.pid)
    })?;
    handle.resume();
    ready_child(&handle, 2)?;
    handle.stop();
    Ok(())
}

// ------------------------------------------------- C1: a held stop with no child

/// **C1, first order: `Ready → Hold → the child dies → Stop`.**
///
/// Before the correction the machine emitted a stop action for a child that
/// had already been reaped: nothing happened, no exit followed, `Stopped` was
/// never reached, the acknowledgement timed out after thirty seconds and
/// `stop`'s control-thread join then blocked for good. This asserts the stop
/// *returns promptly*, says what it found, and that the service still comes
/// back on an abort.
fn held_stop_after_a_crash() -> Result<(), String> {
    let handle = supervisor::start(fake_spec(false, quick_budget(), quick_backoff()));
    let child = ready_child(&handle, 1)?;
    handle.hold();
    wait_for("hold", Duration::from_secs(5), || {
        handle.status().state == State::Stopping
    })?;
    // The child crashes inside the update window and is reaped there.
    kill_pid(child.pid);
    wait_for(
        "the held child's exit report",
        Duration::from_secs(5),
        || handle.status().last_exit.map(|exit| exit.generation) == Some(Generation(1)),
    )?;

    let asked = Instant::now();
    let outcome = handle.stop_serving();
    if asked.elapsed() > Duration::from_secs(5) {
        return Err(format!(
            "the stop of a held worker with no child took {:?} — it waited out its acknowledgement",
            asked.elapsed()
        ));
    }
    match &outcome {
        // **Absence of a child is never evidence of a clean Store join**, so
        // the previous exit is carried rather than passed off as graceful.
        StopOutcome::NotRunning { last_exit, .. } => {
            if last_exit.map(|exit| exit.generation) != Some(Generation(1)) {
                return Err(format!(
                    "the stop did not report the previous exit: {outcome}"
                ));
            }
        }
        other => return Err(format!("a held stop with no child answered {other}")),
    }
    if outcome.was_graceful() {
        return Err("a child that had already died was reported as a graceful stop".into());
    }
    if handle.status().state != State::Stopped {
        return Err(format!(
            "the held stop left state {:?}",
            handle.status().state
        ));
    }
    // The abort/resume continuation: the aborted install brings it back.
    handle.resume();
    ready_child(&handle, 2)?;
    handle.stop();
    Ok(())
}

/// **C1, second order: `the child dies → Backoff → Hold → Stop`.**
///
/// Nothing died inside the window here; the hold arrived while the machine was
/// already backing off, so it suppressed the respawn and the stop found
/// nothing running. Same wedge, different route in — and this one uses the
/// *final* stop, because its extra control-thread join is what turned the
/// wedge into a hang.
fn held_stop_during_backoff() -> Result<(), String> {
    let slow = Backoff {
        initial: Duration::from_secs(10),
        max: Duration::from_secs(10),
    };
    let handle = supervisor::start(fake_spec(false, quick_budget(), slow));
    let child = ready_child(&handle, 1)?;
    kill_pid(child.pid);
    wait_for("backoff", Duration::from_secs(5), || {
        handle.status().state == State::Backoff
    })?;
    handle.hold();
    wait_for("hold", Duration::from_secs(5), || {
        handle.status().state == State::Stopping
    })?;

    let asked = Instant::now();
    let outcome = handle.stop();
    if asked.elapsed() > Duration::from_secs(5) {
        return Err(format!(
            "the final stop of a held worker with no child took {:?} — its join blocked",
            asked.elapsed()
        ));
    }
    if !matches!(
        outcome,
        StopOutcome::NotRunning {
            request: StopRequest::Final,
            ..
        }
    ) {
        return Err(format!(
            "a held final stop with no child answered {outcome}"
        ));
    }
    if handle.status().state != State::Stopped {
        return Err(format!(
            "the held stop left state {:?}",
            handle.status().state
        ));
    }
    Ok(())
}

// ------------------------------------ C2: the deadline covers the graceful ask

/// A latch a test closure blocks on and the test releases.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: std::sync::Condvar,
}

impl Gate {
    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.changed.wait(open).unwrap();
        }
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

/// What the graceful-stop callback can see and do: which child currently owns
/// the rendezvous, and which children it has actually shut down.
#[derive(Default)]
struct World {
    serving: Mutex<Option<String>>,
    executed: Mutex<Vec<String>>,
    refused: Mutex<Vec<String>>,
}

/// A spec whose graceful-stop callback blocks on `gate`, then behaves the way
/// the gateway adapter does: discover who owns the rendezvous *now*, refuse
/// unless it is the child this stop was started for, and only then act.
fn gated_spec(
    gate: Arc<Gate>,
    world: Arc<World>,
    started: Arc<AtomicU64>,
) -> WorkerSpec<FakePayload> {
    let mut spec = fake_spec(false, quick_budget(), quick_backoff());
    let readiness_world = Arc::clone(&world);
    let inner = std::mem::replace(
        &mut spec.readiness,
        Box::new(|_| Err("readiness moved".into())),
    );
    // A child that proves ready owns the rendezvous, exactly as a published
    // `GatewayHello` names the launch that published it.
    spec.readiness = Box::new(move |payload: &FakePayload| {
        inner(payload)?;
        *readiness_world.serving.lock().unwrap() = Some(payload.token.clone());
        Ok(())
    });
    // ...and a child that exits takes its rendezvous with it.
    let exit_world = Arc::clone(&world);
    spec.on_event = Some(Box::new(move |event| {
        if matches!(event.kind, WorkerEventKind::Exited { .. }) {
            exit_world.serving.lock().unwrap().take();
        }
    }));
    spec.stop_deadline = Duration::from_millis(500);
    spec.graceful_stop = Some(Box::new(move |payload: &FakePayload| {
        started.fetch_add(1, Ordering::Relaxed);
        gate.wait();
        // The adapter's discipline: check who is there, act only if it is the
        // child this stop began for.
        let found = world.serving.lock().unwrap().clone();
        match found {
            Some(launch) if launch == payload.token => {
                world.executed.lock().unwrap().push(launch);
                Ok(())
            }
            found => {
                world
                    .refused
                    .lock()
                    .unwrap()
                    .push(found.unwrap_or_else(|| "nobody".into()));
                Err("the rendezvous belongs to another launch".into())
            }
        }
    }));
    spec
}

/// **C2: a stalled graceful-stop callback is bounded, and the control loop
/// keeps working while it is stuck.**
///
/// The callback used to run *on* the control thread, with the deadline armed
/// only after it returned — the guard running on the thread it guards. So this
/// asserts the two things that were impossible then: the child is killed on
/// the stop deadline **while the callback is still blocked**, and the stop
/// completes and answers `Forced { DeadlineElapsed }` rather than passing a
/// kill off as a graceful stop.
fn stalled_graceful_stop_is_bounded() -> Result<(), String> {
    let gate = Arc::new(Gate::default());
    let world = Arc::new(World::default());
    let started = Arc::new(AtomicU64::new(0));
    let handle = supervisor::start(gated_spec(
        Arc::clone(&gate),
        Arc::clone(&world),
        Arc::clone(&started),
    ));
    let child = ready_child(&handle, 1)?;

    let asked = Instant::now();
    let outcome = handle.stop_serving();
    let took = asked.elapsed();
    if started.load(Ordering::Relaxed) == 0 {
        return Err("the graceful stop was never asked".into());
    }
    // **The callback is still blocked, and the stop finished anyway.** It
    // records nothing until it is released, so both of these being empty is
    // the assertion that the stop did not wait for it.
    if !world.executed.lock().unwrap().is_empty() || !world.refused.lock().unwrap().is_empty() {
        return Err("the graceful callback finished; nothing was stalled".into());
    }
    if took > Duration::from_secs(10) {
        return Err(format!("a stalled callback delayed the stop by {took:?}"));
    }
    match &outcome {
        StopOutcome::Forced {
            request: StopRequest::Serving,
            generation: Generation(1),
            why: Forced::DeadlineElapsed,
        } => {}
        other => return Err(format!("a stalled stop answered {other}")),
    }
    if outcome.was_graceful() {
        return Err("a deadline kill was reported as a graceful stop".into());
    }
    wait_for("child death", Duration::from_secs(5), || {
        !pid_alive(child.pid)
    })
    .map_err(|_| "the stalled callback outlived its child's kill".to_owned())?;

    // The control loop was answering all along: it serviced its own deadline,
    // the kill, the exit and the acknowledgement, and it still takes a resume.
    gate.open();
    handle.resume();
    ready_child(&handle, 2)?;
    handle.stop();
    Ok(())
}

/// **A child that ignores a successful graceful request is killed on the
/// deadline**, and the outcome says so rather than passing the kill off as the
/// graceful stop the child agreed to.
///
/// Distinct from the case above and worth its own: there the *callback* never
/// returned, so nothing was ever agreed. Here the ask lands, is accepted, and
/// the child simply does not go — a wedged Store, a runtime that will not
/// drain — which is the shape a stop deadline exists for.
fn an_accepted_stop_the_child_ignores_is_killed_on_the_deadline() -> Result<(), String> {
    let mut spec = fake_spec(false, quick_budget(), quick_backoff());
    let asked = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&asked);
    spec.stop_deadline = Duration::from_millis(500);
    // Accepted at once. The fake role only ever exits on EOF or a kill, so the
    // deadline is the only thing that ends it.
    spec.graceful_stop = Some(Box::new(move |_payload: &FakePayload| {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }));
    let handle = supervisor::start(spec);
    let child = ready_child(&handle, 1)?;

    let began = Instant::now();
    let outcome = handle.stop_serving();
    if asked.load(Ordering::Relaxed) != 1 {
        return Err(format!(
            "the graceful request was made {} times",
            asked.load(Ordering::Relaxed)
        ));
    }
    if began.elapsed() > Duration::from_secs(5) {
        return Err(format!("the deadline did not bound the stop: {began:?}"));
    }
    match &outcome {
        StopOutcome::Forced {
            why: Forced::DeadlineElapsed,
            generation: Generation(1),
            ..
        } => {}
        other => return Err(format!("an ignored graceful stop answered {other}")),
    }
    if outcome.was_graceful() {
        return Err("a child that ignored its stop was reported as graceful".into());
    }
    wait_for("child death", Duration::from_secs(5), || {
        !pid_alive(child.pid)
    })?;
    handle.stop();
    Ok(())
}

/// **The regression case: a callback started for A, released only after A was
/// killed on the deadline and an aborted update resumed into a ready B, sends
/// B no shutdown and leaves it serving.**
///
/// C2 is what makes this reachable — the callback can now outlive its child —
/// and the invariant is that **no authorized shutdown executes on B**, not
/// that nothing reaches it. Here the callback discovers B, sees a launch that
/// is not the one its stop began for, and refuses; `world.executed` is what
/// proves nothing was done to B.
///
/// The other half — an operation minted against A refused once B is serving —
/// is asserted beside it, because the two are different properties and only
/// this one protects the shutdown request.
fn released_callback_never_reaches_the_replacement() -> Result<(), String> {
    let gate = Arc::new(Gate::default());
    let world = Arc::new(World::default());
    let started = Arc::new(AtomicU64::new(0));
    let handle = supervisor::start(gated_spec(
        Arc::clone(&gate),
        Arc::clone(&world),
        Arc::clone(&started),
    ));
    let a = ready_child(&handle, 1)?;
    let a_launch = a.payload.token.clone();

    // The update window, then the stop inside it. The callback blocks; the
    // deadline kills A.
    handle.hold();
    let outcome = handle.stop_serving();
    if !matches!(
        outcome,
        StopOutcome::Forced {
            why: Forced::DeadlineElapsed,
            ..
        }
    ) {
        return Err(format!("A was not killed on the deadline: {outcome}"));
    }
    wait_for("A's death", Duration::from_secs(5), || !pid_alive(a.pid))?;

    // The install aborts and supervision resumes into a healthy B.
    handle.resume();
    let b = ready_child(&handle, 2)?;
    let b_launch = b.payload.token.clone();
    if b_launch == a_launch {
        return Err("the replacement reused A's launch identity".into());
    }

    // Only now is the callback let go — with B serving and discoverable.
    gate.open();
    wait_for(
        "the released callback to finish",
        Duration::from_secs(10),
        || !world.refused.lock().unwrap().is_empty() || !world.executed.lock().unwrap().is_empty(),
    )?;

    let executed = world.executed.lock().unwrap().clone();
    if !executed.is_empty() {
        return Err(format!(
            "an authorized shutdown executed on {executed:?} after A was killed"
        ));
    }
    if world.refused.lock().unwrap().as_slice() != [b_launch.clone()] {
        return Err(format!(
            "the released callback did not discover and refuse B: {:?}",
            world.refused.lock().unwrap()
        ));
    }

    // B is still serving, and the late completion changed nothing.
    std::thread::sleep(Duration::from_millis(300));
    if handle.status().state != State::Ready || !pid_alive(b.pid) {
        return Err(format!(
            "the late stop completion disturbed the replacement: {:?}",
            handle.status().state
        ));
    }
    // Fencing property 2, beside it and deliberately not conflated with it.
    if handle.issue(Generation(1)).is_ok() {
        return Err("an operation minted against A was served once B was running".into());
    }
    handle
        .issue(Generation(2))
        .map_err(|stale| format!("B's own generation was refused: {stale}"))?;
    handle.stop();
    Ok(())
}

fn parent_death() -> Result<(), String> {
    parent_death_via("scenario-parent-death")
}

/// Run `scenario` in a helper process that supervises a worker with a
/// grandchild and then dies abruptly, and require the whole tree to follow.
fn parent_death_via(scenario: &str) -> Result<(), String> {
    // **Read the two pids, not to end of stream.** The helper's worker and
    // grandchild inherit this pipe; waiting for its EOF waits for them, so on
    // a platform with no Job Object the check would pass by outliving the
    // grandchild's own sleep instead of by the tree being killed.
    let mut helper = Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
        .arg(scenario)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("helper: {error}"))?;
    let stdout = helper.stdout.take().ok_or("helper has no stdout")?;
    let mut child = None;
    let mut grand = None;
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|error| format!("helper output: {error}"))?;
        if let Some(pid) = line.strip_prefix("child ") {
            child = pid.trim().parse::<u32>().ok();
        } else if let Some(pid) = line.strip_prefix("grand ") {
            grand = pid.trim().parse::<u32>().ok();
        }
        if child.is_some() && grand.is_some() {
            break;
        }
    }
    let status = helper.wait().map_err(|error| format!("helper: {error}"))?;
    if !status.success() {
        return Err(format!("helper failed: {status}"));
    }
    let child = child.ok_or("helper reported no child pid")?;
    let grand = grand.ok_or("helper reported no grandchild pid")?;
    wait_for("orphan death", Duration::from_secs(5), || {
        !pid_alive(child) && !pid_alive(grand)
    })
    .map_err(|_| "the worker tree outlived its dead parent".into())
}

/// The other process of the parent-death check: supervise a worker with a
/// grandchild, say who they are, and die without stopping anything.
fn parent_death_helper() -> ! {
    supervise_then_die(fake_spec(true, quick_budget(), quick_backoff()))
}

/// The helper's body for any spec whose child spawns a grandchild.
fn supervise_then_die(spec: WorkerSpec<FakePayload>) -> ! {
    let handle = supervisor::start(spec);
    let child = match ready_child(&handle, 1) {
        Ok(child) => child,
        Err(why) => {
            eprintln!("helper: {why}");
            std::process::exit(1)
        }
    };
    let grand = wait_for("grandchild", Duration::from_secs(5), || {
        child.payload.grand_pid.lock().unwrap().is_some()
    })
    .map(|()| child.payload.grand_pid.lock().unwrap().expect("checked"));
    let Ok(grand) = grand else {
        eprintln!("helper: no grandchild");
        std::process::exit(1)
    };
    println!("child {}", child.pid);
    println!("grand {grand}");
    // Abrupt: no stop, no drop. The handle table closing is the test.
    std::mem::forget(handle);
    std::process::exit(0)
}

fn dispatch_strictness() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let cases: [(&[&str], &str); 3] = [
        (&["--worker", "bogus"], "bogus"),
        (&["--worker"], "role name"),
        (&["run", "--worker", "fake"], "first argument"),
    ];
    for (args, expect) in cases {
        let output = Command::new(&exe)
            .args(args)
            .output()
            .map_err(|error| error.to_string())?;
        if output.status.code() != Some(2) {
            return Err(format!(
                "{args:?} exited {:?}, wanted 2",
                output.status.code()
            ));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains(expect) {
            return Err(format!(
                "{args:?} refusal does not name `{expect}`: {stderr}"
            ));
        }
    }
    Ok(())
}

// ------------------------------------------------- a supervised sibling program

/// **A sibling is launched, contained and stopped** exactly as a role is: its
/// grandchild dies with it. Readiness alone proves it ran as a sibling — run
/// as `--worker sibling-label` it would have been refused with exit 2.
fn sibling_launched_contained_stopped() -> Result<(), String> {
    let handle = supervisor::start(sibling_spec(true, None, None));
    let child = ready_child(&handle, 1)?;
    if handle.status().role != SIBLING_LABEL {
        return Err(format!("the status names {}", handle.status().role));
    }
    let grand = wait_for("grandchild report", Duration::from_secs(5), || {
        child.payload.grand_pid.lock().unwrap().is_some()
    })
    .map(|()| child.payload.grand_pid.lock().unwrap().expect("checked"))?;
    if !pid_alive(child.pid) || !pid_alive(grand) {
        return Err("the sibling or its grandchild is not alive while ready".into());
    }
    handle.stop();
    if handle.status().state != State::Stopped {
        return Err(format!("stop left state {:?}", handle.status().state));
    }
    wait_for("sibling tree death", Duration::from_secs(5), || {
        !pid_alive(child.pid) && !pid_alive(grand)
    })
    .map_err(|_| "the sibling's tree survived stop — job containment failed".into())
}

/// **A background sibling stays out of the way** (OCR Plan 3.1): created
/// below normal priority by the launch, and in background I/O mode by its
/// own hand — readiness proves the second, since a child that could not
/// enter it answers with a token the probe refuses — and still below normal
/// afterwards, because entering background mode resets the class and
/// `follow_launch_priority` puts it back. A normal child beside it keeps the
/// class this process has.
#[cfg(windows)]
fn background_sibling() -> Result<(), String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, GetPriorityClass, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    fn class_of(pid: u32) -> Result<u32, String> {
        // SAFETY: a query-only handle to a process this test started, closed
        // before returning.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return Err(format!("the child {pid} could not be opened"));
            }
            let class = GetPriorityClass(handle);
            CloseHandle(handle);
            Ok(class)
        }
    }

    let mut spec = sibling_spec(false, None, None);
    spec.priority = supervisor::Priority::Background;
    let mut inner = std::mem::replace(&mut spec.launch, Box::new(|_| Err("launch moved".into())));
    spec.launch = Box::new(move |generation: Generation| {
        let mut plan = inner(generation)?;
        plan.bootstrap
            .extend_from_slice(format!(" {BACKGROUND_WORD}").as_bytes());
        Ok(plan)
    });
    let handle = supervisor::start(spec);
    let child = ready_child(&handle, 1)
        .map_err(|why| format!("the background child never entered background mode: {why}"))?;
    let class = class_of(child.pid);
    handle.stop();
    if class? != BELOW_NORMAL_PRIORITY_CLASS {
        return Err("the background child is not below normal priority".into());
    }

    let normal = supervisor::start(sibling_spec(false, None, None));
    let child = ready_child(&normal, 1)?;
    let class = class_of(child.pid);
    normal.stop();
    // SAFETY: the pseudo-handle of this process.
    let ours = unsafe { GetPriorityClass(GetCurrentProcess()) };
    if class? != ours {
        return Err("a normal child did not keep its parent's priority class".into());
    }
    Ok(())
}

/// **Released for an update**: held, stopped for the swap — the image let go
/// of, provably — and back on resume as a fresh generation, the path an
/// install takes and the one an aborted install takes back.
fn sibling_released_for_update() -> Result<(), String> {
    let handle = supervisor::start(sibling_spec(false, None, None));
    let child = ready_child(&handle, 1)?;
    handle.hold();
    wait_for("hold", Duration::from_secs(5), || {
        handle.status().state == State::Stopping
    })?;
    if !pid_alive(child.pid) {
        return Err("the hold killed the sibling it was meant to keep".into());
    }
    let outcome = handle.stop_serving();
    match &outcome {
        StopOutcome::Forced {
            request: StopRequest::Serving,
            generation: Generation(1),
            why: Forced::GracefulRefused(_),
        } => {}
        other => return Err(format!("the update's stop answered {other}")),
    }
    if !outcome.child_is_gone() {
        return Err("the update's stop did not let go of the sibling".into());
    }
    wait_for("sibling death", Duration::from_secs(5), || {
        !pid_alive(child.pid)
    })?;
    if handle.status().state != State::Stopped {
        return Err(format!(
            "the update's stop left state {:?}",
            handle.status().state
        ));
    }
    handle.resume();
    ready_child(&handle, 2)?;
    handle.stop();
    Ok(())
}

/// **A sibling that is not there is a spawn failure that names the path it
/// looked at**, and runs the restart budget out visibly like any other.
fn missing_sibling() -> Result<(), String> {
    let budget = RestartBudget {
        max_restarts: 0,
        window: Duration::from_secs(60),
    };
    let mut spec = fake_spec(false, budget, quick_backoff());
    spec.role = SIBLING_LABEL;
    spec.program = Program::Sibling("no-such-sibling-program".into());
    let log = record_events(&mut spec);
    let handle = supervisor::start(spec);
    wait_for("exhaustion", Duration::from_secs(5), || {
        handle.status().state == State::Exhausted
    })?;
    let said = log.lock().unwrap().iter().find_map(|(_, kind)| match kind {
        WorkerEventKind::SpawnFailed { error } => Some(error.clone()),
        _ => None,
    });
    match said {
        Some(error)
            if error.contains("no-such-sibling-program") && error.contains("not beside") => {}
        other => return Err(format!("the spawn failure said {other:?}")),
    }
    handle.stop();
    Ok(())
}

/// **Killed at its memory limit, and told apart from a crash.**
///
/// The hoarding child survives its refused allocation — Windows refuses the
/// commit, it does not end the process — so the end here is the supervisor's,
/// on the job's report. The report comes before the exit it caused; the
/// replacement is served; and an ordinary crash of that replacement is
/// reported as a crash, with no memory report beside it.
#[cfg(windows)]
fn sibling_memory_limit() -> Result<(), String> {
    memory_limit_scenario(HOARD_AND_SURVIVE)
}

/// **The same, for the child a ceiling usually meets: one that aborts on the
/// refusal.** Here the exit races the job's report — the process may be gone
/// before the report is read — and the report must still come first, which is
/// what the waiter holding the exit back for the memory watch is for.
///
/// Honestly: with that hold-back removed this still passed five runs out of
/// five, because the runtime's abort is slower than the watch. It pins the
/// outcome; the ordering itself rests on the completion port's order, argued
/// where the waiter releases the watch.
#[cfg(windows)]
fn sibling_memory_limit_abort() -> Result<(), String> {
    memory_limit_scenario(HOARD_AND_ABORT)
}

#[cfg(windows)]
fn memory_limit_scenario(mode: &'static str) -> Result<(), String> {
    const LIMIT: u64 = 256 << 20;
    let mut spec = sibling_spec(false, Some((1, mode)), Some(LIMIT));
    let log = record_events(&mut spec);
    let handle = supervisor::start(spec);
    let first = ready_child(&handle, 1)?;

    // The hoarder dials back for its go; noting its pid first is the point.
    let mut go = None;
    wait_for(
        "the hoarder to ask for its go",
        Duration::from_secs(10),
        || match first.payload.listener.accept() {
            Ok((stream, _)) => {
                go = Some(stream);
                true
            }
            Err(_) => false,
        },
    )?;
    go.expect("accepted")
        .write_all(b"g")
        .map_err(|error| format!("the go: {error}"))?;

    let reported = |generation: Generation| {
        log.lock().unwrap().iter().position(|(seen, kind)| {
            *seen == generation
                && matches!(kind, WorkerEventKind::MemoryLimitReached { limit_bytes } if *limit_bytes == LIMIT)
        })
    };
    let exited = |generation: Generation| {
        log.lock().unwrap().iter().position(|(seen, kind)| {
            *seen == generation && matches!(kind, WorkerEventKind::Exited { .. })
        })
    };
    wait_for("the memory limit report", Duration::from_secs(15), || {
        reported(Generation(1)).is_some()
    })
    .map_err(|_| "the hoarding sibling was never reported at its memory limit".to_owned())?;
    wait_for("the hoarder's death", Duration::from_secs(5), || {
        !pid_alive(first.pid)
    })
    .map_err(|_| "the sibling outlived its memory limit".to_owned())?;
    let second = ready_child(&handle, 2)?;
    match (reported(Generation(1)), exited(Generation(1))) {
        (Some(report), Some(exit)) if report < exit => {}
        order => return Err(format!("the report and the exit came as {order:?}")),
    }
    if !handle
        .status()
        .last_error
        .is_some_and(|error| error.contains("memory limit"))
    {
        return Err(format!(
            "the status does not say why: {:?}",
            handle.status().last_error
        ));
    }

    kill_pid(second.pid);
    ready_child(&handle, 3)?;
    if exited(Generation(2)).is_none() {
        return Err("the crash of the replacement was not reported".into());
    }
    if reported(Generation(2)).is_some() {
        return Err("an ordinary crash was reported as a memory limit".into());
    }
    handle.stop();
    Ok(())
}
