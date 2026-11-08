// SPDX-License-Identifier: MPL-2.0

//! **The properties Plan 1.4 names, over a real endpoint.**
//!
//! No process is launched and no audio device exists; what is real is the
//! `koru-ipc` endpoint, the framing and the greeting. See
//! [`crate::test_worker`] for why every root is a fresh uuid path -- in short,
//! so that a suite can neither reach nor stop the worker somebody is dictating
//! through, and so that *never assert that a connection fails* is honoured by
//! arming the state instead.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::test_worker::{TestWorker, unbound_root};

/// Long enough for two threads and a pipe, short enough that a hang is a
/// failure rather than a wait.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

/// Collect events until `enough` says so, or fail.
fn collect_until(
    client: &VoiceClient,
    what: &str,
    enough: impl Fn(&[Event]) -> bool,
) -> Vec<Event> {
    let deadline = std::time::Instant::now() + PATIENCE;
    let mut seen = Vec::new();
    loop {
        seen.extend(client.drain());
        if enough(&seen) {
            return seen;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "waited {PATIENCE:?} for {what}; saw {seen:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn is_final(events: &[Event]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, Event::Ended { .. }))
}

// --------------------------------------------------------- the one send path

/// **One path a request may leave by**, and this is what says so.
///
/// The lesson is `koru-store-client`'s `send_with_retry`: a dozen methods each
/// built their own request from an address frozen at construction, so a service
/// that restarted on a new port left every one of them talking to nothing. The
/// guard has to be mechanical, because the mistake is not a bug anybody writes
/// on purpose -- it is what happens when a second method looks easier to write
/// than to route.
///
/// Two writes exist and both are in `link.rs`: the greeting, which is not a
/// request, and the writer thread's, which is the only place a
/// `koru_voice::Request` becomes bytes. `test_worker.rs` is scaffolding that
/// plays the *other* end and is deliberately not scanned.
#[test]
fn a_request_leaves_by_exactly_one_path() {
    let needle = "frame::write_frame(";
    let lib = include_str!("lib.rs");
    let link = include_str!("link.rs");
    assert_eq!(
        lib.matches(needle).count(),
        0,
        "the public surface must not write a frame; routing belongs in link.rs"
    );
    assert_eq!(
        link.matches(needle).count(),
        2,
        "link.rs writes exactly the greeting and the writer thread's request; \
         a third write is a second path a request can leave by"
    );
}

fn install_transport_pause(
    client: &VoiceClient,
    slot: impl FnOnce(&mut TestTransportHooks) -> &mut Option<Arc<TestPause>>,
) -> (Arc<TestPause>, std::sync::mpsc::Receiver<()>) {
    let pause = TestPause::new();
    let (done, completed) = std::sync::mpsc::channel();
    let mut hooks = client
        .shared
        .test_hooks
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    *slot(&mut hooks) = Some(Arc::clone(&pause));
    hooks.command_done = Some(done);
    (pause, completed)
}

#[test]
fn queued_begin_disconnected_before_dequeue_is_never_dialled_or_written() {
    let root = unbound_root();
    let worker = TestWorker::start(&root);
    let client = VoiceClient::at(Some(root));
    let (pause, completed) = install_transport_pause(&client, |hooks| &mut hooks.after_dequeue);

    client
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("Begin is queued immediately");
    pause.wait_until_reached();
    client.disconnect();
    pause.release();
    completed
        .recv_timeout(PATIENCE)
        .expect("transport completed stale command");

    assert_eq!(worker.handshakes(), 0);
    assert!(worker.requests().is_empty());
    assert_eq!(client.live(), None);
    worker.stop();
}

#[test]
fn eof_between_begin_write_and_binding_ends_that_generation_once() {
    let root = unbound_root();
    let worker = TestWorker::start(&root);
    let client = VoiceClient::at(Some(root));
    let (pause, completed) = install_transport_pause(&client, |hooks| &mut hooks.after_write);
    let live = client
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("Begin is queued immediately");
    pause.wait_until_reached();
    let id = client
        .shared
        .link
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .expect("the written Begin has a link")
        .0;
    client.shared.retire_link(id, None);
    pause.release();
    completed
        .recv_timeout(PATIENCE)
        .expect("transport completed retired command");

    let seen = collect_until(&client, "one terminal failure", is_final);
    assert_eq!(
        seen.iter()
            .filter(|event| matches!(event, Event::Ended { session, generation, .. } if *session == live.session && *generation == live.generation))
            .count(),
        1
    );
    assert_eq!(client.live(), None);
    worker.stop();
}

#[test]
fn dropping_last_owner_while_begin_waits_prevents_the_write_and_releases_shared() {
    let root = unbound_root();
    let worker = TestWorker::start(&root);
    let client = VoiceClient::at(Some(root));
    let weak = Arc::downgrade(&client.shared);
    let (pause, completed) = install_transport_pause(&client, |hooks| &mut hooks.before_send);
    client
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("Begin is queued immediately");
    pause.wait_until_reached();
    drop(client);
    pause.release();
    completed
        .recv_timeout(PATIENCE)
        .expect("transport observed owner shutdown");

    assert!(worker.requests().is_empty());
    let deadline = std::time::Instant::now() + PATIENCE;
    while weak.upgrade().is_some() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(
        weak.upgrade().is_none(),
        "transport/link threads retained Shared"
    );
    worker.stop();
}

// ----------------------------------------------------------- meeting, or not

/// A worker and a client on one root meet, and one dictation goes all the way
/// through.
#[test]
fn a_rooted_worker_and_a_rooted_client_meet() {
    let root = unbound_root();
    let worker = TestWorker::start(&root);
    let client = VoiceClient::at(Some(root.clone())).as_product("koru-voice-client-test");

    let live = client
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("the client could not begin a dictation");
    assert_eq!(client.live(), Some(live));
    collect_until(&client, "a partial", |seen| {
        seen.iter().any(|e| matches!(e, Event::Partial { .. }))
    });
    client.finish().expect("finish did not reach the worker");
    let seen = collect_until(&client, "a final and an end", is_final);

    assert!(
        seen.iter().any(|event| matches!(
            event,
            Event::Ended {
                reason: EndReason::Completed,
                ..
            }
        )),
        "the dictation did not complete: {seen:?}"
    );
    assert!(
        seen.iter()
            .all(|event| event.generation() == live.generation),
        "an event for another generation reached the host: {seen:?}"
    );
    assert_eq!(client.live(), None, "a completed session is not still live");
    assert!(client.connected());
    worker.stop();
}

/// **A rooted client and an unrooted worker find nothing rather than each
/// other**, and this asserts it by comparing the names rather than by dialling:
/// dialling the unrooted endpoint would be reaching for the worker a person may
/// be dictating through, which is exactly what the qualification exists to
/// prevent.
#[test]
fn a_rooted_client_and_an_unrooted_worker_derive_different_endpoints() {
    let rooted = koru_ipc::Endpoint::for_service_root(
        koru_ipc::VOICE,
        koru_channel::segment(),
        &unbound_root(),
    )
    .expect("the rooted endpoint could not be named");
    let plain = koru_ipc::Endpoint::for_service(koru_ipc::VOICE, koru_channel::segment())
        .expect("the channel endpoint could not be named");
    assert_ne!(
        rooted.name(),
        plain.name(),
        "a test root must not derive the channel's own endpoint"
    );
    assert_eq!(rooted.service(), koru_ipc::VOICE);
}

/// A client with nobody to ask returns from Begin immediately and reports the
/// terminal transport failure through the ordinary generation-tagged inbox.
#[test]
fn a_client_with_no_way_to_ask_for_a_worker_says_so_at_once() {
    let client = VoiceClient::at(Some(unbound_root()));
    let began = std::time::Instant::now();
    let live = client
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("enqueue is the synchronous contract");
    assert!(
        began.elapsed() < std::time::Duration::from_millis(100),
        "Begin dialled or ensured inline"
    );
    let seen = collect_until(&client, "the queued transport refusal", is_final);
    assert!(matches!(
        seen.as_slice(),
        [Event::Ended { session, generation, reason: EndReason::Failed { code: ErrorCode::WorkerUnavailable, .. } }]
            if *session == live.session && *generation == live.generation
    ));
    assert_eq!(client.live(), None, "a failed begin left a session armed");
}

/// **The client dials; somebody else supervises.** When nothing is listening it
/// asks once, then polls -- which is what lets `koru-agent` own starting the
/// worker while this crate links neither the agent nor its wire.
#[test]
fn nothing_listening_asks_the_host_once_and_then_waits() {
    let root = unbound_root();
    let asked = Arc::new(AtomicU64::new(0));
    let started: Arc<std::sync::Mutex<Option<TestWorker>>> = Arc::new(std::sync::Mutex::new(None));

    let client = VoiceClient::at(Some(root.clone()));
    {
        let asked = Arc::clone(&asked);
        let started = Arc::clone(&started);
        let root = root.clone();
        client.install_ensure(move || {
            asked.fetch_add(1, Ordering::Release);
            let mut held = started.lock().unwrap_or_else(|error| error.into_inner());
            if held.is_none() {
                *held = Some(TestWorker::start(&root));
            }
        });
    }

    client
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("the worker the ensure closure started was not reached");
    collect_until(&client, "the ensured worker to begin", |seen| {
        !seen.is_empty()
    });
    assert_eq!(
        asked.load(Ordering::Acquire),
        1,
        "the host was asked more than once for one dictation"
    );
    client.cancel().expect("cancel did not reach the worker");
    if let Some(worker) = started
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        worker.stop();
    }
}

#[test]
fn begin_then_finish_or_cancel_leave_on_one_fifo_without_inline_transport() {
    for terminal in ["finish", "cancel"] {
        let root = unbound_root();
        let worker = TestWorker::start(&root);
        let client = VoiceClient::at(Some(root));
        let began = std::time::Instant::now();
        client
            .begin(Requested::Auto, None, InputChoice::SystemDefault)
            .expect("begin enqueue");
        if terminal == "finish" {
            client.finish().expect("finish enqueue");
        } else {
            client.cancel().expect("cancel enqueue");
        }
        assert!(began.elapsed() < std::time::Duration::from_millis(100));
        let deadline = std::time::Instant::now() + PATIENCE;
        while worker.requests().len() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "commands were not drained"
            );
            std::thread::yield_now();
        }
        assert_eq!(worker.requests()[..2], ["begin", terminal]);
        worker.stop();
    }
}

