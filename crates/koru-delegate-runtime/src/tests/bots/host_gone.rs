// SPDX-License-Identifier: MPL-2.0

//! A Bot Run whose process the session host no longer has (window run 2,
//! item 1): the process exited and was reaped, or the host restarted and
//! took its process trees with it. Every step goes through the real
//! orchestrator and Bot ledger; the fake host answers what
//! `koru-delegate-host`'s `HostSessions` answers over the real session host:
//! `terminate` of a session the registry no longer holds is
//! `session_not_found` (`SessionRegistry::get` reaps before it looks), and
//! `retire_creation` still says what ran (`CreationLedger::lookup`:
//! `KnownEnded` in the same incarnation, `NoEvidence` in a new one).

use super::*;

fn unsettled(h: &mut Harness, s: &BotSubmission) -> Vec<RunId> {
    h.orchestrator
        .bot_unsettled(
            s.provenance.owner,
            s.provenance.bot,
            64,
            s.provenance.owner.device,
        )
        .unwrap()
        .admissions
        .into_iter()
        .map(|admission| admission.admission.run)
        .collect()
}

fn state(h: &mut Harness, s: &BotSubmission) -> BotAdmissionState {
    h.orchestrator
        .bot_reconcile(&submission_key(s), s.provenance.owner.device)
        .unwrap()
        .unwrap()
        .state
}

fn settled(h: &mut Harness, s: &BotSubmission) {
    assert!(h.attempt(s.request.run).retired, "the attempt is retired");
    assert_eq!(state(h, s), BotAdmissionState::Settled);
    assert!(unsettled(h, s).is_empty(), "nothing is left to settle");
    assert!(
        h.orchestrator
            .claims()
            .holders_of(&koru_delegate_scope::ScopeIdentity::fixture_canonical(
                Path::new(&format!(
                    "C:/fixture/bot-{}",
                    s.request.run.as_uuid().as_bytes()[15]
                ))
            ))
            .is_empty(),
        "the folder claim is released"
    );
}

/// The person's Stop of a running Run, first unconfirmed because the
/// terminate it sent has not ended the process yet (`bot_retire_stopped`).
fn stopped_unconfirmed(h: &mut Harness, s: &BotSubmission) -> BotExecutionFence {
    let fence = prepared(h, s);
    observe(h, &fence);
    running_session(
        h,
        s,
        CreationLookup::Known {
            session: summary(ManagedState::Running, None),
        },
    );
    let first = h
        .orchestrator
        .bot_stop(&key(s), fence.owner.device, Some(&fence));
    assert!(
        matches!(&first, Err(RunError::Port(error)) if error.code == "bot_stop_unconfirmed"),
        "{first:?}"
    );
    assert_eq!(h.status(s.request.run), RunStatus::Cancelled);
    fence
}

/// The process then exits, as the terminate makes it, and the host reaps
/// it. No exit reached the orchestrator (its event pump had ended), which is
/// what the window run's fixture turn did.
fn exited_and_reaped(h: &mut Harness, s: &BotSubmission, fence: &BotExecutionFence) {
    let mut ended = summary(ManagedState::Failed, None);
    ended.process = ProcessState::Exited { code: Some(1) };
    running_session(h, s, CreationLookup::KnownEnded { session: ended });
    h.sessions.lock().unwrap().reaped.insert(fence.session);
}

/// The lifecycle's Stop (and `stop_task`'s) sent again after the process
/// ended and was reaped retires the Run. Before, the terminate's
/// `session_not_found` failed the proof before the host was asked what ran,
/// so every Stop answered that error, the Run stayed `Executing` in
/// `UnsettledWork`, and an Archive or Trash parked `work_running` after its
/// twelve rounds and parked again on every Continue.
#[test]
fn a_stop_sent_again_after_the_host_reaped_the_session_retires_the_run() {
    let mut h = bot_harness();
    let s = submission(90);
    let fence = stopped_unconfirmed(&mut h, &s);
    exited_and_reaped(&mut h, &s, &fence);
    // What the AI owner is told while it waits: still executing, with the
    // fence a Stop needs, and nobody holding it.
    let work = h
        .orchestrator
        .bot_unsettled(
            s.provenance.owner,
            s.provenance.bot,
            64,
            s.provenance.owner.device,
        )
        .unwrap();
    assert_eq!(work.admissions.len(), 1);
    assert!(!work.admissions[0].held);
    assert_eq!(work.admissions[0].admission.fence.as_ref(), Some(&fence));
    assert_eq!(state(&mut h, &s), BotAdmissionState::Executing);

    h.orchestrator
        .bot_stop(&key(&s), fence.owner.device, Some(&fence))
        .unwrap();
    assert_eq!(h.status(s.request.run), RunStatus::Cancelled);
    settled(&mut h, &s);
}

/// The actor's cleanup tick, which retries a Cancelled Run's proof on its own
/// (`bot_cleanup_tick`, driven by `bot_budget_tick`), retires it too.
#[test]
fn the_cleanup_tick_retires_a_stopped_run_whose_session_the_host_reaped() {
    let mut h = bot_harness();
    let s = submission(91);
    let fence = stopped_unconfirmed(&mut h, &s);
    exited_and_reaped(&mut h, &s, &fence);
    assert!(h.orchestrator.bot_budget_tick(s.request.run).unwrap());
    assert_eq!(h.status(s.request.run), RunStatus::Cancelled);
    settled(&mut h, &s);
}

