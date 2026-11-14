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