// ------------------------------------------------------------- waking a host

/// The inbox is filled before the host is woken. The other order lets an idle
/// pass run between the two, find an empty inbox and sleep past the transcript
/// that has just arrived.
#[test]
fn the_inbox_is_filled_before_the_host_is_woken() {
    let root = unbound_root();
    let worker = TestWorker::start(&root);
    let client = VoiceClient::at(Some(root));
    let woken = Arc::new(AtomicU64::new(0));
    {
        let woken = Arc::clone(&woken);
        client.install_wake(move || {
            woken.fetch_add(1, Ordering::Release);
        });
    }

    client
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("begin");
    let deadline = std::time::Instant::now() + PATIENCE;
    while woken.load(Ordering::Acquire) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the host was never woken"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        !client.drain().is_empty(),
        "the host was woken to an empty inbox"
    );
    client.cancel().expect("cancel");
    worker.stop();
}

// ------------------------------------------------------- losing the worker

/// **Killing the worker mid-session surfaces a stable error, and the next
/// session reconnects without a restart.**
///
/// The stable error is `worker_unavailable`, which the contract crate reserves
/// for exactly this and describes as the client's code rather than the
/// worker's. Reconnection is the whole point of the single send path: nothing
/// in the host is rebuilt, no address is refreshed, and the next `begin`
/// dials the endpoint again because that is where dialling lives.
#[test]
fn losing_the_worker_mid_session_ends_it_and_the_next_one_reconnects() {
    let root = unbound_root();
    let worker = TestWorker::start(&root);
    let client = VoiceClient::at(Some(root.clone()));

    let first = client
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("the first begin");
    collect_until(&client, "a partial", |seen| {
        seen.iter().any(|e| matches!(e, Event::Partial { .. }))
    });

    worker.hang_up_on_the_next_request();
    let _ = client.finish();
    let seen = collect_until(&client, "the session to end", is_final);
    let ended = seen
        .iter()
        .find_map(|event| match event {
            Event::Ended {
                session,
                reason: EndReason::Failed { code, .. },
                ..
            } => Some((*session, *code)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("a lost worker did not end the dictation: {seen:?}"));
    assert_eq!(ended, (first.session, ErrorCode::WorkerUnavailable));
    assert_eq!(client.last_error(), Some(ErrorCode::WorkerUnavailable));
    assert_eq!(client.live(), None);

    // **No restart.** The same client, the same worker, a second link.
    let second = client
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("the next dictation did not reconnect");
    assert_ne!(second.session, first.session);
    assert!(second.generation > first.generation);
    collect_until(&client, "a partial on the new link", |seen| {
        seen.iter().any(|e| matches!(e, Event::Partial { .. }))
    });
    assert!(
        worker.handshakes() >= 2,
        "the client did not make a second connection"
    );
    client.cancel().expect("cancel");
    worker.stop();
}

/// **A worker that went away entirely, and a different one on the same name.**
///
/// The case is a worker that exited on its idle timeout and was started again
/// by the next `EnsureService`: a client holding a resolved address would be
/// the only thing that could not find it, and one that holds a *rule* dials the
/// same name and meets the new process. That is the whole reason the endpoint
/// is derived inside the send path rather than at construction.
#[test]
fn a_worker_that_is_replaced_is_dialled_again_rather_than_remembered() {
    let root = unbound_root();
    let first = TestWorker::start(&root);
    let client = VoiceClient::at(Some(root.clone()));
    client
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("the first begin");
    collect_until(&client, "a partial", |seen| {
        seen.iter().any(|e| matches!(e, Event::Partial { .. }))
    });
    first.hang_up_on_the_next_request();
    let _ = client.finish();
    collect_until(&client, "the session to end", is_final);
    first.stop();

    let second = TestWorker::start(&root);
    client
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("a replacement worker on the same name was not found");
    collect_until(&client, "a partial from the replacement", |seen| {
        seen.iter().any(|e| matches!(e, Event::Partial { .. }))
    });
    client.cancel().expect("cancel");
    second.stop();
}

// ------------------------------------------------------ busy, and its scope

/// **A second client of *this worker* is told busy**, with a stable code and
/// the token of the request it refused, so a host can tell the refusal of the
/// dictation it just asked for from a late word about an older one.
#[test]
fn a_second_client_of_one_worker_is_told_busy() {
    let root = unbound_root();
    let worker = TestWorker::start(&root);
    let first = VoiceClient::at(Some(root.clone()));
    let second = VoiceClient::at(Some(root.clone()));

    let live = first
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("the first begin");
    collect_until(&first, "a partial", |seen| {
        seen.iter().any(|e| matches!(e, Event::Partial { .. }))
    });
    assert!(worker.busy());

    let refused = second
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("the second begin reached the worker");
    let seen = collect_until(&second, "a refusal", |seen| !seen.is_empty());
    match seen.first() {
        Some(Event::Refused { session, code, .. }) => {
            assert_eq!(*code, ErrorCode::Busy);
            assert_eq!(*session, refused.session);
        }
        other => panic!("the second client was not told busy: {other:?}"),
    }
    assert_eq!(second.last_error(), Some(ErrorCode::Busy));
    assert_eq!(
        second.live(),
        None,
        "a refused begin left the second client holding a session"
    );
    assert_eq!(
        first.live(),
        Some(live),
        "the refusal reached the client that was dictating"
    );

    first.finish().expect("finish");
    collect_until(&first, "the first to end", is_final);
    worker.stop();
}

/// **The scope of that guarantee, asserted rather than implied.**
///
/// One dictation at a time is a promise about *one installation* and nothing
/// wider. Stable, beta, nightly and a rooted test build each derive their own
/// endpoint and therefore their own worker, and a lock spanning them would let
/// a throwaway dev build block a real session. So two installations dictate at
/// once and neither refuses the other; what happens if both capture at the same
/// moment is an operating-system conflict over the microphone, which a worker
/// reports as `capture_failed` rather than pretending to arbitrate.
///
/// This test exists so nobody later reads "one dictation at a time" as a
/// machine-wide promise the architecture does not make.
#[test]
fn two_installations_each_run_their_own_worker_and_neither_refuses_the_other() {
    let released = unbound_root();
    let rooted = unbound_root();
    let one = TestWorker::start(&released);
    let two = TestWorker::start(&rooted);
    let here = VoiceClient::at(Some(released.clone()));
    let there = VoiceClient::at(Some(rooted.clone()));

    let mine = here
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("the first begin");
    collect_until(&here, "a partial", |seen| {
        seen.iter().any(|e| matches!(e, Event::Partial { .. }))
    });
    assert!(one.busy());
    assert!(!two.busy(), "one installation armed another's worker");

    // The second installation is dictating *at the same time*, and is not
    // refused.
    let yours = there
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("the second installation could not begin");
    let seen = collect_until(&there, "a partial", |seen| {
        seen.iter().any(|e| matches!(e, Event::Partial { .. }))
    });
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, Event::Refused { .. })),
        "one installation refused another: {seen:?}"
    );
    assert_eq!(here.live(), Some(mine));
    assert_eq!(there.live(), Some(yours));
    assert!(one.busy() && two.busy());

    here.finish().expect("finish here");
    there.finish().expect("finish there");
    collect_until(&here, "the first to end", is_final);
    collect_until(&there, "the second to end", is_final);
    one.stop();
    two.stop();
}

