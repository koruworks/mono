// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_api::bots::{BotCommand, BotReply};
use koru_delegate_core::{
    BotId, BotOwner, BotRequestId, RoutineDigest32, RoutineRouteIdentity, ScheduleId,
};

#[test]
fn routine_route_actual_actor_retains_exact_identity_and_refuses_changed_owner() {
    let (mut actor, _, _) = actor();
    actor.device = DeviceId::new();
    let route = RoutineRouteIdentity {
        owner: BotOwner {
            user_scope: koru_store_core::UserScopeId::new(),
            device: actor.device,
        },
        bot: BotId::mint(),
        schedule: ScheduleId::mint(),
        workspace: WorkspaceId::new(),
        creation_request: BotRequestId::mint(),
        seed_hash: RoutineDigest32::from_bytes([1; 32]),
        created_at_ms: 1,
    };
    for _ in 0..2 {
        assert_eq!(
            actor
                .bot_command(BotCommand::RegisterRoutineRoute {
                    identity: route.clone()
                })
                .unwrap(),
            BotReply::RoutineRoute {
                identity: route.clone()
            }
        );
    }
    let slot = koru_delegate_core::RoutineSlot {
        owner: route.owner,
        bot: route.bot,
        schedule: route.schedule,
        occurrence_ms: 60000,
    };
    let BotReply::RoutineClaim {
        observation: koru_delegate_core::RoutineClaimObservation::Absent { slot: observed, .. },
    } = actor
        .bot_command(BotCommand::ReadRoutineClaim { slot })
        .unwrap()
    else {
        panic!("exact absent observation required")
    };
    assert_eq!(observed, slot);
    let acknowledgement:koru_delegate_core::RoutineLinkAcknowledgement=serde_json::from_value(serde_json::json!({
        "claim":{"slot":slot,"definition":{"revision":1,"digest":"11".repeat(32),"owner_handoff":null},
        "ids":{"submission":uuid::Uuid::new_v4(),"run":uuid::Uuid::new_v4(),"chat":uuid::Uuid::new_v4(),"attempt":uuid::Uuid::new_v4(),"user_message":uuid::Uuid::new_v4(),"assistant_message":uuid::Uuid::new_v4()},
        "preparation_hash":"22".repeat(32),"claimed_at_ms":60001},"outcome":{"state":"linked"}
    })).unwrap();
    // A valid-shaped caller assertion cannot fabricate the independent claim.
    assert!(
        actor
            .bot_command(BotCommand::AcknowledgeRoutineLink {
                acknowledgement: acknowledgement.clone()
            })
            .is_err()
    );
    let mut foreign = slot;
    foreign.owner.device = DeviceId::new();
    assert!(
        actor
            .bot_command(BotCommand::ReadRoutineClaim { slot: foreign })
            .is_err()
    );
    let mut foreign = acknowledgement;
    foreign.claim.slot.owner.device = DeviceId::new();
    assert!(
        actor
            .bot_command(BotCommand::AcknowledgeRoutineLink {
                acknowledgement: foreign
            })
            .is_err()
    );
    let mut changed = route.clone();
    changed.seed_hash = RoutineDigest32::from_bytes([2; 32]);
    assert!(
        actor
            .bot_command(BotCommand::RegisterRoutineRoute { identity: changed })
            .is_err()
    );
    let mut changed = route.clone();
    changed.owner.device = DeviceId::new();
    assert!(
        actor
            .bot_command(BotCommand::RegisterRoutineRoute { identity: changed })
            .is_err()
    );
    assert_eq!(
        actor
            .bot_command(BotCommand::RegisterRoutineRoute {
                identity: route.clone()
            })
            .unwrap(),
        BotReply::RoutineRoute { identity: route }
    );
    // NeverDocuments/NeverSessions in actor() panic if registration launches.
}
