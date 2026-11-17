// SPDX-License-Identifier: MPL-2.0

//! M8, P8.1: the gate every Bot admission point reads, and the team value.
//! Cohorts `m8_gate_is_the_only_reader`, `m8_team_value`, `m8_gate_team`
//! and `m8_delegate_restart_closed`. In-memory ledger and scripted sessions.
use super::*;

mod value;

fn code(error: RunError) -> String {
    match error {
        RunError::Port(error) => error.code,
        other => panic!("not a coded refusal: {other:?}"),
    }
}

fn paused_barrier(s: &BotSubmission, epoch: u64) -> BotAdmissionBarrier {
    BotAdmissionBarrier {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        request: BotRequestId::mint(),
        epoch,
        paused: true,
    }
}

/// A Reserved admission with no attempt: what a dispatch leaves behind when
/// its last boundary refuses. Reserved straight in the ledger.
fn reserved(h: &mut Harness, s: &BotSubmission) {
    h.orchestrator
        .bot_submit(s.clone(), s.provenance.owner.device)
        .unwrap();
    boot::replay(&mut h.orchestrator, s);
    assert!(matches!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .reserve_bot(s.request.run, None)
            .unwrap(),
        BotReservation::Acquired
    ));
}

/// [`reserved`] in a delegate run that was told nothing about the team.
fn reserved_untold(h: &mut Harness, s: &BotSubmission) {
    h.orchestrator
        .bot_submit(s.clone(), s.provenance.owner.device)
        .unwrap();
    boot::replay_untold(&mut h.orchestrator, s);
    assert!(matches!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .reserve_bot(s.request.run, None)
            .unwrap(),
        BotReservation::Acquired
    ));
}

fn state(h: &Harness, s: &BotSubmission) -> BotAdmissionState {
    h.schedule_store
        .bot_execution(s.request.run)
        .unwrap()
        .unwrap()
        .state
}

fn no_session_was_created(h: &Harness) -> bool {
    h.calls()
        .iter()
        .all(|call| !matches!(call, Call::Create { .. }))
}

/// `m8_gate_is_the_only_reader`: with only the Bot's own barrier paused,
/// admit, both dispatch boundaries and the attempt start answer exactly as
/// they did before the gate existed.
#[test]
fn m8_gate_is_the_only_reader_and_a_bot_barrier_alone_answers_as_before() {
    // Row 6: the ledger refuses a paused Bot's new submission.
    let mut h = bot_harness();
    let s = submission(70);
    let owner = s.provenance.owner;
    // The team half is told and open, as after the worker's `Hello`.
    crate::test_support::tell_team_never_paused(&mut h.orchestrator, owner);
    assert_eq!(
        h.orchestrator.bot_held(owner, s.provenance.bot).unwrap(),
        Held::No
    );
    h.orchestrator
        .bot_barrier(&paused_barrier(&s, 1), owner.device)
        .unwrap();
    assert_eq!(
        h.orchestrator.bot_held(owner, s.provenance.bot).unwrap(),
        Held::Bot
    );
    assert_eq!(
        code(
            h.orchestrator
                .bot_submit(s.clone(), owner.device)
                .unwrap_err()
        ),
        "bot_fenced"
    );

    // Rows 7 and 8: a queued task stays queued; a reservation is kept.
    let mut h = bot_harness();
    let queued = submission(71);
    h.orchestrator
        .bot_submit(queued.clone(), owner.device)
        .unwrap();
    let barrier = paused_barrier(&queued, 2);
    h.orchestrator.bot_barrier(&barrier, owner.device).unwrap();
    boot::replay_pair(&mut h.orchestrator, barrier, boot::policy(&queued, 2));
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&queued), owner.device)
            .unwrap()
            .is_none()
    );
    assert_eq!(state(&h, &queued), BotAdmissionState::Queued);

    let mut h = bot_harness();
    let kept = submission(72);
    reserved(&mut h, &kept);
    let barrier = paused_barrier(&kept, 2);
    h.orchestrator.bot_barrier(&barrier, owner.device).unwrap();
    boot::replay_pair(&mut h.orchestrator, barrier, boot::policy(&kept, 2));
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&kept), owner.device)
            .unwrap()
            .is_none()
    );
    assert_eq!(state(&h, &kept), BotAdmissionState::Reserved);
    // Row 9: the attempt start refuses with the existing code.
    assert_eq!(
        code(
            h.orchestrator
                .start(
                    kept.request.run,
                    StartOrigin::Bot {
                        bot: kept.provenance.bot
                    }
                )
                .unwrap_err()
        ),
        "bot_boot_pending"
    );
    assert_eq!(state(&h, &kept), BotAdmissionState::Reserved);
    assert!(no_session_was_created(&h));
}
