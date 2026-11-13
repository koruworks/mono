// SPDX-License-Identifier: MPL-2.0
use super::*;
mod lifecycle;

#[test]
fn memory_node_capture_roundtrips_genesis_and_imported_uuid_values() {
    let (mut repo, bot, owner, intent, mut capture) = capture_fixture(true);
    let expected = vec![
        "12345678-1234-5123-8123-123456789abc",
        "00000000-0000-0000-0000-000000000000",
        "ffffffff-ffff-ffff-ffff-ffffffffffff",
    ]
    .into_iter()
    .map(|value| value.parse::<BotMemoryNodeId>().unwrap())
    .collect::<Vec<_>>();
    let BotCapturedMemory::Captured { nodes, .. } = &mut capture.memory else {
        unreachable!()
    };
    *nodes = expected;
    repo.prepare_bot_submission_with_library(&intent, &capture, HASH, bot.revision, 4)
        .unwrap();
    assert_eq!(
        repo.bot_library_capture(owner, bot.id, capture.submission)
            .unwrap(),
        Some(capture.clone())
    );
    assert_eq!(
        serde_json::from_str::<BotLibraryCapture>(&serde_json::to_string(&capture).unwrap())
            .unwrap(),
        capture
    );
    repo.connection
        .execute(
            "UPDATE bot_memory_index_nodes SET node_id=upper(node_id) WHERE ordinal=0",
            [],
        )
        .unwrap();
    assert_eq!(
        repo.bot_library_capture(owner, bot.id, capture.submission),
        Err(BotError::Storage)
    );
}

#[test]
fn new_submission_cannot_pin_bound_memory_on_any_admission_route() {
    for route in ["direct", "api", "chat"] {
        for block in [false, true] {
            let (mut repo, bot, owner, mut intent, capture) = capture_fixture(true);
            let binding = repo.bot_library(owner, bot.id).unwrap().binding.unwrap();
            let version = d::DocumentVersionId::new();
            intent.submission.request.context.push(if block {
                d::RunContextItem::Block {
                    workspace: binding.page.workspace,
                    document: binding.page.document,
                    version,
                    node: d::BlockNodeId::mint(),
                }
            } else {
                d::RunContextItem::Document {
                    workspace: binding.page.workspace,
                    document: binding.page.document,
                    version,
                }
            });
            let before = receipt_count(&repo);
            if route == "chat" {
                let turn = crate::repository::tests::turn(bot.main_chat, uuid::Uuid::new_v4(), 4);
                intent.submission.provenance.ai_attempt_id = Some(turn.attempt.id.as_uuid());
                intent.submission.provenance.assistant_message_id =
                    Some(turn.assistant_message.id.as_uuid());
                intent.submission.input.text = turn.user_message.markdown.clone();
                let result = repo.prepare_bot_chat_with_library(
                    &koru_ai_core::bots::replay::BotChatPreparation::Turn(turn.clone()),
                    &intent,
                    &capture,
                    HASH,
                    bot.revision,
                    turn.attempt.queued_at,
                );
                assert!(result.is_err());
                assert!(repo.attempt(turn.attempt.id).unwrap().is_none());
                assert!(repo.messages(bot.main_chat).unwrap().is_empty());
            } else if route == "api" {
                assert_eq!(
                    repo.prepare_bot_submission_with_library(
                        &intent,
                        &capture,
                        HASH,
                        bot.revision,
                        4
                    ),
                    Err(BotError::Forbidden)
                );
            } else {
                assert_eq!(
                    repo.prepare_bot_submission(&intent, HASH, bot.revision, 4),
                    Err(BotError::Forbidden)
                );
            }
            assert_eq!(receipt_count(&repo), before);
            assert!(
                repo.bot(bot.id)
                    .unwrap()
                    .unwrap()
                    .folder_lock_request
                    .is_none()
            );
            assert!(
                repo.bot_library_capture(owner, bot.id, capture.submission)
                    .unwrap()
                    .is_none()
            );
        }
    }
}

