// SPDX-License-Identifier: MPL-2.0
//! One registered IPC owner and a provider pool. Blocking discovery, context,
//! provider and tool calls run on session/door threads, never this pump.
mod client;
mod ports;
use super::*;
use crate::{
    capacity::Reservation,
    carrier::{Command, Reply as ControlReply},
    configuration::SetupFailure,
    door, sessions,
};
use koru_session_runtime::providers::{
    ManagedProvider, ManagedProviderFactory, ManagedProviderRegistry, ManagedProviderRequest,
    ProviderDiagnostic, ProviderFailure, ProviderStage, builtin_providers,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Instant,
};

struct Factory(ManagedProviderRegistry);
impl ManagedProviderFactory for Factory {
    fn create(
        &self,
        mut request: ManagedProviderRequest,
    ) -> Result<ManagedProvider, ProviderFailure> {
        let failed = || {
            ProviderFailure::new(
                koru_session_protocol::ErrorCode::ProviderFailed,
                ProviderStage::Discovery,
                ProviderDiagnostic::Io,
            )
        };
        // This creation edge runs inside the pool's session thread. The Bot
        // owns this fixed working directory; no provider argument chooses it.
        std::fs::create_dir_all(&request.working_directory).map_err(|_| failed())?;
        let registration = self.0.registration(&request.provider).ok_or_else(failed)?;
        request.executable = koru_session_profiles::resolve_executable(registration.executable)
            .ok_or_else(failed)?
            .into_os_string();
        self.0.create(request)
    }
}
struct Entry {
    reservation: Reservation,
    port: Option<Arc<ports::Port>>,
    finished: Option<(Option<SetupFailure>, bool)>,
    failure_recorded: bool,
    retry: Instant,
}
fn exchange(stream: &mut koru_ipc::Stream, command: &Command) -> Result<ControlReply, Error> {
    koru_ipc::frame::write_frame(stream, VERSION, command).map_err(|_| Error::EngineUnavailable)?;
    let (version, reply) = koru_ipc::frame::read_frame::<_, super::Reply>(stream)
        .map_err(|_| Error::EngineUnavailable)?;
    if version != VERSION {
        return Err(Error::InvalidRequest);
    }
    match reply {
        super::Reply::Control(reply) => Ok(reply),
        super::Reply::Stop => Err(Error::WorkerReplaced),
        _ => Err(Error::Unauthenticated),
    }
}
fn failure(failure: &sessions::Failure) -> SetupFailure {
    match failure {
        sessions::Failure::Provider(_) | sessions::Failure::ProviderEnded => SetupFailure::Provider,
        sessions::Failure::RetirementUnconfirmed => SetupFailure::Retirement,
        sessions::Failure::Engine(Error::InputTooSmall) => SetupFailure::Input,
        sessions::Failure::Engine(_) => SetupFailure::Context,
    }
}

