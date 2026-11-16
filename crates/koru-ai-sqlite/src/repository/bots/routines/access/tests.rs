// SPDX-License-Identifier: MPL-2.0
//! The current-grant reader and the access-edit writer on an in-memory
//! database. No delegate, Store or worker: the barrier install between
//! `prepare` and `complete` is the caller's, and is not simulated here.
use super::super::tests::{confirm, fixture, prepared};
use super::*;

fn fingerprint(n: u8) -> RoutineDigest32 {
    RoutineDigest32::from_bytes([n; 32])
}

/// A Bot with one bound Routine.
fn bound() -> (Repository, Bot, BotOwner, BotRoutineCreation) {
    let (mut repo, bot, owner, command) = fixture();
    let creation = prepared(&mut repo, owner, &command);
    let creation = repo
        .transition_bot_routine_creation(
            &creation.identity,
            creation.observed(),
            confirm(&creation),
            11,
        )
        .unwrap();
    (repo, bot, owner, creation)
}

fn pages(bot: &Bot, count: usize) -> Vec<BotPage> {
    let mut pages: Vec<BotPage> = (0..count)
        .map(|_| BotPage {
            workspace: bot.home_workspace,
            document: DocumentId::new(),
        })
        .collect();
    pages.sort();
    pages
}

fn edit(
    bot: &Bot,
    creation: &BotRoutineCreation,
    expected: u64,
    access: BotAccess,
    selected_pages: Vec<BotPage>,
) -> BotRoutineAccessUpdate {
    BotRoutineAccessUpdate {
        request: BotRequestId::mint(),
        bot: bot.id,
        schedule: creation.identity.schedule,
        expected_grant_revision: expected,
        access,
        selected_pages,
    }
}

fn applied(admission: BotRoutineAccessAdmission) -> BotRoutineAccessRecord {
    match admission {
        BotRoutineAccessAdmission::Prepared(record) => record,
        other => panic!("not written: {other:?}"),
    }
}

/// Receipts of the new kind, grant rows, and the 021 grant columns as raw
/// values: what an edit may and may not write.
fn stored(repo: &Repository, creation: &BotRoutineCreation) -> (u32, u32, String) {
    let count = |sql: &str| -> u32 { repo.connection.query_row(sql, [], |r| r.get(0)).unwrap() };
    let original: String = repo
        .connection
        .query_row(
            "SELECT source_bot_policy_revision||'|'||grant_revision||'|'||grant_json||'|'||hex(grant_digest)||'|'||updated_at_ms FROM bot_routines WHERE schedule_id=?1",
            [creation.identity.schedule.to_storage()],
            |r| r.get(0),
        )
        .unwrap();
    (
        count("SELECT count(*) FROM bot_command_receipts WHERE command_kind='update_routine_access'"),
        count("SELECT count(*) FROM bot_routine_grants"),
        original,
    )
}

#[test]
fn routine_current_grant_with_no_row_is_the_creation_grant_everywhere_it_is_read() {
    let (repo, bot, owner, creation) = bound();
    let schedule = creation.identity.schedule;
    let current = repo.routine_current_grant(owner, bot.id, schedule).unwrap();
    assert!(!current.is_edited());
    assert_eq!(current.grant, creation.identity.grant);
    assert_eq!(current.request, creation.identity.creation_request);
    assert_eq!(current.grant.grant_revision, 1);
    assert_eq!(stored(&repo, &creation).1, 0, "no row is backfilled");
    assert_eq!(
        repo.routine_access_pending(owner, bot.id, schedule)
            .unwrap(),
        None
    );
    // The list row and both detail lines show that same grant.
    let snapshot = repo.routine_list_snapshot(owner, bot.id, None, 16).unwrap();
    let (_, _, item) = repo.routine_list_entry(&snapshot, 0).unwrap();
    let setup = creation.identity.grant.summary().unwrap();
    assert_eq!(item.grant, setup);
    let subject = repo.routine_read_subject(owner, bot.id).unwrap();
    let detail = repo
        .routine_detail_snapshot(&subject, schedule, None, 16)
        .unwrap();
    assert_eq!(detail.current, current);
    let reply = detail.response(100, RoutineDetailLive::Unavailable {}).unwrap();
    assert_eq!(reply.access, setup);
    assert_eq!(reply.access_at_setup, setup);
    // A Routine that is not bound yet reads the same way.
    let (mut repo, bot, owner, command) = fixture();
    let unbound = prepared(&mut repo, owner, &command);
    assert_eq!(
        repo.routine_current_grant(owner, bot.id, unbound.identity.schedule)
            .unwrap()
            .grant,
        unbound.identity.grant
    );
    assert_eq!(
        repo.routine_current_grant(owner, bot.id, ScheduleId::mint()),
        Err(BotError::NotFound)
    );
}

