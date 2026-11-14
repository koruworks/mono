// SPDX-License-Identifier: MPL-2.0
//! Repository proofs use synthetic trusted capture/claim evidence; actual Store
//! and private dispatch evidence are separate integration layers.
use super::*;
use koru_delegate_core::{DocumentVersionId, RoutineClaimIdentity};

fn fixture() -> (Repository, BotRoutineCapturedSources) {
    let (mut repo, bot, owner, command) = super::super::tests::fixture();
    let creation = super::super::tests::prepared(&mut repo, owner, &command);
    repo.transition_bot_routine_creation(
        &creation.identity,
        creation.observed(),
        super::super::tests::confirm(&creation),
        11,
    )
    .unwrap();
    let slot = RoutineSlot {
        owner: creation.identity.owner,
        bot: bot.id,
        schedule: creation.identity.schedule,
        occurrence_ms: 60_000,
    };
    let (identity, canonical, profile) = repo.routine_occurrence_source(&slot).unwrap();
    let mut definition = identity.seed.header.definition.clone();
    definition.revision = 2;
    // The neutral enum is already an existing dependency of AI core; decode
    // its ordinary spelling without adding a SQLite Schedule edge.
    definition.lifecycle = serde_json::from_str("\"active\"").unwrap();
    let role = BotKnowledgeCapture {
        page: canonical.config.role.clone(),
        version: DocumentVersionId::new(),
    };
    let task = command
        .documents
        .iter()
        .map(|document| BotKnowledgeCapture {
            page: BotPage {
                workspace: canonical.home,
                document: *document,
            },
            version: DocumentVersionId::new(),
        })
        .collect();
    let input = BotRoutineCapturedSources {
        slot,
        definition,
        owner_handoff: None,
        creation: identity.route(),
        canonical,
        profile,
        grant: identity.grant,
        role,
        knowledge: vec![],
        task,
        memory: BotCapturedMemory::Absent {},
        index_digest: [1; 32],
    };
    (repo, input)
}
fn prepare(
    repo: &mut Repository,
    input: &BotRoutineCapturedSources,
) -> BotRoutineStoredPreparation {
    match repo.prepare_routine_occurrence(input, 60_001).unwrap() {
        BotRoutinePreparationAdmission::Ready(row) => *row,
        BotRoutinePreparationAdmission::Capacity => panic!("unexpected capacity"),
    }
}
fn claim(row: &BotRoutineStoredPreparation) -> RoutineClaimRecord {
    RoutineClaimRecord {
        claim: RoutineClaimIdentity {
            slot: row.prepared.slot,
            definition: row.prepared.definition,
            ids: row.prepared.ids,
            preparation_hash: row.prepared.preparation_hash,
            claimed_at_ms: 60_123,
        },
        submission_version: 2,
        submission: row.prepared.submission.clone(),
        link_state: RoutineLinkState::Held {},
        updated_at_ms: 60_123,
    }
}
fn proof(input: &BotRoutineCapturedSources) -> BotRoutineFinalizeAuthority {
    BotRoutineFinalizeAuthority {
        owner: input.slot.owner,
        home: input.canonical.home,
        profile: input.profile,
    }
}
fn finalize(
    repo: &mut Repository,
    input: &BotRoutineCapturedSources,
    record: &RoutineClaimRecord,
) -> BotRoutineStoredPreparation {
    match repo
        .finalize_routine_occurrence(record, &proof(input), "koru-chat-v1", 60_200)
        .unwrap()
    {
        BotRoutinePreparationAdmission::Ready(row) => *row,
        BotRoutinePreparationAdmission::Capacity => panic!("unexpected capacity"),
    }
}
fn counts(repo: &Repository) -> Vec<u64> {
    [
        "bot_command_receipts",
        "bot_routine_preparations",
        "chats",
        "messages",
        "inference_attempts",
        "bot_chat_runs",
        "bot_library_captures",
    ]
    .into_iter()
    .map(|table| {
        repo.connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    })
    .collect()
}

