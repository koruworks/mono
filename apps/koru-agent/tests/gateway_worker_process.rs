// SPDX-License-Identifier: MPL-2.0

//! **Real `koru-agent.exe --worker gateway` children.** Every case here spawns
//! the shipped Agent image as a worker and asserts what a supervisor would see:
//! the exit code, what it said on stderr, whether it served, and what it did
//! *not* touch.
//!
//! ## Why a process suite and not a unit test
//!
//! The thing under test is an ordering inside `main` and a pipe between two
//! processes. `dispatch` running before the Agent's CLI, its log sink, its
//! single-instance claim, its tray, its global chord and dev's Windows
//! notification-identity self-restart is not a property any type holds — it is
//! a property of where one call sits, and the only honest proof that a worker
//! child never half-becomes an Agent is a worker child that did not. The
//! textual half of that ordering is pinned beside the role, in
//! `src/gateway_worker/tests.rs`; this file is the behavioural half.
//!
//! ## Every child runs on its own temporary root
//!
//! A rooted Agent takes nothing machine-wide, and every endpoint name carries a
//! digest of the root, so these children never meet each other or the gateway a
//! developer has open. Nothing here writes to `%LOCALAPPDATA%`.
//!
//! ## The harness plays the Agent
//!
//! A supervised gateway registers over the private, root-qualified bridge
//! before it becomes discoverable, and refuses to start without one. So the
//! harness binds `koru_ipc::AGENT_GATEWAY_BRIDGE` under the test root, answers
//! one `Registration` with the real `BridgeAuthority`, and holds the connection
//! open — which is also what makes the bootstrap's capability, its channel, its
//! root fingerprint and **the fingerprint of this exact image** meaningful
//! rather than decorative.
//!
//! That harness started here and is now [`gateway_process`], shared with the
//! five gateway integration suites that came into this package when the
//! standalone `koru-api-gateway.exe` was retired. **What stays local is the
//! part it cannot serve**: the cases below hand the child a bootstrap that is
//! deliberately wrong, or no bootstrap at all, so they spawn by hand and never
//! reach a running gateway.
//!
//! Plan 4 is what puts a real `WorkerSpec` on the Agent's side. This suite
//! proves the child is possible and provable without one.

mod gateway_process;

use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use koru_network_bridge::{BRIDGE_VERSION, BridgeCapability, LaunchBootstrap};

use gateway_process::{Gateway, agent_exe, segment};

/// How long a refusal gets. It should be milliseconds — nothing has opened.
const REFUSAL: Duration = Duration::from_secs(30);

// --------------------------------------------------------------- the fixture

/// A gateway child on a temporary root of its own.
///
/// The child, its bootstrap and the harness bridge behind it are
/// [`gateway_process::Gateway`]; what this adds is the `TempDir` that owns the
/// root, and the named last component the non-ASCII case needs.
struct Worker {
    _home: tempfile::TempDir,
    gateway: Gateway,
}

impl Worker {
    /// Start a supervised child on a fresh root whose last component is
    /// `directory`.
    fn start(directory: &str) -> Self {
        let home = tempfile::tempdir().expect("a temporary home");
        let gateway = Gateway::start(&home.path().join(directory));
        Self {
            _home: home,
            gateway,
        }
    }

    /// Wait until the published rendezvous answers, then read the record.
    fn published(&mut self) -> koru_api_discovery::GatewayHello {
        let hello = gateway_process::hello_with_scopes(
            "koru-agent-gateway-worker-test",
            &[koru_api_client::CONTROL_SCOPE],
        );
        self.gateway.published(&hello)
    }
}

/// What a refused launch said and what it cost.
struct Refusal {
    code: Option<i32>,
    stderr: String,
}

