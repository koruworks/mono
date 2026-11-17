// SPDX-License-Identifier: MPL-2.0

//! M8 piece D, cohorts `m8_ui_team_pause` and `m8_i18n_team`: the model of
//! the roster's *Pause all Bots* control, fed plain values. No Store, no
//! service, no thread, no window.

use super::*;
use koru_ai_core::bots::DeviceId;
use koru_i18n::{Lang, t};

fn view(revision: u64, paused: bool, stage: TeamPauseStage) -> TeamPauseView {
    TeamPauseView {
        owner_device: DeviceId::new(),
        paused,
        revision,
        stage,
        pending_since_ms: (stage == TeamPauseStage::Pending).then_some(1),
    }
}

/// A model that read `view` once, as a shown roster does.
fn read(view: TeamPauseView) -> TeamPause {
    let mut team = TeamPause::default();
    let seq = team.reading();
    assert!(team.read(seq, Ok(view)));
    team
}

fn says(team: &TeamPause) -> (Option<&'static str>, Option<bool>, Option<&'static str>) {
    (
        team.control().map(|control| t(Lang::En, control.words)),
        team.control().map(|control| control.enabled),
        team.line().map(|line| t(Lang::En, line)),
    )
}

/// Every state of the contract's 6.1 table that this build can reach, in
/// its words: the control, whether it can be pressed, the line under it.
#[test]
fn m8_ui_team_pause_every_state_says_its_control_and_its_line() {
    use TeamPauseStage::{Acknowledged, Pending};
    // Nothing read yet: no control is drawn, and nothing is said.
    let unknown = TeamPause::default();
    assert_eq!(unknown.state(), TeamState::Unknown);
    assert_eq!(says(&unknown), (None, None, None));
    assert!(!unknown.holds_bots());

    for (shown, state, control, enabled, line, rows) in [
        (
            view(0, false, Acknowledged),
            TeamState::NotPaused,
            "Pause all Bots",
            true,
            None,
            false,
        ),
        (
            view(1, true, Pending),
            TeamState::Pausing,
            "Pausing…",
            false,
            Some("Waiting for Koru on this computer to confirm."),
            false,
        ),
        (
            view(1, true, Acknowledged),
            TeamState::Paused,
            "Resume all Bots",
            true,
            Some("All Bots are paused. Nothing new will start."),
            true,
        ),
        (
            view(2, false, Pending),
            TeamState::Resuming,
            "Resuming…",
            false,
            Some("Waiting for Koru on this computer to confirm."),
            true,
        ),
        (
            view(2, false, Acknowledged),
            TeamState::NotPaused,
            "Pause all Bots",
            true,
            None,
            false,
        ),
    ] {
        let team = read(shown);
        assert_eq!(team.state(), state);
        assert_eq!(
            says(&team),
            (Some(control), Some(enabled), line),
            "{state:?}"
        );
        assert_eq!(team.holds_bots(), rows, "{state:?}: the Bot rows");
        // Only *Pause all Bots* asks first.
        assert_eq!(team.press_asks(), control == "Pause all Bots");
    }
}

/// **Never an unconfirmed state.** A press says *Pausing…*, disabled. The
/// change's own answer (`pending`) keeps it there, and so does every read
/// until the computer's Koru has confirmed. It is read again on a bounded
/// clock for as long as that takes.
#[test]
fn m8_ui_team_pause_shows_pending_until_the_pause_is_confirmed() {
    use TeamPauseStage::{Acknowledged, Pending};
    let mut team = read(view(0, false, Acknowledged));
    assert!(team.next_read_for_test().is_none(), "nothing to wait for");
    // A resume is not offered while nothing is paused.
    assert_eq!(team.begin(false), None);
    let change = team.begin(true).expect("a pause is offered");
    assert_eq!((change.paused, change.expected), (true, 0));
    team.sent(change.clone());
    assert_eq!(team.state(), TeamState::Pausing);
    assert_eq!(team.control().map(|control| control.enabled), Some(false));
    // No second change while one is on its way.
    assert_eq!(team.begin(true), None);
    assert_eq!(team.begin(false), None);
    assert!(team.awaiting());

    // The owner committed the row and has not been confirmed.
    assert!(team.changed(change.request, Ok(view(1, true, Pending))));
    assert_eq!(team.state(), TeamState::Pausing);
    assert!(
        !team.holds_bots(),
        "the rows do not say paused before it is confirmed"
    );
    let first = team.next_read_for_test().expect("it is read again");
    for _ in 0..3 {
        let seq = team.reading();
        assert!(team.read(seq, Ok(view(1, true, Pending))));
        assert_eq!(team.state(), TeamState::Pausing);
        assert!(team.next_read_for_test().is_some(), "and again");
    }
    assert!(team.next_read_for_test().unwrap() > first);
    // Confirmed.
    let seq = team.reading();
    assert!(team.read(seq, Ok(view(1, true, Acknowledged))));
    assert_eq!(team.state(), TeamState::Paused);
    assert!(team.holds_bots());
    assert!(team.next_read_for_test().is_none());
    assert!(!team.awaiting());

    // Resuming is the same the other way, at the revision read.
    let change = team.begin(false).expect("a resume is offered");
    assert_eq!((change.paused, change.expected), (false, 1));
    team.sent(change.clone());
    assert_eq!(team.state(), TeamState::Resuming);
    assert!(
        team.holds_bots(),
        "the rows stay paused until the resume is confirmed"
    );
    assert!(team.changed(change.request, Ok(view(2, false, Acknowledged))));
    assert_eq!(team.state(), TeamState::NotPaused);
    assert!(!team.holds_bots());
}

