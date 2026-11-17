// SPDX-License-Identifier: MPL-2.0

//! M8 piece B at the actor: the `TeamBarrier` command (delegate IPC 20).
//! Cohort `m8_team_value`. In-memory ledger; no session, Store or provider.
use super::*;
use koru_delegate_api::bots::{BotCommand, BotReply, TEAM_HELD_EPOCH, team_held_epoch};
use koru_delegate_core::{BotOwner, BotRequestId, BotTeamBarrier, DeviceId};

fn team(owner: BotOwner, epoch: u64, paused: bool) -> BotTeamBarrier {
    BotTeamBarrier {
        owner,
        epoch,
        paused,
        request: (epoch > 0).then(BotRequestId::mint),
    }
}

fn send(actor: &mut Actor, barrier: &BotTeamBarrier) -> Response {
    let (reply, answer) = mpsc::sync_channel(1);
    actor.handle(Command::Bot {
        command: BotCommand::TeamBarrier {
            barrier: barrier.clone(),
        },
        reply,
    });
    answer.try_recv().unwrap()
}

fn failure(response: Response) -> Failure {
    match response {
        Response::Failure(failure) => failure,
        _ => panic!("a refusal was expected"),
    }
}

#[test]
fn m8_team_value_the_actor_stores_the_team_barrier_and_answers_what_it_holds() {
    let (mut actor, _, inbox) = actor();
    let owner = bot_submission().provenance.owner;
    actor.device = owner.device;
    assert!(actor.orchestrator.bot_team_value(owner).is_none());

    let never = BotTeamBarrier::never_paused(owner);
    assert!(
        matches!(send(&mut actor, &never), Response::Bot(BotReply::TeamBarrier { barrier }) if barrier == never)
    );
    // A team value may open every queued task: the queue is swept again.
    assert!(actor.bot_drain_pending);
    actor.handle(inbox.recv().unwrap());
    assert!(!actor.bot_drain_pending);

    let paused = team(owner, 5, true);
    for _ in 0..2 {
        assert!(
            matches!(send(&mut actor, &paused), Response::Bot(BotReply::TeamBarrier { barrier }) if barrier == paused),
            "the first send and its repeat answer the held value"
        );
    }
    assert_eq!(actor.orchestrator.bot_team_value(owner), Some(&paused));

    // Older: `bot_team_stale`, final, with the held epoch in its details.
    for older in [never.clone(), team(owner, 4, false)] {
        let stale = failure(send(&mut actor, &older));
        assert_eq!(stale.code, "bot_team_stale");
        assert!(!stale.retryable);
        assert_eq!(team_held_epoch(&stale), Some(5));
        assert_eq!(stale.details.len(), 1);
        assert!(stale.details.contains_key(TEAM_HELD_EPOCH));
    }
    // Same epoch, other content: `bot_team_conflict`, final, no details.
    let conflict = failure(send(&mut actor, &team(owner, 5, false)));
    assert_eq!(conflict.code, "bot_team_conflict");
    assert!(!conflict.retryable);
    assert!(conflict.details.is_empty());
    assert_eq!(actor.orchestrator.bot_team_value(owner), Some(&paused));

    // The AI owner's re-send at the held epoch plus one is accepted.
    let rewritten = team(owner, 6, false);
    assert!(
        matches!(send(&mut actor, &rewritten), Response::Bot(BotReply::TeamBarrier { barrier }) if barrier == rewritten)
    );
}

#[test]
fn m8_team_value_the_actor_refuses_another_devices_owner_and_a_malformed_value() {
    let (mut actor, _, _) = actor();
    let owner = bot_submission().provenance.owner;
    actor.device = owner.device;
    let mut foreign = BotTeamBarrier::never_paused(owner);
    foreign.owner.device = DeviceId::new();
    let refused = failure(send(&mut actor, &foreign));
    assert_eq!(refused.code, "bot_owner_mismatch");
    assert!(actor.orchestrator.bot_team_value(foreign.owner).is_none());

    let malformed = BotTeamBarrier {
        owner,
        epoch: 1,
        paused: true,
        request: None,
    };
    let refused = failure(send(&mut actor, &malformed));
    assert_eq!(refused.code, "bot_invalid");
    assert!(!refused.retryable);
    assert!(actor.orchestrator.bot_team_value(owner).is_none());

    // The refusal texts are fixed and carry nothing of the request.
    let request = BotRequestId::mint();
    let paused = BotTeamBarrier {
        owner,
        epoch: 2,
        paused: true,
        request: Some(request),
    };
    send(&mut actor, &paused);
    let stale = failure(send(&mut actor, &team(owner, 1, true)));
    let text = format!("{stale:?}");
    assert!(!text.contains(&request.to_storage()));
    assert!(!text.contains(&owner.device.to_storage()));
}

/// The delegate's own team gate reaches the AI owner with its fixed text.
#[test]
fn m8_team_value_a_paused_teams_submission_is_refused_with_the_team_code() {
    let (mut actor, _, _) = actor();
    let s = bot_submission();
    let owner = s.provenance.owner;
    actor.device = owner.device;
    send(&mut actor, &team(owner, 1, true));
    let refused = actor
        .bot_command(BotCommand::Submit {
            submission: s.clone(),
        })
        .unwrap_err();
    assert_eq!(refused.code, "bot_team_paused");
    assert!(!refused.retryable);
    assert_eq!(
        refused.message,
        "The team is paused. New work is not admitted."
    );
    assert!(
        actor
            .orchestrator
            .ports_mut()
            .store
            .bots()
            .unwrap()
            .bot_execution(s.request.run)
            .unwrap()
            .is_none(),
        "nothing was admitted"
    );
}
