// SPDX-License-Identifier: MPL-2.0

//! Piece R's runtime cohorts (M6, contract 10.3): one Bot Run asks other
//! Bots, is held, waits, and is resumed by a successor, through the public
//! child-chain builder over the real in-memory ledger, the scripted session
//! host and a fake clock. Scripted runtime composition, not provider
//! qualification. No disk, Store, provider, browser or network.
//!
//! Every test name starts with its cohort.

use super::*;
use crate::ChildResumeStep as Step;
use crate::test_support::{
    ChildChain, ChildEvents, FIXTURE_NOW_MS, ScriptedChildEnrollment, child_chain, publish_owed,
    recipient_bot, routine_child_chain,
};
use koru_delegate_core::children::{
    ChildClock, ChildRefusal, ChildRequest, ChildRequestOutcome, ChildRequestState, ChildRequested,
    ChildSettledBy,
};
use koru_delegate_core::decisions::BotWaitPhase;
use koru_delegate_core::{AgentTurnId, BotAdmissionState, BotRunKey, RunStatus, SessionId};
use std::sync::atomic::Ordering;

mod cancel;
mod clock;
mod delivery;
mod denial;
mod restart;
mod routine;
mod team_pause;

/// The parent's device.
fn device(chain: &ChildChain) -> koru_delegate_core::DeviceId {
    chain.parent.owner.device
}

/// Send `request` as the AI worker would, under the parent's boot.
fn send(chain: &mut ChildChain, request: &ChildRequest) -> ChildRequestOutcome {
    let device = device(chain);
    let boot = chain.parent.boot;
    chain
        .orchestrator
        .bot_request_child(request, device, boot)
        .expect("the ask is answered")
}

/// Ask the recipients `ns` and expect it to be admitted.
fn asked(chain: &mut ChildChain, ns: &[u8]) -> (ChildRequest, ChildRequested) {
    let request = chain.parent.ask(&mut chain.orchestrator, ns);
    match send(chain, &request) {
        ChildRequestOutcome::Created(requested) => (request, requested),
        other => panic!("the ask was not admitted: {other:?}"),
    }
}

fn advance(chain: &mut ChildChain, bridge: &mut ScriptedChildEnrollment) -> Step {
    let (owner, run) = (chain.parent.owner, chain.parent.run);
    chain
        .orchestrator
        .bot_advance_child_resume(owner, run, bridge)
        .expect("the child driver answers")
}

fn execution(chain: &mut ChildChain) -> Option<ChildWaitExecution> {
    let (owner, run) = (chain.parent.owner, chain.parent.run);
    chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .child_wait_execution(owner, run)
        .unwrap()
}

fn requests(chain: &mut ChildChain) -> Vec<ChildRequestReceipt> {
    let (owner, run) = (chain.parent.owner, chain.parent.run);
    let now = chain.clock.load(Ordering::SeqCst);
    chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .read_child_requests(owner, run, now)
        .unwrap()
        .requests
}

/// A person's Stop of one child Run (queued: no fence).
fn stop_child(chain: &mut ChildChain, receipt: &ChildRequestReceipt) {
    try_stop_child(chain, receipt).unwrap();
}

fn try_stop_child(chain: &mut ChildChain, receipt: &ChildRequestReceipt) -> Result<(), RunError> {
    let key = BotRunKey {
        owner: chain.parent.owner,
        bot: receipt.recipient,
        run: receipt.child_run,
    };
    let device = device(chain);
    chain.orchestrator.bot_stop(&key, device, None)
}

/// The parent's admission as the ledger holds it.
fn parent_state(chain: &mut ChildChain) -> BotAdmissionState {
    let run = chain.parent.run;
    chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .bot_execution(run)
        .unwrap()
        .unwrap()
        .state
}

fn child_state(chain: &mut ChildChain, run: RunId) -> BotAdmissionState {
    chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .bot_execution(run)
        .unwrap()
        .unwrap()
        .state
}

/// Ask `ns`, publish the waiting marker, enroll and stop the origin: the
/// parent is held and its requests are open.
fn held(ns: &[u8]) -> (ChildChain, ChildRequested, ScriptedChildEnrollment) {
    held_in(child_chain(), ns)
}

fn held_in(
    mut chain: ChildChain,
    ns: &[u8],
) -> (ChildChain, ChildRequested, ScriptedChildEnrollment) {
    let successor = SessionId::mint();
    let turn = AgentTurnId::mint();
    chain
        .parent
        .script_resume(&mut chain.sessions.lock().unwrap(), successor, turn);
    let (_, requested) = asked(&mut chain, ns);
    let run = chain.parent.run;
    publish_owed(&mut chain.orchestrator, run).expect("publish the waiting marker");
    let mut bridge = chain.parent.enrollment_bridge();
    assert_eq!(advance(&mut chain, &mut bridge), Step::Enrolled);
    assert_eq!(advance(&mut chain, &mut bridge), Step::OriginStopped);
    assert_eq!(
        advance(&mut chain, &mut bridge),
        Step::Waiting("bot_child_requests_open")
    );
    (chain, requested, bridge)
}

