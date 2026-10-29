// SPDX-License-Identifier: MPL-2.0

//! Binding, driven for real.
//!
//! These claim actual endpoints, so each one uses a scratch root of its own —
//! an endpoint is *exclusive by design*, which is the property under test, and
//! cargo runs these in parallel threads of one process.

use std::io::{Read, Write};

use super::*;
use crate::test_support::{exclusive, scratch};
use crate::{AGENT, REOPEN_CLAIM, STORE};

#[test]
fn binding_claims_the_name_and_a_second_bind_is_refused() {
    // The exclusion that replaces a lock file. One mechanism, and it is the
    // same one that answers "is it running".
    let endpoint = scratch(STORE, "exclusive-bind");
    let held = Listener::bind(endpoint.clone()).expect("first bind");

    let e = Listener::bind(endpoint.clone()).expect_err("a second bind must fail");
    // Its own code, not a generic failure. A second bind means something is
    // *already there*, which is what a client wanted — so a process that
    // auto-started stands down quietly on this one and complains on every other
    // bind failure. Telling them apart is the whole reason the variant exists.
    assert_eq!(e.code(), "ipc_already_bound");

    drop(held);
}

#[test]
fn a_bound_endpoint_answers_a_probe_and_a_dropped_one_does_not() {
    // The whole point. There is no file to go stale, no lock to check, and no
    // window in which a client can be told an address that does not work.
    let endpoint = scratch(STORE, "probe-lifecycle");
    assert!(!crate::probe(&endpoint));
    {
        let _held = Listener::bind(endpoint.clone()).expect("bind");
        assert!(crate::probe(&endpoint));
    }
    assert!(!crate::probe(&endpoint));
}

#[test]
fn two_services_on_one_root_do_not_collide() {
    // A gate runs an agent and a store on the same temp root. Holding one must
    // say nothing about the other — if it did, the collision would happen in
    // the suite and nowhere else, which is the worst place for it.
    let root_label = "two-services";
    let store = scratch(STORE, root_label);
    let agent = scratch(AGENT, root_label);
    let _one = Listener::bind(store.clone()).expect("store binds");
    let _two = Listener::bind(agent.clone()).expect("agent binds beside it");
    assert!(crate::probe(&store));
    assert!(crate::probe(&agent));
}

#[test]
fn two_reopen_claims_on_one_root_can_be_held_together() {
    let root =
        std::env::temp_dir().join(format!("koru-ipc-two-reopen-claims-{}", std::process::id()));
    let first = Endpoint::for_service_claim(
        REOPEN_CLAIM,
        koru_channel::segment(),
        &root,
        "5ee8ffdf-00c8-47ba-b294-054820454a0d",
    )
    .unwrap();
    let second = Endpoint::for_service_claim(
        REOPEN_CLAIM,
        koru_channel::segment(),
        &root,
        "f9781e80-f257-42f5-8e70-df6eb1c1bf37",
    )
    .unwrap();
    let _first = Listener::bind(first).expect("first claim binds");
    let _second = Listener::bind(second).expect("second claim binds beside it");
}

#[test]
fn a_message_crosses_the_endpoint_intact() {
    // Framing over a real transport rather than over a `Cursor`: a named pipe
    // buffers, and a write that is never flushed is a deadlock that looks
    // exactly like a hung peer.
    #[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct Ping {
        say: String,
    }

    let endpoint = scratch(STORE, "round-trip");
    let listener = Listener::bind(endpoint.clone()).expect("bind");
    let serving = std::thread::spawn(move || {
        let mut stream = listener.accept().expect("accept");
        let (version, ping): (u8, Ping) = crate::frame::read_frame(&mut stream).unwrap();
        crate::frame::write_frame(
            &mut stream,
            version,
            &Ping {
                say: format!("{} back", ping.say),
            },
        )
        .unwrap();
    });

    let mut stream = crate::connect(&endpoint).expect("connect");
    crate::frame::write_frame(
        &mut stream,
        7,
        &Ping {
            say: "hello".to_string(),
        },
    )
    .unwrap();
    let (version, reply): (u8, Ping) = crate::frame::read_frame(&mut stream).unwrap();
    assert_eq!(version, 7);
    assert_eq!(reply.say, "hello back");
    serving.join().unwrap();
}

