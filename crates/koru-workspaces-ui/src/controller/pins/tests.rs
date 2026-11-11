// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::{
    Code, Error, Timestamp, Workspace,
    service::{ListingState, PinnedWorkspace},
};

fn rows(count: usize) -> Vec<Row> {
    let owner = ProfileId::new();
    (0..count)
        .map(|position| Row {
            id: WorkspaceId::new(),
            owner,
            name: format!("Workspace {position}"),
            position: position as i64,
            authority: crate::Authority::Verified,
            ..Row::default()
        })
        .collect()
}

fn committed(row: &Row, listing: ListingState) -> PinnedWorkspace {
    let mut workspace = Workspace::new(row.owner, row.name.clone(), Timestamp::now()).unwrap();
    workspace.id = row.id;
    workspace.pinned = true;
    PinnedWorkspace { workspace, listing }
}

#[test]
fn cap_counts_pending_choices_and_unpin_still_runs_above_cap() {
    // The rail's pins less one saved, the last one pending, then one more.
    let cap = crate::RAIL_PINS;
    let mut rows = rows(cap + 2);
    for row in &mut rows[..cap - 1] {
        row.pinned = true;
    }
    let mut c = WorkspacesController::new();
    c.adopt_rows(&rows);
    c.perform(Action::TogglePinned(rows[cap - 1].id));
    assert_eq!(c.pending_pin(rows[cap - 1].id), Some(true));
    assert_eq!(
        c.reason_for(&Action::TogglePinned(rows[cap].id), &rows[cap]),
        Some(t(c.lang, Key::WorkspacePinLimit).into())
    );
    c.perform(Action::TogglePinned(rows[cap].id));
    assert!(
        c.pending_pin(rows[cap].id).is_none(),
        "dispatch bypassed the cap"
    );
    for row in &mut rows {
        row.pinned = true;
    }
    c.adopt_rows(&rows);
    c.perform(Action::TogglePinned(rows[cap + 1].id));
    assert_eq!(c.pending_pin(rows[cap + 1].id), Some(false));
    assert!(
        c.row(rows[cap].id).unwrap().pinned,
        "restore silently removed an excess pin"
    );
}

#[test]
fn reorder_sends_the_complete_profile_permutation_and_keeps_one_pending_order() {
    let mut rows = rows(6);
    for i in [0, 2, 4] {
        rows[i].pinned = true;
    }
    rows[1].archived = true;
    rows[3].deleted = true;
    let owner = rows[0].owner;
    let mut c = WorkspacesController::new();
    c.adopt_rows(&rows);
    assert!(c.reorder_before(rows[4].id, Some(rows[0].id)));
    let expected: Vec<_> = [4, 0, 1, 2, 3, 5].map(|i| rows[i].id).into();
    assert_eq!(c.pending_order(owner), Some(expected.as_slice()));
    assert!(!c.reorder_before(rows[2].id, Some(rows[0].id)));
    c.adopt_rows(&rows);
    assert_eq!(c.pending_order(owner), Some(expected.as_slice()));
    assert!(!c.reorder_before(rows[1].id, Some(rows[0].id)));
}

#[test]
fn a_pending_repair_stays_retryable_after_an_unrelated_notice_is_cleared() {
    let rows = rows(1);
    let row = &rows[0];
    let mut c = WorkspacesController::new();
    c.adopt_rows(&rows);
    c.pin_settled(
        row.id,
        Ok(committed(
            row,
            ListingState::Pending {
                cause: "storage_failed".into(),
            },
        )),
        Instant::now(),
    );
    c.perform(Action::DismissNotice);
    assert!(c.notice_for(row.owner).unwrap().is_retryable());
    c.retry_for(ProfileId::new());
    assert!(c.pending.is_empty());
    c.retry_for(row.owner);
    assert!(
        c.pending
            .values()
            .any(|pending| matches!(pending, Pending::ReconcileListing(id) if *id == row.id))
    );
}

#[test]
fn repair_attempts_are_bounded_and_keyed_then_manual_retry_waits_for_catalog() {
    let rows = rows(1);
    let row = &rows[0];
    let mut c = WorkspacesController::new();
    c.adopt_rows(&rows);
    let start = Instant::now();
    c.pin_settled(
        row.id,
        Ok(committed(
            row,
            ListingState::Pending {
                cause: "storage_failed".into(),
            },
        )),
        start,
    );
    assert!(c.notice_for(row.owner).unwrap().is_retryable());
    assert!(!c.poll_pin_repairs_at(start + Duration::from_millis(999)));
    for (attempt, second) in [1, 5, 30].into_iter().enumerate() {
        assert!(c.poll_pin_repairs_at(start + Duration::from_secs(second)));
        let key = *c.pending.keys().next().unwrap();
        assert!(
            !c.poll_pin_repairs_at(start + Duration::from_secs(60)),
            "duplicate repair while its receipt is owed"
        );
        assert!(!c.receive(Answer::Settled(
            CommandId(key.0 + 100),
            Box::new(Answer::Unavailable)
        )));
        assert!(c.pending.contains_key(&key));
        c.receive(Answer::Settled(
            key,
            Box::new(Answer::ListingReconciled(Err(Error::bare(
                Code::StorageFailed,
            )))),
        ));
        assert_eq!(c.pin_repairs[&row.id].attempts, attempt + 1);
    }
    assert!(!c.poll_pin_repairs_at(start + Duration::from_secs(300)));
    assert!(
        !c.has_pending(),
        "stopped repairs keep an idle maintenance timer alive"
    );
    c.perform(Action::Retry);
    let key = *c.pending.keys().next().unwrap();
    c.receive(Answer::Settled(
        key,
        Box::new(Answer::ListingReconciled(Ok(committed(
            row,
            ListingState::Applied,
        )
        .workspace))),
    ));
    assert!(
        c.pin_listing_pending(row.id),
        "receipt alone cleared the catalog repair"
    );
    let mut repaired = row.clone();
    repaired.pinned = true;
    c.adopt_rows(&[repaired]);
    assert!(!c.pin_listing_pending(row.id));
    assert!(c.notice().is_none());
}

#[test]
fn unknown_content_and_other_profiles_do_not_consume_this_profiles_cap() {
    let mut rows = rows(5);
    for row in &mut rows[1..] {
        row.pinned = true;
    }
    rows[1].archived = true;
    rows[2].deleted = true;
    rows[3].authority = crate::Authority::Pending;
    rows[4].owner = ProfileId::new();
    let mut c = WorkspacesController::new();
    c.adopt_rows(&rows);
    assert!(
        c.reason_for(&Action::TogglePinned(rows[0].id), &rows[0])
            .is_none()
    );
    c.perform(Action::TogglePinned(rows[0].id));
    assert_eq!(c.pending_pin(rows[0].id), Some(true));
    for row in &rows[1..4] {
        c.perform(Action::TogglePinned(row.id));
        assert!(c.pending_pin(row.id).is_none());
    }
}
