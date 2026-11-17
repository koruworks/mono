// SPDX-License-Identifier: MPL-2.0
//! Cohort `m8_team_pause_row`: the team pause row and its commands, on an
//! in-memory ai.db. No worker, delegate, Store or provider.
use super::*;

const PAUSE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RESUME: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OTHER: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn owner() -> BotOwner {
    BotOwner {
        user_scope: UserScopeId::new(),
        device: DeviceId::from_uuid(Uuid::new_v4()),
    }
}

fn changed(change: TeamPauseChange) -> BotTeamPauseRow {
    match change {
        TeamPauseChange::Changed(row) => row,
        other => panic!("the row was not written: {other:?}"),
    }
}

fn stored_rows(repo: &Repository) -> i64 {
    repo.connection
        .query_row("SELECT count(*) FROM bot_team_pause", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn m8_team_pause_row_a_team_never_paused_reads_as_revision_zero_and_needs_no_row() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owner();
    assert_eq!(repo.team_pause(owner).unwrap(), None);
    let view = repo.team_pause_view(owner).unwrap();
    assert_eq!(view, TeamPauseView::never_paused(owner.device));
    assert_eq!(
        (
            view.paused,
            view.revision,
            view.stage,
            view.pending_since_ms
        ),
        (false, 0, TeamPauseStage::Acknowledged, None)
    );
    let barrier = repo.team_pause_barrier(owner).unwrap();
    assert_eq!(
        (barrier.epoch, barrier.paused, barrier.request),
        (0, false, None)
    );
    assert!(barrier.valid());
    assert_eq!(barrier.owner.device, owner.device);
    // A resume of a team never paused changes nothing and writes no row.
    let resumed = repo
        .change_team_pause(owner, BotRequestId::mint(), RESUME, 0, false, 5)
        .unwrap();
    assert_eq!(resumed, TeamPauseChange::Unchanged(None));
    assert_eq!(resumed.view(owner), view);
    assert!(resumed.changed().is_none());
    assert_eq!(stored_rows(&repo), 0);
    // The echo of "never paused" needs no record.
    assert_eq!(
        repo.acknowledge_team_pause(owner, &barrier, 6).unwrap(),
        None
    );
    assert_eq!(stored_rows(&repo), 0);
}

#[test]
fn m8_team_pause_row_pause_then_resume_move_the_revision_by_one_and_wait_for_the_echo() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owner();
    let request = BotRequestId::mint();
    let change = repo
        .change_team_pause(owner, request, PAUSE, 0, true, 10)
        .unwrap();
    assert_eq!(change.changed().map(|row| row.revision), Some(1));
    let paused = changed(change);
    assert_eq!(
        paused,
        BotTeamPauseRow {
            owner,
            paused: true,
            revision: 1,
            request,
            fingerprint: PAUSE.into(),
            stage: TeamPauseStage::Pending,
            changed_at_ms: 10,
            acknowledged_at_ms: None,
        }
    );
    assert_eq!(repo.team_pause(owner).unwrap(), Some(paused.clone()));
    let view = paused.view();
    assert_eq!(
        (
            view.owner_device,
            view.paused,
            view.revision,
            view.stage,
            view.pending_since_ms
        ),
        (owner.device, true, 1, TeamPauseStage::Pending, Some(10))
    );
    let barrier = paused.barrier();
    assert_eq!(
        (barrier.epoch, barrier.paused, barrier.request),
        (1, true, Some(request))
    );
    assert!(barrier.valid());

    // Only an echo equal to the row acknowledges it.
    let mut other = barrier.clone();
    other.paused = false;
    assert_eq!(
        repo.acknowledge_team_pause(owner, &other, 11).unwrap(),
        Some(paused.clone())
    );
    let mut older = barrier.clone();
    older.epoch = 0;
    older.request = None;
    older.paused = false;
    assert_eq!(
        repo.acknowledge_team_pause(owner, &older, 11).unwrap(),
        Some(paused.clone())
    );
    let acknowledged = repo
        .acknowledge_team_pause(owner, &barrier, 12)
        .unwrap()
        .unwrap();
    assert_eq!(acknowledged.stage, TeamPauseStage::Acknowledged);
    assert_eq!(acknowledged.acknowledged_at_ms, Some(12));
    assert_eq!(acknowledged.view().pending_since_ms, None);
    // A second echo, from another delegate run, changes nothing.
    assert_eq!(
        repo.acknowledge_team_pause(owner, &barrier, 99).unwrap(),
        Some(acknowledged.clone())
    );

    // Resume: the next revision, pending again.
    let resume = BotRequestId::mint();
    let resumed = changed(
        repo.change_team_pause(owner, resume, RESUME, 1, false, 20)
            .unwrap(),
    );
    assert_eq!(
        (
            resumed.paused,
            resumed.revision,
            resumed.stage,
            resumed.acknowledged_at_ms
        ),
        (false, 2, TeamPauseStage::Pending, None)
    );
    // The echo of the earlier pause no longer acknowledges anything.
    assert_eq!(
        repo.acknowledge_team_pause(owner, &barrier, 21).unwrap(),
        Some(resumed.clone())
    );
    assert_eq!(stored_rows(&repo), 1);
}