#[test]
fn a_server_learns_the_peers_pid_from_the_transport() {
    // **Never from the message.** A pid a peer sends is a claim; this is the
    // operating system's answer, and it is what a routing table may key on.
    let endpoint = scratch(STORE, "peer-pid");
    let listener = Listener::bind(endpoint.clone()).expect("bind");
    let serving = std::thread::spawn(move || {
        let stream = listener.accept().expect("accept");
        // Read one byte so the client's write cannot race the accept.
        let mut byte = [0u8; 1];
        (&stream).read_exact(&mut byte).unwrap();
        (crate::peer_pid(&stream), crate::same_user(&stream))
    });

    let mut stream = crate::connect(&endpoint).expect("connect");
    stream.write_all(b"x").unwrap();
    stream.flush().unwrap();

    let (pid, same) = serving.join().unwrap();
    assert!(same, "a connection from this process is this user's");
    // Both platforms answer today — Unix through `SO_PEERCRED`, Windows through
    // `GetNamedPipeClientProcessId` — and the client here *is* this process.
    // `None` is a legal answer on a platform that will not say, so the
    // assertion is on the value when there is one rather than on there being
    // one.
    if let Some(pid) = pid {
        assert_eq!(pid, std::process::id());
    }
}

#[cfg(unix)]
#[test]
fn a_socket_file_a_crash_left_behind_is_taken_over() {
    // Windows pipes vanish with the process; Unix sockets do not, and a `bind`
    // on the leftover file gets `AddrInUse` from something with nothing behind
    // it. The recovery is a sequence, not a lock: bind, probe, overwrite.
    //
    // The stale file is made with std's `UnixListener`, which unlike
    // `interprocess`'s does not unlink on drop: dropping it closes the fd and
    // leaves the file — exactly what a kill -9 leaves behind. This used to
    // `mem::forget` a live listener instead, which is *not* that: the leaked
    // fd stays open in this process and the kernel keeps completing
    // connections into its backlog, so the probe truthfully answered "alive"
    // and correctly refused — the test was simulating the wrong crash.
    let endpoint = scratch(STORE, "stale-socket");
    prepare_parent(&endpoint).expect("parent dir");
    // A file an earlier *run* left behind is not this test's subject.
    let _ = std::fs::remove_file(endpoint.name());
    let stale = std::os::unix::net::UnixListener::bind(endpoint.name()).expect("a stale file");
    drop(stale);
    assert!(std::path::Path::new(endpoint.name()).exists());

    // Nothing is accepting on it now, so this must succeed rather than
    // reporting the name as held.
    let _second = Listener::bind(endpoint.clone()).expect("take over the stale endpoint");
}

#[test]
fn a_channel_endpoint_is_bindable_too() {
    // The scratch-root endpoints above are what keeps these tests independent;
    // this one proves the ordinary, un-rooted path binds at all. It uses a
    // channel this build is *not*, and takes the lock, so a service a developer
    // is running is never displaced. An installed Koru holds its own channel,
    // so the first spare one that is free is used rather than a fixed one.
    let _guard = exclusive();
    let (endpoint, held) = koru_channel::SEGMENTS
        .iter()
        .filter(|segment| **segment != koru_channel::segment())
        .find_map(|segment| {
            let endpoint = Endpoint::for_service(AGENT, segment).unwrap();
            match Listener::bind(endpoint.clone()) {
                Ok(held) => Some((endpoint, held)),
                Err(IpcError::AlreadyBound(_)) => None,
                Err(other) => panic!("bind a bare channel endpoint: {other:?}"),
            }
        })
        .expect("a spare channel that nothing holds");
    assert!(crate::probe(&endpoint));
    drop(held);
}
