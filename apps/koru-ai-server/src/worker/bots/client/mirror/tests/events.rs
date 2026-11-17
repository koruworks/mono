// SPDX-License-Identifier: MPL-2.0
//! Cohort `m8_events_content_free`: the two rows a linked desktop publishes
//! (`bots.mirror.state`, `bots.mirror.removed`). Against a real owner for
//! the life of a copy, and with pages applied by hand on the desktop's
//! writer for what a live stream cannot be made to do twice: the same page
//! sent again.
use koru_ai_sqlite::{MirrorDeleted, MirrorPage, MirrorRefusal};

use super::*;
use crate::worker::bots::events::{Copies, CopyChange};

const STATE: &str = "bots.mirror.state";
const REMOVED: &str = "bots.mirror.removed";

/// What must never be in a row: the owner's Bot, its folder, what was
/// typed, and the labels of the callers.
fn content(world: &World) -> Vec<String> {
    vec![
        world.bot.config.name.clone(),
        world.bot.config.working_folder.clone(),
        "CURRENT-TASK".into(),
        "linked-window".into(),
        "owner-window".into(),
        "linked-device".into(),
    ]
}

/// Every row the desktop published, checked: only the two names, each with
/// its listed attributes and nothing else, of `owner`, and no content.
fn rows(desk: &Desk, world: &World, owner: DeviceId) -> Vec<RecordedBotEvent> {
    let events = desk.events.events();
    let content = content(world);
    let content: Vec<&str> = content.iter().map(String::as_str).collect();
    assert_holds_none_of(&events, &content);
    for event in &events {
        assert_eq!(event.actor, "ai-worker");
        assert_eq!(event.attr("owner_device"), Some(owner.to_string().as_str()));
        match event.name.as_str() {
            STATE => assert_eq!(event.keys(), ["cursor", "owner_device", "reason", "state"]),
            REMOVED => {
                assert_eq!(event.keys(), ["owner_device", "reason"]);
                assert_eq!(event.attr("reason"), Some("unlinked"));
            }
            other => panic!("a linked desktop published {other}"),
        }
    }
    events
}

/// The reason of every `bots.mirror.state` row, in order.
fn reasons(events: &[RecordedBotEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|event| event.name == STATE)
        .map(|event| event.attr("reason").unwrap().to_owned())
        .collect()
}

/// `(state, reason, cursor)` of every `bots.mirror.state` row, in order.
fn states(events: &[RecordedBotEvent]) -> Vec<(String, String, u64)> {
    events
        .iter()
        .filter(|event| event.name == STATE)
        .map(|event| {
            (
                event.attr("state").unwrap().to_owned(),
                event.attr("reason").unwrap().to_owned(),
                event.attr("cursor").unwrap().parse().unwrap(),
            )
        })
        .collect()
}

fn removed(events: &[RecordedBotEvent]) -> usize {
    events.iter().filter(|event| event.name == REMOVED).count()
}

/// Wait for `count` rows, check that no more follow, and answer them.
fn published(desk: &Desk, world: &World, owner: DeviceId, count: usize) -> Vec<RecordedBotEvent> {
    until("the rows were published", || desk.events.count() >= count);
    std::thread::sleep(Duration::from_millis(300));
    let events = rows(desk, world, owner);
    assert_eq!(events.len(), count, "{events:?}");
    events
}

