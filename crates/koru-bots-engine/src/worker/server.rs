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
    Capacity(mpsc::SyncSender<Reply>),
    WakeBot(uuid::Uuid, mpsc::SyncSender<Reply>),
    StopBot(uuid::Uuid, mpsc::SyncSender<Reply>),
    Stop,
}

pub(super) fn serve(
    bootstrap: Bootstrap,
    input: std::io::Stdin,
    policy: crate::launcher::Policy,
) -> Result<(), Error> {
    let listener = koru_ipc::Listener::bind(endpoint(&bootstrap.root, &bootstrap.segment)?)
        .map_err(|_| Error::EngineUnavailable)?;
    crate::claim_root(&bootstrap.root)?;
    let database = Database::open(&bootstrap.root)?;
    database.connection.execute("DELETE FROM workers", [])?;
    let launcher = crate::launcher::Launcher::start(
        bootstrap.root.clone(),
        bootstrap.segment.clone(),
        policy,
    )?;
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
    loop {
        for event in launcher.events() {
            match event {
                crate::launcher::Event::Registered {
                    bot,
                    incarnation,
                    at_ms,
                } if launcher.last_heard(bot, incarnation).is_some() => {
                    database.connection.execute("INSERT INTO workers(bot,incarnation,started_at_ms,last_heard_at_ms) VALUES (?1,?2,?3,?3) ON CONFLICT(bot) DO UPDATE SET incarnation=excluded.incarnation,started_at_ms=excluded.started_at_ms,last_heard_at_ms=excluded.last_heard_at_ms",rusqlite::params![bot.to_string(),incarnation.to_string(),at_ms])?;
                }
                crate::launcher::Event::Ended { bot, incarnation } => {
                    database.connection.execute(
                        "DELETE FROM workers WHERE bot=?1 AND incarnation=?2",
                        rusqlite::params![bot.to_string(), incarnation.to_string()],
                    )?;
                }
                _ => {}
            }
        }
        let command = match receiver.recv_timeout(std::time::Duration::from_millis(25)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match command {
            ActorCommand::Capacity(reply) => {
                let _ = reply.try_send(Reply::Capacity {
                    sessions: launcher.capacity(),
                });
            }
            ActorCommand::Status(reply) => {
                let response = database
                    .status()
                    .map(|mut status| {
                        for worker in &mut status.workers {
                            if let (Ok(bot), Ok(incarnation)) = (
                                uuid::Uuid::parse_str(&worker.bot),
                                uuid::Uuid::parse_str(&worker.incarnation),
                            ) {
                                if let Some(at) = launcher.last_heard(bot, incarnation) {
                                    worker.last_heard_at_ms = at;
                                }
                            }
                        }
                        Reply::Status(status)
                    })
                    .unwrap_or_else(Reply::Refused);
                let _ = reply.try_send(response);
            }
            ActorCommand::WakeBot(bot, reply) => {
                let enabled:bool=database.connection.query_row("SELECT EXISTS(SELECT 1 FROM bots WHERE id=?1 AND paused=0 AND lifecycle='active')",[bot.to_string()],|row|row.get(0))?;
                let result = if enabled {
                    launcher.ensure(bot)
                } else {
                    Err(Error::InvalidRequest)
                };
                let _ = reply.try_send(
                    result
                        .map(|()| Reply::Queued)
                        .unwrap_or_else(Reply::Refused),
                );
            }
            ActorCommand::StopBot(bot, reply) => {
                let _ = reply.try_send(
                    launcher
                        .stop(bot)
                        .map(|()| Reply::Queued)
                        .unwrap_or_else(Reply::Refused),
                );
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
            Command::Status | Command::Capacity => {
                let (reply, answer) = mpsc::sync_channel(1);
                let command = if matches!(request.command, Command::Status) {
                    ActorCommand::Status(reply)
                } else {
                    ActorCommand::Capacity(reply)
                };
                if actor.try_send(command).is_err() {
                    Reply::Refused(Error::EngineUnavailable)
                } else {
                    answer
                        .recv_timeout(TIMEOUT)
                        .unwrap_or(Reply::Refused(Error::EngineUnavailable))
                }
            }
            Command::WakeBot { bot } | Command::StopBot { bot } => {
                let (reply, answer) = mpsc::sync_channel(1);
                let command = if matches!(request.command, Command::WakeBot { .. }) {
                    ActorCommand::WakeBot(bot, reply)
                } else {
                    ActorCommand::StopBot(bot, reply)
                };
                if actor.try_send(command).is_err() {
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