pub(super) fn run(
    mut stream: koru_ipc::Stream,
    bootstrap: Bootstrap,
    database: BotDatabase,
    alive: Arc<AtomicBool>,
) -> Result<(), Error> {
    let registry = Arc::new(door::http::Registry::default());
    let server = door::http::Server::start(Arc::clone(&registry))?;
    let workspace = crate::workspace::endpoint(&bootstrap.root, &bootstrap.segment)?;
    let factory: Arc<dyn ManagedProviderFactory> = Arc::new(Factory(builtin_providers(Arc::new(
        koru_session_runtime::NoopAttachmentPathResolver,
    ))));
    let mut pool = sessions::Pool::new(bootstrap.incarnation, database)?;
    let (sender, receiver) = mpsc::sync_channel::<client::Request>(64);
    let client = client::Client(sender);
    let mut entries: BTreeMap<Uuid, Entry> = BTreeMap::new();
    let mut poll = Instant::now();
    let result = (|| {
        while alive.load(Ordering::Acquire) {
            // Every command also refreshes the registered connection's heartbeat.
            // Session/HTTP callers never own or lock the stream themselves.
            for request in receiver.try_iter().take(16) {
                let result = exchange(&mut stream, &request.command);
                let failed = result.as_ref().err().copied();
                let _ = request.reply.try_send(result);
                if let Some(error) = failed {
                    return Err(error);
                }
            }
            for finished in pool.finished() {
                if let Some(entry) = entries.get_mut(&finished.thread) {
                    entry.finished = Some((
                        finished.result.as_ref().err().map(failure),
                        finished.retired,
                    ));
                }
            }
            let now = Instant::now();
            let finishing: Vec<Uuid> = entries
                .iter()
                .filter(|(_, e)| e.finished.is_some() && now >= e.retry)
                .map(|(thread, _)| *thread)
                .collect();
            for thread in finishing {
                let entry = entries.get_mut(&thread).unwrap();
                let (code, confirmed) = entry.finished.unwrap();
                entry.retry = now + Duration::from_secs(1);
                if let Some(code) = code
                    && !entry.failure_recorded
                {
                    match exchange(
                        &mut stream,
                        &Command::SetupFailed {
                            reservation: entry.reservation,
                            code,
                        },
                    )? {
                        ControlReply::Done => entry.failure_recorded = true,
                        ControlReply::Refused(Error::StaleTurn) if confirmed => {
                            entries.remove(&thread);
                            continue;
                        }
                        _ if confirmed => continue,
                        _ => return Err(Error::EngineUnavailable),
                    }
                }
                if !confirmed {
                    return Err(Error::EngineUnavailable);
                }
                match exchange(
                    &mut stream,
                    &Command::Retired {
                        reservation: entry.reservation,
                        turn: entry.port.as_ref().and_then(|port| port.turn()),
                    },
                )? {
                    ControlReply::Done | ControlReply::Refused(Error::StaleTurn) => {
                        entries.remove(&thread);
                    }
                    _ => {}
                }
            }
            if now >= poll {
                poll = now + Duration::from_millis(200);
                let ControlReply::Work { starts, stops } = exchange(&mut stream, &Command::Poll)?
                else {
                    return Err(Error::InvalidRequest);
                };
                for stop in stops {
                    if entries
                        .get(&stop.key.thread)
                        .is_some_and(|e| e.reservation == stop)
                    {
                        pool.stop(stop.key.thread);
                    }
                }
                for reservation in starts {
                    if entries.contains_key(&reservation.key.thread) {
                        continue;
                    }
                    let configuration =
                        match exchange(&mut stream, &Command::Configuration { reservation })? {
                            ControlReply::Configuration(configuration) => configuration,
                            ControlReply::Refused(Error::InvalidRequest) => {
                                entries.insert(
                                    reservation.key.thread,
                                    Entry {
                                        reservation,
                                        port: None,
                                        finished: Some((Some(SetupFailure::Configuration), true)),
                                        failure_recorded: false,
                                        retry: now,
                                    },
                                );
                                continue;
                            }
                            ControlReply::Refused(_) => continue,
                            _ => return Err(Error::InvalidRequest),
                        };
                    if !matches!(
                        exchange(&mut stream, &Command::Started { reservation })?,
                        ControlReply::Done
                    ) {
                        continue;
                    }
                    let port = Arc::new(ports::Port::new(
                        client.clone(),
                        reservation,
                        configuration.selection.clone(),
                        workspace.clone(),
                        server.endpoint(),
                        Arc::clone(&registry),
                        pool.journal_database(),
                    ));
                    let request = ManagedProviderRequest {
                        provider: configuration.provider,
                        executable: Default::default(),
                        working_directory: bootstrap
                            .root
                            .join("bots")
                            .join(bootstrap.bot.to_string())
                            .join("workspace"),
                        delegation: None,
                        selection: Some(configuration.selection),
                        mcp_servers: vec![],
                        questions: true,
                    };
                    let started = pool.admit(sessions::Start {
                        thread: reservation.key.thread,
                        worker: bootstrap.incarnation,
                        request,
                        factory: Arc::clone(&factory),
                        port: port.clone(),
                    });
                    entries.insert(
                        reservation.key.thread,
                        Entry {
                            reservation,
                            port: Some(port),
                            finished: started.err().map(|_| (Some(SetupFailure::Provider), true)),
                            failure_recorded: false,
                            retry: now,
                        },
                    );
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    })();
    // Close provider admission first. Let session threads confirm shutdown,
    // answering their queued carrier calls with failure once the pipe ended.
    for thread in entries.keys() {
        pool.stop(*thread);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while pool.occupied() > 0 && Instant::now() < deadline {
        for request in receiver.try_iter() {
            let _ = request.reply.try_send(Err(Error::EngineUnavailable));
        }
        pool.finished();
        std::thread::sleep(Duration::from_millis(10));
    }
    // Dropping signals HTTP shutdown without making an unconfirmed process
    // retirement wait on a remote tool call. Process containment is the final
    // fence; only the supervisor's death report releases engine capacity.
    drop(server);
    match result {
        Err(Error::WorkerReplaced) => Ok(()),
        result => result,
    }
}
