// SPDX-License-Identifier: MPL-2.0

//! Linux desktop-entry I/O, serialized off the event loop. Startup repair,
//! Settings reads and explicit changes share a queue, so a late repair cannot
//! recreate an entry the person just removed. Views poll only their replies.

use std::sync::{OnceLock, mpsc};

#[derive(Clone, Copy, Debug)]
enum Operation {
    Read,
    Set(bool),
    Repair,
}

#[derive(Debug)]
pub(crate) struct Outcome {
    pub(crate) added: Option<bool>,
    pub(crate) error: Option<String>,
}

#[derive(Debug)]
struct Command {
    operation: Operation,
    reply: mpsc::Sender<Outcome>,
}

pub(crate) fn read() -> mpsc::Receiver<Outcome> {
    request(Operation::Read)
}

pub(crate) fn set(on: bool) -> mpsc::Receiver<Outcome> {
    request(Operation::Set(on))
}

pub(crate) fn repair() {
    // The worker logs any failure; startup never waits for the disk.
    drop(request(Operation::Repair));
}

fn request(operation: Operation) -> mpsc::Receiver<Outcome> {
    let (reply, receiver) = mpsc::channel();
    // A binary's tests must not touch the person's desktop integration.
    if cfg!(test) || !koru_autostart::app_menu::SUPPORTED {
        let _ = reply.send(Outcome {
            added: None,
            error: None,
        });
        return receiver;
    }
    static WORKER: OnceLock<Option<mpsc::Sender<Command>>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (sender, commands) = mpsc::channel();
        match std::thread::Builder::new()
            .name("koru-app-menu".into())
            .spawn(move || serve(commands, execute))
        {
            Ok(_) => Some(sender),
            Err(error) => {
                tracing::warn!(target: "koru::app_menu", %error, "could not start the app menu worker");
                None
            }
        }
    });
    let command = Command {
        operation,
        reply: reply.clone(),
    };
    if worker
        .as_ref()
        .is_none_or(|sender| sender.send(command).is_err())
    {
        let _ = reply.send(Outcome {
            added: None,
            error: Some("app menu worker unavailable".into()),
        });
    }
    receiver
}

fn serve(commands: mpsc::Receiver<Command>, mut apply: impl FnMut(Operation) -> Outcome) {
    for command in commands {
        let outcome = apply(command.operation);
        // Closing a tab/window cancels its interest, not an accepted write.
        let _ = command.reply.send(outcome);
    }
}

fn execute(operation: Operation) -> Outcome {
    use koru_autostart::app_menu;
    let result = match operation {
        Operation::Read => Ok(()),
        Operation::Set(true) => std::env::current_exe().and_then(|exe| app_menu::add(&exe)),
        Operation::Set(false) => app_menu::remove(),
        Operation::Repair => std::env::current_exe()
            .and_then(|exe| app_menu::repair(&exe))
            .map(|_| ()),
    };
    let error = result.err().map(|error| {
        tracing::warn!(target: "koru::app_menu", %error, "the app menu operation failed");
        error.to_string()
    });
    Outcome {
        added: Some(app_menu::is_added()),
        error,
    }
}

#[cfg(test)]
mod tests;
