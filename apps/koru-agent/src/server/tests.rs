// SPDX-License-Identifier: MPL-2.0

//! The supervisor's loop, driven by a script: a fake Agent that records what
//! was asked of it and a fake clock that moves only when the loop waits.
//! Nothing here starts a worker, binds an endpoint or installs a signal
//! handler.

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;

use koru_agent_worker::supervisor::{StopOutcome, StopRequest};

use super::*;
use crate::registry::ConnectionId;

/// One thing the loop asked of the Agent, and when, measured from the start.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Did {
    Wake(&'static str, Duration),
    Maintain(Duration),
    Quit(Duration),
    Tick(Duration),
}

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

/// The fake clock and queue. Time is whatever the last wait left it at: a
/// wait ends at the next scripted arrival if that comes first, and otherwise
/// at the instant asked for.
struct Script {
    start: Instant,
    now: Rc<Cell<Instant>>,
    /// `None` closes the queue: every sender is gone.
    arrivals: VecDeque<(Duration, Option<Message>)>,
}

impl Inbox for Script {
    fn now(&self) -> Instant {
        self.now.get()
    }

    fn wait(&mut self, until: Instant) -> Waited {
        let now = self.now.get();
        let Some((at, _)) = self.arrivals.front() else {
            return Waited::Closed;
        };
        let at = self.start + *at;
        if at <= until.max(now) {
            let (_, message) = self.arrivals.pop_front().expect("peeked");
            self.now.set(now.max(at));
            message.map_or(Waited::Closed, Waited::Message)
        } else {
            self.now.set(now.max(until));
            Waited::Elapsed
        }
    }
}

/// The fake Agent.
struct Fake {
    start: Instant,
    now: Rc<Cell<Instant>>,
    did: Vec<Did>,
    /// Maintenance deadlines still to come, nearest first.
    deadlines: VecDeque<Duration>,
    exit: bool,
    /// The event kind whose arrival ends this Agent, as `SessionHostStopped`
    /// ends the real one after a quit.
    exits_on: Option<&'static str>,
}

impl Fake {
    fn at(&self) -> Duration {
        self.now.get() - self.start
    }
}

impl Supervised for Fake {
    fn wake(&mut self, event: AgentEvent) {
        let kind = event.kind();
        self.did.push(Did::Wake(kind, self.at()));
        if self.exits_on == Some(kind) {
            self.exit = true;
        }
    }
    fn maintenance_deadline(&self) -> Option<Instant> {
        self.deadlines.front().map(|after| self.start + *after)
    }
    fn maintain(&mut self, now: Instant) {
        assert_eq!(now, self.now.get(), "maintain is given the loop's clock");
        self.deadlines.pop_front();
        self.did.push(Did::Maintain(self.at()));
    }
    fn should_exit(&self) -> bool {
        self.exit
    }
    fn quit(&mut self) {
        self.did.push(Did::Quit(self.at()));
    }
    fn tick(&mut self) {
        self.did.push(Did::Tick(self.at()));
    }
}

fn scene(arrivals: Vec<(u64, Option<Message>)>) -> (Fake, Script) {
    let start = Instant::now();
    let now = Rc::new(Cell::new(start));
    (
        Fake {
            start,
            now: Rc::clone(&now),
            did: Vec::new(),
            deadlines: VecDeque::new(),
            exit: false,
            exits_on: None,
        },
        Script {
            start,
            now,
            arrivals: arrivals
                .into_iter()
                .map(|(at, message)| (secs(at), message))
                .collect(),
        },
    )
}

fn event(event: AgentEvent) -> Option<Message> {
    Some(Message::Event(Box::new(event)))
}

fn stop(signal: Signal) -> Option<Message> {
    Some(Message::Stop(signal))
}

fn gone(id: u64) -> AgentEvent {
    AgentEvent::ShellGone {
        connection: ConnectionId(id),
    }
}

fn session_host_stopped() -> AgentEvent {
    AgentEvent::SessionHostStopped {
        outcome: StopOutcome::NotRunning {
            request: StopRequest::Serving,
            generation: None,
            last_exit: None,
        },
    }
}

