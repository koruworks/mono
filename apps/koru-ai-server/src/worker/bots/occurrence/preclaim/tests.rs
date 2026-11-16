// SPDX-License-Identifier: MPL-2.0
//! Portable owner checks use the production final validator and real SQL rows.
use super::*;

fn fixture() -> (Repository, SynchronizeRoutinePreclaim, Install) {
    let (repo, request) = policy::preclaim_fixture();
    let snapshot = snapshot(&repo, &request).unwrap();
    let hint = BotBootAdmission::Missing {};
    let admission = repo
        .bot_admission_barrier(&snapshot.metadata, &hint)
        .unwrap();
    let m = &snapshot.metadata;
    let global =
        policy::global_barrier(m.owner, m.id, m.revision, m.policy_revision, m.policy()).unwrap();
    let routine = policy::current_routine_barrier(&snapshot.creation.identity, &snapshot.current);
    (
        repo,
        request,
        Install {
            observed: Hint { snapshot, hint },
            admission,
            global,
            routine,
        },
    )
}

#[test]
fn preclaim_final_selector_never_replaces_installed_request() {
    let (repo, request, install) = fixture();
    assert_eq!(validate_install(&repo, &request, &install), Ok(()));
    let installed = install.admission.clone();
    let other = BotRequestId::mint();
    repo.fixture_execute_batch(&format!(
        "UPDATE bot_command_receipts SET request_id='{other}' WHERE command_kind='create'"
    ))
    .unwrap();
    assert!(snapshot(&repo, &request).unwrap() == install.observed.snapshot);
    let selected = repo
        .bot_admission_barrier(&install.observed.snapshot.metadata, &install.observed.hint)
        .unwrap();
    assert_eq!(selected.request, other);
    assert_eq!(
        validate_install(&repo, &request, &install),
        Err(RoutineBridgeRefusal::Conflict)
    );
    assert_eq!(install.admission, installed);
    assert!(matches!(
        install.observed.hint,
        BotBootAdmission::Missing {}
    ));
}

#[test]
fn preclaim_final_validation_compares_complete_binding_and_capture_and_both_profiles() {
    let (repo, request, install) = fixture();
    for change in 0..4 {
        let mut altered = install.clone();
        match change {
            0 => altered.observed.snapshot.creation.origin_edit_revision = Some(vec![9]),
            1 => altered.observed.snapshot.row.updated_at_ms += 1,
            2 => altered.observed.snapshot.row.capture.library.index_digest = [9; 32],
            _ => altered.observed.snapshot.metadata.main_chat = ChatId::new(),
        }
        assert_eq!(
            validate_install(&repo, &request, &altered),
            Err(RoutineBridgeRefusal::Conflict)
        );
    }
    let mut altered = install.observed.snapshot.clone();
    altered
        .creation
        .identity
        .seed
        .header
        .definition
        .template
        .requested_by = ActorRef::Person {
        profile: koru_ai_core::ProfileId::new(),
    };
    assert_eq!(altered.profile(), Err(RoutineBridgeRefusal::Forbidden));
    altered = install.observed.snapshot.clone();
    altered
        .row
        .capture
        .schedule_definition
        .template
        .requested_by = ActorRef::Person {
        profile: koru_ai_core::ProfileId::new(),
    };
    assert_eq!(altered.profile(), Err(RoutineBridgeRefusal::Forbidden));
}
