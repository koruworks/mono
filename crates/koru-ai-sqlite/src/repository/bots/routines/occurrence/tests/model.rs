// SPDX-License-Identifier: MPL-2.0
//! Same full authority decoders, with deliberately narrower current metadata.
use super::*;

fn materialized() -> (
    Repository,
    BotRoutineCapturedSources,
    BotRoutineStoredPreparation,
) {
    let (mut repo, input) = fixture();
    let draft = prepare(&mut repo, &input);
    let row = finalize(&mut repo, &input, &claim(&draft));
    (repo, input, row)
}

#[test]
fn routine_tool_subject_ignores_current_execution_fields_but_old_readers_stay_strict() {
    let (repo, input, row) = materialized();
    let owner = owner(&input.slot);
    let before = repo
        .routine_tool_subject(owner, input.slot.bot, row.prepared.ids.run)
        .unwrap();
    let count = counts(&repo);
    repo.connection.execute("UPDATE bots SET working_folder=zeroblob(1048577),model=zeroblob(1048577) WHERE bot_id=?1", [input.slot.bot.to_storage()]).unwrap();
    assert_eq!(
        repo.routine_tool_subject(owner, input.slot.bot, row.prepared.ids.run)
            .unwrap(),
        before
    );
    assert_eq!(
        repo.bot_readable_submission_for_run(owner, input.slot.bot, row.prepared.ids.run),
        Err(BotError::Storage)
    );
    assert_eq!(
        repo.routine_occurrence_for_run(owner, input.slot.bot, row.prepared.ids.run),
        Err(BotError::Storage)
    );
    assert_eq!(counts(&repo), count);
}

#[test]
fn routine_tool_subject_rejects_corrupt_receipt_or_unlinked_routine() {
    for mutation in [
        "UPDATE bot_command_receipts SET submission_json='{}' WHERE command_kind='routine_occurrence'",
        "UPDATE bot_command_receipts SET submission_json=zeroblob(4194305) WHERE command_kind='routine_occurrence'",
        "UPDATE bot_command_receipts SET submission_version=1 WHERE command_kind='routine_occurrence'",
        "UPDATE bot_routine_preparations SET capture_json='{}'",
        "UPDATE bot_routine_preparations SET stage='prepared'",
        "UPDATE bot_routine_preparations SET linked_request_id=NULL",
        "DELETE FROM bot_chat_runs",
    ] {
        let (repo, input, row) = materialized();
        // Deliberate damaged retained evidence, not a supported writer mutation.
        repo.connection
            .pragma_update(None, "ignore_check_constraints", "ON")
            .unwrap();
        repo.connection
            .pragma_update(None, "foreign_keys", "OFF")
            .unwrap();
        repo.connection.execute(mutation, []).unwrap();
        assert!(
            repo.routine_tool_subject(owner(&input.slot), input.slot.bot, row.prepared.ids.run)
                .is_err(),
            "{mutation}"
        );
    }
    let (repo, input, row) = materialized();
    let mut foreign = owner(&input.slot);
    foreign.device = koru_delegate_core::DeviceId::new();
    assert_eq!(
        repo.routine_tool_subject(foreign, input.slot.bot, row.prepared.ids.run),
        Err(BotError::Forbidden)
    );
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='trashed' WHERE bot_id=?1",
            [input.slot.bot.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.routine_tool_subject(owner(&input.slot), input.slot.bot, row.prepared.ids.run),
        Err(BotError::NotFound)
    );
}

#[test]
fn routine_tool_creation_replay_keeps_exact_body_and_subject() {
    let (repo, input, _) = materialized();
    let owner = owner(&input.slot);
    let request = input.creation.creation_request;
    let original = repo
        .routine_tool_creation_by_request(owner, input.slot.bot, request)
        .unwrap()
        .unwrap();
    repo.connection.execute("UPDATE bots SET working_folder=zeroblob(1048577),model=zeroblob(1048577),revision=revision+1 WHERE bot_id=?1", [input.slot.bot.to_storage()]).unwrap();
    assert_eq!(
        repo.routine_tool_creation_by_request(owner, input.slot.bot, request)
            .unwrap(),
        Some(original)
    );
    assert_eq!(
        repo.bot_routine_creation_by_request(owner, input.slot.bot, request),
        Err(BotError::Storage)
    );
    let mut other = owner;
    other.device = koru_delegate_core::DeviceId::new();
    assert_eq!(
        repo.routine_tool_creation_by_request(other, input.slot.bot, request),
        Err(BotError::Forbidden)
    );
    assert_eq!(
        repo.routine_tool_creation_by_request(owner, input.slot.bot, BotRequestId::mint())
            .unwrap(),
        None
    );
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET outcome_json='{}' WHERE command_kind='create_routine'",
            [],
        )
        .unwrap();
    assert!(
        repo.routine_tool_creation_by_request(owner, input.slot.bot, request)
            .is_err()
    );
}