#[test]
fn routine_access_edit_writes_one_grant_row_and_every_reader_returns_the_edited_grant() {
    let (mut repo, bot, owner, creation) = bound();
    let schedule = creation.identity.schedule;
    // The Bot's own policy moved since the Routine was created: the edit
    // records the revision observed now.
    repo.connection
        .execute(
            "UPDATE bots SET policy_revision=policy_revision+4 WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    let before = stored(&repo, &creation);
    let list_before = repo.routine_list_snapshot(owner, bot.id, None, 16).unwrap();
    let (_, checkpoint_before, _) = repo.routine_list_entry(&list_before, 0).unwrap();
    let subject = repo.routine_read_subject(owner, bot.id).unwrap();
    let detail_before = repo
        .routine_detail_snapshot(&subject, schedule, None, 16)
        .unwrap();
    repo.validate_routine_list(&list_before, std::slice::from_ref(&checkpoint_before))
        .unwrap();
    repo.validate_routine_detail(&detail_before).unwrap();

    let selected = pages(&bot, 2);
    let first = edit(&bot, &creation, 1, BotAccess::Workspace, selected.clone());
    let record = applied(
        repo.prepare_routine_access_update(owner, &first, fingerprint(1), 20)
            .unwrap(),
    );
    assert_eq!(record.stage, BotReceiptStage::Prepared);
    assert_eq!(record.operation().status, BotOperationStatus::Pending);
    assert_eq!(record.operation().kind, BotOperationKind::UpdateRoutineAccess);

    let current = repo.routine_current_grant(owner, bot.id, schedule).unwrap();
    assert!(current.is_edited());
    assert_eq!(current.request, first.request);
    assert_eq!(current.grant.grant_revision, 2);
    assert_eq!(
        current.grant.source_bot_policy_revision,
        bot.policy_revision + 4
    );
    assert_eq!(
        current.grant.policy.access,
        koru_delegate_core::BotAccess::Workspace
    );
    assert_eq!(
        current.grant.policy.selected_pages,
        selected.iter().map(|page| page.document).collect()
    );
    assert_eq!(current.grant.policy.workspace, bot.home_workspace);
    assert_eq!(current.grant.policy.destination, bot.home_workspace);
    assert_eq!(
        current.grant.digest,
        current.grant.canonical_digest().unwrap()
    );
    assert_eq!(record.receipt.summary(), Some(current.grant.summary().unwrap()));
    // One receipt, one row, and migration 021's frozen columns untouched.
    let after = stored(&repo, &creation);
    assert_eq!((after.0, after.1), (1, 1));
    assert_eq!(after.2, before.2);
    assert_eq!(
        repo.bot_routine_creation(owner, bot.id, schedule)
            .unwrap()
            .unwrap(),
        creation
    );

    // The edit is pending until its barrier install is confirmed.
    assert_eq!(
        repo.routine_access_pending(owner, bot.id, schedule)
            .unwrap(),
        Some(first.request)
    );
    let page = repo.pending_routine_access(0, 16).unwrap();
    assert_eq!(page.pending.len(), 1);
    assert_eq!(
        (page.pending[0].1, page.pending[0].2, page.pending[0].3),
        (bot.id, owner.user_scope, first.request)
    );
    let done = repo
        .complete_routine_access_update(owner, bot.id, first.request, 30)
        .unwrap();
    assert_eq!(done.stage, BotReceiptStage::Completed);
    assert_eq!(done.operation().status, BotOperationStatus::Accepted);
    assert_eq!(
        repo.routine_access_pending(owner, bot.id, schedule)
            .unwrap(),
        None
    );
    assert!(repo.pending_routine_access(0, 16).unwrap().pending.is_empty());
    assert_eq!(
        repo.routine_access_record(owner, bot.id, first.request)
            .unwrap(),
        Some(done)
    );

    // The list row and the detail's "access" show the edited grant; "access
    // at setup" still shows the original; a read taken before the edit no
    // longer validates.
    let setup = creation.identity.grant.summary().unwrap();
    let edited = current.grant.summary().unwrap();
    assert_ne!(edited, setup);
    assert_eq!((edited.access, edited.selected_page_count), (BotAccess::Workspace, 2));
    let list = repo.routine_list_snapshot(owner, bot.id, None, 16).unwrap();
    let (_, checkpoint, item) = repo.routine_list_entry(&list, 0).unwrap();
    assert_eq!(item.grant, edited);
    repo.validate_routine_list(&list, &[checkpoint]).unwrap();
    assert_eq!(
        repo.validate_routine_list(&list, &[checkpoint_before]),
        Err(BotError::Conflict)
    );
    let subject = repo.routine_read_subject(owner, bot.id).unwrap();
    let detail = repo
        .routine_detail_snapshot(&subject, schedule, None, 16)
        .unwrap();
    let reply = detail.response(100, RoutineDetailLive::Unavailable {}).unwrap();
    assert_eq!(reply.access, edited);
    assert_eq!(reply.access_at_setup, setup);
    repo.validate_routine_detail(&detail).unwrap();
    assert_eq!(
        repo.validate_routine_detail(&detail_before),
        Err(BotError::Conflict)
    );

    // A later edit replaces the row. The earlier receipt stays.
    let second = edit(&bot, &creation, 2, BotAccess::Default, vec![]);
    applied(
        repo.prepare_routine_access_update(owner, &second, fingerprint(2), 40)
            .unwrap(),
    );
    let after = stored(&repo, &creation);
    assert_eq!((after.0, after.1), (2, 1));
    assert_eq!(after.2, before.2);
    let current = repo.routine_current_grant(owner, bot.id, schedule).unwrap();
    assert_eq!(
        (current.grant.grant_revision, current.request),
        (3, second.request)
    );
    assert_eq!(
        current.grant.policy.access,
        koru_delegate_core::BotAccess::Default
    );
    assert!(current.grant.policy.selected_pages.is_empty());
}

#[test]
fn routine_access_receipt_replays_idempotently_and_never_raises_the_revision_twice() {
    let (mut repo, bot, owner, creation) = bound();
    let schedule = creation.identity.schedule;
    let first = edit(&bot, &creation, 1, BotAccess::Workspace, pages(&bot, 3));
    let record = applied(
        repo.prepare_routine_access_update(owner, &first, fingerprint(1), 20)
            .unwrap(),
    );
    let written = stored(&repo, &creation);
    let grant = repo.routine_current_grant(owner, bot.id, schedule).unwrap();

    // An exact retry while the install is unconfirmed: the same receipt,
    // still prepared, and nothing written.
    for now in [21, 999] {
        assert_eq!(
            repo.prepare_routine_access_update(owner, &first, fingerprint(1), now)
                .unwrap(),
            BotRoutineAccessAdmission::Retained(record.clone())
        );
    }
    assert_eq!(stored(&repo, &creation), written);
    assert_eq!(
        repo.routine_current_grant(owner, bot.id, schedule).unwrap(),
        grant
    );

    // Completing is repeatable, and a retry after it returns the completed
    // receipt.
    let done = repo
        .complete_routine_access_update(owner, bot.id, first.request, 30)
        .unwrap();
    assert_eq!(
        repo.complete_routine_access_update(owner, bot.id, first.request, 31)
            .unwrap(),
        done
    );
    assert_eq!(
        repo.prepare_routine_access_update(owner, &first, fingerprint(1), 32)
            .unwrap(),
        BotRoutineAccessAdmission::Retained(done.clone())
    );
    assert_eq!(stored(&repo, &creation), written);

    // The same request with other content, or another fingerprint, conflicts.
    for change in 0..4 {
        let mut other = first.clone();
        let mut print = fingerprint(1);
        match change {
            0 => other.selected_pages.pop().map(|_| ()).unwrap(),
            1 => other.access = BotAccess::Default,
            2 => other.expected_grant_revision = 2,
            _ => print = fingerprint(9),
        }
        assert_eq!(
            repo.prepare_routine_access_update(owner, &other, print, 33),
            Err(BotError::Conflict),
            "change {change}"
        );
    }
    assert_eq!(stored(&repo, &creation), written);

    // After a later edit the first request still returns its own receipt,
    // and the grant stays at the later revision.
    let second = edit(&bot, &creation, 2, BotAccess::Default, vec![]);
    applied(
        repo.prepare_routine_access_update(owner, &second, fingerprint(2), 40)
            .unwrap(),
    );
    assert_eq!(
        repo.prepare_routine_access_update(owner, &first, fingerprint(1), 41)
            .unwrap(),
        BotRoutineAccessAdmission::Retained(done.clone())
    );
    assert_eq!(done.receipt.summary().unwrap().grant_revision, 2);
    assert_eq!(
        repo.routine_current_grant(owner, bot.id, schedule)
            .unwrap()
            .grant
            .grant_revision,
        3
    );
    assert_eq!(stored(&repo, &creation).0, 2);

    // The receipt namespace is shared: neither kind reads the other's.
    let mut reused = edit(&bot, &creation, 3, BotAccess::Workspace, vec![]);
    reused.request = creation.identity.creation_request;
    assert_eq!(
        repo.prepare_routine_access_update(owner, &reused, fingerprint(3), 50),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.routine_access_record(owner, bot.id, creation.identity.creation_request),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.bot_routine_creation_by_request(owner, bot.id, first.request),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.bot_receipt(owner.user_scope, first.request, &String::from(fingerprint(1))),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.routine_access_record(owner, bot.id, BotRequestId::mint())
            .unwrap(),
        None
    );
}

#[test]
fn routine_access_edit_refuses_stale_pending_unbound_held_foreign_and_malformed_requests() {
    let (mut repo, bot, owner, creation) = bound();
    let schedule = creation.identity.schedule;
    let untouched = stored(&repo, &creation);
    let try_edit = |repo: &mut Repository, command: &BotRoutineAccessUpdate| {
        repo.prepare_routine_access_update(owner, command, fingerprint(1), 20)
    };

    // A stale grant revision.
    assert_eq!(
        try_edit(&mut repo, &edit(&bot, &creation, 2, BotAccess::Workspace, vec![])),
        Err(BotError::RevisionConflict)
    );
    // Malformed: a page outside the home, pages not normalized, too many.
    let foreign = vec![BotPage {
        workspace: WorkspaceId::new(),
        document: DocumentId::new(),
    }];
    let mut unsorted = pages(&bot, 2);
    unsorted.reverse();
    let mut doubled = pages(&bot, 1);
    doubled.push(doubled[0].clone());
    for selected in [foreign, unsorted, doubled, pages(&bot, 1025)] {
        assert_eq!(
            try_edit(
                &mut repo,
                &edit(&bot, &creation, 1, BotAccess::Default, selected)
            ),
            Err(BotError::Invalid)
        );
    }
    // Another owner, and another Bot's Routine.
    let stranger = BotOwner {
        user_scope: owner.user_scope,
        device: DeviceId::from_uuid(uuid::Uuid::new_v4()),
    };
    assert_eq!(
        repo.prepare_routine_access_update(
            stranger,
            &edit(&bot, &creation, 1, BotAccess::Default, vec![]),
            fingerprint(1),
            20
        ),
        Err(BotError::Forbidden)
    );
    let mut missing = edit(&bot, &creation, 1, BotAccess::Default, vec![]);
    missing.schedule = ScheduleId::mint();
    assert_eq!(try_edit(&mut repo, &missing), Err(BotError::NotFound));
    assert_eq!(stored(&repo, &creation), untouched);

    // While one edit is prepared, another request is told which one.
    let first = edit(&bot, &creation, 1, BotAccess::Workspace, vec![]);
    applied(try_edit(&mut repo, &first).unwrap());
    let written = stored(&repo, &creation);
    for expected in [1, 2] {
        assert_eq!(
            try_edit(
                &mut repo,
                &edit(&bot, &creation, expected, BotAccess::Default, vec![])
            )
            .unwrap(),
            BotRoutineAccessAdmission::Pending {
                request: first.request
            }
        );
    }
    assert_eq!(stored(&repo, &creation), written);
    repo.complete_routine_access_update(owner, bot.id, first.request, 30)
        .unwrap();

    // An Archive or Trash holds the Bot: no new edit.
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='archived' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        try_edit(
            &mut repo,
            &edit(&bot, &creation, 2, BotAccess::Default, vec![])
        ),
        Err(BotError::Forbidden)
    );
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='trashed' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        try_edit(
            &mut repo,
            &edit(&bot, &creation, 2, BotAccess::Default, vec![])
        ),
        Err(BotError::NotFound)
    );
    assert_eq!(
        repo.routine_current_grant(owner, bot.id, schedule),
        Err(BotError::NotFound)
    );
    assert_eq!(stored(&repo, &creation), written);

    // A Routine whose creation is not bound has no access to edit.
    let (mut repo, bot, owner, command) = fixture();
    let unbound = prepared(&mut repo, owner, &command);
    assert_eq!(
        repo.prepare_routine_access_update(
            owner,
            &edit(&bot, &unbound, 1, BotAccess::Workspace, vec![]),
            fingerprint(1),
            20
        ),
        Err(BotError::Conflict)
    );
    assert_eq!(stored(&repo, &unbound).0, 0);
    // A pause or resume of the Bot still being delivered: retry later.
    let (mut repo, bot, owner, creation) = bound();
    repo.prepare_bot_pause(
        owner,
        bot.id,
        BotRequestId::mint(),
        bot.revision,
        true,
        &String::from(fingerprint(4)),
        15,
    )
    .unwrap();
    assert_eq!(
        repo.prepare_routine_access_update(
            owner,
            &edit(&bot, &creation, 1, BotAccess::Workspace, vec![]),
            fingerprint(1),
            20
        ),
        Err(BotError::Settling)
    );
    assert_eq!(stored(&repo, &creation).0, 0);
}

