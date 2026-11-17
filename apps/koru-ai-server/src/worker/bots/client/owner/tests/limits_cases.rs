// SPDX-License-Identifier: MPL-2.0
//! `m8_owner_limits`: the owner's limits of contract 2.4 and 2.5 that the
//! engine holds, since the carrier writes no frame: eight devices, four
//! streams a connection, an idle command stream, four commands in flight
//! for one device.
use koru_bot_client_wire::{Outcome, refusal};

use super::super::command::BUSY;
use super::*;

/// Eight devices are served at once; a ninth is refused `busy`. When one of
/// the eight goes away, another is served.
#[test]
fn m8_owner_limits_a_ninth_device_is_refused_busy() {
    let world = World::new();
    let mut open = Vec::new();
    for _ in 0..8 {
        let device = world.device();
        open.push(world.open(device, Role::Feed).0);
    }
    let ninth = world.device();
    assert_eq!(
        world.refused(ninth, hello(ninth, Role::Feed), 1),
        refusal::BUSY
    );
    assert_eq!(
        world.refused(ninth, hello(ninth, Role::Command), 1),
        refusal::BUSY
    );
    // One of the eight closes its stream.
    drop(open.pop());
    until("the ninth device is served", || {
        let mut client = world.connect(ninth, hello(ninth, Role::Feed), 1);
        matches!(
            world.runtime.block_on(client.next()),
            Some(WireMessage::OwnerHello(_))
        )
    });
}

/// One connection holds four streams; a fifth is refused `busy`, and a
/// stream that ends makes room. A connection that supersedes it has its own
/// four while the old one's streams end, and a device holds no more than
/// those two connections' worth, whatever connection its streams name.
#[test]
fn m8_owner_limits_a_fifth_stream_of_one_connection_is_refused_busy() {
    let world = World::new();
    let device = world.device();
    let mut open = Vec::new();
    for role in [Role::Feed, Role::Command, Role::Live, Role::Command] {
        open.push(world.open_on(device, role, 7).0);
    }
    assert_eq!(
        world.refused(device, hello(device, Role::Command), 7),
        refusal::BUSY
    );
    let mut newer = Vec::new();
    for _ in 0..4 {
        newer.push(world.open_on(device, Role::Command, 8).0);
    }
    for connection in [8, 9] {
        assert_eq!(
            world.refused(device, hello(device, Role::Command), connection),
            refusal::BUSY
        );
    }
    // Another device is not held to this one's count.
    let other = world.device();
    let (_served, _) = world.open_on(other, Role::Command, 7);
    drop(open.pop());
    until("a stream of the first connection is served again", || {
        let mut client = world.connect(device, hello(device, Role::Command), 7);
        matches!(
            world.runtime.block_on(client.next()),
            Some(WireMessage::OwnerHello(_))
        )
    });
}

/// An idle command stream is closed, without a frame, and one that is used
/// stays open.
#[test]
fn m8_owner_limits_an_idle_command_stream_is_closed() {
    let world = World::with(
        Timing {
            command_idle: Duration::from_millis(1_500),
            ..Timing::default()
        },
        Native::default(),
    );
    let device = world.device();
    let (mut idle, _) = world.open(device, Role::Command);
    let (mut used, _) = world.open(device, Role::Command);
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(500));
        let request = Uuid::new_v4();
        // A command that is refused is still a use of the stream.
        let receipt = world.command(
            &mut used,
            request,
            pause(&world.bot, request, world.bot.revision + 9),
        );
        assert_eq!(receipt.outcome, Outcome::Refused);
    }
    assert!(world.runtime.block_on(idle.next()).is_none());
}

/// One device has at most four commands in flight across its streams; a
/// fifth is answered `busy` and is not recorded, so it can be sent again
/// unchanged.
#[test]
fn m8_owner_limits_a_fifth_command_in_flight_is_refused_busy() {
    let world = World::new();
    let device = world.device();
    // Four streams, each with a command the local port holds.
    let mut held = Vec::new();
    let mut releases = Vec::new();
    for index in 0..4 {
        let (entered_tx, entered) = sync_channel(1);
        let (release, release_rx) = sync_channel(1);
        *world.local.gate.lock().unwrap() = Some((entered_tx, release_rx));
        let (stream, _) = world.open_on(device, Role::Command, 1 + index / 3);
        let request = Uuid::new_v4();
        world
            .runtime
            .block_on(stream.command(request, pause(&world.bot, request, world.bot.revision + 9)));
        entered.recv_timeout(Duration::from_secs(10)).unwrap();
        held.push(stream);
        releases.push(release);
    }
    let (mut fifth, _) = world.open_on(device, Role::Command, 2);
    let request = Uuid::new_v4();
    let command = pause(&world.bot, request, world.bot.revision + 9);
    let receipt = world.command(&mut fifth, request, command.clone());
    assert_eq!(receipt.outcome, Outcome::Refused);
    assert_eq!(code(&receipt), BUSY);
    assert!(receipt.failure.unwrap().retryable);
    assert_eq!(receipts(&world).len(), 4);

    for release in releases {
        release.send(()).unwrap();
    }
    for stream in &mut held {
        let receipt = world.runtime.block_on(stream.receipt());
        assert_eq!(code(&receipt), "revision_conflict");
    }
    // The same command, unchanged, is now taken.
    let receipt = world.command(&mut fifth, request, command);
    assert_eq!(code(&receipt), "revision_conflict");
    assert!(!receipt.replayed);
    assert_eq!(receipts(&world).len(), 5);
}
