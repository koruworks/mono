// SPDX-License-Identifier: MPL-2.0
//! `m8_owner_feed`: the feed role's stream. What a page holds is the feed's
//! own cohorts (`m8_feed_pages`, `m8_feed_starts`, `m8_feed_withheld`); this
//! is the stream around it: the subscribe, the acknowledgement window and
//! its timeout, the keepalive, a change reaching a device that is up to
//! date, and a restart of the owner's worker.
use koru_bot_client_wire::{GapReason, Outcome, PageExpectation, refusal};

use super::*;

/// A first subscribe is answered with one gap and the whole feed, reset,
/// each page passing the client's own page checks; a later change arrives
/// by itself; a fresh subscribe on the same stream is the client asking for
/// a reset.
#[test]
fn m8_owner_feed_a_subscribe_is_answered_and_a_change_follows_by_itself() {
    let world = World::new();
    let device = world.device();
    let (mut feed, hello) = world.open(device, Role::Feed);
    let pages = world.runtime.block_on(async {
        feed.subscribe(None, None, None).await;
        match feed.next().await {
            Some(WireMessage::Gap(gap)) => assert_eq!(gap.reason, GapReason::FirstSubscribe),
            other => panic!("expected a gap, read {other:?}"),
        }
        feed.pages().await
    });
    let mut cursor = 0;
    for page in &pages {
        page.check_page(&PageExpectation {
            owner_device: &world.owner.device,
            feed_id: &hello.feed_id,
            boot_id: &hello.boot_id,
            cursor,
        })
        .unwrap();
        page.check_limits(&hello.limits).unwrap();
        assert!(page.reset);
        cursor = page.to_cursor;
    }
    assert_eq!(pages.last().unwrap().head, cursor);

    // Nothing changes: nothing is sent.
    assert!(
        world
            .runtime
            .block_on(feed.silent(Duration::from_millis(300)))
    );

    // A window of the computer that runs the Bot renames it.
    let mut config = world.bot.config.clone();
    config.name = "Renamed at home".into();
    world
        .client()
        .bots(
            local_owner(),
            BotCommand::Update {
                request_id: BotRequestId::mint(),
                bot_id: world.bot.id,
                expected_revision: world.bot.revision,
                config,
            },
        )
        .unwrap();
    let next = world.runtime.block_on(feed.pages());
    assert_eq!(next.len(), 1);
    next[0]
        .check_page(&PageExpectation {
            owner_device: &world.owner.device,
            feed_id: &hello.feed_id,
            boot_id: &hello.boot_id,
            cursor,
        })
        .unwrap();
    assert!(!next[0].reset && next[0].caught_up);
    assert_eq!(next[0].entries.len(), 1);
    assert!(
        serde_json::to_string(&next[0].entries[0])
            .unwrap()
            .contains("Renamed at home")
    );

    // The client asks for a reset.
    let again = world.runtime.block_on(async {
        feed.subscribe(None, None, None).await;
        match feed.next().await {
            Some(WireMessage::Gap(gap)) => assert_eq!(gap.reason, GapReason::ClientRequested),
            other => panic!("expected a gap, read {other:?}"),
        }
        feed.pages().await
    });
    assert!(again.iter().all(|page| page.reset));
    assert_eq!(again[0].from_cursor, 0);
}

/// After the owner's worker restarts, a client that subscribes with the
/// feed, the start and the cursor it holds continues from its cursor: no
/// gap, no reset, and only what changed since. A cursor from a start this
/// database does not know is reset.
#[test]
fn m8_owner_feed_a_restart_of_the_owners_worker_resets_nobody() {
    let mut world = World::new();
    let device = world.device();
    let (mut feed, before) = world.open(device, Role::Feed);
    let pages = world.runtime.block_on(async {
        feed.subscribe(None, None, None).await;
        assert!(matches!(feed.next().await, Some(WireMessage::Gap(_))));
        feed.pages().await
    });
    let cursor = pages.last().unwrap().to_cursor;

    world.restart();
    // The stream of the worker that stopped ends.
    assert!(world.runtime.block_on(async {
        loop {
            match feed.next().await {
                None | Some(WireMessage::Refused(_)) => return true,
                Some(WireMessage::Keepalive(_)) => {}
                Some(_) => return false,
            }
        }
    }));
    let (mut feed, after) = world.open(device, Role::Feed);
    assert_eq!(after.feed_id, before.feed_id);
    assert_ne!(after.boot_id, before.boot_id);
    let pages = world.runtime.block_on(async {
        feed.subscribe(Some(&before.feed_id), Some(&before.boot_id), Some(cursor))
            .await;
        feed.pages().await
    });
    assert!(pages.iter().all(|page| !page.reset));
    assert_eq!(pages[0].from_cursor, cursor);
    assert_eq!(pages[0].boot_id, after.boot_id);
    assert!(pages.iter().all(|page| page.entries.is_empty()));

    // A start the database never recorded: a gap, and everything again.
    let (mut lost, _) = world.open(device, Role::Feed);
    world.runtime.block_on(async {
        lost.subscribe(
            Some(&before.feed_id),
            Some("00000000000000000000000000000abc"),
            Some(cursor),
        )
        .await;
        match lost.next().await {
            Some(WireMessage::Gap(gap)) => assert_eq!(gap.reason, GapReason::FeedRewound),
            other => panic!("expected a gap, read {other:?}"),
        }
        assert!(lost.pages().await.iter().all(|page| page.reset));
    });
}

