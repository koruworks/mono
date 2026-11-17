// SPDX-License-Identifier: MPL-2.0

//! **The display-free supervisor**: the Agent with no window loop.
//!
//! [`crate::run::run`] ends in the window loop, which needs a display. A Bot
//! on a server a person operates has none, so `koru-agent --server --root
//! <path>` enters [`run_server`] instead. It starts the same workers `run`
//! starts, through the same helper functions and in the same order, and then
//! pumps the Agent's events on a plain thread loop.
//!
//! ## What it starts, in order
//!
//! ```text
//! 1  bind the endpoint            run::claim_endpoint
//! 2  watch for a stop signal      (this module)
//! 3  open the device identity     run::open_sync_identity   — fatal if it fails
//! 4  start the session worker     run::start_session_host
//! 5  build the Agent
//! 6  device network, gateway,     run::start_device_services
//!    delegate, sync control
//! 7  install the event waker      Agent::set_event_waker    — starts the gateway ensure
//! 8  answer the endpoint          run::answer_endpoint
//! 9  the loop                     supervise
//! ```
//!
//! ## What it does not start
//!
//! Each of these is an omission by not calling, not a switch: the update
//! lease, the Agents menu watcher, the account capability and its credential
//! store, the reminder worker, the Socials publisher, a pending link, the
//! hotkey thread, the tray, the wake observers and the window loop. Machine
//! input is never enabled. `server_mode_names_nothing_it_must_not_start` in
//! this module's tests reads this file to hold that.
//!
//! ## Three places it deliberately differs from a rooted desktop start
//!
//! - **A device identity that cannot be opened is fatal.** `run` degrades to an
//!   Agent with no device network, gateway or delegate, which on a desktop is
//!   a window that says sync is unavailable. On a server it would be a process
//!   that runs and does nothing, so [`run_server`] refuses to start.
//! - **The delegate is woken on a fixed tick** ([`DELEGATE_TICK`]), because
//!   the native sleep and time-change wakes belong to the window loop.
//! - **A termination signal stops it**, without a question, once, and within
//!   a bound ([`STOP_PATIENCE`]).
//!
//! ## The signal goes to the Agent only
//!
//! The gateway, session and delegate workers install no signal handler. A
//! service manager that signals the whole process group ends them by the
//! default action, with no Store shutdown. This module assumes the signal is
//! delivered to the Agent process alone; holding a deployment to that is the
//! packaging plan's, not this one's.

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use koru_window::WindowApp as _;

use crate::event::{AgentEvent, Mailbox};
use crate::run::{self, StartError};
use crate::{Agent, Paths};

/// How often the loop wakes the delegate's scheduler.
///
/// **This is a correctness bound, not a convenience.** The scheduler sleeps a
/// duration computed once from the wall clock and waits it out on a monotonic
/// timer, and an occurrence evaluated in the minute after its own is recorded
/// as missed: its Run does not start. A desktop re-evaluates when the
/// operating system reports a resume or a clock change; a server has no such
/// source, so after the clock is stepped, the machine is suspended or a zone
/// changes, the scheduler is late by up to one period of this tick.
///
/// Twenty seconds is three ticks inside every minute: a step leaves at least
/// forty seconds of the occurrence's minute, and one tick lost to a busy loop
/// still lands inside it. One wake costs the delegate one evaluation and one
/// schedule discovery, so the price is three of each per minute on an
/// otherwise idle server.
pub const DELEGATE_TICK: Duration = Duration::from_secs(20);

/// How long the loop waits, after a signal, for the Agent to finish its quit
/// before leaving without it.
///
/// A quit stops the session worker on a thread and the Agent sets its exit
/// when that thread reports back. The session worker's own stop deadline is
/// ten seconds, after which it is killed, and the worker supervision waits at
/// most thirty seconds for the report of a stop. This is just past that
/// thirty, so even the supervision's own "timed out" answer arrives inside it
/// and a healthy stop never meets it.
///
/// **The whole stop is bounded by the sum of three waits**: this one; the
/// session supervisor's drop, which is the kill fallback and waits at most
/// thirty seconds for its acknowledgement; and
/// [`crate::app::finish_quit_stop`], at most twelve seconds for the delegate
/// and the gateway. Seventy-seven seconds if every fallback is met, which is
/// inside the ninety a service manager commonly allows before it kills. A
/// healthy stop takes as long as the session worker and the gateway take to
/// stop, which is seconds.
pub const STOP_PATIENCE: Duration = Duration::from_secs(35);

