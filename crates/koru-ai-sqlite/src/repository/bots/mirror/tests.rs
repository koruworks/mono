// SPDX-License-Identifier: MPL-2.0
//! The copy of another computer's Bots at the repository: the halves of
//! cohorts `m8_mirror_apply`, `m8_mirror_reset`, `m8_mirror_counts`,
//! `m8_mirror_never_executes`, `m8_client_commands` and
//! `m8_removal_on_revocation` that need no worker and no stream. An
//! in-memory database and nothing else.
use super::super::tests::fixture_bot;
use super::*;

const NOW: i64 = 1_770_000_000_000;

fn token(index: u32) -> String {
    format!("{index:032x}")
}

fn device() -> DeviceId {
    DeviceId::from_uuid(Uuid::new_v4())
}

/// A copy with nothing in it yet.
fn owned(repo: &mut Repository) -> DeviceId {
    let owner = device();
    let row = repo.mirror_ensure_owner(owner, NOW).unwrap();
    assert_eq!(row.state, LinkedOwnerState::Never);
    assert_eq!((row.generation, row.cursor, row.staging), (0, 0, None));
    owner
}

fn page(owner: DeviceId, from: u64, to: u64, changes: Vec<MirrorChange>) -> MirrorPage {
    MirrorPage {
        owner_device: owner,
        feed_id: token(1),
        boot_id: token(2),
        protocol_version: 1,
        replica_format: 1,
        from_cursor: from,
        to_cursor: to,
        reset: false,
        caught_up: None,
        changes,
    }
}

fn reset(mut page: MirrorPage) -> MirrorPage {
    page.reset = true;
    page
}

fn caught_up(mut page: MirrorPage, withheld: u32) -> MirrorPage {
    page.caught_up = Some(withheld);
    page
}

