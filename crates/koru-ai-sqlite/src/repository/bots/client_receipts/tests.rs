// SPDX-License-Identifier: MPL-2.0
//! The receipts of a linked desktop's commands at the repository: cohort
//! `m8_owner_receipts`, the half that needs no worker. An in-memory database
//! and nothing else.
use super::super::tests::fixture_bot;
use super::*;

const NOW: i64 = 1_770_000_000_000;

fn boot(index: u32) -> String {
    format!("{index:032x}")
}

fn fingerprint(index: u8) -> String {
    format!("{index:02x}").repeat(32)
}

fn peer() -> DeviceId {
    DeviceId::from_uuid(Uuid::new_v4())
}

fn row_count(repo: &Repository) -> i64 {
    repo.connection
        .query_row("SELECT count(*) FROM bot_client_receipts", [], |row| {
            row.get(0)
        })
        .unwrap()
}

/// `m8_owner_receipts`: the first arrival is recorded and applied; a repeat
/// of a closed request is answered from the row with its outcome, its code
/// and its cursor, and never asks to be applied.
#[test]
fn m8_owner_receipts_a_closed_request_is_answered_from_its_row() {
    let mut repo = Repository::in_memory().unwrap();
    let (peer, request) = (peer(), Uuid::new_v4());
    assert_eq!(
        repo.client_receipt_begin(peer, request, &fingerprint(1), "pause_bot", &boot(1), NOW)
            .unwrap(),
        ClientReceiptBegin::Apply
    );
    let head = repo.feed_head().unwrap();
    let closed = repo
        .client_receipt_finish(peer, request, ClientOutcome::Accepted, None, NOW + 5)
        .unwrap();
    assert_eq!(
        closed,
        ClientReceipt {
            outcome: ClientOutcome::Accepted,
            failure_code: None,
            feed_cursor: Some(head),
        }
    );
    // The same request again, from this start and from a later one.
    for start in [boot(1), boot(2)] {
        assert_eq!(
            repo.client_receipt_begin(peer, request, &fingerprint(1), "pause_bot", &start, NOW)
                .unwrap(),
            ClientReceiptBegin::Done(closed.clone())
        );
    }
    // The first outcome stands: a second close changes nothing.
    assert_eq!(
        repo.client_receipt_finish(
            peer,
            request,
            ClientOutcome::Refused,
            Some("not_found"),
            NOW + 9
        )
        .unwrap(),
        closed
    );
    assert_eq!(row_count(&repo), 1);
}

/// `m8_owner_receipts`: a refusal is stored with its code and with no
/// cursor, since it changed nothing.
#[test]
fn m8_owner_receipts_a_refusal_keeps_its_code_and_no_cursor() {
    let mut repo = Repository::in_memory().unwrap();
    let (peer, request) = (peer(), Uuid::new_v4());
    repo.client_receipt_begin(peer, request, &fingerprint(2), "send_turn", &boot(1), NOW)
        .unwrap();
    let closed = repo
        .client_receipt_finish(
            peer,
            request,
            ClientOutcome::Refused,
            Some("team_paused"),
            NOW,
        )
        .unwrap();
    assert_eq!(
        closed,
        ClientReceipt {
            outcome: ClientOutcome::Refused,
            failure_code: Some("team_paused".into()),
            feed_cursor: None,
        }
    );
    // A code that is not a short token is refused, and the row stays open.
    let other = Uuid::new_v4();
    repo.client_receipt_begin(peer, other, &fingerprint(2), "send_turn", &boot(1), NOW)
        .unwrap();
    assert_eq!(
        repo.client_receipt_finish(
            peer,
            other,
            ClientOutcome::Refused,
            Some("a sentence somebody typed"),
            NOW
        ),
        Err(BotError::Invalid)
    );
    assert_eq!(
        repo.client_receipt_begin(peer, other, &fingerprint(2), "send_turn", &boot(1), NOW)
            .unwrap(),
        ClientReceiptBegin::InFlight
    );
}