#[test]
fn m8_team_pause_row_a_repeat_is_answered_from_the_row_and_other_content_conflicts() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owner();
    let request = BotRequestId::mint();
    let paused = changed(
        repo.change_team_pause(owner, request, PAUSE, 0, true, 10)
            .unwrap(),
    );
    // The same request again, even with the revision it first named.
    for expected in [0, 1, 7] {
        assert_eq!(
            repo.change_team_pause(owner, request, PAUSE, expected, true, 50)
                .unwrap(),
            TeamPauseChange::Replayed(paused.clone())
        );
    }
    // The same id with other content.
    assert_eq!(
        repo.change_team_pause(owner, request, OTHER, 0, true, 50),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.change_team_pause(owner, request, PAUSE, 0, false, 50),
        Err(BotError::Conflict)
    );
    assert_eq!(BotError::Conflict.code(), "request_conflict");
    assert_eq!(repo.team_pause(owner).unwrap(), Some(paused.clone()));

    // A wrong expected revision, including a retry of an older request
    // after the row moved on: the caller reads again.
    for expected in [0, 2, 9] {
        assert_eq!(
            repo.change_team_pause(owner, BotRequestId::mint(), RESUME, expected, false, 50),
            Err(BotError::TeamRevisionConflict)
        );
    }
    assert_eq!(
        BotError::TeamRevisionConflict.code(),
        "team_revision_conflict"
    );
    let resumed = changed(
        repo.change_team_pause(owner, BotRequestId::mint(), RESUME, 1, false, 60)
            .unwrap(),
    );
    assert_eq!(
        repo.change_team_pause(owner, request, PAUSE, 0, true, 70),
        Err(BotError::TeamRevisionConflict),
        "the older request finds another id on the row and another revision"
    );
    assert_eq!(repo.team_pause(owner).unwrap(), Some(resumed));

    // A malformed fingerprint is refused before anything is read.
    assert_eq!(
        repo.change_team_pause(owner, BotRequestId::mint(), "short", 2, true, 80),
        Err(BotError::Invalid)
    );
}

#[test]
fn m8_team_pause_row_pause_when_already_paused_answers_the_view_and_writes_nothing() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owner();
    let paused = changed(
        repo.change_team_pause(owner, BotRequestId::mint(), PAUSE, 0, true, 10)
            .unwrap(),
    );
    let again = repo
        .change_team_pause(owner, BotRequestId::mint(), PAUSE, 1, true, 99)
        .unwrap();
    assert_eq!(again, TeamPauseChange::Unchanged(Some(paused.clone())));
    assert_eq!(again.view(owner), paused.view());
    assert!(again.changed().is_none());
    assert_eq!(repo.team_pause(owner).unwrap(), Some(paused));
    // Two windows sent PauseTeam with the same expected revision: one won,
    // the other is told the revision moved.
    assert_eq!(
        repo.change_team_pause(owner, BotRequestId::mint(), PAUSE, 0, true, 99),
        Err(BotError::TeamRevisionConflict)
    );
}