#[test]
fn routine_access_prepared_edit_still_completes_after_the_bot_is_archived_or_trashed() {
    for lifecycle in ["archived", "trashed"] {
        let (mut repo, bot, owner, creation) = bound();
        let first = edit(&bot, &creation, 1, BotAccess::Workspace, vec![]);
        applied(
            repo.prepare_routine_access_update(owner, &first, fingerprint(1), 20)
                .unwrap(),
        );
        repo.connection
            .execute(
                "UPDATE bots SET lifecycle=?2 WHERE bot_id=?1",
                params![bot.id.to_storage(), lifecycle],
            )
            .unwrap();
        // The restart scan still finds it, and the install is confirmed.
        assert_eq!(repo.pending_routine_access(0, 16).unwrap().pending.len(), 1);
        let done = repo
            .complete_routine_access_update(owner, bot.id, first.request, 30)
            .unwrap();
        assert_eq!(done.stage, BotReceiptStage::Completed, "{lifecycle}");
        assert!(repo.pending_routine_access(0, 16).unwrap().pending.is_empty());
        let stranger = BotOwner {
            user_scope: owner.user_scope,
            device: DeviceId::from_uuid(uuid::Uuid::new_v4()),
        };
        assert_eq!(
            repo.complete_routine_access_update(stranger, bot.id, first.request, 31),
            Err(BotError::Forbidden)
        );
        assert_eq!(
            repo.complete_routine_access_update(owner, bot.id, BotRequestId::mint(), 31),
            Err(BotError::NotFound)
        );
    }
}