#[test]
fn exact_receipt_keeps_prebinding_document_pin_without_new_admission() {
    let (mut repo, bot, owner) = fixture();
    let c = prepared(&mut repo, &bot, owner);
    let mut task = intent(&bot);
    task.submission
        .request
        .context
        .push(d::RunContextItem::Document {
            workspace: bot.home_workspace,
            document: c.identity.candidate,
            version: d::DocumentVersionId::new(),
        });
    let capture = BotLibraryCapture {
        version: 1,
        owner,
        bot: bot.id,
        submission: task.submission.provenance.submission,
        config_revision: bot.revision,
        knowledge: vec![],
        memory: BotCapturedMemory::Absent {},
        index_digest: [0; 32],
    };
    let saved = repo
        .prepare_bot_submission_with_library(&task, &capture, HASH, bot.revision, 3)
        .unwrap();
    repo.bind_bot_memory(&confirmation(&c), c.observed(), 4)
        .unwrap();
    assert_eq!(
        repo.prepare_bot_submission_with_library(&task, &capture, HASH, bot.revision, 5)
            .unwrap(),
        saved
    );
    assert_eq!(
        repo.bot_library_capture(owner, bot.id, capture.submission)
            .unwrap(),
        Some(capture)
    );
}

#[test]
fn api_submission_and_library_capture_commit_or_rollback_together() {
    let (mut repo, bot, owner, intent, capture) = capture_fixture(false);
    let before = receipt_count(&repo);
    let mut invalid = capture.clone();
    invalid.knowledge.pop();
    assert!(
        repo.prepare_bot_submission_with_library(&intent, &invalid, HASH, bot.revision, 4)
            .is_err()
    );
    assert_eq!(receipt_count(&repo), before);
    assert!(
        repo.bot(bot.id)
            .unwrap()
            .unwrap()
            .folder_lock_request
            .is_none()
    );
    assert!(
        repo.bot_library_capture(owner, bot.id, capture.submission)
            .unwrap()
            .is_none()
    );
    let receipt = repo
        .prepare_bot_submission_with_library(&intent, &capture, HASH, bot.revision, 4)
        .unwrap();
    assert_eq!(
        repo.bot_library_capture(owner, bot.id, capture.submission)
            .unwrap(),
        Some(capture.clone())
    );
    assert_eq!(receipt_count(&repo), before + 1);
    assert_eq!(
        repo.prepare_bot_submission_with_library(&intent, &capture, HASH, bot.revision, 5)
            .unwrap(),
        receipt
    );
    let mut changed = capture.clone();
    changed.index_digest[0] ^= 1;
    assert_eq!(
        repo.prepare_bot_submission_with_library(&intent, &changed, HASH, bot.revision, 5),
        Err(BotError::Conflict)
    );
    assert_eq!(receipt_count(&repo), before + 1);
}

#[test]
fn legacy_receipt_replay_does_not_gain_library_authority() {
    let (mut repo, bot, owner, intent, capture) = capture_fixture(false);
    let receipt = repo
        .prepare_bot_submission(&intent, HASH, bot.revision, 4)
        .unwrap();
    assert_eq!(
        repo.prepare_bot_submission_with_library(&intent, &capture, HASH, bot.revision, 5),
        Err(BotError::Conflict)
    );
    assert!(
        repo.bot_library_capture(owner, bot.id, capture.submission)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.prepare_bot_submission(&intent, HASH, bot.revision, 6)
            .unwrap(),
        receipt
    );
}