/// `m8_owner_receipts`: the same request id with other content is a
/// conflict, whatever state the row is in, and changes nothing.
#[test]
fn m8_owner_receipts_the_same_id_with_other_content_is_a_conflict() {
    let mut repo = Repository::in_memory().unwrap();
    let (peer, request) = (peer(), Uuid::new_v4());
    repo.client_receipt_begin(peer, request, &fingerprint(3), "send_turn", &boot(1), NOW)
        .unwrap();
    assert_eq!(
        repo.client_receipt_begin(peer, request, &fingerprint(4), "send_turn", &boot(1), NOW)
            .unwrap(),
        ClientReceiptBegin::Conflict
    );
    repo.client_receipt_finish(peer, request, ClientOutcome::Completed, None, NOW)
        .unwrap();
    assert_eq!(
        repo.client_receipt_begin(peer, request, &fingerprint(4), "send_turn", &boot(2), NOW)
            .unwrap(),
        ClientReceiptBegin::Conflict
    );
    // Another device may use the same request id: the key holds the peer.
    assert_eq!(
        repo.client_receipt_begin(
            self::peer(),
            request,
            &fingerprint(4),
            "send_turn",
            &boot(2),
            NOW
        )
        .unwrap(),
        ClientReceiptBegin::Apply
    );
}

/// `m8_owner_receipts`: a request left `applying` by this start is in
/// flight; left by an earlier start it is stranded, with the kind that was
/// recorded, and it stays so until it is closed.
#[test]
fn m8_owner_receipts_an_open_row_is_in_flight_for_its_own_start_and_stranded_after() {
    let mut repo = Repository::in_memory().unwrap();
    let (peer, request) = (peer(), Uuid::new_v4());
    repo.client_receipt_begin(peer, request, &fingerprint(5), "steer", &boot(1), NOW)
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            repo.client_receipt_begin(peer, request, &fingerprint(5), "steer", &boot(1), NOW)
                .unwrap(),
            ClientReceiptBegin::InFlight
        );
    }
    for _ in 0..2 {
        assert_eq!(
            repo.client_receipt_begin(peer, request, &fingerprint(5), "steer", &boot(2), NOW)
                .unwrap(),
            ClientReceiptBegin::Stranded {
                kind: "steer".into()
            }
        );
    }
    let closed = repo
        .client_receipt_finish(peer, request, ClientOutcome::Unknown, None, NOW)
        .unwrap();
    assert_eq!(
        closed,
        ClientReceipt {
            outcome: ClientOutcome::Unknown,
            failure_code: None,
            feed_cursor: None,
        }
    );
    assert_eq!(
        repo.client_receipt_begin(peer, request, &fingerprint(5), "steer", &boot(3), NOW)
            .unwrap(),
        ClientReceiptBegin::Done(closed)
    );
}

/// `m8_owner_receipts`: malformed input writes nothing, and closing a
/// request that was never recorded is `NotFound`.
#[test]
fn m8_owner_receipts_malformed_input_writes_nothing() {
    let mut repo = Repository::in_memory().unwrap();
    let (peer, request) = (peer(), Uuid::new_v4());
    for (fingerprint, kind, start) in [
        ("short".to_owned(), "steer", boot(1)),
        (fingerprint(1), "", boot(1)),
        (fingerprint(1), "Not A Kind", boot(1)),
        (fingerprint(1), "steer", "boot".to_owned()),
    ] {
        assert_eq!(
            repo.client_receipt_begin(peer, request, &fingerprint, kind, &start, NOW),
            Err(BotError::Invalid)
        );
    }
    assert_eq!(row_count(&repo), 0);
    assert_eq!(
        repo.client_receipt_finish(peer, request, ClientOutcome::Completed, None, NOW),
        Err(BotError::NotFound)
    );
}

/// Receipts older than thirty days are deleted when a new one is written,
/// at most two hundred at a time, by the clock the caller passes.
#[test]
fn m8_owner_receipts_old_receipts_are_deleted_two_hundred_at_a_time() {
    let mut repo = Repository::in_memory().unwrap();
    let peer = peer();
    let old = NOW - CLIENT_RECEIPT_KEEP_MS - 1;
    for index in 0..(CLIENT_RECEIPT_PRUNE_BATCH + 50) {
        repo.client_receipt_begin(
            peer,
            Uuid::new_v4(),
            &fingerprint(6),
            "send_turn",
            &boot(1),
            old - index,
        )
        .unwrap();
    }
    // Each of those was written with a clock at which nothing was old.
    assert_eq!(row_count(&repo), CLIENT_RECEIPT_PRUNE_BATCH + 50);
    // The next write, thirty days later, deletes two hundred of them.
    let recent = Uuid::new_v4();
    repo.client_receipt_begin(
        peer,
        recent,
        &fingerprint(6),
        "send_turn",
        &boot(1),
        NOW - 1,
    )
    .unwrap();
    assert_eq!(row_count(&repo), 51);
    // The one after deletes the fifty that were left, and the one after
    // that deletes nothing: what is inside the thirty days stays.
    for expected in [2, 3] {
        repo.client_receipt_begin(
            peer,
            Uuid::new_v4(),
            &fingerprint(6),
            "send_turn",
            &boot(1),
            NOW,
        )
        .unwrap();
        assert_eq!(row_count(&repo), expected);
    }
    assert_eq!(
        repo.client_receipt_begin(peer, recent, &fingerprint(6), "send_turn", &boot(1), NOW)
            .unwrap(),
        ClientReceiptBegin::InFlight
    );
}