#[test]
fn occurrence_worker_metadata_guard_ignores_model_but_requires_current_owner_home_and_visibility() {
    let (repo, input) = fixture();
    let owner = owner(&input.slot);
    repo.connection
        .execute(
            "UPDATE bots SET model=zeroblob(1000000) WHERE bot_id=?1",
            [input.slot.bot.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.routine_occurrence_home(owner, input.slot.bot),
        Ok(input.canonical.home)
    );
    let other = BotOwner {
        device: koru_delegate_core::DeviceId::new(),
        ..owner
    };
    assert_eq!(
        repo.routine_occurrence_home(other, input.slot.bot),
        Err(BotError::Forbidden)
    );
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='trashed' WHERE bot_id=?1",
            [input.slot.bot.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.routine_occurrence_home(owner, input.slot.bot),
        Err(BotError::NotFound)
    );
    // Deliberate damaged metadata, beyond the ordinary FK-protected writer.
    repo.connection
        .pragma_update(None, "foreign_keys", "OFF")
        .unwrap();
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='active', home_workspace_id='malformed' WHERE bot_id=?1",
            [input.slot.bot.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.routine_occurrence_home(owner, input.slot.bot),
        Err(BotError::Storage)
    );
}

#[test]
fn occurrence_claim_lookup_uses_sole_decoder_and_does_not_assert_claim_time_authority() {
    let (mut repo, input) = fixture();
    let row = prepare(&mut repo, &input);
    let mut key = claim(&row).claim;
    assert_eq!(
        repo.routine_preparation_for_claim(&key).unwrap(),
        Some(row.clone())
    );
    key.claimed_at_ms += 1;
    // No claim time lives in AI: this is lookup only. The worker separately
    // compares the entire caller identity with authenticated delegate truth.
    assert_eq!(
        repo.routine_preparation_for_claim(&key).unwrap(),
        Some(row.clone())
    );
    key.ids.run = koru_delegate_core::RunId::mint();
    assert_eq!(
        repo.routine_preparation_for_claim(&key),
        Err(BotError::Conflict)
    );
    let key = claim(&row).claim;
    // Bypass only this in-memory fixture's CHECK to exercise bounded decoding.
    repo.connection
        .pragma_update(None, "ignore_check_constraints", "ON")
        .unwrap();
    repo.connection.execute("UPDATE bot_command_receipts SET submission_version=2 WHERE command_kind='routine_draft'", []).unwrap();
    assert_eq!(
        repo.routine_preparation_for_claim(&key),
        Err(BotError::Storage)
    );
}

#[test]
fn occurrence_exact_replay_precedes_fresh_settings_capture_and_capacity() {
    let (mut repo, input) = fixture();
    let row = prepare(&mut repo, &input);
    let before = counts(&repo);
    assert_eq!(before[3..], [0, 0, 0, 0]);
    repo.connection
        .execute(
            "UPDATE bots SET revision=revision+1,model='changed' WHERE bot_id=?1",
            [input.slot.bot.to_storage()],
        )
        .unwrap();
    let mut changed = input.clone();
    changed.profile = ProfileId::new();
    changed.knowledge.push(changed.role.clone());
    assert_eq!(prepare(&mut repo, &changed), row);
    assert_eq!(counts(&repo), before);
    changed.definition.template.objective =
        koru_delegate_core::Objective::new("different").unwrap();
    assert!(matches!(
        repo.prepare_routine_occurrence(&changed, 60_300),
        Err(BotError::Conflict)
    ));
}

#[test]
fn occurrence_draft_cap_is_exact_subject_and_terminal_closure_releases_it() {
    let (mut repo, mut input) = fixture();
    let first = prepare(&mut repo, &input);
    for n in 1..50 {
        input.slot.occurrence_ms = 60_000 + n * 60_000;
        prepare(&mut repo, &input);
    }
    input.slot.occurrence_ms += 60_000;
    let before = counts(&repo);
    assert!(matches!(
        repo.prepare_routine_occurrence(&input, 60_002).unwrap(),
        BotRoutinePreparationAdmission::Capacity
    ));
    assert_eq!(counts(&repo), before);
    assert!(!repo.routine_preparation_capacity(&input.slot).unwrap());
    let mut other = input.slot;
    other.bot = BotId::mint();
    assert!(has_capacity(&repo.connection, &other).unwrap());
    other = input.slot;
    other.owner.device = koru_delegate_core::DeviceId::new();
    assert!(has_capacity(&repo.connection, &other).unwrap());
    other = input.slot;
    other.owner.user_scope = UserScopeId::new();
    assert!(has_capacity(&repo.connection, &other).unwrap());
    let mut replay = input.clone();
    replay.slot = first.prepared.slot;
    assert_eq!(prepare(&mut repo, &replay), first);
    let observed = RoutineClaimObservation::Absent {
        slot: first.prepared.slot,
        observed_at_ms: 120_000,
    };
    repo.close_unclaimed_preparation(
        &first.prepared.slot,
        first.prepared.semantic_hash,
        BotRoutineCloseReason::ExpiredUnclaimed,
        &observed,
        120_000,
    )
    .unwrap();
    assert!(repo.routine_preparation_capacity(&input.slot).unwrap());
    prepare(&mut repo, &input);
}

#[test]
fn occurrence_finalize_materializes_once_and_accessor_validates_exact_link() {
    let (mut repo, input) = fixture();
    let draft = prepare(&mut repo, &input);
    let record = claim(&draft);
    let row = finalize(&mut repo, &input, &record);
    assert_eq!(row.stage, BotRoutineStoredStage::Linked);
    let before = counts(&repo);
    assert_eq!(&before[3..], &[2, 1, 1, 1]);
    assert_eq!(finalize(&mut repo, &input, &record), row);
    assert_eq!(counts(&repo), before);
    assert_eq!(
        repo.routine_occurrence_for_run(owner(&input.slot), input.slot.bot, row.prepared.ids.run)
            .unwrap(),
        Some(row.clone())
    );
    assert!(
        repo.pending_bot_operations(0, 16)
            .unwrap()
            .iter()
            .all(|(_, _, _, request)| *request != row.prepared.ids.submission)
    );
    repo.connection
        .execute(
            "UPDATE bots SET revision=revision+1,model='later' WHERE bot_id=?1",
            [input.slot.bot.to_storage()],
        )
        .unwrap();
    assert_eq!(finalize(&mut repo, &input, &record), row);
    let mut wrong = record.clone();
    wrong.claim.claimed_at_ms += 1;
    // Repository checks retained fields; claim timestamp is authenticated by
    // worker independent read, and never reconstructed from this row.
    assert_eq!(finalize(&mut repo, &input, &wrong), row);
    repo.connection
        .execute(
            "UPDATE inference_attempts SET operation_id=?2 WHERE id=?1",
            params![
                row.prepared.ids.attempt.to_string(),
                uuid::Uuid::new_v4().to_string()
            ],
        )
        .unwrap();
    assert!(matches!(
        repo.routine_occurrence_for_run(owner(&input.slot), input.slot.bot, row.prepared.ids.run),
        Err(BotError::Storage)
    ));
}

#[test]
fn occurrence_linked_decoder_rejects_changed_or_missing_chat_profile() {
    for profile in [Some(ProfileId::new().to_storage()), None] {
        let (mut repo, input) = fixture();
        let draft = prepare(&mut repo, &input);
        let row = finalize(&mut repo, &input, &claim(&draft));
        repo.connection
            .execute(
                "UPDATE chats SET profile_id=?2 WHERE id=?1",
                params![row.prepared.ids.chat.to_string(), profile],
            )
            .unwrap();
        assert!(matches!(
            repo.routine_occurrence_for_run(
                owner(&input.slot),
                input.slot.bot,
                row.prepared.ids.run
            ),
            Err(BotError::Storage)
        ));
    }
}

#[test]
fn occurrence_every_finalization_insert_rolls_back_to_same_draft() {
    for table in [
        "chats",
        "bot_chats",
        "messages",
        "inference_attempts",
        "bot_chat_runs",
        "bot_run_replay",
        "bot_library_captures",
    ] {
        let (mut repo, input) = fixture();
        let draft = prepare(&mut repo, &input);
        let record = claim(&draft);
        let before = counts(&repo);
        repo.connection.execute_batch(&format!("CREATE TEMP TRIGGER fail_insert BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'fixture'); END;")).unwrap();
        assert!(
            repo.finalize_routine_occurrence(&record, &proof(&input), "koru-chat-v1", 60_200)
                .is_err(),
            "{table}"
        );
        assert_eq!(counts(&repo), before, "{table}");
        assert_eq!(
            repo.read_routine_preparation(&input.slot, draft.prepared.semantic_hash)
                .unwrap(),
            Some(draft)
        );
    }
}

#[test]
fn occurrence_generic_terminal_is_retained_exactly_and_cannot_materialize() {
    let (mut repo, input) = fixture();
    let draft = prepare(&mut repo, &input);
    let mut record = claim(&draft);
    record.link_state = RoutineLinkState::Terminal {
        reason: RoutineTerminalReason::AlreadyTerminal,
    };
    let row = finalize(&mut repo, &input, &record);
    let closure = BotRoutineStoredClosure::ClaimedTerminal {
        reason: RoutineTerminalReason::AlreadyTerminal,
    };
    assert_eq!(row.stage, BotRoutineStoredStage::Closed(closure));
    assert_eq!(closure.preparation_reason(), None);
    assert_eq!(&counts(&repo)[3..], &[0, 0, 0, 0]);
    assert_eq!(
        repo.read_routine_preparation(&input.slot, draft.prepared.semantic_hash)
            .unwrap(),
        Some(row.clone())
    );
    assert_eq!(finalize(&mut repo, &input, &record), row);
}

#[test]
fn occurrence_stop_racing_after_materialization_never_reopens_attempt() {
    let (mut repo, input) = fixture();
    let draft = prepare(&mut repo, &input);
    let mut record = claim(&draft);
    let row = finalize(&mut repo, &input, &record);
    record.link_state = RoutineLinkState::Terminal {
        reason: RoutineTerminalReason::Stopped,
    };
    finalize(&mut repo, &input, &record);
    let id = InferenceAttemptId::from_uuid(row.prepared.ids.attempt);
    let terminal = repo.attempt(id).unwrap().unwrap();
    assert_eq!(terminal.status, AttemptStatus::Cancelled);
    record.link_state = RoutineLinkState::Held {};
    finalize(&mut repo, &input, &record);
    assert_eq!(repo.attempt(id).unwrap().unwrap(), terminal);
    assert_eq!(&counts(&repo)[3..], &[2, 1, 1, 1]);
}

#[test]
fn occurrence_pause_blocks_new_admission_but_exact_draft_remains_readable() {
    let (mut repo, mut input) = fixture();
    let draft = prepare(&mut repo, &input);
    repo.connection
        .execute(
            "UPDATE bots SET paused=1 WHERE bot_id=?1",
            [input.slot.bot.to_storage()],
        )
        .unwrap();
    assert_eq!(prepare(&mut repo, &input), draft);
    assert!(
        repo.finalize_routine_occurrence(&claim(&draft), &proof(&input), "koru-chat-v1", 60010)
            .is_err()
    );
    input.slot.occurrence_ms += 60000;
    assert!(repo.prepare_routine_occurrence(&input, 60011).is_err());
    assert_eq!(&counts(&repo)[3..], &[0, 0, 0, 0]);
}

#[test]
fn occurrence_current_profile_drift_requires_current_proof_and_closes_exact_claim() {
    let (mut repo, input) = fixture();
    let draft = prepare(&mut repo, &input);
    let current = ProfileId::new();
    repo.connection
        .execute("UPDATE chats SET profile_id=?1", [current.to_storage()])
        .unwrap();
    assert!(matches!(
        repo.finalize_routine_occurrence(&claim(&draft), &proof(&input), "koru-chat-v1", 60010),
        Err(BotError::Forbidden)
    ));
    let mut authority = proof(&input);
    authority.profile = current;
    let BotRoutinePreparationAdmission::Ready(row) = repo
        .finalize_routine_occurrence(&claim(&draft), &authority, "koru-chat-v1", 60011)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        row.stage,
        BotRoutineStoredStage::Closed(BotRoutineStoredClosure::ClaimedTerminal {
            reason: RoutineTerminalReason::AuthorityChanged
        })
    );
    assert_eq!(&counts(&repo)[3..], &[0, 0, 0, 0]);
}