/// Run `koru-agent <args>` with `stdin` on its bootstrap pipe, and wait for it
/// to refuse. `stdin` of `None` closes the pipe without writing — which is what
/// a missing bootstrap looks like.
fn refusal(args: &[&str], stdin: Option<&[u8]>) -> Refusal {
    let mut child = Command::new(agent_exe())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the agent");
    {
        let mut pipe = child.stdin.take().expect("the bootstrap pipe");
        if let Some(bytes) = stdin {
            let _ = pipe.write_all(bytes);
            let _ = pipe.flush();
        }
    }
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    let deadline = Instant::now() + REFUSAL;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return Refusal {
                code: status.code(),
                stderr,
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`{args:?}` neither refused nor exited within {REFUSAL:?}: {stderr}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The Agent's own help, which a role's refusal must never print: seeing it
/// would mean `cli::parse` had run, and everything else runs after that.
const AGENT_HELP: &str = "koru-agent — the Koru Agent. It owns the device.";

// ------------------------------------------------------------- the refusals

#[test]
fn an_unknown_role_is_refused_with_exit_2() {
    let refused = refusal(&["--worker", "frobnicate"], None);
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("unknown worker role `frobnicate`"),
        "{}",
        refused.stderr
    );
    // Both roles are offered, so a person who mistyped one can see the other.
    assert!(
        refused.stderr.contains("gateway") && refused.stderr.contains("sessions"),
        "{}",
        refused.stderr
    );
}

#[test]
fn a_misplaced_role_argument_is_refused_with_exit_2() {
    let home = tempfile::tempdir().expect("a temporary home");
    let root = home.path().to_string_lossy().into_owned();
    let refused = refusal(&["--root", &root, "--worker", "gateway"], None);
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(
        refused
            .stderr
            .contains("`--worker` must be the first argument"),
        "{}",
        refused.stderr
    );
}

#[test]
fn the_role_refuses_an_argument_it_does_not_understand() {
    let refused = refusal(&["--worker", "gateway", "--supervised"], None);
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("unknown argument `--supervised`"),
        "{}",
        refused.stderr
    );
}

/// The bug the sessions role had and this one does not: `--root` followed by a
/// flag is a forgotten path, never a directory named `--supervised`.
#[test]
fn a_flag_where_the_root_should_be_is_refused_rather_than_served() {
    let refused = refusal(&["--worker", "gateway", "--root", "--supervised"], None);
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("--root needs a path"),
        "{}",
        refused.stderr
    );
}

/// **The ordering, proved by a process.** `--worker` is a token the Agent's own
/// strict CLI does not know: if `dispatch` did not run first, this line would
/// be rejected by `cli::parse` and the Agent's help would be on stderr. It is
/// the gateway role's usage instead, so nothing after the parse — the log sink,
/// the single-instance claim, the tray, the global chord, the Windows
/// notification identity and its self-restart — was reached.
#[test]
fn a_missing_bootstrap_is_refused_with_exit_2_before_any_agent_machinery() {
    let home = tempfile::tempdir().expect("a temporary home");
    let root = home.path().to_string_lossy().into_owned();
    let refused = refusal(&["--worker", "gateway", "--root", &root], None);
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(
        refused
            .stderr
            .contains("could not read the Agent bootstrap"),
        "{}",
        refused.stderr
    );
    assert!(
        refused
            .stderr
            .contains("usage: koru-agent --worker gateway"),
        "{}",
        refused.stderr
    );
    assert!(!refused.stderr.contains(AGENT_HELP), "{}", refused.stderr);
    // The Agent's own directory is where `run` mints `device.json`. Nothing
    // created it, and nothing created a store either.
    assert!(!home.path().join("agent").exists());
}

#[test]
fn a_malformed_bootstrap_is_refused_with_exit_2() {
    let home = tempfile::tempdir().expect("a temporary home");
    let root = home.path().to_string_lossy().into_owned();
    let refused = refusal(
        &["--worker", "gateway", "--root", &root],
        Some(b"{ not a launch bootstrap }\n"),
    );
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("invalid Agent bootstrap"),
        "{}",
        refused.stderr
    );
    assert!(!refused.stderr.contains(AGENT_HELP), "{}", refused.stderr);
}

// ------------------------------------------------- the supervised refusals
//
// A well-formed bootstrap that describes a different gateway is a *runtime*
// refusal, not a command line the child got wrong, so it costs exit 1. Each of
// these was a `validate` branch with no process-level case at all — the gap
// Plan 1's inventory recorded in Part F.4.

/// Start a child whose bootstrap has been mangled, and report how it refused.
fn supervised_refusal(directory: &str, mangle: impl FnOnce(&mut LaunchBootstrap)) -> Option<i32> {
    let home = tempfile::tempdir().expect("a temporary home");
    let root = home.path().join(directory);
    let store_root = koru_channel::store_root_under(&root);
    std::fs::create_dir_all(&store_root).expect("the rooted store directory");
    let mut bootstrap = LaunchBootstrap {
        device_id: None,
        run_authority: None,
        bots_engine: None,
        launch_id: uuid::Uuid::new_v4().to_string(),
        bridge_version: BRIDGE_VERSION,
        channel: segment().into(),
        root_fingerprint: koru_network_bridge::fingerprint(store_root.to_string_lossy().as_bytes()),
        installation_fingerprint: koru_network_bridge::fingerprint(
            std::fs::read(agent_exe()).expect("this test's own agent image"),
        ),
        capability: BridgeCapability::generate(),
        session_lifecycle: None,
    };
    mangle(&mut bootstrap);
    let mut line = serde_json::to_vec(&bootstrap).expect("encode the bootstrap");
    line.push(b'\n');
    let refused = refusal(
        &["--worker", "gateway", "--root", &root.to_string_lossy()],
        Some(&line),
    );
    assert!(
        refused.stderr.contains("does not match this gateway")
            || refused
                .stderr
                .contains("belongs to a different gateway image"),
        "{}",
        refused.stderr
    );
    assert!(!refused.stderr.contains(AGENT_HELP), "{}", refused.stderr);
    refused.code
}

