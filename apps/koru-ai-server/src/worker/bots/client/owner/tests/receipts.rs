// SPDX-License-Identifier: MPL-2.0
//! Cohort `m8_owner_receipts`: one command, at most one effect, over
//! `TransportSession::pair()` and the real worker. The repository's half
//! (each state of a row) is in `koru-ai-sqlite`.
use koru_ai_core::Timestamp;
use koru_ai_core::bots::TeamPauseView;
use koru_ai_wire::SendTurnAccepted;
use koru_bot_client_wire::Outcome;

use super::super::command::{BUSY, NOT_APPLIED, fingerprint};
use super::*;

/// A start of the worker that is not this one.
const EARLIER_START: &str = "000000000000000000000000000000e1";

/// Record `command` as a worker would before applying it, under `start`
/// (`None` is this run's own start), and stop there.
fn record(
    world: &World,
    device: DeviceId,
    request: Uuid,
    command: &ClientCommand,
    start: Option<&str>,
) {
    let (kind, print) = (command.kind(), fingerprint(command).unwrap());
    let start = start.map(str::to_owned);
    let begun = world.on_writer(move |state| {
        let start = start.unwrap_or_else(|| state.bot_clients.boot_id().unwrap());
        state.repository.client_receipt_begin(
            device,
            request,
            &print,
            kind,
            &start,
            Timestamp::now().millis(),
        )
    });
    assert_eq!(begun, Ok(koru_ai_sqlite::ClientReceiptBegin::Apply));
}

fn messages(world: &World) -> usize {
    world
        .client()
        .read_chat(local_owner(), world.bot.main_chat)
        .unwrap()
        .messages
        .len()
}

/// A `send_turn` sent twice starts one attempt: the second arrival is
/// answered from the receipt and applies nothing. The same after the owner's
/// worker is rebuilt between them.
#[test]
fn m8_owner_receipts_a_duplicate_send_turn_starts_no_second_attempt() {
    let mut world = World::new();
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);
    let request = Uuid::new_v4();
    let command = send(&world.bot, world.bot.main_chat, request);

    let first = world.command(&mut commands, request, command.clone());
    assert_eq!(first.outcome, Outcome::Completed, "{first:?}");
    assert!(!first.replayed);
    let accepted: SendTurnAccepted = first.response.clone().unwrap().parse().unwrap();
    assert!(!accepted.replayed);
    assert!(first.feed_cursor.is_some());
    until("the turn began", || world.begun() == 1);

    let second = world.command(&mut commands, request, command.clone());
    assert_eq!(second.outcome, Outcome::Completed);
    assert!(second.replayed);
    // The response is the first answer's alone; the cursor is the stored one.
    assert!(second.response.is_none() && second.failure.is_none());
    assert_eq!(second.feed_cursor, first.feed_cursor);
    assert_eq!(world.local.applied(), ["send_turn"]);
    until("the reply ended", || {
        world
            .client()
            .read_attempt(local_owner(), accepted.attempt_id)
            .is_ok_and(|read| read.attempt.status.is_terminal())
    });

    // The owner's worker is rebuilt: another start, the same `ai.db`.
    world.restart();
    let (mut commands, _) = world.open(device, Role::Command);
    let third = world.command(&mut commands, request, command);
    assert_eq!(third.outcome, Outcome::Completed);
    assert!(third.replayed);
    assert_eq!(third.feed_cursor, first.feed_cursor);
    assert!(world.local.applied().is_empty());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(world.begun(), 1);
    assert_eq!(messages(&world), 2);
    assert_eq!(
        receipts(&world),
        vec![row("send_turn", "done", Some("completed"))]
    );
    audit_refused_nothing(&world);
}