#[test]
fn routine_access_refusal_is_retained_in_place_of_prepared_and_writes_no_grant() {
    let (mut repo, bot, owner, creation) = bound();
    let schedule = creation.identity.schedule;
    let command = edit(&bot, &creation, 1, BotAccess::Workspace, pages(&bot, 1));
    let refused = repo
        .refuse_routine_access_update(
            owner,
            &command,
            fingerprint(1),
            BotRoutineAccessRefusal::NotPaused,
            20,
        )
        .unwrap();
    assert_eq!(refused.stage, BotReceiptStage::Rejected);
    assert_eq!(refused.operation().status, BotOperationStatus::Rejected);
    assert_eq!(refused.receipt.summary(), None);
    let after = stored(&repo, &creation);
    assert_eq!((after.0, after.1), (1, 0));
    assert!(
        !repo
            .routine_current_grant(owner, bot.id, schedule)
            .unwrap()
            .is_edited()
    );
    assert!(repo.pending_routine_access(0, 16).unwrap().pending.is_empty());
    // A retry gets the same answer from either entry, and nothing is written.
    assert_eq!(
        repo.prepare_routine_access_update(owner, &command, fingerprint(1), 21)
            .unwrap(),
        BotRoutineAccessAdmission::Retained(refused.clone())
    );
    assert_eq!(
        repo.refuse_routine_access_update(
            owner,
            &command,
            fingerprint(1),
            BotRoutineAccessRefusal::PageForbidden,
            22
        )
        .unwrap(),
        refused
    );
    assert_eq!(
        repo.complete_routine_access_update(owner, bot.id, command.request, 23),
        Err(BotError::Conflict)
    );
    assert_eq!(stored(&repo, &creation), after);
    // A new request is free to apply, and an applied edit is not turned
    // into a refusal by a later call: its own receipt is returned.
    let next = edit(&bot, &creation, 1, BotAccess::Workspace, vec![]);
    let written = applied(
        repo.prepare_routine_access_update(owner, &next, fingerprint(2), 30)
            .unwrap(),
    );
    assert_eq!(
        repo.refuse_routine_access_update(
            owner,
            &next,
            fingerprint(2),
            BotRoutineAccessRefusal::NotPaused,
            31
        )
        .unwrap(),
        written
    );
    let after = stored(&repo, &creation);
    assert_eq!((after.0, after.1), (2, 1));
}

