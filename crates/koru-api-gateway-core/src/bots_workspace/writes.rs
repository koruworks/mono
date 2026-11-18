// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_bots_engine::operations::{
    self, Admission, Outcome, Record, State,
    control::{Caller, Command, Reply},
};
use koru_delegate_core::{NativeResultKind, NativeResultReference};
use koru_mcp_server::native::{
    NativeStoreIdentity, NativeWriteDescription, NativeWriteError, NativeWriteEvidence,
    NativeWriteKind,
};
use koru_store_core::NativeEffectKind;

impl Engine {
    pub(super) fn operation(&self, command: Command) -> Result<Reply, Error> {
        match worker::request(
            &self.endpoint,
            self.authority.expose(),
            worker::Command::Operation(command),
        )? {
            worker::Reply::Operation(Reply::Refused(error)) | worker::Reply::Refused(error) => {
                Err(error)
            }
            worker::Reply::Operation(reply) => Ok(reply),
            _ => Err(Error::EngineUnavailable),
        }
    }
}

pub(super) fn prepared(
    description: &NativeWriteDescription,
) -> Result<operations::Prepared, Error> {
    let hash = description.fingerprint.as_bytes();
    if hash.len() != 64 {
        return Err(Error::InvalidRequest);
    }
    let mut fingerprint = [0; 32];
    for (byte, pair) in fingerprint.iter_mut().zip(hash.chunks_exact(2)) {
        let hex = std::str::from_utf8(pair).map_err(|_| Error::InvalidRequest)?;
        *byte = u8::from_str_radix(hex, 16).map_err(|_| Error::InvalidRequest)?;
    }
    Ok(operations::Prepared {
        kind: match description.kind {
            NativeWriteKind::Created => NativeEffectKind::CreateDocument,
            NativeWriteKind::Edited => NativeEffectKind::EditDocument,
        },
        workspace: description.workspace,
        document: description.document,
        fingerprint,
        expected_revision: description.expected_revision.clone(),
        title: description.title.clone(),
    })
}
pub(super) fn identity(record: &Record) -> NativeStoreIdentity {
    NativeStoreIdentity {
        candidate: record.store.document,
        scope: record.store.scope.clone(),
        request_key: record.store.request_key.to_string(),
    }
}
pub(super) fn outcome(record: &Record, evidence: NativeWriteEvidence) -> Outcome {
    let reference = NativeResultReference {
        operation_id: record.key.operation,
        workspace: evidence.workspace,
        document: evidence.document,
        version: evidence.version,
        title: evidence.title,
        confirmed_at_ms: evidence.confirmed_at_ms,
        kind: match evidence.kind {
            NativeWriteKind::Created => NativeResultKind::CreatedPage,
            NativeWriteKind::Edited => NativeResultKind::EditedPage,
        },
        version_id: evidence.version_id,
    };
    let result = serde_json::json!({"status":"applied","operation_id":record.key.operation,"reference":reference});
    Outcome::Applied {
        evidence: reference,
        result,
    }
}

impl Boundary {
    pub(super) fn write(&self, request: workspace::Request) -> Result<Value, Error> {
        let call = request.call()?;
        let resolved = self
            .engine
            .resolve(&request.secret, request.binding, &call)?;
        let authority = read_authority(resolved)?;
        let backend = self.backend(authority)?;
        let mut arguments = call.arguments().clone();
        let operation = arguments
            .as_object_mut()
            .and_then(|a| a.remove("operation_id"))
            .and_then(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()))
            .ok_or(Error::InvalidRequest)?;
        let native = backend
            .prepare_native_write(call.tool(), arguments)
            .map_err(native_error)?;
        let prepared = prepared(native.description())?;
        if prepared.workspace != authority.workspace {
            return Err(Error::NotAllowed);
        }
        let caller = Caller {
            secret: request.secret,
            binding: request.binding,
            descriptor: call.descriptor().clone(),
        };
        let record = dispatch(
            self.incarnation,
            caller.clone(),
            operation,
            prepared,
            |command| self.engine.operation(command),
            |record| backend.execute_native_write(&native, identity(record)),
        )?;
        // Returning retained page metadata is a fresh resource use even after
        // a successful write. Reports themselves did not confer that authority.
        let Reply::Record(current) = self.engine.operation(Command::Get {
            caller,
            key: record.key.clone(),
        })?
        else {
            return Err(Error::EngineUnavailable);
        };
        if matches!(current.state, State::Applied) {
            koru_ai_server::native_document_title(
                self.store.clone(),
                self.store_authority(authority)?,
                current.store.document,
            )
            .map_err(native_error)?;
        }
        result(current)
    }
}

