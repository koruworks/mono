// SPDX-License-Identifier: MPL-2.0
//! M8 piece W, cohort `m8_routine_controls_refused` (row 13 of the team
//! pause's admission inventory), and the lifecycle row of `m8_team_races`.
//! The real worker over an in-memory ai.db; the coordinator is the scripted
//! peer, so "nothing was sent" is read from what it received.
use super::*;

fn team(h: &H, paused: bool, expected: u64) -> TeamPauseView {
    let request_id = BotRequestId::mint();
    let command = if paused {
        BotCommand::PauseTeam {
            request_id,
            expected_team_revision: expected,
        }
    } else {
        BotCommand::ResumeTeam {
            request_id,
            expected_team_revision: expected,
        }
    };
    match h.send(command).unwrap() {
        BotResponse::TeamPause { team } => team,
        _ => panic!("team_pause"),
    }
}

/// Turning a Routine on and Test are new work: under a team pause they
/// answer `team_paused` and nothing reaches the coordinator. Pausing and
/// editing a Routine pass. After the team resumes, Enable is admitted.
#[test]
fn m8_routine_controls_refused_enable_and_test_answer_team_paused_and_send_nothing() {
    let h = H::new();
    let view = team(&h, true, 0);
    assert_eq!((view.revision, view.paused), (1, true));

    let refused = h.enable(1).unwrap_err();
    assert_eq!(
        (refused.code.as_str(), refused.retryable),
        (koru_ai_wire::bot_team_codes::TEAM_PAUSED, false)
    );
    let refused = h.test(1).unwrap_err();
    assert_eq!(refused.code, koru_ai_wire::bot_team_codes::TEAM_PAUSED);
    assert!(h.owner.named("enable").is_empty() && h.owner.named("test").is_empty());
    assert!(h.owner.tests.lock().unwrap().is_empty());
    // The Routine is as it was: paused, at its first revision.
    let live = h.owner.live();
    assert_eq!((live.revision, live.paused), (1, true));

    // Pause only reduces work: it reaches the coordinator (which answers
    // that the Routine is already paused).
    assert_eq!(code(h.pause(1)), codes::STATE_CONFLICT);
    assert_eq!(h.owner.named("pause"), ["pause@1"]);
    // Edit passes and commits.
    let edited = h
        .control(h.edit_command(1, "Routine", "Evening", ZONED))
        .unwrap();
    assert_eq!(edited.outcome, RoutineControlOutcome::Applied);
    assert_eq!(edited.definition_revision, 2);

    // The team resumes: Enable is admitted again.
    let view = team(&h, false, 1);
    assert_eq!((view.revision, view.paused), (2, false));
    let enabled = h.enable(2).unwrap();
    assert_eq!(enabled.outcome, RoutineControlOutcome::Applied);
    assert!(!h.owner.live().paused);
}

/// Paused both ways, the refusal names the Bot's own pause first; the team
/// code appears only when the team alone holds the Bot.
#[test]
fn m8_routine_controls_refused_names_the_bots_own_pause_first() {
    let h = H::with(Options {
        bot_paused: true,
        ..Options::default()
    });
    team(&h, true, 0);
    let refused = h.enable(1).unwrap_err();
    assert_eq!(refused.code, codes::NOT_READY);
    assert_eq!(reasons(&refused), ["bot_paused"]);
    let refused = h.test(1).unwrap_err();
    assert_eq!(reasons(&refused), ["bot_paused"]);
    assert!(h.owner.named("enable").is_empty() && h.owner.named("test").is_empty());
}

/// `m8_team_races`: a Bot is archived while the team is paused, as today.
/// The team pause refuses new work only; it holds no lifecycle command.
#[test]
fn m8_team_races_a_bot_is_archived_while_the_team_is_paused_as_today() {
    let h = H::new();
    team(&h, true, 0);
    let response = h
        .send(BotCommand::Archive {
            request_id: BotRequestId::mint(),
            bot_id: h.bot(),
            expected_revision: h.f.bot.revision,
        })
        .unwrap();
    assert!(matches!(response, BotResponse::Lifecycle { .. }));
    until("the Bot is archived", || {
        matches!(
            h.send(BotCommand::Read { bot_id: h.bot() }),
            Ok(BotResponse::Bot { bot }) if bot.lifecycle == BotLifecycle::Archived
        )
    });
}