/// The nine kinds whose producers run on a server, in an order no sort would
/// produce, ending with the one that ends the loop.
fn the_kinds_a_server_sees() -> Vec<AgentEvent> {
    vec![
        AgentEvent::ShellArrived {
            connection: ConnectionId(1),
            pid: None,
        },
        AgentEvent::ShellSaid {
            connection: ConnectionId(1),
            id: 1,
            body: Box::new(koru_agent_wire::Request::Shutdown),
            read_at: Instant::now(),
        },
        AgentEvent::StoreAvailability {
            warning: None,
            running: true,
        },
        AgentEvent::SettingsChanged,
        AgentEvent::GlobalConsentRefreshed {
            generation: 1,
            consent: None,
        },
        AgentEvent::RootSyncChanged,
        AgentEvent::PairingChanged,
        gone(1),
        session_host_stopped(),
    ]
}

#[test]
fn every_kind_a_server_sees_reaches_the_agent_in_the_order_raised() {
    let arrivals = the_kinds_a_server_sees()
        .into_iter()
        .enumerate()
        .map(|(index, kind)| (index as u64 + 1, event(kind)))
        .collect();
    let (mut agent, mut inbox) = scene(arrivals);
    agent.exits_on = Some("SessionHostStopped");

    assert_eq!(supervise(&mut agent, &mut inbox), ServerExit::Stopped);

    let woken: Vec<&str> = agent
        .did
        .iter()
        .map(|did| match did {
            Did::Wake(kind, _) => *kind,
            other => panic!("nothing but events was scripted: {other:?}"),
        })
        .collect();
    assert_eq!(
        woken,
        [
            "ShellArrived",
            "ShellSaid",
            "StoreAvailability",
            "SettingsChanged",
            "GlobalConsentRefreshed",
            "RootSyncChanged",
            "PairingChanged",
            "ShellGone",
            "SessionHostStopped",
        ]
    );
}

/// The loop has no list of kinds: one that no server producer raises is passed
/// to the Agent like any other, so the enum can grow without this loop
/// silently dropping the new kind.
#[test]
fn a_kind_no_server_producer_raises_is_still_passed_on() {
    let (mut agent, mut inbox) = scene(vec![
        (1, event(AgentEvent::AccountChanged)),
        (2, event(AgentEvent::UpdateChanged)),
        (3, None),
    ]);
    assert_eq!(supervise(&mut agent, &mut inbox), ServerExit::Abandoned);
    assert_eq!(
        agent.did,
        [
            Did::Wake("AccountChanged", secs(1)),
            Did::Wake("UpdateChanged", secs(2)),
        ]
    );
}

#[test]
fn maintenance_runs_at_its_deadline_and_not_before() {
    let (mut agent, mut inbox) = scene(vec![(2, event(gone(1))), (7, event(gone(2))), (9, None)]);
    agent.deadlines = [secs(5), secs(8)].into();

    assert_eq!(supervise(&mut agent, &mut inbox), ServerExit::Abandoned);

    assert_eq!(
        agent.did,
        [
            // An event before the deadline does not bring maintenance forward.
            Did::Wake("ShellGone", secs(2)),
            // With nothing arriving, the loop wakes itself exactly then.
            Did::Maintain(secs(5)),
            Did::Wake("ShellGone", secs(7)),
            Did::Maintain(secs(8)),
        ]
    );
}

/// A deadline that passed while the loop was inside an event is met on the
/// same turn, after the event, rather than waiting for another wake.
#[test]
fn maintenance_that_fell_due_during_an_event_runs_on_that_turn() {
    let (mut agent, mut inbox) = scene(vec![(6, event(gone(1))), (7, None)]);
    // Due at the very instant the event arrives.
    agent.deadlines = [secs(6)].into();

    assert_eq!(supervise(&mut agent, &mut inbox), ServerExit::Abandoned);

    assert_eq!(
        agent.did,
        [Did::Wake("ShellGone", secs(6)), Did::Maintain(secs(6))]
    );
}

/// **The tick's period is a correctness bound.** An occurrence evaluated in
/// the minute after its own is recorded as missed, and on a server this tick
/// is the only thing that re-evaluates after a clock step. Three ticks must
/// fit inside a minute, so one lost tick still lands inside the occurrence's
/// own minute. Raising this constant past twenty seconds needs that argument
/// made again, not just this number changed.
#[test]
fn the_delegate_tick_is_well_under_a_minute() {
    assert!(DELEGATE_TICK > Duration::ZERO);
    assert!(DELEGATE_TICK <= secs(20), "{DELEGATE_TICK:?}");
    assert!(DELEGATE_TICK * 3 <= secs(60), "{DELEGATE_TICK:?}");
}