#[test]
fn exact_retry_keeps_old_capture_while_new_task_uses_current_library() {
    let (mut repo, bot, owner, intent, capture) = capture_fixture(false);
    let receipt = repo
        .prepare_bot_submission_with_library(&intent, &capture, HASH, bot.revision, 4)
        .unwrap();
    let mut edit = knowledge(&bot, owner);
    edit.pages = vec![];
    repo.set_bot_knowledge(&edit, OTHER_HASH, 5).unwrap();
    assert_eq!(
        repo.prepare_bot_submission_with_library(&intent, &capture, HASH, bot.revision, 6)
            .unwrap(),
        receipt
    );
    assert_eq!(
        repo.bot_library_capture(owner, bot.id, capture.submission)
            .unwrap(),
        Some(capture.clone())
    );
    // The first admission still owns the folder reservation until its owner
    // settles/refuses it. End it through the real fence before new work.
    let fenced = repo
        .prepare_bot_refusal_fence(
            owner,
            bot.id,
            capture.submission,
            BotAdmissionRefusal::Rejected,
            false,
            6,
        )
        .unwrap();
    let Some(BotExecutionOutcome::RefusalFence { round, barrier, .. }) = fenced.outcome else {
        panic!("fence")
    };
    repo.acknowledge_bot_refusal_fence(owner, bot.id, capture.submission, round, &barrier, 6)
        .unwrap();
    repo.reject_bot_submission_after_fence(owner, bot.id, capture.submission, round, &barrier, 6)
        .unwrap();
    let current = repo.bot(bot.id).unwrap().unwrap();
    let mut next = intent.clone();
    next.submission.provenance.submission = BotRequestId::mint();
    next.submission.provenance.config_revision = current.revision;
    next.submission.provenance.policy_revision = current.policy_revision;
    next.submission.request.run = d::RunId::mint();
    let next_capture = BotLibraryCapture {
        version: 1,
        owner,
        bot: bot.id,
        submission: next.submission.provenance.submission,
        config_revision: current.revision,
        knowledge: vec![],
        memory: BotCapturedMemory::Absent {},
        index_digest: [0; 32],
    };
    repo.prepare_bot_submission_with_library(&next, &next_capture, HASH, current.revision, 7)
        .unwrap();
    assert!(
        repo.bot_library_capture(owner, bot.id, next_capture.submission)
            .unwrap()
            .unwrap()
            .knowledge
            .is_empty()
    );
}

fn intent(bot: &Bot) -> BotSubmissionIntent {
    BotSubmissionIntent {
        version: 1,
        submission: d::BotSubmission {
            provenance: d::BotProvenance {
                owner: d::BotOwner {
                    user_scope: bot.user_scope,
                    device: bot.owner_device,
                },
                bot: bot.id,
                submission: BotRequestId::mint(),
                chat_id: bot.main_chat.as_uuid(),
                ai_attempt_id: None,
                assistant_message_id: None,
                config_revision: bot.revision,
                policy_revision: bot.policy_revision,
                continued_from: None,
            },
            policy: d::CapturedBotPolicy {
                workspace: bot.home_workspace,
                destination: bot.home_workspace,
                access: d::BotAccess::Default,
                selected_pages: Default::default(),
            },
            request: d::DelegationRequest {
                run: d::RunId::mint(),
                requested_by: d::ActorRef::Person {
                    profile: ProfileId::new(),
                },
                objective: d::Objective::new("Fixture task").unwrap(),
                agent: d::SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                requested_model: None,
                scope: d::WorkspaceScope::BotFolder {
                    folder: d::BotWorkingFolder::new(bot.config.working_folder.clone()).unwrap(),
                },
                context: vec![],
                grants: [bot.home_workspace].into_iter().collect(),
                executing_device: bot.owner_device,
                origin: d::RunOrigin::Manual,
            },
            workspace: bot.home_workspace,
            input: d::PendingInput {
                idempotency_key: d::IdempotencyKey::mint(),
                text: "Fixture task".into(),
                attachment_ids: vec![],
                references: vec![],
                mode: None,
                fast: None,
                reasoning: None,
                skills: vec![],
                model: None,
            },
        },
    }
}