#[test]
fn m8_team_pause_row_each_person_and_computer_has_its_own_row() {
    let mut repo = Repository::in_memory().unwrap();
    let first = owner();
    let other_person = BotOwner {
        user_scope: UserScopeId::new(),
        device: first.device,
    };
    let other_computer = BotOwner {
        user_scope: first.user_scope,
        device: DeviceId::from_uuid(Uuid::new_v4()),
    };
    changed(
        repo.change_team_pause(first, BotRequestId::mint(), PAUSE, 0, true, 10)
            .unwrap(),
    );
    for other in [other_person, other_computer] {
        assert_eq!(repo.team_pause(other).unwrap(), None);
        assert!(!team_paused_on(&repo.connection, other).unwrap());
    }
    assert!(team_paused_on(&repo.connection, first).unwrap());
    let nil = BotOwner {
        user_scope: first.user_scope,
        device: DeviceId::from_uuid(Uuid::nil()),
    };
    assert_eq!(
        repo.change_team_pause(nil, BotRequestId::mint(), PAUSE, 0, true, 10),
        Err(BotError::Invalid)
    );
}

/// The delegate kept running while ai.db was restored to an older revision:
/// it answers `bot_team_stale` with the epoch it holds, and the worker
/// writes its row past it, keeping its own state.
#[test]
fn m8_team_pause_row_a_stale_epoch_after_a_restore_is_written_past() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owner();
    // No row at all, and the delegate holds epoch 6.
    let request = BotRequestId::mint();
    let row = repo
        .advance_team_pause_past(owner, 6, request, OTHER, 30)
        .unwrap();
    assert_eq!(
        (row.paused, row.revision, row.stage, row.request),
        (false, 7, TeamPauseStage::Pending, request)
    );
    assert_eq!(row.barrier().epoch, 7);
    // Already past it: answered unchanged, nothing written.
    assert_eq!(
        repo.advance_team_pause_past(owner, 3, BotRequestId::mint(), OTHER, 31)
            .unwrap(),
        row
    );
    // A paused row keeps its own `paused`.
    let paused = changed(
        repo.change_team_pause(owner, BotRequestId::mint(), PAUSE, 7, true, 40)
            .unwrap(),
    );
    assert_eq!(paused.revision, 8);
    let past = repo
        .advance_team_pause_past(owner, 20, BotRequestId::mint(), OTHER, 41)
        .unwrap();
    assert_eq!(
        (past.paused, past.revision, past.stage),
        (true, 21, TeamPauseStage::Pending)
    );
    assert!(team_paused_on(&repo.connection, owner).unwrap());
    // The revision never decreases, whatever is asked.
    assert!(
        repo.connection
            .execute(
                "UPDATE bot_team_pause SET revision=0 WHERE owner_device_id=?1",
                [owner.device.to_storage()]
            )
            .is_err(),
        "the table refuses a revision below 1"
    );
}

/// A stored value the table's checks cannot exclude is an error, never read
/// as "not paused".
#[test]
fn m8_team_pause_row_a_malformed_stored_row_fails_closed() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owner();
    changed(
        repo.change_team_pause(owner, BotRequestId::mint(), PAUSE, 0, true, 10)
            .unwrap(),
    );
    repo.connection
        .execute(
            "UPDATE bot_team_pause SET request_id=?2 WHERE owner_device_id=?1",
            params![owner.device.to_storage(), "z".repeat(36)],
        )
        .unwrap();
    assert_eq!(repo.team_pause(owner), Err(BotError::Storage));
    assert_eq!(
        team_paused_on(&repo.connection, owner),
        Err(BotError::Storage)
    );
    assert_eq!(
        repo.change_team_pause(owner, BotRequestId::mint(), RESUME, 1, false, 20),
        Err(BotError::Storage)
    );
}
