// SPDX-License-Identifier: MPL-2.0

//! One shell's socket: a reader thread, and a writer with a queue.
//!
//! ## The event loop performs no blocking IPC, in either direction
//!
//! This is the load-bearing rule of the whole agent, and the reason a
//! connection is two threads rather than a socket the loop reads when it feels
//! like it.
//!
//! **A named pipe blocks when the peer stops reading.** So a shell that hangs —
//! or is being debugged, or is paging back in after the machine slept — would
//! otherwise freeze the tray, the hotkey, the overlay and every *other* shell's
//! routing, from inside a `write_all` the loop is sitting in. The failure is
//! total and it looks like the agent has crashed.
//!
//! So:
//!
//! - the **reader** parks on `read_exact` forever and raises what arrives. No
//!   timeout: an idle shell is idle on purpose, and a deadline there would turn
//!   quiet into a disconnection;
//! - the **writer** owns a queue. The loop calls [`Connection::send`], which
//!   pushes and returns. Nothing it does can wait on a peer.
//!
//! `interprocess`'s `Stream::split` gives a `RecvHalf` and a `SendHalf` that are
//! both `Send`, which is what makes this possible at all.
//!
//! ## EOF is the only disconnect signal
//!
//! No heartbeat and no liveness poll. The reader thread ending — clean close,
//! crash, or kill — raises [`AgentEvent::ShellGone`], which is the same argument
//! the rendezvous makes about liveness: you cannot read from a socket nobody is
//! holding. A failed *write* raises it too, because a socket that will not take
//! a byte is one nobody is holding either, and the routing decision has to be
//! made again rather than the result being lost into it.

use std::io::Write;
use std::sync::mpsc::{Receiver, Sender, channel};

use koru_agent_wire::{FRAME_VERSION, FromAgent, FromShell};
use koru_ipc::Stream;

use crate::event::{AgentEvent, Waker};
use crate::registry::ConnectionId;

/// A live shell connection, from the loop's side.
///
/// Holding one is what keeps the writer thread alive; dropping it closes the
/// queue, the writer ends, and the socket goes with it.
pub struct Connection {
    id: ConnectionId,
    outbound: Sender<FromAgent>,
}

impl Connection {
    pub fn id(&self) -> ConnectionId {
        self.id
    }

    /// Queue a message. **Returns immediately, always.**
    ///
    /// `false` means the writer has gone, which is a disconnection the loop
    /// should treat exactly as an EOF — see the module note on why a failed
    /// write is not a lost message but a stale entry.
    pub fn send(&self, message: FromAgent) -> bool {
        self.outbound.send(message).is_ok()
    }

    /// A handle a worker can answer through.
    ///
    /// The loop cannot wait for a slow job — ensuring the store takes seconds —
    /// so the job is given the queue directly and replies when it is done. The
    /// connection may be gone by then, and a send into a closed queue is simply
    /// dropped: the shell that asked has stopped listening.
    pub fn sender(&self) -> Sender<FromAgent> {
        self.outbound.clone()
    }
}

/// A connection over an in-memory channel, for asserting what the loop queues.
/// `adopt` needs a real socket; a test needs to read the frames back.
#[cfg(test)]
impl Connection {
    pub fn for_test(id: ConnectionId) -> (Self, Receiver<FromAgent>) {
        let (outbound, queue) = channel();
        (Connection { id, outbound }, queue)
    }
}

/// Take over an accepted socket: split it, start the writer, and hand back
/// the reading half for [`Reading::start`].
///
/// The handshake has already happened on the caller's thread — see
/// [`crate::rendezvous`] — because a peer that will not be talked to should
/// never have had two threads spawned for it.
pub fn adopt(
    id: ConnectionId,
    stream: Stream,
    waker: Waker,
) -> std::io::Result<(Connection, Reading<impl std::io::Read + Send + 'static>)> {
    // `Stream::split` hands back halves that are both `Send`, which is what
    // makes a reader thread and a writer thread over one socket possible.
    let (recv, send) = koru_ipc::split(stream);
    adopt_halves(id, recv, send, waker)
}

/// [`adopt`] over any two halves, so the order of what a connection raises
/// can be asserted without a socket.
pub(crate) fn adopt_halves<R, W>(
    id: ConnectionId,
    recv: R,
    send: W,
    waker: Waker,
) -> std::io::Result<(Connection, Reading<R>)>
where
    R: std::io::Read + Send + 'static,
    W: Write + Send + 'static,
{
    let (outbound, queue) = channel::<FromAgent>();
    std::thread::Builder::new()
        .name(format!("koru-agent shell {} writer", id.0))
        .spawn(move || write_until_closed(send, queue))?;
    Ok((Connection { id, outbound }, Reading { id, recv, waker }))
}

/// A connection's reading half, not read yet.
///
/// **Nothing the peer sends is raised until [`Reading::start`].** The first
/// request on a connection is judged by facts the accept thread raises about
/// it (`ShellArrived` carries the pid the operating system reported, and a
/// first-party grant is minted only to that pid), so the reader must not exist
/// until they are queued: a request already waiting in the pipe would
/// otherwise overtake them and be judged with none.
pub struct Reading<R> {
    id: ConnectionId,
    recv: R,
    waker: Waker,
}

impl<R: std::io::Read + Send + 'static> Reading<R> {
    /// Start the reader thread.
    pub fn start(self) -> std::io::Result<()> {
        let Self { id, recv, waker } = self;
        std::thread::Builder::new()
            .name(format!("koru-agent shell {} reader", id.0))
            .spawn(move || read_until_eof(id, recv, waker))
            .map(drop)
    }
}

/// Read frames until the socket ends, raising each one.
///
/// **The version is checked on every frame, not only the first.** A peer that
/// changed its framing mid-stream is not something that happens by accident,
/// and reading its bodies anyway would be parsing a format we have just
/// established we do not know.
fn read_until_eof(id: ConnectionId, recv: impl std::io::Read, waker: Waker) {
    let mut recv = recv;
    loop {
        let (version, body) = match koru_ipc::frame::read_raw(&mut recv) {
            Ok(frame) => frame,
            // Every ending arrives here: a clean close, a crash, a kill, and a
            // frame that was not ours. They are the same to the registry.
            Err(_) => break,
        };
        if crate::handshake::framing(version) != crate::handshake::Framing::Understood {
            break;
        }
        let Ok(message) = koru_ipc::frame::decode::<FromShell>(&body) else {
            // A body this agent cannot read, in a framing it can. Not fatal to
            // the connection: the next frame may be fine, and dropping a shell
            // over one malformed message would be a worse outcome than ignoring
            // it.
            koru_ipc::ipc_debug!("shell {}: a message that would not decode", id.0);
            continue;
        };
        let FromShell::Request { id: request, body } = message;
        waker.wake(AgentEvent::ShellSaid {
            connection: id,
            id: request,
            body: Box::new(body),
            read_at: std::time::Instant::now(),
        });
    }
    waker.wake(AgentEvent::ShellGone { connection: id });
}

/// Write queued messages until the queue closes or the socket refuses.
///
/// Ending quietly on a write failure is deliberate: the *reader* is what
/// reports the disconnection, and both threads raising `ShellGone` would make
/// the registry's removal happen twice — which is harmless, because `gone` is
/// idempotent, but the log would show two disconnections where there was one.
fn write_until_closed(send: impl Write, queue: Receiver<FromAgent>) {
    let mut send = send;
    while let Ok(message) = queue.recv() {
        if koru_ipc::frame::write_frame(&mut send, FRAME_VERSION, &message).is_err() {
            break;
        }
    }
}