// ------------------------------------------------------------- generations

/// Two clients of one worker must not both count from one, or the second to
/// dictate -- once the first had finished -- would be told its request was
/// stale rather than being allowed to speak.
#[test]
fn two_clients_do_not_mint_the_same_generation() {
    let first_root = unbound_root();
    let second_root = unbound_root();
    let one = TestWorker::start(&first_root);
    let two = TestWorker::start(&second_root);
    let first = VoiceClient::at(Some(first_root));
    let second = VoiceClient::at(Some(second_root));

    let mine = first
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("the first begin");
    let yours = second
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("the second begin");
    assert_ne!(mine.generation, yours.generation);
    assert_ne!(mine.session, yours.session);

    // ...and a client's own second dictation is ahead of its first, which is
    // the rule the worker actually enforces.
    first.cancel().expect("cancel");
    let again = first
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("the third begin");
    assert!(again.generation > mine.generation);

    first.cancel().expect("cancel");
    second.cancel().expect("cancel");
    one.stop();
    two.stop();
}

/// **An interruption discards**, including one this client could not tell the
/// worker about: a host that was told its cancel failed must not be left
/// holding provisional text for a session nothing will ever end.
#[test]
fn a_cancel_clears_the_session_even_when_it_cannot_be_sent() {
    let root = unbound_root();
    let worker = TestWorker::start(&root);
    let client = VoiceClient::at(Some(root.clone()));
    client
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("begin");
    worker.stop();
    let _ = client.cancel();
    assert_eq!(client.live(), None);
}

