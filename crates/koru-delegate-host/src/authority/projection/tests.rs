// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn classification_requires_positive_current_generation_and_never_falls_back() {
    let projection = AuthorityProjection::default();
    let run = RunId::mint();
    let first = AttemptGeneration::FIRST;
    assert!(projection.resolve(run, first).is_err());
    projection
        .publish(run, first, RunToolAuthority::Ordinary {})
        .unwrap();
    projection
        .publish(run, first.next(), RunToolAuthority::Ordinary {})
        .unwrap();
    assert!(projection.resolve(run, first).is_err());
    assert!(
        projection
            .publish(run, first, RunToolAuthority::Ordinary {})
            .is_err()
    );
    projection.remove(run, Some(first));
    assert_eq!(
        projection.resolve(run, first.next()).unwrap(),
        RunToolAuthority::Ordinary {}
    );
    projection.remove(run, Some(first.next()));
    assert!(projection.resolve(run, first.next()).is_err());
    assert!(
        AuthorityProjection::default()
            .resolve(run, first.next())
            .is_err()
    );
}

#[test]
fn overflow_refuses_without_evicting_an_existing_registration() {
    let projection = AuthorityProjection::default();
    let first = AttemptGeneration::FIRST;
    let retained = RunId::mint();
    projection
        .publish(retained, first, RunToolAuthority::Ordinary {})
        .unwrap();
    for _ in 1..CAPACITY {
        projection
            .publish(RunId::mint(), first, RunToolAuthority::Ordinary {})
            .unwrap();
    }
    assert!(
        projection
            .publish(RunId::mint(), first, RunToolAuthority::Ordinary {})
            .is_err()
    );
    assert_eq!(
        projection.resolve(retained, first).unwrap(),
        RunToolAuthority::Ordinary {}
    );
    projection
        .publish(retained, first.next(), RunToolAuthority::Ordinary {})
        .unwrap();
}

#[test]
fn synchronous_provider_discovery_does_not_wait_for_its_creating_actor() {
    struct FakeSessionFactory {
        projection: AuthorityProjection,
    }
    impl FakeSessionFactory {
        fn create(&self, run: RunId, generation: AttemptGeneration) {
            // The session initializer synchronously discovers the catalog. The
            // private server uses this same read; it sends no actor command.
            assert_eq!(
                self.projection.resolve(run, generation).unwrap(),
                RunToolAuthority::Ordinary {}
            );
        }
    }
    let projection = AuthorityProjection::default();
    let run = RunId::mint();
    let actor = Mutex::new(());
    let _actor_command = actor.lock().unwrap();
    projection
        .publish(run, AttemptGeneration::FIRST, RunToolAuthority::Ordinary {})
        .unwrap();
    FakeSessionFactory { projection }.create(run, AttemptGeneration::FIRST);
}

fn bot() -> RunToolAuthority {
    use koru_delegate_core::{
        BotAccess, BotId, BotOwner, BotProvenance, BotRequestId, CapturedBotPolicy, DeviceId,
        WorkspaceId,
    };
    let workspace = WorkspaceId::from_uuid(uuid::Uuid::new_v4());
    RunToolAuthority::Bot {
        provenance: BotProvenance {
            parent: None,
            routine: None,
            owner: BotOwner {
                user_scope: serde_json::from_value(serde_json::json!(uuid::Uuid::new_v4()))
                    .unwrap(),
                device: DeviceId::new(),
            },
            bot: BotId::mint(),
            submission: BotRequestId::mint(),
            chat_id: uuid::Uuid::new_v4(),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        },
        policy: CapturedBotPolicy {
            workspace,
            destination: workspace,
            access: BotAccess::Default,
            selected_pages: Default::default(),
            connections: Default::default(),
        },
    }
}

/// O147: the open wait overlays the immutable launch value, across the
/// successor's generation, and lifts when the actor clears it.
#[test]
fn an_open_decision_wait_resolves_a_bot_run_as_waiting_until_it_is_cleared() {
    let projection = AuthorityProjection::default();
    let run = RunId::mint();
    let first = AttemptGeneration::FIRST;
    let launch = bot();
    projection.publish(run, first, launch.clone()).unwrap();
    let decision = DecisionId::mint();
    projection
        .publish_wait(run, Some((decision, DecisionPhase::Holding)))
        .unwrap();
    let RunToolAuthority::BotDecisionWait {
        provenance,
        policy,
        decision: named,
        phase,
    } = projection.resolve(run, first).unwrap()
    else {
        panic!("a waiting Bot Run")
    };
    assert_eq!(Some((&provenance, &policy)), launch.bot_subject());
    assert_eq!((named, phase), (decision, DecisionPhase::Holding));
    // The successor's launch value is published for its own generation; the
    // wait is the Run's, so the successor classifies as waiting too.
    projection
        .publish(run, first.next(), launch.clone())
        .unwrap();
    projection
        .publish_wait(run, Some((decision, DecisionPhase::AwaitRetry)))
        .unwrap();
    assert!(matches!(
        projection.resolve(run, first.next()).unwrap(),
        RunToolAuthority::BotDecisionWait {
            phase: DecisionPhase::AwaitRetry,
            ..
        }
    ));
    // Settled or closed: the actor clears it and the launch value returns.
    projection.publish_wait(run, None).unwrap();
    assert_eq!(projection.resolve(run, first.next()).unwrap(), launch);
    // A waiting value is never accepted as a launch value, and an ordinary
    // Run never resolves as waiting.
    assert!(
        projection
            .publish(
                RunId::mint(),
                first,
                RunToolAuthority::BotDecisionWait {
                    provenance,
                    policy,
                    decision,
                    phase: DecisionPhase::Holding,
                },
            )
            .is_err()
    );
    let ordinary = RunId::mint();
    projection
        .publish(ordinary, first, RunToolAuthority::Ordinary {})
        .unwrap();
    projection
        .publish_wait(ordinary, Some((decision, DecisionPhase::Holding)))
        .unwrap();
    assert_eq!(
        projection.resolve(ordinary, first).unwrap(),
        RunToolAuthority::Ordinary {}
    );
}