/// A `retry_turn` sent twice starts one new attempt, across a rebuild of
/// the owner's worker too.
#[test]
fn m8_owner_receipts_a_duplicate_retry_turn_starts_no_second_attempt() {
    // The execution owner fails the first turn, so there is a reply to
    // retry.
    let mut world = World::with(Timing::default(), failing_first());
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);
    let request = Uuid::new_v4();
    let sent = world.command(
        &mut commands,
        request,
        send(&world.bot, world.bot.main_chat, request),
    );
    let accepted: SendTurnAccepted = sent.response.unwrap().parse().unwrap();
    until("the first reply failed", || {
        world
            .client()
            .read_attempt(local_owner(), accepted.attempt_id)
            .is_ok_and(|read| read.attempt.status.is_terminal())
    });
    assert_eq!(world.begun(), 1);
    // The execution owner settles the failed turn before another is sent,
    // as the Bot conversation tests wait for it.
    until("the failed turn was interrupted", || {
        world.delegate.native.state.lock().unwrap().interrupt_calls >= 1
    });

    let request = Uuid::new_v4();
    let command = ClientCommand::RetryTurn {
        chat_id: world.bot.main_chat,
        request: retry(&world.bot, accepted.assistant_message_id, request),
    };
    let first = world.command(&mut commands, request, command.clone());
    assert_eq!(first.outcome, Outcome::Completed, "{first:?}");
    let retried: SendTurnAccepted = first.response.clone().unwrap().parse().unwrap();
    assert_ne!(retried.attempt_id, accepted.attempt_id);
    until("the retry began", || world.begun() == 2);

    let second = world.command(&mut commands, request, command.clone());
    assert!(second.replayed && second.outcome == Outcome::Completed);
    until("the retried reply ended", || {
        world
            .client()
            .read_attempt(local_owner(), retried.attempt_id)
            .is_ok_and(|read| read.attempt.status.is_terminal())
    });
    world.restart();
    let (mut commands, _) = world.open(device, Role::Command);
    let third = world.command(&mut commands, request, command);
    assert!(third.replayed && third.outcome == Outcome::Completed);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(world.begun(), 2);
    assert!(world.local.applied().is_empty());
    audit_refused_nothing(&world);
}

/// A receipt left `applying` by an earlier start of the worker answers
/// `unknown`, stores `unknown`, and applies nothing: now, and at every later
/// arrival. Before the worker was rebuilt, the same open row answered
/// `busy`.
#[test]
fn m8_owner_receipts_a_receipt_left_open_by_an_earlier_start_answers_unknown() {
    let mut world = World::new();
    let device = world.device();
    let request = Uuid::new_v4();
    let command = send(&world.bot, world.bot.main_chat, request);
    // This start recorded the command and has not closed it.
    record(&world, device, request, &command, None);
    let (mut commands, _) = world.open(device, Role::Command);
    let busy = world.command(&mut commands, request, command.clone());
    assert_eq!(busy.outcome, Outcome::Refused);
    assert_eq!(code(&busy), BUSY);
    assert!(busy.failure.as_ref().unwrap().retryable && !busy.replayed);
    assert_eq!(receipts(&world), vec![row("send_turn", "applying", None)]);

    // The worker stops there, and another starts.
    world.restart();
    let (mut commands, _) = world.open(device, Role::Command);
    for _ in 0..2 {
        let receipt = world.command(&mut commands, request, command.clone());
        assert_eq!(receipt.outcome, Outcome::Unknown);
        assert!(receipt.replayed);
        assert!(receipt.response.is_none() && receipt.failure.is_none());
        assert_eq!(receipt.feed_cursor, None);
    }
    assert_eq!(
        receipts(&world),
        vec![row("send_turn", "done", Some("unknown"))]
    );
    assert!(world.local.applied().is_empty());
    assert_eq!(world.begun(), 0);
    assert_eq!(messages(&world), 0);
}

/// A re-send that arrives while the first arrival is still being applied,
/// under the same start, answers `busy` and changes nothing; the first
/// arrival then completes once, and a later re-send is answered from its
/// receipt.
#[test]
fn m8_owner_receipts_a_resend_while_the_first_is_being_applied_answers_busy() {
    let world = World::new();
    let device = world.device();
    let (entered_tx, entered) = sync_channel(1);
    let (release, release_rx) = sync_channel(1);
    *world.local.gate.lock().unwrap() = Some((entered_tx, release_rx));
    let (mut first, _) = world.open(device, Role::Command);
    let (mut second, _) = world.open(device, Role::Command);
    let request = Uuid::new_v4();
    let command = send(&world.bot, world.bot.main_chat, request);

    world
        .runtime
        .block_on(first.command(request, command.clone()));
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    let resent = world.command(&mut second, request, command.clone());
    assert_eq!(resent.outcome, Outcome::Refused);
    assert_eq!(code(&resent), BUSY);
    assert!(resent.failure.as_ref().unwrap().retryable);
    assert!(!resent.replayed && resent.feed_cursor.is_none());
    assert_eq!(receipts(&world), vec![row("send_turn", "applying", None)]);
    assert_eq!(world.begun(), 0);

    release.send(()).unwrap();
    let done = world.runtime.block_on(first.receipt());
    assert_eq!(done.outcome, Outcome::Completed, "{done:?}");
    let again = world.command(&mut second, request, command);
    assert!(again.replayed && again.outcome == Outcome::Completed);
    until("the turn began", || world.begun() == 1);
    assert_eq!(world.local.applied(), ["send_turn"]);
}

