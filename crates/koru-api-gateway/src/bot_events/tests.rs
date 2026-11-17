// SPDX-License-Identifier: MPL-2.0

//! The queue between the AI worker and the journal: a row is written in
//! the order it was published, and publishing never waits. No journal is
//! opened here; what is done with a row is a closure.

use std::sync::Mutex;
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use koru_ai_server::fixtures::BotEventRecorder;
use koru_ai_server::{BotEvents, BotStreamTurnedAway};
use koru_bot_client_wire::{Role, refusal};
use koru_device::DeviceId;

use super::*;

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Publish one row for `peer` through `sink`, as the Bot ingress does.
fn turn_away(sink: &Arc<BotEventJournal>, peer: DeviceId) {
    BotEvents::new(sink.clone() as Arc<dyn BotEventSink>).stream_turned_away(
        peer,
        Some(Role::Feed),
        BotStreamTurnedAway::Refused(refusal::NOT_OWN_DEVICE),
    );
}

#[test]
fn m8_events_content_free_rows_reach_the_journal_in_order() {
    let written = BotEventRecorder::new();
    let journal = {
        let written = written.clone();
        BotEventJournal::start_with(move |draft| written.publish(draft)).unwrap()
    };
    let peers: Vec<DeviceId> = (0..20).map(|_| DeviceId::new()).collect();
    for peer in &peers {
        turn_away(&journal, *peer);
    }
    until("every row is written", || written.count() == peers.len());
    let order: Vec<String> = written
        .events()
        .iter()
        .map(|event| event.attr("peer_device").unwrap().to_owned())
        .collect();
    let published: Vec<String> = peers.iter().map(ToString::to_string).collect();
    assert_eq!(order, published);
}

#[test]
fn m8_events_content_free_publishing_never_waits_for_the_journal() {
    // A journal that takes no row until it is released.
    let (release, held) = sync_channel::<()>(0);
    let held = Mutex::new(held);
    let written = BotEventRecorder::new();
    let journal = {
        let written = written.clone();
        BotEventJournal::start_with(move |draft| {
            let _ = held.lock().unwrap().recv();
            written.publish(draft);
        })
        .unwrap()
    };
    // One row is in the writer's hands, `QUEUE` wait, and the rest are
    // dropped. No call waits.
    let started = Instant::now();
    for _ in 0..QUEUE + 50 {
        turn_away(&journal, DeviceId::new());
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "publishing waited for the journal"
    );
    assert_eq!(written.count(), 0);
    // Released, the journal takes what was queued and nothing more.
    drop(release);
    until("the queue is written", || written.count() >= QUEUE);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        written.count() <= QUEUE + 1,
        "{} rows for a queue of {QUEUE}",
        written.count()
    );
    // The sink still works afterwards.
    let before = written.count();
    turn_away(&journal, DeviceId::new());
    until("a later row is written", || written.count() == before + 1);
}