/// A client that leaves a page unacknowledged is disconnected when the
/// acknowledgement time is over, with no frame; an acknowledgement of a
/// page that was never sent is a protocol error; and an idle feed is kept
/// alive.
#[test]
fn m8_owner_feed_an_unacknowledged_page_disconnects_and_an_idle_feed_is_kept_alive() {
    let world = World::with(
        Timing {
            ack: Duration::from_millis(1_500),
            keepalive: Duration::from_millis(150),
            ..Timing::default()
        },
        Native::default(),
    );
    let device = world.device();

    // Never acknowledges.
    let (mut silent, _) = world.open(device, Role::Feed);
    world.runtime.block_on(async {
        silent.subscribe(None, None, None).await;
        assert!(matches!(silent.next().await, Some(WireMessage::Gap(_))));
        assert!(matches!(silent.next().await, Some(WireMessage::Changes(_))));
        // Kept alive while the page is outstanding, then disconnected.
        loop {
            match silent.next().await {
                Some(WireMessage::Keepalive(_)) => {}
                None => break,
                other => panic!("expected the stream to end, read {other:?}"),
            }
        }
    });

    // Acknowledges, and is then only kept alive.
    let (mut idle, _) = world.open(device, Role::Feed);
    world.runtime.block_on(async {
        idle.subscribe(None, None, None).await;
        assert!(matches!(idle.next().await, Some(WireMessage::Gap(_))));
        idle.pages().await;
        for _ in 0..4 {
            assert!(matches!(idle.next().await, Some(WireMessage::Keepalive(_))));
        }
    });

    // Acknowledges what was never sent.
    let (mut ahead, _) = world.open(device, Role::Feed);
    world.runtime.block_on(async {
        ahead.subscribe(None, None, None).await;
        assert!(matches!(ahead.next().await, Some(WireMessage::Gap(_))));
        let page = match ahead.next().await {
            Some(WireMessage::Changes(page)) => page,
            other => panic!("expected a page, read {other:?}"),
        };
        ahead
            .write(WireMessage::Ack(Ack {
                cursor: page.to_cursor + 1_000,
            }))
            .await;
        assert_eq!(ahead.refusal().await, refusal::PROTOCOL_ERROR);
    });
    // A page asked for before any subscribe: an acknowledgement with no
    // subscription.
    let (mut early, _) = world.open(device, Role::Feed);
    world.runtime.block_on(async {
        early.write(WireMessage::Ack(Ack { cursor: 0 })).await;
        assert_eq!(early.refusal().await, refusal::PROTOCOL_ERROR);
    });
}