/// The delegate worker killed after the Stop, the session host still the
/// same incarnation: the restart classification proves the end from the
/// host's `KnownEnded` and retires the Run. Before, every restart logged it
/// `unresolved` with its claim kept (`claims_kept=1`), for good.
#[test]
fn a_worker_restart_retires_a_stopped_run_whose_session_the_host_reaped() {
    let mut h = bot_harness();
    let s = submission(92);
    let fence = stopped_unconfirmed(&mut h, &s);
    exited_and_reaped(&mut h, &s, &fence);
    let mut ended = summary(ManagedState::Failed, None);
    ended.process = ProcessState::Exited { code: Some(1) };
    let mut h = restarted(h, (host(1), CreationLookup::KnownEnded { session: ended }));
    exited_and_reaped(&mut h, &s, &fence);
    let report = h.orchestrator.restart().unwrap();
    assert!(report.reconciled.contains(&s.request.run), "{report:?}");
    assert!(report.unresolved.is_empty(), "{report:?}");
    assert!(report.claims.kept.is_empty());
    assert_eq!(h.status(s.request.run), RunStatus::Cancelled);
    settled(&mut h, &s);
}

/// The whole Agent restarted after the Stop: the session host is a new
/// incarnation, whose answer for the old key is `NoEvidence`. ADR-341: no
/// evidence from a new incarnation means nothing survived (the host's
/// process trees are in its kill-on-close jobs). The Run is retired as the
/// person's Stop recorded it.
#[test]
fn a_session_host_restart_retires_a_stopped_run() {
    let mut h = bot_harness();
    let s = submission(93);
    let fence = stopped_unconfirmed(&mut h, &s);
    let mut h = restarted(h, (host(2), CreationLookup::NoEvidence));
    {
        let mut script = h.sessions.lock().unwrap();
        script.host = host(2);
        script.reaped.insert(fence.session);
    }
    running_session_on(&mut h, &s, host(2), CreationLookup::NoEvidence);
    let report = h.orchestrator.restart().unwrap();
    assert!(report.reconciled.contains(&s.request.run), "{report:?}");
    assert!(report.claims.kept.is_empty());
    assert_eq!(h.status(s.request.run), RunStatus::Cancelled);
    settled(&mut h, &s);
}

/// A Run that was running when the whole Agent restarted, never stopped:
/// the new host incarnation has no evidence, so the Run is the automatic
/// interruption with its prior attempt ended (§7.6's table for every Run),
/// retired, and nothing is left for an Archive or Trash to wait on. A later
/// Stop with the old fence is answered, and changes nothing.
#[test]
fn a_run_running_when_its_session_host_restarted_is_interrupted_and_retired() {
    let mut h = bot_harness();
    let s = submission(94);
    let fence = prepared(&mut h, &s);
    observe(&mut h, &fence);
    let mut h = restarted(h, (host(2), CreationLookup::NoEvidence));
    {
        let mut script = h.sessions.lock().unwrap();
        script.host = host(2);
        script.reaped.insert(fence.session);
    }
    running_session_on(&mut h, &s, host(2), CreationLookup::NoEvidence);
    let report = h.orchestrator.restart().unwrap();
    assert!(report.interrupted.contains(&s.request.run), "{report:?}");
    assert!(report.claims.kept.is_empty(), "{report:?}");
    let interrupted = RunStatus::Interrupted {
        prior_attempt: PriorAttempt::Ended,
    };
    assert_eq!(h.status(s.request.run), interrupted);
    settled(&mut h, &s);
    h.orchestrator
        .bot_stop(&key(&s), fence.owner.device, Some(&fence))
        .unwrap();
    assert_eq!(h.status(s.request.run), interrupted);
    assert!(
        h.calls()
            .iter()
            .all(|call| !matches!(call, Call::Create { .. } | Call::Submit { .. })),
        "nothing was launched again"
    );
}

/// The process is still running when the host is asked: a Stop stays
/// unconfirmed, and the Run keeps its slot and claim. The proof is the
/// host's answer, never the terminate's.
#[test]
fn a_process_the_host_still_runs_keeps_the_stop_unconfirmed() {
    let mut h = bot_harness();
    let s = submission(95);
    let fence = stopped_unconfirmed(&mut h, &s);
    h.sessions.lock().unwrap().reaped.insert(fence.session);
    let again = h
        .orchestrator
        .bot_stop(&key(&s), fence.owner.device, Some(&fence));
    assert!(
        matches!(&again, Err(RunError::Port(error)) if error.code == "bot_stop_unconfirmed"),
        "{again:?}"
    );
    assert!(!h.attempt(s.request.run).retired);
    assert_eq!(state(&mut h, &s), BotAdmissionState::Executing);
    assert_eq!(unsettled(&mut h, &s), vec![s.request.run]);
}

fn running_session_on(
    h: &mut Harness,
    s: &BotSubmission,
    host: HostInstanceId,
    lookup: CreationLookup,
) {
    let native = h
        .attempt(s.request.run)
        .provider_session
        .unwrap()
        .idempotency;
    h.sessions
        .lock()
        .unwrap()
        .lookups
        .insert(native, (host, lookup));
}