#[test]
fn occurrence_positive_configuration_drift_closes_but_corrupt_source_stays_prepared() {
    let (mut repo, input) = fixture();
    let draft = prepare(&mut repo, &input);
    let record = claim(&draft);
    repo.connection
        .execute(
            "UPDATE bots SET revision=revision+1,model='changed' WHERE bot_id=?1",
            [input.slot.bot.to_storage()],
        )
        .unwrap();
    assert_eq!(
        finalize(&mut repo, &input, &record).stage,
        BotRoutineStoredStage::Closed(BotRoutineStoredClosure::ClaimedTerminal {
            reason: RoutineTerminalReason::ConfigurationChanged
        })
    );
    let (mut repo, input) = fixture();
    let draft = prepare(&mut repo, &input);
    let record = claim(&draft);
    repo.connection
        .execute(
            "UPDATE bots SET working_folder='' WHERE bot_id=?1",
            [input.slot.bot.to_storage()],
        )
        .unwrap();
    assert!(
        repo.finalize_routine_occurrence(&record, &proof(&input), "koru-chat-v1", 60_200)
            .is_err()
    );
    assert_eq!(
        repo.read_routine_preparation(&input.slot, draft.prepared.semantic_hash)
            .unwrap(),
        Some(draft)
    );
}

#[test]
fn occurrence_draft_decoder_rejects_every_nonnull_reserved_receipt_field() {
    for (column, value) in [
        ("submission_version", "2"),
        ("submission_json", "'{}'"),
        ("planned_run_id", "'00000000-0000-4000-8000-000000000001'"),
        ("captured_config_revision", "1"),
        ("captured_policy_revision", "1"),
        ("expected_revision", "1"),
        ("desired_pause", "0"),
        ("pause_ack_stage", "'pending'"),
    ] {
        let (mut repo, input) = fixture();
        let row = prepare(&mut repo, &input);
        repo.connection
            .pragma_update(None, "ignore_check_constraints", "ON")
            .unwrap();
        repo.connection
            .execute(
                &format!("UPDATE bot_command_receipts SET {column}={value} WHERE request_id=?1"),
                [row.prepared.ids.submission.to_storage()],
            )
            .unwrap();
        assert!(
            matches!(
                repo.read_routine_preparation(&input.slot, row.prepared.semantic_hash),
                Err(BotError::Storage)
            ),
            "{column}"
        );
    }
}

#[test]
fn occurrence_recovery_bounds_projection_and_advances_past_corrupt_first_page() {
    let (mut repo, mut input) = fixture();
    let first = prepare(&mut repo, &input);
    for n in 1..18 {
        input.slot.occurrence_ms = 60_000 + n * 60_000;
        prepare(&mut repo, &input);
    }
    repo.connection
        .pragma_update(None, "ignore_check_constraints", "ON")
        .unwrap();
    repo.connection.execute("UPDATE bot_routine_preparations SET semantic_hash=x'01' WHERE submission_request_id=?1",[first.prepared.ids.submission.to_storage()]).unwrap();
    let first = repo
        .routine_occurrence_recovery_page(owner(&input.slot), None)
        .unwrap();
    assert_eq!(first.entries.len(), 16);
    assert!(first.entries[0].is_err());
    let second = repo
        .routine_occurrence_recovery_page(owner(&input.slot), first.next)
        .unwrap();
    assert_eq!(second.entries.len(), 2);
    assert!(second.entries.iter().all(Result::is_ok));
    assert!(second.next.is_none());
}