fn bot(seq: u64, bot_id: BotId) -> MirrorChange {
    MirrorChange::Bot {
        seq,
        bot_id,
        home_workspace: WorkspaceId::new(),
        lifecycle: "active".into(),
        revision: 1,
        record_json: format!(r#"{{"format":1,"seq":{seq}}}"#),
    }
}

fn chat(seq: u64, chat_id: ChatId, bot_id: BotId) -> MirrorChange {
    MirrorChange::Chat {
        seq,
        chat_id,
        bot_id,
        purpose: "main".into(),
        trashed: false,
        updated_at_ms: NOW,
        record_json: format!(
            r#"{{"format":1,"seq":{seq},"activity":{{"running":false,"needs_you":false}}}}"#
        ),
    }
}

fn message(seq: u64, message_id: MessageId, chat_id: ChatId, ordinal: u32) -> MirrorChange {
    MirrorChange::Message {
        seq,
        message_id,
        chat_id,
        bot_id: None,
        ordinal,
        record_json: format!(r#"{{"format":1,"seq":{seq}}}"#),
    }
}

/// One Bot with its conversation and two messages, as four entries.
struct Family {
    bot: BotId,
    chat: ChatId,
    messages: [MessageId; 2],
}

impl Family {
    fn new() -> Self {
        Self {
            bot: BotId::mint(),
            chat: ChatId::new(),
            messages: [MessageId::new(), MessageId::new()],
        }
    }

    fn changes(&self, first_seq: u64) -> Vec<MirrorChange> {
        vec![
            bot(first_seq, self.bot),
            chat(first_seq + 1, self.chat, self.bot),
            message(first_seq + 2, self.messages[0], self.chat, 0),
            message(first_seq + 3, self.messages[1], self.chat, 1),
        ]
    }
}

/// Every table that is not one of the six client tables, with every row of
/// it, as text. Two equal dumps mean no canonical row was written.
fn canonical(repo: &Repository) -> Vec<(String, Vec<String>)> {
    let tables = repo.canonical_rows_for_fixture();
    for expected in [
        "bots",
        "chats",
        "messages",
        "inference_attempts",
        "bot_client_feed",
    ] {
        assert!(
            tables.iter().any(|(table, _)| table == expected),
            "{expected}"
        );
    }
    assert!(
        !tables
            .iter()
            .any(|(table, _)| table.starts_with("bot_mirror_"))
    );
    tables
}

/// `m8_mirror_apply`: a page writes every record and the cursor together;
/// the copy then reads back in the order a conversation reads.
#[test]
fn m8_mirror_apply_a_page_writes_its_records_and_its_cursor() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owned(&mut repo);
    let family = Family::new();
    let applied = repo
        .mirror_apply_page(
            &reset(caught_up(page(owner, 0, 4, family.changes(1)), 2)),
            NOW,
        )
        .unwrap();
    assert_eq!((applied.cursor, applied.switched), (4, true));
    assert_eq!(
        applied.delta,
        MirrorCounts {
            bots: 1,
            chats: 1,
            messages: 2
        }
    );
    let row = repo.mirror_owner(owner).unwrap().unwrap();
    assert_eq!(row.state, LinkedOwnerState::Current);
    assert_eq!((row.cursor, row.generation, row.staging), (4, 1, None));
    assert_eq!(row.feed_id.as_deref(), Some(token(1).as_str()));
    assert_eq!(row.boot_id.as_deref(), Some(token(2).as_str()));
    assert_eq!(row.withheld_bots, 2);
    assert_eq!(row.last_confirmed_at_ms, Some(NOW));
    assert_eq!(repo.mirror_bots(owner, None).unwrap().len(), 1);
    assert_eq!(
        repo.mirror_chats(owner, family.bot, false).unwrap().len(),
        1
    );
    assert!(
        repo.mirror_chats(owner, family.bot, true)
            .unwrap()
            .is_empty()
    );
    let messages = repo.mirror_messages(owner, family.chat).unwrap();
    assert_eq!(messages.len(), 2);
    assert!(messages[0].contains(r#""seq":3"#) && messages[1].contains(r#""seq":4"#));
    // A later page continues from the cursor and rewrites one record.
    let next = page(
        owner,
        4,
        5,
        vec![message(5, family.messages[0], family.chat, 0)],
    );
    let applied = repo.mirror_apply_page(&next, NOW + 1).unwrap();
    assert_eq!(applied.delta, MirrorCounts::default());
    assert!(repo.mirror_messages(owner, family.chat).unwrap()[0].contains(r#""seq":5"#));
    assert_eq!(repo.mirror_owner(owner).unwrap().unwrap().cursor, 5);
}

/// `m8_mirror_apply`: a page that fails anywhere leaves nothing: not the
/// records before the failing one, and not the cursor.
#[test]
fn m8_mirror_apply_a_page_is_applied_whole_or_not_at_all() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owned(&mut repo);
    let family = Family::new();
    repo.mirror_apply_page(
        &reset(caught_up(page(owner, 0, 4, family.changes(1)), 0)),
        NOW,
    )
    .unwrap();
    let before = repo.mirror_row_counts_for_fixture();
    let other = Family::new();
    let mut changes = other.changes(5);
    // The last record of the page is not JSON.
    changes.push(MirrorChange::Team {
        seq: 9,
        record_json: "not json".into(),
    });
    assert_eq!(
        repo.mirror_apply_page(&page(owner, 4, 9, changes), NOW + 1),
        Err(MirrorRefusal::Malformed)
    );
    assert_eq!(repo.mirror_row_counts_for_fixture(), before);
    assert_eq!(repo.mirror_owner(owner).unwrap().unwrap().cursor, 4);
    assert_eq!(
        repo.mirror_place(MirrorSubject::Bot(other.bot)).unwrap(),
        MirrorPlace::Nowhere
    );
    // A record over the size the tables hold is refused the same way.
    let huge = MirrorChange::Team {
        seq: 5,
        record_json: format!(r#"{{"pad":"{}"}}"#, "x".repeat(70_000)),
    };
    assert_eq!(
        repo.mirror_apply_page(&page(owner, 4, 5, vec![huge]), NOW + 1),
        Err(MirrorRefusal::Malformed)
    );
    assert_eq!(repo.mirror_row_counts_for_fixture(), before);
}

/// `m8_mirror_apply`: the checks only the database can make. A page that
/// is not the next one, a page of another feed, a page for an owner there
/// is no copy of, and a page whose tokens are not tokens are each refused
/// with nothing written.
#[test]
fn m8_mirror_apply_each_page_check_refuses_and_writes_nothing() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owned(&mut repo);
    let family = Family::new();
    repo.mirror_apply_page(
        &reset(caught_up(page(owner, 0, 4, family.changes(1)), 0)),
        NOW,
    )
    .unwrap();
    let before = repo.mirror_row_counts_for_fixture();
    let extra = || vec![bot(9, BotId::mint())];
    // Behind the cursor, ahead of it, and backwards.
    for (from, to) in [(3, 9), (5, 9)] {
        assert_eq!(
            repo.mirror_apply_page(&page(owner, from, to, extra()), NOW),
            Err(MirrorRefusal::OutOfOrder),
            "{from}"
        );
    }
    assert_eq!(
        repo.mirror_apply_page(&page(owner, 4, 3, extra()), NOW),
        Err(MirrorRefusal::Malformed)
    );
    // Another feed continues nothing: only a reset may replace the feed.
    let mut other_feed = page(owner, 4, 9, extra());
    other_feed.feed_id = token(7);
    assert_eq!(
        repo.mirror_apply_page(&other_feed, NOW),
        Err(MirrorRefusal::OutOfOrder)
    );
    // A page of a reset that is not its first and not its next.
    assert_eq!(
        repo.mirror_apply_page(&reset(page(owner, 4, 9, extra())), NOW),
        Err(MirrorRefusal::OutOfOrder)
    );
    let mut bad_token = page(owner, 4, 9, extra());
    bad_token.boot_id = "boot".into();
    assert_eq!(
        repo.mirror_apply_page(&bad_token, NOW),
        Err(MirrorRefusal::Malformed)
    );
    assert_eq!(
        repo.mirror_apply_page(&page(device(), 0, 9, extra()), NOW),
        Err(MirrorRefusal::NoOwner)
    );
    assert_eq!(repo.mirror_row_counts_for_fixture(), before);
    assert_eq!(repo.mirror_owner(owner).unwrap().unwrap().cursor, 4);
}

/// `m8_mirror_apply`: removing a conversation deletes its messages, and
/// removing a Bot deletes its conversations and their messages. Another
/// Bot's rows stay.
#[test]
fn m8_mirror_apply_a_removal_deletes_what_belongs_to_the_subject() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owned(&mut repo);
    let (first, second) = (Family::new(), Family::new());
    let mut changes = first.changes(1);
    changes.extend(second.changes(5));
    let extra_chat = ChatId::new();
    let extra_message = MessageId::new();
    changes.push(chat(9, extra_chat, first.bot));
    changes.push(message(10, extra_message, extra_chat, 0));
    repo.mirror_apply_page(&reset(caught_up(page(owner, 0, 10, changes), 0)), NOW)
        .unwrap();
    // One message, then a conversation with its message.
    let applied = repo
        .mirror_apply_page(
            &page(
                owner,
                10,
                12,
                vec![
                    MirrorChange::RemoveMessage {
                        message_id: first.messages[1],
                    },
                    MirrorChange::RemoveChat {
                        chat_id: extra_chat,
                    },
                ],
            ),
            NOW,
        )
        .unwrap();
    assert_eq!(
        applied.delta,
        MirrorCounts {
            bots: 0,
            chats: -1,
            messages: -2
        }
    );
    assert_eq!(repo.mirror_messages(owner, first.chat).unwrap().len(), 1);
    assert!(repo.mirror_messages(owner, extra_chat).unwrap().is_empty());
    assert_eq!(
        repo.mirror_place(MirrorSubject::Message(extra_message))
            .unwrap(),
        MirrorPlace::Nowhere
    );
    // The Bot: its conversation and the message that was left go with it.
    let applied = repo
        .mirror_apply_page(
            &page(
                owner,
                12,
                13,
                vec![MirrorChange::RemoveBot { bot_id: first.bot }],
            ),
            NOW,
        )
        .unwrap();
    assert_eq!(
        applied.delta,
        MirrorCounts {
            bots: -1,
            chats: -1,
            messages: -1
        }
    );
    assert!(repo.mirror_bot(owner, first.bot).unwrap().is_none());
    assert!(repo.mirror_chat(owner, first.chat).unwrap().is_none());
    assert!(repo.mirror_messages(owner, first.chat).unwrap().is_empty());
    assert!(repo.mirror_bot(owner, second.bot).unwrap().is_some());
    assert_eq!(repo.mirror_messages(owner, second.chat).unwrap().len(), 2);
    assert_eq!(
        repo.mirror_counts(owner).unwrap(),
        MirrorCounts {
            bots: 1,
            chats: 1,
            messages: 2
        }
    );
}

/// `m8_mirror_apply`: an owner cannot claim an id that routes elsewhere. A
/// Bot, a conversation or a message that is this computer's own, or that
/// another owner's copy holds, is refused whichever entry names it, and the
/// page with it.
#[test]
fn m8_mirror_apply_a_local_id_or_another_owners_id_is_refused() {
    let mut repo = Repository::in_memory().unwrap();
    let local = fixture_bot();
    repo.save_bot(
        &local,
        ProfileId::new(),
        BotRequestId::mint(),
        "create",
        None,
    )
    .unwrap();
    let (owner, other) = (owned(&mut repo), owned(&mut repo));
    let family = Family::new();
    repo.mirror_apply_page(
        &reset(caught_up(page(other, 0, 4, family.changes(1)), 0)),
        NOW,
    )
    .unwrap();
    let before = repo.mirror_row_counts_for_fixture();
    let canonical_before = canonical(&repo);
    let fresh = Family::new();
    let claims: Vec<(&str, MirrorChange)> = vec![
        ("a local Bot", bot(2, local.id)),
        (
            "a conversation of a local Bot",
            chat(2, ChatId::new(), local.id),
        ),
        ("a local conversation", chat(2, local.main_chat, fresh.bot)),
        (
            "a message of a local conversation",
            message(2, MessageId::new(), local.main_chat, 0),
        ),
        ("another owner's Bot", bot(2, family.bot)),
        (
            "a conversation of another owner's Bot",
            chat(2, ChatId::new(), family.bot),
        ),
        (
            "another owner's conversation",
            chat(2, family.chat, fresh.bot),
        ),
        (
            "another owner's message",
            message(2, family.messages[0], fresh.chat, 0),
        ),
        (
            "a message in another owner's conversation",
            message(2, MessageId::new(), family.chat, 0),
        ),
        (
            "a message naming another owner's Bot",
            MirrorChange::Message {
                seq: 2,
                message_id: MessageId::new(),
                chat_id: fresh.chat,
                bot_id: Some(family.bot),
                ordinal: 0,
                record_json: "{}".into(),
            },
        ),
    ];
    for (what, claim) in claims {
        // A good record first: it must not survive the refused page.
        let changes = vec![bot(1, fresh.bot), claim];
        assert_eq!(
            repo.mirror_apply_page(&reset(page(owner, 0, 2, changes)), NOW),
            Err(MirrorRefusal::Claimed),
            "{what}"
        );
        assert_eq!(repo.mirror_row_counts_for_fixture(), before, "{what}");
    }
    assert_eq!(canonical(&repo), canonical_before);
    // The same ids from the owner that holds them are its own to rewrite.
    repo.mirror_apply_page(&page(other, 4, 5, vec![bot(5, family.bot)]), NOW)
        .unwrap();
    // Whose each id is: this computer's own tables are looked at first.
    assert_eq!(
        repo.mirror_place(MirrorSubject::Bot(local.id)).unwrap(),
        MirrorPlace::Local
    );
    assert_eq!(
        repo.mirror_place(MirrorSubject::Chat(local.main_chat))
            .unwrap(),
        MirrorPlace::Local
    );
    assert_eq!(
        repo.mirror_place(MirrorSubject::Bot(family.bot)).unwrap(),
        MirrorPlace::Linked {
            owner_device: other,
            chat_id: None
        }
    );
    assert_eq!(
        repo.mirror_place(MirrorSubject::Message(family.messages[1]))
            .unwrap(),
        MirrorPlace::Linked {
            owner_device: other,
            chat_id: Some(family.chat)
        }
    );
    assert_eq!(
        repo.mirror_place(MirrorSubject::Chat(ChatId::new()))
            .unwrap(),
        MirrorPlace::Nowhere
    );
}

/// `m8_mirror_reset`: the pages of a reset are written where no read sees
/// them, and the copy that was there stays readable until the page that
/// brings the reset up to date, which swaps them at once.
#[test]
fn m8_mirror_reset_staging_is_invisible_and_the_switch_is_one_transaction() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owned(&mut repo);
    let old = Family::new();
    repo.mirror_apply_page(&reset(caught_up(page(owner, 0, 4, old.changes(1)), 0)), NOW)
        .unwrap();
    // A reset: the owner's feed was replaced. Two staged pages.
    let new = Family::new();
    let mut staged = reset(page(owner, 0, 2, new.changes(1)[..2].to_vec()));
    staged.feed_id = token(9);
    let applied = repo.mirror_apply_page(&staged, NOW + 1).unwrap();
    assert!(!applied.switched);
    assert_eq!(applied.generation, 2);
    let row = repo.mirror_owner(owner).unwrap().unwrap();
    assert_eq!(
        (row.generation, row.cursor, row.staging),
        (1, 4, Some((2, 2)))
    );
    assert_eq!(row.feed_id.as_deref(), Some(token(1).as_str()));
    // Nothing of the reset is read, and nothing of the old copy is gone.
    assert!(repo.mirror_bot(owner, new.bot).unwrap().is_none());
    assert_eq!(
        repo.mirror_place(MirrorSubject::Chat(new.chat)).unwrap(),
        MirrorPlace::Nowhere
    );
    assert!(repo.mirror_bot(owner, old.bot).unwrap().is_some());
    assert_eq!(repo.mirror_messages(owner, old.chat).unwrap().len(), 2);
    assert_eq!(
        repo.mirror_counts(owner).unwrap(),
        MirrorCounts {
            bots: 1,
            chats: 1,
            messages: 2
        }
    );
    // The page that brings it up to date: the swap.
    let mut last = reset(caught_up(
        page(owner, 2, 4, new.changes(1)[2..].to_vec()),
        1,
    ));
    last.feed_id = token(9);
    let applied = repo.mirror_apply_page(&last, NOW + 2).unwrap();
    assert!(applied.switched);
    let row = repo.mirror_owner(owner).unwrap().unwrap();
    assert_eq!((row.generation, row.cursor, row.staging), (2, 4, None));
    assert_eq!(row.feed_id.as_deref(), Some(token(9).as_str()));
    assert_eq!(row.withheld_bots, 1);
    assert!(repo.mirror_bot(owner, old.bot).unwrap().is_none());
    assert!(repo.mirror_messages(owner, old.chat).unwrap().is_empty());
    assert!(repo.mirror_bot(owner, new.bot).unwrap().is_some());
    assert_eq!(repo.mirror_messages(owner, new.chat).unwrap().len(), 2);
    // No row of the old generation is left anywhere.
    assert_eq!(repo.mirror_row_counts_for_fixture()[1..4], [1, 1, 2]);
}

/// `m8_mirror_reset`: a reset that stopped half way leaves staged rows. At
/// the next start, before the next subscription, and when a reset begins
/// again from 0, they are deleted, and the copy that was there is intact.
#[test]
fn m8_mirror_reset_an_interrupted_reset_is_dropped_and_begins_again() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owned(&mut repo);
    let old = Family::new();
    repo.mirror_apply_page(&reset(caught_up(page(owner, 0, 4, old.changes(1)), 0)), NOW)
        .unwrap();
    let visible = repo.mirror_row_counts_for_fixture();
    let stage = |repo: &mut Repository| {
        let new = Family::new();
        repo.mirror_apply_page(&reset(page(owner, 0, 2, new.changes(1)[..2].to_vec())), NOW)
            .unwrap();
        assert_ne!(repo.mirror_row_counts_for_fixture(), visible);
    };
    // Dropped before the next subscription.
    stage(&mut repo);
    repo.mirror_discard_staging(owner, NOW).unwrap();
    assert_eq!(repo.mirror_row_counts_for_fixture(), visible);
    assert_eq!(repo.mirror_owner(owner).unwrap().unwrap().staging, None);
    // Dropped at the next start, which also finds no subscription open.
    stage(&mut repo);
    repo.mirror_recover(NOW).unwrap();
    assert_eq!(repo.mirror_row_counts_for_fixture(), visible);
    let row = repo.mirror_owner(owner).unwrap().unwrap();
    assert_eq!((row.staging, row.state), (None, LinkedOwnerState::Stale));
    assert_eq!((row.generation, row.cursor), (1, 4));
    // Dropped by the first page of the reset that begins again.
    stage(&mut repo);
    let again = Family::new();
    repo.mirror_apply_page(&reset(page(owner, 0, 1, vec![bot(1, again.bot)])), NOW)
        .unwrap();
    let counts = repo.mirror_row_counts_for_fixture();
    assert_eq!(counts[1..4], [visible[1] + 1, visible[2], visible[3]]);
    // A page that continues the old copy ends the reset that was staged.
    repo.mirror_apply_page(&page(owner, 4, 5, vec![bot(5, old.bot)]), NOW)
        .unwrap();
    assert_eq!(repo.mirror_row_counts_for_fixture(), visible);
    assert_eq!(repo.mirror_messages(owner, old.chat).unwrap().len(), 2);
}

/// `m8_mirror_counts`: every page says how the row counts moved, a record
/// that was already there moves nothing, and the sum is what a count of
/// the copy reads.
#[test]
fn m8_mirror_counts_follow_every_page() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owned(&mut repo);
    let mut held = MirrorCounts::default();
    let add = |held: &mut MirrorCounts, delta: MirrorCounts| {
        held.bots += delta.bots;
        held.chats += delta.chats;
        held.messages += delta.messages;
    };
    let (first, second) = (Family::new(), Family::new());
    let applied = repo
        .mirror_apply_page(
            &reset(caught_up(page(owner, 0, 4, first.changes(1)), 0)),
            NOW,
        )
        .unwrap();
    add(&mut held, applied.delta);
    let mut changes = second.changes(5);
    // The same records again, and one removal.
    changes.extend(first.changes(9));
    changes.push(MirrorChange::RemoveMessage {
        message_id: second.messages[0],
    });
    let applied = repo
        .mirror_apply_page(&page(owner, 4, 13, changes), NOW)
        .unwrap();
    add(&mut held, applied.delta);
    assert_eq!(
        held,
        MirrorCounts {
            bots: 2,
            chats: 2,
            messages: 3
        }
    );
    assert_eq!(repo.mirror_counts(owner).unwrap(), held);
    // A removal of something that is not there moves nothing.
    let applied = repo
        .mirror_apply_page(
            &page(
                owner,
                13,
                14,
                vec![MirrorChange::RemoveBot {
                    bot_id: BotId::mint(),
                }],
            ),
            NOW,
        )
        .unwrap();
    assert_eq!(applied.delta, MirrorCounts::default());
}