/// A change whose answer never arrived stays *Pausing…*: it may have been
/// made. The next read says which. A read that was asked before the change
/// was sent says nothing about it.
#[test]
fn m8_ui_team_pause_a_lost_answer_is_settled_by_a_read_never_by_time() {
    use TeamPauseStage::{Acknowledged, Pending};
    for landed in [true, false] {
        let mut team = read(view(0, false, Acknowledged));
        // A read is already on its way when the control is pressed.
        let early = team.reading();
        let change = team.begin(true).unwrap();
        team.sent(change.clone());
        assert!(team.read(early, Ok(view(0, false, Acknowledged))));
        assert_eq!(
            team.state(),
            TeamState::Pausing,
            "the early read settles nothing"
        );
        // No answer says what happened to the change.
        assert!(team.changed(change.request, Err(Failure::Unanswered)));
        assert_eq!(team.state(), TeamState::Pausing);
        assert!(team.read_due(Instant::now()), "a read is owed at once");
        let seq = team.reading();
        if landed {
            assert!(team.read(seq, Ok(view(1, true, Pending))));
            assert_eq!(team.state(), TeamState::Pausing);
            assert_eq!(team.line(), Some(Words::BotsTeamWaitingConfirm));
        } else {
            assert!(team.read(seq, Ok(view(0, false, Acknowledged))));
            assert_eq!(team.state(), TeamState::NotPaused);
            assert_eq!(
                team.line().map(|line| t(Lang::En, line)),
                Some("Couldn't change this just now. Try again.")
            );
            // The next press clears the line and tries again.
            assert!(team.begin(true).is_some());
            assert_eq!(team.line(), None);
        }
    }
}

/// Another window changed the team first: nothing failed, the state is read
/// again. Any other refusal says the change was not made. An answer about
/// another request, or an older read, changes nothing.
#[test]
fn m8_ui_team_pause_refusals_and_stale_answers() {
    use TeamPauseStage::Acknowledged;
    let mut team = read(view(0, false, Acknowledged));
    let change = team.begin(true).unwrap();
    team.sent(change.clone());
    assert!(!team.changed(BotRequestId::mint(), Ok(view(1, true, Acknowledged))));
    assert_eq!(team.state(), TeamState::Pausing);
    assert!(team.changed(
        change.request,
        Err(Failure::Refused(REVISION_CONFLICT.to_owned()))
    ));
    assert_eq!(team.state(), TeamState::NotPaused);
    assert_eq!(team.line(), None, "nothing failed");
    assert!(team.read_due(Instant::now()));
    // The other window paused it.
    let seq = team.reading();
    assert!(!team.read(seq.wrapping_sub(1), Ok(view(9, false, Acknowledged))));
    assert!(team.read(seq, Ok(view(1, true, Acknowledged))));
    assert_eq!(team.state(), TeamState::Paused);

    let change = team.begin(false).unwrap();
    team.sent(change.clone());
    assert!(team.changed(
        change.request,
        Err(Failure::Refused("storage_error".to_owned()))
    ));
    assert_eq!(team.state(), TeamState::Paused, "as it was");
    assert_eq!(team.line(), Some(Words::BotsTeamChangeFailed));
    // A change that could not even be queued says the same.
    let mut team = read(view(0, false, Acknowledged));
    assert!(team.begin(true).is_some());
    team.not_sent();
    assert_eq!(team.state(), TeamState::NotPaused);
    assert_eq!(team.line(), Some(Words::BotsTeamChangeFailed));
    // A failed read keeps what is shown and is asked again.
    let seq = team.reading();
    assert!(team.read(seq, Err(Failure::Unanswered)));
    assert_eq!(team.state(), TeamState::NotPaused);
    assert!(team.next_read_for_test().is_some());
    // An older revision never replaces a newer one.
    let mut team = read(view(3, true, Acknowledged));
    let seq = team.reading();
    assert!(team.read(seq, Ok(view(2, false, Acknowledged))));
    assert_eq!(team.state(), TeamState::Paused);
}