/// A client that has never been asked for anything holds no link, opens no
/// socket and starts no thread. What every host gets at construction, and what
/// makes an installation with voice switched off cost nothing.
#[test]
fn a_new_client_connects_to_nothing() {
    let client = VoiceClient::at(Some(unbound_root()));
    assert!(!client.connected());
    assert!(client.worker().is_none());
    assert!(client.drain().is_empty());
    assert_eq!(client.last_error(), None);
}

// ------------------------------------------------------------ the microphone

/// **A device list comes back, and it is the worker's answer rather than
/// anything this process enumerated.** Nothing in this crate links `cpal`; the
/// list is a fixture on the other end of a real socket.
#[test]
fn a_client_asks_the_worker_which_microphones_there_are() {
    let root = unbound_root();
    let worker = TestWorker::start(&root);
    let client = VoiceClient::at(Some(root.clone()));

    let devices = client
        .devices()
        .expect("the worker answered no device list");
    assert_eq!(devices, crate::test_worker::fixture_devices());
    assert!(
        devices.iter().filter(|d| d.default).count() == 1,
        "exactly one device is the host's own choice: {devices:?}"
    );
    worker.stop();
}

/// **Asking what the microphones are does not disturb a dictation.** The query
/// carries a session id as a correlation id and arms nothing, so the live
/// dictation is still live afterwards and its events still arrive.
#[test]
fn asking_for_devices_leaves_a_live_dictation_alone() {
    let root = unbound_root();
    let worker = TestWorker::start(&root);
    let client = VoiceClient::at(Some(root.clone()));

    let live = client
        .begin(Requested::Auto, None, InputChoice::SystemDefault)
        .expect("the client could not begin a dictation");
    client
        .devices()
        .expect("the worker answered no device list");
    assert_eq!(
        client.live(),
        Some(live),
        "a device query ended the dictation it was asked beside"
    );

    client.finish().expect("finish did not reach the worker");
    let seen = collect_until(&client, "a final and an end", is_final);
    assert!(
        seen.iter()
            .all(|event| event.generation() == live.generation),
        "the device query's answer reached the host as a dictation event: {seen:?}"
    );
    worker.stop();
}

