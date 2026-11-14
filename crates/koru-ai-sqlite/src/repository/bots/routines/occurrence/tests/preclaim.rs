// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn preclaim_snapshot_uses_shared_decoders_without_current_execution_fields() {
    for (folder, model) in [
        ("zeroblob(1048577)", "zeroblob(1048577)"),
        ("replace(hex(zeroblob(524289)),'0','x')", "'invalid model'"),
    ] {
        let (mut repo, input) = fixture();
        let draft = prepare(&mut repo, &input);
        let before = repo
            .routine_preclaim_snapshot(&input.slot, draft.prepared.semantic_hash)
            .unwrap()
            .unwrap();
        let original_counts = counts(&repo);
        repo.connection
            .execute(
                &format!("UPDATE bots SET working_folder={folder},model={model} WHERE bot_id=?1"),
                [input.slot.bot.to_storage()],
            )
            .unwrap();
        assert_eq!(
            repo.routine_preclaim_snapshot(&input.slot, draft.prepared.semantic_hash)
                .unwrap(),
            Some(before)
        );
        // Ordinary readers retain their existing scalar-type guard. They do
        // not acquire the new projection's deliberate narrower semantics.
        if folder.starts_with("zeroblob") {
            assert_eq!(
                repo.read_routine_preparation(&input.slot, draft.prepared.semantic_hash),
                Err(BotError::Storage)
            );
            assert_eq!(
                repo.bot_routine_creation(owner(&input.slot), input.slot.bot, input.slot.schedule),
                Err(BotError::Storage)
            );
        } else {
            assert_eq!(
                repo.read_routine_preparation(&input.slot, draft.prepared.semantic_hash)
                    .unwrap(),
                Some(draft.clone())
            );
        }
        assert!(
            repo.finalize_routine_occurrence(
                &claim(&draft),
                &proof(&input),
                "koru-chat-v1",
                60_200
            )
            .is_err()
        );
        assert_eq!(counts(&repo), original_counts);
    }
}

#[test]
fn preclaim_snapshot_rejects_corrupt_binding_and_authority_identity() {
    let (mut repo, input) = fixture();
    let draft = prepare(&mut repo, &input);
    let (retained, creation, metadata) = repo
        .routine_preclaim_snapshot(&input.slot, draft.prepared.semantic_hash)
        .unwrap()
        .unwrap();
    assert_eq!(retained, draft);
    assert_eq!(creation.identity.route(), input.creation);
    assert_eq!(creation.identity.grant, input.grant);
    assert_eq!(metadata.owner, owner(&input.slot));
    assert_eq!(metadata.home_workspace, input.canonical.home);
    assert_eq!(
        repo.routine_preclaim_snapshot(&input.slot, RoutineDigest32::from_bytes([99; 32])),
        Err(BotError::Conflict)
    );
    let mut other = input.slot;
    other.owner.device = koru_delegate_core::DeviceId::new();
    assert_eq!(
        repo.routine_preclaim_snapshot(&other, draft.prepared.semantic_hash),
        Err(BotError::Forbidden)
    );
    other = input.slot;
    other.occurrence_ms += 60_000;
    assert_eq!(
        repo.routine_preclaim_snapshot(&other, draft.prepared.semantic_hash),
        Ok(None)
    );
    other.bot = BotId::from_uuid(uuid::Uuid::nil());
    assert_eq!(
        repo.routine_preclaim_snapshot(&other, draft.prepared.semantic_hash),
        Err(BotError::Invalid)
    );
    finalize(&mut repo, &input, &claim(&draft));
    assert_eq!(
        repo.routine_preclaim_snapshot(&input.slot, draft.prepared.semantic_hash),
        Ok(None)
    );

    let (mut repo, input) = fixture();
    let draft = prepare(&mut repo, &input);
    let mut terminal = claim(&draft);
    terminal.link_state = RoutineLinkState::Terminal {
        reason: RoutineTerminalReason::Stopped,
    };
    finalize(&mut repo, &input, &terminal);
    assert_eq!(
        repo.routine_preclaim_snapshot(&input.slot, draft.prepared.semantic_hash),
        Ok(None)
    );

    for statement in [
        "UPDATE bot_routines SET origin_edit_revision=x''",
        "UPDATE bot_routines SET grant_digest=zeroblob(32)",
        "UPDATE bot_routines SET seed_hash=zeroblob(32)",
        "UPDATE bot_routines SET owner_device_id='00000000-0000-4000-8000-000000000001'",
        "UPDATE bot_routines SET workspace_id='00000000-0000-4000-8000-000000000001'",
        "UPDATE bot_routines SET creation_request_id='00000000-0000-4000-8000-000000000001'",
        "UPDATE bot_command_receipts SET fingerprint='broken' WHERE command_kind='create_routine'",
        "UPDATE bot_routine_preparations SET preparation_hash=zeroblob(32)",
    ] {
        let (mut repo, input) = fixture();
        let draft = prepare(&mut repo, &input);
        // Deliberate damaged in-memory evidence, beyond the normal writer.
        repo.connection
            .pragma_update(None, "foreign_keys", "OFF")
            .unwrap();
        repo.connection
            .pragma_update(None, "ignore_check_constraints", "ON")
            .unwrap();
        repo.connection.execute(statement, []).unwrap();
        assert!(
            repo.routine_preclaim_snapshot(&input.slot, draft.prepared.semantic_hash)
                .is_err(),
            "{statement}"
        );
    }
}