/// How the supervisor's loop ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerExit {
    /// The Agent said it was done: a signal, or `koru-agent stop`, ran the
    /// ordinary quit to its end.
    Stopped,
    /// A signal asked for a stop and the Agent had not finished
    /// [`STOP_PATIENCE`] later. The process leaves anyway; what its workers
    /// were doing is ended by the supervision's fallbacks.
    StopTimedOut,
    /// Every producer of events is gone, so nothing could ever end the loop.
    Abandoned,
}

/// Which termination signal arrived. Carried for the log line only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Signal {
    /// Ctrl+C on Windows, SIGINT on Unix.
    Interrupt,
    /// SIGTERM. Unix only.
    #[cfg_attr(not(unix), allow(dead_code))]
    Terminate,
}

impl Signal {
    fn name(self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::Terminate => "terminate",
        }
    }
}

/// What the loop receives.
pub(crate) enum Message {
    /// An event a producer raised through the [`Mailbox`].
    Event(Box<AgentEvent>),
    /// A termination signal.
    Stop(Signal),
}

/// What waiting produced.
pub(crate) enum Waited {
    Message(Message),
    /// The instant waited for arrived first.
    Elapsed,
    /// Every sender is gone.
    Closed,
}

/// The loop's clock and its queue, as one seam, because a wait is both: it
/// ends at a message or at an instant. The real one is a channel and the
/// monotonic clock; the tests' is a script.
pub(crate) trait Inbox {
    fn now(&self) -> Instant;
    /// The next message, or [`Waited::Elapsed`] once `until` has arrived. With
    /// `until` already past it returns at once: a message if one is waiting.
    fn wait(&mut self, until: Instant) -> Waited;
}

/// What the loop needs of the Agent. The real one is [`AgentUnderLoop`]; the
/// tests' records what was asked of it.
pub(crate) trait Supervised {
    fn wake(&mut self, event: AgentEvent);
    fn maintenance_deadline(&self) -> Option<Instant>;
    fn maintain(&mut self, now: Instant);
    fn should_exit(&self) -> bool;
    /// Begin the quit that asks nobody.
    fn quit(&mut self);
    /// Wake the delegate's scheduler.
    fn tick(&mut self);
}

/// **The loop.** Receive, hand to the Agent, maintain at the deadline, tick
/// the delegate, leave when the Agent says so.
///
/// There is no switch over event kinds. The kinds whose producers run on a
/// server are `ShellArrived`, `ShellSaid`, `ShellGone`, `StoreAvailability`,
/// `SettingsChanged`, `GlobalConsentRefreshed`, `PairingChanged`,
/// `RootSyncChanged` and `SessionHostStopped`; every kind is passed to the
/// Agent unchanged, in the order raised, because none needs a display to
/// handle and a filter would be one more list to keep in step with the enum.
///
/// **It must keep running after a stop is asked for.** A quit completes when
/// `SessionHostStopped` arrives and the Agent sets its exit, so the signal
/// only starts the quit; the loop goes on receiving until
/// [`Supervised::should_exit`], or until [`STOP_PATIENCE`] has passed.
///
/// A second signal while the first stop is under way starts nothing and
/// shortens nothing: the stop already has a bound.
pub(crate) fn supervise(agent: &mut impl Supervised, inbox: &mut impl Inbox) -> ServerExit {
    let mut next_tick = inbox.now() + DELEGATE_TICK;
    let mut stop_deadline: Option<Instant> = None;
    loop {
        if agent.should_exit() {
            return ServerExit::Stopped;
        }
        if stop_deadline.is_some_and(|deadline| inbox.now() >= deadline) {
            return ServerExit::StopTimedOut;
        }
        let until = [
            Some(next_tick),
            agent.maintenance_deadline(),
            stop_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(next_tick);
        match inbox.wait(until) {
            Waited::Message(Message::Event(event)) => agent.wake(*event),
            Waited::Message(Message::Stop(signal)) => {
                if stop_deadline.is_none() {
                    tracing::info!(
                        target: "koru::server",
                        signal = signal.name(),
                        "a signal asked the Agent to stop"
                    );
                    stop_deadline = Some(inbox.now() + STOP_PATIENCE);
                    agent.quit();
                } else {
                    tracing::info!(
                        target: "koru::server",
                        signal = signal.name(),
                        "another signal arrived while the Agent is already stopping"
                    );
                }
            }
            Waited::Elapsed => {}
            Waited::Closed => return ServerExit::Abandoned,
        }
        // Timed duties run on every turn, whatever woke it, so a steady
        // stream of events can delay neither maintenance nor the tick.
        let now = inbox.now();
        if agent
            .maintenance_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            agent.maintain(now);
        }
        if now >= next_tick {
            agent.tick();
            next_tick = now + DELEGATE_TICK;
        }
    }
}

