// SPDX-License-Identifier: MPL-2.0

//! **The endpoint, and one conversation per connection.**
//!
//! The worker binds `koru_ipc::READ`, qualified by the Store root it was given
//! (`Endpoint::for_service_root`), so the Store that launched it with `--root`
//! dials the same name and two rooted stacks never meet. Binding is also the
//! single-instance rule: a second worker on the same root finds the name taken
//! and stands down.
//!
//! A conversation is frames of `koru_read::Request` in and `koru_read::Event`
//! out, at `koru_read::FRAME_VERSION`. **The first request must be `Status`**,
//! the handshake that lets the Store check the launch id and the protocol
//! before it sends a file; anything else first closes the connection without
//! an answer. Events are written by a thread of the connection's own, so a
//! job thread never blocks on a full pipe.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use koru_ipc::{Endpoint, Listener};
use koru_read::{Event, FRAME_VERSION, Request};

use crate::service::{ConnectionId, Service};

/// The endpoint this worker serves for `root`.
pub fn endpoint(root: &Path) -> Result<Endpoint, koru_ipc::IpcError> {
    Endpoint::for_service_root(koru_ipc::READ, koru_channel::segment(), root)
}

/// Claim the endpoint. `AlreadyBound` means another worker serves this root.
pub fn bind(root: &Path) -> Result<Listener, koru_ipc::IpcError> {
    Listener::bind(endpoint(root)?)
}

/// Accept connections until the process ends. Runs on its own thread.
pub fn serve(listener: Listener, service: Service) {
    // The service and the channel, never the endpoint's name: on Unix that is
    // a filesystem path, and on Windows it carries the account's SID.
    tracing::info!(
        service = listener.endpoint().service(),
        channel = listener.endpoint().segment(),
        "the read worker is listening"
    );
    let next = AtomicU64::new(1);
    loop {
        let stream = match listener.accept() {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(%error, "a read connection could not be accepted");
                continue;
            }
        };
        // **Same user, or nothing.** The endpoint's security already says so;
        // this is the second half, because what crosses it is people's words.
        if !koru_ipc::same_user(&stream) {
            tracing::warn!("a read connection from another user was refused");
            continue;
        }
        let id = next.fetch_add(1, Ordering::Relaxed);
        let service = service.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("koru-read-conn-{id}"))
            .spawn(move || {
                let (reader, writer) = koru_ipc::split(stream);
                converse(id, reader, writer, &service);
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "a read connection thread could not be started");
        }
    }
}

/// One connection, from its first frame to its end. Generic over the two
/// halves so it is tested over bytes in memory.
pub fn converse<R: Read, W: Write + Send + 'static>(
    connection: ConnectionId,
    mut reader: R,
    mut writer: W,
    service: &Service,
) {
    let (sink, outbound) = std::sync::mpsc::channel::<Event>();
    let writing = std::thread::Builder::new()
        .name(format!("koru-read-out-{connection}"))
        .spawn(move || {
            while let Ok(event) = outbound.recv() {
                if koru_ipc::frame::write_frame(&mut writer, FRAME_VERSION, &event).is_err() {
                    break;
                }
            }
        });
    let Ok(writing) = writing else {
        tracing::warn!("a read writer thread could not be started");
        return;
    };
    let mut first = true;
    // A read error is the ordinary end: the Store closed the connection.
    while let Ok((version, body)) = koru_ipc::frame::read_raw(&mut reader) {
        if version != FRAME_VERSION {
            tracing::warn!(
                theirs = version,
                ours = FRAME_VERSION,
                "a read frame arrived at a version this worker cannot read"
            );
            break;
        }
        let Ok(request) = koru_ipc::frame::decode::<Request>(&body) else {
            tracing::warn!("a read request could not be understood");
            break;
        };
        if first && request != Request::Status {
            tracing::warn!("a read connection did not open with a status question");
            break;
        }
        first = false;
        service.handle(connection, request, &sink);
    }
    service.connection_closed(connection);
    // The writer drains what is queued and ends when the last sender — this
    // one, and a job's clone — is gone.
    drop(sink);
    let _ = writing.join();
}

#[cfg(test)]
mod tests;