#[test]
fn the_delegate_is_ticked_on_its_period_when_nothing_else_happens() {
    let (mut agent, mut inbox) = scene(vec![(65, None)]);

    assert_eq!(supervise(&mut agent, &mut inbox), ServerExit::Abandoned);

    assert_eq!(
        agent.did,
        [
            Did::Tick(DELEGATE_TICK),
            Did::Tick(DELEGATE_TICK * 2),
            Did::Tick(DELEGATE_TICK * 3),
        ]
    );
}

/// A busy server must not be a late one: an event every second for a minute
/// delays no tick.
#[test]
fn a_steady_stream_of_events_does_not_delay_the_tick() {
    let mut arrivals: Vec<(u64, Option<Message>)> = (1..=61)
        .map(|second| (second, event(gone(second))))
        .collect();
    arrivals.push((62, None));
    let (mut agent, mut inbox) = scene(arrivals);

    assert_eq!(supervise(&mut agent, &mut inbox), ServerExit::Abandoned);

    let ticks: Vec<Did> = agent
        .did
        .iter()
        .filter(|did| matches!(did, Did::Tick(_)))
        .cloned()
        .collect();
    assert_eq!(
        ticks,
        [
            Did::Tick(DELEGATE_TICK),
            Did::Tick(DELEGATE_TICK * 2),
            Did::Tick(DELEGATE_TICK * 3),
        ]
    );
    let woken = agent
        .did
        .iter()
        .filter(|did| matches!(did, Did::Wake(..)))
        .count();
    assert_eq!(woken, 61);
}

/// **One stop.** The first signal begins the quit; the loop then keeps
/// receiving, because the quit completes only when the session worker reports
/// back. A second signal during that wait begins nothing.
#[test]
fn a_signal_begins_one_quit_and_the_loop_waits_for_the_agent_to_finish() {
    let (mut agent, mut inbox) = scene(vec![
        (3, stop(Signal::Terminate)),
        (4, stop(Signal::Interrupt)),
        (4, stop(Signal::Terminate)),
        (5, event(session_host_stopped())),
        // Never reached: the loop has left.
        (6, event(gone(1))),
    ]);
    agent.exits_on = Some("SessionHostStopped");

    assert_eq!(supervise(&mut agent, &mut inbox), ServerExit::Stopped);

    assert_eq!(
        agent.did,
        [Did::Quit(secs(3)), Did::Wake("SessionHostStopped", secs(5)),]
    );
}

/// **And bounded.** An Agent that never finishes its quit does not hold the
/// process: the loop leaves `STOP_PATIENCE` after the first signal, to the
/// second, whatever else arrives meanwhile, and a later signal does not move
/// the bound in either direction.
#[test]
fn a_quit_that_never_finishes_ends_the_loop_at_the_bound() {
    let mut arrivals = vec![(3, stop(Signal::Terminate))];
    arrivals.extend((1..=20).map(|step| (3 + step * 5, event(gone(step)))));
    arrivals.push((10, stop(Signal::Terminate)));
    arrivals.sort_by_key(|(at, _)| *at);
    let (mut agent, mut inbox) = scene(arrivals);

    assert_eq!(supervise(&mut agent, &mut inbox), ServerExit::StopTimedOut);

    assert_eq!(inbox.now() - inbox.start, secs(3) + STOP_PATIENCE);
    let quits: Vec<&Did> = agent
        .did
        .iter()
        .filter(|did| matches!(did, Did::Quit(_)))
        .collect();
    assert_eq!(quits, [&Did::Quit(secs(3))]);
    // Events went on being handled until the bound, and none after it.
    let last_wake = agent.did.iter().rev().find_map(|did| match did {
        Did::Wake(_, at) => Some(*at),
        _ => None,
    });
    assert_eq!(last_wake, Some(secs(3) + STOP_PATIENCE));
}

/// The stop's bound is a number somebody chose. It is past the thirty
/// seconds the worker supervision itself waits for the report of a stop, so
/// that report always lands inside it. And with the two waits that follow the
/// loop (the session supervisor's drop, thirty seconds at most, and
/// `app::finish_quit_stop`, twelve at most) the whole stop stays inside the
/// ninety seconds a service manager commonly allows before it kills.
#[test]
fn the_stop_bound_leaves_room_inside_a_service_managers_patience() {
    assert!(STOP_PATIENCE > secs(30), "{STOP_PATIENCE:?}");
    assert!(
        STOP_PATIENCE + secs(30) + secs(12) < secs(90),
        "{STOP_PATIENCE:?}"
    );
}

