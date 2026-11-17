// SPDX-License-Identifier: MPL-2.0
//! Cohort `m8_two_clients`: two linked devices at once, and the refusals of
//! the computer that runs the Bot reaching them unchanged.
use koru_ai_core::bots::BotConfig;
use koru_bot_client_wire::{Entry, GapReason, Outcome};

use super::*;

/// What pages say of each subject, as the bytes a copy would keep: every
/// member of an entry but its `seq`, which names when it was read.
fn said(pages: &[Changes]) -> Vec<String> {
    pages
        .iter()
        .flat_map(|page| &page.entries)
        .map(|entry: &Entry| {
            let mut entry = entry.clone();
            entry.seq = 0;
            serde_json::to_string(&entry).unwrap()
        })
        .collect()
}

fn renamed(bot: &Bot, name: &str) -> BotConfig {
    let mut config = bot.config.clone();
    config.name = name.into();
    config
}

fn update(bot: &Bot, request: Uuid, config: BotConfig) -> ClientCommand {
    ClientCommand::UpdateBot {
        request_id: BotRequestId::from_uuid(request),
        bot_id: bot.id,
        expected_revision: bot.revision,
        config,
    }
}

/// Two devices that subscribe are sent equal feeds: the same entries with
/// the same records, the same counts, and, after a command of one of them,
/// the same change. The command's receipt names a cursor the feed then
/// reaches.
#[test]
fn m8_two_clients_are_sent_equal_feeds() {
    let world = World::new();
    let (one, two) = (world.device(), world.device());
    let (mut feed_one, hello_one) = world.open(one, Role::Feed);
    let (mut feed_two, hello_two) = world.open(two, Role::Feed);
    assert_eq!(hello_one.feed_id, hello_two.feed_id);
    assert_eq!(hello_one.boot_id, hello_two.boot_id);
    assert_eq!(hello_one.owner_device, world.owner.device);

    let (first_one, first_two) = world.runtime.block_on(async {
        let mut first = Vec::new();
        for feed in [&mut feed_one, &mut feed_two] {
            feed.subscribe(None, None, None).await;
            match feed.next().await {
                Some(WireMessage::Gap(gap)) => {
                    assert_eq!(gap.reason, GapReason::FirstSubscribe);
                    assert_eq!(gap.owner_device, world.owner.device);
                }
                other => panic!("expected a gap, read {other:?}"),
            }
            first.push(feed.pages().await);
        }
        (first.remove(0), first.remove(0))
    });
    assert!(first_one.iter().all(|page| page.reset));
    assert_eq!(said(&first_one), said(&first_two));
    // The Bot and its main conversation.
    assert_eq!(said(&first_one).len(), 2);
    let last = first_one.last().unwrap();
    assert_eq!(last.live, first_two.last().unwrap().live);
    assert_eq!(last.live.map(|live| (live.bots, live.chats)), Some((1, 1)));
    assert_eq!(last.withheld_bots, Some(0));

    // One of them pauses the Bot; both are sent the Bot again, alike.
    let (mut commands, _) = world.open(one, Role::Command);
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
    let cursor = receipt
        .feed_cursor
        .expect("a command that took effect names a cursor");
    let (next_one, next_two) = world
        .runtime
        .block_on(async { (feed_one.pages().await, feed_two.pages().await) });
    assert!(next_one.iter().all(|page| !page.reset));
    assert!(next_one.last().unwrap().to_cursor >= cursor);
    assert_eq!(said(&next_one)[0], said(&next_two)[0]);
    let entry = &next_one[0].entries[0];
    assert_eq!(
        (entry.kind.as_str(), entry.id.as_str()),
        ("bot", world.bot.id.to_string().as_str())
    );
    assert!(said(&next_one)[0].contains("\"paused\":true"));
    audit_refused_nothing(&world);
}

/// Two devices change one Bot from the same revision: the first is applied,
/// the second is answered `revision_conflict`, and the Bot holds the first
/// one's change.
#[test]
fn m8_two_clients_the_second_writer_of_one_revision_gets_a_revision_conflict() {
    let world = World::new();
    let (one, two) = (world.device(), world.device());
    let (mut commands_one, _) = world.open(one, Role::Command);
    let (mut commands_two, _) = world.open(two, Role::Command);

    let request = Uuid::new_v4();
    let first = world.command(
        &mut commands_one,
        request,
        update(&world.bot, request, renamed(&world.bot, "From the laptop")),
    );
    assert_eq!(first.outcome, Outcome::Completed, "{first:?}");
    let request = Uuid::new_v4();
    let second = world.command(
        &mut commands_two,
        request,
        update(&world.bot, request, renamed(&world.bot, "From the tablet")),
    );
    assert_eq!(second.outcome, Outcome::Refused);
    assert_eq!(code(&second), "revision_conflict");
    assert_eq!(world.read_bot().config.name, "From the laptop");

    // The same through the local command itself: two pauses of one
    // revision, the second refused by the Bot's own revision check.
    let current = world.read_bot();
    let request = Uuid::new_v4();
    let first = world.command(
        &mut commands_one,
        request,
        pause(&current, request, current.revision),
    );
    assert!(matches!(
        first.outcome,
        Outcome::Accepted | Outcome::Completed
    ));
    let request = Uuid::new_v4();
    let second = world.command(
        &mut commands_two,
        request,
        pause(&current, request, current.revision),
    );
    assert_eq!(second.outcome, Outcome::Refused, "{second:?}");
    assert_eq!(
        world.local.applied(),
        ["update_bot", "pause_bot", "pause_bot"]
    );
    audit_refused_nothing(&world);
}

