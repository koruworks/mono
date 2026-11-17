// SPDX-License-Identifier: MPL-2.0

//! The actor's per-turn decision resume call (O123 rotation) over the real
//! fixed publication worker and its enrollment mailbox. The orchestrator side
//! is a scripted `DecisionResume` (open-wait listing and one-step driver), so
//! these prove the actor's rotation, error isolation and single-ticket gating,
//! not the driver's ledger transitions (the runtime's own tests own those).
//! No Store, provider, disk or network is involved.

use super::*;
use crate::actor::decision_resume::{DecisionResume, ResumeTurn, decision_resume_turn};
use crate::publication::{Completion, PublicationWorker};
use koru_delegate_core::BotOwner;
use koru_delegate_core::decisions::{
    BotWaitEnrollmentReply, DecisionPreparedProjection, DecisionTransitionId, EnrollBotWaitCleanup,
};
use koru_delegate_document::{RunDocument, RunDocumentError, RunProjectionStaging};
use koru_delegate_runtime::{
    AppliedRunProjection, DecisionEnrollmentBridge, DecisionResumeStep as Step, Port,
    RunProjectionIntent, RunPublicationDocument, RunPublicationOperation, RunPublicationStart,
    RunPublicationTicket,
};
use std::sync::atomic::AtomicUsize;

/// Scripted orchestrator half: lists `runs` (kept in Run order) with the
/// orchestrator's rotation, and records every Run offered to the driver.
struct Script {
    runs: Vec<(BotOwner, RunId)>,
    failing: Vec<RunId>,
    unlisted: bool,
    /// When set, a step asks the bridge to enroll this request, as the
    /// driver's HoldRequested step does.
    enroll: Option<EnrollBotWaitCleanup>,
    advanced: Vec<RunId>,
}
impl Script {
    fn new(runs: &[RunId], owner: BotOwner) -> Self {
        let mut runs: Vec<_> = runs.iter().map(|run| (owner, *run)).collect();
        runs.sort_by_key(|(_, run)| *run);
        Self {
            runs,
            failing: Vec::new(),
            unlisted: false,
            enroll: None,
            advanced: Vec::new(),
        }
    }
}
impl DecisionResume for Script {
    fn resume_runs(&mut self, after: Option<RunId>) -> Result<Vec<(BotOwner, RunId)>, RunError> {
        if self.unlisted {
            return Err(
                PortError::new(Port::Store, "bot_unavailable", "scripted listing failure").into(),
            );
        }
        // One page strictly after the cursor, then the wrapped page at or
        // before it, as `bot_decision_resume_runs` answers.
        let mut runs: Vec<_> = self
            .runs
            .iter()
            .copied()
            .filter(|(_, run)| after.is_none_or(|cursor| *run > cursor))
            .collect();
        if let Some(cursor) = after {
            runs.extend(self.runs.iter().copied().filter(|(_, run)| *run <= cursor));
        }
        Ok(runs)
    }
    fn advance(
        &mut self,
        _owner: BotOwner,
        run: RunId,
        bridge: &mut dyn DecisionEnrollmentBridge,
    ) -> Result<Step, RunError> {
        self.advanced.push(run);
        if self.failing.contains(&run) {
            return Err(PortError::new(Port::Store, "bot_invalid", "scripted refusal").into());
        }
        let Some(request) = &self.enroll else {
            return Ok(Step::OriginStopped);
        };
        match bridge.enroll(request) {
            Ok(_) => Ok(Step::Enrolled),
            // The driver maps the retryable code to Waiting.
            Err(error) if error.code == "decision_enrollment_pending" => {
                Ok(Step::Waiting("decision_enrollment_pending"))
            }
            Err(error) => Err(error.into()),
        }
    }
}

fn owner() -> BotOwner {
    bot_submission().provenance.owner
}

fn sorted_runs(count: usize) -> Vec<RunId> {
    let mut runs: Vec<_> = (0..count).map(|_| RunId::mint()).collect();
    runs.sort();
    runs
}

#[test]
fn decision_resume_tick_advances_one_run_per_turn_and_rotates() {
    let mut worker = PublicationWorker::start(|| {}).unwrap();
    let runs = sorted_runs(3);
    let mut script = Script::new(&runs, owner());
    let mut cursor = None;
    let mut offered = Vec::new();
    for turn in 0..4 {
        let outcome = decision_resume_turn(&mut script, &mut cursor, &mut worker);
        assert_eq!(script.advanced.len(), turn + 1, "exactly one Run per turn");
        let ResumeTurn::Stepped(run, Step::OriginStopped) = outcome else {
            panic!("a step: {outcome:?}")
        };
        assert_eq!(cursor, Some(run), "the cursor follows the Run offered");
        offered.push(run);
    }
    assert_eq!(
        offered,
        vec![runs[0], runs[1], runs[2], runs[0]],
        "rotation after the cursor, wrapping"
    );
    // No open wait: the turn is idle, offers nothing and keeps the cursor.
    script.runs.clear();
    assert_eq!(
        decision_resume_turn(&mut script, &mut cursor, &mut worker),
        ResumeTurn::Idle
    );
    assert_eq!(script.advanced.len(), 4);
    assert_eq!(cursor, Some(runs[0]));
    drop(worker.shutdown());
}