/// A wrapper `request_id` that differs from the embedded command's is
/// refused `invalid_request` and recorded nowhere. The same request id with
/// other content is `request_conflict`, and the first command's effect
/// stands.
#[test]
fn m8_owner_receipts_one_request_id_per_intent() {
    let world = World::new();
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);
    let revision = world.bot.revision;

    let (wrapper, embedded) = (Uuid::new_v4(), Uuid::new_v4());
    for command in [
        pause(&world.bot, embedded, revision),
        send(&world.bot, world.bot.main_chat, embedded),
    ] {
        let receipt = world.command(&mut commands, wrapper, command);
        assert_eq!(receipt.outcome, Outcome::Refused);
        assert_eq!(code(&receipt), "invalid_request");
    }
    assert!(receipts(&world).is_empty());
    assert!(world.local.applied().is_empty());

    let request = Uuid::new_v4();
    let first = world.command(&mut commands, request, pause(&world.bot, request, revision));
    assert!(
        matches!(first.outcome, Outcome::Accepted | Outcome::Completed),
        "{first:?}"
    );
    let other = world.command(
        &mut commands,
        request,
        ClientCommand::ResumeBot {
            request_id: BotRequestId::from_uuid(request),
            bot_id: world.bot.id,
            expected_revision: revision,
        },
    );
    assert_eq!(other.outcome, Outcome::Refused);
    assert_eq!(code(&other), "request_conflict");
    assert!(!other.replayed);
    assert!(world.read_bot().paused);
    assert_eq!(world.local.applied(), ["pause_bot"]);
    assert_eq!(receipts(&world).len(), 1);
    audit_refused_nothing(&world);
}

/// A refusal is a receipt too: the local command's own refusal is recorded
/// and a repeat is answered from the row, without the local command being
/// asked again.
#[test]
fn m8_owner_receipts_a_refusal_is_recorded_and_replayed() {
    let world = World::new();
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);
    let request = Uuid::new_v4();
    // A revision the Bot does not have.
    let command = pause(&world.bot, request, world.bot.revision + 7);
    let first = world.command(&mut commands, request, command.clone());
    assert_eq!(first.outcome, Outcome::Refused);
    assert_eq!(code(&first), "revision_conflict");
    assert!(!first.replayed && first.feed_cursor.is_none());
    let second = world.command(&mut commands, request, command);
    assert_eq!(second.outcome, Outcome::Refused);
    assert_eq!(code(&second), "revision_conflict");
    assert!(second.replayed);
    assert!(!second.failure.unwrap().retryable);
    assert_eq!(world.local.applied(), ["pause_bot"]);
    assert!(!world.read_bot().paused);
}

/// A local command that was handed over and gave no answer in time may
/// still run: the receipt is `unknown`, never `refused`, and a repeat is
/// answered `unknown` without the command being handed over again.
#[test]
fn m8_owner_receipts_a_local_command_that_did_not_answer_is_unknown() {
    let world = World::new();
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);
    *world.local.scripted.lock().unwrap() = Some(Failure {
        code: "ai_worker_timeout".into(),
        message: "The AI service did not answer in time.".into(),
        retryable: true,
        details: Default::default(),
    });
    let request = Uuid::new_v4();
    let command = send(&world.bot, world.bot.main_chat, request);
    let first = world.command(&mut commands, request, command.clone());
    assert_eq!(first.outcome, Outcome::Unknown);
    assert_eq!(code(&first), "ai_worker_timeout");
    let second = world.command(&mut commands, request, command);
    assert_eq!(second.outcome, Outcome::Unknown);
    assert!(second.replayed);
    assert_eq!(world.local.applied(), ["send_turn"]);
    assert_eq!(world.begun(), 0);
}

