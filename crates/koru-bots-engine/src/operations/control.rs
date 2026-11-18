// SPDX-License-Identifier: MPL-2.0
//! Private gateway-to-engine commands. This channel's authority never goes
//! to a provider or Bot worker. All resource reads still resolve a live call.
use super::*;
use crate::{
    Database,
    authority::{Descriptor, Secret},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Caller {
    pub secret: Secret,
    pub binding: Uuid,
    pub descriptor: Descriptor,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum Command {
    Record {
        boundary: Uuid,
        command: Uuid,
        caller: Caller,
        operation: Uuid,
        prepared: Prepared,
    },
    Acquire {
        boundary: Uuid,
        command: Uuid,
        caller: Caller,
        key: Key,
        fingerprint: [u8; 32],
    },
    Report {
        boundary: Uuid,
        command: Uuid,
        key: Key,
        lease: Lease,
        outcome: Outcome,
    },
    ExecuteNotReceived {
        boundary: Uuid,
        command: Uuid,
        key: Key,
        acquire_command: Uuid,
    },
    Get {
        caller: Caller,
        key: Key,
    },
    Recover {
        boundary: Uuid,
        key: Key,
        lease: Lease,
    },
}
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OperationCommand(<redacted>)")
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub enum Reply {
    Record(Record),
    Admission(Admission),
    Refused(Error),
}
impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OperationReply(<redacted>)")
    }
}
pub fn handle(db: &mut Database, command: Command, now: i64) -> Reply {
    let result = match command {
        Command::Record {
            boundary,
            command,
            caller,
            operation,
            prepared,
        } => db
            .record_operation(
                boundary,
                command,
                &caller.secret,
                caller.binding,
                &caller.descriptor,
                operation,
                prepared,
                now,
            )
            .map(Reply::Record),
        Command::Acquire {
            boundary,
            command,
            caller,
            key,
            fingerprint,
        } => db
            .acquire_operation(
                boundary,
                command,
                &caller.secret,
                caller.binding,
                &caller.descriptor,
                &key,
                fingerprint,
                now,
            )
            .map(Reply::Admission),
        Command::Report {
            boundary,
            command,
            key,
            lease,
            outcome,
        } => db
            .report_operation(boundary, command, &key, &lease, outcome, now)
            .map(Reply::Record),
        Command::ExecuteNotReceived {
            boundary,
            command,
            key,
            acquire_command,
        } => db
            .execute_not_received(boundary, command, &key, acquire_command, now)
            .map(Reply::Record),
        Command::Get { caller, key } => db
            .get_operation(&caller.secret, caller.binding, &caller.descriptor, &key)
            .map(Reply::Record),
        Command::Recover {
            boundary,
            key,
            lease,
        } => db
            .request_operation_recovery(boundary, &key, &lease)
            .map(Reply::Record),
    };
    result.unwrap_or_else(Reply::Refused)
}