#[test]
fn decision_resume_tick_an_erroring_run_does_not_block_the_next() {
    let mut worker = PublicationWorker::start(|| {}).unwrap();
    let runs = sorted_runs(2);
    let mut script = Script::new(&runs, owner());
    script.failing.push(runs[0]);
    let mut cursor = None;
    assert_eq!(
        decision_resume_turn(&mut script, &mut cursor, &mut worker),
        ResumeTurn::Unchanged(runs[0]),
        "a refused step is traced, not raised"
    );
    assert_eq!(
        decision_resume_turn(&mut script, &mut cursor, &mut worker),
        ResumeTurn::Stepped(runs[1], Step::OriginStopped),
        "the next turn moves past the erroring Run"
    );
    assert_eq!(
        decision_resume_turn(&mut script, &mut cursor, &mut worker),
        ResumeTurn::Unchanged(runs[0]),
        "the erroring Run is still offered in its turn, never dropped"
    );
    // A failed listing advances nothing and keeps the cursor; the next turn
    // resumes the rotation where it stood.
    script.unlisted = true;
    assert_eq!(
        decision_resume_turn(&mut script, &mut cursor, &mut worker),
        ResumeTurn::Unavailable
    );
    assert_eq!(script.advanced, vec![runs[0], runs[1], runs[0]]);
    assert_eq!(cursor, Some(runs[0]));
    script.unlisted = false;
    assert_eq!(
        decision_resume_turn(&mut script, &mut cursor, &mut worker),
        ResumeTurn::Stepped(runs[1], Step::OriginStopped)
    );
    drop(worker.shutdown());
}

// ------------------------------------------------- the single ticket gate

struct Handle {
    document: RunDocument,
    staging: RunProjectionStaging,
}
impl RunDocumentHandle for Handle {
    fn document(&self) -> &RunDocument {
        &self.document
    }
    fn commit(
        &mut self,
        write: &mut dyn FnMut(&RunDocument) -> Result<(), RunDocumentError>,
    ) -> PortResult<()> {
        write(&self.document)?;
        Ok(())
    }
    fn prepare_decision_projection(
        &mut self,
        intent: &RunProjectionIntent,
    ) -> PortResult<DecisionPreparedProjection> {
        Ok(self.staging.prepare(&self.document, intent)?)
    }
    fn apply_prepared_projection(
        &mut self,
        prepared: &DecisionPreparedProjection,
    ) -> PortResult<AppliedRunProjection> {
        self.staging.apply(&self.document, prepared)?;
        Ok(self
            .staging
            .acknowledge(&self.document, prepared, &self.document.version())?)
    }
    fn discard_prepared_projection(&mut self, transition: DecisionTransitionId) -> PortResult<()> {
        Ok(self.staging.discard(transition)?)
    }
}

fn publication_start() -> RunPublicationStart {
    let (document, intent) = crate::documents::projection_fixture();
    RunPublicationStart {
        ticket: RunPublicationTicket {
            run: intent.identity.run,
            epoch: uuid::Uuid::new_v4(),
        },
        document: RunPublicationDocument::Attached(Box::new(Handle {
            document,
            staging: RunProjectionStaging::default(),
        })),
        operation: RunPublicationOperation::Prepare(intent),
    }
}

fn enrollment_request() -> EnrollBotWaitCleanup {
    use koru_delegate_core::decisions::*;
    fn v4<T: std::str::FromStr>() -> T
    where
        T::Err: std::fmt::Debug,
    {
        uuid::Uuid::new_v4().to_string().parse().unwrap()
    }
    let intent = BotWaitCleanupIntentV1 {
        version: DecisionVersionV1,
        owner: owner(),
        bot: v4(),
        run: v4(),
        wait_id: BotWaitId::mint(),
        decision_id: DecisionId::mint(),
        decision_revision: DecisionRevision::new(2).unwrap(),
        wait_revision: DecisionRevision::new(3).unwrap(),
        origin_attempt: v4(),
        origin_generation: koru_delegate_core::AttemptGeneration::from_raw(1).unwrap(),
        origin_session: v4(),
        origin_turn: v4(),
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
    };
    EnrollBotWaitCleanup {
        key: intent.key().unwrap(),
        cleanup_intent: intent,
    }
}

