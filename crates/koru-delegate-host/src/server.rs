// SPDX-License-Identifier: MPL-2.0

//! The private endpoint: one connection per call from the gateway's
//! `LocalDelegate`, answered by the actor.
//!
//! An accept thread hands each connection to a thread of its own, which
//! reads one frame, turns it into a [`Command`] with a reply channel, and
//! writes what comes back. The two verbs that are the worker's own — the
//! readiness hello and the authenticated stop — are answered here without
//! the actor, so a supervisor's probe never waits behind a run. A stream
//! holds its connection and forwards the actor's items until the run
//! ends or the caller goes.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
    time::Duration,
};

use koru_delegate_api::ipc::{Hello, Request, Response, WIRE_VERSION, WorkerCounters};
use koru_delegate_api::wire::{Failure, RunStreamItem};
use koru_ipc::{Listener, Stream, frame};

use crate::actor::Command;
use crate::scheduler::ScheduleWatchEvent;

/// How long a caller has to send its one frame, and how long an answer
/// may take: a creation fsyncs twice and a resume creates a session.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How many stream items may queue for a slow caller before the actor
/// leaves the rest for the next notification.
const STREAM_QUEUE: usize = 256;
/// Schedule invalidations are coalescible. A slow consumer is disconnected by
/// the scheduler rather than allowing an unbounded queue to grow.
const SCHEDULE_STREAM_QUEUE: usize = 16;

#[derive(Clone)]
pub struct Identity {
    pub launch_id: String,
    pub authority: String,
    pub device_id: String,
    pub run_authority: Option<koru_delegate_api::authority::RunAuthorityCredential>,
    pub run_authorities: crate::authority::AuthorityProjection,
}

pub fn spawn(
    listener: Listener,
    identity: Identity,
    commands: mpsc::Sender<Command>,
    stop: mpsc::Sender<&'static str>,
    active_runs: Arc<AtomicU32>,
    scheduler: crate::scheduler::Control,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("koru-delegate accept".into())
        .spawn(move || {
            while let Ok(stream) = listener.accept() {
                let identity = identity.clone();
                let commands = commands.clone();
                let stop = stop.clone();
                let active = Arc::clone(&active_runs);
                let scheduler = scheduler.clone();
                let spawned = std::thread::Builder::new()
                    .name("koru-delegate connection".into())
                    .stack_size(256 * 1024)
                    .spawn(move || handle(stream, identity, commands, stop, active, scheduler));
                if let Err(error) = spawned {
                    tracing::warn!(target: "koru::delegate", %error, "a connection could not be served");
                }
            }
        })
        .map(|_| ())
        .map_err(|error| format!("could not start the accept thread: {error}"))
}

/// The orchestrator's live counters, as the wire carries them.
///
/// **Read on the connection thread, deliberately.** They are atomics, not
/// state the actor owns, so the readiness answer never waits behind a run
/// — which is the whole reason a diagnostic is worth having: a worker with
/// every run busy is exactly when somebody asks what it is doing.
fn counters() -> WorkerCounters {
    let live = koru_delegate_runtime::counters::snapshot();
    WorkerCounters {
        intents_recorded: live.intents_recorded,
        attempts_opened: live.attempts_opened,
        contexts_captured: live.contexts_captured,
        captures_failed: live.captures_failed,
        publications_queued: live.publications_queued,
        artifacts_registered: live.artifacts_registered,
        publications_acknowledged: live.publications_acknowledged,
        publication_retries: live.publication_retries,
        restarts: live.restarts,
        restart_completed_forward: live.restart_completed_forward,
        restart_started: live.restart_started,
        restart_reattached: live.restart_reattached,
        restart_reconciled: live.restart_reconciled,
        restart_interrupted: live.restart_interrupted,
        restart_unresolved: live.restart_unresolved,
        schedule_wakes: live.schedule_wakes,
        schedule_evaluations: live.schedule_evaluations,
        schedule_documents_evaluated: live.schedule_documents_evaluated,
        schedule_evaluation_failures: live.schedule_evaluation_failures,
        schedule_fired: live.schedule_fired,
        schedule_skipped_missed: live.schedule_skipped_missed,
        schedule_skipped_busy: live.schedule_skipped_busy,
    }
}

fn write(stream: &Stream, response: &Response) -> bool {
    let mut writer = stream;
    frame::write_frame(&mut writer, WIRE_VERSION, response).is_ok()
}

