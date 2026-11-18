// SPDX-License-Identifier: MPL-2.0

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

use super::*;
use crate::Database;

enum ActorCommand {
    Status(mpsc::SyncSender<Reply>),
    Stop,
}

pub(super) fn serve(bootstrap: Bootstrap, input: std::io::Stdin) -> Result<(), Error> {
    let listener = koru_ipc::Listener::bind(endpoint(&bootstrap.root, &bootstrap.segment)?)
        .map_err(|_| Error::EngineUnavailable)?;
    crate::claim_root(&bootstrap.root)?;
    let database = Database::open(&bootstrap.root)?;
    let (sender, receiver) = mpsc::sync_channel(64);
    let parent = sender.clone();
    koru_agent_worker::ParentPipe::new(input)
        .watch("koru-bots parent", move || {
            let _ = parent.send(ActorCommand::Stop);
        })
        .map_err(|_| Error::EngineUnavailable)?;
    let active = Arc::new(AtomicUsize::new(0));
    std::thread::Builder::new()
        .name("koru-bots accept".into())
        .spawn(move || {
            while let Ok(stream) = listener.accept() {
                if !koru_ipc::same_user(&stream) {
                    continue;
                }
                if active.fetch_add(1, Ordering::AcqRel) >= 16 {
                    active.fetch_sub(1, Ordering::AcqRel);
                    continue;
                }
                let permit = Permit(Arc::clone(&active));
                let sender = sender.clone();
                let identity = bootstrap.clone();
                let _ = std::thread::Builder::new()
                    .name("koru-bots connection".into())
                    .spawn(move || {
                        let _permit = permit;
                        handle(stream, &identity, sender);
                    });
            }
        })
        .map_err(|_| Error::EngineUnavailable)?;
    while let Ok(command) = receiver.recv() {
        match command {
            ActorCommand::Status(reply) => {
                let response = database
                    .status()
                    .map(Reply::Status)
                    .unwrap_or_else(Reply::Refused);
                let _ = reply.try_send(response);
            }
            ActorCommand::Stop => break,
        }
    }
    Ok(())
}

struct Permit(Arc<AtomicUsize>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn handle(
    mut stream: koru_ipc::Stream,
    bootstrap: &Bootstrap,
    actor: mpsc::SyncSender<ActorCommand>,
) {
    koru_ipc::set_timeouts(&stream, Some(TIMEOUT));
    let Ok((version, request)) = koru_ipc::frame::read_frame::<_, Request>(&mut stream) else {
        return;
    };
    if version != WIRE_VERSION {
        return;
    }
    let response = if !matches_authority(&request.authority, &bootstrap.authority) {
        Reply::Refused(Error::Unauthenticated)
    } else {
        match request.command {
            Command::Hello => Reply::Hello {
                launch_id: bootstrap.launch_id,
            },
            Command::Status => {
                let (reply, answer) = mpsc::sync_channel(1);
                if actor.try_send(ActorCommand::Status(reply)).is_err() {
                    Reply::Refused(Error::EngineUnavailable)
                } else {
                    answer
                        .recv_timeout(TIMEOUT)
                        .unwrap_or(Reply::Refused(Error::EngineUnavailable))
                }
            }
            Command::Stop => {
                if actor.try_send(ActorCommand::Stop).is_ok() {
                    Reply::Stopping
                } else {
                    Reply::Refused(Error::EngineUnavailable)
                }
            }
        }
    };
    let _ = koru_ipc::frame::write_frame(&mut stream, WIRE_VERSION, &response);
}

fn matches_authority(supplied: &str, expected: &str) -> bool {
    supplied.len() == expected.len()
        && supplied
            .bytes()
            .zip(expected.bytes())
            .fold(0, |difference, (a, b)| difference | (a ^ b))
            == 0
}

#[cfg(test)]
mod tests;
