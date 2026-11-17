// SPDX-License-Identifier: MPL-2.0

//! Cohort `m8_events_content_free`: `bots.team_pause.changed`, with the
//! real worker over an in-memory ai.db and the scripted delegate.

use super::*;
use crate::worker::bots::events::recorder::RecordedBotEvent;

/// `(epoch, paused, stage)` of every row, checked to hold the four listed
/// attributes and nothing else.
fn rows(f: &Fixture) -> Vec<(u64, bool, &'static str)> {
    let events = f.owner.1.events();
    assert_holds_none_of(&events, &content(f));
    events.iter().map(|event| team_row(f, event)).collect()
}

fn team_row(f: &Fixture, event: &RecordedBotEvent) -> (u64, bool, &'static str) {
    assert_eq!(event.name, "bots.team_pause.changed");
    assert_eq!(event.actor, "ai-worker");
    assert_eq!(event.keys(), ["epoch", "owner_device", "paused", "stage"]);
    assert_eq!(
        event.attr("owner_device"),
        Some(f.team.device.to_string().as_str())
    );
    (
        event.attr("epoch").unwrap().parse().unwrap(),
        event.attr("paused").unwrap().parse().unwrap(),
        match event.attr("stage").unwrap() {
            "pending" => "pending",
            "acknowledged" => "acknowledged",
            other => panic!("stage {other}"),
        },
    )
}

/// What the fixture's Bot and its owner are called and where they live:
/// none of it may be in a row.
fn content(f: &Fixture) -> Vec<&str> {
    vec![
        f.bot.config.name.as_str(),
        f.bot.config.working_folder.as_str(),
        "team-fixture",
        "internal-debug",
    ]
}

/// Nothing more is published for a while.
fn settled(f: &Fixture, count: usize) {
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(f.owner.1.count(), count, "{:?}", f.owner.1.events());
}

/// One row when the team row moves and one when that epoch is confirmed. A
/// repeat answered from the row, a team already in the asked state, a wrong
/// revision, a read, and a worker or delegate that starts again and is told
/// the same value publish nothing.
#[test]
fn m8_events_content_free_team_pause_is_one_row_per_stage_and_none_on_a_repeat() {
    let owner = Owner::new();
    let f = start(owner.clone(), |_, _| {});
    // The worker starts and tells the delegate "never paused": no row moved.
    until("the delegate was told", || f.owner.team_sends() >= 1);
    assert_eq!(f.view().revision, 0);
    settled(&f, 0);

    // Pause.
    let request_id = BotRequestId::mint();
    let pause = BotCommand::PauseTeam {
        request_id,
        expected_team_revision: 0,
    };
    f.command(pause.clone()).unwrap();
    f.acknowledged(1, true);
    until("both rows", || f.owner.1.count() == 2);
    assert_eq!(rows(&f), [(1, true, "pending"), (1, true, "acknowledged")]);

    // The same request again is answered from the row.
    f.command(pause.clone()).unwrap();
    // Another request for the state the team is already in writes nothing.
    assert_eq!(f.change(true, 1).unwrap().revision, 1);
    // A wrong expected revision is refused.
    assert!(f.change(false, 7).is_err());
    // A read changes nothing.
    assert_eq!(f.view().revision, 1);
    settled(&f, 2);

    // Resume: the next epoch, its two rows.
    f.change(false, 1).unwrap();
    f.acknowledged(2, false);
    until("four rows", || f.owner.1.count() == 4);
    assert_eq!(
        rows(&f)[2..],
        [(2, false, "pending"), (2, false, "acknowledged")]
    );
    // The first request once more, long after: its row moved on, and it is
    // refused or answered without a write. Either way, no row.
    let _ = f.command(pause);
    settled(&f, 4);

    // The delegate restarts and is told the same value again: an echo of an
    // epoch that is already acknowledged publishes nothing.
    owner.restart();
    f.on_writer(|state| state.request_bot_boot_refresh());
    until("the new delegate run was told", || {
        f.owner.held().is_some_and(|held| held.epoch == 2)
    });
    settled(&f, 4);

    // The worker restarts over the same ai.db and tells the delegate again.
    let (team, authority, bot) = (f.team, f.authority.clone(), f.bot.clone());
    let repository = f.stop();
    owner.with(|run| run.seen.clear());
    let (worker, returned) = spawn(repository, authority.clone(), owner.clone());
    let f = Fixture {
        worker,
        owner: owner.clone(),
        bot,
        team,
        authority,
        returned,
    };
    until("the restarted worker told the delegate", || {
        f.owner.team_sends() >= 1
    });
    f.acknowledged(2, false);
    settled(&f, 4);
    assert_eq!(rows(&f).len(), 4);
}

/// While the delegate is away the change is published as `pending` once,
/// however many times it is sent; `acknowledged` follows when the delegate
/// confirms it.
#[test]
fn m8_events_content_free_team_pause_is_acknowledged_once_when_the_delegate_returns() {
    let owner = Owner::new();
    let f = start(owner.clone(), |_, _| {});
    until("the delegate was told", || f.owner.team_sends() >= 1);
    owner.with(|run| run.away = true);
    let sends = f.owner.team_sends();
    f.change(true, 0).unwrap();
    // Sent, and sent again, and never answered.
    until("the value was sent twice more", || {
        f.owner.team_sends() >= sends + 2
    });
    assert_eq!(rows(&f), [(1, true, "pending")]);
    owner.with(|run| run.away = false);
    f.acknowledged(1, true);
    until("the acknowledgement was published", || {
        f.owner.1.count() == 2
    });
    settled(&f, 2);
    assert_eq!(rows(&f), [(1, true, "pending"), (1, true, "acknowledged")]);
}

/// A row the worker writes itself, past an epoch the delegate holds, is a
/// move of the team row like any other: one `pending`, one `acknowledged`.
#[test]
fn m8_events_content_free_team_pause_a_row_rewritten_past_the_delegates_epoch_is_published() {
    // No row here; the delegate holds epoch 5, paused.
    let owner = Owner::new();
    let f = start(owner.clone(), |_, team| {
        owner.with(|run| {
            run.team = Some(BotTeamBarrier {
                owner: delegate_owner(team),
                epoch: 5,
                paused: true,
                request: Some(BotRequestId::mint()),
            })
        });
    });
    f.acknowledged(6, false);
    until("both rows", || f.owner.1.count() == 2);
    settled(&f, 2);
    assert_eq!(
        rows(&f),
        [(6, false, "pending"), (6, false, "acknowledged")]
    );
}
