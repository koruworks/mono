// SPDX-License-Identifier: MPL-2.0
//! `m8_owner_commands`: commands of the allow-list that the four contract
//! cohorts do not drive to their effect: a stop, and the team's pause and
//! resume as a pair.
use koru_ai_wire::{CancelAttemptResponse, SendTurnAccepted};
use koru_bot_client_wire::Outcome;

use super::*;

/// A linked device stops a running reply: the local stop runs once, the
/// reply ends, and a repeat of the same command is answered from its
/// receipt.
#[test]
fn m8_owner_commands_a_stop_ends_the_running_reply_once() {
    // The execution owner begins the turn, never ends it, and confirms a
    // stop.
    let mut native = never_ending();
    native.fail_first = true;
    let world = World::with(Timing::default(), native);
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);
    let request = Uuid::new_v4();
    let sent = world.command(
        &mut commands,
        request,
        send(&world.bot, world.bot.main_chat, request),
    );
    let accepted: SendTurnAccepted = sent.response.unwrap().parse().unwrap();
    until("the turn began", || world.begun() == 1);

    let request = Uuid::new_v4();
    let stop = ClientCommand::CancelAttempt {
        attempt_id: accepted.attempt_id,
    };
    let first = world.command(&mut commands, request, stop.clone());
    assert_eq!(first.outcome, Outcome::Completed, "{first:?}");
    let answered: CancelAttemptResponse = first.response.unwrap().parse().unwrap();
    assert_eq!(answered.attempt_id, accepted.attempt_id);
    until("the reply ended", || {
        world
            .client()
            .read_attempt(local_owner(), accepted.attempt_id)
            .is_ok_and(|read| read.attempt.status.is_terminal())
    });
    let again = world.command(&mut commands, request, stop);
    assert!(again.replayed && again.outcome == Outcome::Completed);
    assert_eq!(world.local.applied(), ["send_turn", "cancel_attempt"]);
    assert_eq!(world.begun(), 1);
    audit_refused_nothing(&world);
}

/// A linked device pauses and resumes every Bot of this computer: each is
/// the local team command, the team's row moves once for each, and what the
/// owner then reads is what the receipts said.
#[test]
fn m8_owner_commands_the_team_is_paused_and_resumed() {
    let world = World::new();
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);
    let owner = world.owner;
    let team = move |world: &World| {
        world.on_writer(move |state| state.repository.team_pause_view(owner).unwrap())
    };

    let request = Uuid::new_v4();
    let paused = world.command(
        &mut commands,
        request,
        ClientCommand::PauseTeam {
            request_id: BotRequestId::from_uuid(request),
            expected_team_revision: 0,
        },
    );
    assert!(
        matches!(paused.outcome, Outcome::Accepted | Outcome::Completed),
        "{paused:?}"
    );
    let BotResponse::TeamPause { team: answered } = paused.response.unwrap().parse().unwrap()
    else {
        panic!("the team's pause")
    };
    assert_eq!((answered.paused, answered.revision), (true, 1));
    assert_eq!(answered.owner_device, world.owner.device);
    let view = team(&world);
    assert_eq!((view.paused, view.revision), (true, 1));

    // A resume at a revision the team does not have is the local command's
    // own refusal.
    let request = Uuid::new_v4();
    let stale = world.command(
        &mut commands,
        request,
        ClientCommand::ResumeTeam {
            request_id: BotRequestId::from_uuid(request),
            expected_team_revision: 0,
        },
    );
    assert_eq!(code(&stale), "team_revision_conflict");

    let request = Uuid::new_v4();
    let resumed = world.command(
        &mut commands,
        request,
        ClientCommand::ResumeTeam {
            request_id: BotRequestId::from_uuid(request),
            expected_team_revision: 1,
        },
    );
    assert!(
        matches!(resumed.outcome, Outcome::Accepted | Outcome::Completed),
        "{resumed:?}"
    );
    let view = team(&world);
    assert_eq!((view.paused, view.revision), (false, 2));
    assert_eq!(
        world.local.applied(),
        ["pause_team", "resume_team", "resume_team"]
    );
    audit_refused_nothing(&world);
}