/// The Agent as the loop sees it: one lock for the length of each call, the
/// same rule `run`'s window host follows, because the rendezvous thread pushes
/// connections into the same mutex.
struct AgentUnderLoop {
    agent: Arc<Mutex<Agent>>,
    /// `None` when the delegate could not be supervised; there is then no
    /// scheduler to be late.
    delegate_wakes: Option<crate::delegate_host::DelegateWakeIngress>,
}

impl Supervised for AgentUnderLoop {
    fn wake(&mut self, event: AgentEvent) {
        // The kind before the dispatch consumes the event: the stall line
        // names what the loop was inside, never what it carried.
        let kind = event.kind();
        // A poisoned mutex means a producer panicked. As in the window host,
        // carrying on is better than taking the supervision down with it.
        if let Ok(mut agent) = run::locked(&self.agent) {
            let started = Instant::now();
            agent.wake(event);
            let took = started.elapsed();
            if took >= run::STALL_FLOOR {
                tracing::warn!(
                    target: "koru::stall",
                    took_ms = took.as_millis() as u64,
                    kind,
                    "agent event dispatch stalled"
                );
            }
        }
    }

    fn maintenance_deadline(&self) -> Option<Instant> {
        run::locked(&self.agent)
            .ok()
            .and_then(|agent| agent.maintenance_deadline())
    }

    fn maintain(&mut self, now: Instant) {
        if let Ok(mut agent) = run::locked(&self.agent) {
            agent.maintain(now);
        }
    }

    fn should_exit(&self) -> bool {
        run::locked(&self.agent).is_ok_and(|agent| agent.should_exit())
    }

    fn quit(&mut self) {
        if let Ok(mut agent) = run::locked(&self.agent) {
            agent.quit_without_asking();
        }
    }

    fn tick(&mut self) {
        if let Some(delegate) = &self.delegate_wakes {
            // Nonblocking and coalesced: it sets an edge and returns.
            delegate.wake();
        }
    }
}

/// The real inbox: the channel every producer and the signal watcher send
/// into, and the monotonic clock.
struct ChannelInbox {
    receiver: mpsc::Receiver<Message>,
}

impl Inbox for ChannelInbox {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wait(&mut self, until: Instant) -> Waited {
        let wait = until.saturating_duration_since(Instant::now());
        let received = if wait.is_zero() {
            self.receiver.try_recv().map_err(|error| match error {
                mpsc::TryRecvError::Empty => mpsc::RecvTimeoutError::Timeout,
                mpsc::TryRecvError::Disconnected => mpsc::RecvTimeoutError::Disconnected,
            })
        } else {
            self.receiver.recv_timeout(wait)
        };
        match received {
            Ok(message) => Waited::Message(message),
            Err(mpsc::RecvTimeoutError::Timeout) => Waited::Elapsed,
            Err(mpsc::RecvTimeoutError::Disconnected) => Waited::Closed,
        }
    }
}