/// `koru-agent stop` reaches the same quit over a connection. The loop needs
/// no signal for it: it leaves when the Agent says it is done.
#[test]
fn a_quit_the_agent_decides_itself_ends_the_loop_without_a_signal() {
    let (mut agent, mut inbox) = scene(vec![
        (1, event(gone(1))),
        (2, event(session_host_stopped())),
        (3, event(gone(2))),
    ]);
    agent.exits_on = Some("SessionHostStopped");

    assert_eq!(supervise(&mut agent, &mut inbox), ServerExit::Stopped);

    assert_eq!(
        agent.did,
        [
            Did::Wake("ShellGone", secs(1)),
            Did::Wake("SessionHostStopped", secs(2)),
        ]
    );
}

/// What the real loop is given in place of winit's proxy: the mailbox replays
/// what was raised before the loop existed, in order, and then delivers
/// straight through, all of it as loop messages on the channel.
#[test]
fn the_mailbox_delivers_into_the_loops_channel_in_order() {
    let mailbox = Mailbox::new();
    let waker = mailbox.waker();
    waker.wake(gone(1));
    waker.wake(gone(2));

    let (sender, receiver) = mpsc::channel::<Message>();
    mailbox.attach(deliver_into(sender));
    waker.wake(gone(3));

    let mut inbox = ChannelInbox { receiver };
    let mut seen = Vec::new();
    // An instant already past: the wait returns at once, with what is queued.
    let past = Instant::now();
    while let Waited::Message(message) = inbox.wait(past) {
        match message {
            Message::Event(event) => match *event {
                AgentEvent::ShellGone { connection } => seen.push(connection.0),
                other => panic!("not one of ours: {other:?}"),
            },
            Message::Stop(signal) => panic!("no signal was sent: {signal:?}"),
        }
    }
    assert_eq!(seen, [1, 2, 3]);
    // Empty, and still open: the mailbox holds a sender.
    assert!(matches!(inbox.wait(past), Waited::Elapsed));
    drop(waker);
    drop(mailbox);
    assert!(matches!(inbox.wait(past), Waited::Closed));
}

/// **The quit a signal reaches is the one that asks nobody**, on the real
/// Agent: the object is built as the Agent's own unit tests build it, with no
/// worker, endpoint or loop behind it. With no session worker to stop it is
/// done at once, and the real loop adapter reports that.
#[test]
fn a_signal_quits_the_real_agent_without_a_question() {
    let agent = Arc::new(Mutex::new(Agent::new(
        Paths::for_root(std::path::PathBuf::from("C:/nowhere")),
        "device".into(),
        "instance".into(),
    )));
    let start = Instant::now();
    let mut inbox = Script {
        start,
        now: Rc::new(Cell::new(start)),
        // At once, so the wait cannot depend on any deadline the real Agent
        // holds against the real clock.
        arrivals: [(secs(0), stop(Signal::Terminate)), (secs(500), None)].into(),
    };
    let mut under_loop = AgentUnderLoop {
        agent: Arc::clone(&agent),
        delegate_wakes: None,
        routine_clock: None,
    };

    assert_eq!(supervise(&mut under_loop, &mut inbox), ServerExit::Stopped);

    assert_eq!(inbox.now(), start, "the quit waited for nothing");
    assert!(agent.lock().unwrap().should_exit());
}

#[test]
fn a_server_is_never_run_on_the_per_user_root() {
    // Refused before the endpoint is claimed or anything else is touched.
    let refused = run_server(Paths::for_channel(std::path::PathBuf::from("ordinary")));
    match refused {
        Err(StartError::Failed(why)) => assert!(why.contains("explicit root"), "{why}"),
        other => panic!(
            "a server started on a channel root: {:?}",
            other.map(|_| ())
        ),
    }
}