/// A message sent into a conversation whose reply is still running is
/// answered the existing `chat_busy`, and no second attempt starts.
#[test]
fn m8_two_clients_a_second_message_into_a_running_conversation_is_chat_busy() {
    // The execution owner begins the turn and never ends it.
    let world = World::with(Timing::default(), never_ending());
    let (one, two) = (world.device(), world.device());
    let (mut commands_one, _) = world.open(one, Role::Command);
    let (mut commands_two, _) = world.open(two, Role::Command);
    let request = Uuid::new_v4();
    let first = world.command(
        &mut commands_one,
        request,
        send(&world.bot, world.bot.main_chat, request),
    );
    assert_eq!(first.outcome, Outcome::Completed, "{first:?}");
    until("the turn began", || world.begun() == 1);

    let request = Uuid::new_v4();
    let second = world.command(
        &mut commands_two,
        request,
        send(&world.bot, world.bot.main_chat, request),
    );
    assert_eq!(second.outcome, Outcome::Refused);
    assert_eq!(code(&second), "chat_busy");
    assert!(second.failure.unwrap().retryable);
    assert_eq!(world.begun(), 1);
}

/// What a window on the computer that runs the Bot is refused, a linked
/// device is refused in the same words: a paused Bot and a paused team,
/// each compared with the answer the worker's own port gives a local
/// caller for the same message.
#[test]
fn m8_two_clients_a_paused_bot_and_a_paused_team_refuse_a_linked_device_as_they_refuse_a_window() {
    let world = World::new();
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);
    let local = |operation: Uuid| {
        let ClientCommand::SendTurn { request } = send(&world.bot, world.bot.main_chat, operation)
        else {
            unreachable!()
        };
        world
            .client()
            .send_turn(local_owner(), request)
            .map(|_| ())
            .unwrap_err()
    };

    // The Bot's own pause.
    let request = Uuid::new_v4();
    let paused = world.command(
        &mut commands,
        request,
        pause(&world.bot, request, world.bot.revision),
    );
    assert!(matches!(
        paused.outcome,
        Outcome::Accepted | Outcome::Completed
    ));
    world.settled_bot();
    let window = local(Uuid::new_v4());
    let request = Uuid::new_v4();
    let linked = world.command(
        &mut commands,
        request,
        send(&world.bot, world.bot.main_chat, request),
    );
    assert_eq!(linked.outcome, Outcome::Refused);
    let failure = linked.failure.unwrap();
    assert_eq!(
        (failure.code, failure.message, failure.retryable),
        (window.code, window.message, window.retryable)
    );

    // The team pause, with the Bot resumed first.
    let current = world.settled_bot();
    let request = Uuid::new_v4();
    let resumed = world.command(
        &mut commands,
        request,
        ClientCommand::ResumeBot {
            request_id: BotRequestId::from_uuid(request),
            bot_id: world.bot.id,
            expected_revision: current.revision,
        },
    );
    assert!(
        matches!(resumed.outcome, Outcome::Accepted | Outcome::Completed),
        "{resumed:?}"
    );
    world.settled_bot();
    let request = Uuid::new_v4();
    let team = world.command(
        &mut commands,
        request,
        ClientCommand::PauseTeam {
            request_id: BotRequestId::from_uuid(request),
            expected_team_revision: 0,
        },
    );
    assert!(
        matches!(team.outcome, Outcome::Accepted | Outcome::Completed),
        "{team:?}"
    );
    let window = local(Uuid::new_v4());
    assert_eq!(window.code, "team_paused");
    let request = Uuid::new_v4();
    let linked = world.command(
        &mut commands,
        request,
        send(&world.bot, world.bot.main_chat, request),
    );
    let failure = linked.failure.unwrap();
    assert_eq!(
        (failure.code, failure.message, failure.retryable),
        (window.code, window.message, window.retryable)
    );
    assert_eq!(world.begun(), 0);
    audit_refused_nothing(&world);
}

/// Whatever the local command refuses with reaches the linked device as it
/// is: its code, its words and whether a retry may help. Shown with the
/// refusal a session a person holds on this computer gives, scripted in
/// front of the worker's port.
#[test]
fn m8_two_clients_a_held_sessions_refusal_passes_through() {
    let world = World::new();
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);
    let held = Failure {
        code: "session_held".into(),
        message: "Somebody is using this Bot's session on this computer.".into(),
        retryable: true,
        details: Default::default(),
    };
    *world.local.scripted.lock().unwrap() = Some(held.clone());
    let request = Uuid::new_v4();
    let receipt = world.command(
        &mut commands,
        request,
        send(&world.bot, world.bot.main_chat, request),
    );
    assert_eq!(receipt.outcome, Outcome::Refused);
    let failure = receipt.failure.unwrap();
    assert_eq!(
        (failure.code, failure.message, failure.retryable),
        (held.code, held.message, held.retryable)
    );
    assert_eq!(world.begun(), 0);
}
