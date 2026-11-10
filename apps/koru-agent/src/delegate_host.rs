// SPDX-License-Identifier: MPL-2.0

//! Supervision of the delegate worker.
//!
//! The orchestrator of delegated runs is a worker child of this same
//! executable — `koru-agent.exe --worker delegate` (D12) — spawned and
//! watched through `koru-agent-worker`, exactly as the gateway and the
//! session host are. This file is the **supervisor adapter**: what one
//! delegate child is, how it is proved ready, how its health is checked,
//! and how it is asked to stop. The *role* is `delegate_worker.rs`, and
//! the pair mirrors `gateway_host.rs` ↔ `gateway_worker.rs`.
//!
//! ## Startup never waits for it
//!
//! [`start`] returns as soon as the supervision thread exists, the way the
//! gateway's does: a person holding a dictation chord must not wait behind
//! a worker that is itself waiting for the gateway to publish. The child
//! is started **after** the sessions and gateway workers in `run.rs`,
//! because it reaches both — the session host over its IPC, the Store
//! through the gateway — and its readiness deadline is long enough to
//! cover a gateway that is still composing.
//!
//! ## What a crash costs, and what it does not
//!
//! A crash loses nothing durable: the run's status is in the Run document,
//! the attempt's state in `runs.db`, and the provider session in the
//! session host, which is a sibling worker and survives. The restarted
//! child classifies every live attempt with §7.6's table — reattach where
//! the session lives, `Interrupted` where nothing does, never `Failed`
//! (D10) — and the restart budget bounds how often that happens before a
//! person is told.
//!
//! ## Three secrets, as the session host has them
//!
//! `launch_id` per child is identity: readiness proves the endpoint answers
//! as this launch. The stop authority per child stops exactly that child.
//! There is no third: the delegate worker holds no lifecycle authority over
//! anything but its own runs.

use std::path::Path;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

use koru_agent_worker::machine::State;
use koru_agent_worker::supervisor::{
    self as worker, Generation, LaunchPlan, StatusReport, StopOutcome, WorkerEventKind,
    WorkerHandle, WorkerSpec,
};
use koru_delegate_api::LocalDelegate;

use crate::Paths;

/// What the role's closures need to reach and stop **their own** child.
pub struct DelegateChild {
    pub launch_id: String,
    stop_authority: String,
    transport: LocalDelegate,
}

impl DelegateChild {
    fn hello(&self) -> Result<koru_delegate_host::Hello, String> {
        self.transport
            .hello()
            .map_err(|failure| failure.to_string())
    }
}

/// How long a fresh child has to reach the session host, wait for the
/// gateway to publish, open `runs.db`, classify its ledger and bind its
/// endpoint. The gateway's own ready deadline is sixty seconds and this
/// child waits for that gateway, so it gets more.
const READY_DEADLINE: Duration = Duration::from_secs(120);
const WAKE_RETRY_INITIAL: Duration = Duration::from_millis(100);
const WAKE_RETRY_MAX: Duration = Duration::from_secs(1);

pub struct Supervisor {
    handle: WorkerHandle<DelegateChild>,
    endpoint: koru_ipc::Endpoint,
    wakes: DelegateWakeIngress,
}

#[derive(Clone)]
pub struct DelegateWakeIngress {
    pending: Arc<AtomicBool>,
    sender: mpsc::SyncSender<()>,
}

impl DelegateWakeIngress {
    /// Nonblocking and coalesced: native callbacks only set this edge. The
    /// blocking worker-control IPC call runs on the ingress thread.
    pub fn wake(&self) {
        self.pending.store(true, Ordering::Release);
        if matches!(
            self.sender.try_send(()),
            Err(mpsc::TrySendError::Disconnected(_))
        ) {
            self.pending.store(false, Ordering::Release);
        }
    }
}

static DELEGATE: OnceLock<Supervisor> = OnceLock::new();

/// Start supervising the delegate worker and install it for this process.
///
/// **Returns immediately.** The first spawn happens on the supervision
/// thread; nothing here opens a database or waits for readiness.
pub fn start(
    paths: &Paths,
    device: koru_device::DeviceId,
    bot_authority: koru_delegate_api::bots::BotDelegateAuthority,
) -> Result<&'static Supervisor, String> {
    let endpoint = crate::delegate_endpoint(paths).map_err(|error| error.to_string())?;
    let authority = Arc::new(Mutex::new(String::new()));
    let wakes = wake_ingress(endpoint.clone(), Arc::clone(&authority))?;
    let handle = worker::start(delegate_spec(
        paths,
        &endpoint,
        authority,
        device,
        bot_authority,
    ));
    let supervisor = Supervisor {
        handle,
        endpoint,
        wakes,
    };
    if DELEGATE.set(supervisor).is_err() {
        tracing::warn!(
            target: "koru::delegate_host",
            "a delegate supervisor was already installed; keeping the first"
        );
    }
    Ok(DELEGATE.get().expect("a delegate supervisor"))
}

