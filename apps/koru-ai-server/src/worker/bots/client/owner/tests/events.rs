// SPDX-License-Identifier: MPL-2.0
//! Cohort `m8_events_content_free`: the three rows the owner server
//! publishes (`bots.client.session`, `bots.client.command`,
//! `bots.client.reset`), over `TransportSession::pair()` and the real
//! worker. Each is published at its transition, once, holds only its listed
//! attributes, and a repeated command publishes nothing.
use koru_ai_core::Timestamp;
use koru_bot_client_wire::{GapReason, Outcome, refusal};

use super::super::command::{NOT_APPLIED, fingerprint};
use super::*;

const SESSION: &str = "bots.client.session";
const COMMAND: &str = "bots.client.command";
const RESET: &str = "bots.client.reset";

/// A start of the worker that is not this one.
const EARLIER_START: &str = "000000000000000000000000000000e1";
/// A build string a linked device says in its hello: text of another
/// computer.
const BUILD: &str = "Budget-Bot-Build";

/// What must never be in a row: the Bot's name and folder, what was typed,
/// the labels of the callers, and what the other computer said of itself.
fn content(world: &World) -> Vec<String> {
    vec![
        world.bot.config.name.clone(),
        world.bot.config.working_folder.clone(),
        "CURRENT-TASK".into(),
        "linked-device".into(),
        "owner-window".into(),
        BUILD.into(),
    ]
}

fn holds_no_content(world: &World, events: &[RecordedBotEvent]) {
    let content = content(world);
    let content: Vec<&str> = content.iter().map(String::as_str).collect();
    assert_holds_none_of(events, &content);
    for event in events {
        assert_eq!(event.actor, "ai-worker");
    }
}

/// Wait until `count` rows of `name` were published, check that no more
/// follow, and answer them.
fn published(world: &World, name: &str, count: usize) -> Vec<RecordedBotEvent> {
    until("the rows were published", || {
        world.events.named(name).len() >= count
    });
    std::thread::sleep(Duration::from_millis(200));
    let events = world.events.named(name);
    assert_eq!(events.len(), count, "{events:?}");
    holds_no_content(world, &events);
    events
}

fn text(value: &str) -> Option<String> {
    Some(value.to_owned())
}

/// `(peer, stage, role, protocol_version, reason)`.
type Session = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn session(event: &RecordedBotEvent) -> Session {
    for key in event.keys() {
        assert!(
            ["peer_device", "stage", "role", "protocol_version", "reason"].contains(&key),
            "{key}"
        );
    }
    let attr = |key: &str| event.attr(key).map(str::to_owned);
    (
        attr("peer_device"),
        attr("stage"),
        attr("role"),
        attr("protocol_version"),
        attr("reason"),
    )
}

fn hello_from(device: DeviceId, role: Role) -> ClientHello {
    let mut said = hello(device, role);
    said.client_build = BUILD.into();
    said
}

/// A stream publishes `admitted` when its hello is answered and one end:
/// `closed` when the device goes away, `refused` when it is turned away
/// before its hello is answered, and `closed` with the refusal's reason
/// when an admitted stream is ended by one.
#[test]
fn m8_events_content_free_a_stream_is_admitted_and_ends_once() {
    let world = World::new();
    let device = world.device();
    let peer = text(&device.to_string());

    // Admitted, then the device closes it.
    let mut feed = world.connect(device, hello_from(device, Role::Feed), 1);
    assert!(matches!(
        world.runtime.block_on(feed.next()),
        Some(WireMessage::OwnerHello(_))
    ));
    let events = published(&world, SESSION, 1);
    assert_eq!(
        session(&events[0]),
        (
            peer.clone(),
            text("admitted"),
            text("feed"),
            text("1"),
            None
        )
    );
    drop(feed);
    let events = published(&world, SESSION, 2);
    assert_eq!(
        session(&events[1]),
        (
            peer.clone(),
            text("closed"),
            text("feed"),
            text("1"),
            text("closed")
        )
    );

    // A device the admission port does not name.
    let other = DeviceId::from_uuid(Uuid::new_v4());
    assert_eq!(
        world.refused(other, hello_from(other, Role::Command), 1),
        refusal::NOT_OWN_DEVICE
    );
    let events = published(&world, SESSION, 3);
    assert_eq!(
        session(&events[2]),
        (
            text(&other.to_string()),
            text("refused"),
            text("command"),
            None,
            text("not_own_device")
        )
    );

    // No common version: none is published either.
    let mut newer = hello_from(device, Role::Live);
    (newer.min_version, newer.max_version) = (2, 3);
    assert_eq!(
        world.refused(device, newer, 1),
        refusal::UNSUPPORTED_VERSION
    );
    let events = published(&world, SESSION, 4);
    assert_eq!(
        session(&events[3]),
        (
            peer.clone(),
            text("refused"),
            text("live"),
            None,
            text("unsupported_version")
        )
    );

    // Admitted, and then the port stops naming the device: its next message
    // ends the stream with a refusal, which is how the stream closed.
    let (mut commands, _) = world.open(device, Role::Command);
    published(&world, SESSION, 5);
    world.port.forget(device);
    let request = Uuid::new_v4();
    let reason = world.runtime.block_on(async {
        commands
            .command(request, pause(&world.bot, request, world.bot.revision))
            .await;
        commands.refusal().await
    });
    assert_eq!(reason, refusal::NOT_OWN_DEVICE);
    let events = published(&world, SESSION, 6);
    assert_eq!(
        session(&events[4]),
        (
            peer.clone(),
            text("admitted"),
            text("command"),
            text("1"),
            None
        )
    );
    assert_eq!(
        session(&events[5]),
        (
            peer,
            text("closed"),
            text("command"),
            text("1"),
            text("not_own_device")
        )
    );
    // Nothing was recorded, so no command was published.
    assert!(receipts(&world).is_empty());
    assert!(world.events.named(COMMAND).is_empty());
    assert!(world.events.named(RESET).is_empty());
}