/// A receipt write is not a Bot write: it moves no feed marker, and the
/// audit, which looks at every commit of this database, refuses none of it.
#[test]
fn m8_owner_receipts_a_receipt_write_moves_no_marker() {
    let mut repo = Repository::in_memory().unwrap();
    let bot = fixture_bot();
    repo.save_bot(&bot, ProfileId::new(), BotRequestId::mint(), "create", None)
        .unwrap();
    let head = repo.feed_head().unwrap();
    let (peer, request) = (peer(), Uuid::new_v4());
    repo.client_receipt_begin(peer, request, &fingerprint(7), "pause_bot", &boot(1), NOW)
        .unwrap();
    repo.client_receipt_finish(peer, request, ClientOutcome::Completed, None, NOW)
        .unwrap();
    assert_eq!(repo.feed_head().unwrap(), head);
    assert!(repo.feed_audit_refusals().is_empty());
}

/// The settle read: a Bot command's own receipt is found by the person and
/// the request id, with whether it still waits for its owner; a request
/// that wrote none answers nothing.
#[test]
fn m8_owner_receipts_a_bot_commands_own_receipt_settles_a_stranded_request() {
    let mut repo = Repository::in_memory().unwrap();
    let bot = fixture_bot();
    let created = BotRequestId::mint();
    repo.save_bot(&bot, ProfileId::new(), created, "create", None)
        .unwrap();
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let pause = BotRequestId::mint();
    repo.prepare_bot_pause(owner, bot.id, pause, 1, true, &fingerprint(8), NOW)
        .unwrap();
    assert_eq!(
        repo.client_settle_bot_request(bot.user_scope, pause.as_uuid())
            .unwrap(),
        Some(ClientBotRequest {
            kind: "pause".into(),
            pending: true,
        })
    );
    assert_eq!(
        repo.client_settle_bot_request(bot.user_scope, created.as_uuid())
            .unwrap()
            .map(|found| found.pending),
        Some(false)
    );
    // Another person's request id, and one nothing recorded.
    assert_eq!(
        repo.client_settle_bot_request(UserScopeId::new(), pause.as_uuid())
            .unwrap(),
        None
    );
    assert_eq!(
        repo.client_settle_bot_request(bot.user_scope, Uuid::new_v4())
            .unwrap(),
        None
    );
}

/// What a linked desktop's command may name through a conversation: a Bot
/// conversation a copy shows, and nothing else.
#[test]
fn m8_owner_authority_only_a_shown_bot_conversation_names_its_bot() {
    let mut repo = Repository::in_memory().unwrap();
    let bot = fixture_bot();
    repo.save_bot(&bot, ProfileId::new(), BotRequestId::mint(), "create", None)
        .unwrap();
    assert_eq!(repo.client_chat_bot(bot.main_chat).unwrap(), Some(bot.id));
    // An ordinary chat of the person, and an id nothing has.
    let ordinary = ChatId::new();
    repo.create_chat(&crate::repository::tests::chat(ordinary, 0))
        .unwrap();
    assert_eq!(repo.client_chat_bot(ordinary).unwrap(), None);
    assert_eq!(repo.client_chat_bot(ChatId::new()).unwrap(), None);
    // A conversation that is not shown: its marker is written removed.
    repo.fixture_execute_batch(&format!(
        "UPDATE bot_client_feed SET removed = 1 WHERE kind = 'chat' AND subject_id = '{}'",
        bot.main_chat.to_storage()
    ))
    .unwrap();
    assert_eq!(repo.client_chat_bot(bot.main_chat).unwrap(), None);
}
