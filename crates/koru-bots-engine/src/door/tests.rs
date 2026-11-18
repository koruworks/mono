// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

struct Boundary {
    open: AtomicBool,
    revoked: AtomicBool,
    revoke_after_bind: AtomicBool,
    fail_journal: AtomicBool,
    trace: Mutex<Vec<String>>,
    bound: Mutex<Option<(Binding, Uuid, [u8; 32], Value)>>,
}
impl Boundary {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            open: AtomicBool::new(false),
            revoked: AtomicBool::new(false),
            revoke_after_bind: AtomicBool::new(false),
            fail_journal: AtomicBool::new(false),
            trace: Mutex::new(vec![]),
            bound: Mutex::new(None),
        })
    }
    fn authorize(&self) -> Result<(), Error> {
        if self.revoked.load(Ordering::Acquire) {
            Err(Error::CredentialRevoked)
        } else {
            Ok(())
        }
    }
    fn door(self: &Arc<Self>) -> Door {
        Door::open(Secret::parse("ab".repeat(32)).unwrap(), self.clone()).unwrap()
    }
}
impl Port for Boundary {
    fn catalog(&self, _: &Secret) -> Result<Vec<Value>, Error> {
        self.authorize()?;
        Ok(vec![
            serde_json::json!({"name":"read_document","inputSchema":{"type":"object"}}),
        ])
    }
    fn bind(&self, _: &Secret, _: Uuid, call: &Call) -> Result<Binding, Error> {
        self.authorize()?;
        if !self.open.load(Ordering::Acquire) {
            return Err(Error::NoTurn);
        }
        let binding = Binding {
            id: Uuid::new_v4(),
            turn: crate::messages::Turn {
                id: Uuid::new_v4(),
                generation: 1,
                worker: Uuid::new_v4(),
                session: Uuid::new_v4(),
            },
        };
        self.trace.lock().unwrap().push("bind".into());
        *self.bound.lock().unwrap() = Some((
            binding,
            call.door_call(),
            call.argument_digest(),
            call.arguments().clone(),
        ));
        if self.revoke_after_bind.load(Ordering::Acquire) {
            self.revoked.store(true, Ordering::Release);
        }
        Ok(binding)
    }
    fn journal(&self, _: Binding, tool: &str) -> Result<(), Error> {
        self.trace.lock().unwrap().push(tool.into());
        if self.fail_journal.load(Ordering::Acquire) {
            Err(Error::StorageUnavailable)
        } else {
            Ok(())
        }
    }
    fn execute(&self, _: &Secret, binding: Binding, call: &Call) -> Result<Value, Error> {
        self.authorize()?;
        assert_eq!(
            self.bound.lock().unwrap().as_ref(),
            Some(&(
                binding,
                call.door_call(),
                call.argument_digest(),
                call.arguments().clone()
            ))
        );
        self.trace.lock().unwrap().push("execute".into());
        Ok(serde_json::json!({"content":[]}))
    }
}

#[test]
fn discovery_works_before_a_turn_but_no_resource_call_can_run() {
    let boundary = Boundary::new();
    let door = boundary.door();
    assert_eq!(door.catalog().unwrap().len(), 1);
    assert_eq!(
        door.call(br#"{"name":"read_document","arguments":{}}"#)
            .err(),
        Some(Error::NoTurn)
    );
    assert!(boundary.trace.lock().unwrap().is_empty());
    boundary.revoked.store(true, Ordering::Release);
    assert_eq!(door.catalog().err(), Some(Error::CredentialRevoked));
}

#[test]
fn binds_the_forwarded_value_and_journals_the_actual_tool_before_execution() {
    let boundary = Boundary::new();
    boundary.open.store(true, Ordering::Release);
    let door = boundary.door();
    door.call(br#"{"name":"read_document","arguments":{"page":"synthetic","nested":{"b":2,"a":1}},"_meta":{"toolCallId":"not-qualified"}}"#).unwrap();
    assert_eq!(
        *boundary.trace.lock().unwrap(),
        vec!["bind", "read_document", "execute"]
    );
    let original = boundary.bound.lock().unwrap().as_ref().unwrap().1;
    // The provider supplied no verifiable arrival identity. Another request is
    // another arrival, never an inferred event ID or a retargeted old binding.
    door.call(br#"{"name":"read_document","arguments":{}}"#)
        .unwrap();
    assert_ne!(boundary.bound.lock().unwrap().as_ref().unwrap().1, original);
}

#[test]
fn duplicate_keys_and_unknown_tools_never_reach_binding() {
    let boundary = Boundary::new();
    boundary.open.store(true, Ordering::Release);
    let door = boundary.door();
    for raw in [
        br#"{"name":"read_document","arguments":{"a":1,"a":2}}"#.as_slice(),
        br#"{"name":"read_document","name":"other","arguments":{}}"#,
    ] {
        assert_eq!(door.call(raw).err(), Some(Error::InvalidRequest));
    }
    assert_eq!(
        door.call(br#"{"name":"other","arguments":{}}"#).err(),
        Some(Error::NotAllowed)
    );
    assert!(boundary.trace.lock().unwrap().is_empty());
}

#[test]
fn journal_failure_and_revocation_between_binding_and_execution_fail_closed() {
    for revoke in [false, true] {
        let boundary = Boundary::new();
        boundary.open.store(true, Ordering::Release);
        boundary.revoke_after_bind.store(revoke, Ordering::Release);
        boundary.fail_journal.store(!revoke, Ordering::Release);
        let door = boundary.door();
        assert_eq!(
            door.call(br#"{"name":"read_document","arguments":{}}"#)
                .err(),
            Some(if revoke {
                Error::CredentialRevoked
            } else {
                Error::StorageUnavailable
            })
        );
        assert_eq!(
            *boundary.trace.lock().unwrap(),
            vec!["bind", "read_document"]
        );
    }
}
