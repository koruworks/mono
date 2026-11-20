// SPDX-License-Identifier: MPL-2.0

//! **A supervised gateway child, for every suite that needs one.**
//!
//! The five gateway integration suites — `gateway_sync`, `gateway_ai`,
//! `gateway_settings`, `gateway_catalog_events`, `gateway_host_parity` — used
//! to spawn `koru-api-gateway.exe --root <root>` and wait for discovery. That
//! binary is gone. The only image that hosts the Gateway now is
//! `koru-agent.exe --worker gateway`, and that role is **supervised-only**: it
//! reads one newline-terminated `LaunchBootstrap` from stdin, proves the
//! bootstrap's channel, its root fingerprint and *this image's* fingerprint,
//! and connects the Agent's private bridge before it publishes anything. So a
//! suite that wants a gateway now has to play the Agent, and this module is
//! that harness — one copy, because five copies of a bootstrap writer is five
//! places for a launch contract to drift.
//!
//! A directory under `tests/` is not itself a test target, which is what makes
//! `mod gateway_process;` the supported way to share code between integration
//! test binaries. Each binary compiles its own copy and uses the part it needs;
//! `dead_code` is allowed here for exactly that reason.
//!
//! ## What a caller gets
//!
//! - [`Gateway::start`] and [`Gateway::start_with`] — spawn on a root, with any
//!   extra role arguments (`gateway_ai` passes `--deterministic-ai-test`), and
//!   write the bootstrap.
//! - [`Gateway::published`] — the readiness wait, answering with the
//!   `GatewayHello` the old fixtures returned. The caller supplies its own
//!   [`ClientHello`], so a failure still names the suite that opened the
//!   connection.
//! - [`Gateway::terminate`] — **kill and reap**, not a graceful stop. Several
//!   cases kill the gateway deliberately to prove what an acknowledgement
//!   meant; a polite stop would let it finish work it had already called
//!   durable, which is the one thing those assertions must not depend on.
//! - [`Drop`] — kills, reaps, and closes the harness bridge, so the next
//!   gateway on the same root binds a released rendezvous.
//!
//! ## Every child runs on its own root, and nothing machine-wide
//!
//! Endpoint names carry a digest of the root, so these children never meet each
//! other or the gateway a developer has open. Nothing here writes to
//! `%LOCALAPPDATA%`.

#![allow(dead_code)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use koru_api_discovery::{ClientHello, GatewayHello};
use koru_network_bridge::wire::{Registration, RegistrationReply, WIRE_VERSION};
use koru_network_bridge::{BRIDGE_VERSION, BridgeCapability, BridgeIngress, LaunchBootstrap};

/// How long a real gateway child gets to compose Store, AI and events, bind a
/// loopback listener and publish itself. Generous: this is a cold process on a
/// machine that may be running the rest of the workspace suite, and it now
/// proves a bootstrap and dials a bridge before it publishes anything.
pub const READY: Duration = Duration::from_secs(60);

/// The channel this test binary is, asked rather than written down so
/// `cargo test --release` moves the child it spawns with it.
pub fn segment() -> &'static str {
    koru_channel::segment()
}

pub fn agent_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_koru-agent"))
}

/// A hello asking for no scopes, which is what a readiness poll wants.
pub fn hello(product: &str) -> ClientHello {
    hello_with_scopes(product, &[])
}

pub fn hello_with_scopes(product: &str, scopes: &[&str]) -> ClientHello {
    ClientHello {
        product: product.into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        segment: segment().into(),
        api_version: koru_api_wire::API_VERSION,
        requested_scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
        grant: None,
    }
}

/// A supervised gateway child on one root, and the harness's bridge behind it.
pub struct Gateway {
    root: PathBuf,
    launch_id: String,
    child: Child,
    /// The write end of the bootstrap pipe. **Dropping it is a parent death**,
    /// which is the cross-platform signal the role watches for.
    pipe: Option<std::process::ChildStdin>,
    /// Dropped last, and only after the child is gone: closing it releases the
    /// bridge endpoint so the next gateway on this root can bind it.
    agent: Option<FakeAgent>,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.agent.take();
    }
}

