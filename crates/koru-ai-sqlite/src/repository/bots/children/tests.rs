// SPDX-License-Identifier: MPL-2.0
//! Cohorts `m6_child_chat_rows` (the repository half), `m6_may_ask` and
//! `m6_activity_waiting`. In-memory repositories only: no worker, delegate,
//! Store or provider, and every delegate reply here is a fixture value.
use super::*;
use crate::repository::tests::{chat, turn};
use koru_ai_core::bots::BotRepository;
use koru_delegate_core as d;
use std::collections::BTreeSet;

fn at(n: i64) -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000 + n)
}

pub(in crate::repository::bots) struct Parent {
    pub repo: Repository,
    pub owner: BotOwner,
    pub bot: Bot,
    pub turn: PendingTurn,
    pub submission: d::BotSubmission,
    pub fence: BotExecutionFence,
}
impl Parent {
    pub fn origin(&self) -> DecisionFence {
        DecisionFence {
            owner: self.fence.owner,
            bot: self.fence.bot,
            run: self.fence.run,
            attempt: self.fence.attempt,
            generation: self.fence.generation,
            session: self.fence.session,
            turn: self.fence.turn.unwrap(),
        }
    }
    pub fn run(&self) -> d::RunId {
        self.submission.request.run
    }
    pub fn attempt(&self) -> InferenceAttemptId {
        self.turn.attempt.id
    }
}

/// A Bot whose reply is executing: its submission accepted with a fence and
/// its AI attempt bound to that fence.
pub(in crate::repository::bots) fn parent() -> Parent {
    let (mut repo, turn, submission, fence) = crate::repository::bot_replay::tests::prepared();
    let p = &submission.provenance;
    let owner = BotOwner {
        user_scope: p.owner.user_scope,
        device: p.owner.device,
    };
    let admission = d::BotAdmissionView {
        key: d::BotSubmissionKey {
            owner: p.owner,
            bot: p.bot,
            request: p.submission,
        },
        run: submission.request.run,
        state: d::BotAdmissionState::Executing,
        queue_sequence: 1,
        fence: Some(fence.clone()),
    };
    repo.connection
        .execute(
            "UPDATE bots SET folder_lock_request_id=?1 WHERE bot_id=?2",
            params![p.submission.to_storage(), p.bot.to_storage()],
        )
        .unwrap();
    repo.acknowledge_bot_submission(owner, p.bot, p.submission, &admission, 3)
        .unwrap();
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET fingerprint=?1 WHERE request_id=?2",
            params!["a".repeat(64), p.submission.to_storage()],
        )
        .unwrap();
    let bot = repo.bot(p.bot).unwrap().unwrap();
    Parent {
        repo,
        owner,
        bot,
        turn,
        submission,
        fence,
    }
}

/// Another active Bot of the same person, home workspace and computer.
fn recipient(parent: &mut Parent, name: &str) -> Bot {
    let mut bot = parent.bot.clone();
    bot.id = BotId::mint();
    bot.config.name = name.into();
    bot.config.working_folder = format!("D:/{name}");
    bot.config.role.document = DocumentId::new();
    bot.main_chat = ChatId::new();
    bot.folder_lock_request = None;
    bot.folder_frozen_at_ms = None;
    bot.revision = 1;
    bot.policy_revision = 1;
    parent
        .repo
        .save_bot(&bot, ProfileId::new(), BotRequestId::mint(), "create", None)
        .unwrap();
    parent.repo.bot(bot.id).unwrap().unwrap()
}

fn item(parent: &Parent, recipient: &Bot) -> BotChildAskItem {
    let child_request = ChildRequestId::mint();
    let mut conversation = chat(ChatId::new(), 5);
    conversation.title = "Asked".into();
    conversation.agent_working_folder = Some(recipient.config.working_folder.clone());
    let opening = turn(conversation.id, Uuid::new_v4(), 5);
    let mut submission = parent.submission.clone();
    submission.provenance.bot = recipient.id;
    submission.provenance.submission = BotRequestId::from_uuid(child_request.as_uuid());
    submission.provenance.chat_id = conversation.id.as_uuid();
    submission.provenance.ai_attempt_id = Some(opening.attempt.id.as_uuid());
    submission.provenance.assistant_message_id = Some(opening.assistant_message.id.as_uuid());
    submission.provenance.config_revision = recipient.revision;
    submission.provenance.policy_revision = recipient.policy_revision;
    submission.provenance.parent = Some(d::ChildParent {
        run: parent.run(),
        bot: parent.bot.id,
        chat_id: parent.bot.main_chat.as_uuid(),
        child_request,
    });
    submission.request.run = d::RunId::mint();
    submission.request.scope = d::WorkspaceScope::BotFolder {
        folder: d::BotWorkingFolder::new(recipient.config.working_folder.clone()).unwrap(),
    };
    submission.input.idempotency_key = d::IdempotencyKey::mint();
    assert!(submission.valid());
    BotChildAskItem {
        child_request,
        chat: conversation,
        turn: opening,
        intent: BotSubmissionIntent {
            version: 1,
            submission,
        },
        fingerprint: "b".repeat(64),
        expected_revision: recipient.revision,
    }
}

fn ask(parent: &Parent, items: Vec<BotChildAskItem>) -> BotChildAsk {
    BotChildAsk {
        owner: parent.owner,
        parent_bot: parent.bot.id,
        parent_chat: parent.bot.main_chat,
        parent_run: parent.run(),
        parent_assistant_message: parent.turn.assistant_message.id,
        ask_key: Uuid::new_v4(),
        ask_fingerprint: "c".repeat(64),
        wait: BotWaitId::mint(),
        items,
    }
}

fn admissions(ask: &BotChildAsk) -> Vec<d::BotAdmissionView> {
    ask.items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let p = &item.intent.submission.provenance;
            d::BotAdmissionView {
                key: d::BotSubmissionKey {
                    owner: p.owner,
                    bot: p.bot,
                    request: p.submission,
                },
                run: item.intent.submission.request.run,
                state: d::BotAdmissionState::Queued,
                queue_sequence: index as u64 + 2,
                fence: None,
            }
        })
        .collect()
}

