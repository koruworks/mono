// SPDX-License-Identifier: MPL-2.0
use super::super::tests::{confirm, fixture, prepared};
use super::*;

#[test]
fn listing_keyset_is_exact_bounded_and_preserves_absent_cursor_order() {
    let (mut repo, bot, owner, mut input) = fixture();
    let mut ids = Vec::new();
    for _ in 0..18 {
        input.request = BotRequestId::mint();
        ids.push(prepared(&mut repo, owner, &input).identity.schedule);
    }
    ids.sort();
    for limit in [0, 17, 255] {
        assert_eq!(
            repo.routine_list_snapshot(owner, bot.id, None, limit),
            Err(BotError::Invalid)
        );
    }
    let first = repo.routine_list_snapshot(owner, bot.id, None, 16).unwrap();
    assert_eq!(
        first
            .roster
            .items
            .iter()
            .map(|r| r.route.schedule)
            .collect::<Vec<_>>(),
        ids[..16]
    );
    assert_eq!(first.roster.next, Some(ids[15]));
    let last = repo
        .routine_list_snapshot(owner, bot.id, first.roster.next, 16)
        .unwrap();
    assert_eq!(last.roster.items.len(), 2);
    assert_eq!(last.roster.next, None);
    let absent = ScheduleId::mint();
    let page = repo
        .routine_list_snapshot(owner, bot.id, Some(absent), 16)
        .unwrap();
    assert_eq!(
        page.roster
            .items
            .iter()
            .map(|r| r.route.schedule)
            .collect::<Vec<_>>(),
        ids.into_iter()
            .filter(|id| *id > absent)
            .take(16)
            .collect::<Vec<_>>()
    );
}

#[test]
fn listing_narrow_snapshot_preserves_original_creation_page_configuration_guard() {
    let (mut repo, bot, owner, input) = fixture();
    prepared(&mut repo, owner, &input);
    repo.connection.execute("UPDATE bots SET working_folder=zeroblob(40000),folder_lock_request_id=zeroblob(100) WHERE bot_id=?1",[bot.id.to_storage()]).unwrap();
    assert!(
        repo.bot_routine_creation_page(owner, bot.id, None, 16)
            .is_err()
    );
    let snapshot = repo.routine_list_snapshot(owner, bot.id, None, 16).unwrap();
    assert_eq!(snapshot.roster.items.len(), 1);
    let (_, checkpoint, _) = repo.routine_list_entry(&snapshot, 0).unwrap();
    repo.validate_routine_list(&snapshot, &[checkpoint])
        .unwrap();
}

#[test]
fn listing_invalid_and_oversized_models_are_unavailable_without_hiding_roster() {
    for sql in [
        "model=zeroblob(1000000)",
        "model=' '",
        "provider='invalid provider'",
        "provider='openai'",
        "reasoning='unknown'",
        "reasoning=zeroblob(999)",
    ] {
        let (mut repo, bot, owner, input) = fixture();
        prepared(&mut repo, owner, &input);
        repo.connection
            .execute(
                &format!("UPDATE bots SET {sql} WHERE bot_id=?1"),
                [bot.id.to_storage()],
            )
            .unwrap();
        let snapshot = repo.routine_list_snapshot(owner, bot.id, None, 16).unwrap();
        assert_eq!(snapshot.model, BotRoutineModelObservation::Unavailable {});
        assert_eq!(snapshot.roster.items.len(), 1);
    }
}

