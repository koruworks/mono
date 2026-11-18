// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_bots_engine::{
    authority::Binding,
    messages::Turn,
    operations::{Key, Lease, Recovery},
};
use koru_store_core::{DocumentId, NativeEffectBinding};

fn fixture() -> (Caller, Record) {
    let call = Call::parse(Uuid::new_v4(), "create_document".into(), b"{}").unwrap();
    let binding = Binding {
        id: Uuid::new_v4(),
        turn: Turn {
            id: Uuid::new_v4(),
            generation: 1,
            worker: Uuid::new_v4(),
            session: Uuid::new_v4(),
        },
    };
    let owner = UserScopeId::new();
    let workspace = WorkspaceId::new();
    let bot = Uuid::new_v4();
    let thread = Uuid::new_v4();
    let record = Record {
        key: Key {
            scope: owner.to_storage(),
            bot,
            thread,
            operation: Uuid::new_v4(),
        },
        prepared: operations::Prepared {
            kind: NativeEffectKind::CreateDocument,
            workspace,
            document: None,
            fingerprint: [42; 32],
            expected_revision: None,
            title: "fixture".into(),
        },
        store: NativeEffectBinding {
            user_scope: owner,
            scope: format!("bots:{bot}:{thread}"),
            request_key: Uuid::new_v4(),
            workspace,
            document: DocumentId::new(),
            kind: NativeEffectKind::CreateDocument,
            request_fingerprint: [42; 32],
        },
        output: Uuid::new_v4(),
        recorded_call: binding,
        state: State::Recorded,
        lease: None,
        outcome: None,
        recovery: Recovery::default(),
    };
    (
        Caller {
            secret: Secret::parse("ab".repeat(32)).unwrap(),
            binding: binding.id,
            descriptor: call.descriptor().clone(),
        },
        record,
    )
}
#[test]
fn lost_acquire_reply_abandons_dispatch_before_c1_and_never_executes() {
    let (caller, record) = fixture();
    let boundary = Uuid::new_v4();
    let mut acquire = None;
    let mut c1 = false;
    let result = dispatch(
        boundary,
        caller,
        record.key.operation,
        record.prepared.clone(),
        |command| match command {
            Command::Record { .. } => Ok(Reply::Record(record.clone())),
            Command::Acquire { command, .. } => {
                acquire = Some(command);
                Err(Error::EngineUnavailable)
            }
            Command::ExecuteNotReceived {
                boundary: boot,
                acquire_command,
                ..
            } => {
                assert_eq!(boot, boundary);
                assert_eq!(Some(acquire_command), acquire);
                c1 = true;
                Ok(Reply::Record(record.clone()))
            }
            _ => panic!("unexpected control"),
        },
        |_| panic!("abandoned Execute dispatched"),
    );
    assert_eq!(result.err(), Some(Error::RecoveryPending));
    assert!(c1);
}
#[test]
fn pending_and_terminal_retries_never_dispatch() {
    for terminal in [false, true] {
        let (caller, record) = fixture();
        assert!(
            dispatch(
                Uuid::new_v4(),
                caller,
                record.key.operation,
                record.prepared.clone(),
                |command| match command {
                    Command::Record { .. } => Ok(Reply::Record(record.clone())),
                    Command::Acquire { .. } => Ok(Reply::Admission(if terminal {
                        Admission::Terminal(record.clone())
                    } else {
                        Admission::Pending(record.clone())
                    })),
                    _ => panic!("no reports on replay"),
                },
                |_| panic!("retry dispatched")
            )
            .is_ok()
        );
    }
}
#[test]
fn unconfirmed_dispatch_requests_recovery_instead_of_claiming_refusal() {
    let (caller, record) = fixture();
    let boundary = Uuid::new_v4();
    let mut recovering = false;
    let result = dispatch(
        boundary,
        caller,
        record.key.operation,
        record.prepared.clone(),
        |command| match command {
            Command::Record { .. } => Ok(Reply::Record(record.clone())),
            Command::Acquire { command, .. } => {
                let mut admitted = record.clone();
                admitted.state = State::Leased;
                admitted.lease = Some(Lease {
                    id: Uuid::new_v4(),
                    boundary,
                    acquire_command: command,
                    binding: record.recorded_call,
                });
                Ok(Reply::Admission(Admission::Execute(admitted)))
            }
            Command::Recover { .. } => {
                recovering = true;
                Ok(Reply::Record(record.clone()))
            }
            _ => panic!("uncertainty must not report refusal"),
        },
        |_| {
            Err(NativeWriteError::Unconfirmed(koru_mcp_server::ToolError {
                code: "native_store_unavailable".into(),
                message: String::new(),
                details: None,
            }))
        },
    );
    assert_eq!(result.err(), Some(Error::RecoveryPending));
    assert!(recovering);
}

#[test]
fn execute_dispatches_once_and_reports_exact_evidence() {
    let (caller, record) = fixture();
    let boundary = Uuid::new_v4();
    let mut reports = 0;
    let result = dispatch(
        boundary,
        caller,
        record.key.operation,
        record.prepared.clone(),
        |command| match command {
            Command::Record { .. } => Ok(Reply::Record(record.clone())),
            Command::Acquire { command, .. } => {
                let mut admitted = record.clone();
                admitted.state = State::Leased;
                admitted.lease = Some(Lease {
                    id: Uuid::new_v4(),
                    boundary,
                    acquire_command: command,
                    binding: record.recorded_call,
                });
                Ok(Reply::Admission(Admission::Execute(admitted)))
            }
            Command::Report { key, outcome, .. } => {
                reports += 1;
                assert!(key == record.key);
                let Outcome::Applied { evidence, result } = &outcome else {
                    panic!("native evidence")
                };
                assert_eq!(evidence.document, record.store.document);
                assert_eq!(result["status"], "applied");
                let mut saved = record.clone();
                saved.state = State::Applied;
                saved.outcome = Some(outcome);
                Ok(Reply::Record(saved))
            }
            _ => panic!("unexpected control"),
        },
        |admitted| {
            Ok(NativeWriteEvidence {
                kind: NativeWriteKind::Created,
                workspace: admitted.store.workspace,
                document: admitted.store.document,
                version: vec![2, 1, 2, 2, 4],
                title: "fixture".into(),
                confirmed_at_ms: 10,
                version_id: None,
            })
        },
    )
    .unwrap();
    assert_eq!(reports, 1);
    assert_eq!(result.state, State::Applied);
}
