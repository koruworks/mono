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