#[test]
fn a_bootstrap_from_a_different_image_is_refused() {
    assert_eq!(
        supervised_refusal("image", |bootstrap| {
            bootstrap.installation_fingerprint = koru_network_bridge::fingerprint(b"another exe");
        }),
        Some(1)
    );
}

#[test]
fn a_bootstrap_for_another_channel_is_refused() {
    assert_eq!(
        supervised_refusal("channel", |bootstrap| {
            bootstrap.channel = format!("{}-elsewhere", segment());
        }),
        Some(1)
    );
}

#[test]
fn a_bootstrap_for_another_root_is_refused() {
    assert_eq!(
        supervised_refusal("root", |bootstrap| {
            bootstrap.root_fingerprint = koru_network_bridge::fingerprint(b"another root");
        }),
        Some(1)
    );
}

// ------------------------------------------------------------- it serves

/// The whole of the happy path: a valid bootstrap serves, the child is the
/// gateway and nothing else, and closing the pipe ends it.
#[test]
fn a_valid_bootstrap_serves_takes_no_agent_singleton_and_ends_with_its_parent() {
    let mut worker = Worker::start("rehearsal");
    let published = worker.published();

    // It is *this* launch's gateway: the record carries the launch id the
    // bootstrap named, which is what a supervisor adopts a child by.
    assert_eq!(
        published.supervisor_launch_id.as_deref(),
        Some(worker.gateway.launch_id())
    );
    assert_eq!(published.segment, segment());
    assert!(published.api_endpoint.starts_with("http://127.0.0.1:"));
    // No session host in this fixture, so the sessions adapter is absent —
    // truthfully, rather than by failing to start.
    assert!(!published.capabilities.iter().any(|c| c == "sessions"));

    // **No Agent singleton.** If the child had reached `koru_agent::run` it
    // would be holding this endpoint for this root, and binding it would fail.
    let agent_endpoint = koru_ipc::Endpoint::for_service_channel_root(
        koru_ipc::AGENT,
        segment(),
        worker.gateway.root(),
    )
    .expect("the rooted Agent endpoint");
    let claimed = koru_ipc::Listener::bind(agent_endpoint)
        .expect("no gateway worker claims the Agent's single-instance endpoint");
    drop(claimed);

    // **No Agent machinery ran at all.** `<root>/agent/` is where `run` mints
    // `device.json`, writes the reminder ledger and keeps the desktop
    // preferences that back the tray; the store directory beside it is the
    // gateway's own and is expected.
    assert!(
        !worker.gateway.root().join("agent").exists(),
        "a gateway worker created the Agent's own directory"
    );
    assert!(
        koru_channel::store_root_under(worker.gateway.root())
            .join("logs")
            .exists()
    );

    // **Never the identity self-restart.** That path spawns `current_exe()`
    // again with the original arguments and returns; this child is still the
    // one that was started, and it is still serving.
    assert!(
        worker.gateway.try_wait().is_none(),
        "the gateway worker replaced itself"
    );

    // **Parent death.** The supervisor held the write end for the child's whole
    // life; letting it go is the cross-platform signal, and the child stops.
    worker.gateway.drop_pipe();
    let status = worker
        .gateway
        .wait_for_exit(Duration::from_secs(60))
        .expect("the gateway worker ends when its parent lets the pipe go");
    assert!(
        status.success(),
        "a parent death should be a clean stop, not a crash: {status}"
    );
}

/// A root whose path is not ASCII is a path like any other: the endpoint name,
/// the store directory and the published record all carry it unchanged.
#[test]
fn a_non_ascii_rooted_path_serves() {
    let mut worker = Worker::start("Κόρου 日本語 café");
    let published = worker.published();
    assert_eq!(
        published.supervisor_launch_id.as_deref(),
        Some(worker.gateway.launch_id())
    );
    assert!(koru_channel::store_root_under(worker.gateway.root()).exists());
    worker.gateway.drop_pipe();
    assert!(
        worker
            .gateway
            .wait_for_exit(Duration::from_secs(60))
            .expect("the worker ends with its parent")
            .success()
    );
}