/// **A menu may not start an inference process.** With nothing listening and an
/// ensure closure installed, `devices` still answers at once and the closure is
/// never called -- which is what separates drawing a list from asking to
/// dictate.
#[test]
fn asking_for_devices_never_starts_a_worker() {
    let asked = Arc::new(AtomicU64::new(0));
    let client = VoiceClient::at(Some(unbound_root()));
    {
        let asked = Arc::clone(&asked);
        client.install_ensure(move || {
            asked.fetch_add(1, Ordering::Release);
        });
    }

    let began = std::time::Instant::now();
    let refused = client
        .devices()
        .expect_err("there is no worker on this root");
    assert_eq!(refused.code, ErrorCode::WorkerUnavailable);
    assert_eq!(
        asked.load(Ordering::Acquire),
        0,
        "drawing a device list asked for a worker to be started"
    );
    assert!(
        began.elapsed() < std::time::Duration::from_secs(2),
        "a device query waited for a worker it had not asked for"
    );
}

/// A Settings picker is an explicit request for the live machine list. It may
/// use the same host-owned supervision route as a dictation without opening a
/// microphone or arming a session.
#[test]
fn a_device_picker_can_start_the_worker_before_it_asks() {
    let root = unbound_root();
    let asked = Arc::new(AtomicU64::new(0));
    let started: Arc<std::sync::Mutex<Option<TestWorker>>> = Arc::new(std::sync::Mutex::new(None));
    let client = VoiceClient::at(Some(root.clone()));
    {
        let asked = Arc::clone(&asked);
        let started = Arc::clone(&started);
        let root = root.clone();
        client.install_ensure(move || {
            asked.fetch_add(1, Ordering::Release);
            let mut held = started.lock().unwrap_or_else(|error| error.into_inner());
            if held.is_none() {
                *held = Some(TestWorker::start(&root));
            }
        });
    }

    let devices = client
        .devices_starting_worker()
        .expect("the picker did not reach the worker it started");
    assert_eq!(devices, crate::test_worker::fixture_devices());
    assert_eq!(asked.load(Ordering::Acquire), 1);
    assert_eq!(client.live(), None, "enumeration armed a dictation");

    if let Some(worker) = started
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        worker.stop();
    }
}