/// A device that is up to date learns that a Bot's workspace is now kept
/// on this computer without doing anything: the reconcile on the timer
/// rewrites the Bot's markers and the feed sends them as removed, with the
/// withheld count. And back.
#[test]
fn m8_owner_feed_a_workspace_kept_on_this_computer_reaches_a_device_that_is_up_to_date() {
    let world = World::with(
        Timing {
            reconcile: Duration::from_millis(200),
            ..Timing::default()
        },
        Native::default(),
    );
    let device = world.device();
    let (mut feed, _) = world.open(device, Role::Feed);
    let first = world.runtime.block_on(async {
        feed.subscribe(None, None, None).await;
        assert!(matches!(feed.next().await, Some(WireMessage::Gap(_))));
        feed.pages().await
    });
    assert_eq!(first.last().unwrap().withheld_bots, Some(0));

    world
        .authority
        .keep_on_this_computer(world.bot.home_workspace, true);
    let removed = world.runtime.block_on(feed.pages());
    let entries: Vec<_> = removed.iter().flat_map(|page| &page.entries).collect();
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|entry| entry.removed && entry.record.is_none())
    );
    let last = removed.last().unwrap();
    assert_eq!(last.withheld_bots, Some(1));
    assert_eq!(last.live.map(|live| (live.bots, live.chats)), Some((0, 0)));

    world
        .authority
        .keep_on_this_computer(world.bot.home_workspace, false);
    let back = world.runtime.block_on(feed.pages());
    let entries: Vec<_> = back.iter().flat_map(|page| &page.entries).collect();
    assert!(entries.iter().all(|entry| !entry.removed));
    assert_eq!(back.last().unwrap().withheld_bots, Some(0));

    // A command of a linked device still names a cursor the feed reaches.
    let (mut commands, _) = world.open(device, Role::Command);
    let request = Uuid::new_v4();
    let receipt = world.command(
        &mut commands,
        request,
        pause(&world.bot, request, world.bot.revision),
    );
    assert!(matches!(
        receipt.outcome,
        Outcome::Accepted | Outcome::Completed
    ));
    let cursor = receipt.feed_cursor.unwrap();
    let next = world.runtime.block_on(feed.pages());
    assert!(next.last().unwrap().to_cursor >= cursor);
}

/// The writer's budget, on the writer's own state with the clock passed in:
/// a waiting page is built at once when no command is queued; with one
/// queued, only a quarter of a second after the last page; and during a
/// reset, at most one page per fifty milliseconds. A page whose stream went
/// away is never built.
#[test]
fn m8_owner_feed_the_writer_builds_a_waiting_page_only_within_its_budget() {
    use super::super::super::feed::{FeedBudget, HomeCopies, OwnerFeed, PageOutcome};
    use super::super::writer::{PageRequest, Writer};

    let (mut repository, inner, _) = fixture();
    let authority = Home {
        inner,
        local_only: Mutex::default(),
    };
    let feed = OwnerFeed::start(&mut repository, &authority, OwnerFeed::new_boot_id()).unwrap();
    let mut writer = Writer::new(None);
    writer.install(feed);
    let ask = |writer: &mut Writer, reset: bool| {
        let (reply, answer) = tokio::sync::oneshot::channel();
        writer.queue_page(PageRequest {
            cursor: 0,
            reset,
            copies: HomeCopies::default(),
            reply,
        });
        answer
    };
    let built = |answer: &mut tokio::sync::oneshot::Receiver<_>| {
        matches!(answer.try_recv(), Ok(Ok(PageOutcome::Page(_))))
    };
    let start = std::time::Instant::now();
    let at = |millis: u64| start + Duration::from_millis(millis);

    // Nothing queued and no page built before: built at once.
    let mut first = ask(&mut writer, false);
    assert_eq!(writer.build_page(&repository, 0, at(0)), None);
    assert!(built(&mut first));

    // A command is queued: the next page waits a quarter of a second
    // after the last one, and is not built before.
    let mut second = ask(&mut writer, false);
    assert_eq!(
        writer.build_page(&repository, 1, at(10)),
        Some(FeedBudget::STARVED_AFTER - Duration::from_millis(10))
    );
    assert!(!built(&mut second));
    assert_eq!(
        writer.build_page(&repository, 1, at(249)),
        Some(Duration::from_millis(1))
    );
    assert!(!built(&mut second));
    assert_eq!(writer.build_page(&repository, 1, at(250)), None);
    assert!(built(&mut second));

    // A reset: one page per fifty milliseconds, even with nothing queued.
    let mut third = ask(&mut writer, true);
    let mut fourth = ask(&mut writer, true);
    assert_eq!(
        writer.build_page(&repository, 0, at(260)),
        Some(FeedBudget::RESET_SPACING - Duration::from_millis(10))
    );
    assert!(!built(&mut third));
    assert_eq!(
        writer.build_page(&repository, 0, at(300)),
        Some(FeedBudget::RESET_SPACING)
    );
    assert!(built(&mut third) && !built(&mut fourth));
    assert_eq!(writer.build_page(&repository, 0, at(350)), None);
    assert!(built(&mut fourth));

    // A stream that went away while its page waited gets none built, and
    // the budget is not spent on it.
    drop(ask(&mut writer, false));
    let mut kept = ask(&mut writer, false);
    assert_eq!(writer.build_page(&repository, 0, at(360)), None);
    assert!(built(&mut kept));
}
