// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn queued_changes_and_reads_stay_ordered_when_a_view_closes() {
    let (commands, incoming) = mpsc::channel();
    let (closed, abandoned) = mpsc::channel();
    drop(abandoned);
    commands
        .send(Command {
            operation: Operation::Set(true),
            reply: closed,
        })
        .unwrap();
    let (read, reading) = mpsc::channel();
    commands
        .send(Command {
            operation: Operation::Read,
            reply: read,
        })
        .unwrap();
    let (remove, removed) = mpsc::channel();
    commands
        .send(Command {
            operation: Operation::Set(false),
            reply: remove,
        })
        .unwrap();
    drop(commands);
    let mut added = false;
    serve(incoming, |operation| {
        if let Operation::Set(on) = operation {
            added = on;
        }
        Outcome {
            added: Some(added),
            error: None,
        }
    });
    assert_eq!(reading.recv().unwrap().added, Some(true));
    assert_eq!(removed.recv().unwrap().added, Some(false));
}

#[test]
fn a_failed_write_returns_the_reading_and_error_to_the_requester() {
    let (commands, incoming) = mpsc::channel();
    let (reply, outcome) = mpsc::channel();
    commands
        .send(Command {
            operation: Operation::Set(true),
            reply,
        })
        .unwrap();
    drop(commands);
    serve(incoming, |_| Outcome {
        added: Some(false),
        error: Some("denied".into()),
    });
    let outcome = outcome.recv().unwrap();
    assert_eq!(outcome.added, Some(false));
    assert_eq!(outcome.error.as_deref(), Some("denied"));
}