/// `m8_mirror_never_executes`: applying pages of every kind, a reset, and
/// deleting a copy write no row of any table but the six client tables.
/// In particular nothing is submitted, no attempt starts, no receipt of a
/// Bot command is written, and the feed this computer serves does not move.
#[test]
fn m8_mirror_never_executes_no_page_writes_a_canonical_table() {
    let mut repo = Repository::in_memory().unwrap();
    let local = fixture_bot();
    repo.save_bot(
        &local,
        ProfileId::new(),
        BotRequestId::mint(),
        "create",
        None,
    )
    .unwrap();
    let before = canonical(&repo);
    let head = repo.feed_head().unwrap();
    let owner = owned(&mut repo);
    let family = Family::new();
    let mut changes = family.changes(1);
    changes.push(MirrorChange::Team {
        seq: 5,
        record_json: r#"{"format":1}"#.into(),
    });
    repo.mirror_apply_page(&reset(caught_up(page(owner, 0, 5, changes), 0)), NOW)
        .unwrap();
    repo.mirror_apply_page(
        &page(
            owner,
            5,
            8,
            vec![
                message(6, MessageId::new(), family.chat, 2),
                MirrorChange::RemoveChat {
                    chat_id: family.chat,
                },
                MirrorChange::RemoveTeam,
            ],
        ),
        NOW,
    )
    .unwrap();
    repo.mirror_apply_page(&reset(page(owner, 0, 4, Family::new().changes(1))), NOW)
        .unwrap();
    repo.mirror_recover(NOW).unwrap();
    repo.mirror_set_state(owner, LinkedOwnerState::Unsupported, NOW)
        .unwrap();
    repo.mirror_withdraw(&[], NOW).unwrap();
    while repo.mirror_delete_batch(owner, 2).unwrap() == MirrorDeleted::More {}
    assert_eq!(canonical(&repo), before);
    assert_eq!(repo.feed_head().unwrap(), head);
    assert!(repo.feed_audit_refusals().is_empty());
    assert_eq!(repo.mirror_row_counts_for_fixture(), [0; 6]);
}