/// How a producer's event reaches the loop: the closure [`Mailbox::attach`]
/// is given, with a channel where the window loop has winit's proxy.
fn deliver_into(sender: mpsc::Sender<Message>) -> impl Fn(AgentEvent) + Send + 'static {
    move |event| {
        // A send that fails is not an error: the loop has ended, which is the
        // ordinary way this process stops.
        let _ = sender.send(Message::Event(Box::new(event)));
    }
}

/// Start the Agent with no window, pump it until it stops, and stop its
/// workers within a bound.
///
/// `paths` must name an explicit root. The command line already refuses
/// `--server` without `--root`; this refuses again so that no other caller can
/// run a server on a person's own per-user root.
pub fn run_server(paths: Paths) -> Result<ServerExit, StartError> {
    if paths.explicit_root().is_none() {
        return Err(StartError::Failed(
            "server mode needs an explicit root and was given none".into(),
        ));
    }

    // 1. The claim. Before anything is read, written or spawned.
    let listener = run::claim_endpoint(&paths)?;

    // 2. The stop signal, before any worker exists, so a signal that arrives
    //    during start-up is queued and becomes an orderly stop the moment the
    //    loop begins rather than ending the process around its children.
    let (sender, receiver) = mpsc::channel::<Message>();
    watch_signals(sender.clone())?;

    // 3. Who this device is. **Fatal here**, where `run` carries on without a
    //    device network: a server with no identity starts no gateway and no
    //    delegate, and would sit there healthy-looking and useless. The text
    //    carries the error's code and nothing from the identity itself.
    let identity = run::open_sync_identity(&paths).map_err(|error| {
        koru_ipc::ipc_debug!("agent: sync identity unavailable: {error}");
        StartError::Failed(format!(
            "the device identity could not be opened ({}); a server does not start without one",
            error.code
        ))
    })?;
    let (device, sync_status) = run::device_and_sync_status(&paths, &identity);
    let local_device = identity.descriptor.device_id;
    let endpoint_secret = identity.endpoint_secret.secret_bytes();
    let lifecycle_identity = (
        identity.descriptor.clone(),
        identity.signing_key.secret_bytes(),
    );
    let instance_id = uuid::Uuid::new_v4().to_string();

    // 4. The session worker, ready before anything can be routed to it.
    let (session_host, session_lifecycle, session_catalog) = run::start_session_host(&paths)?;

    // 5. The Agent. Machine input stays at its default, off; no update lease,
    //    health token or rehearsal is set.
    let mut initial_agent = Agent::new(paths.clone(), device.id.clone(), instance_id.clone());
    initial_agent.set_session_lifecycle(session_lifecycle, session_catalog);
    initial_agent.set_sync_identity(
        identity,
        sync_status.trusted_device_count,
        sync_status.authorization_generation,
    );
    run::note_pending_profile_cleanup(&mut initial_agent, &paths);
    let agent = Arc::new(Mutex::new(initial_agent));
    // Producers start below and the loop does not exist until step 9.
    // Everything raised in between is held and replayed in order.
    let mailbox = Mailbox::new();
    let waker = mailbox.waker();

    // 6. The device network, the gateway, the delegate and sync control. The
    //    two handles it returns are what a desktop composes its account from;
    //    a server has no account, so they are dropped.
    run::start_device_services(
        &paths,
        local_device,
        endpoint_secret,
        Some(&lifecycle_identity),
        &agent,
        &waker,
    )?;

    // 7. `set_event_waker` starts the gateway ensure, after the device network
    //    its bridge handshake needs.
    if let Ok(mut agent) = agent.lock() {
        agent.set_event_waker(waker.clone());
    }

    // 8. Answer the endpoint.
    run::answer_endpoint(
        listener,
        &agent,
        &waker,
        device.id.clone(),
        instance_id,
        sync_status,
    )?;

    // 9. The loop. Never returns until the Agent stops.
    let delegate_wakes = crate::delegate_host::current().map(|host| host.wake_ingress());
    mailbox.attach(deliver_into(sender));
    tracing::info!(
        target: "koru::server",
        tick_secs = DELEGATE_TICK.as_secs(),
        delegate = delegate_wakes.is_some(),
        "the display-free supervisor is running"
    );
    let ended = supervise(
        &mut AgentUnderLoop {
            agent,
            delegate_wakes,
        },
        &mut ChannelInbox { receiver },
    );
    if ended != ServerExit::Stopped {
        tracing::warn!(
            target: "koru::server",
            ended = ?ended,
            "the supervisor's loop ended without the Agent finishing its quit"
        );
    }
    // Session trees stop first, as they do when the window host is dropped;
    // the supervisor's drop is the bounded kill fallback.
    drop(session_host);
    // Then the process waits, briefly, for the delegate and the gateway the
    // quit asked to stop, so the Store is not ended by force a few
    // milliseconds into a shutdown it had agreed to. Bounded.
    crate::app::finish_quit_stop();
    Ok(ended)
}

