// SPDX-License-Identifier: MPL-2.0

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

use super::*;
use crate::Database;
mod context;
mod recovery;

enum ActorCommand {
    Hello(mpsc::SyncSender<Reply>),
    Status(mpsc::SyncSender<Reply>),
    Capacity(mpsc::SyncSender<Reply>),
    Person(crate::person::Request, mpsc::SyncSender<Reply>),
    Creation(crate::creation::Request, mpsc::SyncSender<Reply>),
    Operation(crate::operations::control::Command, mpsc::SyncSender<Reply>),
    SessionCatalog(crate::authority::Secret, mpsc::SyncSender<Reply>),
    ResolveCall(
        crate::authority::Secret,
        Uuid,
        crate::authority::Descriptor,
        mpsc::SyncSender<Reply>,
    ),
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
    let mut database = Database::open(&bootstrap.root)?;
    database.recover_interrupted_turns()?;
    database.connection.execute("DELETE FROM workers", [])?;
    let (worker_requests, worker_commands) = mpsc::sync_channel::<crate::carrier::Request>(64);
    let launcher = crate::launcher::Launcher::start(
        bootstrap.root.clone(),
        bootstrap.segment.clone(),
        policy,
        worker_requests,
    )?;
    let (sender, receiver) = mpsc::sync_channel(64);
    let launch_id = bootstrap.launch_id;
    let mut contexts = context::Jobs::new(
        crate::workspace::endpoint(&bootstrap.root, &bootstrap.segment)?,
        crate::authority::Secret::parse(bootstrap.authority.clone())?,
    );
    let mut recovery = recovery::Jobs::new(
        crate::workspace::endpoint(&bootstrap.root, &bootstrap.segment)?,
        crate::authority::Secret::parse(bootstrap.authority.clone())?,
    );
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
    let mut pending = std::collections::VecDeque::new();
    let mut observations = crate::observations::LiveEvents::default();
    let mut answers = crate::questions::Transient::default();
    let mut admission = crate::capacity::Admission::new(launcher.capacity());
    let mut dispatch = true;
    let mut retry_dispatch = None;
    loop {
        dispatch |= recovery.poll(&mut database, std::time::Instant::now());
        contexts.poll(&admission);
        pending.extend(launcher.events());
        while let Some(event) = pending.pop_front() {
            let current = match &event {
                crate::launcher::Event::Registered {
                    bot, incarnation, ..
                } => launcher.last_heard(*bot, *incarnation).is_some(),
                _ => true,
            };
            if !current {
                continue;
            }
            if current && record_worker_event(&mut database, &event).is_err() {
                // Do not discard a death report or take down unrelated workers.
                // Keep it pending; commands continue to receive stable errors
                // while storage is unavailable. Failed registration cannot mint
                // authority because its worker row does not exist.
                if let crate::launcher::Event::Registered { bot, .. } = &event {
                    let _ = launcher.stop(*bot);
                }
                pending.push_front(event);
                break;
            }
            match event {
                crate::launcher::Event::Allocated { bot, incarnation }
                | crate::launcher::Event::Registered {
                    bot, incarnation, ..
                } => {
                    admission.bind_worker(bot, incarnation);
                    launcher.occupied(bot, incarnation, admission.occupied_by(bot, incarnation));
                }
                crate::launcher::Event::Ended { incarnation, .. } => {
                    admission.worker_ended(incarnation);
                    answers.prune(&database);
                }
            }
            dispatch = true;
        }
        for request in worker_commands.try_iter().take(16) {
            let who = request.worker;
            let context_assignment = match &request.command {
                crate::carrier::Command::Context { reservation } => Some(*reservation),
                _ => None,
            };
            dispatch |= !matches!(
                request.command,
                crate::carrier::Command::Poll
                    | crate::carrier::Command::Context { .. }
                    | crate::carrier::Command::Configuration { .. }
                    | crate::carrier::Command::Head { .. }
                    | crate::carrier::Command::Catalog { .. }
                    | crate::carrier::Command::Bind { .. }
                    | crate::carrier::Command::OwnTool { .. }
                    | crate::carrier::Command::Event { .. }
                    | crate::carrier::Command::OfferSteer { .. }
                    | crate::carrier::Command::Answers { .. }
            );
            let response = crate::carrier::handle(
                &mut database,
                &mut admission,
                &mut answers,
                who,
                request.command,
                now_ms(),
            );
            let response = match response {
                crate::carrier::Reply::Observed(event) => observations
                    .push(event)
                    .map(|()| crate::carrier::Reply::Done)
                    .unwrap_or_else(crate::carrier::Reply::Refused),
                crate::carrier::Reply::LoadContext(sources) => contexts.request(
                    who,
                    context_assignment.expect("only Context prepares sources"),
                    sources,
                ),
                response => response,
            };
            launcher.occupied(
                who.bot,
                who.incarnation,
                admission.occupied_by(who.bot, who.incarnation),
            );
            let _ = request.reply.try_send(response);
        }
        dispatch |= retry_dispatch.is_some_and(|deadline| std::time::Instant::now() >= deadline);
        if dispatch {
            // A committed Stop remains discoverable even if the first read
            // after its commit fails. Never free a physical slot here.
            admission.set_capacity(launcher.capacity());
            retry_dispatch = match admission
                .refresh_stops(&database)
                .and_then(|()| dispatch_work(&database, &mut admission, &launcher, now_ms()))
            {
                Ok(()) => None,
                Err(_) => Some(std::time::Instant::now() + std::time::Duration::from_secs(1)),
            };
            dispatch = false;
        }
        let command = match receiver.recv_timeout(std::time::Duration::from_millis(25)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match command {
            ActorCommand::Creation(request, reply) => {
                let result = crate::creation::handle(&mut database, request, now_ms());
                let _ = reply.try_send(result.map(Reply::Creation).unwrap_or_else(Reply::Refused));
            }
            ActorCommand::Person(request, reply) => {
                let changes = request.command.changes();
                let result = crate::person::handle(
                    &mut database,
                    &observations,
                    &mut answers,
                    request,
                    now_ms(),
                );
                answers.prune(&database);
                let _ = reply.try_send(result.map(Reply::Person).unwrap_or_else(Reply::Refused));
                if changes {
                    dispatch = true;
                    recovery.changed();
                    // Bounded maintenance on normal work, never timer writes.
                    let _ = database.prune_receipt_answers(now_ms());
                }
            }
            ActorCommand::Operation(command, reply) => {
                let result = crate::operations::control::handle(&mut database, command, now_ms());
                let _ = reply.try_send(Reply::Operation(result));
                dispatch = true;
                recovery.changed();
            }
            ActorCommand::SessionCatalog(secret, reply) => {
                let result = database
                    .boundary_catalog_grant(&secret)
                    .map(|grant| Reply::Catalog(CatalogGrant(grant)))
                    .unwrap_or_else(Reply::Refused);
                let _ = reply.try_send(result);
            }
            ActorCommand::ResolveCall(secret, binding, descriptor, reply) => {
                let result = database
                    .resolve_descriptor(&secret, binding, &descriptor)
                    .map(Reply::Resolved)
                    .unwrap_or_else(Reply::Refused);
                let _ = reply.try_send(result);
            }
            ActorCommand::Hello(reply) => {
                let _ = reply.try_send(Reply::Hello { launch_id });
            }
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
                let _ = reply.try_send(wake_bot(&database, bot, || launcher.ensure(bot)));
                dispatch = true;
            }
            ActorCommand::StopBot(bot, reply) => {
                let _ = reply.try_send(stop_bot(&mut database, launcher.incarnation(bot), || {
                    launcher.stop(bot)
                }));
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
            Command::Creation(request) => {
                let (reply, answer) = mpsc::sync_channel(1);
                if actor
                    .try_send(ActorCommand::Creation(request, reply))
                    .is_err()
                {
                    Reply::Refused(Error::EngineUnavailable)
                } else {
                    answer
                        .recv_timeout(TIMEOUT)
                        .unwrap_or(Reply::Refused(Error::EngineUnavailable))
                }
            }
            Command::Person(request) => {
                let (reply, answer) = mpsc::sync_channel(1);
                if actor
                    .try_send(ActorCommand::Person(request, reply))
                    .is_err()
                {
                    Reply::Refused(Error::EngineUnavailable)
                } else {
                    answer
                        .recv_timeout(TIMEOUT)
                        .unwrap_or(Reply::Refused(Error::EngineUnavailable))
                }
            }
            Command::Operation(command) => {
                let (reply, answer) = mpsc::sync_channel(1);
                if actor
                    .try_send(ActorCommand::Operation(command, reply))
                    .is_err()
                {
                    Reply::Refused(Error::EngineUnavailable)
                } else {
                    answer
                        .recv_timeout(TIMEOUT)
                        .unwrap_or(Reply::Refused(Error::EngineUnavailable))
                }
            }
            Command::SessionCatalog { secret } => {
                let (reply, answer) = mpsc::sync_channel(1);
                if actor
                    .try_send(ActorCommand::SessionCatalog(secret, reply))
                    .is_err()
                {
                    Reply::Refused(Error::EngineUnavailable)
                } else {
                    answer
                        .recv_timeout(TIMEOUT)
                        .unwrap_or(Reply::Refused(Error::EngineUnavailable))
                }
            }
            Command::ResolveCall {
                secret,
                binding,
                descriptor,
            } => {
                let (reply, answer) = mpsc::sync_channel(1);
                if actor
                    .try_send(ActorCommand::ResolveCall(
                        secret, binding, descriptor, reply,
                    ))
                    .is_err()
                {
                    Reply::Refused(Error::EngineUnavailable)
                } else {
                    answer
                        .recv_timeout(TIMEOUT)
                        .unwrap_or(Reply::Refused(Error::EngineUnavailable))
                }
            }
            Command::Hello | Command::Status | Command::Capacity => {
                let (reply, answer) = mpsc::sync_channel(1);
                let command = match request.command {
                    Command::Hello => ActorCommand::Hello(reply),
                    Command::Status => ActorCommand::Status(reply),
                    _ => ActorCommand::Capacity(reply),
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

fn wake_bot(database: &Database, bot: Uuid, ensure: impl FnOnce() -> Result<(), Error>) -> Reply {
    let result = (|| {
        let enabled: bool = database.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM bots b LEFT JOIN team t ON t.owner=b.owner WHERE b.id=?1 AND b.paused=0 AND b.lifecycle='active' AND COALESCE(t.paused,0)=0)",
            [bot.to_string()], |row| row.get(0),
        )?;
        if !enabled {
            return Err(Error::InvalidRequest);
        }
        ensure()
    })();
    result
        .map(|()| Reply::Queued)
        .unwrap_or_else(Reply::Refused)
}

fn stop_bot(
    database: &mut Database,
    incarnation: Option<Uuid>,
    stop: impl FnOnce() -> Result<(), Error>,
) -> Reply {
    // Commit revocation before asking the supervisor to end the process. A
    // slow shutdown must not leave the old provider able to start new effects.
    let result = (|| {
        if let Some(worker) = incarnation {
            database.interrupt_worker(worker)?;
        }
        stop()
    })();
    result
        .map(|()| Reply::Queued)
        .unwrap_or_else(Reply::Refused)
}

fn record_worker_event(
    database: &mut Database,
    event: &crate::launcher::Event,
) -> Result<(), Error> {
    match event {
        crate::launcher::Event::Allocated { .. } => {}
        crate::launcher::Event::Registered {
            bot,
            incarnation,
            at_ms,
        } => {
            database.connection.execute("INSERT INTO workers(bot,incarnation,started_at_ms,last_heard_at_ms) VALUES (?1,?2,?3,?3) ON CONFLICT(bot) DO UPDATE SET incarnation=excluded.incarnation,started_at_ms=excluded.started_at_ms,last_heard_at_ms=excluded.last_heard_at_ms",rusqlite::params![bot.to_string(),incarnation.to_string(),at_ms])?;
        }
        crate::launcher::Event::Ended { bot, incarnation } => {
            database.interrupt_worker(*incarnation)?;
            database.connection.execute(
                "DELETE FROM workers WHERE bot=?1 AND incarnation=?2",
                rusqlite::params![bot.to_string(), incarnation.to_string()],
            )?;
        }
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn dispatch_work(
    database: &Database,
    admission: &mut crate::capacity::Admission,
    launcher: &crate::launcher::Launcher,
    now: i64,
) -> Result<(), Error> {
    while let Some(decision) = admission.next(database, now.max(0) as u64)? {
        match decision {
            crate::capacity::Decision::Start { reservation, .. } => {
                let bot = reservation.key.bot;
                if let Some(worker) = launcher.incarnation(bot) {
                    admission.bind_worker(bot, worker);
                    launcher.occupied(bot, worker, admission.occupied_by(bot, worker));
                } else if let Err(error) = launcher.ensure(bot) {
                    admission.withdraw(reservation)?;
                    return Err(error);
                }
            }
            // The next worker Poll observes the fenced stop assignment. The
            // slot remains occupied until its provider retirement is confirmed.
            crate::capacity::Decision::End(_) => {}
        }
    }
    Ok(())
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
