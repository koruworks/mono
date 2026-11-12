// SPDX-License-Identifier: MPL-2.0

use std::sync::mpsc;
use std::time::Duration;

use koru_agent_wire::{FRAME_VERSION, FromShell, Request};

use super::*;
use crate::event::Mailbox;

fn first_request_waiting() -> std::io::Cursor<Vec<u8>> {
    let mut bytes = Vec::new();
    koru_ipc::frame::write_frame(
        &mut bytes,
        FRAME_VERSION,
        &FromShell::Request {
            id: 1,
            body: Request::RequestFirstPartyGrant,
        },
    )
    .unwrap();
    std::io::Cursor::new(bytes)
}

/// **Window run 2's first Bot read after a restart.** The app opens a
/// connection and sends `RequestFirstPartyGrant` the moment it is welcomed,
/// so the request is often already in the pipe when the accept thread hands
/// the connection over — and the hand-over waits for the Agent's lock, which
/// the loop may hold. The loop must still see the transport's pid before the
/// request: `RequestFirstPartyGrant` reads it from `peer_pids`, which only
/// `ShellArrived` fills, and a request seen first is refused `Unverified`
/// (`routing.log`: `first-party grant connection=6 pid=None
/// refused=Some(Unverified)`, then `transport arrived connection=6
/// pid=Some(5840)`).
#[test]
fn a_request_already_waiting_is_raised_after_the_pid_it_is_judged_by() {
    let mailbox = Mailbox::new();
    let waker = mailbox.waker();
    let (raised, events) = mpsc::channel();
    mailbox.attach(move |event| {
        let _ = raised.send(event);
    });
    let (connection, reading) = connection::adopt_halves(
        ConnectionId(6),
        first_request_waiting(),
        std::io::sink(),
        waker.clone(),
    )
    .unwrap();
    welcome(connection, reading, Some(5840), &waker, &|connection| {
        // The loop holds the Agent while it works; the hand-over waits.
        std::thread::sleep(Duration::from_millis(200));
        drop(connection);
    });
    let next = || events.recv_timeout(Duration::from_secs(5)).unwrap();
    match next() {
        AgentEvent::ShellArrived { connection, pid } => {
            assert_eq!(connection, ConnectionId(6));
            assert_eq!(pid, Some(5840));
        }
        other => panic!("the request overtook the pid it is judged by: {other:?}"),
    }
    assert!(matches!(
        next(),
        AgentEvent::ShellSaid { connection: ConnectionId(6), id: 1, ref body, .. }
            if matches!(**body, Request::RequestFirstPartyGrant)
    ));
    assert!(matches!(
        next(),
        AgentEvent::ShellGone {
            connection: ConnectionId(6)
        }
    ));
}