/// The life of one copy against a real owner: linked, first received (a
/// reset from nothing), brought up to date, lost, found again, unlinked and
/// deleted. Each is one row; the pages in between, which change no state,
/// are none.
#[test]
fn m8_events_content_free_a_copy_publishes_each_change_once_and_its_removal_once() {
    let world = World::new();
    let runtime = runtime();
    // Linked, the first copy received, and a reply's pages after it.
    let desk = copied(&runtime, &world);
    let owner = world.owner.device;
    let events = published(&desk, &world, owner, 4);
    assert_eq!(
        reasons(&events),
        ["linked", "reset_started", "reset_finished", "caught_up"]
    );
    let first = states(&events);
    assert_eq!(first[0], ("never".to_owned(), "linked".to_owned(), 0));
    assert_eq!(first[3].0, "current");
    assert!(first[3].2 > 0);

    // More pages: the owner's Bot changes twice. The copy stays current.
    owner_toggles(&world);
    owner_toggles(&world);
    until("the copy followed", || {
        desk.read(world.bot.id)
            .is_ok_and(|bot| bot.revision == world.read_bot().revision)
    });
    published(&desk, &world, owner, 4);

    // The stream is lost, and the owner stays away while it is dialed.
    desk.hooks.offline();
    desk.hooks.sever();
    until("the copy is stale", || {
        desk.state(owner) == Some(LinkedOwnerState::Stale)
    });
    let dials = desk.hooks.opened();
    until("the owner was dialed again", || {
        desk.hooks.opened() >= dials + 2
    });
    let events = published(&desk, &world, owner, 5);
    assert_eq!(reasons(&events)[4], "stream_lost");
    assert_eq!(states(&events)[4].0, "stale");

    // Found again.
    desk.hooks.online();
    desk.current(&world);
    until("the copy said so", || desk.events.count() >= 6);
    std::thread::sleep(Duration::from_millis(300));
    let events = rows(&desk, &world, owner);
    let after = states(&events)[5..].to_vec();
    assert_eq!(
        after.last().map(|row| (row.0.as_str(), row.1.as_str())),
        Some(("current", "caught_up")),
        "{after:?}"
    );
    // The owner continued from the copy's cursor, or sent it whole again:
    // either way nothing but the ends of a reset comes before `caught_up`.
    assert!(
        after[..after.len() - 1]
            .iter()
            .all(|row| ["reset_started", "reset_finished"].contains(&row.1.as_str())),
        "{after:?}"
    );
    assert_eq!(removed(&events), 0);
    let before = events.len();

    // Unlinked: hidden at once, deleted in batches, removed once.
    desk.peers.tell(&[]);
    until("the copy was deleted", || desk.rows()[..5] == [0; 5]);
    let events = published(&desk, &world, owner, before + 2);
    assert_eq!(
        (
            events[before].name.as_str(),
            events[before].attr("state"),
            events[before].attr("reason")
        ),
        (STATE, Some("withdrawing"), Some("unlinked"))
    );
    assert_eq!(events[before + 1].name, REMOVED);
    assert_eq!(removed(&events), 1);

    // The owner's own rows hold no content either, and none is a copy's.
    let served = world.events.events();
    let content = content(&world);
    let content: Vec<&str> = content.iter().map(String::as_str).collect();
    assert_holds_none_of(&served, &content);
    assert!(
        served
            .iter()
            .all(|event| event.name.starts_with("bots.client."))
    );
    assert!(served.iter().any(|event| event.name == "bots.client.reset"
        && event.attr("reason") == Some("first_subscribe")
        && event.attr("peer_device") == Some(desk.device.to_string().as_str())));
}

const FEED: &str = "0123456789abcdef0123456789abcdef";
const START: &str = "fedcba9876543210fedcba9876543210";

fn page(owner: DeviceId, from: u64, to: u64, reset: bool, caught_up: bool) -> MirrorPage {
    MirrorPage {
        owner_device: owner,
        feed_id: FEED.into(),
        boot_id: START.into(),
        protocol_version: 1,
        replica_format: 1,
        from_cursor: from,
        to_cursor: to,
        reset,
        caught_up: caught_up.then_some(0),
        changes: Vec::new(),
    }
}

/// Apply one page on the desktop's writer as the follower applies it.
fn apply(desk: &Desk, page: MirrorPage) -> Result<(), MirrorRefusal> {
    let owner = page.owner_device;
    let began_again = page.reset && page.from_cursor == 0;
    desk.on_writer(move |state| {
        state
            .mirror_change(
                Copies::One(owner),
                CopyChange::CaughtUp,
                began_again,
                |state| state.repository.mirror_apply_page(&page, 5),
            )
            .map(|_| ())
    })
}

