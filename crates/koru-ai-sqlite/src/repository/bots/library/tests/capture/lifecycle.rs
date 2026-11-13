// SPDX-License-Identifier: MPL-2.0
//! Lifecycle transactions retain Memory identity, binding and immutable capture.
use super::*;
use crate::{LifecycleCapture, LifecycleMemberDraft};

#[test]
fn archive_trash_and_restore_retain_bound_memory_and_submission_capture() {
    let (mut repo, bot, owner, intent, capture) = capture_fixture(true);
    insert(&mut repo, &intent, &capture, true).unwrap();
    let provenance = &intent.submission.provenance;
    repo.acknowledge_bot_submission(
        owner,
        bot.id,
        provenance.submission,
        &d::BotAdmissionView {
            key: d::BotSubmissionKey {
                owner: provenance.owner,
                bot: bot.id,
                request: provenance.submission,
            },
            run: intent.submission.request.run,
            state: d::BotAdmissionState::Refused,
            queue_sequence: 1,
            fence: None,
        },
        5,
    )
    .unwrap();
    let before = repo.bot_library(owner, bot.id).unwrap();
    for kind in [BotLifecycleKind::Archive, BotLifecycleKind::Trash] {
        let current = repo.bot_any_lifecycle(bot.id).unwrap().unwrap().0;
        let operation = repo
            .capture_bot_lifecycle(&LifecycleCapture {
                operation: BotRequestId::mint(),
                owner,
                bot: bot.id,
                kind,
                chat: None,
                fingerprint: HASH.into(),
                expected_revision: current.revision,
                source: None,
                members: vec![
                    LifecycleMemberDraft {
                        subject: BotLifecycleSubject::Bot,
                        reason: BotLifecycleReason::Bot,
                        prior_state: BotLifecyclePriorState::Live,
                        intended: BotLifecycleIntended::Hide,
                        retained: None,
                    },
                    LifecycleMemberDraft {
                        subject: BotLifecycleSubject::Chat {
                            chat_id: bot.main_chat,
                        },
                        reason: BotLifecycleReason::OwnedConversation,
                        prior_state: BotLifecyclePriorState::Live,
                        intended: BotLifecycleIntended::Hide,
                        retained: None,
                    },
                ],
                now: 10,
            })
            .unwrap();
        let pause_request = operation.pause_request.unwrap();
        let Some(BotAdmissionReceipt::Pause(pause)) = repo
            .bot_admission_receipt(owner, bot.id, pause_request)
            .unwrap()
        else {
            panic!("lifecycle must retain its exact pause receipt")
        };
        repo.acknowledge_bot_pause(owner, bot.id, pause_request, &pause.intent.barrier, 11)
            .unwrap();
        repo.advance_bot_lifecycle(
            owner.user_scope,
            operation.operation,
            BotLifecycleStage::Captured,
            BotLifecycleStage::Settling,
            true,
            11,
        )
        .unwrap();
        match kind {
            BotLifecycleKind::Archive => {
                repo.complete_bot_archive(owner.user_scope, operation.operation, 12)
                    .unwrap();
            }
            BotLifecycleKind::Trash => {
                repo.complete_bot_trash(owner.user_scope, operation.operation, 12)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let current = repo.bot_any_lifecycle(bot.id).unwrap().unwrap().0;
        let restore = LifecycleCapture {
            operation: BotRequestId::mint(),
            owner,
            bot: bot.id,
            kind: BotLifecycleKind::Restore,
            chat: None,
            fingerprint: HASH.into(),
            expected_revision: current.revision,
            source: Some(operation.operation),
            members: vec![],
            now: 13,
        };
        match kind {
            BotLifecycleKind::Archive => {
                repo.unarchive_bot(&restore).unwrap();
            }
            BotLifecycleKind::Trash => {
                repo.restore_trashed_bot(&restore).unwrap();
            }
            _ => unreachable!(),
        }
        let after = repo.bot_library(owner, bot.id).unwrap();
        assert_eq!(after.binding, before.binding);
        assert_eq!(after.creation, before.creation);
        assert_eq!(
            repo.bot_library_capture(owner, bot.id, capture.submission)
                .unwrap(),
            Some(capture.clone())
        );
    }
}
