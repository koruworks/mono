// SPDX-License-Identifier: MPL-2.0
//! Driver, job and refusal-mapping cases over a scripted delegate port.
//! Scripted composition, not provider, actor or transport qualification;
//! no disk, Store, profile, provider, browser or telemetry is touched.
use super::jobs::Origin;
use super::*;
use crate::worker::routine_call::{RoutineCall, RoutineCallGates, refusal_channel};
use koru_delegate_api::routines::ROUTINE_BRIDGE_DEADLINE;
use koru_delegate_core::decisions::{BotDecisionWaitObservation, DecisionResume};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

type WaitRead = Result<Option<BotDecisionWaitObservation>, DecisionWaitPortError>;
type ResumeRead = Result<Option<DecisionResume>, DecisionWaitPortError>;

struct Scripted {
    wait: Mutex<WaitRead>,
    resume: Mutex<ResumeRead>,
    wait_calls: AtomicUsize,
    resume_calls: AtomicUsize,
}
impl Scripted {
    fn new(wait: WaitRead, resume: ResumeRead) -> Arc<Self> {
        Arc::new(Self {
            wait: Mutex::new(wait),
            resume: Mutex::new(resume),
            wait_calls: AtomicUsize::new(0),
            resume_calls: AtomicUsize::new(0),
        })
    }
}
impl DecisionWaitDelegate for Scripted {
    fn read_decision_wait(&self, _: &DecisionWaitSubject, within: Duration) -> WaitRead {
        assert!(within <= ROUTINE_BRIDGE_DEADLINE, "one shared deadline");
        self.wait_calls.fetch_add(1, Ordering::SeqCst);
        self.wait.lock().unwrap().clone()
    }
    fn read_decision_resume(&self, _: &DecisionWaitSubject, within: Duration) -> ResumeRead {
        assert!(within <= ROUTINE_BRIDGE_DEADLINE, "one shared deadline");
        self.resume_calls.fetch_add(1, Ordering::SeqCst);
        self.resume.lock().unwrap().clone()
    }
}

fn owner_of(key: &BotWaitEnrollmentKey) -> BotOwner {
    BotOwner {
        user_scope: key.owner.user_scope,
        device: key.owner.device,
    }
}

fn read_operation(
    gates: &RoutineCallGates,
    key: &BotWaitEnrollmentKey,
    cancellation: Cancellation,
    serial: u64,
    epoch: u64,
) -> (
    Operation,
    RoutineCall<BotWaitEnrollmentReply, BotWaitRefusal>,
) {
    let context = RoutineCallContext::new(
        Uuid::new_v4(),
        Instant::now() + ROUTINE_BRIDGE_DEADLINE,
        cancellation,
    );
    let (reply, call) =
        refusal_channel::<BotWaitEnrollmentReply, BotWaitRefusal>(context.cancellation.clone());
    let operation = Operation {
        key: Key {
            request_id: context.request_id,
            serial,
            owner_epoch: epoch,
            wait: key.clone(),
            read: true,
            phase: 1,
        },
        request: Request::Read(ReadBotWaitCleanup { key: key.clone() }),
        reply,
        context,
        _permit: gates.acquire(true).unwrap(),
    };
    (operation, call)
}

#[test]
fn decision_refusals_never_turn_unknown_into_missing() {
    // The vocabulary has no Missing at all; unknown storage is Unavailable.
    for (error, code) in [
        (BotError::Invalid, BotWaitRefusal::Invalid),
        (BotError::Forbidden, BotWaitRefusal::Forbidden),
        (BotError::Conflict, BotWaitRefusal::Conflict),
        (BotError::RevisionConflict, BotWaitRefusal::Stale),
        (BotError::NotFound, BotWaitRefusal::Stale),
        (BotError::Storage, BotWaitRefusal::Unavailable),
        (BotError::Unavailable, BotWaitRefusal::Unavailable),
    ] {
        assert_eq!(refusal(error), code);
    }
    assert_eq!(
        bridge_refusal(RoutineBridgeRefusal::Capacity),
        BotWaitRefusal::Unavailable
    );
    assert_eq!(
        bridge_refusal(RoutineBridgeRefusal::Unavailable),
        BotWaitRefusal::Unavailable
    );
}