#[test]
fn listing_common_metadata_corruption_foreign_owner_and_trash_refuse_page() {
    let (mut repo, bot, owner, input) = fixture();
    prepared(&mut repo, owner, &input);
    assert_eq!(
        repo.routine_list_snapshot(
            BotOwner {
                device: DeviceId::new(),
                ..owner
            },
            bot.id,
            None,
            16
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
        repo.routine_list_snapshot(owner, bot.id, None, 16),
        Err(BotError::NotFound)
    );
    repo.connection
        .execute("PRAGMA ignore_check_constraints=ON", [])
        .unwrap();
    for column in [
        "user_scope_id",
        "owner_device_id",
        "home_workspace_id",
        "revision",
        "policy_revision",
        "access",
    ] {
        let (mut r, b, o, c) = fixture();
        prepared(&mut r, o, &c);
        r.connection
            .execute("PRAGMA ignore_check_constraints=ON", [])
            .unwrap();
        r.connection
            .execute(
                &format!("UPDATE bots SET {column}=zeroblob(100000) WHERE bot_id=?1"),
                [b.id.to_storage()],
            )
            .unwrap();
        assert!(
            r.routine_list_snapshot(o, b.id, None, 16).is_err(),
            "{column}"
        );
    }
}

#[test]
fn listing_creation_stages_never_invent_live_or_execution_history() {
    for transition in 0..4 {
        let (mut repo, bot, owner, input) = fixture();
        let c = prepared(&mut repo, owner, &input);
        let expected = match transition {
            0 => BotRoutineCreationState::Prepared,
            1 => {
                repo.transition_bot_routine_creation(
                    &c.identity,
                    c.observed(),
                    BotRoutineCreationTransition::Unconfirmed,
                    11,
                )
                .unwrap();
                BotRoutineCreationState::Unconfirmed
            }
            2 => {
                repo.transition_bot_routine_creation(&c.identity, c.observed(), confirm(&c), 11)
                    .unwrap();
                BotRoutineCreationState::Bound
            }
            _ => {
                repo.transition_bot_routine_creation(
                    &c.identity,
                    c.observed(),
                    BotRoutineCreationTransition::Conflict(
                        BotRoutineCreationRefusal::DocumentChanged,
                    ),
                    11,
                )
                .unwrap();
                BotRoutineCreationState::Conflict
            }
        };
        let snapshot = repo.routine_list_snapshot(owner, bot.id, None, 16).unwrap();
        let (_, checkpoint, item) = repo.routine_list_entry(&snapshot, 0).unwrap();
        assert_eq!(item.creation.state, expected);
        assert_eq!(item.live, BotRoutineLiveObservation::Unavailable {});
        assert_eq!(
            item.local_conversations,
            if expected == BotRoutineCreationState::Bound {
                BotRoutineLocalConversations::Available {
                    items: vec![],
                    more: false,
                }
            } else {
                BotRoutineLocalConversations::Unavailable {}
            }
        );
        repo.validate_routine_list(&snapshot, &[checkpoint])
            .unwrap();
    }
}

#[test]
fn listing_grant_is_retained_and_full_identity_corruption_is_not_empty() {
    let (mut repo, bot, owner, input) = fixture();
    let c = prepared(&mut repo, owner, &input);
    repo.connection
        .execute(
            "UPDATE bots SET access='workspace',policy_revision=policy_revision+1 WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    let snapshot = repo.routine_list_snapshot(owner, bot.id, None, 16).unwrap();
    let (_, _, item) = repo.routine_list_entry(&snapshot, 0).unwrap();
    assert_eq!(item.grant.access, BotAccess::Default);
    assert_eq!(
        item.grant.source_bot_policy_revision,
        c.identity.grant.source_bot_policy_revision
    );
    repo.connection
        .execute(
            "UPDATE bot_routines SET seed_hash=zeroblob(32) WHERE schedule_id=?1",
            [c.identity.schedule.to_storage()],
        )
        .unwrap();
    assert!(repo.routine_list_entry(&snapshot, 0).is_err());
}

#[test]
fn listing_final_validation_detects_same_revision_identity_vv_receipt_and_metadata_drift() {
    for sql in [
        "UPDATE bot_routines SET store_request_id='00000000-0000-4000-8000-000000000009'",
        "UPDATE bot_routines SET origin_edit_revision=x'090909'",
        "UPDATE bot_command_receipts SET expected_revision=expected_revision+1 WHERE command_kind='create_routine'",
        "UPDATE bots SET reasoning='off'",
        "UPDATE chats SET profile_id='00000000-0000-4000-8000-000000000009'",
    ] {
        let (mut repo, bot, owner, input) = fixture();
        let c = prepared(&mut repo, owner, &input);
        repo.transition_bot_routine_creation(&c.identity, c.observed(), confirm(&c), 11)
            .unwrap();
        let snapshot = repo.routine_list_snapshot(owner, bot.id, None, 16).unwrap();
        let (_, checkpoint, _) = repo.routine_list_entry(&snapshot, 0).unwrap();
        repo.connection.execute(sql, []).unwrap();
        assert!(
            repo.validate_routine_list(&snapshot, &[checkpoint])
                .is_err(),
            "{sql}"
        );
    }
}