fn fingerprint(index: u8) -> String {
    format!("{index:02x}").repeat(32)
}

/// `m8_client_commands`: a command is recorded as `sent` with the bytes
/// that go out; its receipt settles it and drops the bytes; the same
/// request again is found, and with other content it is a conflict.
#[test]
fn m8_client_commands_a_command_is_recorded_then_settled() {
    let mut repo = Repository::in_memory().unwrap();
    let (owner, request) = (device(), Uuid::new_v4());
    let bytes = r#"{"type":"command"}"#;
    assert_eq!(
        repo.mirror_command_begin(
            owner,
            request,
            &fingerprint(1),
            "pause_bot",
            None,
            bytes,
            NOW
        )
        .unwrap(),
        MirrorCommandBegin::Send
    );
    let row = repo.mirror_command(owner, request).unwrap().unwrap();
    assert_eq!(row.state, MirrorCommandState::Sent);
    assert_eq!(row.command_json.as_deref(), Some(bytes));
    assert_eq!((row.outcome, row.failure_code.clone()), (None, None));
    // The same request again: found. Other content: a conflict.
    assert_eq!(
        repo.mirror_command_begin(
            owner,
            request,
            &fingerprint(1),
            "pause_bot",
            None,
            bytes,
            NOW
        )
        .unwrap(),
        MirrorCommandBegin::Known(row)
    );
    assert_eq!(
        repo.mirror_command_begin(
            owner,
            request,
            &fingerprint(2),
            "pause_bot",
            None,
            bytes,
            NOW
        )
        .unwrap(),
        MirrorCommandBegin::Conflict
    );
    repo.mirror_command_receipt(
        owner,
        request,
        ClientOutcome::Accepted,
        None,
        Some(7),
        NOW + 1,
    )
    .unwrap();
    let row = repo.mirror_command(owner, request).unwrap().unwrap();
    assert_eq!(row.state, MirrorCommandState::Receipted);
    assert_eq!(row.outcome, Some(ClientOutcome::Accepted));
    assert_eq!((row.feed_cursor, row.command_json), (Some(7), None));
    // The first answer stands.
    repo.mirror_command_receipt(
        owner,
        request,
        ClientOutcome::Refused,
        Some("busy"),
        None,
        NOW,
    )
    .unwrap();
    assert_eq!(
        repo.mirror_command(owner, request)
            .unwrap()
            .unwrap()
            .outcome,
        Some(ClientOutcome::Accepted)
    );
    // Text is never stored as a failure code.
    let other = Uuid::new_v4();
    repo.mirror_command_begin(owner, other, &fingerprint(3), "send_turn", None, bytes, NOW)
        .unwrap();
    repo.mirror_command_receipt(
        owner,
        other,
        ClientOutcome::Refused,
        Some("what somebody typed"),
        None,
        NOW,
    )
    .unwrap();
    let row = repo.mirror_command(owner, other).unwrap().unwrap();
    assert_eq!(
        (row.outcome, row.failure_code),
        (Some(ClientOutcome::Refused), None)
    );
}