/// `(request, kind, outcome, failure)`, checked to be of `device` and to
/// hold nothing else.
fn command_row(
    event: &RecordedBotEvent,
    device: DeviceId,
) -> (String, String, String, Option<String>) {
    for key in event.keys() {
        assert!(
            ["peer_device", "request", "kind", "outcome", "failure"].contains(&key),
            "{key}"
        );
    }
    assert_eq!(event.attr("peer_device"), Some(device.to_string().as_str()));
    (
        event.attr("request").unwrap().to_owned(),
        event.attr("kind").unwrap().to_owned(),
        event.attr("outcome").unwrap().to_owned(),
        event.attr("failure").map(str::to_owned),
    )
}

fn recorded(
    request: Uuid,
    kind: &str,
    outcome: &str,
    failure: Option<&str>,
) -> (String, String, String, Option<String>) {
    (
        request.to_string(),
        kind.to_owned(),
        outcome.to_owned(),
        failure.map(str::to_owned),
    )
}

/// A command is published when its receipt is recorded, once. A repeat
/// answered from the receipt publishes nothing, on the same start of the
/// worker and after it was rebuilt; neither does a command that was refused
/// before anything was recorded. A command an earlier start left open is
/// published when it is settled, once.
#[test]
fn m8_events_content_free_a_command_is_published_with_its_receipt_and_never_on_a_replay() {
    let mut world = World::new();
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);

    // A message, applied.
    let request = Uuid::new_v4();
    let command = send(&world.bot, world.bot.main_chat, request);
    let first = world.command(&mut commands, request, command.clone());
    assert_eq!(first.outcome, Outcome::Completed, "{first:?}");
    let events = published(&world, COMMAND, 1);
    assert_eq!(
        command_row(&events[0], device),
        recorded(request, "send_turn", "completed", None)
    );
    assert_eq!(
        events[0].keys(),
        ["kind", "outcome", "peer_device", "request"]
    );
    // Sent again: answered from the receipt.
    let second = world.command(&mut commands, request, command.clone());
    assert!(second.replayed);

    // Refused before anything was recorded: the wrapper's request id is not
    // the embedded command's.
    let stray = world.command(
        &mut commands,
        Uuid::new_v4(),
        pause(&world.bot, Uuid::new_v4(), world.bot.revision),
    );
    assert_eq!(code(&stray), "invalid_request");

    // Recorded and refused by the local command: a stale revision.
    let conflict = Uuid::new_v4();
    let stale = pause(&world.bot, conflict, world.bot.revision + 9);
    let refused = world.command(&mut commands, conflict, stale.clone());
    assert_eq!(refused.outcome, Outcome::Refused);
    assert_eq!(code(&refused), "revision_conflict");
    let events = published(&world, COMMAND, 2);
    assert_eq!(
        command_row(&events[1], device),
        recorded(conflict, "pause_bot", "refused", Some("revision_conflict"))
    );
    assert!(world.command(&mut commands, conflict, stale).replayed);

    // A local failure whose code is not one of the declared set: the
    // receipt keeps the code, the row says `other`.
    *world.local.scripted.lock().unwrap() = Some(crate::worker::failure(
        "Budget-Bot-Secret",
        "Budget Bot said no",
        false,
    ));
    let odd = Uuid::new_v4();
    let answered = world.command(
        &mut commands,
        odd,
        pause(&world.bot, odd, world.bot.revision),
    );
    assert_eq!(code(&answered), "Budget-Bot-Secret");
    let events = published(&world, COMMAND, 3);
    assert_eq!(
        command_row(&events[2], device),
        recorded(odd, "pause_bot", "refused", Some("other"))
    );
    assert_holds_none_of(&events, &["Budget-Bot-Secret", "Budget Bot said no"]);

    // The owner's worker is rebuilt: the same request is still answered
    // from its receipt and publishes nothing.
    let accepted: koru_ai_wire::SendTurnAccepted = first.response.clone().unwrap().parse().unwrap();
    until("the reply ended", || {
        world
            .client()
            .read_attempt(local_owner(), accepted.attempt_id)
            .is_ok_and(|read| read.attempt.status.is_terminal())
    });
    world.restart();
    let (mut commands, _) = world.open(device, Role::Command);
    assert!(world.command(&mut commands, request, command).replayed);
    published(&world, COMMAND, 3);

    // A command an earlier start recorded and never closed is settled at
    // its next arrival: published then, once.
    let stranded = Uuid::new_v4();
    let left = pause(&world.bot, stranded, world.read_bot().revision);
    let (kind, print) = (left.kind(), fingerprint(&left).unwrap());
    let begun = world.on_writer(move |state| {
        state.repository.client_receipt_begin(
            device,
            stranded,
            &print,
            kind,
            EARLIER_START,
            Timestamp::now().millis(),
        )
    });
    assert_eq!(begun, Ok(koru_ai_sqlite::ClientReceiptBegin::Apply));
    published(&world, COMMAND, 3);
    let settled = world.command(&mut commands, stranded, left.clone());
    assert!(settled.replayed);
    assert_eq!(code(&settled), NOT_APPLIED);
    let events = published(&world, COMMAND, 4);
    assert_eq!(
        command_row(&events[3], device),
        recorded(stranded, "pause_bot", "refused", Some("not_applied"))
    );
    assert!(world.command(&mut commands, stranded, left).replayed);
    published(&world, COMMAND, 4);
    // Nothing of it was applied a second time.
    assert!(world.local.applied().is_empty());
    audit_refused_nothing(&world);
}

