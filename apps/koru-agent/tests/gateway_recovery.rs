// SPDX-License-Identifier: MPL-2.0

//! **What survives a Gateway that is killed under work already in flight.**
//!
//! The host is `koru-agent.exe --worker gateway`, supervised by a real
//! `koru-agent.exe` on a temporary root. Every case here does the same three
//! things: arrange work whose outcome a client has been told something about,
//! kill the process that is serving, and assert what the client sees once
//! service is back — that a durable acknowledgement was durable, that nothing
//! was replayed or duplicated, that what the record promised before the kill it
//! promises after it, and that the admission the dead host issued is refused.
//!
//! **This suite was written as a comparison.** Plan 6 ran each assertion twice,
//! against the standalone `koru-api-gateway.exe` and against the supervised
//! worker, while both hosts existed — that was the evidence the rehost changed
//! nothing. Plan 7 deleted the standalone binary, and with it the second arm.
//! What is left is not half a comparison: it is the recovery contract of the
//! host that ships, and the assertions are the ones both hosts passed.
//!
//! **The `_worker` suffix is kept deliberately.** It names a distinction that
//! no longer exists in this file, which normally argues for dropping it — but
//! ADR-317 records the comparison, and `docs/contributing/release/VERIFICATION.md` — the
//! half a machine cannot run — lists these cases by name as the half that
//! survived, so renaming them would orphan the record that licensed them.
//!
//! ## The fixture, and why it is this small
//!
//! [`Host`] starts an Agent on a temporary root, hands back a discovered
//! client, kills whichever process is serving, and waits for the supervisor to
//! put service back. That is all it does.
//!
//! There is deliberately no second fixture layer, no two-root harness, no fake
//! installation resolver and no process-tree cleanup. `Endpoint::for_service_root`
//! owns root isolation — every endpoint name carries a digest of the root — and
//! the Job Object owns the process tree.
//!
//! **The kill goes through the published record.** `instance_id` is
//! `"<pid>-<millis>"`, built in `crates/koru-api-gateway/src/discovery.rs`, so the
//! pid of the process that is *actually serving* is already on the record a
//! client holds. This fixture owns a `Child` for the Agent and never one for
//! the gateway, which is the honest shape: the gateway is the supervisor's
//! child, not the test's.
//!
//! **Recovery is the supervisor's.** Nothing here restarts a gateway; the
//! Agent replaces its child inside a bounded budget and the test waits for a
//! record with a new `instance_id`. A test that started the replacement itself
//! would be proving its own fixture.
//!
//! ## What is asserted elsewhere, and why not here
//!
//! - **AI streams.** `gateway_ai.rs::every_in_flight_stream_reaches_one_terminal_outcome_when_the_host_dies`
//!   arms two streams against the deterministic mock provider and kills the
//!   host under them. It reaches that provider with `--deterministic-ai-test`,
//!   which the role accepts only in a build with debug assertions, and it
//!   spawns its own gateway child rather than asking a supervisor for one — the
//!   Agent builds its child's argument list and has no reason to offer a mock.
//!   What this file says about the AI composition instead is in
//!   `the_published_record_replaces_its_identity_and_keeps_its_contract_worker`:
//!   whatever the composition offered before the kill, it offers after it.
//! - **Workspace *migration*, and the archive/soft-delete half of removal.**
//!   Both routes take a vouched scope, and a grant is minted only for a process
//!   running the `koru` beside the Agent, so no test process can ever hold one —
//!   that mechanism is itself asserted, in `lifecycle.rs::a_caller_that_is_not_the_koru_app_is_refused_a_first_party_grant`.
//!   The five-step operation is pinned against the router with a minted vouched
//!   admission in `gateway_lifecycle.rs`, which builds `GatewayState` in
//!   process and is host-independent by construction. What *is* driven through
//!   a real host here is the removal a manage-workspaces window performs, and
//!   the boundary itself: the vouched routes refuse an ungranted caller
//!   identically before and after a kill.
//!
//! ## Every host runs on its own temporary root
//!
//! Nothing here writes to `%LOCALAPPDATA%`, registers a hotkey, creates a tray
//! icon or touches the gateway a developer has open.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use koru_agent_wire::{FRAME_VERSION, FromShell, PROTOCOL_VERSION, Request, Response, ShellHello};
use koru_api_client::{Client, Connection, Event};
use koru_api_discovery::{ClientHello, GatewayHello};
use koru_store_core::ids::DocumentId;
use koru_store_core::manifest::Manifest;

/// How long a cold host gets to open Store, AI and events and publish itself.
/// Generous: this is a real process on a machine that may be running the rest
/// of the workspace suite.
const READY: Duration = Duration::from_secs(90);

/// How long a supervised replacement gets. Longer than [`READY`] by the
/// supervisor's own backoff, and by the moment it takes to notice the death.
const RECOVERED: Duration = Duration::from_secs(180);

/// How long a bounded wait for an answer that should already be true gets.
const PROMPT: Duration = Duration::from_secs(30);

/// What a window that reads and edits asks for. Naming scopes filters rather
/// than adds, so everything a case below uses is here.
const WINDOW_SCOPES: &[&str] = &[
    "store.read_documents",
    "store.edit_documents",
    "settings.read",
    "settings.write",
    koru_api_client::EVENTS_READ_SCOPE,
    // Reading the journal and writing to it are two scopes, and a default
    // session holds both — but naming any scope filters rather than adds, so a
    // caller that names the read one and then appends has to name this one too.
    "events.write",
    // Named rather than spelled: the gateway owns this string and a copy here
    // would go on asking for whichever half kept the old one.
    koru_api_gateway_core::MANAGE_WORKSPACES_SCOPE,
];

