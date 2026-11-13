// SPDX-License-Identifier: MPL-2.0
use super::*;

fn bind(repo: &mut Repository, bot: &Bot) -> (Bot, BotMemoryBinding) {
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let creation = repo
        .prepare_bot_memory_creation(
            owner,
            bot.id,
            BotRequestId::mint(),
            bot.revision,
            &BotMemorySeed {
                title: "Memory".into(),
                hash: [7; 32],
            },
            HASH,
            2,
        )
        .unwrap();
    repo.bind_bot_memory(
        &BotMemoryConfirmation {
            identity: creation.identity.clone(),
            original_edit_revision: vec![1],
            live_edit_revision: vec![1],
        },
        creation.observed(),
        3,
    )
    .unwrap();
    (
        repo.bot(bot.id).unwrap().unwrap(),
        repo.bot_library(owner, bot.id).unwrap().binding.unwrap(),
    )
}

fn intent(bot: &Bot, sources: &LibrarySources) -> BotSubmissionIntent {
    super::super::super::admission::make_library_intent(
        bot,
        BotRequestId::mint(),
        bot.main_chat,
        bot.home_workspace,
        &BotTask {
            text: "raw task".into(),
            documents: vec![],
        },
        None,
        &pins(sources),
        sources,
        ProfileId::new(),
    )
    .unwrap()
}

fn captured(sources: &mut LibrarySources, bytes: &[u8]) {
    let binding = sources.memory_binding.as_ref().unwrap();
    sources.memory_capture = Some(PreparedMemoryCapture {
        owner: binding.owner,
        bot: binding.bot,
        creation_request: binding.creation_request,
        page: binding.page.clone(),
        version: d::DocumentVersionId::new(),
        nodes: vec![],
        omitted_count: 2,
        invalid_count: 1,
        index_digest: *blake3::hash(bytes).as_bytes(),
    });
}

fn chat_intent(
    bot: &Bot,
    sources: &LibrarySources,
) -> (PendingTurn, BotSubmissionIntent, BotLibraryCapture) {
    let mut turn = super::super::super::tests::turn(bot.main_chat, Uuid::new_v4(), 1);
    turn.user_message.markdown = "raw task".into();
    let mut intent = intent(bot, sources);
    intent.submission.provenance.ai_attempt_id = Some(turn.attempt.id.as_uuid());
    intent.submission.provenance.assistant_message_id = Some(turn.assistant_message.id.as_uuid());
    let capture = sources.capture(&intent).unwrap();
    (turn, intent, capture)
}

#[test]
fn both_admission_composers_commit_exact_memory_capture() {
    for shared in [false, true] {
        let (mut repo, bot, _) = fixture();
        let (bot, binding) = bind(&mut repo, &bot);
        let mut sources = LibrarySources::read(&repo, &bot, vec![]).unwrap();
        assert!(
            sources.capture(&intent(&bot, &sources)).is_err(),
            "bound never becomes Absent"
        );
        captured(&mut sources, b"exact memory index");
        let (turn, intent, capture) = chat_intent(&bot, &sources);
        assert!(!sources.pages().contains(&binding.page));
        if shared {
            repo.prepare_bot_chat_with_library(
                &BotChatPreparation::Turn(turn.clone()),
                &intent,
                &capture,
                HASH,
                bot.revision,
                turn.attempt.queued_at,
            )
            .unwrap();
        } else {
            let mut intent = intent.clone();
            intent.submission.provenance.ai_attempt_id = None;
            intent.submission.provenance.assistant_message_id = None;
            repo.prepare_bot_submission_with_library(&intent, &capture, HASH, bot.revision, 4)
                .unwrap();
        }
        assert_eq!(
            repo.bot_library_capture(binding.owner, bot.id, capture.submission)
                .unwrap(),
            Some(capture)
        );
    }
}

#[test]
fn bind_race_refuses_stale_absent_capture_without_partial_turn() {
    let (mut repo, bot, _) = fixture();
    let sources = LibrarySources::read(&repo, &bot, vec![]).unwrap();
    let (turn, intent, capture) = chat_intent(&bot, &sources);
    let (_, binding) = bind(&mut repo, &bot);
    assert!(
        repo.prepare_bot_chat_with_library(
            &BotChatPreparation::Turn(turn.clone()),
            &intent,
            &capture,
            HASH,
            bot.revision,
            turn.attempt.queued_at
        )
        .is_err()
    );
    assert!(repo.attempt(turn.attempt.id).unwrap().is_none());
    assert!(
        repo.bot_library_capture(binding.owner, bot.id, capture.submission)
            .unwrap()
            .is_none()
    );
}

#[test]
fn admitted_absent_survives_later_binding_and_rejects_corrupt_digest() {
    let (mut repo, bot, _) = fixture();
    let sources = LibrarySources::read(&repo, &bot, vec![]).unwrap();
    let (turn, intent, capture) = chat_intent(&bot, &sources);
    repo.prepare_bot_chat_with_library(
        &BotChatPreparation::Turn(turn.clone()),
        &intent,
        &capture,
        HASH,
        bot.revision,
        turn.attempt.queued_at,
    )
    .unwrap();
    let expected = BotMemoryContext::read(&repo, turn.attempt.id, bot.main_chat)
        .unwrap()
        .unwrap();
    bind(&mut repo, &bot);
    let resolution = BotMemoryResolution {
        expected,
        cancellation: Cancellation::default(),
        index: VerifiedMemoryIndex {
            capture,
            bytes: vec![],
        },
    };
    resolution.validate(&repo, turn.attempt.id).unwrap();
    let mut corrupt = resolution;
    corrupt.index.capture.index_digest = [9; 32];
    assert!(corrupt.validate(&repo, turn.attempt.id).is_err());
    corrupt.index.capture.index_digest = *blake3::hash(&[]).as_bytes();
    corrupt.cancellation.cancel();
    assert!(
        corrupt.validate(&repo, turn.attempt.id).is_err(),
        "late cancellation cannot deliver"
    );
}

#[test]
fn captured_digest_mismatch_and_legacy_capture_refuse_delivery() {
    let (mut repo, bot, _) = fixture();
    let (bot, _) = bind(&mut repo, &bot);
    let mut sources = LibrarySources::read(&repo, &bot, vec![]).unwrap();
    captured(&mut sources, b"expected index");
    let (turn, intent, capture) = chat_intent(&bot, &sources);
    repo.prepare_bot_chat_with_library(
        &BotChatPreparation::Turn(turn.clone()),
        &intent,
        &capture,
        HASH,
        bot.revision,
        turn.attempt.queued_at,
    )
    .unwrap();
    let expected = BotMemoryContext::read(&repo, turn.attempt.id, bot.main_chat)
        .unwrap()
        .unwrap();
    assert!(
        BotMemoryResolution {
            expected,
            cancellation: Cancellation::default(),
            index: VerifiedMemoryIndex {
                capture,
                bytes: b"substituted".to_vec()
            }
        }
        .validate(&repo, turn.attempt.id)
        .is_err()
    );

    let (mut repo, bot, _) = fixture();
    let sources = LibrarySources::read(&repo, &bot, vec![]).unwrap();
    let (turn, intent, _) = chat_intent(&bot, &sources);
    repo.prepare_bot_chat(
        &BotChatPreparation::Turn(turn.clone()),
        &intent,
        HASH,
        bot.revision,
        turn.attempt.queued_at,
    )
    .unwrap();
    assert!(BotMemoryContext::read(&repo, turn.attempt.id, bot.main_chat).is_err());
}