/// `m8_client_commands`: a command whose stream ended is `unconfirmed` and
/// is offered again with the same bytes; one that a stopped worker left
/// `sent` becomes `unconfirmed` at the next start; an answer of `unknown`
/// is final and is never offered again; after a day it expires.
#[test]
fn m8_client_commands_unconfirmed_is_resent_unknown_is_final_and_a_day_expires() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = device();
    let bytes = |index: u8| format!(r#"{{"type":"command","n":{index}}}"#);
    let begin = |repo: &mut Repository, index: u8, at: i64| {
        let request = Uuid::new_v4();
        repo.mirror_command_begin(
            owner,
            request,
            &fingerprint(index),
            "send_turn",
            Some(BotId::mint()),
            &bytes(index),
            at,
        )
        .unwrap();
        request
    };
    let closed = begin(&mut repo, 1, NOW);
    repo.mirror_command_unconfirmed(owner, closed, NOW).unwrap();
    // A worker stopped with this one written and unanswered.
    let stranded = begin(&mut repo, 2, NOW + 1);
    assert_eq!(repo.mirror_commands_unconfirmed(owner).unwrap().len(), 1);
    repo.mirror_recover(NOW + 2).unwrap();
    let open = repo.mirror_commands_unconfirmed(owner).unwrap();
    assert_eq!(
        open.iter()
            .map(|command| (command.request_id, command.command_json.clone().unwrap()))
            .collect::<Vec<_>>(),
        vec![(closed, bytes(1)), (stranded, bytes(2))]
    );
    // `unknown` is an answer: the row is settled and never offered again.
    repo.mirror_command_receipt(owner, closed, ClientOutcome::Unknown, None, None, NOW + 3)
        .unwrap();
    let open = repo.mirror_commands_unconfirmed(owner).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].request_id, stranded);
    let row = repo.mirror_command(owner, closed).unwrap().unwrap();
    assert_eq!(
        (row.state, row.outcome),
        (MirrorCommandState::Receipted, Some(ClientOutcome::Unknown))
    );
    // Not yet a day: nothing expires. A day after it was first sent: it does.
    assert_eq!(
        repo.mirror_commands_sweep(NOW + MIRROR_COMMAND_UNCONFIRMED_MS)
            .unwrap(),
        0
    );
    assert_eq!(
        repo.mirror_commands_sweep(NOW + 1 + MIRROR_COMMAND_UNCONFIRMED_MS)
            .unwrap(),
        1
    );
    let row = repo.mirror_command(owner, stranded).unwrap().unwrap();
    assert_eq!(
        (row.state, row.command_json),
        (MirrorCommandState::Expired, None)
    );
    assert!(repo.mirror_commands_unconfirmed(owner).unwrap().is_empty());
    // A late receipt does not revive an expired command.
    repo.mirror_command_receipt(owner, stranded, ClientOutcome::Completed, None, None, NOW)
        .unwrap();
    assert_eq!(
        repo.mirror_command(owner, stranded).unwrap().unwrap().state,
        MirrorCommandState::Expired
    );
    // Settled and expired rows are deleted after a week, 200 at a time.
    for index in 0..205u32 {
        let request = Uuid::new_v4();
        repo.mirror_command_begin(
            owner,
            request,
            &fingerprint(9),
            "pause_bot",
            None,
            "{}",
            NOW + i64::from(index),
        )
        .unwrap();
        repo.mirror_command_receipt(
            owner,
            request,
            ClientOutcome::Completed,
            None,
            None,
            NOW + i64::from(index),
        )
        .unwrap();
    }
    assert_eq!(repo.mirror_row_counts_for_fixture()[5], 207);
    let later = NOW + 2 * MIRROR_COMMAND_KEEP_MS;
    repo.mirror_commands_sweep(later).unwrap();
    assert_eq!(repo.mirror_row_counts_for_fixture()[5], 7);
    repo.mirror_commands_sweep(later).unwrap();
    assert_eq!(repo.mirror_row_counts_for_fixture()[5], 0);
}

