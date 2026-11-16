// SPDX-License-Identifier: MPL-2.0
//! An occurrence captures the grant its Routine runs under now, and only the
//! owner Test may capture a paused definition. Repository half only: the
//! claim record is synthetic, as in the sibling tests.
use super::*;
use koru_ai_core::bots::BotRoutineCaptureLifecycle;

fn edited(repo: &mut Repository, input: &BotRoutineCapturedSources) -> BotRoutineCurrentGrant {
    let owner = owner(&input.slot);
    let command = BotRoutineAccessUpdate {
        request: BotRequestId::mint(),
        bot: input.slot.bot,
        schedule: input.slot.schedule,
        expected_grant_revision: 1,
        access: BotAccess::Workspace,
        selected_pages: vec![],
    };
    repo.prepare_routine_access_update(owner, &command, RoutineDigest32::from_bytes([5; 32]), 20)
        .unwrap();
    repo.complete_routine_access_update(owner, input.slot.bot, command.request, 21)
        .unwrap();
    repo.routine_current_grant(owner, input.slot.bot, input.slot.schedule)
        .unwrap()
}

fn grant_revision_column(repo: &Repository, input: &BotRoutineCapturedSources) -> u64 {
    repo.connection
        .query_row(
            "SELECT grant_revision FROM bot_routine_preparations WHERE schedule_id=?1 AND occurrence_ms=?2",
            params![input.slot.schedule.to_storage(), input.slot.occurrence_ms],
            |r| r.get(0),
        )
        .unwrap()
}

/// Contract 4.5: the edited grant is accepted and the setup grant is refused
/// once an edit exists.
#[test]
fn routine_preparation_requires_current_grant() {
    let (mut repo, mut input) = fixture();
    let setup = input.grant.clone();
    let current = edited(&mut repo, &input);
    assert_eq!(current.grant.grant_revision, 2);
    assert_ne!(current.grant, setup);
    let before = counts(&repo);

    // The setup grant no longer describes what the Routine may do.
    assert_eq!(
        repo.prepare_routine_occurrence(&input, 60_001).err(),
        Some(BotError::Conflict)
    );
    assert_eq!(counts(&repo), before);
    // Neither does a grant that only claims the current revision.
    let mut forged = setup.clone();
    forged.grant_revision = 2;
    forged.digest = forged.canonical_digest().unwrap();
    input.grant = forged;
    assert_eq!(
        repo.prepare_routine_occurrence(&input, 60_001).err(),
        Some(BotError::Conflict)
    );
    assert_eq!(counts(&repo), before);

    input.grant = current.grant.clone();
    let draft = prepare(&mut repo, &input);
    assert_eq!(draft.capture.grant, current.grant);
    assert_eq!(draft.prepared.submission.policy, current.grant.policy);
    assert_eq!(
        draft
            .prepared
            .submission
            .provenance
            .routine
            .unwrap()
            .grant_revision,
        2
    );
    assert_eq!(grant_revision_column(&repo, &input), 2);
}