/// The installed supervision, or `None` in a process that never started
/// one — a unit test, or an Agent whose device network did not come up.
pub fn current() -> Option<&'static Supervisor> {
    DELEGATE.get()
}

impl Supervisor {
    /// What the supervision is doing — the inspectable answer.
    pub fn status(&self) -> StatusReport {
        self.handle.status()
    }

    pub fn endpoint(&self) -> &koru_ipc::Endpoint {
        &self.endpoint
    }

    /// Whether a child is serving right now.
    pub fn ready(&self) -> bool {
        self.handle.status().state == State::Ready
    }

    pub fn wake_ingress(&self) -> DelegateWakeIngress {
        self.wakes.clone()
    }

    /// Stop the worker for good, on the way out. Blocking; call it from a
    /// lifecycle worker, never from winit. The Job Object is the fallback.
    pub fn stop_for_quit(&self) -> StopOutcome {
        self.handle.stop()
    }
}

/// The role's whole knowledge, spelled as the worker crate's closures.
fn delegate_spec(
    paths: &Paths,
    endpoint: &koru_ipc::Endpoint,
    wake_authority: Arc<Mutex<String>>,
    device: koru_device::DeviceId,
    bot_authority: koru_delegate_api::bots::BotDelegateAuthority,
) -> WorkerSpec<DelegateChild> {
    let segment: String = crate::segment().into();
    let root = paths.root().to_path_buf();
    let store_dir = paths.store_dir();
    let explicit_root = paths.explicit_root().map(Path::to_path_buf);
    let launch_endpoint = endpoint.clone();

    let mut spec = WorkerSpec::new(
        "delegate",
        move |_generation: Generation| {
            let launch_id = uuid::Uuid::new_v4().to_string();
            let stop_authority = uuid::Uuid::new_v4().to_string();
            *wake_authority
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = stop_authority.clone();
            // Already validated by the Agent; never reopen or mint identity here.
            let device_id = device.to_storage();
            let bootstrap = koru_delegate_host::LaunchBootstrap::new(
                launch_id.clone(),
                segment.clone(),
                root.clone(),
                store_dir.clone(),
                explicit_root.is_some(),
                device_id,
                stop_authority.clone(),
            )
            .with_bot_authority(bot_authority.clone());
            let bootstrap = serde_json::to_vec(&bootstrap)
                .map_err(|error| format!("could not encode the delegate bootstrap: {error}"))?;
            let mut args: Vec<std::ffi::OsString> = Vec::new();
            if let Some(root) = &explicit_root {
                args.push("--root".into());
                args.push(root.clone().into());
            }
            Ok(LaunchPlan {
                args,
                bootstrap,
                payload: DelegateChild {
                    launch_id,
                    stop_authority,
                    transport: LocalDelegate::new(launch_endpoint.clone(), "koru-agent-supervisor"),
                },
            })
        },
        |child: &DelegateChild| {
            let hello = child.hello()?;
            if hello.launch_id != child.launch_id {
                return Err("the delegate endpoint belongs to another launch".into());
            }
            Ok(())
        },
    );
    spec.ready_deadline = READY_DEADLINE;
    spec.health = Some(Box::new(|child: &DelegateChild| {
        // Tolerant on purpose: a health failure kills and replaces the
        // child, and one busy accept is not evidence that a run died.
        let mut last = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(250));
            }
            match child.hello() {
                Ok(hello) if hello.launch_id == child.launch_id => return Ok(()),
                Ok(_) => last = "another launch answers".into(),
                Err(error) => last = error,
            }
        }
        Err(last)
    }));
    spec.health_interval = Duration::from_secs(30);
    spec.stop_deadline = Duration::from_secs(10);
    spec.graceful_stop = Some(Box::new(|child: &DelegateChild| {
        child
            .transport
            .stop(child.stop_authority.clone())
            .map_err(|failure| failure.to_string())
    }));
    spec.on_event = Some(Box::new(|event| match &event.kind {
        WorkerEventKind::Ready => tracing::info!(
            target: "koru::delegate_host",
            generation = %event.generation,
            "the delegate child is serving"
        ),
        WorkerEventKind::Exited { code } => tracing::warn!(
            target: "koru::delegate_host",
            generation = %event.generation,
            code = code.unwrap_or(-1),
            "the delegate child exited"
        ),
        WorkerEventKind::RestartScheduled { delay } => tracing::warn!(
            target: "koru::delegate_host",
            delay_ms = delay.as_millis() as u64,
            "a delegate restart is scheduled"
        ),
        WorkerEventKind::SpawnFailed { error } => tracing::error!(
            target: "koru::delegate_host",
            %error,
            "the delegate child could not be started"
        ),
        WorkerEventKind::Exhausted => tracing::error!(
            target: "koru::delegate_host",
            "the delegate restart budget is exhausted"
        ),
        WorkerEventKind::Held => tracing::info!(
            target: "koru::delegate_host", "delegate restarts are held"
        ),
        WorkerEventKind::Resumed => tracing::info!(
            target: "koru::delegate_host", "delegate supervision resumed"
        ),
        WorkerEventKind::Stopped => tracing::info!(
            target: "koru::delegate_host",
            generation = %event.generation,
            "the delegate child was stopped"
        ),
        WorkerEventKind::ProbeFailed { error } | WorkerEventKind::HealthFailed { error } => {
            koru_ipc::ipc_debug!("delegate: probe failed: {error}");
        }
    }));
    spec
}

