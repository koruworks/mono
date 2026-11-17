// SPDX-License-Identifier: MPL-2.0
//! Delegate IPC 20: the team pause. Cohorts `m8_ipc_team_codec` and
//! `m8_ipc_team_frame_version`.
use super::*;
use koru_delegate_core::{BotOwner, BotRequestId, BotTeamBarrier, DeviceId};

fn owner() -> BotOwner {
    BotOwner {
        user_scope: serde_json::from_value(serde_json::json!(Uuid::new_v4())).unwrap(),
        device: DeviceId::from_uuid(Uuid::new_v4()),
    }
}

/// Never paused, paused at epoch 1, resumed at epoch 2.
fn values() -> [BotTeamBarrier; 3] {
    let owner = owner();
    [
        BotTeamBarrier::never_paused(owner),
        BotTeamBarrier {
            owner,
            epoch: 1,
            paused: true,
            request: Some(BotRequestId::mint()),
        },
        BotTeamBarrier {
            owner,
            epoch: 2,
            paused: false,
            request: Some(BotRequestId::mint()),
        },
    ]
}

#[test]
fn m8_ipc_team_codec_command_and_reply_round_trip_bind_the_owner_and_refuse_unknown_fields() {
    assert_eq!(crate::ipc::WIRE_VERSION, 20);
    for barrier in values() {
        assert!(barrier.valid());
        let command = BotCommand::TeamBarrier {
            barrier: barrier.clone(),
        };
        assert_eq!(command.owner(), Some(&barrier.owner));
        // It is none of the bounded cohorts of earlier versions.
        assert_eq!(command.child_valid(), None);
        assert_eq!(command.private_valid(), None);
        assert_eq!(command.routine_control_valid(), None);
        let value = serde_json::to_value(&command).unwrap();
        assert_eq!(value["command"], "team_barrier");
        assert_eq!(value["barrier"]["epoch"], barrier.epoch);
        assert_eq!(value["barrier"]["paused"], barrier.paused);
        let bytes = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &command).unwrap();
        let (version, decoded): (u8, BotCommand) =
            koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(version, crate::ipc::WIRE_VERSION);
        assert!(decoded == command);
        assert_eq!(format!("{command:?}"), "BotCommand([REDACTED])");

        let reply = BotReply::TeamBarrier {
            barrier: barrier.clone(),
        };
        let value = serde_json::to_value(&reply).unwrap();
        assert_eq!(value["result"], "team_barrier");
        let response = crate::ipc::Response::Bot(reply.clone());
        let bytes = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &response).unwrap();
        let read = crate::local::read_current_response(&mut bytes.as_slice()).unwrap();
        assert!(matches!(read, crate::ipc::Response::Bot(decoded) if decoded == reply));

        // Unknown fields are refused on the command, the reply and the value.
        for (mut extra, nested) in [
            (serde_json::to_value(&command).unwrap(), false),
            (serde_json::to_value(&command).unwrap(), true),
        ] {
            if nested {
                extra["barrier"]["extra"] = serde_json::json!(true);
            } else {
                extra["extra"] = serde_json::json!(true);
            }
            assert!(serde_json::from_value::<BotCommand>(extra).is_err());
        }
        let mut extra = serde_json::to_value(&reply).unwrap();
        extra["barrier"]["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<BotReply>(extra).is_err());
    }
}

#[test]
fn m8_ipc_team_codec_the_value_is_never_paused_at_epoch_zero_and_names_its_request_after() {
    let owner = owner();
    let never = BotTeamBarrier::never_paused(owner);
    assert_eq!((never.epoch, never.paused, never.request), (0, false, None));
    assert!(never.valid());
    let request = Some(BotRequestId::mint());
    for (epoch, paused, request, valid) in [
        (0, true, None, false),
        (0, false, request, false),
        (1, true, None, false),
        (1, false, None, false),
        (1, true, request, true),
        (1, false, request, true),
        (i64::MAX as u64, true, request, true),
        (i64::MAX as u64 + 1, true, request, false),
    ] {
        let barrier = BotTeamBarrier {
            owner,
            epoch,
            paused,
            request,
        };
        assert_eq!(barrier.valid(), valid, "{epoch}/{paused}");
    }
}

#[test]
fn m8_ipc_team_codec_each_refusal_has_one_fixed_text_and_stale_carries_the_held_epoch() {
    for code in ["bot_team_paused", "bot_team_stale", "bot_team_conflict"] {
        let failure = team_failure(code).unwrap();
        assert_eq!(failure.code, code);
        assert!(!failure.retryable, "{code}");
        assert!(!failure.message.is_empty());
        assert!(failure.details.is_empty());
        let bytes = koru_ipc::frame::encode(
            crate::ipc::WIRE_VERSION,
            &crate::ipc::Response::Failure(failure.clone()),
        )
        .unwrap();
        let read = crate::local::read_current_response(&mut bytes.as_slice()).unwrap();
        assert!(matches!(read, crate::ipc::Response::Failure(decoded) if decoded == failure));
    }
    assert!(team_failure("bot_fenced").is_none());
    assert!(team_failure("bot_boot_pending").is_none());

    let stale = team_stale(41);
    assert_eq!(stale.code, "bot_team_stale");
    assert!(!stale.retryable);
    assert_eq!(team_held_epoch(&stale), Some(41));
    assert_eq!(stale.details[TEAM_HELD_EPOCH], serde_json::json!(41));
    // Only that code carries one, and only a readable one counts.
    assert_eq!(
        team_held_epoch(&team_failure("bot_team_stale").unwrap()),
        None
    );
    assert_eq!(
        team_held_epoch(
            &team_failure("bot_team_conflict")
                .unwrap()
                .with_detail(TEAM_HELD_EPOCH, 7u64)
        ),
        None
    );
}

/// The frame layer is the version gate: a peer on IPC 19 is refused before
/// its body is decoded, whatever the body. The worker's own version check is
/// tested with the host.
#[test]
fn m8_ipc_team_frame_version_a_frame_stamped_19_is_refused_by_the_20_reader() {
    assert_eq!(crate::ipc::WIRE_VERSION, 20);
    let previous = crate::ipc::WIRE_VERSION - 1;
    assert_eq!(previous, 19);
    for barrier in values() {
        // A reply stamped 19 never becomes an answer to the team command.
        let reply = crate::ipc::Response::Bot(BotReply::TeamBarrier {
            barrier: barrier.clone(),
        });
        let old = koru_ipc::frame::encode(previous, &reply).unwrap();
        let failure = crate::local::read_current_response(&mut old.as_slice()).unwrap_err();
        assert_eq!(failure.code, "delegate_incompatible");
        assert!(!failure.retryable);
        // The version is checked before the body: an undecodable 19 body is
        // the same refusal, never a decode error.
        let undecodable =
            koru_ipc::frame::encode(previous, &serde_json::json!({"old":true})).unwrap();
        let failure = crate::local::read_current_response(&mut undecodable.as_slice()).unwrap_err();
        assert_eq!(failure.code, "delegate_incompatible");
        // The command stamped 19 carries its stamp, so a reader that compares
        // it never mistakes it for a current frame.
        let command = BotCommand::TeamBarrier { barrier };
        let stamped = koru_ipc::frame::encode(previous, &command).unwrap();
        let (version, _) = koru_ipc::frame::read_raw(&mut stamped.as_slice()).unwrap();
        assert_eq!(version, previous);
        assert_ne!(version, crate::ipc::WIRE_VERSION);
    }
}