/// This file with its comments removed: what `run_server` can actually call.
fn code_of(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// **The omissions are held by reading the source.** Server mode leaves things
/// out by not calling them, and nothing at run time can observe a call that
/// was never made, so this is the check there is: none of the desktop-only
/// producers, and in particular no account capability and no operating-system
/// credential store, is named anywhere in the supervisor's code.
#[test]
fn server_mode_names_nothing_it_must_not_start() {
    let code = code_of(include_str!("../server.rs"));
    for absent in [
        // The account block, and the keyring it constructs.
        "OsCredentialStore",
        "koru_oauth_credentials",
        "AccountControl",
        "crate::account",
        "startup_capability",
        "cloud_origin",
        "crate::enrollment",
        "crate::cloud_host",
        "set_account_control",
        // The reminder and Socials workers.
        "crate::reminders",
        "ReminderWorker",
        "koru_socials_publisher",
        // The window loop, the tray, the hotkey, machine input.
        "koru_window::run",
        "crate::tray",
        "crate::hotkey",
        "set_hotkey_control",
        "set_machine_input_enabled",
        "crate::agents_menu",
        // Updates and links.
        "koru_update",
        "set_update_lease",
        "set_update_health_token",
        "set_rehearsal_updates",
        "resolve_link",
        // And never the desktop entry point itself.
        "run::run(",
        "crate::run(",
    ] {
        assert!(
            !code.contains(absent),
            "server mode names `{absent}`, which it must not start"
        );
    }
}

/// The account block was not lifted into a helper the server calls: the
/// shared start-up steps in `run.rs` name no account capability either.
#[test]
fn the_shared_start_up_steps_do_not_enter_the_account_block() {
    let run = include_str!("../run.rs");
    let from = run
        .find("// Start-up steps shared with")
        .expect("run.rs lost the marker above its shared steps");
    let to = run[from..]
        .find("fn start_macos_reminder_wakes")
        .map(|offset| from + offset)
        .expect("run.rs lost the function that follows its shared steps");
    let shared = code_of(&run[from..to]);
    for expected in [
        "fn claim_endpoint(",
        "fn open_sync_identity(",
        "fn device_and_sync_status(",
        "fn start_session_host(",
        "fn note_pending_profile_cleanup(",
        "fn start_device_services(",
        "fn answer_endpoint(",
    ] {
        assert!(shared.contains(expected), "{expected} is not a shared step");
    }
    for absent in [
        "OsCredentialStore",
        "koru_oauth_credentials",
        "AccountControl",
        "crate::account",
        "crate::enrollment",
        "crate::cloud_host",
        "crate::reminders",
        "koru_socials_publisher",
        "crate::hotkey",
        "crate::tray",
        "koru_window::run",
    ] {
        assert!(
            !shared.contains(absent),
            "a shared start-up step names `{absent}`"
        );
    }
}

/// The order is the readiness contract, for a server as for a desktop: both
/// entry points call the shared steps in the same sequence.
#[test]
fn both_entry_points_call_the_shared_steps_in_the_same_order() {
    fn positions(code: &str, calls: &[&str]) -> Vec<usize> {
        calls
            .iter()
            .map(|call| {
                code.find(call)
                    .unwrap_or_else(|| panic!("`{call}` is not called"))
            })
            .collect()
    }
    fn ascending(positions: &[usize]) -> bool {
        positions.windows(2).all(|pair| pair[0] < pair[1])
    }

    let server = code_of(include_str!("../server.rs"));
    let server = &server[server
        .find("pub fn run_server(")
        .expect("server.rs has no run_server")..];
    assert!(ascending(&positions(
        server,
        &[
            "run::claim_endpoint(",
            "watch_signals(",
            "run::open_sync_identity(",
            "run::device_and_sync_status(",
            "run::start_session_host(",
            "Agent::new(",
            "run::note_pending_profile_cleanup(",
            "run::start_device_services(",
            "set_event_waker(",
            "run::answer_endpoint(",
            "mailbox.attach(",
            "supervise(",
            "drop(session_host)",
            "finish_quit_stop(",
        ],
    )));

    let run = code_of(include_str!("../run.rs"));
    let run = &run[run.find("pub fn run(").expect("run.rs has no run")..];
    assert!(ascending(&positions(
        run,
        &[
            "claim_endpoint(",
            "open_sync_identity(",
            "device_and_sync_status(",
            "start_session_host(",
            "Agent::new(",
            "note_pending_profile_cleanup(",
            "start_device_services(",
            "set_event_waker(",
            "answer_endpoint(",
            "koru_window::run(",
            "finish_quit_stop(",
        ],
    )));
}