fn wake_ingress(
    endpoint: koru_ipc::Endpoint,
    authority: Arc<Mutex<String>>,
) -> Result<DelegateWakeIngress, String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let pending = Arc::new(AtomicBool::new(false));
    let worker_pending = Arc::clone(&pending);
    std::thread::Builder::new()
        .name("delegate-native-wakes".to_owned())
        .spawn(move || {
            let transport = LocalDelegate::new(endpoint, "koru-agent-native-wake");
            run_wake_ingress(receiver, worker_pending, || {
                let authority = authority
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                if authority.is_empty() {
                    return false;
                }
                transport.wake_schedules(authority).is_ok()
            });
        })
        .map_err(|error| format!("could not start delegate wake ingress: {error}"))?;
    Ok(DelegateWakeIngress { pending, sender })
}

fn run_wake_ingress(
    receiver: mpsc::Receiver<()>,
    pending: Arc<AtomicBool>,
    mut send: impl FnMut() -> bool,
) {
    while receiver.recv().is_ok() {
        // Consume the queued edge before the blocking IPC. A signal racing
        // with this send can then queue one more edge; SyncSender capacity
        // coalesces any additional signals without losing that second wake.
        pending.store(false, Ordering::Release);
        let mut retry = WAKE_RETRY_INITIAL;
        let mut unavailable = false;
        loop {
            if send() {
                if unavailable {
                    tracing::info!(
                        target: "koru::delegate_host",
                        "delegate schedule wake delivery recovered"
                    );
                }
                break;
            }
            if !unavailable {
                tracing::warn!(
                    target: "koru::delegate_host",
                    "delegate schedule wake delivery unavailable; retrying"
                );
                unavailable = true;
            }
            match receiver.recv_timeout(retry) {
                Ok(()) => pending.store(false, Ordering::Release),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            retry = retry.saturating_mul(2).min(WAKE_RETRY_MAX);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_role_the_supervisor_names_is_the_role_main_dispatches() {
        assert_eq!(crate::delegate_worker::role().name, "delegate");
    }

    #[test]
    fn a_wake_during_a_blocked_send_is_delivered_after_it() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let pending = Arc::new(AtomicBool::new(false));
        let ingress = DelegateWakeIngress {
            pending: Arc::clone(&pending),
            sender,
        };
        let (entered, observed) = mpsc::sync_channel(2);
        let (release, released) = mpsc::sync_channel(2);
        let worker_pending = Arc::clone(&pending);
        let worker = std::thread::spawn(move || {
            run_wake_ingress(receiver, worker_pending, || {
                entered.send(()).unwrap();
                released.recv().unwrap();
                true
            });
        });

        ingress.wake();
        observed.recv().unwrap();
        ingress.wake();
        release.send(()).unwrap();
        observed.recv().unwrap();
        release.send(()).unwrap();
        drop(ingress);
        worker.join().unwrap();
    }

    #[test]
    fn a_transient_wake_failure_retries_the_same_edge() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let pending = Arc::new(AtomicBool::new(false));
        let ingress = DelegateWakeIngress {
            pending: Arc::clone(&pending),
            sender,
        };
        let (attempted, attempts) = mpsc::sync_channel(2);
        let worker_pending = Arc::clone(&pending);
        let worker = std::thread::spawn(move || {
            let mut count = 0;
            run_wake_ingress(receiver, worker_pending, || {
                count += 1;
                attempted.send(count).unwrap();
                count > 1
            });
        });

        ingress.wake();
        assert_eq!(attempts.recv().unwrap(), 1);
        assert_eq!(attempts.recv().unwrap(), 2);
        drop(ingress);
        worker.join().unwrap();
    }
}
