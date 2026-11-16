// SPDX-License-Identifier: MPL-2.0

//! Piece B cohorts of M6 over the real in-memory ledger: `m6_child_admission`,
//! `m6_child_cycle`, `m6_child_cycle_atomic`, `m6_child_occupancy`,
//! `m6_child_clock_rows`, `m6_child_settle`, `m6_child_slot_rule`,
//! `m6_child_resume_rows`. Real admission, attempt, policy and publication
//! owners; a fake clock; no Store, provider, Profile or file.
use super::*;
use crate::{
    AttemptPhase, BOT_ACTIVE_BUDGET_MS, BotExecutionCapacity, BotLedger, BotReservation,
    BotSettlement, CrashPoint, Moment, RunsDb, WriteOp, fixtures,
};
use koru_delegate_core::children::*;
use koru_delegate_core::decisions::*;
use koru_delegate_core::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

struct World {
    db: Option<RunsDb>,
    clock: Arc<AtomicI64>,
    boot: DelegateBootId,
    policies: Vec<(RunId, BotPolicyBarrier)>,
}
impl World {
    fn new() -> Self {
        let clock = Arc::new(AtomicI64::new(1000));
        Self {
            db: Some(RunsDb::in_memory(Box::new(fixtures::FakeClock(
                clock.clone(),
            )))),
            clock,
            boot: DelegateBootId::mint(),
            policies: Vec::new(),
        }
    }
    fn db(&mut self) -> &mut RunsDb {
        self.db.as_mut().unwrap()
    }
    fn read(&self) -> &RunsDb {
        self.db.as_ref().unwrap()
    }
    fn now(&self) -> i64 {
        self.clock.load(Ordering::SeqCst)
    }
    fn tick(&self, by_ms: i64) -> i64 {
        self.clock.fetch_add(by_ms, Ordering::SeqCst) + by_ms
    }
    /// A new Bot with one executing Run on an acknowledged turn. Takes one of
    /// the two host slots.
    fn parent(&mut self) -> BotExecutionFence {
        let (db, fence, policy) =
            crate::native_results::tests::decision_base_in(self.db.take().unwrap(), RunId::mint());
        self.db = Some(db);
        self.policies.push((fence.run, policy));
        fence
    }
    /// The effective policy barrier the fixture installed for this parent.
    fn policy(&self, parent: &BotExecutionFence) -> BotPolicyBarrier {
        self.policies
            .iter()
            .find(|(run, _)| *run == parent.run)
            .expect("a fixture parent")
            .1
            .clone()
    }
    fn admitted(&self, run: RunId) -> crate::BotAdmissionRecord {
        self.read().bot_admission(run).unwrap().unwrap()
    }
    fn base(&self, run: RunId) -> DecisionPublicationBase {
        let admitted = self.admitted(run);
        let intent = self.read().intent(run).unwrap().unwrap();
        let mut header = RunHeader::from_request(admitted.submission.request, 777);
        header.bot = Some(admitted.submission.provenance.clone());
        // A later wait of the same Run starts from the document owner's
        // durable baseline: the exact last applied marker.
        if let Some(applied) = self
            .read()
            .latest_applied_decision_projection(admitted.submission.provenance.owner, run)
            .unwrap()
        {
            let timeline = applied.target.timeline_end().unwrap();
            return DecisionPublicationBase {
                workspace: intent.workspace,
                document: intent.run_document.unwrap(),
                source_schema: 5u8.try_into().unwrap(),
                observed_created_at_ms: 777,
                timeline_len: timeline,
                subject_digest: decision_projection_subject_digest(&header).unwrap(),
                predecessor: DecisionProjectionPredecessor {
                    observed_created_at_ms: 777,
                    timeline_start: timeline,
                    projection: Some(DecisionProjectionPrior {
                        transition: applied.identity.transition,
                        sequence: applied.identity.sequence,
                        target_digest: applied.identity.target_digest,
                    }),
                    status: applied.target.effects().status.clone(),
                    failure_reason: applied.target.effects().failure_reason,
                },
                bookkeeping: None,
            };
        }
        DecisionPublicationBase {
            workspace: intent.workspace,
            document: intent.run_document.unwrap(),
            source_schema: 5u8.try_into().unwrap(),
            observed_created_at_ms: 777,
            timeline_len: 0,
            subject_digest: decision_projection_subject_digest(&header).unwrap(),
            predecessor: DecisionProjectionPredecessor {
                observed_created_at_ms: 777,
                timeline_start: 0,
                projection: None,
                status: RunStatus::running(),
                failure_reason: None,
            },
            bookkeeping: None,
        }
    }
    /// One ask of `parent` to `recipients`, built as the AI owner builds it.
    fn ask(&self, parent: &BotExecutionFence, recipients: &[BotId]) -> ChildRequest {
        let admitted = self.admitted(parent.run);
        let subject = &admitted.submission;
        let ActorRef::Person { profile: human } = subject.request.requested_by else {
            panic!("fixture person")
        };
        let wait = BotWaitId::mint();
        let ask_key = uuid::Uuid::new_v4();
        let parent_policy = ChildPolicyRef::of(
            subject.provenance.config_revision,
            subject.provenance.policy_revision,
            &subject.policy,
        )
        .unwrap();
        let requests = recipients
            .iter()
            .enumerate()
            .map(|(ordinal, recipient)| {
                let child_request = ChildRequestId::mint();
                let child_run = RunId::mint();
                let mut submission = subject.clone();
                submission.provenance = BotProvenance {
                    parent: Some(ChildParent {
                        run: parent.run,
                        bot: parent.bot,
                        chat_id: subject.provenance.chat_id,
                        child_request,
                    }),
                    owner: parent.owner,
                    bot: *recipient,
                    submission: BotRequestId::from_uuid(child_request.as_uuid()),
                    chat_id: uuid::Uuid::new_v4(),
                    ai_attempt_id: None,
                    assistant_message_id: None,
                    config_revision: 1,
                    policy_revision: 1,
                    continued_from: None,
                    routine: None,
                };
                submission.request.run = child_run;
                submission.request.origin = RunOrigin::Manual;
                submission.input.idempotency_key = IdempotencyKey::mint();
                let binding = ChildRequestBindingV1 {
                    version: DecisionVersionV1,
                    owner: parent.owner,
                    parent_bot: parent.bot,
                    parent_run: parent.run,
                    wait,
                    ask_key,
                    ordinal: ordinal as u8,
                    parent_submission: u32::from(admitted.submission_version),
                    parent_routine: subject.provenance.routine,
                    human,
                    recipient_bot: *recipient,
                    child_run,
                    child_request,
                    workspace: subject.workspace,
                    task_digest: child_task_digest(submission.request.objective.as_str()).unwrap(),
                    documents: vec![],
                    parent_policy,
                    child_policy: ChildPolicyRef::of(1, 1, &submission.policy).unwrap(),
                };
                ChildRequestItem {
                    child_request,
                    binding,
                    submission,
                }
            })
            .collect();
        ChildRequest {
            parent: parent.clone(),
            ask_key,
            human,
            requests,
        }
    }
    fn send(&mut self, request: &ChildRequest) -> ChildRequestOutcome {
        let base = self.base(request.parent.run);
        let boot = self.boot;
        self.db().request_child(request, boot, &base).unwrap()
    }
    /// Ask and require admission.
    fn admit(&mut self, parent: &BotExecutionFence, recipients: &[BotId]) -> ChildRequested {
        let request = self.ask(parent, recipients);
        match self.send(&request) {
            ChildRequestOutcome::Created(created) => created,
            other => panic!("expected an admitted ask, got {other:?}"),
        }
    }
    fn refused(&mut self, request: &ChildRequest) -> ChildRefused {
        match self.send(request) {
            ChildRequestOutcome::Refused(refused) => refused,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    fn execution(&self, parent: &BotExecutionFence) -> ChildWaitExecution {
        self.read()
            .child_wait_execution(parent.owner, parent.run)
            .unwrap()
            .expect("an open child wait")
    }
    fn ticket(&self, parent: &BotExecutionFence) -> DecisionWaitExpected {
        self.execution(parent).expected
    }
    fn enroll(&mut self, parent: &BotExecutionFence) -> crate::ChildWaitRecord {
        let execution = self.execution(parent);
        let key = execution.wait.intent.key().unwrap();
        let ai_attempt = self
            .admitted(parent.run)
            .submission
            .provenance
            .ai_attempt_id
            .unwrap();
        self.db()
            .record_child_enrollment(
                &execution.expected,
                &BotChildWaitEnrollmentReply::Enrolled {
                    receipt_digest: key.receipt_digest,
                    key,
                    ai_attempt,
                },
            )
            .unwrap()
    }
    /// End and retire the origin attempt, as the physical stop would, and
    /// build the release proof from the retained attempt.
    fn release(&mut self, parent: &BotExecutionFence) -> OriginWaitStop {
        let now = self.now();
        let execution = self.execution(parent);
        let origin = execution.wait.origin;
        let mut attempt = self.read().current_attempt(parent.run).unwrap().unwrap();
        if !attempt.retired {
            self.db()
                .advance(parent.run, attempt.generation, AttemptPhase::Ended)
                .unwrap();
            attempt = self.db().retire(parent.run, attempt.generation).unwrap();
        }
        let digest = execution.wait.intent.digest().unwrap();
        OriginWaitStop::Confirmed {
            origin_fence: origin,
            wait_id: execution.wait.wait,
            intent_digest: digest,
            enrollment_digest: digest,
            cause: BotWaitCleanupCause::ChildWaitCleanup,
            creation_retirement: ObservedCreationRetirement {
                creation_key: attempt.provider_session.unwrap().idempotency,
                host: attempt.host_instance.unwrap(),
                result: ObservedRetirementResult::KnownEnded {
                    session: origin.session,
                },
                observed_at_ms: now,
            },
            native_drain: DecisionNativeDrain {
                run: origin.run,
                attempt: origin.attempt,
                generation: origin.generation,
                observed_at_ms: now,
                effects: DecisionList::default(),
            },
        }
    }
    /// Enroll, stop the origin and record the proof: the wait is `retired`
    /// and the parent's host slot is free.
    fn retire(&mut self, parent: &BotExecutionFence) -> crate::ChildWaitRecord {
        self.enroll(parent);
        let release = self.release(parent);
        let ticket = self.ticket(parent);
        self.db()
            .record_child_origin_stop(&ticket, &release)
            .unwrap()
    }
    /// Publish the wait's pending projection: prepare it and acknowledge it.
    fn publish(&mut self, parent: &BotExecutionFence) -> DecisionProjectionAcknowledgment {
        let record = self
            .read()
            .next_decision_projection(parent.owner, parent.run)
            .unwrap()
            .expect("a pending projection");
        let delta = DecisionPreparedDelta::new(vec![5, 6]).unwrap();
        let prepared = DecisionPreparedProjection {
            identity: record.identity.clone(),
            target: record.target.canonical_bytes().unwrap(),
            base_version: DecisionVersionVector::new(vec![1]).unwrap(),
            result_version: DecisionVersionVector::new(vec![2]).unwrap(),
            delta_digest: decision_projection_delta_digest(&delta).unwrap(),
            delta,
        };
        self.db()
            .record_decision_preparation(parent.owner, &prepared)
            .unwrap();
        let acknowledgment = DecisionProjectionAcknowledgment {
            identity: record.identity.clone(),
            delta_digest: prepared.delta_digest,
            observed_version: DecisionVersionVector::new(vec![3]).unwrap(),
        };
        self.db()
            .acknowledge_decision_projection(parent.owner, &acknowledgment)
            .unwrap();
        acknowledgment
    }
    fn settle_child(&mut self, receipt: &ChildRequestReceipt) {
        self.db()
            .settle_bot(receipt.child_run, BotSettlement::Settled)
            .unwrap();
    }
    fn settle_children(&mut self, requested: &ChildRequested) {
        for receipt in &requested.receipts {
            self.settle_child(receipt);
        }
    }
    fn requests(&self, parent: &BotExecutionFence) -> ChildRequests {
        self.read()
            .read_child_requests(parent.owner, parent.run, self.now())
            .unwrap()
    }
    fn capacity(&self, parent: &BotExecutionFence) -> BotExecutionCapacity {
        self.read()
            .bot_execution_inventory(parent.owner.device)
            .unwrap()
            .owned
            .into_iter()
            .find(|owned| owned.execution.run == parent.run)
            .expect("an owned execution")
            .capacity
    }
    fn occupied(&self, parent: &BotExecutionFence) -> usize {
        self.read()
            .bot_execution_inventory(parent.owner.device)
            .unwrap()
            .occupied
    }
    fn count(&self, table: &str) -> i64 {
        self.read()
            .conn()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    /// Reserve the successor, bind its session, walk it to Running and
    /// activate it. Its continuation input is not acknowledged.
    fn successor(&mut self, parent: &BotExecutionFence) -> ChildWaitExecution {
        let ticket = self.ticket(parent);
        let host = fixtures::host(77);
        let reserved = match self
            .db()
            .reserve_child_successor(&ticket, host, [42; 32], None)
            .unwrap()
        {
            ChildSuccessorReservation::Reserved(execution) => *execution,
            other => panic!("expected a reservation, got {other:?}"),
        };
        let resume = reserved.resume.clone().unwrap();
        let (attempt, generation, _, creation) = resume.consumer_identity().unwrap();
        let bound = self
            .db()
            .bind_child_successor(&reserved.expected, creation, host, SessionId::mint())
            .unwrap();
        self.db()
            .advance(parent.run, generation, AttemptPhase::Starting)
            .unwrap();
        self.db()
            .advance(parent.run, generation, AttemptPhase::Running)
            .unwrap();
        self.db()
            .activate_bot(parent.run, attempt, generation, None)
            .unwrap();
        self.db()
            .set_bot_active(parent.run, attempt, generation, true, None)
            .unwrap();
        bound
    }
    fn acknowledge(&mut self, parent: &BotExecutionFence, turn: AgentTurnId) -> ChildWaitExecution {
        let execution = self.execution(parent);
        let resume = execution.resume.clone().unwrap();
        self.db()
            .record_child_resume_input(
                &execution.expected,
                DecisionTransitionId::mint(),
                resume.input_key,
                resume.input_fingerprint,
                Some(&DecisionInputAcknowledgment {
                    turn,
                    receipt_digest: DecisionDigest::from_bytes([43; 32]),
                }),
            )
            .unwrap()
    }
}

// ---- m6_child_admission ------------------------------------------------------

#[test]
fn m6_child_admission_three_items_share_one_wait_born_hold_requested() {
    let mut w = World::new();
    let parent = w.parent();
    let recipients = [BotId::mint(), BotId::mint(), BotId::mint()];
    let request = w.ask(&parent, &recipients);
    let created = match w.send(&request) {
        ChildRequestOutcome::Created(created) => created,
        other => panic!("{other:?}"),
    };
    assert_eq!(created.wait.phase, BotWaitPhase::HoldRequested);
    assert_eq!(created.wait.open_requests, 3);
    assert_eq!(created.wait.wait, request.wait().unwrap());
    assert_eq!(created.enrollment.wait_id, created.wait.wait);
    assert_eq!(
        created.enrollment.cause,
        BotWaitCleanupCause::ChildWaitCleanup
    );
    assert_eq!(created.enrollment.origin(), request.origin().unwrap());
    assert_eq!(created.receipts.len(), 3);
    for (index, receipt) in created.receipts.iter().enumerate() {
        let item = &request.requests[index];
        assert_eq!(receipt.ordinal as usize, index);
        assert_eq!(receipt.child_request, item.child_request);
        assert_eq!(receipt.recipient, recipients[index]);
        assert_eq!(receipt.state, ChildRequestState::Open);
        assert_eq!(receipt.clock, ChildClock::Running);
        assert_eq!(receipt.child, BotAdmissionState::Queued);
        assert_eq!(receipt.binding_digest, item.binding.digest().unwrap());
        assert_eq!((receipt.elapsed_ms, receipt.budget_ms), (0, 900_000));
        // The child rows equal what `admit_bot` writes: the same submission,
        // queued, version 1, with the next global queue sequences in order.
        let admitted = w.admitted(receipt.child_run);
        assert_eq!(admitted.submission, item.submission);
        assert_eq!(admitted.submission_version, 1);
        assert_eq!(admitted.state, BotAdmissionState::Queued);
        assert_eq!(
            w.read().reconcile_bot(&BotSubmissionKey {
                owner: parent.owner,
                bot: recipients[index],
                request: item.submission.provenance.submission,
            }),
            Ok(Some(admitted.clone()))
        );
        assert!(w.read().intent(receipt.child_run).unwrap().is_some());
    }
    let sequences: Vec<u64> = created
        .receipts
        .iter()
        .map(|receipt| w.admitted(receipt.child_run).sequence)
        .collect();
    assert!(sequences.windows(2).all(|pair| pair[0] + 1 == pair[1]));
    assert!(sequences[0] > w.admitted(parent.run).sequence);
    // One wait, three requests, three child admissions, one owed Control.
    assert_eq!(w.count("bot_wait_runtime"), 1);
    assert_eq!(w.count("bot_child_requests"), 3);
    assert_eq!(w.count("bot_decision_publications"), 1);
    let control = w
        .read()
        .next_decision_projection(parent.owner, parent.run)
        .unwrap()
        .unwrap();
    let RunProjectionTargetV1::Control {
        action,
        reason,
        wait_phase,
        effects,
        ..
    } = &control.target
    else {
        panic!("a Control")
    };
    assert_eq!(*action, DecisionControlAction::ChildWaiting {});
    assert_eq!(*reason, BotWaitReason::Child {});
    assert_eq!(*wait_phase, BotWaitPhase::HoldRequested);
    assert_eq!(effects.steps.as_slice()[0].kind, StepKind::WaitingForBot);
    assert!(
        effects
            .status
            .attention()
            .unwrap()
            .contains(Attention::WaitingForBot)
    );
    assert!(control.identity.decision.is_none());
    // No stored cell holds the task text.
    let cells: Vec<String> = w
        .read()
        .conn()
        .prepare("SELECT binding_json FROM bot_child_requests")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(cells.iter().all(|cell| !cell.contains("SENTINEL")));
    // The reads agree, for the parent and for each child.
    let read = w.requests(&parent);
    assert_eq!(read.requests, created.receipts);
    assert_eq!(read.wait, Some(created.wait));
    assert_eq!(
        w.read()
            .child_request_of(parent.owner, created.receipts[1].child_run, w.now())
            .unwrap(),
        Some(created.receipts[1].clone())
    );
    assert_eq!(
        w.read()
            .child_request_of(parent.owner, parent.run, w.now())
            .unwrap(),
        None
    );
}

#[test]
fn m6_child_admission_replays_before_every_limit_and_refuses_a_changed_ask() {
    let mut w = World::new();
    let parent = w.parent();
    let request = w.ask(&parent, &[BotId::mint(), BotId::mint(), BotId::mint()]);
    let ChildRequestOutcome::Created(created) = w.send(&request) else {
        panic!("created")
    };
    let rows = (
        w.count("bot_admissions"),
        w.count("bot_child_requests"),
        w.count("bot_wait_runtime"),
        w.count("bot_decision_publications"),
    );
    // The same ask again: the same ids, the same frozen intent, nothing new.
    // The Run is at its limit of three, has an open wait, and its origin may
    // even be stopped; replay precedes all of it.
    assert_eq!(
        w.send(&request),
        ChildRequestOutcome::AlreadyCreated(created.clone())
    );
    w.retire(&parent);
    let ChildRequestOutcome::AlreadyCreated(after_stop) = w.send(&request) else {
        panic!("replay after the origin stop")
    };
    assert_eq!(after_stop.enrollment, created.enrollment);
    assert_eq!(after_stop.receipts, created.receipts);
    assert_eq!(after_stop.wait.phase, BotWaitPhase::Retired);
    assert_eq!(
        rows,
        (
            w.count("bot_admissions"),
            w.count("bot_child_requests"),
            w.count("bot_wait_runtime"),
            w.count("bot_decision_publications"),
        )
    );
    // The same key with anything different is a conflict, never a second ask.
    let mut fewer = request.clone();
    fewer.requests.pop();
    let base = w.base(parent.run);
    let boot = w.boot;
    assert_eq!(
        w.db().request_child(&fewer, boot, &base),
        Err(BotLedgerError::Conflict)
    );
    let mut other = w.ask(&parent, &[BotId::mint()]);
    other.ask_key = request.ask_key;
    for item in &mut other.requests {
        item.binding.ask_key = request.ask_key;
        item.binding.wait = request.wait().unwrap();
    }
    assert_eq!(
        w.db().request_child(&other, boot, &base),
        Err(BotLedgerError::Conflict)
    );
    // A request id used by another ask is a conflict too.
    let second = w.parent();
    let mut reused = w.ask(&second, &[BotId::mint()]);
    reused.requests[0].child_request = request.requests[0].child_request;
    reused.requests[0].binding.child_request = request.requests[0].child_request;
    reused.requests[0]
        .submission
        .provenance
        .parent
        .as_mut()
        .unwrap()
        .child_request = request.requests[0].child_request;
    let base = w.base(second.run);
    assert_eq!(
        w.db().request_child(&reused, boot, &base),
        Err(BotLedgerError::Conflict)
    );
    assert_eq!(rows.1, w.count("bot_child_requests"));
}

#[test]
fn m6_child_admission_each_refusal_commits_nothing() {
    let mut w = World::new();
    let parent = w.parent();
    let before = |w: &World| {
        (
            w.count("bot_admissions"),
            w.count("run_intents"),
            w.count("bot_child_requests"),
            w.count("bot_wait_runtime"),
            w.count("bot_decision_publications"),
        )
    };
    let rows = before(&w);

    // Self-ask, named with its item.
    let own = w.ask(&parent, &[BotId::mint(), parent.bot]);
    assert_eq!(
        w.refused(&own),
        ChildRefused {
            code: ChildRefusal::SelfAsk,
            ordinal: Some(1)
        }
    );

    // A child policy that holds a grant the parent Run lacks: the last item
    // is refused and the first two are not admitted either.
    let mut wider = w.ask(&parent, &[BotId::mint(), BotId::mint(), BotId::mint()]);
    let item = &mut wider.requests[2];
    item.submission
        .policy
        .connections
        .insert(ConnectionGrantRef {
            connection: ConnectionId::mint(),
            operation: PrivateOperationKind::ReadItem,
        });
    item.binding.child_policy = ChildPolicyRef::of(1, 1, &item.submission.policy).unwrap();
    assert_eq!(
        w.refused(&wider),
        ChildRefused {
            code: ChildRefusal::ConnectionsNotSubset,
            ordinal: Some(2)
        }
    );
    assert_eq!(before(&w), rows);

    // A paused recipient, and a submission older than the recipient's barrier.
    let paused = BotId::mint();
    w.db()
        .apply_bot_barrier(&BotAdmissionBarrier {
            owner: parent.owner,
            bot: paused,
            request: BotRequestId::mint(),
            epoch: 1,
            paused: true,
        })
        .unwrap();
    let request = w.ask(&parent, &[BotId::mint(), paused]);
    assert_eq!(
        w.refused(&request),
        ChildRefused {
            code: ChildRefusal::RecipientPaused,
            ordinal: Some(1)
        }
    );
    let stale = BotId::mint();
    w.db()
        .apply_bot_barrier(&BotAdmissionBarrier {
            owner: parent.owner,
            bot: stale,
            request: BotRequestId::mint(),
            epoch: 2,
            paused: false,
        })
        .unwrap();
    let request = w.ask(&parent, &[stale]);
    assert_eq!(
        w.refused(&request),
        ChildRefused {
            code: ChildRefusal::RecipientStale,
            ordinal: Some(0)
        }
    );

    // Less than two minutes of active budget left.
    let request = w.ask(&parent, &[BotId::mint()]);
    w.tick(480_000);
    assert_eq!(w.refused(&request).code, ChildRefusal::BudgetLow);
    w.tick(-480_000);
    w.tick(479_000);
    assert!(matches!(
        w.send(&w.ask(&parent, &[BotId::mint()])),
        ChildRequestOutcome::Created(_)
    ));
    assert_eq!(before(&w).3, rows.3 + 1);
}

#[test]
fn m6_child_admission_fence_person_and_shape_are_errors_not_refusals() {
    let mut w = World::new();
    let parent = w.parent();
    let base = w.base(parent.run);
    let boot = w.boot;
    // Another turn, another attempt or another session is not the parent's
    // current executing attempt.
    let mut turn = w.ask(&parent, &[BotId::mint()]);
    turn.parent.turn = Some(AgentTurnId::mint());
    assert_eq!(
        w.db().request_child(&turn, boot, &base),
        Err(BotLedgerError::Fenced)
    );
    let mut session = w.ask(&parent, &[BotId::mint()]);
    session.parent.session = SessionId::mint();
    assert_eq!(
        w.db().request_child(&session, boot, &base),
        Err(BotLedgerError::Fenced)
    );
    // A person who is not the parent Run's person.
    let mut person = w.ask(&parent, &[BotId::mint()]);
    let other = ProfileId::new();
    person.human = other;
    for item in &mut person.requests {
        item.binding.human = other;
        item.submission.request.requested_by = ActorRef::Person { profile: other };
    }
    assert_eq!(
        w.db().request_child(&person, boot, &base),
        Err(BotLedgerError::Invalid)
    );
    // A binding that does not match its submission, four items, no turn.
    let mut mismatched = w.ask(&parent, &[BotId::mint()]);
    mismatched.requests[0].submission.request.run = RunId::mint();
    assert_eq!(
        w.db().request_child(&mismatched, boot, &base),
        Err(BotLedgerError::Invalid)
    );
    let mut four = w.ask(&parent, &[BotId::mint(), BotId::mint(), BotId::mint()]);
    let extra = w.ask(&parent, &[BotId::mint()]).requests.remove(0);
    four.requests.push(extra);
    assert_eq!(
        w.db().request_child(&four, boot, &base),
        Err(BotLedgerError::Invalid)
    );
    let mut turnless = w.ask(&parent, &[BotId::mint()]);
    turnless.parent.turn = None;
    assert_eq!(
        w.db().request_child(&turnless, boot, &base),
        Err(BotLedgerError::Invalid)
    );
    // A parent policy reference that is not the parent admission's policy.
    let mut policy = w.ask(&parent, &[BotId::mint()]);
    policy.requests[0].binding.parent_policy.policy_revision = 9;
    assert_eq!(
        w.db().request_child(&policy, boot, &base),
        Err(BotLedgerError::Invalid)
    );
    assert_eq!(w.count("bot_child_requests"), 0);
    assert_eq!(w.count("bot_wait_runtime"), 0);
}

#[test]
fn m6_child_admission_wait_depth_limit_lease_and_queue_refusals() {
    let mut w = World::new();
    let parent = w.parent();
    // An ask while the parent holds a native lease is refused.
    let key = NativeResultKey {
        run: parent.run,
        attempt: parent.attempt,
        generation: parent.generation,
        turn: parent.turn.unwrap(),
        operation_id: uuid::Uuid::new_v4(),
    };
    let prepare = PrepareNativeResult {
        key: key.clone(),
        kind: NativeResultOperation::CreatePage,
        destination: w.admitted(parent.run).submission.workspace,
        target: None,
        request_fingerprint: [3; 32],
        expected_revision: None,
        config_revision: 1,
        policy_revision: 1,
        title: "Held".into(),
    };
    use crate::NativeResultLedger;
    w.db().prepare_native_result(&prepare).unwrap();
    let proof = FreshBotPolicyProof {
        global: w.policy(&parent),
        routine: None,
    };
    w.db()
        .lease_native_result(&key, [3; 32], &parent, &proof)
        .unwrap();
    let request = w.ask(&parent, &[BotId::mint()]);
    assert_eq!(w.refused(&request).code, ChildRefusal::Capacity);
    w.db()
        .confirm_native_result(&ConfirmNativeResult {
            key: key.clone(),
            request_fingerprint: [3; 32],
            observation: NativeResultObservation::NotApplied {
                code: NativeResultRefusal::Conflict,
            },
        })
        .unwrap();

    // One wait per Run: a second ask under another key while the first is open.
    let first = w.admit(&parent, &[BotId::mint(), BotId::mint()]);
    let request = w.ask(&parent, &[BotId::mint()]);
    assert_eq!(w.refused(&request).code, ChildRefusal::WaitOpen);
    // A decision cannot be prepared while the child wait is open either.
    assert_eq!(w.count("bot_decisions"), 0);

    // Depth one: a child Run never asks. Start the first child and let it ask.
    let child = first.receipts[0].child_run;
    let child_bot = first.receipts[0].recipient;
    w.db()
        .satisfy_intent(child, fixtures::document(0xd1))
        .unwrap();
    assert_eq!(
        w.db().reserve_bot(child, None).unwrap(),
        BotReservation::Acquired
    );
    let attempt = fixtures::running_attempt(w.db(), child, 200);
    w.db()
        .activate_bot(child, attempt.attempt, attempt.generation, None)
        .unwrap();
    let mut send = crate::BotInitialSend {
        attempt: attempt.attempt,
        generation: attempt.generation,
        key: IdempotencyKey::mint(),
        fingerprint: [2; 32],
        turn: None,
    };
    w.db().prepare_bot_initial_send(child, &send, None).unwrap();
    let turn = AgentTurnId::mint();
    send.turn = Some(turn);
    w.db().acknowledge_bot_initial_send(child, &send).unwrap();
    let child_fence = BotExecutionFence {
        owner: parent.owner,
        bot: child_bot,
        run: child,
        attempt: attempt.attempt,
        generation: attempt.generation,
        session: attempt.provider_session.unwrap().session.unwrap(),
        turn: Some(turn),
    };
    let nested = w.ask(&child_fence, &[BotId::mint()]);
    assert_eq!(w.refused(&nested).code, ChildRefusal::Depth);
    // `admit_bot` refuses a submission that names a parent, and replays one
    // `request_child` wrote.
    let orphan = w
        .ask(&parent, &[BotId::mint()])
        .requests
        .remove(0)
        .submission;
    assert_eq!(w.db().admit_bot(&orphan), Err(BotLedgerError::Fenced));
    let written = w.admitted(first.receipts[1].child_run);
    assert_eq!(w.db().admit_bot(&written.submission), Ok(written));
}

#[test]
fn m6_child_admission_three_requests_per_run_and_fifty_queued_per_recipient() {
    let mut w = World::new();
    let parent = w.parent();
    // 49 queued requests admit; 50 refuse.
    let recipient = BotId::mint();
    let template = w.ask(&parent, &[recipient]).requests.remove(0).submission;
    for _ in 0..49 {
        let mut queued = template.clone();
        queued.provenance.parent = None;
        queued.provenance.submission = BotRequestId::mint();
        queued.request.run = RunId::mint();
        w.db().admit_bot(&queued).unwrap();
    }
    let first = w.admit(&parent, &[recipient]);
    assert_eq!(w.admitted(first.receipts[0].child_run).sequence, 51);
    // Now the recipient holds 50 queued rows. Finish the first wait so the
    // Run may ask again, then ask the full recipient.
    w.settle_child(&first.receipts[0]);
    w.publish(&parent);
    w.retire(&parent);
    w.successor(&parent);
    let turn = AgentTurnId::mint();
    w.acknowledge(&parent, turn);
    let acknowledgment = w.publish(&parent);
    let ticket = w.ticket(&parent);
    w.db()
        .settle_decision_wait(&ticket, &acknowledgment)
        .unwrap();
    let execution = w.read().current_attempt(parent.run).unwrap().unwrap();
    let successor = BotExecutionFence {
        attempt: execution.attempt,
        generation: execution.generation,
        session: execution.provider_session.unwrap().session.unwrap(),
        turn: Some(turn),
        ..parent.clone()
    };
    let mut queued = template.clone();
    queued.provenance.parent = None;
    queued.provenance.submission = BotRequestId::mint();
    queued.request.run = RunId::mint();
    w.db().admit_bot(&queued).unwrap();
    let full = w.ask(&successor, &[BotId::mint(), recipient]);
    assert_eq!(
        w.refused(&full),
        ChildRefused {
            code: ChildRefusal::RecipientFull,
            ordinal: Some(1)
        }
    );
    // Two more requests are within the Run's three; a third is not.
    let three = w.ask(&successor, &[BotId::mint(), BotId::mint(), BotId::mint()]);
    assert_eq!(w.refused(&three).code, ChildRefusal::RecipientLimit);
    let second = w.admit(&successor, &[BotId::mint(), BotId::mint()]);
    assert_eq!(second.wait.open_requests, 2);
    assert_eq!(w.requests(&parent).requests.len(), 3);
    // The second wait names the successor as its origin.
    assert_eq!(second.enrollment.origin_generation, successor.generation);
}

// ---- m6_child_cycle ----------------------------------------------------------

#[test]
fn m6_child_cycle_opposing_and_transitive_asks_are_refused_and_fan_out_is_not() {
    let mut w = World::new();
    let a = w.parent();
    let x = w.parent();
    // A asks X and C: allowed (fan-out from one parent is no cycle).
    let c = BotId::mint();
    let from_a = w.admit(&a, &[x.bot, c]);
    // X asks A: the direct cycle.
    let back = w.ask(&x, &[a.bot]);
    assert_eq!(
        w.refused(&back),
        ChildRefused {
            code: ChildRefusal::Cycle,
            ordinal: Some(0)
        }
    );
    // A three-recipient ask with one bad recipient is refused whole.
    let mixed = w.ask(&x, &[BotId::mint(), a.bot, BotId::mint()]);
    assert_eq!(
        w.refused(&mixed),
        ChildRefused {
            code: ChildRefusal::Cycle,
            ordinal: Some(1)
        }
    );
    assert_eq!(w.count("bot_child_requests"), 2);
    // Free A's host slot so a third Bot can run: A -> X, X -> B, then B -> A.
    w.retire(&a);
    let b = w.parent();
    w.admit(&x, &[b.bot]);
    let closing = w.ask(&b, &[a.bot]);
    assert_eq!(w.refused(&closing).code, ChildRefusal::Cycle);
    // B asking X is a cycle too (X waits on B); B asking C is not.
    let direct = w.ask(&b, &[x.bot]);
    assert_eq!(w.refused(&direct).code, ChildRefusal::Cycle);
    // After A's requests settle, the edges from A are gone: B may ask A.
    w.settle_children(&from_a);
    let after = w.ask(&b, &[a.bot, c]);
    assert!(matches!(w.send(&after), ChildRequestOutcome::Created(_)));
}

#[test]
fn m6_child_cycle_walk_uses_the_open_edge_index_and_admits_a_legal_wide_graph() {
    let w = World::new();
    let conn = w.read().conn();
    let plan: Vec<String> = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {}", cycle::EDGES))
        .unwrap()
        .query_map(
            rusqlite::params![fixtures::v4(1).to_string(), fixtures::v4(2).to_string()],
            |r| r.get(3),
        )
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|line| line.contains("bot_child_requests_open_parent")),
        "{plan:?}"
    );
    // A legal graph: 17 waiting Bots, each with three open requests, 51
    // recipients in all, arranged as a tree so every edge is reachable from
    // the first recipient. Rows are written with foreign keys off: only the
    // edge columns matter to the walk.
    conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    let device = DeviceId::from_uuid(fixtures::v4(0xa5));
    let bots: Vec<BotId> = (0..52).map(|_| BotId::mint()).collect();
    for parent in 0..17usize {
        let run = RunId::mint();
        let wait = BotWaitId::mint();
        for ordinal in 0..3usize {
            let recipient = bots[1 + parent * 3 + ordinal];
            conn.execute(
                "INSERT INTO bot_child_requests(child_request_id,parent_run_id,wait_id,ask_key,ordinal,user_scope,device_id,parent_bot_id,recipient_bot_id,human_profile_id,child_run_id,binding_version,binding_digest,binding_json,state,wait_elapsed_ms,clock,clock_boot_id,clock_since_ms,revision,created_at_ms,updated_at_ms)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?6,?10,1,zeroblob(32),'{}','open',0,'running',?6,0,1,0,0)",
                rusqlite::params![
                    ChildRequestId::mint().to_storage(),
                    run.to_storage(),
                    wait.to_storage(),
                    uuid::Uuid::new_v4().to_string(),
                    ordinal as i64,
                    fixtures::v4(0xb0).to_string(),
                    device.to_storage(),
                    bots[parent].to_storage(),
                    recipient.to_storage(),
                    RunId::mint().to_storage()
                ],
            )
            .unwrap();
        }
    }
    // A new Bot asks the root: nothing reaches it, and the walk reads every
    // one of the 51 edges, expanding each Bot once.
    let asker = BotId::mint();
    let walk = cycle::walk(conn, device, asker, &[bots[0]]).unwrap();
    assert!(!walk.closes);
    assert_eq!(walk.edges_read, 51);
    assert!(walk.edges_read <= CHILD_GRAPH_EDGE_LIMIT);
    assert_eq!(walk.expanded, 52);
    // The deepest leaf asking the root would close the longest path.
    let leaf = bots[51];
    assert!(cycle::walk(conn, device, leaf, &[bots[0]]).unwrap().closes);
    // A Bot of another computer shares no edge.
    let elsewhere = DeviceId::from_uuid(fixtures::v4(0xa6));
    assert_eq!(
        cycle::walk(conn, elsewhere, leaf, &[bots[0]])
            .unwrap()
            .edges_read,
        0
    );
}

// ---- m6_child_cycle_atomic ---------------------------------------------------

#[test]
fn m6_child_cycle_atomic_the_walk_and_the_insert_commit_or_roll_back_together() {
    let mut w = World::new();
    let a = w.parent();
    let b = w.parent();
    let forward = w.ask(&a, &[b.bot]);
    let opposing = w.ask(&b, &[a.bot]);
    // The admitted ask dies after its writes and before its commit: no row
    // of it survives, so the opposing ask run next is admitted.
    let base = w.base(a.run);
    let boot = w.boot;
    w.db().arm_crash(CrashPoint {
        op: WriteOp::RequestChild,
        at: Moment::AfterWriteBeforeCommit,
    });
    assert!(w.db().request_child(&forward, boot, &base).is_err());
    w.db().restart_in_memory();
    assert_eq!(w.count("bot_child_requests"), 0);
    assert_eq!(w.count("bot_wait_runtime"), 0);
    assert_eq!(w.count("bot_decision_publications"), 0);
    assert_eq!(w.count("bot_admissions"), 2);
    assert!(matches!(w.send(&opposing), ChildRequestOutcome::Created(_)));
    // Without the crash, the two opposing asks run in sequence and exactly
    // the second is refused.
    assert_eq!(w.refused(&forward).code, ChildRefusal::Cycle);

    let mut w = World::new();
    let a = w.parent();
    let b = w.parent();
    let forward = w.ask(&a, &[b.bot]);
    let opposing = w.ask(&b, &[a.bot]);
    assert!(matches!(w.send(&forward), ChildRequestOutcome::Created(_)));
    assert_eq!(w.refused(&opposing).code, ChildRefusal::Cycle);
    assert_eq!(w.count("bot_child_requests"), 1);
}

// ---- m6_child_occupancy ------------------------------------------------------

#[test]
fn m6_child_occupancy_follows_the_wait_phase() {
    let mut w = World::new();
    let parent = w.parent();
    assert_eq!(w.capacity(&parent), BotExecutionCapacity::Occupied);
    let asked = w.admit(&parent, &[BotId::mint()]);
    // hold_requested: the process may live.
    assert_eq!(w.capacity(&parent), BotExecutionCapacity::Occupied);
    assert_eq!(w.occupied(&parent), 1);
    w.enroll(&parent);
    assert_eq!(w.capacity(&parent), BotExecutionCapacity::Occupied);
    // retired with a proven stop: a qualified release. The Bot is still owned.
    let retired = w.retire(&parent);
    assert_eq!(retired.phase, BotWaitPhase::Retired);
    assert_eq!(
        w.capacity(&parent),
        BotExecutionCapacity::QualifiedChildRelease {
            wait: retired.wait,
            revision: retired.revision,
            origin: retired.origin,
        }
    );
    assert_eq!(w.occupied(&parent), 0);
    assert_eq!(
        w.admitted(parent.run).state,
        BotAdmissionState::Executing,
        "the parent keeps its Bot slot while it waits"
    );
    assert_eq!(w.admitted(parent.run).active_since_ms, None);
    // resume_reserved: a successor was reserved under a free slot.
    w.settle_children(&asked);
    w.publish(&parent);
    w.successor(&parent);
    assert_eq!(w.capacity(&parent), BotExecutionCapacity::Occupied);
    assert_eq!(w.occupied(&parent), 1);
    // cleanup_pending with a resume row: a real successor may be running.
    let ticket = w.ticket(&parent);
    w.db()
        .request_decision_cleanup(
            &ticket,
            DecisionTransitionId::mint(),
            DecisionClosureReason::Canceled,
        )
        .unwrap();
    assert_eq!(
        w.execution(&parent).wait.phase,
        BotWaitPhase::CleanupPending
    );
    assert_eq!(w.capacity(&parent), BotExecutionCapacity::Occupied);
}

#[test]
fn m6_child_occupancy_cleanup_without_proof_is_occupied_and_with_proof_is_released() {
    // A Stop before the origin stop was proven: a live process.
    let mut w = World::new();
    let parent = w.parent();
    w.admit(&parent, &[BotId::mint()]);
    w.enroll(&parent);
    let ticket = w.ticket(&parent);
    w.db()
        .request_decision_cleanup(
            &ticket,
            DecisionTransitionId::mint(),
            DecisionClosureReason::Canceled,
        )
        .unwrap();
    assert_eq!(w.capacity(&parent), BotExecutionCapacity::Occupied);
    assert_eq!(w.occupied(&parent), 1);
    // Once the stop is proven, the cleanup wait is a qualified release.
    let release = w.release(&parent);
    let ticket = w.ticket(&parent);
    let stopped = w.db().record_child_origin_stop(&ticket, &release).unwrap();
    assert_eq!(stopped.phase, BotWaitPhase::CleanupPending);
    assert!(matches!(
        w.capacity(&parent),
        BotExecutionCapacity::QualifiedChildRelease { .. }
    ));
    assert_eq!(w.occupied(&parent), 0);
}

#[test]
fn m6_child_occupancy_an_unqualified_wait_is_occupied_and_never_corrupts_the_inventory() {
    let mut w = World::new();
    let waiting = w.parent();
    w.admit(&waiting, &[BotId::mint()]);
    let retired = w.retire(&waiting);
    assert_eq!(w.occupied(&waiting), 0);
    // Break one release check without touching the wait: reopen the active
    // interval, which a retired origin can never have.
    w.read()
        .conn()
        .execute(
            "UPDATE bot_admissions SET active_since_ms=1000 WHERE run_id=?1",
            [waiting.run.to_storage()],
        )
        .unwrap();
    assert_eq!(w.capacity(&waiting), BotExecutionCapacity::Occupied);
    assert_eq!(w.occupied(&waiting), 1);
    // Two real Runs beside it: three "occupied" Runs, and still no error.
    // A decision arm's failed check fails the inventory; a child arm's never.
    let first = w.parent();
    let inventory = w
        .read()
        .bot_execution_inventory(waiting.owner.device)
        .unwrap();
    assert_eq!(inventory.occupied, 2);
    // A second real Run cannot be reserved while the host reads full, so
    // write it as executing directly to show the corruption test's count.
    let other = w
        .ask(&first, &[BotId::mint()])
        .requests
        .remove(0)
        .submission;
    let mut real = other.clone();
    real.provenance.parent = None;
    real.provenance.submission = BotRequestId::mint();
    real.request.run = RunId::mint();
    w.db().admit_bot(&real).unwrap();
    w.read()
        .conn()
        .execute(
            "UPDATE bot_admissions SET state='executing' WHERE run_id=?1",
            [real.request.run.to_storage()],
        )
        .unwrap();
    let inventory = w
        .read()
        .bot_execution_inventory(waiting.owner.device)
        .expect("an unqualified child wait is not ledger corruption");
    assert_eq!(inventory.occupied, 3);
    assert_eq!(inventory.owned.len(), 3);
    // Reservation arithmetic still uses the full count: nothing is admitted.
    let mut queued = real.clone();
    queued.provenance.bot = BotId::mint();
    queued.provenance.submission = BotRequestId::mint();
    queued.request.run = RunId::mint();
    w.db().admit_bot(&queued).unwrap();
    assert_eq!(
        w.db().reserve_bot(queued.request.run, None).unwrap(),
        BotReservation::HostBusy
    );
    let _ = retired;
}

#[test]
fn m6_child_occupancy_child_waits_do_not_break_or_starve_decision_preparation() {
    // With one open child wait, the cap's reason list still decodes; with
    // three, they are not mistaken for the two cleanup waits.
    let mut w = World::new();
    let mut parents = Vec::new();
    for _ in 0..3 {
        let parent = w.parent();
        w.admit(&parent, &[BotId::mint()]);
        w.retire(&parent);
        parents.push(parent);
        let tx = w.read().conn().unchecked_transaction().unwrap();
        assert_eq!(
            crate::decisions::wait_cap_on(&tx, parents[0].owner),
            Ok(()),
            "a decision may still be prepared beside {} child waits",
            parents.len()
        );
    }
    assert_eq!(w.count("bot_wait_runtime"), 3);
    // The inventory counts them as waits, none as a cleanup wait.
    let inventory = w
        .read()
        .bot_execution_inventory(parents[0].owner.device)
        .unwrap();
    assert_eq!(inventory.occupied, 0);
    assert_eq!(inventory.owned.len(), 3);
    // A real decision is prepared beside them, through the production owner.
    let decider = w.parent();
    let admitted = w.admitted(decider.run);
    let ActorRef::Person { profile } = admitted.submission.request.requested_by else {
        panic!("fixture person")
    };
    let global = w.policy(&decider);
    let binding = DecisionBindingV1::KoruEdit(DecisionEditBinding {
        version: DecisionVersionV1,
        owner: decider.owner,
        bot: decider.bot,
        run: decider.run,
        origin_attempt: decider.attempt,
        origin_generation: decider.generation,
        origin_session: decider.session,
        origin_turn: decider.turn.unwrap(),
        submission: admitted.submission.provenance.submission,
        routine: None,
        human_profile: profile,
        workspace: admitted.submission.workspace,
        target: fixtures::document(200),
        action: DecisionEditAction::EditDocument,
        operation_id: uuid::Uuid::new_v4(),
        edit_fingerprint: [3; 32],
        expected_revision: DecisionBytes::new(vec![2, 1, 2, 2, 4]).unwrap(),
        payload_version: DecisionVersionV1,
        payload: DecisionBytes::new(b"EDIT".to_vec()).unwrap(),
        captured_policy: admitted.submission.policy.clone(),
        global_policy: DecisionGlobalPolicy {
            key: global.key.clone(),
            config_revision: DecisionRevision::new(global.config_revision).unwrap(),
            policy_revision: DecisionRevision::new(global.policy_revision).unwrap(),
            digest: DecisionDigest::from_bytes(
                crate::native_results::policy_digest(&global).unwrap(),
            ),
        },
        routine_policy: None,
    });
    let proof = FreshBotPolicyProof {
        global,
        routine: None,
    };
    let base = w.base(decider.run);
    let boot = w.boot;
    let record = w
        .db()
        .prepare_bot_decision(&binding, &proof, boot, &base)
        .unwrap();
    assert_eq!(
        record.wait.reason,
        BotWaitReason::Decision {
            decision: record.receipt.decision
        }
    );
    // A5: with an open decision wait, the Run cannot ask a Bot.
    let request = w.ask(&decider, &[BotId::mint()]);
    assert_eq!(w.refused(&request).code, ChildRefusal::DecisionOpen);
}

#[test]
fn m6_child_occupancy_fifty_open_waits_are_the_cap_for_both_writers() {
    let mut w = World::new();
    let parent = w.parent();
    // 49 open child waits written as rows of other Runs' identity (a fixture
    // cannot run 49 parents on a two-slot host). Foreign keys are off for the
    // seed only; the cap reads identity and reason alone.
    {
        let conn = w.read().conn();
        conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        for index in 0..49 {
            conn.execute(
                "INSERT INTO bot_wait_runtime(wait_id,run_id,user_scope,device_id,bot_id,attempt_id,generation,session_id,turn_id,reason,phase,revision,ai_enrollment_request_json,created_at_ms,updated_at_ms)
                 VALUES(?1,?2,?3,?4,?5,?6,1,?7,?8,'child','hold_requested',1,'{}',?9,?9)",
                rusqlite::params![
                    BotWaitId::mint().to_storage(),
                    RunId::mint().to_storage(),
                    parent.owner.user_scope.to_storage(),
                    parent.owner.device.to_storage(),
                    BotId::mint().to_storage(),
                    RunAttemptId::mint().to_storage(),
                    SessionId::mint().to_string(),
                    AgentTurnId::mint().to_string(),
                    index
                ],
            )
            .unwrap();
        }
        conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    }
    let tx = w.read().conn().unchecked_transaction().unwrap();
    assert_eq!(crate::decisions::wait_cap_on(&tx, parent.owner), Ok(()));
    drop(tx);
    // The 50th wait is admitted by the child writer...
    let request = w.ask(&parent, &[BotId::mint()]);
    assert!(matches!(w.send(&request), ChildRequestOutcome::Created(_)));
    // ...and then both writers refuse: an ask, and a decision's preparation.
    let tx = w.read().conn().unchecked_transaction().unwrap();
    assert_eq!(
        crate::decisions::wait_cap_on(&tx, parent.owner),
        Err(BotLedgerError::Capacity)
    );
    drop(tx);
    let other = w.parent();
    let request = w.ask(&other, &[BotId::mint()]);
    assert_eq!(w.refused(&request).code, ChildRefusal::Capacity);
}

// ---- m6_child_clock_rows -----------------------------------------------------

#[test]
fn m6_child_clock_rows_fold_adopt_pause_and_never_decrease() {
    let mut w = World::new();
    let parent = w.parent();
    let other = w.parent();
    let asked = w.admit(&parent, &[BotId::mint(), BotId::mint()]);
    let beside = w.admit(&other, &[BotId::mint()]);
    let device = parent.owner.device;
    let boot = w.boot;
    let stored = |w: &World, id: ChildRequestId| -> (i64, String, Option<String>, Option<i64>) {
        w.read()
            .conn()
            .query_row(
                "SELECT wait_elapsed_ms,clock,clock_boot_id,clock_since_ms FROM bot_child_requests WHERE child_request_id=?1",
                [id.to_storage()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
    };
    let first = asked.receipts[0].child_request;
    assert_eq!(
        stored(&w, first),
        (0, "running".into(), Some(boot.to_storage()), Some(1000))
    );
    // A read reports the open interval without writing it.
    w.tick(4_000);
    assert_eq!(w.requests(&parent).requests[0].elapsed_ms, 4_000);
    assert_eq!(stored(&w, first).0, 0);
    // A gap: fold up to the earlier sample, restart at the later one. The
    // 20 seconds in between are not counted.
    let changed = w
        .db()
        .fold_child_clocks(device, boot, 5_000, 25_000)
        .unwrap();
    assert_eq!(changed.len(), 3, "every running clock of this computer");
    assert_eq!(
        stored(&w, first),
        (
            4_000,
            "running".into(),
            Some(boot.to_storage()),
            Some(25_000)
        )
    );
    assert_eq!(changed[0].revision.get(), 2);
    // A wall clock set back counts nothing and expires nothing.
    w.db()
        .fold_child_clocks(device, boot, 1_000, 26_000)
        .unwrap();
    assert_eq!(stored(&w, first).0, 4_000);
    // A fold with nothing to change writes nothing.
    let revision = w.requests(&parent).requests[0].revision;
    assert!(
        w.db()
            .fold_child_clocks(device, boot, 26_000, 26_000)
            .unwrap()
            .is_empty()
    );
    assert_eq!(w.requests(&parent).requests[0].revision, revision);

    // Pausing the parent Bot folds and pauses only its own requests.
    w.clock.store(30_000, Ordering::SeqCst);
    let barrier = |paused: bool, epoch: u64| BotAdmissionBarrier {
        owner: parent.owner,
        bot: parent.bot,
        request: BotRequestId::mint(),
        epoch,
        paused,
    };
    w.db().apply_bot_barrier(&barrier(true, 1)).unwrap();
    assert_eq!(stored(&w, first), (8_000, "paused".into(), None, None));
    assert_eq!(
        stored(&w, asked.receipts[1].child_request).1,
        "paused".to_string()
    );
    assert_eq!(
        stored(&w, beside.receipts[0].child_request).1,
        "running".to_string(),
        "another Bot's requests keep running"
    );
    // While paused, nothing counts and a fold leaves the clock paused.
    w.clock.store(600_000, Ordering::SeqCst);
    assert_eq!(w.requests(&parent).requests[0].elapsed_ms, 8_000);
    let changed = w
        .db()
        .fold_child_clocks(device, boot, 600_000, 600_000)
        .unwrap();
    assert_eq!(changed.len(), 1, "only the other Bot's running clock");
    assert_eq!(stored(&w, first).1, "paused".to_string());
    // Unpausing: the next fold restarts the clocks from then, with what was
    // stored. The paused time is not counted.
    w.db().apply_bot_barrier(&barrier(false, 2)).unwrap();
    assert_eq!(stored(&w, first).1, "paused".to_string());
    let changed = w
        .db()
        .fold_child_clocks(device, boot, 600_000, 600_000)
        .unwrap();
    assert_eq!(changed.len(), 2);
    assert_eq!(
        stored(&w, first),
        (
            8_000,
            "running".into(),
            Some(boot.to_storage()),
            Some(600_000)
        )
    );

    // A new delegate process adopts a clock an older one left running: the
    // stored time is kept, the time since is not invented.
    let later = DelegateBootId::mint();
    w.clock.store(900_000, Ordering::SeqCst);
    let adopted = w
        .db()
        .fold_child_clocks(device, later, 900_000, 900_000)
        .unwrap();
    assert_eq!(adopted.len(), 3);
    assert_eq!(
        stored(&w, first),
        (
            8_000,
            "running".into(),
            Some(later.to_storage()),
            Some(900_000)
        )
    );
    // The cap: stored time never exceeds the budget, however long the fold.
    w.db()
        .fold_child_clocks(device, later, 9_000_000, 9_000_000)
        .unwrap();
    assert_eq!(stored(&w, first).0, 900_000);
    assert_eq!(w.requests(&parent).requests[0].elapsed_ms, 900_000);
}

// ---- m6_child_settle ---------------------------------------------------------

#[test]
fn m6_child_settle_the_childs_own_ending_touches_only_its_request() {
    let mut w = World::new();
    let parent = w.parent();
    let asked = w.admit(&parent, &[BotId::mint(), BotId::mint()]);
    let snapshot = |w: &World| -> Vec<(String, i64, i64)> {
        let conn = w.read().conn();
        ["bot_wait_runtime", "bot_decision_publications"]
            .into_iter()
            .map(|table| {
                (
                    table.to_string(),
                    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                        .unwrap(),
                    conn.query_row(
                        &format!("SELECT coalesce(max(updated_at_ms),0)+coalesce(sum(revision),0) FROM {}", if table == "bot_wait_runtime" { "bot_wait_runtime" } else { "(SELECT created_at_ms AS updated_at_ms, run_sequence AS revision FROM bot_decision_publications)" }),
                        [],
                        |r| r.get(0),
                    )
                    .unwrap(),
                )
            })
            .collect()
    };
    let parent_before = (snapshot(&w), w.admitted(parent.run));
    w.tick(7_000);
    w.settle_child(&asked.receipts[0]);
    let after = w.requests(&parent);
    let settled = &after.requests[0];
    assert_eq!(settled.state, ChildRequestState::Settled);
    assert_eq!(settled.settled_by, Some(ChildSettledBy::ChildEnded));
    assert_eq!(settled.clock, ChildClock::Stopped);
    assert_eq!(settled.child, BotAdmissionState::Settled);
    assert_eq!(settled.elapsed_ms, 7_000);
    assert_eq!(settled.revision.get(), 2);
    // The sibling request, the wait, the publications and the parent's
    // admission are untouched.
    assert_eq!(
        after.requests[1],
        asked.receipts[1].clone().tap_elapsed(7_000)
    );
    assert_eq!(after.wait.unwrap().open_requests, 1);
    assert_eq!((snapshot(&w), w.admitted(parent.run)), parent_before);
    // A second settle of the child matches no row and is not an error.
    w.settle_child(&asked.receipts[0]);
    assert_eq!(w.requests(&parent).requests[0], *settled);
    // A refused child reads `child_ended` too; how it ended is the child's.
    w.db()
        .settle_bot(asked.receipts[1].child_run, BotSettlement::Refused)
        .unwrap();
    let last = &w.requests(&parent).requests[1];
    assert_eq!(last.settled_by, Some(ChildSettledBy::ChildEnded));
    assert_eq!(last.child, BotAdmissionState::Refused);
    assert_eq!(w.requests(&parent).wait.unwrap().open_requests, 0);
}

trait TapElapsed {
    fn tap_elapsed(self, elapsed_ms: u32) -> Self;
}
impl TapElapsed for ChildRequestReceipt {
    fn tap_elapsed(mut self, elapsed_ms: u32) -> Self {
        self.elapsed_ms = elapsed_ms;
        self
    }
}

#[test]
fn m6_child_settle_succeeds_whatever_state_the_parent_is_in() {
    // The parent's wait is being cleaned up, its admission is not executing
    // and its publication prefix is full: a publication insert for the
    // parent would be refused. The child's settlement reads none of it.
    let mut w = World::new();
    let parent = w.parent();
    let asked = w.admit(&parent, &[BotId::mint()]);
    let conn = w.read().conn();
    conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    conn.execute(
        "UPDATE bot_admissions SET state='queued',active_since_ms=NULL,payload_json='{}' WHERE run_id=?1",
        [parent.run.to_storage()],
    )
    .unwrap();
    conn.execute(
        "DELETE FROM run_attempts WHERE run_id=?1",
        [parent.run.to_storage()],
    )
    .unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    assert!(
        w.read()
            .child_wait_execution(parent.owner, parent.run)
            .is_err(),
        "the parent's own state is unreadable"
    );
    w.settle_child(&asked.receipts[0]);
    let (state, by): (String, String) = w
        .read()
        .conn()
        .query_row(
            "SELECT state,settled_by FROM bot_child_requests WHERE child_request_id=?1",
            [asked.receipts[0].child_request.to_storage()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((state.as_str(), by.as_str()), ("settled", "child_ended"));
    assert_eq!(
        w.admitted(asked.receipts[0].child_run).state,
        BotAdmissionState::Settled
    );
}

#[test]
fn m6_child_settle_timeout_and_cleanup_settle_first_and_a_late_result_changes_nothing() {
    let mut w = World::new();
    let parent = w.parent();
    let asked = w.admit(&parent, &[BotId::mint(), BotId::mint(), BotId::mint()]);
    let boot = w.boot;
    let ids: Vec<ChildRequestId> = asked.receipts.iter().map(|r| r.child_request).collect();
    // Before the budget could have been reached, a timeout settles nothing.
    w.tick(899_999);
    assert!(
        w.db()
            .settle_child_requests(parent.owner, boot, &ids)
            .unwrap()
            .is_empty()
    );
    // Another boot does not own these clocks.
    w.tick(1);
    assert!(
        w.db()
            .settle_child_requests(parent.owner, DelegateBootId::mint(), &ids)
            .unwrap()
            .is_empty()
    );
    // At the budget, the owner settles the request it names: `timed_out`.
    let timed = w
        .db()
        .settle_child_requests(parent.owner, boot, &ids[..1])
        .unwrap();
    assert_eq!(timed.len(), 1);
    assert_eq!(timed[0].settled_by, Some(ChildSettledBy::TimedOut));
    assert_eq!(timed[0].elapsed_ms, 900_000);
    assert_eq!(timed[0].clock, ChildClock::Stopped);
    assert!(
        w.db()
            .settle_child_requests(parent.owner, boot, &ids[..1])
            .unwrap()
            .is_empty(),
        "a settled request is left as it is"
    );
    // The child of a timed-out request ends later: nothing changes.
    w.settle_child(&asked.receipts[0]);
    let after = w.requests(&parent).requests[0].clone();
    assert_eq!(after.settled_by, Some(ChildSettledBy::TimedOut));
    assert_eq!(after.revision, timed[0].revision);

    // Stop of the parent: the cleanup request settles every open request
    // `parent_stopped` before the phase moves (the trigger requires it).
    w.enroll(&parent);
    let ticket = w.ticket(&parent);
    w.db()
        .request_decision_cleanup(
            &ticket,
            DecisionTransitionId::mint(),
            DecisionClosureReason::Canceled,
        )
        .unwrap();
    let stopped = w.requests(&parent);
    assert_eq!(stopped.wait.unwrap().phase, BotWaitPhase::CleanupPending);
    assert_eq!(stopped.wait.unwrap().open_requests, 0);
    for receipt in &stopped.requests[1..] {
        assert_eq!(receipt.settled_by, Some(ChildSettledBy::ParentStopped));
        assert_eq!(receipt.state, ChildRequestState::Settled);
    }
    assert_eq!(
        stopped.requests[0].settled_by,
        Some(ChildSettledBy::TimedOut)
    );
    // A late result after `parent_stopped` changes nothing either, and the
    // child's own settlement still succeeds.
    w.settle_child(&asked.receipts[1]);
    assert_eq!(
        w.requests(&parent).requests[1],
        stopped.requests[1]
            .clone()
            .with_child(BotAdmissionState::Settled)
    );
    // The observation names the cause; no successor exists and none can.
    let observation = w
        .read()
        .read_bot_child_wait(parent.owner, parent.bot, parent.run, ticket.wait)
        .unwrap()
        .unwrap();
    assert_eq!(
        observation.disposition,
        ChildWaitDisposition::Closed {
            cause: DecisionClosureReason::Canceled
        }
    );
    assert!(observation.resume.is_none());
    let ticket = w.ticket(&parent);
    assert!(matches!(
        w.db()
            .reserve_child_successor(&ticket, fixtures::host(77), [42; 32], None),
        Ok(ChildSuccessorReservation::NotRetired)
    ));
}

trait WithChild {
    fn with_child(self, child: BotAdmissionState) -> Self;
}
impl WithChild for ChildRequestReceipt {
    fn with_child(mut self, child: BotAdmissionState) -> Self {
        self.child = child;
        self
    }
}

// ---- m6_child_slot_rule ------------------------------------------------------

/// A queued admission of a new Bot, ready to be reserved.
fn queued(w: &mut World, like: &BotExecutionFence) -> RunId {
    let mut submission = w.admitted(like.run).submission;
    submission.provenance.bot = BotId::mint();
    submission.provenance.submission = BotRequestId::mint();
    submission.provenance.ai_attempt_id = None;
    submission.provenance.assistant_message_id = None;
    submission.request.run = RunId::mint();
    w.db().admit_bot(&submission).unwrap();
    submission.request.run
}

#[test]
fn m6_child_slot_rule_a_resumable_parent_goes_ahead_of_queued_work() {
    let mut w = World::new();
    let waiting = w.parent();
    let running = w.parent();
    let asked = w.admit(&waiting, &[BotId::mint()]);
    w.retire(&waiting);
    let run = queued(&mut w, &running);
    // One slot is free and the wait still has an open request: it is not
    // due, so queued work may take the slot.
    let inventory = w
        .read()
        .bot_execution_inventory(waiting.owner.device)
        .unwrap();
    assert_eq!(inventory.occupied, 1);
    assert_eq!(due_on(w.read().conn(), &inventory, w.now()).unwrap(), 0);
    // The request settles: the wait is resumable and due. free (1) <= due (1).
    w.settle_children(&asked);
    assert_eq!(due_on(w.read().conn(), &inventory, w.now()).unwrap(), 1);
    assert_eq!(
        w.db().reserve_bot(run, None).unwrap(),
        BotReservation::HostBusy
    );
    // A paused parent's wait is not counted, for as long as it is paused.
    let barrier = |paused: bool, epoch: u64| BotAdmissionBarrier {
        owner: waiting.owner,
        bot: waiting.bot,
        request: BotRequestId::mint(),
        epoch,
        paused,
    };
    w.db().apply_bot_barrier(&barrier(true, 1)).unwrap();
    assert_eq!(due_on(w.read().conn(), &inventory, w.now()).unwrap(), 0);
    let ticket = w.ticket(&waiting);
    assert!(matches!(
        w.db()
            .reserve_child_successor(&ticket, fixtures::host(77), [42; 32], None),
        Ok(ChildSuccessorReservation::Paused)
    ));
    w.db().apply_bot_barrier(&barrier(false, 2)).unwrap();
    assert_eq!(due_on(w.read().conn(), &inventory, w.now()).unwrap(), 1);
    // The parent takes the slot: it is then occupied, never due, and `due`
    // has fallen to zero. The host is full, so queued work still waits.
    w.publish(&waiting);
    w.successor(&waiting);
    let inventory = w
        .read()
        .bot_execution_inventory(waiting.owner.device)
        .unwrap();
    assert_eq!(inventory.occupied, 2);
    assert_eq!(due_on(w.read().conn(), &inventory, w.now()).unwrap(), 0);
    assert_eq!(
        w.db().reserve_bot(run, None).unwrap(),
        BotReservation::HostBusy
    );
}

#[test]
fn m6_child_slot_rule_waits_that_can_never_resume_do_not_stop_the_queue() {
    let mut w = World::new();
    // One unqualified wait, one with a spent budget: neither is due.
    let unqualified = w.parent();
    let spent = w.parent();
    for parent in [&unqualified, &spent] {
        let asked = w.admit(parent, &[BotId::mint()]);
        w.retire(parent);
        w.settle_children(&asked);
    }
    let conn = w.read().conn();
    conn.execute(
        "UPDATE bot_admissions SET active_elapsed_ms=?2 WHERE run_id=?1",
        rusqlite::params![spent.run.to_storage(), BOT_ACTIVE_BUDGET_MS as i64],
    )
    .unwrap();
    // The other wait loses its AI receipt: a release check fails.
    conn.execute_batch("DROP TRIGGER bot_wait_runtime_immutable")
        .unwrap();
    conn.execute(
        "UPDATE bot_wait_runtime SET ai_cleanup_receipt_json=NULL WHERE run_id=?1",
        [unqualified.run.to_storage()],
    )
    .unwrap();
    let inventory = w
        .read()
        .bot_execution_inventory(spent.owner.device)
        .unwrap();
    assert_eq!(w.capacity(&unqualified), BotExecutionCapacity::Occupied);
    assert!(matches!(
        w.capacity(&spent),
        BotExecutionCapacity::QualifiedChildRelease { .. }
    ));
    assert_eq!(inventory.occupied, 1);
    assert_eq!(due_on(w.read().conn(), &inventory, w.now()).unwrap(), 0);
    // A queued admission is served: free (1) > due (0).
    let run = queued(&mut w, &spent);
    assert_eq!(
        w.db().reserve_bot(run, None).unwrap(),
        BotReservation::Acquired
    );
    // Each reservation attempt says why it can never happen, so the owner
    // cleans the wait up at once.
    let ticket = w.ticket(&spent);
    assert!(matches!(
        w.db()
            .reserve_child_successor(&ticket, fixtures::host(77), [42; 32], None),
        Ok(ChildSuccessorReservation::BudgetSpent)
    ));
    let ticket = w.ticket(&unqualified);
    assert!(matches!(
        w.db()
            .reserve_child_successor(&ticket, fixtures::host(77), [42; 32], None),
        Ok(ChildSuccessorReservation::Unqualified)
    ));
}

#[test]
fn m6_child_slot_rule_a_decision_wait_is_never_counted() {
    // A retired decision wait frees its slot today and is not "due": the
    // rule is for child waits only.
    let mut w = World::new();
    let parent = w.parent();
    let global = w.policy(&parent);
    let admitted = w.admitted(parent.run);
    let ActorRef::Person { profile } = admitted.submission.request.requested_by else {
        panic!("fixture person")
    };
    let binding = DecisionBindingV1::KoruEdit(DecisionEditBinding {
        version: DecisionVersionV1,
        owner: parent.owner,
        bot: parent.bot,
        run: parent.run,
        origin_attempt: parent.attempt,
        origin_generation: parent.generation,
        origin_session: parent.session,
        origin_turn: parent.turn.unwrap(),
        submission: admitted.submission.provenance.submission,
        routine: None,
        human_profile: profile,
        workspace: admitted.submission.workspace,
        target: fixtures::document(200),
        action: DecisionEditAction::EditDocument,
        operation_id: uuid::Uuid::new_v4(),
        edit_fingerprint: [3; 32],
        expected_revision: DecisionBytes::new(vec![2, 1, 2, 2, 4]).unwrap(),
        payload_version: DecisionVersionV1,
        payload: DecisionBytes::new(b"EDIT".to_vec()).unwrap(),
        captured_policy: admitted.submission.policy.clone(),
        global_policy: DecisionGlobalPolicy {
            key: global.key.clone(),
            config_revision: DecisionRevision::new(global.config_revision).unwrap(),
            policy_revision: DecisionRevision::new(global.policy_revision).unwrap(),
            digest: DecisionDigest::from_bytes(
                crate::native_results::policy_digest(&global).unwrap(),
            ),
        },
        routine_policy: None,
    });
    let proof = FreshBotPolicyProof {
        global,
        routine: None,
    };
    let base = w.base(parent.run);
    let boot = w.boot;
    let record = w
        .db()
        .prepare_bot_decision(&binding, &proof, boot, &base)
        .unwrap();
    w.publish(&parent);
    let record = w
        .read()
        .bot_decision(parent.owner, parent.run, record.receipt.decision)
        .unwrap()
        .unwrap();
    let record = crate::test_support::request_enrollment(w.db(), &record).unwrap();
    let record = crate::test_support::record_enrollment(w.db(), &record).unwrap();
    let now = w.now();
    let record = crate::test_support::stop_origin(w.db(), &record, now).unwrap();
    assert_eq!(record.wait.phase, BotWaitPhase::Retired);
    let inventory = w
        .read()
        .bot_execution_inventory(parent.owner.device)
        .unwrap();
    assert_eq!(inventory.occupied, 0);
    assert!(matches!(
        inventory.owned[0].capacity,
        BotExecutionCapacity::QualifiedDecisionRelease { .. }
    ));
    assert_eq!(due_on(w.read().conn(), &inventory, w.now()).unwrap(), 0);
    let run = queued(&mut w, &parent);
    assert_eq!(
        w.db().reserve_bot(run, None).unwrap(),
        BotReservation::Acquired
    );
    // A5 from the other side: the decision's successor, whose decision is
    // not settled, cannot ask a Bot. It is told why, not that it is stale.
    let record = crate::test_support::bind_successor(
        w.db(),
        &record,
        fixtures::host(78),
        SessionId::mint(),
        &proof,
        boot,
    )
    .unwrap();
    let (_, successor) =
        crate::test_support::acknowledge_successor(w.db(), &record, AgentTurnId::mint()).unwrap();
    let request = w.ask(&successor, &[BotId::mint()]);
    assert_eq!(w.refused(&request).code, ChildRefusal::DecisionOpen);
    assert_eq!(w.count("bot_child_requests"), 0);
}

// ---- m6_child_resume_rows ----------------------------------------------------

#[test]
fn m6_child_resume_rows_reserve_requires_a_resumable_wait_and_a_free_slot() {
    let mut w = World::new();
    let parent = w.parent();
    let beside = w.parent();
    let asked = w.admit(&parent, &[BotId::mint(), BotId::mint()]);
    let host = fixtures::host(77);
    // Not retired yet.
    let ticket = w.ticket(&parent);
    assert!(matches!(
        w.db()
            .reserve_child_successor(&ticket, host, [42; 32], None),
        Ok(ChildSuccessorReservation::NotRetired)
    ));
    w.retire(&parent);
    // Retired, with requests still open.
    let ticket = w.ticket(&parent);
    assert!(matches!(
        w.db()
            .reserve_child_successor(&ticket, host, [42; 32], None),
        Ok(ChildSuccessorReservation::OpenRequests)
    ));
    w.settle_children(&asked);
    // A stale ticket is refused.
    let mut stale = ticket.clone();
    stale.revision = DecisionRevision::new(1).unwrap();
    assert_eq!(
        w.db().reserve_child_successor(&stale, host, [42; 32], None),
        Err(BotLedgerError::Stale)
    );
    // No free slot: both are taken by Runs that execute.
    let third = queued(&mut w, &beside);
    assert_eq!(
        w.db().reserve_bot(third, None).unwrap(),
        BotReservation::HostBusy
    );
    w.read()
        .conn()
        .execute(
            "UPDATE bot_admissions SET state='executing' WHERE run_id=?1",
            [third.to_storage()],
        )
        .unwrap();
    assert!(matches!(
        w.db()
            .reserve_child_successor(&ticket, host, [42; 32], None),
        Ok(ChildSuccessorReservation::HostBusy)
    ));
    w.read()
        .conn()
        .execute(
            "UPDATE bot_admissions SET state='settled' WHERE run_id=?1",
            [third.to_storage()],
        )
        .unwrap();
    // Now it reserves: generation + 1, a prepared resume row, resume_reserved.
    let ChildSuccessorReservation::Reserved(reserved) = w
        .db()
        .reserve_child_successor(&ticket, host, [42; 32], None)
        .unwrap()
    else {
        panic!("a reservation")
    };
    assert_eq!(reserved.wait.phase, BotWaitPhase::ResumeReserved);
    let resume = reserved.resume.clone().unwrap();
    assert_eq!(resume.state, DecisionResumeState::Prepared {});
    assert_eq!(resume.origin, reserved.wait.origin);
    let (attempt, generation, session, _) = resume.consumer_identity().unwrap();
    assert_eq!(generation, parent.generation.next());
    assert_ne!(attempt, parent.attempt);
    assert_eq!(session, None);
    assert_eq!(reserved.current_attempt.attempt, attempt);
    assert!(matches!(
        reserved.cleanup_target,
        DecisionCleanupTarget::Consumer { .. }
    ));
    // The admission's state never changed: it is still executing.
    assert_eq!(w.admitted(parent.run).state, BotAdmissionState::Executing);
    // An exact retry answers the same reservation; another host or input
    // fingerprint is refused; a second successor is impossible.
    assert!(matches!(
        w.db().reserve_child_successor(&ticket, host, [42; 32], None),
        Ok(ChildSuccessorReservation::AlreadyReserved(again)) if again.resume == reserved.resume
    ));
    assert_eq!(
        w.db()
            .reserve_child_successor(&ticket, host, [41; 32], None),
        Err(BotLedgerError::Conflict)
    );
    assert_eq!(w.count("bot_child_resumes"), 1);
    assert_eq!(w.read().attempts(parent.run).unwrap().len(), 2);
}

#[test]
fn m6_child_resume_rows_the_successor_is_activated_and_its_budget_runs() {
    let mut w = World::new();
    let parent = w.parent();
    let asked = w.admit(&parent, &[BotId::mint()]);
    w.publish(&parent);
    w.tick(60_000);
    w.retire(&parent);
    // The budget stopped with the origin: one minute was spent.
    assert_eq!(w.admitted(parent.run).active_elapsed_ms, 60_000);
    w.tick(300_000);
    assert_eq!(w.admitted(parent.run).active_elapsed_at(w.now()), 60_000);
    w.settle_children(&asked);
    // An attempt the resume row does not name is refused activity: the
    // stopped origin, and a made-up attempt.
    let ticket = w.ticket(&parent);
    let host = fixtures::host(77);
    let ChildSuccessorReservation::Reserved(reserved) = w
        .db()
        .reserve_child_successor(&ticket, host, [42; 32], None)
        .unwrap()
    else {
        panic!("a reservation")
    };
    let (attempt, generation, _, creation) = reserved
        .resume
        .clone()
        .unwrap()
        .consumer_identity()
        .unwrap();
    assert_eq!(
        w.db()
            .set_bot_active(parent.run, parent.attempt, parent.generation, true, None),
        Err(BotLedgerError::Fenced)
    );
    assert_eq!(
        w.db()
            .activate_bot(parent.run, RunAttemptId::mint(), generation, None),
        Err(BotLedgerError::Fenced)
    );
    // The recorded successor binds, starts and is activated: its budget runs.
    let session = SessionId::mint();
    let bound = w
        .db()
        .bind_child_successor(&reserved.expected, creation, host, session)
        .unwrap();
    assert_eq!(
        bound.resume.clone().unwrap().state,
        DecisionResumeState::SessionBound {}
    );
    // A second bind of the same session is a replay; another session is not.
    w.db()
        .bind_child_successor(&bound.expected, creation, host, session)
        .unwrap();
    assert!(
        w.db()
            .bind_child_successor(&bound.expected, creation, host, SessionId::mint())
            .is_err()
    );
    w.db()
        .advance(parent.run, generation, AttemptPhase::Starting)
        .unwrap();
    w.db()
        .advance(parent.run, generation, AttemptPhase::Running)
        .unwrap();
    w.db()
        .activate_bot(parent.run, attempt, generation, None)
        .unwrap();
    w.db()
        .set_bot_active(parent.run, attempt, generation, true, None)
        .unwrap();
    assert_eq!(w.admitted(parent.run).active_since_ms, Some(w.now()));
    w.tick(30_000);
    assert_eq!(w.admitted(parent.run).active_elapsed_at(w.now()), 90_000);
    // Before the continuation input is acknowledged the successor is not
    // the Run's delivering attempt.
    assert_eq!(
        w.read()
            .child_acknowledged_continuation(parent.owner, parent.run)
            .unwrap(),
        None
    );
    // The input is recorded unconfirmed, then acknowledged once, for good.
    let resume = bound.resume.clone().unwrap();
    let unconfirmed = w
        .db()
        .record_child_resume_input(
            &bound.expected,
            DecisionTransitionId::mint(),
            resume.input_key,
            resume.input_fingerprint,
            None,
        )
        .unwrap();
    assert_eq!(
        unconfirmed.resume.clone().unwrap().state,
        DecisionResumeState::InputUnconfirmed {}
    );
    assert!(
        w.db()
            .record_child_resume_input(
                &bound.expected,
                DecisionTransitionId::mint(),
                IdempotencyKey::mint(),
                resume.input_fingerprint,
                None,
            )
            .is_err(),
        "another input key is not this resume's"
    );
    let turn = AgentTurnId::mint();
    let acknowledged = w.acknowledge(&parent, turn);
    assert_eq!(
        acknowledged.resume.clone().unwrap().accepted_turn(),
        Some(turn)
    );
    assert_eq!(
        w.read()
            .child_acknowledged_continuation(parent.owner, parent.run)
            .unwrap(),
        Some(ChildAcknowledgedContinuation {
            wait: acknowledged.wait.wait,
            attempt,
            generation,
            session,
            turn,
        })
    );
    // A later, different acknowledgment is refused; the same one replays.
    w.acknowledge(&parent, turn);
    let execution = w.execution(&parent);
    assert!(
        w.db()
            .record_child_resume_input(
                &execution.expected,
                DecisionTransitionId::mint(),
                resume.input_key,
                resume.input_fingerprint,
                Some(&DecisionInputAcknowledgment {
                    turn: AgentTurnId::mint(),
                    receipt_digest: DecisionDigest::from_bytes([43; 32]),
                }),
            )
            .is_err()
    );
    // The acknowledgment owed exactly one Resume Control, which clears the
    // waiting marker; once it is applied the wait settles and the Run is an
    // ordinary Bot Run again.
    // While that last publication is owed, a new ask is told to retry: the
    // wait is closing, not open.
    let successor = BotExecutionFence {
        attempt,
        generation,
        session,
        turn: Some(turn),
        ..parent.clone()
    };
    let early = w.ask(&successor, &[BotId::mint()]);
    let refused = w.refused(&early);
    assert_eq!(refused.code, ChildRefusal::WaitClosing);
    assert!(refused.code.retryable());
    let pending = w
        .read()
        .next_decision_projection(parent.owner, parent.run)
        .unwrap()
        .unwrap();
    let RunProjectionTargetV1::Control {
        action, effects, ..
    } = &pending.target
    else {
        panic!("a Control")
    };
    assert_eq!(
        *action,
        DecisionControlAction::Resume {
            via: ResumeSeam::RecordedContext
        }
    );
    assert_eq!(effects.status, RunStatus::running());
    let acknowledgment = w.publish(&parent);
    let ticket = w.ticket(&parent);
    let settled = w
        .db()
        .settle_decision_wait(&ticket, &acknowledgment)
        .unwrap();
    assert_eq!(settled.phase, BotWaitPhase::Settled);
    assert_eq!(
        w.read()
            .child_wait_execution(parent.owner, parent.run)
            .unwrap(),
        None
    );
    assert_eq!(w.capacity(&parent), BotExecutionCapacity::Occupied);
    // The successor's acknowledged turn may now save a page: the open-wait
    // fence has lifted and its turn is a known input turn.
    use crate::NativeResultLedger;
    let prepare = PrepareNativeResult {
        key: NativeResultKey {
            run: parent.run,
            attempt,
            generation,
            turn,
            operation_id: uuid::Uuid::new_v4(),
        },
        kind: NativeResultOperation::CreatePage,
        destination: w.admitted(parent.run).submission.workspace,
        target: None,
        request_fingerprint: [3; 32],
        expected_revision: None,
        config_revision: 1,
        policy_revision: 1,
        title: "After the wait".into(),
    };
    w.db().prepare_native_result(&prepare).unwrap();
    // The observation of the settled wait still reads, and is not closed.
    let observation = w
        .read()
        .read_bot_child_wait(parent.owner, parent.bot, parent.run, settled.wait)
        .unwrap()
        .unwrap();
    assert_eq!(observation.disposition, ChildWaitDisposition::Open {});
    assert!(matches!(
        observation.origin_stop,
        OriginWaitStop::Confirmed { .. }
    ));
    assert_eq!(observation.resume.unwrap().accepted_turn(), Some(turn));
}

#[test]
fn m6_child_resume_rows_selection_is_ordered_bounded_and_resumes_by_cursor() {
    let mut w = World::new();
    let first = w.parent();
    w.admit(&first, &[BotId::mint()]);
    w.retire(&first);
    w.tick(10);
    let second = w.parent();
    let asked = w.admit(&second, &[BotId::mint(), BotId::mint()]);
    w.retire(&second);
    w.settle_children(&asked);
    w.tick(10);
    let third = w.parent();
    w.admit(&third, &[BotId::mint()]);
    let device = first.owner.device;
    let all = w.read().child_resume_runs(device, None, 16).unwrap();
    assert_eq!(
        all.iter().map(|run| run.run).collect::<Vec<_>>(),
        [first.run, second.run, third.run],
        "oldest first"
    );
    assert_eq!(
        all.iter()
            .map(|run| (run.phase, run.open_requests))
            .collect::<Vec<_>>(),
        [
            (BotWaitPhase::Retired, 1),
            (BotWaitPhase::Retired, 0),
            (BotWaitPhase::HoldRequested, 1)
        ]
    );
    // The page is bounded and continues strictly after its cursor.
    let page = w.read().child_resume_runs(device, None, 2).unwrap();
    assert_eq!(page.len(), 2);
    let rest = w
        .read()
        .child_resume_runs(device, Some(&page[1].cursor), 2)
        .unwrap();
    assert_eq!(
        rest.iter().map(|run| run.run).collect::<Vec<_>>(),
        [third.run]
    );
    assert!(
        w.read()
            .child_resume_runs(device, Some(&rest[0].cursor), 16)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        w.read().child_resume_runs(device, None, 0).unwrap().len(),
        1
    );
    assert_eq!(
        w.read()
            .child_resume_runs(device, None, 1000)
            .unwrap()
            .len(),
        3
    );
    // The selector reads the child open-wait index.
    let plan: Vec<String> = w
        .read()
        .conn()
        .prepare("EXPLAIN QUERY PLAN SELECT wait_id FROM bot_wait_runtime INDEXED BY bot_wait_runtime_child_open WHERE device_id=?1 AND reason='child' AND phase<>'settled' ORDER BY created_at_ms,wait_id")
        .unwrap()
        .query_map([device.to_storage()], |r| r.get(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|line| line.contains("bot_wait_runtime_child_open")),
        "{plan:?}"
    );
    // A settled wait leaves the selection.
    let another = DeviceId::from_uuid(fixtures::v4(0xa6));
    assert!(
        w.read()
            .child_resume_runs(another, None, 16)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn m6_child_resume_rows_a_stop_after_reservation_closes_the_resume_and_never_resumes() {
    let mut w = World::new();
    let parent = w.parent();
    let asked = w.admit(&parent, &[BotId::mint()]);
    w.publish(&parent);
    w.retire(&parent);
    w.settle_children(&asked);
    let bound = w.successor(&parent);
    let resume = bound.resume.clone().unwrap();
    let ticket = w.ticket(&parent);
    w.db()
        .request_decision_cleanup(
            &ticket,
            DecisionTransitionId::mint(),
            DecisionClosureReason::Canceled,
        )
        .unwrap();
    let execution = w.execution(&parent);
    assert_eq!(execution.wait.phase, BotWaitPhase::CleanupPending);
    assert!(matches!(
        execution.resume.clone().unwrap().state,
        DecisionResumeState::Closed {
            acknowledgment: None
        }
    ));
    // The cleanup subject is the successor, not the retired origin.
    assert!(matches!(
        execution.cleanup_target,
        DecisionCleanupTarget::Consumer { .. }
    ));
    // A late acknowledgment is retained as history and resumes nothing: no
    // Resume Control is owed and the successor may not become active again.
    let late_turn = AgentTurnId::mint();
    let late = w.acknowledge(&parent, late_turn);
    // A wait being cleaned up is open, not closing: a new ask is refused
    // for good, never told to retry.
    let (attempt, generation, session, _) = resume.consumer_identity().unwrap();
    let stopping = BotExecutionFence {
        attempt,
        generation,
        session: session.unwrap(),
        turn: Some(late_turn),
        ..parent.clone()
    };
    let request = w.ask(&stopping, &[BotId::mint()]);
    assert_eq!(w.refused(&request).code, ChildRefusal::WaitOpen);
    assert!(matches!(
        late.resume.clone().unwrap().state,
        DecisionResumeState::Closed {
            acknowledgment: Some(_)
        }
    ));
    assert_eq!(late.wait.phase, BotWaitPhase::CleanupPending);
    let pending = w
        .read()
        .next_decision_projection(parent.owner, parent.run)
        .unwrap()
        .unwrap();
    assert!(matches!(
        &pending.target,
        RunProjectionTargetV1::Control {
            action: DecisionControlAction::CleanupRequested { .. },
            ..
        }
    ));
    w.db()
        .set_bot_active(parent.run, attempt, generation, false, None)
        .unwrap();
    assert_eq!(
        w.db()
            .set_bot_active(parent.run, attempt, generation, true, None),
        Err(BotLedgerError::Fenced)
    );
    // No further cleanup request may supersede this one's cause.
    let observation = w
        .read()
        .read_bot_child_wait(parent.owner, parent.bot, parent.run, execution.wait.wait)
        .unwrap()
        .unwrap();
    assert_eq!(
        observation.disposition,
        ChildWaitDisposition::Closed {
            cause: DecisionClosureReason::Canceled
        }
    );
    assert!(matches!(
        observation.consumer_cleanup,
        ConsumerCleanup::Owed { .. }
    ));
}

#[test]
fn m6_child_resume_rows_a_child_run_never_asks_a_private_question() {
    // The ledger refuses to prepare a private-operation decision for a Run
    // that is some request's child, whatever the gateway did, before any
    // policy or fence is consulted and without writing anything.
    let mut w = World::new();
    let parent = w.parent();
    let asked = w.admit(&parent, &[BotId::mint()]);
    let child = asked.receipts[0].child_run;
    assert!(is_child_run_on(w.read().conn(), child).unwrap());
    assert!(!is_child_run_on(w.read().conn(), parent.run).unwrap());
    let admitted = w.admitted(child);
    let ActorRef::Person { profile } = admitted.submission.request.requested_by else {
        panic!("fixture person")
    };
    let global = w.policy(&parent);
    let proposal = crate::private_effects::tests::proposal(uuid::Uuid::new_v4());
    let binding = |run: RunId, bot: BotId| {
        DecisionBindingV1::PrivateOperation(DecisionPrivateBinding {
            version: DecisionVersionV1,
            owner: parent.owner,
            bot,
            run,
            origin_attempt: RunAttemptId::mint(),
            origin_generation: AttemptGeneration::FIRST,
            origin_session: SessionId::mint(),
            origin_turn: AgentTurnId::mint(),
            submission: admitted.submission.provenance.submission,
            routine: None,
            human_profile: profile,
            workspace: admitted.submission.workspace,
            connection: proposal.connection,
            connection_revision: proposal.connection_revision,
            account: proposal.account,
            item: proposal.item,
            item_ref: proposal.item_ref,
            operation: proposal.operation,
            operation_id: proposal.operation_id,
            request_fingerprint: proposal.request_fingerprint,
            captured_policy: admitted.submission.policy.clone(),
            global_policy: DecisionGlobalPolicy {
                key: BotPolicyBarrierKey {
                    bot,
                    ..global.key.clone()
                },
                config_revision: DecisionRevision::new(global.config_revision).unwrap(),
                policy_revision: DecisionRevision::new(global.policy_revision).unwrap(),
                digest: DecisionDigest::from_bytes(
                    crate::native_results::policy_digest(&global).unwrap(),
                ),
            },
            routine_policy: None,
        })
    };
    let proof = FreshBotPolicyProof {
        global: global.clone(),
        routine: None,
    };
    let base = w.base(parent.run);
    let boot = w.boot;
    let child_binding = binding(child, asked.receipts[0].recipient);
    assert_eq!(
        w.db()
            .prepare_private_decision(&child_binding, &proof, boot, &base)
            .map(|_| ()),
        Err(crate::PrivateLedgerError::Ledger(BotLedgerError::Fenced))
    );
    // The generic prepare entry refuses the same binding the same way.
    assert_eq!(
        w.db()
            .prepare_bot_decision(&child_binding, &proof, boot, &base)
            .map(|_| ()),
        Err(BotLedgerError::Fenced)
    );
    assert_eq!(w.count("bot_decisions"), 0);
    assert_eq!(w.count("bot_wait_runtime"), 1);
}

// ---- m6_version_id_column (behaviour half) -----------------------------------

#[test]
fn m6_version_id_column_is_stored_with_the_verified_result_and_survives_registration() {
    use crate::NativeResultLedger;
    let mut w = World::new();
    let parent = w.parent();
    let proof = FreshBotPolicyProof {
        global: w.policy(&parent),
        routine: None,
    };
    let stored = |w: &World, operation: uuid::Uuid| -> Option<String> {
        w.read()
            .conn()
            .query_row(
                "SELECT verified_version_id FROM native_result_intents WHERE operation_id=?1",
                [operation.to_string()],
                |r| r.get(0),
            )
            .unwrap()
    };
    let mut saved = Vec::new();
    for version_id in [Some(DocumentVersionId::new()), None] {
        let key = NativeResultKey {
            run: parent.run,
            attempt: parent.attempt,
            generation: parent.generation,
            turn: parent.turn.unwrap(),
            operation_id: uuid::Uuid::new_v4(),
        };
        let destination = w.admitted(parent.run).submission.workspace;
        let prepared = w
            .db()
            .prepare_native_result(&PrepareNativeResult {
                key: key.clone(),
                kind: NativeResultOperation::CreatePage,
                destination,
                target: None,
                request_fingerprint: [3; 32],
                expected_revision: None,
                config_revision: 1,
                policy_revision: 1,
                title: "Saved".into(),
            })
            .unwrap();
        // Null until the result is verified.
        assert_eq!(stored(&w, key.operation_id), None);
        w.db()
            .lease_native_result(&key, [3; 32], &parent, &proof)
            .unwrap();
        let evidence = NativeResultReference {
            version_id,
            operation_id: key.operation_id,
            workspace: prepared.destination,
            document: prepared.target,
            version: vec![1, 2, 3],
            title: "Saved".into(),
            confirmed_at_ms: 5,
            kind: NativeResultKind::CreatedPage,
        };
        let confirm = ConfirmNativeResult {
            key: key.clone(),
            request_fingerprint: [3; 32],
            observation: NativeResultObservation::Durable {
                evidence: evidence.clone(),
            },
        };
        let verified = w.db().confirm_native_result(&confirm).unwrap();
        // Stored, and returned in the retained reference exactly as sent, so
        // the sender's equality test on the reply still holds.
        assert_eq!(verified.verified, Some(evidence.clone()));
        assert_eq!(
            stored(&w, key.operation_id),
            version_id.map(|id| id.to_storage())
        );
        // An exact replay answers the same; a different version id conflicts.
        assert_eq!(w.db().confirm_native_result(&confirm).unwrap(), verified);
        let mut other = confirm.clone();
        if let NativeResultObservation::Durable { evidence } = &mut other.observation {
            evidence.version_id = Some(DocumentVersionId::new());
        }
        assert_eq!(
            w.db().confirm_native_result(&other),
            Err(BotLedgerError::Conflict)
        );
        // It survives the stage moving to `registered`.
        let registered = w.db().register_native_result(&key, &evidence).unwrap();
        assert_eq!(registered.stage, NativeResultStage::Registered);
        assert_eq!(registered.verified, Some(evidence.clone()));
        assert_eq!(
            stored(&w, key.operation_id),
            version_id.map(|id| id.to_storage())
        );
        saved.push(evidence);
    }
    assert!(saved[0].version_id.is_some() && saved[1].version_id.is_none());
}

// ---- the wait's publications: format, cleanup, delivery, a Routine parent ----

#[test]
fn m6_child_admission_first_projection_names_bot_run_format_5_or_6() {
    for (schema, accepted) in [(5u8, true), (6, true), (3, true), (4, false)] {
        let mut w = World::new();
        let parent = w.parent();
        let request = w.ask(&parent, &[BotId::mint()]);
        let mut base = w.base(parent.run);
        base.source_schema = schema.try_into().unwrap();
        let boot = w.boot;
        let outcome = w.db().request_child(&request, boot, &base);
        assert_eq!(outcome.is_ok(), accepted, "source schema {schema}");
        if accepted {
            let control = w
                .read()
                .next_decision_projection(parent.owner, parent.run)
                .unwrap()
                .unwrap();
            assert_eq!(u8::from(control.identity.source_schema), schema);
        } else {
            // The refused format left nothing: no child, no wait, no request.
            assert_eq!(w.count("bot_child_requests"), 0);
            assert_eq!(w.count("bot_wait_runtime"), 0);
            assert_eq!(w.count("bot_admissions"), 1);
        }
    }
    // After an applied projection the document is at 5 or 6: a later Control
    // of a format 6 Run still names 6.
    let mut w = World::new();
    let parent = w.parent();
    let request = w.ask(&parent, &[BotId::mint()]);
    let mut base = w.base(parent.run);
    base.source_schema = 6u8.try_into().unwrap();
    let boot = w.boot;
    w.db().request_child(&request, boot, &base).unwrap();
    w.publish(&parent);
    w.enroll(&parent);
    let ticket = w.ticket(&parent);
    let cleanup = w
        .db()
        .request_decision_cleanup(
            &ticket,
            DecisionTransitionId::mint(),
            DecisionClosureReason::Canceled,
        )
        .unwrap();
    assert_eq!(u8::from(cleanup.identity.source_schema), 6);
}

#[test]
fn m6_child_settle_a_stop_in_the_hold_is_cleaned_up_and_the_wait_settles() {
    let mut w = World::new();
    let parent = w.parent();
    let asked = w.admit(&parent, &[BotId::mint(), BotId::mint()]);
    w.publish(&parent);
    w.enroll(&parent);
    // Stop: the cleanup request settles the requests and owes one Control.
    let ticket = w.ticket(&parent);
    let transition = DecisionTransitionId::mint();
    let requested = w
        .db()
        .request_decision_cleanup(&ticket, transition, DecisionClosureReason::Canceled)
        .unwrap();
    // An exact retry replays the retained request, whatever happened since.
    assert_eq!(
        w.db()
            .request_decision_cleanup(&ticket, transition, DecisionClosureReason::Canceled)
            .unwrap(),
        requested
    );
    assert_eq!(w.requests(&parent).wait.unwrap().open_requests, 0);
    // The settled wait cannot be given a successor, and its Run stays
    // occupied until the origin's end is proven.
    assert_eq!(w.capacity(&parent), BotExecutionCapacity::Occupied);
    let release = w.release(&parent);
    let ticket = w.ticket(&parent);
    w.db().record_child_origin_stop(&ticket, &release).unwrap();
    // The final cleanup evidence names the origin: no successor ever existed.
    let now = w.now();
    let observed = w
        .read()
        .decision_wait_execution(parent.owner, parent.run)
        .unwrap()
        .unwrap();
    assert_eq!(observed.wait.reason, BotWaitReason::Child {});
    assert!(matches!(
        observed.cleanup_target,
        DecisionCleanupTarget::Origin { .. }
    ));
    let attempt = w.read().current_attempt(parent.run).unwrap().unwrap();
    let cleanup = ConsumerCleanup::Confirmed {
        cleanup_revision: observed.wait.revision,
        creation_retirement: ObservedCreationRetirement {
            creation_key: observed.cleanup_target.creation_key(),
            host: attempt.host_instance.unwrap(),
            result: ObservedRetirementResult::KnownEnded {
                session: parent.session,
            },
            observed_at_ms: now,
        },
        target: observed.cleanup_target.clone(),
        attempt_retirement: DecisionAttemptRetirement {
            run: parent.run,
            attempt: attempt.attempt,
            generation: attempt.generation,
            retired_at_ms: attempt.updated_at_ms,
        },
        native_drain: DecisionNativeDrain {
            run: parent.run,
            attempt: attempt.attempt,
            generation: attempt.generation,
            observed_at_ms: now,
            effects: DecisionList::default(),
        },
        retained_effect_outcome: None,
    };
    let cleaned = w
        .db()
        .record_nondecision_cleanup(
            &observed.expected,
            transition,
            DecisionTransitionId::mint(),
            &cleanup,
        )
        .unwrap();
    assert_eq!(cleaned.phase, BotWaitPhase::CleanupPending);
    assert!(cleaned.cleanup.is_some());
    // No new cleanup request after the final cleanup, for every wait (O152).
    let ticket = w.ticket(&parent);
    assert_eq!(
        w.db()
            .request_decision_cleanup(
                &ticket,
                DecisionTransitionId::mint(),
                DecisionClosureReason::Canceled
            )
            .map(|_| ()),
        Err(BotLedgerError::Stale)
    );
    // The Stop Control ends the Run as cancelled; once it is applied the
    // wait settles and the parent's admission can be settled.
    let acknowledgment = w.publish(&parent);
    assert_eq!(
        w.db().settle_bot(parent.run, BotSettlement::Settled),
        Err(BotLedgerError::Fenced),
        "an admission is never settled while its wait is open"
    );
    let ticket = w.ticket(&parent);
    let settled = w
        .db()
        .settle_decision_wait(&ticket, &acknowledgment)
        .unwrap();
    assert_eq!(settled.phase, BotWaitPhase::Settled);
    w.db()
        .settle_bot(parent.run, BotSettlement::Settled)
        .unwrap();
    // The children were only settled as requests: their Runs are untouched
    // and still end by themselves.
    for receipt in &asked.receipts {
        assert_eq!(
            w.admitted(receipt.child_run).state,
            BotAdmissionState::Queued
        );
        w.settle_child(receipt);
    }
    let after = w.requests(&parent);
    assert!(after.wait.is_none());
    assert!(
        after
            .requests
            .iter()
            .all(|receipt| receipt.settled_by == Some(ChildSettledBy::ParentStopped))
    );
}

fn receipt_for(
    w: &World,
    parent: &BotExecutionFence,
    attempt: RunAttemptId,
    generation: AttemptGeneration,
) -> BotDeliveryReceipt {
    let provenance = w.admitted(parent.run).submission.provenance;
    BotDeliveryReceipt {
        run: parent.run,
        attempt,
        generation,
        chat_id: provenance.chat_id,
        ai_attempt_id: provenance.ai_attempt_id.unwrap(),
        assistant_message_id: provenance.assistant_message_id.unwrap(),
        confirmed_at_ms: w.now(),
    }
}

#[test]
fn m6_child_resume_rows_the_successor_delivers_the_runs_one_answer() {
    let mut w = World::new();
    let parent = w.parent();
    let asked = w.admit(&parent, &[BotId::mint()]);
    w.publish(&parent);
    w.retire(&parent);
    w.settle_children(&asked);
    let bound = w.successor(&parent);
    let (attempt, generation, session, _) =
        bound.resume.clone().unwrap().consumer_identity().unwrap();
    let delivered = receipt_for(&w, &parent, attempt, generation);
    let now = w.now();
    // Before its input is acknowledged the successor delivers nothing.
    assert!(
        w.db()
            .record_decision_delivery(
                &delivered,
                &bound.expected,
                DecisionTransitionId::mint(),
                DecisionDeliveryOutcome::Completed,
                TimelineEntryId::mint(),
                1,
                now,
            )
            .is_err()
    );
    let turn = AgentTurnId::mint();
    w.acknowledge(&parent, turn);
    w.publish(&parent);
    let execution = w.execution(&parent);
    // The stopped origin's receipt is refused once a successor is recorded.
    let origin = receipt_for(&w, &parent, parent.attempt, parent.generation);
    assert!(
        w.db()
            .record_decision_delivery(
                &origin,
                &execution.expected,
                DecisionTransitionId::mint(),
                DecisionDeliveryOutcome::Retained,
                TimelineEntryId::mint(),
                0,
                now,
            )
            .is_err()
    );
    let transition = DecisionTransitionId::mint();
    let step = TimelineEntryId::mint();
    let (event, record) = w
        .db()
        .record_decision_delivery(
            &delivered,
            &execution.expected,
            transition,
            DecisionDeliveryOutcome::Completed,
            step,
            1,
            now,
        )
        .unwrap();
    let RunProjectionTargetV1::Control {
        fence,
        disposition,
        reason,
        effects,
        ..
    } = &record.target
    else {
        panic!("a Control")
    };
    assert_eq!(
        *fence,
        DecisionFence {
            attempt,
            generation,
            session: session.unwrap(),
            turn,
            ..execution.wait.origin
        }
    );
    assert!(disposition.is_none());
    assert_eq!(*reason, BotWaitReason::Child {});
    assert_eq!(effects.status, RunStatus::Completed);
    // One delivery: an exact retry replays it, and the receipt is retained.
    assert_eq!(
        w.db()
            .record_decision_delivery(
                &delivered,
                &execution.expected,
                transition,
                DecisionDeliveryOutcome::Completed,
                step,
                1,
                now,
            )
            .unwrap()
            .0,
        event
    );
    assert_eq!(w.admitted(parent.run).delivery, Some(delivered));
    let acknowledgment = w.publish(&parent);
    let ticket = w.ticket(&parent);
    assert_eq!(
        w.db()
            .settle_decision_wait(&ticket, &acknowledgment)
            .unwrap()
            .phase,
        BotWaitPhase::Settled
    );
}

#[test]
fn m6_child_settle_the_origins_own_delivery_settles_every_request() {
    // The origin's turn completed naturally while it waited (O127): its
    // receipt is retained and no request may stay open.
    let mut w = World::new();
    let parent = w.parent();
    let asked = w.admit(&parent, &[BotId::mint(), BotId::mint()]);
    w.publish(&parent);
    w.enroll(&parent);
    let ticket = w.ticket(&parent);
    let receipt = receipt_for(&w, &parent, parent.attempt, parent.generation);
    let now = w.now();
    w.db()
        .record_decision_delivery(
            &receipt,
            &ticket,
            DecisionTransitionId::mint(),
            DecisionDeliveryOutcome::Retained,
            TimelineEntryId::mint(),
            0,
            now,
        )
        .unwrap();
    let after = w.requests(&parent);
    assert_eq!(after.wait.unwrap().open_requests, 0);
    assert_eq!(after.wait.unwrap().phase, BotWaitPhase::HoldRequested);
    assert!(
        after
            .requests
            .iter()
            .all(|receipt| receipt.settled_by == Some(ChildSettledBy::ParentStopped))
    );
    // The cleanup that follows names the cause: the AI turn ended.
    let ticket = w.ticket(&parent);
    w.db()
        .request_decision_cleanup(
            &ticket,
            DecisionTransitionId::mint(),
            DecisionClosureReason::Canceled,
        )
        .unwrap();
    let observation = w
        .read()
        .read_bot_child_wait(parent.owner, parent.bot, parent.run, ticket.wait)
        .unwrap()
        .unwrap();
    assert_eq!(
        observation.disposition,
        ChildWaitDisposition::Closed {
            cause: DecisionClosureReason::AiTerminal
        }
    );
    assert_eq!(
        observation.fresh_stop_eligibility,
        FreshWaitStopEligibility::Terminal
    );
    // A child ending afterwards changes nothing.
    w.settle_child(&asked.receipts[0]);
    assert_eq!(
        w.requests(&parent).requests[0].settled_by,
        Some(ChildSettledBy::ParentStopped)
    );
}

#[test]
fn m6_child_resume_rows_a_routine_parent_keeps_its_routine_and_needs_its_proof() {
    let mut w = World::new();
    let parent = w.parent();
    let mut submission = w.admitted(parent.run).submission;
    let route = crate::bots::routines::tests::convert(w.db(), &mut submission, true);
    let routine_policy = RoutinePolicyBarrier {
        owner: parent.owner,
        bot: parent.bot,
        schedule: route.schedule,
        request: BotRequestId::mint().into(),
        grant_revision: 1,
        policy: submission.policy.clone(),
        effects_allowed: true,
    };
    w.db().install_routine_policy(&routine_policy).unwrap();
    w.db()
        .apply_bot_barrier(&BotAdmissionBarrier {
            owner: parent.owner,
            bot: parent.bot,
            request: BotRequestId::mint(),
            epoch: 1,
            paused: false,
        })
        .unwrap();
    assert_eq!(w.admitted(parent.run).submission_version, 2);
    // A Routine Run may ask. Its child is ordinary interactive work.
    let request = w.ask(&parent, &[BotId::mint()]);
    assert_eq!(request.requests[0].binding.parent_submission, 2);
    assert!(request.requests[0].binding.parent_routine.is_some());
    let ChildRequestOutcome::Created(asked) = w.send(&request) else {
        panic!("a Routine parent's ask is admitted")
    };
    let child = w.admitted(asked.receipts[0].child_run);
    assert_eq!(child.submission_version, 1);
    assert!(child.submission.provenance.routine.is_none());
    w.publish(&parent);
    w.retire(&parent);
    w.settle_children(&asked);
    // The reservation runs the Routine check: without the Routine's current
    // proof it is a definitive refusal, never a second attempt.
    let ticket = w.ticket(&parent);
    let host = fixtures::host(77);
    assert_eq!(
        w.db()
            .reserve_child_successor(&ticket, host, [42; 32], None)
            .map(|_| ()),
        Err(BotLedgerError::Fenced)
    );
    assert_eq!(w.read().attempts(parent.run).unwrap().len(), 1);
    let proof = FreshBotPolicyProof {
        global: w.policy(&parent),
        routine: Some(routine_policy),
    };
    let ChildSuccessorReservation::Reserved(reserved) = w
        .db()
        .reserve_child_successor(&ticket, host, [42; 32], Some(&proof))
        .unwrap()
    else {
        panic!("a reservation")
    };
    // Generation 2 of the same Routine Run; the admission is untouched.
    assert_eq!(
        reserved.current_attempt.generation,
        parent.generation.next()
    );
    let after = w.admitted(parent.run);
    assert_eq!(after.submission_version, 2);
    assert_eq!(
        after.submission.provenance.routine,
        submission.provenance.routine
    );
    assert_eq!(after.state, BotAdmissionState::Executing);
    assert_eq!(w.read().attempts(parent.run).unwrap().len(), 2);
}

#[test]
fn m6_child_resume_rows_a_paused_routine_parent_is_held_not_refused() {
    // Pausing the parent Bot holds the resume with no limit (decision 14).
    // For a Routine parent the pause is read before the Routine check, which
    // would otherwise turn it into a definitive refusal and end the wait.
    let mut w = World::new();
    let parent = w.parent();
    let mut submission = w.admitted(parent.run).submission;
    let route = crate::bots::routines::tests::convert(w.db(), &mut submission, true);
    let routine_policy = RoutinePolicyBarrier {
        owner: parent.owner,
        bot: parent.bot,
        schedule: route.schedule,
        request: BotRequestId::mint().into(),
        grant_revision: 1,
        policy: submission.policy.clone(),
        effects_allowed: true,
    };
    w.db().install_routine_policy(&routine_policy).unwrap();
    let asked = w.admit(&parent, &[BotId::mint()]);
    w.publish(&parent);
    w.retire(&parent);
    w.settle_children(&asked);
    w.db()
        .apply_bot_barrier(&BotAdmissionBarrier {
            owner: parent.owner,
            bot: parent.bot,
            request: BotRequestId::mint(),
            epoch: 1,
            paused: true,
        })
        .unwrap();
    let ticket = w.ticket(&parent);
    let proof = FreshBotPolicyProof {
        global: w.policy(&parent),
        routine: Some(routine_policy),
    };
    for offered in [None, Some(&proof)] {
        assert!(matches!(
            w.db()
                .reserve_child_successor(&ticket, fixtures::host(77), [42; 32], offered),
            Ok(ChildSuccessorReservation::Paused)
        ));
    }
    assert_eq!(w.read().attempts(parent.run).unwrap().len(), 1);
    assert_eq!(w.execution(&parent).wait.phase, BotWaitPhase::Retired);
}

#[test]
fn m6_child_occupancy_child_waits_are_not_counted_as_cleanup_waits() {
    // Three open child waits beside a Run that needs a cleanup-only wait:
    // the two-wait cleanup limit counts none of them.
    let mut w = World::new();
    for _ in 0..3 {
        let parent = w.parent();
        w.admit(&parent, &[BotId::mint()]);
        w.retire(&parent);
    }
    let stopping = w.parent();
    let origin = DecisionFence {
        owner: stopping.owner,
        bot: stopping.bot,
        run: stopping.run,
        attempt: stopping.attempt,
        generation: stopping.generation,
        session: stopping.session,
        turn: stopping.turn.unwrap(),
    };
    let observed = StreamSequence::new(7).unwrap();
    let mut base = w.base(stopping.run);
    base.bookkeeping = Some(DecisionSourceBookkeeping {
        boundary: DecisionSourceBoundary {
            fence: origin,
            producer_epoch: uuid::Uuid::new_v4(),
            stream: DecisionSourceStream::Session,
            sequence: observed,
            resulting: DecisionSourceFrontiers {
                agent_after: None,
                session_after: Some(observed),
            },
        },
        unflushed: DecisionList::default(),
        usage: None,
        allocated_steps: DecisionList::default(),
    });
    let wait = w
        .db()
        .begin_bot_wait_cleanup(
            &origin,
            BotWaitReason::Capacity {},
            observed,
            DecisionTransitionId::mint(),
            &base,
        )
        .unwrap();
    assert_eq!(wait.reason, BotWaitReason::Capacity {});
    assert_eq!(w.count("bot_wait_runtime"), 4);
    // The inventory reads all four and is not corrupt: one cleanup wait,
    // three child waits, one occupied Run.
    let inventory = w
        .read()
        .bot_execution_inventory(stopping.owner.device)
        .unwrap();
    assert_eq!((inventory.owned.len(), inventory.occupied), (4, 1));
}
