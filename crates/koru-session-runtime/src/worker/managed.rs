// SPDX-License-Identifier: MPL-2.0

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use koru_events::TerminalSessionOutcome;
use koru_session_protocol::{AgentTurnId, ApprovalDecision, ApprovalId, ErrorCode, ProcessState};

use crate::providers::{AgentControl, AgentInput, ProviderFailure, ProviderWake};

use super::control::wait_for_start;
use super::{Shared, failed};

const COMMAND_CAPACITY: usize = 32;

enum ManagedCommand {
    Submit {
        input: AgentInput,
        reply: SyncSender<Result<(), ProviderFailure>>,
    },
    Cancel {
        turn_id: AgentTurnId,
        reply: SyncSender<Result<(), ProviderFailure>>,
    },
    Steer {
        input: AgentInput,
        reply: SyncSender<Result<(), ProviderFailure>>,
    },
    Approval {
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        choice: Option<koru_session_protocol::ApprovalChoiceId>,
        reply: SyncSender<Result<(), ProviderFailure>>,
    },
}

struct HandleInner {
    sender: SyncSender<ManagedCommand>,
    wake: Arc<dyn ProviderWake>,
    stopping: AtomicBool,
}

#[derive(Clone)]
pub(super) struct ManagedHandle {
    inner: Arc<HandleInner>,
}

pub(super) struct ManagedReceiver {
    receiver: Receiver<ManagedCommand>,
    inner: Arc<HandleInner>,
}

pub(super) fn command_queue(wake: Arc<dyn ProviderWake>) -> (ManagedHandle, ManagedReceiver) {
    let (sender, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let inner = Arc::new(HandleInner {
        sender,
        wake,
        stopping: AtomicBool::new(false),
    });
    (
        ManagedHandle {
            inner: Arc::clone(&inner),
        },
        ManagedReceiver { receiver, inner },
    )
}

impl ManagedHandle {
    pub(super) fn submit(&self, input: AgentInput) -> Result<(), ErrorCode> {
        let (reply, response) = mpsc::sync_channel(1);
        self.send(ManagedCommand::Submit { input, reply })?;
        receive(response)
    }

    pub(super) fn steer(&self, input: AgentInput) -> Result<(), ErrorCode> {
        let (reply, response) = mpsc::sync_channel(1);
        self.send(ManagedCommand::Steer { input, reply })?;
        receive(response)
    }

    pub(super) fn cancel(&self, turn_id: AgentTurnId) -> Result<(), ErrorCode> {
        let (reply, response) = mpsc::sync_channel(1);
        self.send(ManagedCommand::Cancel { turn_id, reply })?;
        receive(response)
    }

    pub(super) fn answer_approval(
        &self,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        choice: Option<koru_session_protocol::ApprovalChoiceId>,
    ) -> Result<(), ErrorCode> {
        let (reply, response) = mpsc::sync_channel(1);
        self.send(ManagedCommand::Approval {
            approval_id,
            decision,
            choice,
            reply,
        })?;
        receive(response)
    }

    pub(super) fn terminate(&self) {
        self.inner.stopping.store(true, Ordering::Release);
        self.inner.wake.wake();
    }

    fn send(&self, command: ManagedCommand) -> Result<(), ErrorCode> {
        if self.inner.stopping.load(Ordering::Acquire) {
            return Err(ErrorCode::SessionExited);
        }
        match self.inner.sender.try_send(command) {
            Ok(()) => {
                self.inner.wake.wake();
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(ErrorCode::Backpressure),
            Err(TrySendError::Disconnected(_)) => Err(ErrorCode::ProviderUnavailable),
        }
    }
}

fn receive(response: Receiver<Result<(), ProviderFailure>>) -> Result<(), ErrorCode> {
    response
        .recv()
        .map_err(|_| ErrorCode::ProviderUnavailable)?
        .map_err(|failure| failure.code)
}

pub(super) fn spawn_worker(
    mut control: Box<dyn AgentControl>,
    receiver: ManagedReceiver,
    shared: Arc<Shared>,
    start_gate: Arc<(Mutex<bool>, Condvar)>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("koru-managed-session-{}", shared.id))
        .stack_size(512 * 1024)
        .spawn(move || {
            wait_for_start(&start_gate);
            managed_loop(&mut *control, receiver, shared);
        })
}

fn managed_loop(control: &mut dyn AgentControl, receiver: ManagedReceiver, shared: Arc<Shared>) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        loop {
            if receiver.inner.stopping.load(Ordering::Acquire) {
                let result = control.shutdown();
                shared.finish(
                    match result {
                        Ok(()) => ProcessState::Exited { code: None },
                        Err(failure) => {
                            let code = failure.code;
                            failed(code.clone(), code.as_str())
                        }
                    },
                    TerminalSessionOutcome::Terminated,
                );
                return;
            }

            loop {
                match receiver.receiver.try_recv() {
                    Ok(command) => handle_command(control, command),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        receiver.inner.stopping.store(true, Ordering::Release);
                        break;
                    }
                }
            }
            if receiver.inner.stopping.load(Ordering::Acquire) {
                continue;
            }

            match control.next_event(Duration::MAX) {
                Ok(Some(event)) => shared.apply_agent_event(event),
                Ok(None) => {}
                Err(failure) => {
                    let code = failure.code;
                    let _ = control.shutdown();
                    shared.finish(
                        failed(code.clone(), code.as_str()),
                        TerminalSessionOutcome::Failed,
                    );
                    return;
                }
            }
        }
    }));
    if result.is_err() {
        let _ = control.shutdown();
        shared.finish(
            failed(ErrorCode::ProviderFailed, "provider_failed"),
            TerminalSessionOutcome::Failed,
        );
    }
}

fn handle_command(control: &mut dyn AgentControl, command: ManagedCommand) {
    match command {
        ManagedCommand::Submit { input, reply } => {
            let _ = reply.send(control.submit(input));
        }
        ManagedCommand::Cancel { turn_id, reply } => {
            let _ = reply.send(control.cancel(turn_id));
        }
        ManagedCommand::Steer { input, reply } => {
            let _ = reply.send(control.steer(input));
        }
        ManagedCommand::Approval {
            approval_id,
            decision,
            choice,
            reply,
        } => {
            let _ = reply.send(control.answer_approval(approval_id, decision, choice));
        }
    }
}