impl Gateway {
    /// Start a supervised gateway on `root`.
    pub fn start(root: &Path) -> Self {
        Self::start_with(root, &[])
    }

    /// Start a supervised gateway on `root`, passing `extra` to the role after
    /// `--root`. The role's parser is strict, so anything here is a token it
    /// actually accepts.
    pub fn start_with(root: &Path, extra: &[&str]) -> Self {
        let store_root = koru_channel::store_root_under(root);
        std::fs::create_dir_all(&store_root).expect("the rooted store directory");

        let launch_id = uuid::Uuid::new_v4().to_string();
        let bootstrap = LaunchBootstrap {
            device_id: None,
            run_authority: None,
            bots_engine: None,
            launch_id: launch_id.clone(),
            bridge_version: BRIDGE_VERSION,
            channel: segment().into(),
            // **The store root, not the channel root.** The Agent fingerprints
            // the path it hands the Store and the gateway checks that half.
            root_fingerprint: koru_network_bridge::fingerprint(
                store_root.to_string_lossy().as_bytes(),
            ),
            // **This exact image**, which is `koru-agent.exe` and not a gateway
            // binary — the property that makes "the Agent launched this" mean
            // something now that the two are one file.
            installation_fingerprint: koru_network_bridge::fingerprint(
                std::fs::read(agent_exe()).expect("this test's own agent image"),
            ),
            capability: BridgeCapability::generate(),
            // No session host in this harness: the gateway then serves without
            // a sessions adapter, which is a documented degradation and is what
            // the standalone host these suites used to spawn also did.
            session_lifecycle: None,
        };

        // The bridge is bound *before* the child starts, because the child
        // connects to it before it binds anything of its own.
        let agent = FakeAgent::listen(&store_root, &bootstrap);

        let mut command = Command::new(agent_exe());
        command.args(["--worker", "gateway", "--root"]).arg(root);
        command
            .args(extra)
            .env("KORU_STORE_NO_DIALOG", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().expect("spawn the gateway worker");
        let mut pipe = child.stdin.take().expect("the bootstrap pipe");
        let line = serde_json::to_vec(&bootstrap).expect("encode the bootstrap");
        pipe.write_all(&line).expect("write the bootstrap");
        pipe.write_all(b"\n").expect("terminate the bootstrap line");
        pipe.flush().expect("flush the bootstrap");

        Self {
            root: root.to_path_buf(),
            launch_id,
            child,
            pipe: Some(pipe),
            agent: Some(agent),
        }
    }

    /// The launch this child was started for, which is what it publishes as its
    /// `supervisor_launch_id`.
    pub fn launch_id(&self) -> &str {
        &self.launch_id
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Has the child exited? `None` means it is still running.
    pub fn try_wait(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().expect("try_wait")
    }

    /// **Let the bootstrap pipe go**, which is a parent death to the child.
    pub fn drop_pipe(&mut self) {
        self.pipe.take();
    }

    /// Wait until the rendezvous answers, then hand back the record.
    ///
    /// The caller supplies its own hello so a failure names which suite was
    /// dialling.
    pub fn published(&mut self, hello: &ClientHello) -> GatewayHello {
        let deadline = Instant::now() + READY;
        loop {
            if let Ok(found) = koru_api_discovery::connect_at(segment(), &self.root, hello) {
                return found;
            }
            if let Some(status) = self.try_wait() {
                panic!(
                    "the gateway worker exited with {status} before it published for `{}`",
                    hello.product
                );
            }
            assert!(
                Instant::now() < deadline,
                "the gateway worker did not publish for `{}` within {READY:?}",
                hello.product
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// **Kill the host and reap it**, so the next one binds a released
    /// rendezvous.
    ///
    /// Killed rather than asked, because what several cases test is what an
    /// acknowledgement *meant*: a graceful stop would let the host finish work
    /// it had already said was durable, which is the one thing those assertions
    /// must not be allowed to depend on.
    pub fn terminate(mut self) {
        self.child.kill().expect("terminate gateway");
        self.child.wait().expect("reap gateway");
        // `Drop` closes the harness bridge after this returns, which is what
        // frees the endpoint for the replacement.
    }

    /// Wait for the child to exit, or answer `None` if it did not.
    pub fn wait_for_exit(&mut self, within: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + within;
        loop {
            match self.try_wait() {
                Some(status) => return Some(status),
                None if Instant::now() >= deadline => return None,
                None => std::thread::sleep(Duration::from_millis(25)),
            }
        }
    }
}

/// The Agent's half of the private bridge: bind, admit children with the real
/// `BridgeAuthority`, and hold each link open.
///
/// **Stoppable, on purpose.** Several cases kill a gateway and start its
/// replacement on the *same* root; a detached thread parked in `accept` would
/// keep the bridge endpoint bound and the replacement could never register. So
/// the thread is joined: the flag is set, one connection wakes the blocking
/// accept, and dropping the listener releases the name.
struct FakeAgent {
    endpoint: koru_ipc::Endpoint,
    stopping: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeAgent {
    fn listen(store_root: &Path, bootstrap: &LaunchBootstrap) -> Self {
        let endpoint = koru_ipc::Endpoint::for_service_root(
            koru_ipc::AGENT_GATEWAY_BRIDGE,
            segment(),
            store_root,
        )
        .expect("a rooted bridge endpoint");
        let listener = koru_ipc::Listener::bind(endpoint.clone()).expect("bind the Agent bridge");
        let mut authority = koru_network_bridge::BridgeAuthority::new(bootstrap);
        let stopping = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stopping);
        let thread = std::thread::Builder::new()
            .name("fake agent bridge".into())
            .spawn(move || {
                // Held so the gateway's end of each admitted link stays open
                // for as long as this fixture does.
                let mut held: Vec<(koru_ipc::Stream, BridgeIngress)> = Vec::new();
                while let Ok(mut stream) = listener.accept() {
                    if flag.load(Ordering::SeqCst) {
                        break;
                    }
                    // A registration that never arrives must not park this
                    // thread forever — that would make the stop below hang.
                    koru_ipc::set_timeouts(&stream, Some(Duration::from_secs(30)));
                    let Ok((version, registration)) =
                        koru_ipc::frame::read_frame::<_, Registration>(&mut stream)
                    else {
                        continue;
                    };
                    let reply = if version != WIRE_VERSION {
                        Err(RegistrationReply::Refused("frame version".into()))
                    } else {
                        authority
                            .register(&registration.identity, &registration.capability)
                            .map_err(|refusal| RegistrationReply::Refused(format!("{refusal:?}")))
                    };
                    match reply {
                        Ok(ingress) => {
                            if koru_ipc::frame::write_frame(
                                &mut stream,
                                WIRE_VERSION,
                                &RegistrationReply::Accepted,
                            )
                            .is_ok()
                            {
                                koru_ipc::set_timeouts(&stream, None);
                                held.push((stream, ingress));
                            }
                        }
                        Err(refused) => {
                            let _ =
                                koru_ipc::frame::write_frame(&mut stream, WIRE_VERSION, &refused);
                        }
                    }
                }
            })
            .expect("the fake Agent bridge thread");
        Self {
            endpoint,
            stopping,
            thread: Some(thread),
        }
    }
}

impl Drop for FakeAgent {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // One connection that says nothing: enough to return from the blocking
        // accept, after which the flag is read and the thread ends.
        let _ = koru_ipc::connect(&self.endpoint);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
