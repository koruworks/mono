// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::{BotBootAdmission, BotId, BotOwner, BotRequestId};
use std::{sync::Mutex, time::Duration};

fn subject() -> (BotOwner, BotId) {
    (
        serde_json::from_value(
            serde_json::json!({"user_scope":Uuid::new_v4(),"device":Uuid::new_v4()}),
        )
        .unwrap(),
        BotId::mint(),
    )
}
fn present(owner: BotOwner, bot: BotId) -> BotBootAdmission {
    BotBootAdmission::Present {
        barrier: BotAdmissionBarrier {
            owner,
            bot,
            request: BotRequestId::mint(),
            epoch: i64::MAX as u64,
            paused: true,
        },
    }
}
struct Port {
    answer: Result<BotReply, crate::Failure>,
    calls: Mutex<Vec<(BotCommand, Duration)>>,
    delay: Duration,
}
impl Port {
    fn new(answer: BotReply) -> Self {
        Self {
            answer: Ok(answer),
            calls: Mutex::new(vec![]),
            delay: Duration::ZERO,
        }
    }
}
impl BotDelegatePort for Port {
    fn execute(&self, _: BotCommand) -> Result<BotReply, crate::Failure> {
        panic!("bounded path required")
    }
    fn execute_within(
        &self,
        command: BotCommand,
        within: Duration,
    ) -> Result<BotReply, crate::Failure> {
        self.calls.lock().unwrap().push((command, within));
        if !self.delay.is_zero() {
            // Consume the supplied remaining budget only after known entry.
            std::thread::sleep(within.saturating_add(self.delay));
        }
        self.answer.clone()
    }
}

#[test]
fn admission_read_wire_is_closed_correlated_and_keeps_ipc16() {
    let (owner, bot) = subject();
    let command = BotCommand::ReadAdmissionBarrier { owner, bot };
    assert_eq!(command.owner(), Some(&owner));
    assert_eq!(crate::ipc::WIRE_VERSION, 19);
    let frame = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &command).unwrap();
    assert_eq!(
        koru_ipc::frame::read_frame::<_, BotCommand>(&mut frame.as_slice())
            .unwrap()
            .1,
        command
    );
    for path in [vec!["force"], vec!["owner", "force"]] {
        let mut value = serde_json::to_value(&command).unwrap();
        if path.len() == 1 {
            value["force"] = true.into();
        } else {
            value["owner"]["force"] = true.into();
        }
        assert!(serde_json::from_value::<BotCommand>(value).is_err());
    }
    let reply = BotReply::AdmissionBarrier {
        owner,
        bot,
        admission: present(owner, bot),
    };
    for path in [
        vec!["force"],
        vec!["owner", "force"],
        vec!["admission", "force"],
        vec!["admission", "barrier", "force"],
        vec!["admission", "barrier", "owner", "force"],
    ] {
        let mut value = serde_json::to_value(&reply).unwrap();
        let mut nested = &mut value;
        for key in &path[..path.len() - 1] {
            nested = &mut nested[*key];
        }
        nested[path[path.len() - 1]] = true.into();
        assert!(serde_json::from_value::<BotReply>(value).is_err());
    }
    assert_eq!(
        serde_json::from_value::<BotReply>(serde_json::to_value(&reply).unwrap()).unwrap(),
        reply
    );
}

#[test]
fn admission_read_helper_requires_exact_nested_subject_and_positive_epoch() {
    let (owner, bot) = subject();
    for admission in [BotBootAdmission::Missing {}, present(owner, bot)] {
        let port = Port::new(BotReply::AdmissionBarrier {
            owner,
            bot,
            admission: admission.clone(),
        });
        assert_eq!(
            port.read_admission_barrier(owner, bot, Duration::from_secs(1))
                .unwrap(),
            admission
        );
        let calls = port.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, BotCommand::ReadAdmissionBarrier { owner, bot });
        assert!(calls[0].1 > Duration::ZERO && calls[0].1 <= Duration::from_secs(1));
    }
    let mut bad = vec![
        BotReply::Missing,
        BotReply::Applied,
        BotReply::AdmissionBarrier {
            owner,
            bot,
            admission: BotBootAdmission::Unavailable {},
        },
        BotReply::AdmissionBarrier {
            owner: subject().0,
            bot,
            admission: BotBootAdmission::Missing {},
        },
        BotReply::AdmissionBarrier {
            owner,
            bot: BotId::mint(),
            admission: BotBootAdmission::Missing {},
        },
    ];
    for change in 0..5 {
        let BotBootAdmission::Present { mut barrier } = present(owner, bot) else {
            unreachable!()
        };
        match change {
            0 => barrier.owner = subject().0,
            1 => barrier.bot = BotId::mint(),
            2 => barrier.epoch = 0,
            3 => barrier.epoch = i64::MAX as u64 + 1,
            _ => barrier.request = BotRequestId::from_uuid(Uuid::nil()),
        }
        bad.push(BotReply::AdmissionBarrier {
            owner,
            bot,
            admission: BotBootAdmission::Present { barrier },
        });
    }
    for answer in bad {
        assert!(
            Port::new(answer)
                .read_admission_barrier(owner, bot, Duration::from_secs(1))
                .is_err()
        );
    }
}

#[test]
fn admission_read_helper_refuses_zero_invalid_and_late_completion_without_retries() {
    let (owner, bot) = subject();
    let port = Port::new(BotReply::AdmissionBarrier {
        owner,
        bot,
        admission: BotBootAdmission::Missing {},
    });
    assert!(
        port.read_admission_barrier(owner, bot, Duration::ZERO)
            .is_err()
    );
    assert!(
        port.read_admission_barrier(owner, BotId::from_uuid(Uuid::nil()), Duration::from_secs(1))
            .is_err()
    );
    let mut invalid = owner;
    invalid.device = koru_delegate_core::DeviceId::from_uuid(Uuid::nil());
    assert!(
        port.read_admission_barrier(invalid, bot, Duration::from_secs(1))
            .is_err()
    );
    assert!(port.calls.lock().unwrap().is_empty());
    let mut port = port;
    port.delay = Duration::from_millis(1);
    assert!(
        port.read_admission_barrier(owner, bot, Duration::from_millis(100))
            .is_err()
    );
    assert_eq!(port.calls.lock().unwrap().len(), 1);
}

#[test]
fn admission_read_unknown_transport_and_old_peer_never_become_missing() {
    let (owner, bot) = subject();
    let old = koru_ipc::frame::encode(15, &serde_json::json!({"old":true})).unwrap();
    for bytes in [old, vec![], vec![0, 1, 2, 3]] {
        let failure = crate::local::read_current_response(&mut bytes.as_slice()).unwrap_err();
        let port = Port {
            answer: Err(failure.clone()),
            calls: Mutex::new(vec![]),
            delay: Duration::ZERO,
        };
        assert_eq!(
            port.read_admission_barrier(owner, bot, Duration::from_secs(1))
                .unwrap_err(),
            failure
        );
        assert_eq!(port.calls.lock().unwrap().len(), 1);
    }
    assert!(LocalBotDelegate::answer(crate::ipc::Response::Bot(BotReply::Missing)).is_ok());
    // The generic transport preserves the variant; only the typed helper may
    // establish correlated absence, and it rejects generic Missing above.
}