/// A reset is published when the `gap` is written, with the gap's reason.
/// A subscription that continues from its cursor publishes nothing.
#[test]
fn m8_events_content_free_a_reset_is_published_when_the_gap_is_written() {
    let world = World::new();
    let device = world.device();
    let (mut feed, hello) = world.open(device, Role::Feed);
    assert!(world.events.named(RESET).is_empty());
    let pages = world.runtime.block_on(async {
        feed.subscribe(None, None, None).await;
        match feed.next().await {
            Some(WireMessage::Gap(gap)) => assert_eq!(gap.reason, GapReason::FirstSubscribe),
            other => panic!("expected a gap, read {other:?}"),
        }
        feed.pages().await
    });
    let cursor = pages.last().unwrap().to_cursor;
    let reset = |event: &RecordedBotEvent| {
        assert_eq!(event.keys(), ["peer_device", "reason"]);
        assert_eq!(event.attr("peer_device"), Some(device.to_string().as_str()));
        event.attr("reason").unwrap().to_owned()
    };
    let events = published(&world, RESET, 1);
    assert_eq!(reset(&events[0]), "first_subscribe");

    // The same device subscribes from the cursor it holds, on another
    // stream: the owner continues from it. No gap, no row.
    let (mut again, _) = world.open_on(device, Role::Feed, 2);
    let next = world.runtime.block_on(async {
        again
            .subscribe(Some(&hello.feed_id), Some(&hello.boot_id), Some(cursor))
            .await;
        again.pages().await
    });
    assert!(next.iter().all(|page| !page.reset));
    published(&world, RESET, 1);

    // The client asks for a reset on its first stream.
    world.runtime.block_on(async {
        feed.subscribe(None, None, None).await;
        match feed.next().await {
            Some(WireMessage::Gap(gap)) => assert_eq!(gap.reason, GapReason::ClientRequested),
            other => panic!("expected a gap, read {other:?}"),
        }
        feed.pages().await
    });
    let events = published(&world, RESET, 2);
    assert_eq!(reset(&events[1]), "client_requested");
    assert!(world.events.named(COMMAND).is_empty());
}