/// `m8_removal_on_revocation`: an owner the admission port no longer names
/// is hidden from every read in the transaction that marks it, its
/// commands that were not confirmed expire, and its rows are deleted in
/// bounded batches, the owner row last. A delete that stopped half way is
/// found again at the next start and finishes. Another owner is untouched.
#[test]
fn m8_removal_on_revocation_hides_at_once_deletes_in_batches_and_survives_a_stop() {
    let mut repo = Repository::in_memory().unwrap();
    let (gone, kept) = (owned(&mut repo), owned(&mut repo));
    let (family, other) = (Family::new(), Family::new());
    let mut changes = family.changes(1);
    changes.push(MirrorChange::Team {
        seq: 5,
        record_json: r#"{"format":1}"#.into(),
    });
    repo.mirror_apply_page(&reset(caught_up(page(gone, 0, 5, changes), 0)), NOW)
        .unwrap();
    repo.mirror_apply_page(
        &reset(caught_up(page(kept, 0, 4, other.changes(1)), 0)),
        NOW,
    )
    .unwrap();
    let unconfirmed = Uuid::new_v4();
    let settled = Uuid::new_v4();
    for request in [unconfirmed, settled] {
        repo.mirror_command_begin(gone, request, &fingerprint(1), "send_turn", None, "{}", NOW)
            .unwrap();
    }
    repo.mirror_command_unconfirmed(gone, unconfirmed, NOW)
        .unwrap();
    repo.mirror_command_receipt(gone, settled, ClientOutcome::Completed, None, None, NOW)
        .unwrap();
    // The port still names both: nothing is withdrawn.
    assert!(repo.mirror_withdraw(&[gone, kept], NOW).unwrap().is_empty());
    // It names only one.
    assert_eq!(repo.mirror_withdraw(&[kept], NOW + 1).unwrap(), vec![gone]);
    // Hidden at once, with every row still on the disk.
    assert_eq!(repo.mirror_row_counts_for_fixture()[..5], [2, 2, 2, 4, 1]);
    assert_eq!(
        repo.mirror_owners()
            .unwrap()
            .iter()
            .map(|owner| owner.owner_device)
            .collect::<Vec<_>>(),
        vec![kept]
    );
    assert!(repo.mirror_bots(gone, None).unwrap().is_empty());
    assert!(repo.mirror_bot(gone, family.bot).unwrap().is_none());
    assert!(
        repo.mirror_chats(gone, family.bot, false)
            .unwrap()
            .is_empty()
    );
    assert!(repo.mirror_chat(gone, family.chat).unwrap().is_none());
    assert!(repo.mirror_messages(gone, family.chat).unwrap().is_empty());
    assert!(
        repo.mirror_message(gone, family.messages[0])
            .unwrap()
            .is_none()
    );
    assert!(repo.mirror_team(gone).unwrap().is_none());
    assert_eq!(repo.mirror_counts(gone).unwrap().messages, 2);
    for subject in [
        MirrorSubject::Bot(family.bot),
        MirrorSubject::Chat(family.chat),
        MirrorSubject::Message(family.messages[0]),
    ] {
        assert_eq!(repo.mirror_place(subject).unwrap(), MirrorPlace::Nowhere);
    }
    // No page is applied to it any more, and nothing makes it current.
    assert_eq!(
        repo.mirror_apply_page(&page(gone, 5, 6, vec![bot(6, BotId::mint())]), NOW),
        Err(MirrorRefusal::NoOwner)
    );
    repo.mirror_set_state(gone, LinkedOwnerState::Current, NOW)
        .unwrap();
    assert_eq!(
        repo.mirror_owner(gone).unwrap().unwrap().state,
        LinkedOwnerState::Withdrawing
    );
    // The command that was not confirmed is expired; the record of both stays.
    assert_eq!(
        repo.mirror_command(gone, unconfirmed)
            .unwrap()
            .unwrap()
            .state,
        MirrorCommandState::Expired
    );
    assert_eq!(
        repo.mirror_command(gone, settled).unwrap().unwrap().state,
        MirrorCommandState::Receipted
    );
    // A batch of three rows leaves rows behind.
    assert_eq!(
        repo.mirror_delete_batch(gone, 3).unwrap(),
        MirrorDeleted::More
    );
    assert_eq!(repo.mirror_row_counts_for_fixture()[..5], [2, 2, 1, 2, 1]);
    // The worker stops here. The next start finds the copy still to delete.
    repo.mirror_recover(NOW + 2).unwrap();
    assert_eq!(repo.mirror_withdrawing().unwrap(), vec![gone]);
    assert_eq!(
        repo.mirror_owner(gone).unwrap().unwrap().state,
        LinkedOwnerState::Withdrawing
    );
    let mut batches = 0;
    while repo.mirror_delete_batch(gone, 1).unwrap() == MirrorDeleted::More {
        batches += 1;
        // The owner row is there for as long as any row of the copy is.
        assert!(repo.mirror_owner(gone).unwrap().is_some());
    }
    assert_eq!(batches, 2);
    assert!(repo.mirror_owner(gone).unwrap().is_none());
    assert!(repo.mirror_withdrawing().unwrap().is_empty());
    assert_eq!(repo.mirror_row_counts_for_fixture()[..5], [1, 1, 1, 2, 0]);
    // The other owner's copy is whole, and so is the record of what was sent.
    assert_eq!(repo.mirror_messages(kept, other.chat).unwrap().len(), 2);
    assert_eq!(repo.mirror_row_counts_for_fixture()[5], 2);
    // A copy that is not being withdrawn is never deleted by a batch.
    assert_eq!(
        repo.mirror_delete_batch(kept, 100).unwrap(),
        MirrorDeleted::Done
    );
    assert_eq!(repo.mirror_messages(kept, other.chat).unwrap().len(), 2);
    // The same owner linked again starts from nothing.
    let again = repo.mirror_ensure_owner(gone, NOW + 3).unwrap();
    assert_eq!(
        (again.state, again.cursor, again.generation),
        (LinkedOwnerState::Never, 0, 0)
    );
}