fn segment() -> &'static str {
    koru_channel::segment()
}

fn agent_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_koru-agent"))
}

// ------------------------------------------------------------------ the host

/// A gateway serving one temporary root, and the Agent that supervises it.
struct Host {
    root: tempfile::TempDir,
    /// The process this fixture started: the Agent. The gateway itself is its
    /// child, and this fixture never owns a handle to it — it is reached, and
    /// killed, by the pid on the record it publishes.
    process: Child,
}

impl Host {
    /// Start an Agent and wait until its gateway is published on its root.
    fn start() -> Self {
        let root = tempfile::tempdir().expect("a temporary root");
        let process = Command::new(agent_exe())
            .arg("--root")
            .arg(root.path())
            // `--console` so a failure prints where the harness can see it
            // rather than into a notification area a rooted Agent does not
            // create anyway.
            .arg("--console")
            .env("KORU_STORE_NO_DIALOG", "1")
            .stdin(std::process::Stdio::null())
            .spawn()
            .expect("spawn the agent");
        let mut host = Self { root, process };
        host.wait_until_the_agent_answers();
        // The Agent ensures a gateway at startup; asking is how a window asks,
        // and it is what makes the wait below bounded by the supervisor rather
        // than by a race with startup.
        host.ensure_the_gateway();
        host.await_service("after the host started");
        host
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    // ------------------------------------------------------------- discovery

    fn hello(&self, scopes: &[&str]) -> ClientHello {
        ClientHello {
            product: "koru-gateway-recovery".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            segment: segment().into(),
            api_version: koru_api_wire::API_VERSION,
            requested_scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
            grant: None,
        }
    }

    /// The record the gateway on this root is publishing right now.
    fn record(&self, scopes: &[&str], what: &str) -> GatewayHello {
        koru_api_discovery::connect_at(segment(), self.root(), &self.hello(scopes)).unwrap_or_else(
            |error| panic!("no gateway is published {what} on the supervised host: {error}"),
        )
    }

    /// A client whose credential is pinned to exactly this record. It cannot
    /// rediscover, which is what makes it a *stale* credential once the host
    /// that minted it is gone.
    fn pinned(&self, record: &GatewayHello) -> Client {
        Client::from_gateway(record.clone()).expect("build a client from the record")
    }

    /// A client that rediscovers, which is what every real surface holds.
    fn client(&self, scopes: &[&str]) -> Client {
        Client::open_at_with_scopes(segment(), self.root(), scopes)
            .expect("build a renewable gateway client")
    }

    fn await_service(&mut self, what: &str) {
        let deadline = Instant::now() + READY;
        loop {
            if koru_api_discovery::connect_at(segment(), self.root(), &self.hello(&[])).is_ok() {
                return;
            }
            if let Some(status) = self.process.try_wait().expect("try_wait") {
                panic!("the Agent exited with {status} {what}");
            }
            assert!(
                Instant::now() < deadline,
                "the supervised host published nothing {what} within {READY:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // --------------------------------------------------------- the interrupt

    /// **Kill the process that is serving, whichever host this is.**
    ///
    /// Killed rather than asked, on purpose: a graceful stop would let the host
    /// finish work it had already reported as durable, which is the one thing
    /// these assertions must not be allowed to depend on. It is also the honest
    /// shape of a crash — an exit nobody asked the supervisor for.
    ///
    /// The gateway is a *child* of the process this fixture started, so there
    /// is no `Child` handle to kill: the pid comes off the record a client
    /// holds, which is the only place this test can learn it.
    fn interrupt(&mut self, serving: &GatewayHello) {
        let pid = serving_pid(serving);
        assert_ne!(
            pid,
            self.process.id(),
            "the Agent published its own pid as its gateway child's"
        );
        kill_pid(pid);
        let deadline = Instant::now() + PROMPT;
        while pid_alive(pid) {
            assert!(
                Instant::now() < deadline,
                "the gateway did not die when it was killed"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// **Wait for service to come back, and answer with the record that
    /// replaced it.**
    ///
    /// This starts nothing. The gateway is put back by its own supervisor
    /// inside a bounded budget, and a test that restarted it by hand would be
    /// proving its own fixture rather than the recovery.
    fn recover(&mut self, previous: &GatewayHello) -> GatewayHello {
        let deadline = Instant::now() + RECOVERED;
        loop {
            if let Ok(found) =
                koru_api_discovery::connect_at(segment(), self.root(), &self.hello(&[]))
                && found.instance_id != previous.instance_id
            {
                break;
            }
            if let Some(status) = self.process.try_wait().expect("try_wait") {
                panic!("the Agent exited with {status} while its gateway was being replaced");
            }
            assert!(
                Instant::now() < deadline,
                "the supervisor did not replace its gateway within {RECOVERED:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let replacement = self.record(&[], "after the recovery");
        assert_ne!(
            replacement.instance_id, previous.instance_id,
            "the supervisor answered with the record of the gateway that was killed"
        );
        replacement
    }

    /// **What was not the Gateway is still there.**
    ///
    /// The whole crash-isolation claim: the Agent answers, promptly, and its
    /// sessions worker is still bound. A host with nothing above it could not
    /// be asked this at all, which is what the supervision bought.
    fn survivors_still_answer(&self) {
        let asked = Instant::now();
        let mut shell = self.shell();
        assert!(
            matches!(shell.ask(Request::Status), Response::Status { .. }),
            "the Agent stopped answering when its gateway died"
        );
        // The two shapes a dictation chord turns into, both of which have to
        // work while the Gateway is gone.
        assert!(
            matches!(
                shell.ask(Request::DictationState {
                    route: u64::MAX,
                    state: koru_agent_wire::DictationState::Refused,
                }),
                Response::Failed { .. }
            ),
            "a routed dictation state was not answered while the gateway was down"
        );
        let round_trip = asked.elapsed();
        assert!(
            round_trip < Duration::from_secs(10),
            "the Agent took {round_trip:?} to answer after its gateway died"
        );
        assert!(
            koru_ipc::probe(&self.session_endpoint()),
            "the sessions worker went with the gateway"
        );
    }

    // ------------------------------------------------------- the Agent's side

    fn agent_endpoint(&self) -> koru_ipc::Endpoint {
        koru_ipc::Endpoint::for_service_root(koru_ipc::AGENT, segment(), self.root())
            .expect("a rooted Agent endpoint")
    }

    fn session_endpoint(&self) -> koru_ipc::Endpoint {
        koru_ipc::Endpoint::for_service_root(koru_ipc::SESSION_HOST, segment(), self.root())
            .expect("a rooted sessions endpoint")
    }

    fn wait_until_the_agent_answers(&self) {
        let deadline = Instant::now() + PROMPT;
        while koru_ipc::connect(&self.agent_endpoint()).is_err() {
            assert!(Instant::now() < deadline, "the agent never accepted");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn shell(&self) -> Shell {
        Shell::greet(&self.agent_endpoint())
    }

    fn ensure_the_gateway(&self) {
        let mut shell = self.shell();
        koru_ipc::set_timeouts(&shell.stream, Some(Duration::from_secs(300)));
        assert_eq!(
            shell.ask(Request::EnsureService {
                service: koru_agent_wire::Service::ApiGateway,
            }),
            Response::Ok,
            "the gateway did not come up"
        );
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Asked politely first, so the Store closes its databases inside a
        // directory `TempDir` is about to remove. On Windows that removal fails
        // *silently* when a handle is still open, and the root is abandoned
        // with its databases in it.
        if let Ok(stream) = koru_ipc::connect(&self.agent_endpoint()) {
            let mut write = &stream;
            let _ = koru_ipc::frame::write_frame(
                &mut write,
                FRAME_VERSION,
                &ShellHello {
                    product: "koru-gateway-recovery".into(),
                    version: "0".into(),
                    protocol_version: PROTOCOL_VERSION,
                },
            );
            let mut read = &stream;
            let _ = koru_ipc::frame::read_raw(&mut read);
            let _ = koru_ipc::frame::write_frame(
                &mut write,
                FRAME_VERSION,
                &FromShell::Request {
                    id: 1,
                    body: Request::Shutdown,
                },
            );
            let _ = write.flush();
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut exited = false;
        while Instant::now() < deadline {
            if matches!(self.process.try_wait(), Ok(Some(_))) {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !exited {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
        // And anything still answering on this root that this fixture did not
        // start — a replacement the supervisor made after the last assertion,
        // for instance. A leaked child inherits the harness's stdout, so
        // `cargo test` never sees EOF and a run in which every test passed
        // reads as a hang.
        let deadline = Instant::now() + Duration::from_secs(15);
        while koru_api_discovery::probe_at(segment(), self.root()).is_ok() {
            if let Ok(found) = koru_api_discovery::connect_at(
                segment(),
                self.root(),
                &self.hello(&[koru_api_client::CONTROL_SCOPE]),
            ) && let Ok(client) = Client::from_gateway(found)
            {
                let _ = client.store().shutdown();
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// **The pid of whichever process is serving, off the record a client holds.**
///
/// `instance_id` is `format!("{pid}-{millis}")` in
/// `crates/koru-api-gateway/src/discovery.rs`. It is the only place this test can
/// learn which process to kill — the gateway is the supervisor's child, not
/// this fixture's. Parsed rather than assumed: if the correlation id ever
/// changes shape this says so, instead of killing pid 0 and reporting a
/// mystery.
fn serving_pid(record: &GatewayHello) -> u32 {
    record
        .instance_id
        .split('-')
        .next()
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| {
            panic!(
                "the published instance id is not `<pid>-<millis>`: {}",
                record.instance_id
            )
        })
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

/// One connection to the Agent, driven by hand — the same frames a shell sends.
struct Shell {
    stream: koru_ipc::Stream,
    next_id: u64,
}

impl Shell {
    fn greet(endpoint: &koru_ipc::Endpoint) -> Self {
        let stream = koru_ipc::connect(endpoint).expect("connect to the agent");
        koru_ipc::set_timeouts(&stream, Some(Duration::from_secs(60)));
        let mut shell = Self { stream, next_id: 1 };
        let mut write = &shell.stream;
        koru_ipc::frame::write_frame(
            &mut write,
            FRAME_VERSION,
            &ShellHello {
                product: "koru-gateway-recovery".into(),
                version: "0".into(),
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .expect("say hello");
        write.flush().expect("flush the hello");
        let mut read = &shell.stream;
        koru_ipc::frame::read_raw(&mut read).expect("the agent greeted back");
        shell.next_id = 1;
        shell
    }

    fn ask(&mut self, body: Request) -> Response {
        let id = self.next_id;
        self.next_id += 1;
        let mut write = &self.stream;
        koru_ipc::frame::write_frame(&mut write, FRAME_VERSION, &FromShell::Request { id, body })
            .expect("send the request");
        write.flush().expect("flush the request");
        loop {
            let mut read = &self.stream;
            let (_, frame) =
                koru_ipc::frame::read_frame::<_, koru_agent_wire::FromAgent>(&mut read)
                    .expect("read a frame");
            if let koru_agent_wire::FromAgent::Response { id: got, body } = frame
                && got == id
            {
                return body;
            }
        }
    }
}

// ------------------------------------------------------------ shared helpers

/// A profile and a workspace to work in, through the ordinary catalog surface.
fn workspace(client: &Client, name: &str) -> koru_store_core::ids::WorkspaceId {
    let profile = client
        .store()
        .create_profile(name, None, None, Some("en"))
        .expect("create a profile");
    client
        .store()
        .create_workspace(profile.id, "Workspace")
        .expect("create a workspace")
        .id
}

fn drain(connection: &mut Connection, manifest: &mut Manifest) {
    loop {
        match connection.next_event() {
            Ok(Some(Event::Updates { updates, .. })) => {
                for update in updates {
                    manifest.import(&update).expect("import an update");
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
}

fn within(what: &str, budget: Duration, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("{what} did not happen within {budget:?}");
}

// ------------------------------------------------------ the published record

/// **The contract half survives the kill; the identity half must not.**
///
/// A client finds the Gateway by dialling the root-qualified rendezvous and
/// reading one record. Two halves of it mean different things and a rehost can
/// break them in opposite directions:
///
/// - the *contract* — API version, channel, build version, capabilities — is
///   what a client decides it can talk to. Identical across the kill, or a
///   window that reconnected would find a different product. **This is also
///   where the AI composition is checked across the kill**: whatever the
///   composition offered before, it offers after.
/// - the *identity* — instance, endpoint, credential — says this is a different
///   process. It must change, and the predecessor's admission must be refused,
///   or a client could go on using an authority the new host never issued.
///
/// `supervisor_launch_id` belongs to the identity half and is the sharpest of
/// them: a supervised child publishes the launch it was started *for*, so a
/// record still carrying the killed child's launch would mean the supervisor
/// re-served a corpse.
#[test]
fn the_published_record_replaces_its_identity_and_keeps_its_contract_worker() {
    let mut host = Host::start();
    let before = host.record(&[], "before the kill");
    let stale = host.pinned(&before);
    stale.store().status().expect("the first host serves");

    host.interrupt(&before);
    host.survivors_still_answer();
    let after = host.recover(&before);

    assert_eq!(before.api_version, after.api_version);
    assert_eq!(before.segment, after.segment);
    assert_eq!(before.gateway_version, after.gateway_version);
    assert_eq!(
        before.capabilities, after.capabilities,
        "the replacement published a different capability set"
    );

    assert_ne!(
        before.instance_id, after.instance_id,
        "a replacement that reuses the instance id is indistinguishable from the host it replaced"
    );
    assert_ne!(
        before.session.expose(),
        after.session.expose(),
        "the replacement re-served its predecessor's admission"
    );

    let first = before
        .supervisor_launch_id
        .as_deref()
        .expect("a supervised child publishes the launch it was started for");
    let second = after
        .supervisor_launch_id
        .as_deref()
        .expect("the replacement publishes its own launch");
    assert_ne!(
        first, second,
        "the killed child was still the one being published"
    );

    // **Stale credentials are refused.** It does not matter which half refuses
    // first — the address may be gone or the bearer may be unknown. The
    // invariant is that an admission does not carry across a replacement.
    assert!(
        stale.store().status().is_err(),
        "a client holding the killed gateway's record was still served"
    );
    host.pinned(&after)
        .store()
        .status()
        .expect("the replacement serves");
}

// ------------------------------------------- ordinary reads, edits, and sync

/// **A durable acknowledgement outlives the host that gave it, exactly once.**
///
/// `Ack { durable: true }` means the Store committed the bytes, not that a
/// process still holding them says so. The host is killed with the sync socket
/// open, immediately after the acknowledgement: no graceful stop, no chance to
/// finish anything it had already claimed was on the disk.
///
/// Three things are asserted afterwards, and the third is the one this plan
/// exists for:
///
/// - the acknowledged document is there, read back through a *freshly
///   discovered* client, because the question is what the store holds and not
///   what the first client's replica remembers;
/// - the workspace came back with it, so this is a store that reopened rather
///   than one that started empty;
/// - **it is there once.** A recovery that replayed an uncertain mutation would
///   put the same document in the manifest twice, and every other assertion
///   here would still pass.
#[test]
fn an_acknowledged_edit_survives_the_kill_and_is_not_duplicated_worker() {
    let mut host = Host::start();
    let client = host.client(WINDOW_SCOPES);
    let workspace = workspace(&client, "Recovery");
    let manifest_id = client.store().manifest(workspace).expect("a manifest id");
    let snapshot = client
        .store()
        .snapshot(workspace, manifest_id)
        .expect("a snapshot");
    let mut manifest = Manifest::from_snapshot(&snapshot).expect("a manifest");
    let room = format!("workspace:{}:manifest", workspace.to_storage());
    let discovery = client.store().discovery();
    let mut connection = Connection::open(&discovery).expect("open a sync connection");
    connection
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("a read timeout");
    connection
        .join(&room, &manifest.version())
        .expect("join the room");
    drain(&mut connection, &mut manifest);

    let sent = manifest.version();
    let acknowledged = DocumentId::new();
    manifest
        .add_document(acknowledged, "Acknowledged before the kill", "page", None)
        .expect("add a document");
    let batch = connection
        .send_update(
            &room,
            manifest.export_updates_since(&sent).expect("updates"),
        )
        .expect("send the update");
    let deadline = Instant::now() + PROMPT;
    let mut durable = false;
    while !durable {
        if let Ok(Some(Event::Acked { ack, .. })) = connection.next_event()
            && ack.batch == batch
        {
            durable = ack.durable;
        }
        assert!(
            Instant::now() < deadline,
            "the gateway never acknowledged the update as durable"
        );
    }

    let before = host.record(&[], "before the kill");
    drop(connection);
    host.interrupt(&before);
    host.survivors_still_answer();
    host.recover(&before);

    let reopened = host.client(WINDOW_SCOPES);
    let manifest_id = reopened
        .store()
        .manifest(workspace)
        .expect("a manifest id from the replacement");
    let snapshot = reopened
        .store()
        .snapshot(workspace, manifest_id)
        .expect("a snapshot from the replacement");
    let reloaded = Manifest::from_snapshot(&snapshot).expect("the reopened manifest");
    let copies = reloaded
        .entries()
        .iter()
        .filter(|entry| entry.id == acknowledged)
        .count();
    assert_eq!(
        copies, 1,
        "a durably acknowledged document came back {copies} times"
    );
    assert!(
        reopened
            .store()
            .workspaces()
            .expect("the replacement lists workspaces")
            .iter()
            .any(|listed| listed.id == workspace),
        "the reopened store does not list the workspace the update landed in"
    );
}

// -------------------------------------- settings confirmation, and a lost reply

/// **A settings write whose reply was lost is reported, and nothing uncertain
/// is replayed.**
///
/// This is the confirmation ledger's whole point. A confirmed write is issued
/// and the host is killed underneath it, so the reply either arrives or does
/// not. Both answers are legitimate and the case asserts both branches:
///
/// - **Exactly one terminal outcome.** A receipt settles once. A second outcome
///   for one intent is how a surface ends up showing a security-sensitive
///   switch in a state nobody chose.
/// - **`Accepted` means it is stored.** The replacement must read back what the
///   caller was told landed.
/// - **`Disconnected` means unknown, and unknown is never re-sent.** After
///   recovery an independent writer sets the field the other way, and the link
///   must never turn it back: a link that replayed its uncertain intent would
///   silently overwrite a value a person had just chosen. The link's own
///   recovery is a *re-read* — `koru-api-client`'s worker reconciles rather
///   than retries — and this is that promise at the process boundary.
#[test]
fn a_confirmed_settings_write_whose_reply_was_lost_is_reported_and_never_replayed_worker() {
    let mut host = Host::start();
    use koru_settings::GlobalPatch;

    let mut link =
        koru_api_client::SectionLink::global(segment().to_owned(), Some(host.root().to_path_buf()));
    within("the settings link reached the authority", PROMPT, || {
        link.poll();
        link.is_connected()
    });

    let before = host.record(&[], "before the kill");
    // **The intent is formed after the host is already gone**, which is the
    // shape that reliably produces a reply nobody can hear. Issuing it first
    // and killing afterwards is the same class and was tried: the round trip is
    // a file write and it beat the kill every time, so the interesting branch
    // was never reached. Both branches are asserted below regardless, because
    // which one happens is a race and neither is wrong.
    host.interrupt(&before);
    let mut receipt = link.save_confirmed(GlobalPatch {
        spellcheck_enabled: Some(true),
        ..Default::default()
    });

    let mut settled = None;
    let deadline = Instant::now() + PROMPT;
    while settled.is_none() {
        settled = receipt.poll();
        assert!(
            Instant::now() < deadline,
            "the confirmed write never reached a terminal outcome"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    // A receipt settles once. Polling again answers `None` for ever after.
    assert!(
        receipt.poll().is_none(),
        "one confirmed intent produced a second terminal outcome"
    );

    host.survivors_still_answer();
    host.recover(&before);

    let independent = host.client(WINDOW_SCOPES);
    let outcome = settled.expect("a terminal outcome");
    // Named rather than printed whole: an `Accepted` carries the entire
    // section, and the transcript wants the branch, not the settings.
    let named = match &outcome {
        koru_api_client::ConfirmedOutcome::Accepted(_) => "Accepted",
        koru_api_client::ConfirmedOutcome::Reconciled { .. } => "Reconciled",
        koru_api_client::ConfirmedOutcome::Refused { .. } => "Refused",
        koru_api_client::ConfirmedOutcome::ContractMismatch { .. } => "ContractMismatch",
        koru_api_client::ConfirmedOutcome::Disconnected => "Disconnected",
        koru_api_client::ConfirmedOutcome::Stale => "Stale",
        koru_api_client::ConfirmedOutcome::TimedOut => "TimedOut",
    };
    eprintln!("[the supervised host] the confirmed write settled as {named}");
    if matches!(outcome, koru_api_client::ConfirmedOutcome::Accepted(_)) {
        // **`Accepted` is a promise about the disk.** The caller was told the
        // authority took it, so the replacement has to read it back.
        within(
            "the accepted write was readable on the replacement",
            PROMPT,
            || {
                independent
                    .settings()
                    .global()
                    .map(|global| global.spellcheck_enabled)
                    .unwrap_or(false)
            },
        );
    }

    // **And then nothing is replayed, whichever way it settled.** An
    // independent writer chooses the *other* value, and the link must leave it
    // alone: a link that re-asserted a settled intent — uncertain or accepted —
    // would silently overwrite a value a person had just chosen. The link's own
    // recovery is a re-read, and this is that promise at the process boundary.
    within(
        "the replacement accepted an independent write",
        PROMPT,
        || {
            independent
                .settings()
                .patch_global(&GlobalPatch {
                    spellcheck_enabled: Some(false),
                    ..Default::default()
                })
                .is_ok()
        },
    );
    // Long enough for the link's heal to run — it retries once a second for as
    // long as it is out of touch — so a replay would have landed by the end.
    let watching = Instant::now() + Duration::from_secs(10);
    while Instant::now() < watching {
        link.poll();
        let stored = independent
            .settings()
            .global()
            .expect("the replacement answers a settings read")
            .spellcheck_enabled;
        assert!(
            !stored,
            "a settled settings intent was replayed onto the replacement after {named}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

// ------------------------------------------- catalog watches and the activity read

/// **A window's catalog watch and its activity read come back with the host.**
///
/// A catalog watch is one SSE tail held by a window; the activity read is the
/// events worker's own index, ahead of the Store fallback. A rehost that opened
/// no events worker, or let `/v1/events/*` fall through, answers 404 or 503
/// here while every in-process test still passes — and a *recovery* that
/// brought a host back without its events worker would fail only after the
/// kill, which is what this adds.
///
/// The watch is deliberately re-established rather than expected to survive:
/// the process holding the stream is gone, and a client that pretended
/// otherwise would be showing a tail nobody is writing to. What must survive is
/// the ability to watch again and to read what happened.
#[test]
fn a_catalog_watch_and_the_activity_read_resume_on_the_replacement_worker() {
    let mut host = Host::start();
    let client = host.client(WINDOW_SCOPES);
    assert!(
        client.events().available(),
        "the supervised host published no events capability"
    );

    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counting = std::sync::Arc::clone(&seen);
    let watch = client
        .events()
        .watch_catalog(move |_| {
            counting.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })
        .expect("open a catalog watch");
    within(
        "the catalog watch reached the events worker",
        PROMPT,
        || {
            client
                .events()
                .status()
                .map(|status| status.sse_subscribers > 0)
                .unwrap_or(false)
        },
    );
    let first_workspace = workspace(&client, "Watched");
    within("the first mutation invalidated the catalog", PROMPT, || {
        seen.load(std::sync::atomic::Ordering::Relaxed) > 0
    });

    let subject = koru_events::catalog::TaskSubject {
        workspace: uuid::Uuid::new_v4(),
        document: uuid::Uuid::new_v4(),
        node: uuid::Uuid::new_v4(),
    };
    client
        .events()
        .send(koru_events::catalog::TaskCreated {
            task: subject,
            reason: koru_events::catalog::TaskCreatedReason::Explicit,
            node_kind: koru_events::catalog::TaskNodeKind::Task,
        })
        .expect("the host accepted a task event");

    let before = host.record(&[], "before the kill");
    host.interrupt(&before);
    // The tail's own thread notices the host went; closing it must not hang.
    watch.close();
    host.survivors_still_answer();
    host.recover(&before);

    let reopened = host.client(WINDOW_SCOPES);
    let seen_again = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counting = std::sync::Arc::clone(&seen_again);
    let resumed = reopened
        .events()
        .watch_catalog(move |_| {
            counting.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })
        .expect("open a catalog watch on the replacement");
    within(
        "the resumed catalog watch reached the replacement's events worker",
        PROMPT,
        || {
            reopened
                .events()
                .status()
                .map(|status| status.sse_subscribers > 0)
                .unwrap_or(false)
        },
    );
    reopened
        .store()
        .rename_workspace(first_workspace, "Renamed after the kill")
        .expect("rename a workspace through the replacement");
    within(
        "a mutation after the kill invalidated the resumed catalog",
        PROMPT,
        || seen_again.load(std::sync::atomic::Ordering::Relaxed) > 0,
    );

    // The journal is the events worker's own durable index, and the read is
    // addressed by subject — so a replacement that opened a different events
    // worker, or none, answers nothing here.
    let page = reopened
        .events()
        .activity(
            koru_events::SubjectKind::Task,
            &subject.subject().id,
            None,
            10,
        )
        .expect("the replacement answered the activity read");
    assert_eq!(
        page.entries.len(),
        1,
        "the activity written before the kill did not survive it: {:?}",
        page.entries
    );
    assert_eq!(page.entries[0].name.as_str(), "task.created");
    resumed.close();
}

// ---------------------------------------------------- workspace removal in flight

/// **A removal that was in flight when the host died lands once, or not at
/// all — and never twice.**
///
/// The soft delete a manage-workspaces window performs is the reachable half of
/// this class; the vouched archive, soft-delete-with-confirmation and migration
/// routes need a grant no test process can hold, and are pinned against the
/// router in `gateway_lifecycle.rs`.
///
/// Two workspaces exist and only one is removed, so "the removal happened" is
/// distinguishable from "the catalog came back empty" — which is the failure a
/// store that reopened wrongly would produce, and which a single-workspace case
/// would read as success.
#[test]
fn a_workspace_removal_in_flight_lands_once_and_is_never_replayed_worker() {
    let mut host = Host::start();
    let client = host.client(WINDOW_SCOPES);
    let profile = client
        .store()
        .create_profile("Removal", None, None, Some("en"))
        .expect("create a profile");
    let removed = client
        .store()
        .create_workspace(profile.id, "Removed")
        .expect("create the workspace to remove")
        .id;
    let kept = client
        .store()
        .create_workspace(profile.id, "Kept")
        .expect("create the workspace to keep")
        .id;

    let before = host.record(&[], "before the kill");
    // Asked, then the host is killed underneath the answer. Whether the reply
    // reached this process is exactly what is uncertain, so the result is
    // recorded rather than asserted.
    let answered = client.store().delete_workspace(removed);
    host.interrupt(&before);
    host.survivors_still_answer();
    host.recover(&before);

    let reopened = host.client(WINDOW_SCOPES);
    let listed = reopened
        .store()
        .workspaces()
        .expect("the replacement lists workspaces");
    let survivors: Vec<_> = listed.iter().filter(|w| w.id == kept).collect();
    assert_eq!(
        survivors.len(),
        1,
        "the workspace nobody removed did not come back exactly once"
    );
    let target: Vec<_> = listed.iter().filter(|w| w.id == removed).collect();
    assert!(
        target.len() <= 1,
        "the removed workspace came back {} times",
        target.len()
    );
    if answered.is_ok() {
        // The caller was told it landed, so it must be durable.
        assert!(
            target
                .first()
                .map(|w| w.deleted_at.is_some())
                .unwrap_or(true),
            "an acknowledged removal was undone by the replacement"
        );
    }

    // **Nothing uncertain is replayed, and a retry is the caller's.** Asking
    // again is idempotent rather than a second mutation: the workspace is
    // removed once, and the one nobody touched is untouched.
    let _ = reopened.store().delete_workspace(removed);
    let after = reopened
        .store()
        .workspaces()
        .expect("the replacement lists workspaces again");
    assert_eq!(
        after.iter().filter(|w| w.id == removed).count(),
        target.len(),
        "a repeated removal duplicated the workspace row"
    );
    assert!(
        after
            .iter()
            .find(|w| w.id == kept)
            .expect("the kept workspace")
            .deleted_at
            .is_none(),
        "a removal reached a workspace nobody asked about"
    );
}

// ------------------------------------------- the Tasks port and grant admission

/// **Provenance and admission, before and after the kill.**
///
/// - **The Tasks query port** lives in the Store worker the host embeds, and the
///   Gateway reaches it through the fallback that forwards what it does not own.
///   A store with no task service answers `limit: 0` and no definitions; a real
///   port echoes the clamped limit and ships the system attribute registry,
///   which is what tells the two apart. A replacement that came back without
///   its task service would pass every in-process test and fail here.
/// - **Grant admission.** The vouched scopes are the destructive ones, and a
///   caller that names them without a grant the Agent vouched for is admitted
///   *ordinary* — quietly, by design. So the boundary is asserted by what it
///   refuses: the vouched route answers 403 to this process before the kill and
///   after it. **The replacement re-resolves the Agent endpoint**, which is the
///   half a rehost could break: a supervised child that came back without an
///   Agent to ask would make every caller ordinary and nothing else would say
///   so. The grant path's own contract — that a process that is not the Koru
///   app is refused a grant at all — is
///   `lifecycle.rs::a_caller_that_is_not_the_koru_app_is_refused_a_first_party_grant`.
#[test]
fn the_tasks_port_and_the_vouched_boundary_answer_the_same_on_a_replacement_worker() {
    let mut host = Host::start();
    let scopes: Vec<&str> = WINDOW_SCOPES
        .iter()
        .copied()
        .chain([
            koru_api_client::WORKSPACE_DESTROY_SCOPE,
            koru_api_client::WORKSPACE_MIGRATE_SCOPE,
        ])
        .collect();
    let client = host.client(&scopes);
    let workspace = workspace(&client, "Tasks");

    let query = koru_store_wire::tasks::TaskQuery {
        workspaces: vec![workspace],
        limit: 25,
        ..Default::default()
    };
    let answered = client
        .store()
        .query_tasks(&query)
        .expect("the host answered the tasks query");
    assert_eq!(
        answered.limit, 25,
        "the answer did not come from an installed task port"
    );
    assert!(
        !answered.definitions.is_empty(),
        "the system attribute registry did not come back with the answer"
    );

    let refused_before = refusal(
        client
            .catalog()
            .confirmation(
                &workspace.to_storage(),
                koru_api_client::WorkspaceLifecycleAction::Archive,
            )
            .expect_err("an ungranted caller was admitted to a vouched route"),
    );

    let before = host.record(&[], "before the kill");
    host.interrupt(&before);
    host.survivors_still_answer();
    host.recover(&before);

    let reopened = host.client(&scopes);
    let answered_again = reopened
        .store()
        .query_tasks(&query)
        .expect("the replacement answered the tasks query");
    assert_eq!(
        answered_again.limit, 25,
        "the replacement came back without its task port"
    );
    assert_eq!(
        answered_again.definitions.len(),
        answered.definitions.len(),
        "the replacement's attribute registry disagrees with its predecessor's"
    );

    let refused_after = refusal(
        reopened
            .catalog()
            .confirmation(
                &workspace.to_storage(),
                koru_api_client::WorkspaceLifecycleAction::Archive,
            )
            .expect_err("the replacement admitted an ungranted caller to a vouched route"),
    );
    assert_eq!(
        refused_after, refused_before,
        "the vouched boundary answered differently after the kill"
    );
    assert!(
        refused_after.starts_with("403 "),
        "an ungranted caller was refused for the wrong reason: {refused_after}"
    );
}

/// A refusal reduced to what may be compared across two hosts: the status and
/// the stable code. The developer message may carry machine details and is
/// deliberately not part of the comparison.
fn refusal(error: koru_api_client::WorkspaceError) -> String {
    match error {
        koru_api_client::WorkspaceError::Api(failure) => {
            format!("{} {}", failure.status, failure.code)
        }
        koru_api_client::WorkspaceError::Transport(error) => format!("transport {error}"),
    }
}

// ------------------------------------------------------------- worker-only cases

/// **A Gateway death is not a session-host loss, and the Agent does not go with
/// it.**
///
/// The distinction ADR-280 rests on: a session tab must tell *temporary Gateway
/// unavailability* from *its own host being gone*. Here the Gateway is killed
/// with a live session running, and the session is still there afterwards —
/// same id, same host instance — because the sessions worker is a different
/// child of the same Agent and nothing about it changed.
///
/// The other half of ADR-280, what happens when the *sessions* worker dies, is
/// where it can be armed: `crates/koru-session-backend`'s generation tests
/// drive the handshake ids and the `SessionsLost` report directly, and
/// `crates/koru-agent-worker/tests/worker_process.rs` drives the crash and the
/// bounded restart. Neither belongs in a Gateway recovery battery, and the
/// sessions host publishes a uuid rather than a pid, so nothing here can reach
/// in and kill it the way `interrupt` reaches the Gateway.
///
#[test]
fn a_gateway_death_leaves_the_agent_and_its_live_sessions_alive() {
    use std::str::FromStr as _;

    let mut host = Host::start();
    let sessions = koru_session_client::SessionClient::connect(
        &host.session_endpoint(),
        "koru-gateway-recovery",
        koru_session_protocol::ProtocolOffer {
            versions: koru_session_protocol::ProtocolRange {
                min: koru_session_protocol::MIN_PROTOCOL_VERSION,
                max: koru_session_protocol::MAX_PROTOCOL_VERSION,
            },
            capabilities: koru_session_protocol::CapabilitySet::new(
                [
                    koru_session_protocol::capability::CONTROLLER_LEASES,
                    koru_session_protocol::capability::RENDER_DELTAS,
                    koru_session_protocol::capability::TERMINATE,
                ]
                .into_iter()
                .map(|name| koru_session_protocol::CapabilityName::from_str(name).unwrap()),
            ),
        },
    )
    .expect("the sessions worker answered");

    #[cfg(windows)]
    let argv: Vec<String> = vec![
        "powershell.exe".into(),
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-Command".into(),
        "Start-Sleep -Seconds 120".into(),
    ];
    #[cfg(unix)]
    let argv: Vec<String> = vec!["/bin/sh".into(), "-c".into(), "sleep 120".into()];
    let created = sessions
        .create(koru_session_wire::CreateSession {
            profile: koru_session_protocol::SessionProfileKey::BuiltIn(
                koru_session_protocol::BuiltInProfileKey::from_str("platform-shell").unwrap(),
            ),
            parent_session_id: None,
            profile_kind: koru_session_protocol::ProfileKind::Shell,
            display_name: "survives a gateway death".into(),
            working_directory: host.root().to_path_buf(),
            mode: koru_session_protocol::SessionMode::RawPty,
            attachment_delivery: koru_session_protocol::AttachmentDeliveryPolicy::Unsupported,
            capabilities: koru_session_protocol::CapabilitySet::default(),
            argv,
            environment: koru_session_wire::EnvironmentSpec::Inherit,
            size: koru_session_protocol::GridSize::new(80, 24).unwrap(),
            project_id: None,
        })
        .expect("a live session");

    let before = host.record(&[], "before the kill");
    host.interrupt(&before);
    host.survivors_still_answer();

    // **The same sessions worker, and the same session.** Not a reconnect that
    // found an empty catalog and called it a resume: the id is the one created
    // before the Gateway died.
    assert!(
        sessions
            .list()
            .expect("the sessions worker still answers its existing client")
            .iter()
            .any(|listed| listed.id == created.id),
        "a live session was lost when the gateway died"
    );

    host.recover(&before);
    assert!(
        sessions
            .list()
            .expect("the sessions worker answers after the gateway came back")
            .iter()
            .any(|listed| listed.id == created.id),
        "a live session was lost when the gateway was replaced"
    );
}

/// **Two roots, two Agents, two gateway children, and a kill on one of them.**
///
/// The existing `lifecycle.rs::two_rooted_agents_each_supervise_their_own_gateway_child`
/// proves the steady state. What this adds is the failure: killing one root's
/// gateway must not move, restart or be observed by the other's. The rendezvous
/// name carries a digest of the root, so the two could not meet — this is the
/// assertion that they did not.
#[test]
fn two_roots_recover_independently_and_never_observe_each_other() {
    let mut one = Host::start();
    let other = Host::start();

    let one_before = one.record(&[], "on the first root");
    let other_before = other.record(&[], "on the second root");
    assert_ne!(one_before.instance_id, other_before.instance_id);
    assert_ne!(one_before.api_endpoint, other_before.api_endpoint);
    assert_ne!(
        one_before.supervisor_launch_id, other_before.supervisor_launch_id,
        "two roots were served by one child"
    );

    one.interrupt(&one_before);
    one.survivors_still_answer();
    let one_after = one.recover(&one_before);

    // The other root never noticed: same child, same launch, same address.
    let other_after = other.record(&[], "on the second root after the first was killed");
    assert_eq!(
        other_after.instance_id, other_before.instance_id,
        "killing one root's gateway replaced the other root's"
    );
    assert_eq!(
        other_after.supervisor_launch_id,
        other_before.supervisor_launch_id
    );
    assert_eq!(other_after.api_endpoint, other_before.api_endpoint);

    // And the replacement is the first root's own, not the other's.
    assert_ne!(one_after.instance_id, other_after.instance_id);
    assert_ne!(one_after.api_endpoint, other_after.api_endpoint);
    assert!(one.root().join("store").join("catalog.db").exists());
    assert!(other.root().join("store").join("catalog.db").exists());
}
