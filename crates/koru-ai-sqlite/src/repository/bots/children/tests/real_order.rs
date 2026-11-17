// SPDX-License-Identifier: MPL-2.0
//! Cohort `m6_enrollment_bind`: a child wait's enrollment in the order a real
//! Run takes. The delegate answers `Submit` while the Run is queued, so the
//! receipt names no execution; the session and then the turn are bound later.
//! The enrollment still requires the receipt's execution to be the asking
//! turn's, so the bind records it there, once, and never a different one.
use super::*;
use crate::repository::bot_replay::tests::{answer_submit, receipt_execution, submitted};

/// [`parent`] in the order a real Run takes. Nothing is bound yet; `fence`
/// is the execution the delegate goes on to name, with its turn.
fn queued() -> Parent {
    let (repo, turn, submission, fence) = submitted();
    let p = &submission.provenance;
    let owner = BotOwner {
        user_scope: p.owner.user_scope,
        device: p.owner.device,
    };
    let bot = repo.bot(p.bot).unwrap().unwrap();
    Parent {
        repo,
        owner,
        bot,
        turn,
        submission,
        fence,
    }
}

/// The execution with no turn, as `Reconcile` names it before `BeginTurn`.
fn session_of(fence: &BotExecutionFence) -> BotExecutionFence {
    let mut session = fence.clone();
    session.turn = None;
    session
}

/// `Phase::Admission`, then `Phase::Begin`.
fn bind(parent: &mut Parent) {
    let (attempt, fence) = (parent.attempt(), parent.fence.clone());
    parent
        .repo
        .bind_bot_execution(attempt, &session_of(&fence), at(4))
        .unwrap();
    parent
        .repo
        .bind_bot_execution(attempt, &fence, at(5))
        .unwrap();
}

fn running() -> Parent {
    let mut parent = queued();
    bind(&mut parent);
    parent
}

fn enroll(
    parent: &mut Parent,
    origin: &DecisionFence,
) -> Result<BotChildWaitEnrollmentKey, BotError> {
    let intent = BotChildWaitCleanupIntentV1::frozen(BotWaitId::mint(), origin).unwrap();
    let key = intent.key().unwrap();
    parent
        .repo
        .enroll_bot_child_wait(
            parent.owner,
            &EnrollBotChildWaitCleanup {
                key: key.clone(),
                cleanup_intent: intent,
            },
            at(6),
        )
        .map(|_| key)
}

#[test]
fn m6_enrollment_bind_a_child_wait_enrolls_in_the_order_a_real_run_takes() {
    let mut parent = queued();
    // `Submit` was answered: the receipt names no execution.
    assert_eq!(receipt_execution(&parent.repo, &parent.submission), None);
    let (attempt, fence) = (parent.attempt(), parent.fence.clone());
    // The session is bound: the receipt names it, with no turn.
    parent
        .repo
        .bind_bot_execution(attempt, &session_of(&fence), at(4))
        .unwrap();
    assert_eq!(
        receipt_execution(&parent.repo, &parent.submission),
        Some(session_of(&fence))
    );
    // The turn is bound: the receipt names the execution with its turn.
    parent
        .repo
        .bind_bot_execution(attempt, &fence, at(5))
        .unwrap();
    assert_eq!(
        receipt_execution(&parent.repo, &parent.submission),
        Some(fence)
    );
    let origin = parent.origin();
    enroll(&mut parent, &origin).unwrap();
    assert_eq!(count(&parent.repo, "bot_child_wait_enrollments"), 1);
}

#[test]
fn m6_enrollment_bind_another_run_another_generation_and_a_stale_turn_are_still_refused() {
    let mut parent = running();
    let origin = parent.origin();
    let mut generation = origin;
    generation.generation = origin.generation.next();
    let mut turn = origin;
    turn.turn = d::AgentTurnId::mint();
    let mut session = origin;
    session.session = d::SessionId::mint();
    let mut attempt = origin;
    attempt.attempt = d::RunAttemptId::mint();
    for other in [generation, turn, session, attempt] {
        assert_eq!(enroll(&mut parent, &other), Err(BotError::Conflict));
    }
    // An execution of a Run this Bot does not have.
    let mut run = origin;
    run.run = d::RunId::mint();
    assert!(enroll(&mut parent, &run).is_err());
    // Another Run's reply, bound and running: its execution is not this
    // Run's, whichever of the two the request names.
    let other = running();
    assert!(enroll(&mut parent, &other.origin()).is_err());
    assert_eq!(count(&parent.repo, "bot_child_wait_enrollments"), 0);
    // A turn that is not bound yet does not enroll either.
    let mut early = queued();
    let (attempt, fence) = (early.attempt(), early.fence.clone());
    let origin = early.origin();
    assert!(enroll(&mut early, &origin).is_err());
    early
        .repo
        .bind_bot_execution(attempt, &session_of(&fence), at(4))
        .unwrap();
    assert_eq!(enroll(&mut early, &origin), Err(BotError::Conflict));
    assert_eq!(count(&early.repo, "bot_child_wait_enrollments"), 0);
}