/// From a held parent whose requests have all settled: reserve, bind,
/// install and feed the successor, then publish its Resume Control and
/// settle the wait.
fn resume(chain: &mut ChildChain, bridge: &mut ScriptedChildEnrollment) {
    for expected in [
        Step::SuccessorReserved,
        Step::SuccessorBound,
        Step::SuccessorInstalled,
        Step::InputAcknowledged,
    ] {
        assert_eq!(advance(chain, bridge), expected);
    }
    let run = chain.parent.run;
    publish_owed(&mut chain.orchestrator, run).expect("publish the Resume Control");
    chain
        .orchestrator
        .settle_publication_wait(run)
        .expect("the resumed wait settles");
}

// ------------------------------------------------------------ m6_child_actor

/// The thirteen steps of 4.1 at the runtime: ask, hold, enroll, stop, the
/// child ends, the successor is reserved, launched, installed and fed, the
/// Resume Control clears the marker and the wait settles. The parent's host
/// slot is free while it waits (its origin attempt is retired) and its Bot
/// admission is still Executing.
#[test]
fn m6_child_actor_runtime_steps_ask_hold_wait_resume() {
    let mut chain = child_chain();
    let events = ChildEvents::install(&mut chain.orchestrator);
    let run = chain.parent.run;
    let successor = SessionId::mint();
    let turn = AgentTurnId::mint();
    chain
        .parent
        .script_resume(&mut chain.sessions.lock().unwrap(), successor, turn);

    // 1-3: the ask is one transaction; the wait is born hold_requested and
    // the Run classifies as waiting before the caller learns of it.
    let (request, requested) = asked(&mut chain, &[1]);
    assert_eq!(requested.wait.phase, BotWaitPhase::HoldRequested);
    assert_eq!(requested.receipts.len(), 1);
    let receipt = requested.receipts[0].clone();
    assert_eq!(receipt.state, ChildRequestState::Open);
    assert_eq!(receipt.recipient, recipient_bot(1));
    assert_eq!(receipt.child, BotAdmissionState::Queued);
    let mark = chain.child_waits.get(run).expect("the wait is published");
    assert_eq!(mark.wait, requested.wait.wait);
    assert_eq!(mark.open_requests, 1);
    // One `requested` event for the commit.
    let published = events.published();
    assert_eq!(published.len(), 1);
    assert!(published[0].1.requested);
    // The same ask again replays; nothing new is written or published.
    match send(&mut chain, &request) {
        ChildRequestOutcome::AlreadyCreated(again) => {
            assert_eq!(again.receipts, requested.receipts);
            assert_eq!(again.enrollment, requested.enrollment);
        }
        other => panic!("not a replay: {other:?}"),
    }
    assert_eq!(events.published().len(), 1);

    // 4: the ChildWaiting Control.
    assert_eq!(publish_owed(&mut chain.orchestrator, run).unwrap(), 1);

    // 5-6: enrollment, then the origin's proven stop.
    let mut bridge = chain.parent.enrollment_bridge();
    assert_eq!(advance(&mut chain, &mut bridge), Step::Enrolled);
    assert_eq!(bridge.requests(), 1);
    assert_eq!(advance(&mut chain, &mut bridge), Step::OriginStopped);
    let held = execution(&mut chain).unwrap();
    assert_eq!(held.wait.phase, BotWaitPhase::Retired);
    assert!(held.current_attempt.retired, "the host slot is free");
    assert_eq!(parent_state(&mut chain), BotAdmissionState::Executing);
    assert_eq!(
        advance(&mut chain, &mut bridge),
        Step::Waiting("bot_child_requests_open")
    );

    // 7-8: the child ends (a person stops it while queued).
    stop_child(&mut chain, &receipt);
    let settled = requests(&mut chain);
    assert_eq!(settled[0].state, ChildRequestState::Settled);
    assert_eq!(settled[0].settled_by, Some(ChildSettledBy::ChildEnded));

    // 9-13: the successor, its continuation, the Resume Control, settle.
    resume(&mut chain, &mut bridge);
    assert!(execution(&mut chain).is_none(), "the wait settled");
    assert!(chain.child_waits.get(run).is_none(), "the fence is lifted");
    {
        let script = chain.sessions.lock().unwrap();
        assert!(script.attached(successor));
    }
    let header = chain.orchestrator.header(run).unwrap();
    assert!(matches!(header.status, RunStatus::Running { .. }));
    // The settlement the child's own ending wrote is published once by the
    // parent's wait owner.
    let changed: Vec<_> = events
        .published()
        .into_iter()
        .filter(|(_, fact)| !fact.requested)
        .collect();
    assert!(
        changed
            .iter()
            .any(|(_, fact)| fact.settled_by == Some(ChildSettledBy::ChildEnded))
    );
    let ids: std::collections::BTreeSet<_> = events.published().iter().map(|(id, _)| *id).collect();
    assert_eq!(
        ids.len(),
        events.published().len(),
        "one event per revision"
    );
}