fn result(record: Record) -> Result<Value, Error> {
    match record.outcome {
        Some(Outcome::Applied { result, .. }) => Ok(result),
        Some(Outcome::Refused { stable_code }) => Ok(
            serde_json::json!({"status":"refused","operation_id":record.key.operation,"phase":"before_dispatch","code":stable_code}),
        ),
        None => Ok(
            serde_json::json!({"status":if record.state==State::Unresolved {"unresolved"} else {"pending"},"operation_id":record.key.operation}),
        ),
    }
}

/// One synchronous request owns the dispatch path. An IPC failure drops its
/// reply reader before C1 is sent; no callback/future remains able to consume
/// a delayed Execute. There is exactly one call of the native effect closure.
fn dispatch(
    boundary: Uuid,
    caller: Caller,
    operation: Uuid,
    prepared: operations::Prepared,
    mut control: impl FnMut(Command) -> Result<Reply, Error>,
    apply: impl FnOnce(&Record) -> Result<NativeWriteEvidence, NativeWriteError>,
) -> Result<Record, Error> {
    let Reply::Record(record) = control(Command::Record {
        boundary,
        command: Uuid::new_v4(),
        caller: caller.clone(),
        operation,
        prepared: prepared.clone(),
    })?
    else {
        return Err(Error::EngineUnavailable);
    };
    let acquire_command = Uuid::new_v4();
    let admission = control(Command::Acquire {
        boundary,
        command: acquire_command,
        caller,
        key: record.key.clone(),
        fingerprint: prepared.fingerprint,
    });
    let admitted = match admission {
        Ok(Reply::Admission(Admission::Execute(record))) => record,
        Ok(Reply::Admission(Admission::Pending(record) | Admission::Terminal(record))) => {
            return Ok(record);
        }
        Err(Error::EngineUnavailable) => {
            // The synchronous reader has returned and been dropped. From here
            // the dispatch closure is unreachable, even if Execute was sent.
            let _ = control(Command::ExecuteNotReceived {
                boundary,
                command: Uuid::new_v4(),
                key: record.key,
                acquire_command,
            });
            return Err(Error::RecoveryPending);
        }
        Err(error) => return Err(error),
        _ => return Err(Error::EngineUnavailable),
    };
    let lease = admitted.lease.as_ref().ok_or(Error::LeaseMismatch)?.clone();
    // Recheck the exact semantic request against the consumed lease. An IPC
    // adapter bug must not dispatch different content under a valid lease.
    if admitted.key != record.key
        || !admitted.prepared.same(&prepared)
        || lease.boundary != boundary
        || lease.acquire_command != acquire_command
    {
        return Err(Error::OperationConflict);
    }
    let report = match apply(&admitted) {
        Ok(evidence) => outcome(&admitted, evidence),
        Err(NativeWriteError::Refused(error)) => Outcome::Refused {
            stable_code: error.code,
        },
        Err(NativeWriteError::Unconfirmed(_)) => {
            let _ = control(Command::Recover {
                boundary,
                key: admitted.key.clone(),
                lease,
            });
            return Err(Error::RecoveryPending);
        }
    };
    match control(Command::Report {
        boundary,
        command: Uuid::new_v4(),
        key: admitted.key.clone(),
        lease,
        outcome: report,
    }) {
        Ok(Reply::Record(record)) => Ok(record),
        Err(error) => Err(error),
        _ => Err(Error::EngineUnavailable),
    }
}

#[cfg(test)]
mod tests;