#[test]
fn m6_enrollment_bind_a_second_different_execution_is_refused_and_never_recorded() {
    let mut parent = queued();
    let (attempt, fence) = (parent.attempt(), parent.fence.clone());
    let first = session_of(&fence);
    parent
        .repo
        .bind_bot_execution(attempt, &first, at(4))
        .unwrap();
    // The same generation under another session, another attempt, a later
    // generation, and another Run's execution: each bind is refused and the
    // receipt keeps the one execution that was bound.
    let mut session = first.clone();
    session.session = d::SessionId::mint();
    let mut other_attempt = first.clone();
    other_attempt.attempt = d::RunAttemptId::mint();
    let mut later = first.clone();
    later.generation = first.generation.next();
    let mut foreign = first.clone();
    foreign.run = d::RunId::mint();
    for other in [session, other_attempt, later, foreign] {
        assert!(
            parent
                .repo
                .bind_bot_execution(attempt, &other, at(5))
                .is_err()
        );
        assert_eq!(
            receipt_execution(&parent.repo, &parent.submission),
            Some(first.clone())
        );
    }
    // The turn, once; a different turn afterwards is refused.
    parent
        .repo
        .bind_bot_execution(attempt, &fence, at(6))
        .unwrap();
    let mut stale = fence.clone();
    stale.turn = Some(d::AgentTurnId::mint());
    assert!(
        parent
            .repo
            .bind_bot_execution(attempt, &stale, at(7))
            .is_err()
    );
    assert!(
        parent
            .repo
            .bind_bot_execution(attempt, &first, at(7))
            .is_err(),
        "a bound turn is never taken back"
    );
    assert_eq!(
        receipt_execution(&parent.repo, &parent.submission),
        Some(fence)
    );
}

#[test]
fn m6_enrollment_bind_a_receipt_that_names_another_execution_is_kept_and_refused() {
    // The delegate's answer to `Submit` already named an execution, and it
    // is not the one bound later. The bind does not replace it, and the
    // enrollment refuses as it always did.
    let (mut repo, turn, submission, fence) = submitted();
    let mut answered = fence.clone();
    answered.session = d::SessionId::mint();
    answered.turn = Some(d::AgentTurnId::mint());
    answer_submit(&mut repo, &submission, Some(answered.clone()));
    let p = &submission.provenance;
    let owner = BotOwner {
        user_scope: p.owner.user_scope,
        device: p.owner.device,
    };
    let bot = repo.bot(p.bot).unwrap().unwrap();
    let mut parent = Parent {
        repo,
        owner,
        bot,
        turn,
        submission,
        fence,
    };
    bind(&mut parent);
    assert_eq!(
        receipt_execution(&parent.repo, &parent.submission),
        Some(answered)
    );
    let origin = parent.origin();
    assert_eq!(enroll(&mut parent, &origin), Err(BotError::Conflict));
}

#[test]
fn m6_enrollment_bind_a_restart_and_a_resumed_run_still_enroll() {
    // The binds are repeated (a worker that reads the admission again): the
    // receipt is unchanged and the wait enrolls.
    let mut parent = running();
    bind_again(&mut parent);
    let origin = parent.origin();
    let first = enroll(&mut parent, &origin).unwrap();
    // `Submit`'s answer is stored after the execution was bound (the two
    // replies raced): the receipt still names the bound execution.
    let (mut repo, _, submission, fence) = crate::repository::bot_replay::tests::prepared();
    answer_submit(&mut repo, &submission, None);
    assert_eq!(receipt_execution(&repo, &submission), Some(fence));
    // The Run is resumed after its wait: the replay rolls over to the
    // successor. The receipt keeps the admission's own execution; binding
    // the successor again changes nothing there; and the successor's own
    // wait enrolls because it is the installed consumer.
    let resume = acknowledged(first.wait_id, origin);
    let rolled = parent
        .repo
        .rollover_bot_child_replay(
            parent.owner,
            &first,
            &resume,
            &fence_of(&origin),
            None,
            at(7),
        )
        .unwrap();
    let consumer = rolled.consumer_fence.clone();
    assert_ne!(consumer, parent.fence);
    let attempt = parent.attempt();
    parent
        .repo
        .bind_bot_execution(attempt, &consumer, at(8))
        .unwrap();
    assert_eq!(
        receipt_execution(&parent.repo, &parent.submission),
        Some(parent.fence.clone()),
        "the receipt keeps the admission's own execution"
    );
    enroll(&mut parent, &as_origin(&consumer)).unwrap();
    // The first execution is no longer the Run's current one.
    assert_eq!(enroll(&mut parent, &origin), Err(BotError::Conflict));
    assert_eq!(count(&parent.repo, "bot_child_wait_enrollments"), 2);
}

fn bind_again(parent: &mut Parent) {
    let (attempt, fence) = (parent.attempt(), parent.fence.clone());
    parent
        .repo
        .bind_bot_execution(attempt, &fence, at(9))
        .unwrap();
    assert_eq!(
        receipt_execution(&parent.repo, &parent.submission),
        Some(fence)
    );
}