/// The settled kinds. A stranded `pause_bot` whose own receipt exists was
/// applied (`accepted` while it waits for its owner); one with none was
/// never applied (`refused`, `not_applied`), and neither is applied now.
/// The same for `create_chat` and `update_bot` with no receipt of their
/// own.
#[test]
fn m8_owner_receipts_a_stranded_bot_command_is_settled_from_its_own_receipt() {
    let world = World::new();
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);
    let (bot, owner, revision) = (world.bot.clone(), world.owner, world.bot.revision);

    // Applied by the worker that stopped: the Bot command's own receipt is
    // there, in the transaction of the pause.
    let applied = Uuid::new_v4();
    let command = pause(&bot, applied, revision);
    record(&world, device, applied, &command, Some(EARLIER_START));
    world.on_writer({
        let bot = bot.id;
        move |state| {
            state
                .repository
                .prepare_bot_pause(
                    owner,
                    bot,
                    BotRequestId::from_uuid(applied),
                    revision,
                    true,
                    &"ab".repeat(32),
                    Timestamp::now().millis(),
                )
                .unwrap();
        }
    });
    let receipt = world.command(&mut commands, applied, command);
    // `accepted` while the pause waits for its execution owner, `completed`
    // once a recovery pass of the worker has delivered it.
    assert!(
        matches!(receipt.outcome, Outcome::Accepted | Outcome::Completed),
        "{receipt:?}"
    );
    assert!(receipt.replayed && receipt.failure.is_none());
    assert!(receipt.feed_cursor.is_some());
    let paused = world.read_bot();
    assert!(paused.paused);

    // Recorded and never applied.
    let mut renamed = bot.config.clone();
    renamed.name = "Never applied".into();
    let never = [
        ClientCommand::ResumeBot {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: paused.revision,
        },
        ClientCommand::CreateChat {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
        },
        ClientCommand::UpdateBot {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: paused.revision,
            config: renamed,
        },
    ];
    for command in never {
        let request = command.embedded_request_id().unwrap();
        record(&world, device, request, &command, Some(EARLIER_START));
        for _ in 0..2 {
            let receipt = world.command(&mut commands, request, command.clone());
            assert_eq!(receipt.outcome, Outcome::Refused, "{receipt:?}");
            assert_eq!(code(&receipt), NOT_APPLIED);
            assert!(receipt.replayed && receipt.feed_cursor.is_none());
            assert!(!receipt.failure.unwrap().retryable);
        }
    }
    // Nothing of the four was handed to a local command, and the Bot is as
    // the pause left it.
    assert!(world.local.applied().is_empty());
    assert_eq!(world.read_bot(), paused);
}

/// A stranded `pause_team` whose request is on the team's row was applied;
/// one that finds the team already in the state it asked for is `completed`;
/// and one that finds the team in the other state, with another request on
/// the row, cannot be told from one that was applied and undone: `unknown`.
#[test]
fn m8_owner_receipts_a_stranded_team_command_is_settled_from_the_team_row() {
    let world = World::new();
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);
    let owner = world.owner;
    let team = |paused: bool, request: Uuid, expected: u64| {
        let request_id = BotRequestId::from_uuid(request);
        if paused {
            ClientCommand::PauseTeam {
                request_id,
                expected_team_revision: expected,
            }
        } else {
            ClientCommand::ResumeTeam {
                request_id,
                expected_team_revision: expected,
            }
        }
    };

    // The row carries the request: the pause was applied by the worker
    // that stopped.
    let applied = Uuid::new_v4();
    let command = team(true, applied, 0);
    record(&world, device, applied, &command, Some(EARLIER_START));
    world.on_writer(move |state| {
        state
            .repository
            .change_team_pause(
                owner,
                BotRequestId::from_uuid(applied),
                &"cd".repeat(32),
                0,
                true,
                Timestamp::now().millis(),
            )
            .unwrap();
    });
    let receipt = world.command(&mut commands, applied, command);
    assert!(
        matches!(receipt.outcome, Outcome::Accepted | Outcome::Completed),
        "{receipt:?}"
    );
    assert!(receipt.replayed);

    // Another pause, never applied: the team is paused, which is what it
    // asked for.
    let same = Uuid::new_v4();
    let command = team(true, same, 0);
    record(&world, device, same, &command, Some(EARLIER_START));
    let receipt = world.command(&mut commands, same, command);
    assert_eq!(receipt.outcome, Outcome::Completed, "{receipt:?}");

    // A resume that finds the team paused under another request.
    let other = Uuid::new_v4();
    let command = team(false, other, 1);
    record(&world, device, other, &command, Some(EARLIER_START));
    let receipt = world.command(&mut commands, other, command);
    assert_eq!(receipt.outcome, Outcome::Unknown, "{receipt:?}");
    assert!(receipt.replayed && receipt.failure.is_none());

    // None of the three was applied now: the row is the one written above.
    assert!(world.local.applied().is_empty());
    let view: TeamPauseView =
        world.on_writer(move |state| state.repository.team_pause_view(owner).unwrap());
    assert_eq!((view.paused, view.revision), (true, 1));
}