/// The repository half of
/// `routine_occurrence_after_access_edit_captures_edited_grant_and_is_claimed`:
/// after an access edit the next occurrence is prepared with the edited
/// grant, and the claim the delegate reports for it is linked.
#[test]
fn routine_occurrence_after_access_edit_captures_edited_grant_and_links_its_claim() {
    let (mut repo, mut input) = fixture();
    input.grant = edited(&mut repo, &input).grant;
    let draft = prepare(&mut repo, &input);
    // The preclaim snapshot and the tool lookup still decode the draft.
    let (row, creation, _) = repo
        .routine_preclaim_snapshot(&input.slot, draft.prepared.semantic_hash)
        .unwrap()
        .unwrap();
    assert_eq!(row, draft);
    assert_eq!(creation.identity.grant.grant_revision, 1);
    assert_eq!(row.capture.grant.grant_revision, 2);

    let record = claim(&draft);
    let linked = finalize(&mut repo, &input, &record);
    assert_eq!(linked.stage, BotRoutineStoredStage::Linked);
    assert_eq!(linked.capture.grant, input.grant);
    assert_eq!(
        repo.routine_occurrence_for_run(
            owner(&input.slot),
            input.slot.bot,
            linked.prepared.ids.run
        )
        .unwrap(),
        Some(linked.clone())
    );
    // The linked receipt is an ordinary Routine occurrence submission.
    let kind: String = repo
        .connection
        .query_row(
            "SELECT command_kind FROM bot_command_receipts WHERE request_id=?1",
            [linked.prepared.ids.submission.to_storage()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kind, "routine_occurrence");

    // An occurrence prepared before a later edit is closed at its claim, not
    // run under a grant that is no longer current.
    let mut later = input.clone();
    later.slot.occurrence_ms = 120_000;
    let stale = match repo.prepare_routine_occurrence(&later, 120_001).unwrap() {
        BotRoutinePreparationAdmission::Ready(row) => *row,
        BotRoutinePreparationAdmission::Capacity => panic!("unexpected capacity"),
    };
    let owner = owner(&input.slot);
    let command = BotRoutineAccessUpdate {
        request: BotRequestId::mint(),
        bot: input.slot.bot,
        schedule: input.slot.schedule,
        expected_grant_revision: 2,
        access: BotAccess::Default,
        selected_pages: vec![],
    };
    repo.prepare_routine_access_update(owner, &command, RoutineDigest32::from_bytes([6; 32]), 30)
        .unwrap();
    let mut record = claim(&stale);
    record.claim.claimed_at_ms = 120_123;
    record.updated_at_ms = 120_123;
    let closed = match repo
        .finalize_routine_occurrence(&record, &proof(&later), "koru-chat-v1", 120_200)
        .unwrap()
    {
        BotRoutinePreparationAdmission::Ready(row) => *row,
        BotRoutinePreparationAdmission::Capacity => panic!("unexpected capacity"),
    };
    assert_eq!(
        closed.stage,
        BotRoutineStoredStage::Closed(BotRoutineStoredClosure::ClaimedTerminal {
            reason: RoutineTerminalReason::ConfigurationChanged
        })
    );
}

/// The same occurrence under the definition a new Routine is seeded with:
/// exactly Paused(Manual). Taken from the retained seed, so this crate names
/// no Schedule type.
fn manually_paused(repo: &Repository, input: &BotRoutineCapturedSources) -> BotRoutineCapturedSources {
    let creation = repo
        .bot_routine_creation(owner(&input.slot), input.slot.bot, input.slot.schedule)
        .unwrap()
        .unwrap();
    let mut input = input.clone();
    input.definition.lifecycle = creation.identity.seed.header.definition.lifecycle;
    assert_eq!(
        BotRoutineCaptureLifecycle::of(&input.definition.lifecycle),
        Some(BotRoutineCaptureLifecycle::TestPausedManual)
    );
    input
}

/// Paused for a reason that is not the person's own pause.
fn otherwise_paused(test: &BotRoutineCapturedSources) -> BotRoutineCapturedSources {
    let manual = serde_json::to_string(&test.definition.lifecycle).unwrap();
    let other = manual.replace("manual", "scope_unavailable");
    assert_ne!(other, manual);
    let mut input = test.clone();
    input.definition.lifecycle = serde_json::from_str(&other).unwrap();
    assert_eq!(
        BotRoutineCaptureLifecycle::of(&input.definition.lifecycle),
        None
    );
    input
}

/// Guard 7: the default expectation is Active. A paused definition is
/// captured only when the caller says it expects the owner Test's.
#[test]
fn routine_preparation_default_expectation_rejects_a_paused_definition() {
    let (mut repo, active) = fixture();
    let test = manually_paused(&repo, &active);
    let before = counts(&repo);
    assert_eq!(
        repo.prepare_routine_occurrence(&test, 60_001).err(),
        Some(BotError::Invalid)
    );
    assert_eq!(
        repo.prepare_routine_occurrence_expecting(
            &test,
            BotRoutineCaptureLifecycle::Active,
            60_001
        )
        .err(),
        Some(BotError::Invalid)
    );
    // Stating the Test expectation does not admit an Active definition, or
    // one paused for any other reason.
    assert_eq!(
        repo.prepare_routine_occurrence_expecting(
            &active,
            BotRoutineCaptureLifecycle::TestPausedManual,
            60_001
        )
        .err(),
        Some(BotError::Invalid)
    );
    let unavailable = otherwise_paused(&test);
    for expected in [
        BotRoutineCaptureLifecycle::Active,
        BotRoutineCaptureLifecycle::TestPausedManual,
    ] {
        assert_eq!(
            repo.prepare_routine_occurrence_expecting(&unavailable, expected, 60_001)
                .err(),
            Some(BotError::Invalid)
        );
    }
    assert_eq!(counts(&repo), before);

    // The Test: prepared under the paused definition, read back through the
    // one decoder, and linked at its claim like any occurrence.
    let draft = match repo
        .prepare_routine_occurrence_expecting(
            &test,
            BotRoutineCaptureLifecycle::TestPausedManual,
            60_001,
        )
        .unwrap()
    {
        BotRoutinePreparationAdmission::Ready(row) => *row,
        BotRoutinePreparationAdmission::Capacity => panic!("unexpected capacity"),
    };
    assert_eq!(
        BotRoutineCaptureLifecycle::of(&draft.capture.schedule_definition.lifecycle),
        Some(BotRoutineCaptureLifecycle::TestPausedManual)
    );
    assert_eq!(
        repo.read_routine_preparation(&test.slot, draft.prepared.semantic_hash)
            .unwrap(),
        Some(draft.clone())
    );
    // A retry of the same Test returns the retained draft; the same slot
    // offered as a scheduled occurrence is still refused.
    assert!(matches!(
        repo.prepare_routine_occurrence_expecting(
            &test,
            BotRoutineCaptureLifecycle::TestPausedManual,
            60_050
        )
        .unwrap(),
        BotRoutinePreparationAdmission::Ready(row) if *row == draft
    ));
    assert_eq!(
        repo.prepare_routine_occurrence(&test, 60_050).err(),
        Some(BotError::Invalid)
    );
    let linked = finalize(&mut repo, &test, &claim(&draft));
    assert_eq!(linked.stage, BotRoutineStoredStage::Linked);
    assert_eq!(
        repo.routine_occurrence_for_run(
            owner(&test.slot),
            test.slot.bot,
            linked.prepared.ids.run
        )
        .unwrap(),
        Some(linked)
    );
}
