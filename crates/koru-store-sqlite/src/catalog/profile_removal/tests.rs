// SPDX-License-Identifier: MPL-2.0

use super::*;

fn fixture() -> (SqliteCatalog, ProfileId) {
    let mut catalog = SqliteCatalog::in_memory().unwrap();
    let profile = Profile::new("Owner", Locale::default(), at(0)).unwrap();
    catalog.put_profile(&profile).unwrap();
    (catalog, profile.id)
}

fn at(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn begin(
    catalog: &mut SqliteCatalog,
    id: ProfileId,
    millis: i64,
    kind: RemovalKind,
) -> RemovalMark {
    catalog
        .update_profile_removal(
            id,
            RemovalUpdate::Begin {
                kind,
                at: at(millis),
            },
        )
        .unwrap()
        .unwrap()
}

#[test]
fn lease_stop_supersession_and_keep_reject_old_workers() {
    let (mut catalog, id) = fixture();
    let first = begin(&mut catalog, id, 0, RemovalKind::Local);
    assert_eq!(
        catalog
            .update_profile_removal(
                id,
                RemovalUpdate::Begin {
                    kind: RemovalKind::Local,
                    at: at(60_000)
                }
            )
            .unwrap_err()
            .code(),
        Code::RemovalInProgress
    );
    let second = begin(&mut catalog, id, 60_001, RemovalKind::Local);
    assert_ne!(first.attempt, second.attempt);
    for change in [
        RemovalUpdate::Stop {
            attempt: first.attempt,
        },
        RemovalUpdate::Keep {
            attempt: first.attempt,
        },
        RemovalUpdate::Step {
            attempt: first.attempt,
            at: at(60_002),
        },
    ] {
        assert_eq!(
            catalog
                .update_profile_removal(id, change)
                .unwrap_err()
                .code(),
            Code::RemovalAttemptStale
        );
        assert_eq!(read(&catalog.connection, id).unwrap(), Some(second.clone()));
    }
    catalog
        .update_profile_removal(
            id,
            RemovalUpdate::Stop {
                attempt: second.attempt,
            },
        )
        .unwrap();
    assert_eq!(
        catalog
            .update_profile_removal(
                id,
                RemovalUpdate::Step {
                    attempt: second.attempt,
                    at: at(60_003)
                }
            )
            .unwrap_err()
            .code(),
        Code::RemovalAttemptStale
    );
    let third = begin(&mut catalog, id, 60_003, RemovalKind::Local);
    catalog
        .update_profile_removal(
            id,
            RemovalUpdate::Keep {
                attempt: third.attempt,
            },
        )
        .unwrap();
    assert!(read(&catalog.connection, id).unwrap().is_none());
    assert_eq!(
        catalog
            .remove_profile_local(id, Some(third.attempt), at(60_004))
            .unwrap_err()
            .code(),
        Code::RemovalAttemptStale
    );
    assert!(catalog.profile(id).unwrap().is_some());
}

#[test]
fn steps_refresh_liveness_and_hosted_cleanup_cannot_be_downgraded_or_kept() {
    let (mut catalog, id) = fixture();
    let first = begin(&mut catalog, id, 0, RemovalKind::HostedCleanup);
    catalog
        .update_profile_removal(
            id,
            RemovalUpdate::Step {
                attempt: first.attempt,
                at: at(50_000),
            },
        )
        .unwrap();
    assert_eq!(
        catalog
            .update_profile_removal(
                id,
                RemovalUpdate::Begin {
                    kind: RemovalKind::Local,
                    at: at(100_000)
                }
            )
            .unwrap_err()
            .code(),
        Code::RemovalInProgress
    );
    catalog
        .update_profile_removal(
            id,
            RemovalUpdate::Stop {
                attempt: first.attempt,
            },
        )
        .unwrap();
    let second = begin(&mut catalog, id, 100_000, RemovalKind::Local);
    assert_eq!(second.kind, RemovalKind::HostedCleanup);
    assert_eq!(
        catalog
            .update_profile_removal(
                id,
                RemovalUpdate::Keep {
                    attempt: second.attempt
                }
            )
            .unwrap_err()
            .code(),
        Code::RemovalKindHosted
    );
}

#[test]
fn completion_compares_attempt_before_empty_check_and_clears_mark_atomically() {
    let (mut catalog, id) = fixture();
    let workspace = Workspace::new(id, "Content", at(0)).unwrap();
    catalog.put_workspace(&workspace).unwrap();
    assert_eq!(
        catalog
            .remove_profile_local(id, None, at(1))
            .unwrap_err()
            .code(),
        Code::RemovalNotBegun
    );
    assert_eq!(
        catalog
            .remove_profile_local(id, Some(RemovalAttemptId::new()), at(1))
            .unwrap_err()
            .code(),
        Code::RemovalAttemptStale
    );
    let mark = begin(&mut catalog, id, 1, RemovalKind::Local);
    assert_eq!(
        catalog
            .remove_profile_local(id, Some(mark.attempt), at(2))
            .unwrap_err()
            .code(),
        Code::ProfileNotEmpty
    );
    catalog
        .set_workspace_deleted(workspace.id, Some(at(3)), &[])
        .unwrap();
    // Failure at either durable boundary rolls back both the marker and removal.
    catalog.connection.execute_batch("CREATE TEMP TRIGGER fail_removal BEFORE DELETE ON store_state BEGIN SELECT RAISE(FAIL, 'fixture'); END;").unwrap();
    assert_eq!(
        catalog
            .remove_profile_local(id, Some(mark.attempt), at(4))
            .unwrap_err()
            .code(),
        Code::StorageFailed
    );
    assert!(catalog.profile(id).unwrap().is_some());
    assert_eq!(read(&catalog.connection, id).unwrap(), Some(mark.clone()));
    catalog
        .connection
        .execute_batch("DROP TRIGGER fail_removal;")
        .unwrap();
    assert_eq!(
        catalog
            .remove_profile_local(id, Some(mark.attempt), at(5))
            .unwrap(),
        LocalProfileRemoval::Removed
    );
    assert!(read(&catalog.connection, id).unwrap().is_none());
    assert!(catalog.profile(id).unwrap().is_none());
    assert!(
        catalog
            .workspace(workspace.id)
            .unwrap()
            .unwrap()
            .deleted_at
            .is_some()
    );
    assert_eq!(
        catalog
            .remove_profile_local(id, Some(mark.attempt), at(6))
            .unwrap_err()
            .code(),
        Code::RemovalAttemptStale
    );
}