fn capture_fixture(
    bound: bool,
) -> (
    Repository,
    Bot,
    BotOwner,
    BotSubmissionIntent,
    BotLibraryCapture,
) {
    let (mut repo, mut bot, owner) = fixture();
    let memory = if bound {
        let c = prepared(&mut repo, &bot, owner);
        let b = repo
            .bind_bot_memory(&confirmation(&c), c.observed(), 3)
            .unwrap();
        bot = repo.bot(bot.id).unwrap().unwrap();
        BotCapturedMemory::Captured {
            page: b.page,
            version: d::DocumentVersionId::new(),
            creation_request: b.creation_request,
            nodes: vec![
                BotMemoryNodeId::from_uuid(uuid::Uuid::new_v4()),
                BotMemoryNodeId::from_uuid(uuid::Uuid::new_v4()),
            ],
            omitted_count: 2,
            invalid_count: 1,
        }
    } else {
        BotCapturedMemory::Absent {}
    };
    let page = BotPage {
        workspace: bot.home_workspace,
        document: DocumentId::new(),
    };
    let mut input = knowledge(&bot, owner);
    let second_page = BotPage {
        workspace: bot.home_workspace,
        document: DocumentId::new(),
    };
    input.pages = vec![page.clone(), second_page.clone()];
    repo.set_bot_knowledge(&input, HASH, 3).unwrap();
    bot = repo.bot(bot.id).unwrap().unwrap();
    let mut intent = intent(&bot);
    let version = d::DocumentVersionId::new();
    let second_version = d::DocumentVersionId::new();
    intent
        .submission
        .request
        .context
        .push(d::RunContextItem::Document {
            workspace: page.workspace,
            document: page.document,
            version,
        });
    intent
        .submission
        .request
        .context
        .push(d::RunContextItem::Document {
            workspace: second_page.workspace,
            document: second_page.document,
            version: second_version,
        });
    let capture = BotLibraryCapture {
        version: 1,
        owner,
        bot: bot.id,
        submission: intent.submission.provenance.submission,
        config_revision: bot.revision,
        knowledge: vec![
            BotKnowledgeCapture { page, version },
            BotKnowledgeCapture {
                page: second_page,
                version: second_version,
            },
        ],
        memory,
        index_digest: [3; 32],
    };
    (repo, bot, owner, intent, capture)
}
fn insert(
    repo: &mut Repository,
    intent: &BotSubmissionIntent,
    capture: &BotLibraryCapture,
    commit: bool,
) -> Result<(), BotError> {
    let tx = repo.connection.transaction().unwrap();
    admission::prepare_bot_submission_tx(&tx, intent, HASH, capture.config_revision, 4)?;
    insert_library_capture_tx(&tx, capture, 4)?;
    if commit {
        tx.commit().unwrap();
    }
    Ok(())
}

