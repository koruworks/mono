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
    }
}