/// `m8_routing`: the newest messages of the conversations that are running
/// or need the person are found again, and only those. They are where the
/// replies and pending calls a command may name are read from.
#[test]
fn m8_routing_active_messages_are_the_newest_of_running_conversations() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owned(&mut repo);
    let (idle, busy) = (Family::new(), Family::new());
    let mut changes = idle.changes(1);
    changes.push(bot(5, busy.bot));
    changes.push(MirrorChange::Chat {
        seq: 6,
        chat_id: busy.chat,
        bot_id: busy.bot,
        purpose: "main".into(),
        trashed: false,
        updated_at_ms: NOW,
        record_json: r#"{"format":1,"activity":{"running":true,"needs_you":false}}"#.into(),
    });
    for ordinal in 0..6u32 {
        changes.push(message(
            7 + u64::from(ordinal),
            MessageId::new(),
            busy.chat,
            ordinal,
        ));
    }
    repo.mirror_apply_page(&reset(caught_up(page(owner, 0, 12, changes), 0)), NOW)
        .unwrap();
    let active = repo.mirror_active_messages(4).unwrap();
    assert_eq!(active.len(), 4);
    assert!(active.iter().all(|(device, _)| *device == owner));
    let seqs: Vec<bool> = (9..=12)
        .map(|seq| {
            active
                .iter()
                .any(|(_, record)| record.contains(&format!(r#""seq":{seq}}}"#)))
        })
        .collect();
    assert_eq!(seqs, vec![true; 4]);
}