// -------------------------------------------------------- m6_child_saturation

/// Two waiting parents on a two-slot host are not modelled by one chain; the
/// shape that matters is that a held parent frees its host slot and a
/// queued child can be dispatched beside it. A parent whose stop is
/// unproven keeps its slot and its requests still settle by their clock.
#[test]
fn m6_child_saturation_unproven_stop_keeps_the_slot_and_requests_still_time_out() {
    let mut chain = child_chain();
    // The origin's creation never ends on the host: its stop is unproven.
    let origin = chain.parent.origin_creation;
    chain
        .sessions
        .lock()
        .unwrap()
        .script_retirement(origin, koru_session_protocol::CreationLookup::Creating);
    let (_, requested) = asked(&mut chain, &[1]);
    let run = chain.parent.run;
    publish_owed(&mut chain.orchestrator, run).unwrap();
    let mut bridge = chain.parent.enrollment_bridge();
    assert_eq!(advance(&mut chain, &mut bridge), Step::Enrolled);
    assert_eq!(
        advance(&mut chain, &mut bridge),
        Step::Waiting("bot_origin_stop_unproven"),
        "an unproven stop records nothing"
    );
    let still = execution(&mut chain).unwrap();
    assert_eq!(still.wait.phase, BotWaitPhase::HoldRequested);
    assert!(!still.current_attempt.retired, "the slot stays occupied");
    // The request's clock still runs out.
    clock::count(&mut chain, BOT_CHILD_WAIT);
    let after = requests(&mut chain);
    assert_eq!(after[0].child_request, requested.receipts[0].child_request);
    assert_eq!(after[0].settled_by, Some(ChildSettledBy::TimedOut));
    // With nothing open and the origin never retired, the wait is cleaned
    // up instead of resumed.
    assert_eq!(
        advance(&mut chain, &mut bridge),
        Step::CleanupRequested("bot_origin_stop_unproven")
    );
}

/// Counted milliseconds of the child budget.
const BOT_CHILD_WAIT: u64 = koru_delegate_core::children::BOT_CHILD_WAIT_MS;

// --------------------------------------------------- m6_child_folder_overlap

/// An ask to a Bot whose folder overlaps the parent's is refused at once,
/// with the item's ordinal, and nothing is written.
#[test]
fn m6_child_folder_overlap_is_refused_at_once_with_its_ordinal() {
    let mut chain = child_chain();
    let parent_scope = chain.parent.submission.request.scope.clone();
    let request = chain
        .parent
        .ask_with(&mut chain.orchestrator, &[1, 2], |submission| {
            if submission.provenance.bot == recipient_bot(2) {
                submission.request.scope = parent_scope.clone();
            }
        });
    match send(&mut chain, &request) {
        ChildRequestOutcome::Refused(refused) => {
            assert_eq!(refused.code, ChildRefusal::FolderOverlap);
            assert_eq!(refused.code.code(), "bot_child_folder_overlap");
            assert_eq!(refused.ordinal, Some(1));
        }
        other => panic!("not refused: {other:?}"),
    }
    assert!(requests(&mut chain).is_empty());
    assert!(execution(&mut chain).is_none());
    assert!(chain.child_waits.get(chain.parent.run).is_none());
}

/// While the retained scopes are not recovered the overlap cannot be
/// decided: the ask is refused with the retryable `scope_recovery_pending`
/// (the gateway's `child_status_unknown`), never as an overlap, and nothing
/// is admitted.
#[test]
fn m6_child_folder_overlap_scope_busy_is_retryable_not_overlap() {
    let mut chain = child_chain();
    let request = chain.parent.ask(&mut chain.orchestrator, &[1]);
    chain.orchestrator.scope_recovery_incomplete = true;
    let device = device(&chain);
    let boot = chain.parent.boot;
    let error = chain
        .orchestrator
        .bot_request_child(&request, device, boot)
        .unwrap_err();
    assert!(
        matches!(&error, RunError::Port(port) if port.code == "scope_recovery_pending"),
        "{error:?}"
    );
    chain.orchestrator.scope_recovery_incomplete = false;
    assert!(requests(&mut chain).is_empty());
    // Once recovered, the same ask is admitted.
    assert!(matches!(
        send(&mut chain, &request),
        ChildRequestOutcome::Created(_)
    ));
}