/// Turn termination signals into [`Message::Stop`], on a thread of their own.
///
/// Returns once the handlers are registered, so a failure to register is a
/// failure to start rather than a server nothing can stop cleanly. The thread
/// lives for the process: a signal that arrives after the loop has ended finds
/// a closed channel and the thread ends.
///
/// The watcher only sends. The quit itself is begun by the loop, on the
/// loop's thread, so nothing here ever takes the Agent's lock.
fn watch_signals(stop: mpsc::Sender<Message>) -> Result<(), StartError> {
    let (ready, readied) = mpsc::sync_channel::<Result<(), String>>(1);
    std::thread::Builder::new()
        .name("koru-agent signals".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready.send(Err(error.to_string()));
                    return;
                }
            };
            runtime.block_on(signals(ready, stop));
        })
        .map_err(|error| StartError::Failed(format!("signal handling: {error}")))?;
    match readied.recv() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(why)) => Err(StartError::Failed(format!("signal handling: {why}"))),
        Err(_) => Err(StartError::Failed(
            "signal handling: the watcher ended before it was ready".into(),
        )),
    }
}

/// SIGINT and SIGTERM.
#[cfg(unix)]
async fn signals(ready: mpsc::SyncSender<Result<(), String>>, stop: mpsc::Sender<Message>) {
    use tokio::signal::unix::{SignalKind, signal};

    let registered = signal(SignalKind::interrupt()).and_then(|interrupt| {
        signal(SignalKind::terminate()).map(|terminate| (interrupt, terminate))
    });
    let (mut interrupt, mut terminate) = match registered {
        Ok(streams) => streams,
        Err(error) => {
            let _ = ready.send(Err(error.to_string()));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    loop {
        let received = tokio::select! {
            received = interrupt.recv() => received.map(|()| Signal::Interrupt),
            received = terminate.recv() => received.map(|()| Signal::Terminate),
        };
        let Some(signal) = received else { return };
        if stop.send(Message::Stop(signal)).is_err() {
            return;
        }
    }
}

/// Ctrl+C. A release build has no console and so no Ctrl+C; `koru-agent stop
/// --root <path>` stops it through the same quit.
#[cfg(windows)]
async fn signals(ready: mpsc::SyncSender<Result<(), String>>, stop: mpsc::Sender<Message>) {
    let mut interrupt = match tokio::signal::windows::ctrl_c() {
        Ok(stream) => stream,
        Err(error) => {
            let _ = ready.send(Err(error.to_string()));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    while interrupt.recv().await.is_some() {
        if stop.send(Message::Stop(Signal::Interrupt)).is_err() {
            return;
        }
    }
}

#[cfg(not(any(unix, windows)))]
async fn signals(ready: mpsc::SyncSender<Result<(), String>>, _stop: mpsc::Sender<Message>) {
    let _ = ready.send(Err("this platform has no termination signal Koru handles".into()));
}

#[cfg(test)]
mod tests;