#[test]
fn library_capture_is_atomic_immutable_and_exact() {
    for bound in [false, true] {
        let (mut repo, bot, owner, intent, capture) = capture_fixture(bound);
        let before = receipt_count(&repo);
        insert(&mut repo, &intent, &capture, false).unwrap();
        assert_eq!(receipt_count(&repo), before);
        assert!(
            repo.bot_library_capture(owner, bot.id, capture.submission)
                .unwrap()
                .is_none()
        );
        assert!(
            repo.bot(bot.id)
                .unwrap()
                .unwrap()
                .folder_lock_request
                .is_none()
        );
        insert(&mut repo, &intent, &capture, true).unwrap();
        assert_eq!(
            repo.bot_library_capture(owner, bot.id, capture.submission)
                .unwrap(),
            Some(capture.clone())
        );
        {
            let tx = repo.connection.transaction().unwrap();
            assert_eq!(
                insert_library_capture_tx(&tx, &capture, 5).unwrap(),
                capture
            );
            let mut changed = capture.clone();
            changed.index_digest[0] = 4;
            assert_eq!(
                insert_library_capture_tx(&tx, &changed, 5),
                Err(BotError::Conflict)
            );
        }
        // New current settings cannot rewrite exact captured pin versions.
        let current = repo.bot(bot.id).unwrap().unwrap();
        repo.set_bot_knowledge(&knowledge(&current, owner), OTHER_HASH, 6)
            .unwrap();
        assert_eq!(
            repo.bot_library_capture(owner, bot.id, capture.submission)
                .unwrap(),
            Some(capture.clone())
        );
        if !bound {
            let current = repo.bot(bot.id).unwrap().unwrap();
            let c = prepared(&mut repo, &current, owner);
            repo.bind_bot_memory(&confirmation(&c), c.observed(), 7)
                .unwrap();
            assert_eq!(
                repo.bot_library_capture(owner, bot.id, capture.submission)
                    .unwrap(),
                Some(capture)
            );
        }
    }
    for field in ["pin", "owner", "bot", "revision", "binding"] {
        let (mut repo, bot, _owner, intent, mut capture) = capture_fixture(true);
        let before = receipt_count(&repo);
        match field {
            "pin" => capture.knowledge[0].version = d::DocumentVersionId::new(),
            "owner" => capture.owner.device = DeviceId::new(),
            "bot" => capture.bot = BotId::mint(),
            "revision" => capture.config_revision += 1,
            "binding" => {
                if let BotCapturedMemory::Captured {
                    creation_request, ..
                } = &mut capture.memory
                {
                    *creation_request = BotRequestId::mint();
                }
            }
            _ => unreachable!(),
        }
        assert!(
            insert(&mut repo, &intent, &capture, true).is_err(),
            "{field}"
        );
        assert_eq!(receipt_count(&repo), before);
        assert!(
            repo.bot(bot.id)
                .unwrap()
                .unwrap()
                .folder_lock_request
                .is_none()
        );
    }
}

#[test]
fn library_capture_corruption_refuses_partial_results() {
    for sql in [
        "DELETE FROM bot_knowledge_captures",
        "DELETE FROM bot_knowledge_captures WHERE ordinal=(SELECT max(ordinal) FROM bot_knowledge_captures)",
        "UPDATE bot_knowledge_captures SET ordinal=3 WHERE ordinal=0",
        "UPDATE bot_knowledge_captures SET history_version_id='not-a-uuid'",
        "UPDATE bot_memory_index_nodes SET ordinal=8 WHERE ordinal=0",
        "DELETE FROM bot_memory_index_nodes WHERE ordinal=0",
        "UPDATE bot_library_captures SET config_revision=config_revision+1",
        "UPDATE bot_library_captures SET bot_id='different-bot'",
        "UPDATE bot_memory_creation_intents SET state='unconfirmed',origin_edit_revision=NULL",
    ] {
        let (mut repo, bot, owner, intent, capture) = capture_fixture(true);
        insert(&mut repo, &intent, &capture, true).unwrap();
        repo.connection.execute(sql, []).unwrap();
        assert_eq!(
            repo.bot_library_capture(owner, bot.id, capture.submission),
            Err(BotError::Storage),
            "{sql}"
        );
    }
}

#[test]
fn library_capture_admission_requires_complete_current_library() {
    for omit_memory in [false, true] {
        let (mut repo, bot, _, intent, mut capture) = capture_fixture(true);
        if omit_memory {
            capture.memory = BotCapturedMemory::Absent {};
        } else {
            capture.knowledge.clear();
        }
        let before = receipt_count(&repo);
        assert_eq!(
            insert(&mut repo, &intent, &capture, true),
            Err(BotError::Conflict)
        );
        assert_eq!(receipt_count(&repo), before);
        assert!(
            repo.bot(bot.id)
                .unwrap()
                .unwrap()
                .folder_lock_request
                .is_none()
        );
    }
}
