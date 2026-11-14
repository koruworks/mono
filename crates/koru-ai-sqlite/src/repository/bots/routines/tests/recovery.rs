// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn routine_recovery_page_is_narrow_bounded_and_advances_past_corrupt_identity() {
    let (mut repo, bot, owner, mut command) = fixture();
    assert!(!repo.bot_routine_recovery_needed().unwrap());
    let mut creations = Vec::new();
    for _ in 0..18 {
        command.request = BotRequestId::mint();
        creations.push(prepared(&mut repo, owner, &command));
    }
    // Inventory never decodes a seed/grant, including oversized corrupt bytes.
    repo.connection
        .pragma_update(None, "ignore_check_constraints", "ON")
        .unwrap();
    repo.connection
        .execute(
            "UPDATE bot_routines SET seed_json=zeroblob(5000000) WHERE schedule_id=?1",
            [creations[0].identity.schedule.to_storage()],
        )
        .unwrap();
    repo.connection
        .execute(
            "UPDATE bot_routines SET creation_state='broken' WHERE schedule_id=?1",
            [creations[1].identity.schedule.to_storage()],
        )
        .unwrap();
    let first = repo.bot_routine_recovery_page(owner, None).unwrap();
    assert_eq!(first.entries.len(), 16);
    assert!(first.entries[0].is_ok());
    assert!(first.entries[1].is_err());
    assert!(
        repo.bot_routine_recovery_candidate(owner, first.entries[0].as_ref().unwrap())
            .is_err()
    );
    let cursor = first.next.unwrap();
    command.request = BotRequestId::mint();
    prepared(&mut repo, owner, &command);
    let second = repo.bot_routine_recovery_page(owner, Some(cursor)).unwrap();
    assert_eq!(second.entries.len(), 2);
    assert!(second.next.is_none());
    assert!(repo.bot_routine_recovery_needed().unwrap());
    let valid = second.entries[0].as_ref().unwrap();
    let (creation, _) = repo.bot_routine_recovery_candidate(owner, valid).unwrap();
    assert_eq!(creation.identity.bot, bot.id);
    let mut foreign = cursor;
    foreign.owner.device = DeviceId::new();
    assert!(
        repo.bot_routine_recovery_page(owner, Some(foreign))
            .is_err()
    );
}

#[test]
fn routine_recovery_candidate_rechecks_config_profile_token_and_terminal_state() {
    for mutation in 0..4 {
        let (mut repo, bot, owner, command) = fixture();
        let c = prepared(&mut repo, owner, &command);
        let entry = repo
            .bot_routine_recovery_page(owner, None)
            .unwrap()
            .entries
            .remove(0)
            .unwrap();
        assert!(repo.bot_routine_recovery_candidate(owner, &entry).is_ok());
        match mutation {
            0 => {
                repo.connection
                    .execute(
                        "UPDATE bots SET revision=revision+1 WHERE bot_id=?1",
                        [bot.id.to_storage()],
                    )
                    .unwrap();
            }
            1 => {
                repo.connection
                    .execute(
                        "UPDATE chats SET profile_id=?1 WHERE id=?2",
                        params![ProfileId::new().to_storage(), bot.main_chat.to_storage()],
                    )
                    .unwrap();
            }
            2 => {
                repo.transition_bot_routine_creation(
                    &c.identity,
                    c.observed(),
                    BotRoutineCreationTransition::Unconfirmed,
                    11,
                )
                .unwrap();
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
            }
        }
        assert!(repo.bot_routine_recovery_candidate(owner, &entry).is_err());
        if mutation < 2 {
            let conflict = repo
                .reject_changed_bot_routine_creation(owner, &entry, 12)
                .unwrap();
            assert_eq!(conflict.identity, c.identity);
            assert_eq!(conflict.state, BotRoutineCreationState::Conflict);
            assert_eq!(
                conflict.refusal,
                Some(BotRoutineCreationRefusal::AuthorityChanged)
            );
            let old = repo
                .prepare_bot_routine_creation(owner, &command, hash(), 99)
                .unwrap();
            assert_eq!(old, receipt_for(&conflict));
            assert!(
                repo.reject_changed_bot_routine_creation(owner, &entry, 100)
                    .is_err()
            );
        } else {
            assert!(
                repo.reject_changed_bot_routine_creation(owner, &entry, 12)
                    .is_err()
            );
        }
    }
}

#[test]
fn routine_canonical_uncertainty_never_becomes_terminal_disagreement() {
    for mutation in 0..3 {
        let (mut repo, bot, owner, command) = fixture();
        let c = prepared(&mut repo, owner, &command);
        let entry = repo
            .bot_routine_recovery_page(owner, None)
            .unwrap()
            .entries
            .remove(0)
            .unwrap();
        repo.connection
            .pragma_update(None, "ignore_check_constraints", "ON")
            .unwrap();
        match mutation {
            0 => {
                repo.connection.execute("UPDATE bots SET revision=revision+1,working_folder='relative' WHERE bot_id=?1", [bot.id.to_storage()]).unwrap();
            }
            1 => {
                repo.connection
                    .execute(
                        "UPDATE chats SET profile_id=NULL WHERE id=?1",
                        [bot.main_chat.to_storage()],
                    )
                    .unwrap();
            }
            _ => {
                repo.connection
                    .execute(
                        "UPDATE bots SET owner_device_id=?1 WHERE bot_id=?2",
                        params![DeviceId::new().to_storage(), bot.id.to_storage()],
                    )
                    .unwrap();
            }
        }
        assert!(
            repo.reject_changed_bot_routine_creation(owner, &entry, 12)
                .is_err()
        );
        let state: String = repo
            .connection
            .query_row(
                "SELECT creation_state FROM bot_routines WHERE schedule_id=?1",
                [c.identity.schedule.to_storage()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "prepared");
        let stage: String = repo
            .connection
            .query_row(
                "SELECT stage FROM bot_command_receipts WHERE request_id=?1",
                [command.request.to_storage()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stage, "prepared");
    }
}