#[test]
fn late_owner_completion_is_not_current_and_cancels_the_old_operation() {
    let fence = fixture::fence();
    let key = fixture::holding(&fence).key;
    let gates = RoutineCallGates::default();
    let mut driver = DecisionWaitDriver::new(None);
    driver.synchronize(Some(owner_of(&key)), 1);
    let cancellation = Cancellation::default();
    let (operation, _call) = read_operation(&gates, &key, cancellation.clone(), 1, 1);
    driver.active.insert(
        1,
        Active {
            key: operation.key.clone(),
            cancellation: cancellation.clone(),
        },
    );
    assert!(driver.current(&operation.key));
    // A new owner epoch: the old completion can no longer touch anything.
    driver.synchronize(Some(owner_of(&key)), 2);
    assert!(cancellation.is_cancelled());
    assert!(!driver.current(&operation.key));
    // A foreign owner never makes an operation current either.
    let mut foreign = owner_of(&key);
    foreign.device = koru_delegate_core::DeviceId::new();
    let mut driver = DecisionWaitDriver::new(None);
    driver.synchronize(Some(foreign), 1);
    driver.active.insert(
        1,
        Active {
            key: operation.key.clone(),
            cancellation: Cancellation::default(),
        },
    );
    assert!(!driver.current(&operation.key));
}

#[test]
fn deadline_or_cancel_keeps_the_physical_permit_until_actual_completion() {
    let fence = fixture::fence();
    let holding = fixture::holding(&fence);
    let gates = RoutineCallGates::default();
    let delegate = Scripted::new(Ok(Some(holding.clone())), Ok(None));
    let cancellation = Cancellation::default();
    let (operation, call) = read_operation(&gates, &holding.key, cancellation.clone(), 1, 1);
    // HTTP drop or timeout cancels the waiter, not the owned lane work.
    drop(call);
    assert!(cancellation.is_cancelled());
    let job = DecisionWaitJob::for_operation(operation, delegate.clone());
    let result = job.run();
    // A cancelled caller's read is unknown completion; no delegate call.
    assert_eq!(result.wait, Err(DecisionWaitPortError::Unavailable));
    assert_eq!(delegate.wait_calls.load(Ordering::SeqCst), 0);
    // The permit still belongs to the completion until the writer drops it.
    let mut held = Vec::new();
    while let Ok(permit) = gates.acquire(true) {
        held.push(permit);
    }
    let before = held.len();
    drop(held);
    drop(result);
    let mut after = Vec::new();
    while let Ok(permit) = gates.acquire(true) {
        after.push(permit);
    }
    assert_eq!(after.len(), before + 1);
}

#[test]
fn lost_or_mismatched_delegate_answers_are_never_an_answer() {
    let fence = fixture::fence();
    let holding = fixture::holding(&fence);
    let other = fixture::holding(&fence);
    let gates = RoutineCallGates::default();
    for (case, script, expected) in [
        (
            0,
            Err(DecisionWaitPortError::Unavailable),
            Err(DecisionWaitPortError::Unavailable),
        ),
        // Another wait's observation is contradictory, not this wait.
        (
            1,
            Ok(Some(other.clone())),
            Err(DecisionWaitPortError::Refused(BotWaitRefusal::Conflict)),
        ),
        // An authenticated "no such wait" is passed through as such.
        (2, Ok(None), Ok(None)),
        (3, Ok(Some(holding.clone())), Ok(Some(holding.clone()))),
    ] {
        let delegate = Scripted::new(script, Ok(None));
        let (operation, _call) =
            read_operation(&gates, &holding.key, Cancellation::default(), 1, 1);
        let result = DecisionWaitJob::for_operation(operation, delegate.clone()).run();
        assert_eq!(result.wait, expected, "case {case}");
        assert!(result.resume.is_none(), "case {case}");
        assert_eq!(
            delegate.resume_calls.load(Ordering::SeqCst),
            0,
            "case {case}"
        );
    }
}

#[test]
fn chat_read_reads_the_resume_row_only_for_a_held_wait() {
    let fence = fixture::fence();
    let holding = fixture::holding(&fence);
    let resume = fixture::acknowledged(&holding.key);
    let attempt = InferenceAttemptId::new();
    for held in [false, true] {
        let delegate = Scripted::new(Ok(Some(holding.clone())), Ok(Some(resume.clone())));
        let result =
            DecisionWaitJob::for_chat(attempt, holding.key.clone(), held, delegate.clone()).run();
        assert!(matches!(result.origin, Origin::Chat { .. }));
        assert_eq!(result.wait, Ok(Some(holding.clone())));
        assert_eq!(result.resume, held.then(|| Ok(Some(resume.clone()))));
        assert_eq!(
            delegate.resume_calls.load(Ordering::SeqCst),
            usize::from(held)
        );
    }
    // A resume row for another decision is contradictory.
    let mut foreign = resume.clone();
    foreign.decision = koru_delegate_core::decisions::DecisionId::mint();
    let delegate = Scripted::new(Ok(Some(holding.clone())), Ok(Some(foreign)));
    let result = DecisionWaitJob::for_chat(attempt, holding.key.clone(), true, delegate).run();
    assert_eq!(
        result.resume,
        Some(Err(DecisionWaitPortError::Refused(
            BotWaitRefusal::Conflict
        )))
    );
}