// ------------------------------------------------------------ the level meter

/// Wait until `holds` says the client has arrived, or fail.
fn wait_for(what: &str, holds: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + PATIENCE;
    while !holds() {
        assert!(
            std::time::Instant::now() < deadline,
            "waited {PATIENCE:?} for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// **A meter's level reaches the meter, not the inbox**, and it arms no
/// dictation: the host's dictation drain sees nothing, and a stop leaves by
/// the same one path.
#[test]
fn a_meter_reports_the_workers_level_and_arms_no_dictation() {
    let root = unbound_root();
    let worker = TestWorker::start(&root);
    let client = VoiceClient::at(Some(root.clone()));
    let woken = Arc::new(AtomicU64::new(0));
    {
        let woken = Arc::clone(&woken);
        client.install_wake(move || {
            woken.fetch_add(1, Ordering::Release);
        });
    }

    assert_eq!(client.meter_level(), None);
    client
        .meter(InputChoice::named("Yeti Nano"))
        .expect("the meter could not be enqueued");
    assert_eq!(
        client.meter_level(),
        Some(LevelBucket::SILENT),
        "a meter starts silent"
    );
    let heard = LevelBucket::new(crate::test_worker::METER_LEVEL);
    wait_for("the worker's level", || client.meter_level() == Some(heard));
    assert!(
        woken.load(Ordering::Acquire) > 0,
        "a level did not wake the host"
    );
    assert_eq!(client.live(), None, "a meter armed a dictation");
    assert!(
        client.drain().is_empty(),
        "a meter's level reached the inbox"
    );

    client.stop_meter().expect("the stop could not be enqueued");
    assert_eq!(client.meter_level(), None);
    wait_for("the stop", || worker.requests().contains(&"meter_stop"));
    assert_eq!(worker.requests(), vec!["meter", "meter_stop"]);
    // Stopping nothing sends nothing.
    client.stop_meter().expect("a second stop is a no-op");
    worker.stop();
}

/// **An older worker simply gives no meter.** It refuses this build's
/// greeting, as every protocol bump has it do; the meter stays silent, no
/// event reaches the host, and the code is there for a diagnostic.
#[test]
fn an_older_worker_gives_no_meter_and_no_event() {
    let root = unbound_root();
    let worker = TestWorker::start_speaking(&root, koru_voice::PROTOCOL_VERSION - 1);
    let client = VoiceClient::at(Some(root.clone()));

    client
        .meter(InputChoice::SystemDefault)
        .expect("enqueueing never waits on the worker");
    wait_for("the refusal", || {
        client.last_error() == Some(ErrorCode::ProtocolMismatch)
    });
    assert_eq!(client.meter_level(), Some(LevelBucket::SILENT));
    assert!(client.drain().is_empty(), "{:?}", client.drain());
    assert_eq!(worker.requests(), Vec::<&str>::new());
    client
        .stop_meter()
        .expect("stopping a meter that never ran");
    worker.stop();
}