/// The question asked before a pause: its title, its two sentences, its
/// answers left to right, and the keyboard's order through them. Cancel is
/// where the keyboard starts and what Enter answers until an answer is
/// chosen; Tab moves to *Pause all Bots* and back.
#[test]
fn m8_ui_team_pause_the_question_and_its_tab_order() {
    use koru_ui::confirm::Outcome;
    use winit::keyboard::{Key, NamedKey};
    let mut dialog = question(Lang::En);
    assert_eq!(dialog.title(), "Pause all Bots?");
    assert_eq!(
        dialog.body(),
        "No Bot will start new work until you resume: no replies, no Routines, no requests between Bots.\n\nWork that is running now keeps going."
    );
    let answers: Vec<_> = crate::input_engine::with(|engine| {
        dialog
            .answers(engine, (1200.0, 900.0))
            .into_iter()
            .map(|(outcome, label, _)| (outcome, label))
            .collect()
    });
    assert_eq!(
        answers,
        [
            (Outcome::Cancel, "Cancel".to_owned()),
            (Outcome::Confirm, "Pause all Bots".to_owned()),
        ]
    );
    // An Enter in flight cancels; nothing is paused by accident.
    assert_eq!(dialog.default_action(), Some(Outcome::Cancel));
    assert_eq!(dialog.key(&Key::Named(NamedKey::Enter)), Outcome::Cancel);
    // Tab: Cancel, Pause all Bots, Cancel.
    assert_eq!(dialog.key(&Key::Named(NamedKey::Tab)), Outcome::Keep);
    assert_eq!(dialog.focused(), Some(Outcome::Confirm));
    assert_eq!(dialog.key(&Key::Named(NamedKey::Enter)), Outcome::Confirm);
    assert_eq!(dialog.key(&Key::Named(NamedKey::Tab)), Outcome::Keep);
    assert_eq!(dialog.focused(), Some(Outcome::Cancel));
    assert_eq!(dialog.key(&Key::Named(NamedKey::Escape)), Outcome::Cancel);
}

/// The refusals a person meets while every Bot is paused, in sentences: a
/// message to a Bot that is not working, and turning a Routine on or
/// testing it.
#[test]
fn m8_ui_team_pause_refusals_are_said_in_sentences() {
    let send = crate::bots::view::BOT_REFUSALS
        .iter()
        .find(|(code, _)| *code == TEAM_PAUSED)
        .map(|(_, words)| t(Lang::En, *words));
    assert_eq!(send, Some("All Bots are paused. Resume them to send this."));
    assert_eq!(
        t(
            Lang::En,
            crate::bots::routines::control::refusal_words(TEAM_PAUSED)
        ),
        "All Bots are paused."
    );
    assert_eq!(TEAM_PAUSED, koru_ai_core::bots::BotError::TeamPaused.code());
    assert_eq!(
        REVISION_CONFLICT,
        koru_ai_core::bots::BotError::TeamRevisionConflict.code()
    );
}

/// Cohort `m8_i18n_team`: every word of the team pause is in all eight
/// languages, with the English placeholders, and no other language is
/// English copied.
#[test]
fn m8_i18n_team_every_word_is_in_all_eight_languages() {
    let keys = [
        Words::BotsTeamPauseAll,
        Words::BotsTeamResumeAll,
        Words::BotsTeamPausing,
        Words::BotsTeamResuming,
        Words::BotsTeamWaitingConfirm,
        Words::BotsTeamPausedLine,
        Words::BotsTeamPauseTitle,
        Words::BotsTeamPauseBodyNoNew,
        Words::BotsTeamPauseBodyRunning,
        Words::BotsPausedWithTeam,
        Words::BotsRowPausedWithTeam,
        Words::BotsTeamPausedSend,
        Words::BotsTeamPausedShort,
        Words::BotsTeamChangeFailed,
    ];
    let langs = [
        Lang::En,
        Lang::Es,
        Lang::PtBr,
        Lang::De,
        Lang::Fr,
        Lang::Ar,
        Lang::ZhHans,
        Lang::Ja,
    ];
    for key in keys {
        let english = t(Lang::En, key);
        assert!(!english.is_empty());
        let holes = english.matches("{}").count();
        for lang in langs {
            let words = t(lang, key);
            assert!(!words.trim().is_empty(), "{key:?} in {lang:?}");
            assert_eq!(words.matches("{}").count(), holes, "{key:?} in {lang:?}");
            if lang != Lang::En {
                assert_ne!(words, english, "{key:?} in {lang:?} is English");
            }
        }
    }
    // The control's accessible names are the contract's.
    assert_eq!(t(Lang::En, Words::BotsTeamPauseAll), "Pause all Bots");
    assert_eq!(t(Lang::En, Words::BotsTeamResumeAll), "Resume all Bots");
    assert_eq!(
        koru_i18n::fill(t(Lang::En, Words::BotsRowPausedWithTeam), &["Scout"]),
        "Scout, paused with the team"
    );
}