/// Every change of the copy tables, made by hand on a desktop that follows
/// nobody, each made twice. The first publishes its row; the second, which
/// finds nothing to change or is refused, publishes nothing. That includes
/// every page: a page that was sent again is refused and says nothing.
#[test]
fn m8_events_content_free_a_page_sent_again_and_a_change_made_twice_publish_nothing() {
    let world = World::new();
    let runtime = runtime();
    // Its port is told nothing: nobody is dialed, and nothing but this test
    // writes its copy tables.
    let desk = Desk::plain(&runtime, &world);
    let owner = world.owner.device;
    let expect = |desk: &Desk, expected: &[(&str, &str, u64)]| {
        let events = rows(desk, &world, owner);
        let expected: Vec<(String, String, u64)> = expected
            .iter()
            .map(|(state, reason, cursor)| ((*state).to_owned(), (*reason).to_owned(), *cursor))
            .collect();
        assert_eq!(states(&events), expected);
        events
    };
    let mut seen: Vec<(&str, &str, u64)> = Vec::new();

    // The owner's row is made, once.
    for _ in 0..2 {
        desk.on_writer(move |state| {
            state.mirror_change(Copies::One(owner), CopyChange::Linked, false, |state| {
                state.repository.mirror_ensure_owner(owner, 1)
            })
        })
        .unwrap();
    }
    seen.push(("never", "linked", 0));
    expect(&desk, &seen);

    // A reset from nothing, in three pages. The middle one is sent again.
    apply(&desk, page(owner, 0, 3, true, false)).unwrap();
    seen.push(("never", "reset_started", 0));
    expect(&desk, &seen);
    apply(&desk, page(owner, 3, 7, true, false)).unwrap();
    assert_eq!(
        apply(&desk, page(owner, 3, 7, true, false)),
        Err(MirrorRefusal::OutOfOrder)
    );
    expect(&desk, &seen);
    // The last one switches the copy and brings it up to date. Sent again,
    // it is refused.
    apply(&desk, page(owner, 7, 9, true, true)).unwrap();
    seen.push(("current", "reset_finished", 9));
    seen.push(("current", "caught_up", 9));
    expect(&desk, &seen);
    assert_eq!(
        apply(&desk, page(owner, 7, 9, true, true)),
        Err(MirrorRefusal::OutOfOrder)
    );
    // A page that continues the copy changes no state; sent again it is
    // refused.
    apply(&desk, page(owner, 9, 12, false, true)).unwrap();
    assert_eq!(
        apply(&desk, page(owner, 9, 12, false, true)),
        Err(MirrorRefusal::OutOfOrder)
    );
    // A page of another feed is refused.
    let mut foreign = page(owner, 12, 13, false, true);
    foreign.feed_id = START.into();
    assert_eq!(apply(&desk, foreign), Err(MirrorRefusal::OutOfOrder));
    expect(&desk, &seen);

    // The stream is lost, twice over.
    for _ in 0..2 {
        desk.on_writer(move |state| {
            state.mirror_change(Copies::One(owner), CopyChange::StreamLost, false, |state| {
                state
                    .repository
                    .mirror_set_state(owner, LinkedOwnerState::Stale, 6)
            })
        })
        .unwrap();
    }
    seen.push(("stale", "stream_lost", 12));
    expect(&desk, &seen);

    // A reset begins and is dropped before it finished, as a new connection
    // drops what the last one left. Dropping nothing says nothing.
    apply(&desk, page(owner, 0, 2, true, false)).unwrap();
    seen.push(("stale", "reset_started", 12));
    for _ in 0..2 {
        desk.on_writer(move |state| {
            state.mirror_change(
                Copies::One(owner),
                CopyChange::ResetDropped,
                false,
                |state| state.repository.mirror_discard_staging(owner, 7),
            )
        })
        .unwrap();
    }
    seen.push(("stale", "reset_dropped", 12));
    expect(&desk, &seen);

    // A start of the worker finds a copy that is stale: nothing to say.
    desk.on_writer(move |state| {
        state.mirror_change(Copies::All, CopyChange::WorkerStarted, false, |state| {
            state.repository.mirror_recover(8)
        })
    })
    .unwrap();
    expect(&desk, &seen);

    // The owner stops being a linked computer, twice over.
    for _ in 0..2 {
        desk.on_writer(move |state| {
            state.mirror_change(Copies::All, CopyChange::Unlinked, false, |state| {
                state.repository.mirror_withdraw(&[], 9)
            })
        })
        .unwrap();
    }
    seen.push(("withdrawing", "unlinked", 12));
    let events = expect(&desk, &seen);
    assert_eq!(removed(&events), 0);

    // The copy is deleted in batches; the batch that deletes its last row
    // publishes the removal. A batch for a copy that is gone says nothing.
    let delete = |desk: &Desk| {
        desk.on_writer(move |state| {
            state.mirror_deletion(
                owner,
                |state| state.repository.mirror_delete_batch(owner, 1),
                |step| matches!(step, Ok(MirrorDeleted::Done)),
            )
        })
        .unwrap()
    };
    let mut batches = 0;
    while delete(&desk) == MirrorDeleted::More {
        batches += 1;
        assert!(batches < 50);
        assert_eq!(removed(&rows(&desk, &world, owner)), 0);
    }
    assert_eq!(delete(&desk), MirrorDeleted::Done);
    let events = expect(&desk, &seen);
    assert_eq!(removed(&events), 1);
    assert_eq!(events.last().unwrap().name, REMOVED);
    assert_eq!(desk.rows()[..5], [0; 5]);
}