/// The host's enrollment client stand-in on the worker thread: answers the
/// exact Enrolled reply and counts physical calls.
struct Client(Arc<AtomicUsize>);
impl DecisionEnrollmentBridge for Client {
    fn enroll(&mut self, request: &EnrollBotWaitCleanup) -> PortResult<BotWaitEnrollmentReply> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(BotWaitEnrollmentReply::Enrolled {
            receipt_digest: request.key.receipt_digest,
            key: request.key.clone(),
            ai_attempt: request.key.wait_id.as_uuid(),
        })
    }
}

fn woken(wake: &mpsc::Receiver<()>) {
    wake.recv_timeout(Duration::from_secs(5))
        .expect("bounded worker wake");
}

#[test]
fn decision_resume_tick_starts_no_enrollment_while_a_document_handle_is_out() {
    let (send, wake) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut worker = PublicationWorker::start_with_enrollment(
        move || {
            let _ = send.send(());
        },
        Box::new(Client(calls.clone())),
    )
    .unwrap();
    let runs = sorted_runs(1);
    let mut script = Script::new(&runs, owner());
    script.enroll = Some(enrollment_request());
    let mut cursor = None;

    // A Run's handle is out (Prepared retains it on the worker).
    let start = publication_start();
    let ticket = start.ticket;
    assert!(worker.begin(start).is_ok());
    woken(&wake);
    let Some(Completion::Prepared { .. }) = worker.completion() else {
        panic!("prepared")
    };
    for _ in 0..2 {
        assert_eq!(
            decision_resume_turn(&mut script, &mut cursor, &mut worker),
            ResumeTurn::Unchanged(runs[0]),
            "enrollment pending is a normal wait"
        );
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no enrollment while the handle is out"
    );

    // The handle comes back: the next turn starts the job on the worker, and
    // a later turn claims its exact reply.
    worker.return_handle(ticket).unwrap();
    woken(&wake);
    let Some(Completion::Returned { handle, .. }) = worker.completion() else {
        panic!("returned")
    };
    drop(handle);
    assert_eq!(
        decision_resume_turn(&mut script, &mut cursor, &mut worker),
        ResumeTurn::Unchanged(runs[0])
    );
    woken(&wake);
    assert_eq!(
        decision_resume_turn(&mut script, &mut cursor, &mut worker),
        ResumeTurn::Stepped(runs[0], Step::Enrolled)
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "one physical enrollment call"
    );
    assert!(worker.shutdown().is_empty());
}

#[test]
fn decision_resume_without_a_client_answers_unavailable_and_keeps_rotating() {
    let (send, wake) = mpsc::channel();
    let mut worker = PublicationWorker::start(move || {
        let _ = send.send(());
    })
    .unwrap();
    let runs = sorted_runs(1);
    let mut script = Script::new(&runs, owner());
    script.enroll = Some(enrollment_request());
    let mut cursor = None;
    assert_eq!(
        decision_resume_turn(&mut script, &mut cursor, &mut worker),
        ResumeTurn::Unchanged(runs[0])
    );
    woken(&wake);
    // `decision_enrollment_unavailable` is not retryable for the driver: it
    // is refused, traced, and the Run is offered again on a later turn.
    assert_eq!(
        decision_resume_turn(&mut script, &mut cursor, &mut worker),
        ResumeTurn::Unchanged(runs[0])
    );
    assert_eq!(script.advanced.len(), 2);
    assert!(worker.shutdown().is_empty());
}

#[test]
fn an_idle_resume_turn_releases_the_maintenance_tick_and_a_bot_command_rearms_it() {
    let (mut actor, _, _) = actor();
    assert!(
        actor.decision_resume_pending,
        "a restart may find open waits"
    );
    assert!(actor.publications_pending());
    assert_eq!(
        actor.advance_decision_resume(),
        ResumeTurn::Idle,
        "no open wait, no step"
    );
    assert!(!actor.decision_resume_pending);
    // M6: a restart may find open child waits too; an idle child turn
    // releases its half of the deadline the same way.
    assert!(actor.child_resume_pending);
    assert_eq!(
        actor.advance_child_resume(),
        crate::actor::child_resume::ChildTurn::Idle
    );
    assert!(!actor.child_resume_pending);
    assert!(
        !actor.publications_pending(),
        "an idle actor keeps no maintenance deadline"
    );
    let (reply, _answer) = mpsc::sync_channel(1);
    actor.handle(Command::Bot {
        command: koru_delegate_api::bots::BotCommand::ReadBotDecisionWait {
            owner: owner(),
            bot: koru_delegate_core::BotId::from_uuid(uuid::Uuid::new_v4()),
            run: RunId::mint(),
            wait_id: koru_delegate_core::decisions::BotWaitId::mint(),
            decision_id: koru_delegate_core::decisions::DecisionId::mint(),
        },
        reply,
    });
    assert!(
        actor.decision_resume_pending,
        "a decision command may have opened a wait"
    );
    assert!(
        actor.child_resume_pending,
        "a Bot command may have opened a child wait"
    );
}