fn handle(
    stream: Stream,
    identity: Identity,
    commands: mpsc::Sender<Command>,
    stop: mpsc::Sender<&'static str>,
    active_runs: Arc<AtomicU32>,
    scheduler: crate::scheduler::Control,
) {
    koru_ipc::set_timeouts(&stream, Some(REQUEST_TIMEOUT));
    let mut reader = &stream;
    let Ok(Some(request)) = read_current_request(&mut reader) else {
        return;
    };
    let (reply_tx, reply_rx) = mpsc::sync_channel::<Response>(1);
    let command = match request {
        Request::ResolveRunAuthority {
            authority,
            run,
            generation,
        } => {
            let response = if identity
                .run_authority
                .as_ref()
                .is_some_and(|expected| expected.matches(&authority))
            {
                match identity.run_authorities.resolve(run, generation) {
                    Ok(()) => Response::RunAuthorityConfirmed {},
                    Err(failure) => Response::Failure(failure),
                }
            } else {
                Response::Failure(Failure::new(
                    "run_authority_unavailable",
                    "The Run authority credential does not match.",
                    false,
                ))
            };
            write(&stream, &response);
            return;
        }
        Request::Hello => {
            write(
                &stream,
                &Response::Hello(Hello {
                    launch_id: identity.launch_id,
                    pid: std::process::id(),
                    device_id: identity.device_id,
                    active_runs: active_runs.load(Ordering::Acquire),
                    counters: counters(),
                }),
            );
            return;
        }
        Request::Stop { authority } => {
            let response = if authority == identity.authority {
                let _ = stop.send("stop authority");
                Response::Stopping
            } else {
                Response::Failure(Failure::new(
                    "forbidden",
                    "The stop authority does not match this worker.",
                    false,
                ))
            };
            write(&stream, &response);
            return;
        }
        Request::WakeSchedules { authority } => {
            let response = match wake_schedules(&scheduler, &identity.authority, &authority) {
                Ok(true) => Response::SchedulesWoken,
                Ok(false) => Response::Failure(Failure::new(
                    "forbidden",
                    "The worker-control authority does not match this worker.",
                    false,
                )),
                Err(error) => {
                    Response::Failure(Failure::new("schedule_evaluation_failed", error, true))
                }
            };
            write(&stream, &response);
            return;
        }
        Request::CreateSchedule { request, .. } => {
            let response = match scheduler.create(request) {
                Ok(created) => Response::ScheduleCreated(created),
                Err(failure) => Response::Failure(failure),
            };
            write(&stream, &response);
            return;
        }
        Request::RenameSchedule { request, .. } => {
            let response = match scheduler.rename(request) {
                Ok(renamed) => Response::ScheduleRenamed(renamed),
                Err(failure) => Response::Failure(failure),
            };
            write(&stream, &response);
            return;
        }
        Request::EditSchedule { request, .. } => {
            let response = match scheduler.edit(request) {
                Ok(edited) => Response::ScheduleEdited(edited),
                Err(failure) => Response::Failure(failure),
            };
            write(&stream, &response);
            return;
        }
        Request::PauseSchedule { request, .. } => {
            let response = match scheduler.pause(request) {
                Ok(paused) => Response::SchedulePaused(paused),
                Err(failure) => Response::Failure(failure),
            };
            write(&stream, &response);
            return;
        }
        Request::ResumeSchedule { request, .. } => {
            let response = match scheduler.resume(request) {
                Ok(resumed) => Response::ScheduleResumed(resumed),
                Err(failure) => Response::Failure(failure),
            };
            write(&stream, &response);
            return;
        }
        Request::RetireSchedule { request, .. } => {
            let response = match scheduler.retire(request) {
                Ok(retired) => Response::ScheduleRetired(retired),
                Err(failure) => Response::Failure(failure),
            };
            write(&stream, &response);
            return;
        }
        Request::ListSchedules { workspace, .. } => {
            let response = match scheduler.list(workspace) {
                Ok(schedules) => Response::Schedules(schedules),
                Err(failure) => Response::Failure(failure),
            };
            write(&stream, &response);
            return;
        }
        Request::WatchSchedules {
            workspace, after, ..
        } => {
            let (sink, changes) = mpsc::sync_channel(SCHEDULE_STREAM_QUEUE);
            let watcher = match scheduler.watch(workspace, after, sink) {
                Ok(watcher) => watcher,
                Err(failure) => {
                    write(&stream, &Response::Failure(failure));
                    return;
                }
            };
            if !write(&stream, &Response::ScheduleWatchReady) {
                scheduler.cancel_watch(watcher);
                return;
            }
            koru_ipc::set_timeouts(&stream, None);
            let (mut reader, mut writer) = koru_ipc::split(stream);
            let cancel_scheduler = scheduler.clone();
            let control = std::thread::Builder::new()
                .name("koru-delegate schedule control".into())
                .stack_size(96 * 1024)
                .spawn(move || {
                    let _ = read_current_request(&mut reader);
                    cancel_scheduler.cancel_watch(watcher);
                });
            let Ok(control) = control else {
                scheduler.cancel_watch(watcher);
                return;
            };
            while let Ok(item) = changes.recv() {
                match item {
                    ScheduleWatchEvent::Changed(change) => {
                        if frame::write_frame(
                            &mut writer,
                            WIRE_VERSION,
                            &Response::ScheduleChanged(change),
                        )
                        .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            let _ = frame::write_frame(&mut writer, WIRE_VERSION, &Response::ScheduleEnd);
            drop(writer);
            let _ = control.join();
            return;
        }
        Request::Create { request, .. } => Command::Create {
            request,
            reply: reply_tx,
        },
        Request::CancelScheduleWatch => return,
        Request::List { workspace, .. } => Command::List {
            workspace,
            reply: reply_tx,
        },
        Request::Run { run, window, .. } => Command::Run {
            run,
            window,
            reply: reply_tx,
        },
        Request::Cancel { run, .. } => Command::Cancel {
            run,
            reply: reply_tx,
        },
        Request::Resume { run, confirm, .. } => Command::Resume {
            run,
            confirm,
            reply: reply_tx,
        },
        Request::Rate { run, request, .. } => Command::Rate {
            run,
            request,
            reply: reply_tx,
        },
        Request::Trash { run, artifacts, .. } => Command::Trash {
            run,
            artifacts,
            reply: reply_tx,
        },
        Request::Restore { run, .. } => Command::Restore {
            run,
            reply: reply_tx,
        },
        Request::Purge { run, .. } => Command::Purge {
            run,
            reply: reply_tx,
        },
        Request::Stream {
            run,
            workspace,
            after,
            ..
        } => {
            let (sink, items) = mpsc::sync_channel::<RunStreamItem>(STREAM_QUEUE);
            if commands
                .send(Command::Stream {
                    run,
                    workspace,
                    after,
                    sink,
                })
                .is_err()
            {
                write(&stream, &Response::Failure(Failure::unavailable()));
                return;
            }
            koru_ipc::set_timeouts(&stream, None);
            while let Ok(item) = items.recv() {
                let closed = matches!(item, RunStreamItem::Closed);
                if !write(&stream, &Response::Item(item)) {
                    return;
                }
                if closed {
                    break;
                }
            }
            write(&stream, &Response::End);
            return;
        }
    };
    if commands.send(command).is_err() {
        write(&stream, &Response::Failure(Failure::unavailable()));
        return;
    }
    let response = match reply_rx.recv_timeout(REQUEST_TIMEOUT) {
        Ok(response) => response,
        Err(_) => Response::Failure(Failure::new(
            "delegate_timeout",
            "The delegate worker did not answer in time.",
            true,
        )),
    };
    write(&stream, &response);
}

fn wake_schedules(
    scheduler: &crate::scheduler::Control,
    expected: &str,
    offered: &str,
) -> Result<bool, String> {
    if offered == expected {
        scheduler.wake();
        return Ok(true);
    }
    #[cfg(feature = "test-support")]
    {
        if std::env::var("DELEGATE_PROCESS_TEST_WAKE_AUTHORITY")
            .is_ok_and(|fixture| fixture == offered)
            && std::env::var_os("DELEGATE_PROCESS_TEST_CLOCK").is_some()
        {
            scheduler.fixture_wake_and_wait()?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// Do not turn a stale frame into an actor command: an old worker can parse
/// the request while silently losing the model choice in its run header.
fn read_current_request(reader: &mut impl std::io::Read) -> std::io::Result<Option<Request>> {
    let (version, body) = frame::read_raw(reader)?;
    if version != WIRE_VERSION {
        return Ok(None);
    }
    frame::decode(&body).map(Some)
}

#[cfg(test)]
mod tests;