#[test]
fn routine_current_grant_refuses_a_corrupt_edited_row_instead_of_falling_back() {
    for sql in [
        "UPDATE bot_routine_grants SET grant_digest=zeroblob(32)",
        "UPDATE bot_routine_grants SET grant_revision=grant_revision+1",
        "UPDATE bot_routine_grants SET source_bot_policy_revision=source_bot_policy_revision+1",
        "UPDATE bot_routine_grants SET grant_json=json_set(grant_json,'$.policy.access','default')",
        "UPDATE bot_routine_grants SET grant_json='{}'",
        "UPDATE bot_routine_grants SET owner_device_id='00000000-0000-4000-8000-000000000009'",
        "UPDATE bot_command_receipts SET stage='rejected' WHERE command_kind='update_routine_access'",
        "UPDATE bot_command_receipts SET outcome_json=json_set(outcome_json,'$.outcome.selected_page_count',7) WHERE command_kind='update_routine_access'",
    ] {
        let (mut repo, bot, owner, creation) = bound();
        let schedule = creation.identity.schedule;
        let first = edit(&bot, &creation, 1, BotAccess::Workspace, vec![]);
        applied(
            repo.prepare_routine_access_update(owner, &first, fingerprint(1), 20)
                .unwrap(),
        );
        repo.complete_routine_access_update(owner, bot.id, first.request, 30)
            .unwrap();
        repo.connection
            .execute_batch(&format!("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON; {sql};"))
            .unwrap();
        assert_eq!(
            repo.routine_current_grant(owner, bot.id, schedule),
            Err(BotError::Storage),
            "{sql}"
        );
        // Neither the list row nor the detail invents the setup grant.
        let snapshot = repo.routine_list_snapshot(owner, bot.id, None, 16).unwrap();
        assert!(repo.routine_list_entry(&snapshot, 0).is_err(), "{sql}");
        let subject = repo.routine_read_subject(owner, bot.id).unwrap();
        assert!(
            repo.routine_detail_snapshot(&subject, schedule, None, 16)
                .is_err(),
            "{sql}"
        );
    }
}

#[test]
fn routine_access_write_failure_rolls_back_the_receipt_and_the_row_together() {
    let (mut repo, bot, owner, creation) = bound();
    let before = stored(&repo, &creation);
    repo.connection
        .execute_batch("CREATE TEMP TRIGGER fail_grant BEFORE INSERT ON bot_routine_grants BEGIN SELECT RAISE(ABORT,'fixture'); END;")
        .unwrap();
    let command = edit(&bot, &creation, 1, BotAccess::Workspace, vec![]);
    assert!(
        repo.prepare_routine_access_update(owner, &command, fingerprint(1), 20)
            .is_err()
    );
    assert_eq!(stored(&repo, &creation), before);
    assert_eq!(
        repo.routine_access_record(owner, bot.id, command.request)
            .unwrap(),
        None
    );
    repo.connection
        .execute_batch("DROP TRIGGER fail_grant")
        .unwrap();
    applied(
        repo.prepare_routine_access_update(owner, &command, fingerprint(1), 21)
            .unwrap(),
    );
}
