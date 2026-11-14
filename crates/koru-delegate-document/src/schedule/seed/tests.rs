// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::*;
use koru_delegate_schedule::*;

pub(in crate::schedule) fn fixture() -> (ScheduleHeader, ScheduleName) {
    let owner = BotOwner {
        user_scope: koru_store_core::UserScopeId::new(),
        device: DeviceId::new(),
    };
    let home = WorkspaceId::new();
    let header = ScheduleHeader {
        schedule: ScheduleId::mint(),
        created_at_ms: 1,
        initial_owner: owner.device,
        definition: ScheduleDefinition {
            revision: 1,
            effective_at_ms: 1,
            routine: Some(
                RoutineAssociation::new(owner, BotId::mint(), home, BotRequestId::mint()).unwrap(),
            ),
            lifecycle: ScheduleLifecycle::Paused {
                reason: SchedulePauseReason::Manual,
            },
            recurrence: serde_json::from_value(
                serde_json::json!({"type":"floating","cron":"0 9 * * *"}),
            )
            .unwrap(),
            template: DelegationTemplate {
                requested_by: ActorRef::Person {
                    profile: ProfileId::new(),
                },
                objective: Objective::new("Routine seed").unwrap(),
                agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                requested_model: None,
                scope: WorkspaceScope::BotFolder {
                    folder: BotWorkingFolder::new("/fixture".into()).unwrap(),
                },
                grants: [home].into(),
                context: vec![],
            },
        },
    };
    (header, ScheduleName::new("Routine").unwrap())
}

#[test]
fn routine_seed_retains_original_peer_and_semantic_version() {
    let (header, name) = fixture();
    let first = ScheduleDocument::routine_seed_update(&header, &name).unwrap();
    let second = ScheduleDocument::routine_seed_update(&header, &name).unwrap();
    assert_ne!(first, second);
    let original = ScheduleDocument::verify_routine_seed(&first, &header, &name).unwrap();
    let replay = ScheduleDocument::verify_routine_seed(&first, &header, &name).unwrap();
    assert_eq!(original.version(), replay.version());
    assert_ne!(
        original.version(),
        ScheduleDocument::verify_routine_seed(&second, &header, &name)
            .unwrap()
            .version()
    );
    assert_eq!(original.header().unwrap(), header);
    assert!(original.handoffs().unwrap().is_empty());
    assert!(original.firings().unwrap().is_empty());
}

#[test]
fn routine_seed_refuses_changed_shape_schema_history_and_pending_ancestry() {
    let (header, name) = fixture();
    for mutation in 0..7 {
        let doc = ScheduleDocument::create_named(&header, &name).unwrap();
        match mutation {
            0 => {
                doc.root().insert(K_SCHEMA, 1).unwrap();
            }
            1 => {
                doc.root().delete(K_NAME).unwrap();
            }
            2 => {
                doc.root().insert(K_RETIRED, true).unwrap();
            }
            3 => {
                doc.doc.get_map("extra").insert("secret", true).unwrap();
            }
            4 => {
                doc.handoff_list().push(true).unwrap();
            }
            5 => {
                doc.firing_list().push(true).unwrap();
            }
            _ => {
                doc.root().insert(K_NAME, "Renamed").unwrap();
            }
        }
        doc.doc.commit();
        assert!(
            ScheduleDocument::verify_routine_seed(&doc.export_snapshot().unwrap(), &header, &name)
                .is_err(),
            "mutation {mutation}"
        );
    }
    let doc = ScheduleDocument::create_named(&header, &name).unwrap();
    let before = doc.version();
    doc.root().insert("extra", true).unwrap();
    doc.doc.commit();
    let delta = doc.export_updates_since(&before).unwrap();
    assert!(ScheduleDocument::verify_routine_seed(&delta, &header, &name).is_err());
    assert!(
        ScheduleDocument::verify_routine_seed(&vec![0; SEED_LIMIT + 1], &header, &name).is_err()
    );
    let mut active = header.clone();
    active.definition.lifecycle = ScheduleLifecycle::Active;
    assert!(ScheduleDocument::routine_seed_update(&active, &name).is_err());
}