fn count(repo: &Repository, table: &str) -> i64 {
    repo.connection
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn counts(repo: &Repository) -> [i64; 6] {
    [
        "chats",
        "bot_chats",
        "bot_command_receipts",
        "bot_child_chats",
        "messages",
        "bot_chat_runs",
    ]
    .map(|table| count(repo, table))
}

fn prepared(result: BotChildAskPrepared) -> Vec<BotChildChat> {
    match result {
        BotChildAskPrepared::Prepared(rows) => rows,
        other => panic!("not prepared: {other:?}"),
    }
}

fn refused(result: BotChildAskPrepared) -> BotChildAskRefusal {
    match result {
        BotChildAskPrepared::Refused(refusal) => refusal,
        other => panic!("not refused: {other:?}"),
    }
}

fn frozen(parent: &Parent, wait: BotWaitId) -> BotChildWaitCleanupIntentV1 {
    BotChildWaitCleanupIntentV1::frozen(wait, &parent.origin()).unwrap()
}

/// A resume of `wait` acknowledged by a fresh successor of `origin`.
pub(in crate::repository::bots) fn acknowledged(
    wait: BotWaitId,
    origin: DecisionFence,
) -> ChildResume {
    ChildResume {
        wait,
        origin,
        consumer: DecisionConsumer::RecordedSuccessor {
            attempt: d::RunAttemptId::mint(),
            generation: d::AttemptGeneration::from_raw(origin.generation.get() + 1).unwrap(),
            creation_key: d::IdempotencyKey::mint(),
            session: Some(d::SessionId::mint()),
        },
        input_key: d::IdempotencyKey::mint(),
        input_fingerprint: [7; 32],
        state: DecisionResumeState::InputAcknowledged {
            acknowledgment: DecisionInputAcknowledgment {
                turn: d::AgentTurnId::mint(),
                receipt_digest: DecisionDigest::from_bytes([8; 32]),
            },
        },
    }
}

fn fence_of(origin: &DecisionFence) -> BotExecutionFence {
    origin_fence(origin)
}

fn as_origin(fence: &BotExecutionFence) -> DecisionFence {
    DecisionFence {
        owner: fence.owner,
        bot: fence.bot,
        run: fence.run,
        attempt: fence.attempt,
        generation: fence.generation,
        session: fence.session,
        turn: fence.turn.unwrap(),
    }
}

// ---- m6_child_chat_rows ------------------------------------------------------

#[test]
fn m6_child_chat_rows_an_ask_is_prepared_whole_and_replayed_by_its_key() {
    let mut parent = parent();
    let (scout, scribe) = (
        recipient(&mut parent, "scout"),
        recipient(&mut parent, "scribe"),
    );
    let request = ask(&parent, vec![item(&parent, &scout), item(&parent, &scribe)]);
    let before = counts(&parent.repo);
    let rows = prepared(parent.repo.prepare_bot_child_ask(&request, at(6)).unwrap());
    // Two conversations, two receipts, two request rows, two opening turns
    // (a task and a pending reply each) and their two associations.
    let after = counts(&parent.repo);
    assert_eq!(
        [0, 1, 2, 3, 4, 5].map(|i| after[i] - before[i]),
        [2, 2, 2, 2, 4, 2]
    );
    assert_eq!(rows.len(), 2);
    for (index, (row, (item, bot))) in rows
        .iter()
        .zip(request.items.iter().zip([&scout, &scribe]))
        .enumerate()
    {
        assert_eq!(usize::from(row.ordinal), index);
        assert_eq!(row.child_request, item.child_request);
        assert_eq!(row.chat, item.chat.id);
        assert_eq!(row.recipient_bot, bot.id);
        assert_eq!(row.child_run, item.intent.submission.request.run);
        assert_eq!(row.parent_bot, parent.bot.id);
        assert_eq!(row.parent_chat, parent.bot.main_chat);
        assert_eq!(row.parent_run, parent.run());
        assert_eq!(
            row.parent_assistant_message,
            parent.turn.assistant_message.id
        );
        assert_eq!(row.ask_key, request.ask_key);
        assert_eq!(row.wait, request.wait);
        assert_eq!(row.stage, BotReceiptStage::Prepared);
        assert_eq!((row.refusal.clone(), row.refusal_ordinal), (None, None));
        assert!(!row.admitted());
        // The request is the recipient's ordinary `start` receipt.
        let receipt = parent
            .repo
            .bot_admission_receipt(
                parent.owner,
                bot.id,
                BotRequestId::from_uuid(row.child_request.as_uuid()),
            )
            .unwrap()
            .unwrap();
        let BotAdmissionReceipt::Submission(receipt) = receipt else {
            panic!("a child request is a submission receipt");
        };
        assert_eq!(receipt.kind, BotOperationKind::Start);
        assert_eq!(receipt.intent, item.intent);
        assert_eq!(
            parent
                .repo
                .bot_child_chat(parent.owner, row.child_request)
                .unwrap()
                .as_ref(),
            Some(row)
        );
    }
    // The same key and list: the stored ids, and nothing is written.
    let settled = counts(&parent.repo);
    assert_eq!(
        parent.repo.prepare_bot_child_ask(&request, at(7)).unwrap(),
        BotChildAskPrepared::Retained(rows.clone())
    );
    // The same key with fresh ids (a caller that minted again) is still the
    // same ask: the stored ids answer, not the new ones.
    let mut minted_again = ask(&parent, vec![item(&parent, &scout), item(&parent, &scribe)]);
    minted_again.ask_key = request.ask_key;
    assert_eq!(
        parent
            .repo
            .prepare_bot_child_ask(&minted_again, at(7))
            .unwrap(),
        BotChildAskPrepared::Retained(rows.clone())
    );
    // The same key with a different list.
    let mut different = minted_again.clone();
    different.ask_fingerprint = "d".repeat(64);
    assert_eq!(
        refused(
            parent
                .repo
                .prepare_bot_child_ask(&different, at(7))
                .unwrap()
        ),
        BotChildAskRefusal::AskKeyConflict
    );
    assert_eq!(counts(&parent.repo), settled);
    assert_eq!(
        parent
            .repo
            .bot_child_ask(parent.owner, parent.run(), request.ask_key)
            .unwrap(),
        rows
    );
    // No child table holds the task, the title or any other text.
    for table in ["bot_child_chats", "bot_child_wait_enrollments"] {
        let dump: Vec<String> = parent
            .repo
            .connection
            .prepare(&format!("SELECT * FROM {table}"))
            .unwrap()
            .query_map([], |r| {
                Ok((0..r.as_ref().column_count())
                    .map(|i| format!("{:?}", r.get::<_, rusqlite::types::Value>(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for row in dump {
            assert!(!row.contains("Question") && !row.contains("Asked"), "{row}");
        }
    }
}

#[test]
fn m6_child_chat_rows_each_refusal_writes_nothing() {
    let mut parent = parent();
    let scout = recipient(&mut parent, "scout");
    let scribe = recipient(&mut parent, "scribe");
    let before = counts(&parent.repo);
    let sql = |parent: &Parent, sql: &str, bot: &Bot| {
        parent
            .repo
            .connection
            .execute(sql, [bot.id.to_storage()])
            .unwrap();
    };
    // The switch is off.
    sql(
        &parent,
        "UPDATE bots SET may_ask=0 WHERE bot_id=?1",
        &parent.bot.clone(),
    );
    let request = ask(&parent, vec![item(&parent, &scout)]);
    assert_eq!(
        refused(parent.repo.prepare_bot_child_ask(&request, at(6)).unwrap()),
        BotChildAskRefusal::AskNotAllowed
    );
    sql(
        &parent,
        "UPDATE bots SET may_ask=1 WHERE bot_id=?1",
        &parent.bot.clone(),
    );
    // The recipient is paused; the item at fault is named.
    sql(&parent, "UPDATE bots SET paused=1 WHERE bot_id=?1", &scribe);
    let request = ask(&parent, vec![item(&parent, &scout), item(&parent, &scribe)]);
    let refusal = refused(parent.repo.prepare_bot_child_ask(&request, at(6)).unwrap());
    assert_eq!(refusal, BotChildAskRefusal::RecipientPaused { index: 1 });
    assert_eq!(
        (refusal.code(), refusal.index()),
        ("recipient_paused", Some(1))
    );
    sql(&parent, "UPDATE bots SET paused=0 WHERE bot_id=?1", &scribe);
    // Archived, in Trash, another home workspace, another computer, another
    // person, and a Bot that does not exist: one code for all of them.
    let request = ask(&parent, vec![item(&parent, &scout), item(&parent, &scribe)]);
    for change in [
        "UPDATE bots SET lifecycle='archived' WHERE bot_id=?1",
        "UPDATE bots SET lifecycle='trashed' WHERE bot_id=?1",
        "UPDATE bots SET home_workspace_id='00000000-0000-4000-8000-0000000000aa' WHERE bot_id=?1",
        "UPDATE bots SET owner_device_id='00000000-0000-4000-8000-0000000000ab' WHERE bot_id=?1",
        "UPDATE bots SET user_scope_id='00000000-0000-4000-8000-0000000000ac' WHERE bot_id=?1",
    ] {
        sql(&parent, change, &scribe);
        let refusal = refused(parent.repo.prepare_bot_child_ask(&request, at(6)).unwrap());
        assert_eq!(
            refusal,
            BotChildAskRefusal::RecipientUnavailable { index: 1 },
            "{change}"
        );
        assert_eq!(refusal.code(), "recipient_unavailable");
        parent
            .repo
            .connection
            .execute(
                "UPDATE bots SET lifecycle='active',home_workspace_id=?2,owner_device_id=?3,user_scope_id=?4 WHERE bot_id=?1",
                params![
                    scribe.id.to_storage(),
                    scribe.home_workspace.to_storage(),
                    scribe.owner_device.to_storage(),
                    scribe.user_scope.to_storage()
                ],
            )
            .unwrap();
    }
    let mut ghost = scribe.clone();
    ghost.id = BotId::mint();
    let request = ask(&parent, vec![item(&parent, &ghost)]);
    assert_eq!(
        refused(parent.repo.prepare_bot_child_ask(&request, at(6)).unwrap()),
        BotChildAskRefusal::RecipientUnavailable { index: 0 }
    );
    // A Bot never asks itself, and the item is named.
    let mut request = ask(&parent, vec![item(&parent, &scout), item(&parent, &scribe)]);
    request.items[1].intent.submission.provenance.bot = parent.bot.id;
    let refusal = refused(parent.repo.prepare_bot_child_ask(&request, at(6)).unwrap());
    assert_eq!(refusal, BotChildAskRefusal::SelfAsk { index: 1 });
    assert_eq!((refusal.code(), refusal.index()), ("self_ask", Some(1)));
    request.items.truncate(1);
    // The same Bot twice in one call is a malformed request.
    request.items.push(item(&parent, &scout));
    assert_eq!(
        parent.repo.prepare_bot_child_ask(&request, at(6)),
        Err(BotError::Invalid)
    );
    // Four items.
    let extra = [
        recipient(&mut parent, "three"),
        recipient(&mut parent, "four"),
    ];
    let before_four = counts(&parent.repo);
    let request = ask(
        &parent,
        vec![
            item(&parent, &scout),
            item(&parent, &scribe),
            item(&parent, &extra[0]),
            item(&parent, &extra[1]),
        ],
    );
    assert_eq!(
        parent.repo.prepare_bot_child_ask(&request, at(6)),
        Err(BotError::Invalid)
    );
    assert_eq!(counts(&parent.repo), before_four);
    // A failure at the last item leaves nothing of the first: the recipient's
    // configuration moved since the caller read it.
    let mut request = ask(&parent, vec![item(&parent, &scout), item(&parent, &scribe)]);
    request.items[1].expected_revision += 1;
    assert_eq!(
        parent.repo.prepare_bot_child_ask(&request, at(6)),
        Err(BotError::RevisionConflict)
    );
    // A Run that is not this Bot's reply in this conversation cannot ask.
    let mut foreign = ask(&parent, vec![item(&parent, &scout)]);
    foreign.parent_assistant_message = MessageId::new();
    assert_eq!(
        parent.repo.prepare_bot_child_ask(&foreign, at(6)),
        Err(BotError::NotFound)
    );
    // Nothing above wrote a conversation, a receipt, a turn or a request row
    // (the two extra Bots are the only difference).
    let after = counts(&parent.repo);
    assert_eq!(after[3], 0);
    assert_eq!(after, before_four);
    assert_eq!(before_four[0] - before[0], 2, "the two extra Bots' chats");
    // And the next valid ask is admitted whole.
    let request = ask(&parent, vec![item(&parent, &scout), item(&parent, &scribe)]);
    assert_eq!(
        prepared(parent.repo.prepare_bot_child_ask(&request, at(6)).unwrap()).len(),
        2
    );
}

#[test]
fn m6_child_chat_rows_a_refused_asks_chats_are_listed_nowhere() {
    let mut parent = parent();
    let scout = recipient(&mut parent, "scout");
    let listed = |parent: &Parent, bot: &Bot| -> Vec<ChatId> {
        parent
            .repo
            .bot_chat_page(bot.id, None, 100)
            .unwrap()
            .chats
            .into_iter()
            .map(|c| c.chat.id)
            .collect()
    };
    let active = |parent: &Parent| -> Vec<ChatId> {
        parent
            .repo
            .bot_activity(parent.owner, parent.bot.home_workspace, 200)
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.chat_id)
            .collect()
    };
    let request = ask(&parent, vec![item(&parent, &scout)]);
    let child = request.items[0].chat.id;
    let rows = prepared(parent.repo.prepare_bot_child_ask(&request, at(6)).unwrap());
    // Prepared, not yet admitted: its conversation is in no list and on no
    // card, although its pending reply exists.
    assert_eq!(listed(&parent, &scout), vec![scout.main_chat]);
    assert!(!active(&parent).contains(&child));
    assert!(
        parent
            .repo
            .bot_child_requests_of_chat(parent.owner, parent.bot.id, parent.bot.main_chat, 60)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        parent
            .repo
            .bot_child_origin_of_chat(parent.owner, scout.id, child)
            .unwrap(),
        None
    );
    // The delegate refused: every receipt is rejected and the code is kept.
    let rejected = parent
        .repo
        .reject_bot_child_ask(
            parent.owner,
            parent.run(),
            request.ask_key,
            "recipient_full",
            Some(0),
            BotAdmissionRefusal::Capacity,
            at(7),
        )
        .unwrap();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].stage, BotReceiptStage::Rejected);
    assert_eq!(rejected[0].refusal.as_deref(), Some("recipient_full"));
    assert_eq!(rejected[0].refusal_ordinal, Some(0));
    assert_eq!(rejected[0].child_request, rows[0].child_request);
    // The first-task folder reservation the request took is released.
    assert_eq!(
        parent
            .repo
            .bot(scout.id)
            .unwrap()
            .unwrap()
            .folder_lock_request,
        None
    );
    // Still listed nowhere, and the same key answers the same refusal.
    assert_eq!(listed(&parent, &scout), vec![scout.main_chat]);
    assert!(!active(&parent).contains(&child));
    assert_eq!(
        parent.repo.prepare_bot_child_ask(&request, at(8)).unwrap(),
        BotChildAskPrepared::Retained(rejected.clone())
    );
    let again = |parent: &mut Parent, code: &str, ordinal| {
        parent.repo.reject_bot_child_ask(
            parent.owner,
            parent.run(),
            request.ask_key,
            code,
            ordinal,
            BotAdmissionRefusal::Capacity,
            at(9),
        )
    };
    assert_eq!(
        again(&mut parent, "recipient_full", Some(0)),
        Ok(rejected.clone())
    );
    assert_eq!(
        again(&mut parent, "recipient_paused", Some(0)),
        Err(BotError::Conflict)
    );
    assert_eq!(
        again(&mut parent, "recipient_full", None),
        Err(BotError::Conflict)
    );
    assert_eq!(
        again(&mut parent, "Not a code", None),
        Err(BotError::Invalid)
    );
    // A refused ask cannot be accepted afterwards.
    assert_eq!(
        parent.repo.accept_bot_child_ask(
            parent.owner,
            parent.run(),
            request.ask_key,
            &frozen(&parent, request.wait),
            &admissions(&request),
            at(9)
        ),
        Err(BotError::Conflict)
    );
    assert_eq!(count(&parent.repo, "bot_child_wait_enrollments"), 0);

    // An admitted ask is listed: in the recipient's conversations, as a card
    // of the parent's conversation and as the child conversation's origin.
    let admitted = ask(&parent, vec![item(&parent, &scout)]);
    let child = admitted.items[0].chat.id;
    parent
        .repo
        .prepare_bot_child_ask(&admitted, at(10))
        .unwrap();
    let (_, rows) = parent
        .repo
        .accept_bot_child_ask(
            parent.owner,
            parent.run(),
            admitted.ask_key,
            &frozen(&parent, admitted.wait),
            &admissions(&admitted),
            at(11),
        )
        .unwrap();
    assert!(rows[0].admitted());
    assert!(listed(&parent, &scout).contains(&child));
    assert!(active(&parent).contains(&child));
    assert_eq!(
        parent
            .repo
            .bot_child_requests_of_chat(parent.owner, parent.bot.id, parent.bot.main_chat, 60)
            .unwrap(),
        rows
    );
    assert_eq!(
        parent
            .repo
            .bot_child_origin_of_chat(parent.owner, scout.id, child)
            .unwrap()
            .as_ref(),
        Some(&rows[0])
    );
    // The origin is the recipient's conversation's, not any Bot's.
    assert_eq!(
        parent
            .repo
            .bot_child_origin_of_chat(parent.owner, parent.bot.id, child)
            .unwrap(),
        None
    );
    assert_eq!(
        parent
            .repo
            .bot_child_origin_of_chat(parent.owner, scout.id, scout.main_chat)
            .unwrap(),
        None
    );
}

#[test]
fn m6_child_chat_rows_accept_enrolls_in_the_same_transaction_and_is_idempotent() {
    let mut parent = parent();
    let (scout, scribe) = (
        recipient(&mut parent, "scout"),
        recipient(&mut parent, "scribe"),
    );
    let request = ask(&parent, vec![item(&parent, &scout), item(&parent, &scribe)]);
    parent.repo.prepare_bot_child_ask(&request, at(6)).unwrap();
    let intent = frozen(&parent, request.wait);
    // An admission missing for one item: nothing is written, the enrollment
    // included.
    assert_eq!(
        parent.repo.accept_bot_child_ask(
            parent.owner,
            parent.run(),
            request.ask_key,
            &intent,
            &admissions(&request)[..1],
            at(7)
        ),
        Err(BotError::Conflict)
    );
    let mut wrong_run = admissions(&request);
    wrong_run[1].run = d::RunId::mint();
    assert!(
        parent
            .repo
            .accept_bot_child_ask(
                parent.owner,
                parent.run(),
                request.ask_key,
                &intent,
                &wrong_run,
                at(7)
            )
            .is_err()
    );
    assert_eq!(count(&parent.repo, "bot_child_wait_enrollments"), 0);
    assert!(
        parent
            .repo
            .bot_child_ask(parent.owner, parent.run(), request.ask_key)
            .unwrap()
            .iter()
            .all(|row| row.stage == BotReceiptStage::Prepared)
    );
    // An intent for another wait.
    assert_eq!(
        parent.repo.accept_bot_child_ask(
            parent.owner,
            parent.run(),
            request.ask_key,
            &frozen(&parent, BotWaitId::mint()),
            &admissions(&request),
            at(7)
        ),
        Err(BotError::Conflict)
    );
    let (reply, rows) = parent
        .repo
        .accept_bot_child_ask(
            parent.owner,
            parent.run(),
            request.ask_key,
            &intent,
            &admissions(&request),
            at(8),
        )
        .unwrap();
    let key = intent.key().unwrap();
    assert_eq!(
        reply,
        BotChildWaitEnrollmentReply::Enrolled {
            key: key.clone(),
            ai_attempt: parent.attempt().as_uuid(),
            receipt_digest: intent.digest().unwrap(),
        }
    );
    assert!(
        rows.iter()
            .all(|row| row.stage == BotReceiptStage::Completed)
    );
    assert_eq!(count(&parent.repo, "bot_child_wait_enrollments"), 1);
    // A second delivery of the same reply changes nothing and answers the same.
    assert_eq!(
        parent.repo.accept_bot_child_ask(
            parent.owner,
            parent.run(),
            request.ask_key,
            &intent,
            &admissions(&request),
            at(9)
        ),
        Ok((reply.clone(), rows.clone()))
    );
    assert_eq!(count(&parent.repo, "bot_child_wait_enrollments"), 1);
    // The interceptor finds the enrollment by the exact origin fence only.
    assert_eq!(
        parent
            .repo
            .bot_child_wait_enrollment_for_origin(parent.owner, parent.attempt(), &parent.fence)
            .unwrap(),
        Some(key)
    );
    let mut later_turn = parent.fence.clone();
    later_turn.turn = Some(d::AgentTurnId::mint());
    let mut no_turn = parent.fence.clone();
    no_turn.turn = None;
    for other in [later_turn, no_turn] {
        assert_eq!(
            parent
                .repo
                .bot_child_wait_enrollment_for_origin(parent.owner, parent.attempt(), &other)
                .unwrap(),
            None
        );
    }
    // An accepted ask cannot be refused.
    assert_eq!(
        parent.repo.reject_bot_child_ask(
            parent.owner,
            parent.run(),
            request.ask_key,
            "recipient_full",
            None,
            BotAdmissionRefusal::Capacity,
            at(9)
        ),
        Err(BotError::Conflict)
    );
}

#[test]
fn m6_child_chat_rows_the_enrollment_writer_inserts_if_absent_else_requires_the_same_digest() {
    let mut parent = parent();
    let wait = BotWaitId::mint();
    let intent = frozen(&parent, wait);
    let request = EnrollBotChildWaitCleanup {
        key: intent.key().unwrap(),
        cleanup_intent: intent.clone(),
    };
    let read = ReadBotChildWaitCleanup {
        key: request.key.clone(),
    };
    // Absent, and the named origin is the Run's current execution.
    assert_eq!(
        parent.repo.read_bot_child_wait_cleanup(parent.owner, &read),
        Ok(BotChildWaitEnrollmentReply::Missing {
            key: request.key.clone()
        })
    );
    // The route creates the row when the worker's own write was lost.
    let first = parent
        .repo
        .enroll_bot_child_wait(parent.owner, &request, at(6))
        .unwrap();
    assert!(first.matches_request(&request.key, false));
    assert_eq!(count(&parent.repo, "bot_child_wait_enrollments"), 1);
    // The intent was frozen at wait revision 1, and that is what is stored.
    assert_eq!(request.key.wait_revision.get(), 1);
    assert_eq!(
        parent
            .repo
            .connection
            .query_row(
                "SELECT wait_revision FROM bot_child_wait_enrollments",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    // The second writer, whichever it is, is answered from the row. Nothing
    // about the wait's or the Run's current state is compared: the Bot may
    // have been paused and edited and the attempt stopped since.
    parent
        .repo
        .connection
        .execute(
            "UPDATE bots SET policy_revision=policy_revision+1,paused=1 WHERE bot_id=?1",
            [parent.bot.id.to_storage()],
        )
        .unwrap();
    parent
        .repo
        .connection
        .execute(
            "UPDATE inference_attempts SET status='interrupted',stop_reason='human_stop' WHERE id=?1",
            [parent.attempt().to_storage()],
        )
        .unwrap();
    parent
        .repo
        .connection
        .execute(
            "UPDATE bot_run_replay SET stop_reason='ai_restart' WHERE ai_attempt_id=?1",
            [parent.attempt().to_storage()],
        )
        .unwrap();
    assert_eq!(
        parent
            .repo
            .enroll_bot_child_wait(parent.owner, &request, at(7)),
        Ok(first.clone())
    );
    assert_eq!(
        parent.repo.read_bot_child_wait_cleanup(parent.owner, &read),
        Ok(first.clone())
    );
    assert_eq!(count(&parent.repo, "bot_child_wait_enrollments"), 1);
    // A different digest for the same wait is a conflict, never a second row
    // and never an overwrite.
    let mut other_origin = parent.origin();
    other_origin.turn = d::AgentTurnId::mint();
    let other = BotChildWaitCleanupIntentV1::frozen(wait, &other_origin).unwrap();
    assert_ne!(other.digest().unwrap(), intent.digest().unwrap());
    assert_eq!(
        parent.repo.enroll_bot_child_wait(
            parent.owner,
            &EnrollBotChildWaitCleanup {
                key: other.key().unwrap(),
                cleanup_intent: other
            },
            at(8)
        ),
        Err(BotError::Conflict)
    );
    // A key that does not match its intent is malformed.
    let mut crossed = request.clone();
    crossed.key.wait_id = BotWaitId::mint();
    assert_eq!(
        parent
            .repo
            .enroll_bot_child_wait(parent.owner, &crossed, at(8)),
        Err(BotError::Invalid)
    );
    // Another person's owner.
    let stranger = BotOwner {
        user_scope: UserScopeId::new(),
        device: parent.owner.device,
    };
    assert_eq!(
        parent.repo.enroll_bot_child_wait(stranger, &request, at(8)),
        Err(BotError::Forbidden)
    );
    assert_eq!(count(&parent.repo, "bot_child_wait_enrollments"), 1);
}

#[test]
fn m6_child_chat_rows_a_new_enrollment_needs_the_current_origin_and_a_live_attempt() {
    // An origin that is not the Run's current execution.
    let mut stale = parent();
    let mut origin = stale.origin();
    origin.turn = d::AgentTurnId::mint();
    let intent = BotChildWaitCleanupIntentV1::frozen(BotWaitId::mint(), &origin).unwrap();
    let request = EnrollBotChildWaitCleanup {
        key: intent.key().unwrap(),
        cleanup_intent: intent,
    };
    assert_eq!(
        stale
            .repo
            .enroll_bot_child_wait(stale.owner, &request, at(6)),
        Err(BotError::Conflict)
    );
    assert_eq!(
        stale.repo.read_bot_child_wait_cleanup(
            stale.owner,
            &ReadBotChildWaitCleanup {
                key: request.key.clone()
            }
        ),
        Err(BotError::Conflict)
    );
    // A reply that already ended.
    let mut ended = parent();
    ended
        .repo
        .connection
        .execute(
            "UPDATE inference_attempts SET status='interrupted',stop_reason='human_stop' WHERE id=?1",
            [ended.attempt().to_storage()],
        )
        .unwrap();
    let intent = frozen(&ended, BotWaitId::mint());
    assert_eq!(
        ended.repo.enroll_bot_child_wait(
            ended.owner,
            &EnrollBotChildWaitCleanup {
                key: intent.key().unwrap(),
                cleanup_intent: intent
            },
            at(6)
        ),
        Err(BotError::Conflict)
    );
    assert_eq!(count(&stale.repo, "bot_child_wait_enrollments"), 0);
    assert_eq!(count(&ended.repo, "bot_child_wait_enrollments"), 0);
}

#[test]
fn m6_child_chat_rows_two_waits_of_one_run_each_have_a_rollover() {
    let mut parent = parent();
    parent
        .repo
        .connection
        .execute(
            "UPDATE bot_run_replay SET last_sequence=1000 WHERE ai_attempt_id=?1",
            [parent.attempt().to_storage()],
        )
        .unwrap();
    let enroll = |parent: &mut Parent, origin: &DecisionFence| {
        let intent = BotChildWaitCleanupIntentV1::frozen(BotWaitId::mint(), origin).unwrap();
        let key = intent.key().unwrap();
        parent
            .repo
            .enroll_bot_child_wait(
                parent.owner,
                &EnrollBotChildWaitCleanup {
                    key: key.clone(),
                    cleanup_intent: intent,
                },
                at(6),
            )
            .unwrap();
        key
    };
    // The first wait: the admission's own execution is stopped and resumed.
    let origin = parent.origin();
    let first = enroll(&mut parent, &origin);
    let resume = acknowledged(first.wait_id, origin);
    // A resume that is not acknowledged installs nothing.
    let mut unacknowledged = resume.clone();
    unacknowledged.state = DecisionResumeState::InputUnconfirmed {};
    assert_eq!(
        parent.repo.rollover_bot_child_replay(
            parent.owner,
            &first,
            &unacknowledged,
            &fence_of(&origin),
            Some(1000),
            at(7)
        ),
        Err(BotError::Invalid)
    );
    // A cursor that is not the attempt's.
    assert_eq!(
        parent.repo.rollover_bot_child_replay(
            parent.owner,
            &first,
            &resume,
            &fence_of(&origin),
            Some(999),
            at(7)
        ),
        Err(BotError::Conflict)
    );
    assert_eq!(count(&parent.repo, "bot_child_replay_rollovers"), 0);
    let rolled = parent
        .repo
        .rollover_bot_child_replay(
            parent.owner,
            &first,
            &resume,
            &fence_of(&origin),
            Some(1000),
            at(7),
        )
        .unwrap();
    assert_eq!(rolled.wait, first.wait_id);
    assert_eq!(rolled.old_fence, parent.fence);
    assert_eq!(rolled.old_last_sequence, Some(1000));
    // The successor is installed and its cursor starts again.
    let state = parent.repo.bot_replay_state(parent.attempt()).unwrap();
    assert_eq!(state.fence.as_ref(), Some(&rolled.consumer_fence));
    assert_eq!(state.last_sequence, None);
    assert_eq!(rolled.consumer_fence.generation.get(), 2);
    // Exact replay answers the retained rollover; another successor for the
    // same wait is a conflict.
    assert_eq!(
        parent.repo.rollover_bot_child_replay(
            parent.owner,
            &first,
            &resume,
            &fence_of(&origin),
            Some(1000),
            at(8)
        ),
        Ok(rolled.clone())
    );
    assert_eq!(
        parent.repo.rollover_bot_child_replay(
            parent.owner,
            &first,
            &acknowledged(first.wait_id, origin),
            &fence_of(&origin),
            Some(1000),
            at(8)
        ),
        Err(BotError::Conflict)
    );
    // The stopped origin no longer matches the interceptor's lookup of a
    // *new* wait, and a wait whose origin is the retired execution cannot
    // enroll: the Run's current execution is the successor.
    let retired = BotChildWaitCleanupIntentV1::frozen(BotWaitId::mint(), &origin).unwrap();
    assert_eq!(
        parent.repo.enroll_bot_child_wait(
            parent.owner,
            &EnrollBotChildWaitCleanup {
                key: retired.key().unwrap(),
                cleanup_intent: retired
            },
            at(8)
        ),
        Err(BotError::Conflict)
    );
    // The second wait of the same Run: its origin is the successor the first
    // wait installed, which the installed-consumer read accepts.
    let second_origin = as_origin(&rolled.consumer_fence);
    let second = enroll(&mut parent, &second_origin);
    let second_resume = acknowledged(second.wait_id, second_origin);
    let rolled_again = parent
        .repo
        .rollover_bot_child_replay(
            parent.owner,
            &second,
            &second_resume,
            &rolled.consumer_fence,
            None,
            at(9),
        )
        .unwrap();
    assert_eq!(rolled_again.consumer_fence.generation.get(), 3);
    assert_eq!(count(&parent.repo, "bot_child_wait_enrollments"), 2);
    assert_eq!(count(&parent.repo, "bot_child_replay_rollovers"), 2);
    assert_eq!(
        parent
            .repo
            .bot_replay_state(parent.attempt())
            .unwrap()
            .fence
            .as_ref(),
        Some(&rolled_again.consumer_fence)
    );
    // Each wait's interceptor lookup still names its own origin.
    for (fence, key) in [(&parent.fence, &first), (&rolled.consumer_fence, &second)] {
        assert_eq!(
            parent
                .repo
                .bot_child_wait_enrollment_for_origin(parent.owner, parent.attempt(), fence)
                .unwrap()
                .as_ref(),
            Some(key)
        );
    }
    // A rollover without its wait's enrollment does not exist.
    let orphan = BotChildWaitCleanupIntentV1::frozen(
        BotWaitId::mint(),
        &as_origin(&rolled_again.consumer_fence),
    )
    .unwrap()
    .key()
    .unwrap();
    assert_eq!(
        parent.repo.rollover_bot_child_replay(
            parent.owner,
            &orphan,
            &acknowledged(orphan.wait_id, orphan.origin()),
            &rolled_again.consumer_fence,
            None,
            at(9)
        ),
        Err(BotError::NotFound)
    );
}

// ---- m6_may_ask --------------------------------------------------------------

#[test]
fn m6_may_ask_is_on_by_default_and_changes_only_with_an_access_change() {
    let mut parent = parent();
    let owner = parent.owner;
    let bot = parent.bot.clone();
    assert!(bot.may_ask, "a Bot may ask by default");
    assert!(
        parent
            .repo
            .bot_policy_metadata(owner, bot.id)
            .unwrap()
            .may_ask
    );
    let policy_before = parent
        .repo
        .bot_policy_metadata(owner, bot.id)
        .unwrap()
        .policy();
    // An ordinary configuration edit cannot move the switch.
    let mut edited = bot.clone();
    edited.revision += 1;
    edited.updated_at_ms += 1;
    edited.may_ask = false;
    assert_eq!(
        parent.repo.save_bot(
            &edited,
            ProfileId::new(),
            BotRequestId::mint(),
            &"e".repeat(64),
            Some(bot.revision)
        ),
        Err(BotError::Invalid)
    );
    assert!(parent.repo.bot(bot.id).unwrap().unwrap().may_ask);
    // An access change does, and bumps both revisions as every access edit.
    let mut desired = bot.clone();
    desired.revision += 1;
    desired.policy_revision += 1;
    desired.updated_at_ms += 1;
    desired.may_ask = false;
    let barrier = d::BotPolicyBarrier {
        key: d::BotPolicyBarrierKey {
            owner: d::BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            },
            bot: bot.id,
            request: "12345678-1234-8234-8234-123456789abc".parse().unwrap(),
        },
        config_revision: desired.revision,
        policy_revision: desired.policy_revision,
        policy: bot_native_policy(&desired),
        effects_allowed: true,
    };
    // The switch is not part of the captured policy: the barrier's policy is
    // byte for byte the one the Bot had.
    assert_eq!(barrier.policy, policy_before);
    assert_eq!(
        serde_json::to_vec(&barrier.policy).unwrap(),
        serde_json::to_vec(&bot_native_policy(&bot)).unwrap()
    );
    let request = BotRequestId::mint();
    let receipt = parent
        .repo
        .prepare_bot_access(
            &desired,
            ProfileId::new(),
            request,
            &"f".repeat(64),
            bot.revision,
            &barrier,
        )
        .unwrap();
    assert!(!receipt.bot.may_ask);
    let stored = parent.repo.bot(bot.id).unwrap().unwrap();
    assert!(!stored.may_ask);
    assert_eq!(
        (stored.revision, stored.policy_revision),
        (bot.revision + 1, bot.policy_revision + 1)
    );
    let metadata = parent.repo.bot_policy_metadata(owner, bot.id).unwrap();
    assert!(!metadata.may_ask);
    assert_eq!(metadata.policy(), policy_before);
    // The stored receipt of the Bot omits the field only while it is on.
    let json: String = parent
        .repo
        .connection
        .query_row(
            "SELECT outcome_json FROM bot_command_receipts WHERE request_id=?1",
            [request.to_storage()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(json.contains("\"may_ask\":false"), "{json}");
    let created: String = parent
        .repo
        .connection
        .query_row(
            "SELECT outcome_json FROM bot_command_receipts WHERE bot_id=?1 AND command_kind='create'",
            [bot.id.to_storage()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!created.contains("may_ask"), "{created}");
}

// ---- m6_activity_waiting -----------------------------------------------------

#[test]
fn m6_activity_waiting_names_the_bots_asked_and_says_paused() {
    let mut parent = parent();
    let (scout, scribe) = (
        recipient(&mut parent, "scout"),
        recipient(&mut parent, "scribe"),
    );
    let activity = |parent: &Parent| -> Vec<BotActivity> {
        parent
            .repo
            .bot_activity(parent.owner, parent.bot.home_workspace, 200)
            .unwrap()
            .entries
    };
    let of = |parent: &Parent, chat: ChatId| -> Option<BotActivity> {
        activity(parent).into_iter().find(|e| e.chat_id == chat)
    };
    let main = parent.bot.main_chat;
    // Before the ask the reply is simply running.
    assert_eq!(of(&parent, main).unwrap().state, BotActivityState::Running);
    assert_eq!(of(&parent, main).unwrap().waiting, None);
    let request = ask(&parent, vec![item(&parent, &scout), item(&parent, &scribe)]);
    parent.repo.prepare_bot_child_ask(&request, at(6)).unwrap();
    // Prepared only: the parent does not wait yet, and no child is shown.
    assert_eq!(of(&parent, main).unwrap().state, BotActivityState::Running);
    assert_eq!(activity(&parent).len(), 1);
    parent
        .repo
        .accept_bot_child_ask(
            parent.owner,
            parent.run(),
            request.ask_key,
            &frozen(&parent, request.wait),
            &admissions(&request),
            at(7),
        )
        .unwrap();
    // The parent waits on both Bots, in the order it named them; each child
    // conversation is running.
    let waiting = of(&parent, main).unwrap();
    assert_eq!(waiting.state, BotActivityState::Waiting);
    assert_eq!(
        waiting.waiting,
        Some(BotActivityWaiting {
            on: vec![scout.id, scribe.id],
            paused: false
        })
    );
    for item in &request.items {
        let child = of(&parent, item.chat.id).unwrap();
        assert_eq!(child.state, BotActivityState::Running);
        assert_eq!(child.waiting, None);
    }
    // The wire form: a token and the detail, and nothing new on other rows.
    assert_eq!(
        serde_json::to_value(&waiting).unwrap(),
        serde_json::json!({
            "bot_id": parent.bot.id,
            "chat_id": main,
            "state": "waiting",
            "waiting": {"on": [scout.id, scribe.id], "paused": false}
        })
    );
    assert!(
        serde_json::to_value(of(&parent, request.items[0].chat.id).unwrap())
            .unwrap()
            .get("waiting")
            .is_none()
    );
    // A paused parent Bot's wait says so: it has no time limit.
    parent
        .repo
        .connection
        .execute(
            "UPDATE bots SET paused=1 WHERE bot_id=?1",
            [parent.bot.id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        of(&parent, main).unwrap().waiting,
        Some(BotActivityWaiting {
            on: vec![scout.id, scribe.id],
            paused: true
        })
    );
    parent
        .repo
        .connection
        .execute(
            "UPDATE bots SET paused=0 WHERE bot_id=?1",
            [parent.bot.id.to_storage()],
        )
        .unwrap();
    // One child's reply ended: the parent waits on the other alone.
    parent
        .repo
        .connection
        .execute(
            "UPDATE messages SET state='complete',completed_at_ms=?2 WHERE id=?1",
            params![
                request.items[0].turn.assistant_message.id.to_storage(),
                at(9).millis()
            ],
        )
        .unwrap();
    assert_eq!(
        of(&parent, main).unwrap().waiting,
        Some(BotActivityWaiting {
            on: vec![scribe.id],
            paused: false
        })
    );
    // The successor is installed: the reply is running again.
    let origin = parent.origin();
    let key = frozen(&parent, request.wait).key().unwrap();
    parent
        .repo
        .rollover_bot_child_replay(
            parent.owner,
            &key,
            &acknowledged(request.wait, origin),
            &fence_of(&origin),
            None,
            at(10),
        )
        .unwrap();
    let resumed = of(&parent, main).unwrap();
    assert_eq!(resumed.state, BotActivityState::Running);
    assert_eq!(resumed.waiting, None);
    // The waiting probe reads the reply's own Run through the enrollment
    // index and never a conversation's history.
    let plan: Vec<String> = parent
        .repo
        .connection
        .prepare(&format!("EXPLAIN QUERY PLAN {}", reads::BOT_ACTIVITY_SQL))
        .unwrap()
        .query_map(
            params![
                parent.owner.user_scope.to_storage(),
                parent.owner.device.to_storage(),
                parent.bot.home_workspace.to_storage()
            ],
            |r| r.get::<_, String>(3),
        )
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let plan = plan.join(" | ");
    assert!(plan.contains("bot_child_wait_enrollments_run"), "{plan}");
    assert!(
        !plan.contains("SCAN we") && !plan.contains("SCAN cc"),
        "{plan}"
    );
}

#[test]
fn m6_activity_waiting_a_card_open_for_the_person_outranks_the_wait() {
    let mut parent = parent();
    let scout = recipient(&mut parent, "scout");
    let request = ask(&parent, vec![item(&parent, &scout)]);
    parent.repo.prepare_bot_child_ask(&request, at(6)).unwrap();
    parent
        .repo
        .accept_bot_child_ask(
            parent.owner,
            parent.run(),
            request.ask_key,
            &frozen(&parent, request.wait),
            &admissions(&request),
            at(7),
        )
        .unwrap();
    let main = parent.bot.main_chat;
    let of = |parent: &Parent| -> BotActivity {
        parent
            .repo
            .bot_activity(parent.owner, parent.bot.home_workspace, 200)
            .unwrap()
            .entries
            .into_iter()
            .find(|e| e.chat_id == main)
            .unwrap()
    };
    assert_eq!(of(&parent).state, BotActivityState::Waiting);
    parent
        .repo
        .connection
        .execute(
            "INSERT INTO tool_invocations(id,attempt_id,chat_id,tool_name,read_only,status,approval,proposed_at_ms) VALUES(?1,?2,?3,'agent',0,'approval_required','pending',9)",
            params![
                ToolInvocationId::new().to_storage(),
                parent.attempt().to_storage(),
                main.to_storage()
            ],
        )
        .unwrap();
    let needs = of(&parent);
    assert_eq!(needs.state, BotActivityState::NeedsYou);
    assert_eq!(needs.waiting, None);
    // The single-conversation read still says on whom it waits.
    assert_eq!(
        parent
            .repo
            .bot_child_waiting(parent.owner, parent.bot.id, main)
            .unwrap()
            .map(|w| (w.recipients, w.requests, w.paused, w.parent_run)),
        Some((
            vec![scout.id],
            vec![request.items[0].child_request],
            false,
            parent.run()
        ))
    );
}

#[test]
fn m6_activity_waiting_core_tokens_are_stable() {
    assert_eq!(
        serde_json::to_value(BotActivityState::Waiting).unwrap(),
        serde_json::json!("waiting")
    );
    // An entry that does not wait serializes exactly as before M6.
    let entry = BotActivity {
        bot_id: BotId::mint(),
        chat_id: ChatId::new(),
        state: BotActivityState::Running,
        waiting: None,
    };
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(
        json.as_object().unwrap().keys().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            &"bot_id".to_owned(),
            &"chat_id".to_owned(),
            &"state".to_owned()
        ])
    );
    assert_eq!(serde_json::from_value::<BotActivity>(json).unwrap(), entry);
}
