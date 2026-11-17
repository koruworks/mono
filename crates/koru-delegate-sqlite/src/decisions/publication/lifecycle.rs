// SPDX-License-Identifier: MPL-2.0

//! Committing lifecycle owners. No callback can omit the resulting Control.
use super::super::write as mutation;
use super::*;

pub(super) fn commit_control(
    tx: &Transaction<'_>,
    transition: DecisionTransitionId,
    mutation: mutation::DecisionMutation,
    mut action: DecisionControlAction,
    now: i64,
) -> Result<DecisionRecord, BotLedgerError> {
    let record = mutation.record;
    let owner = record.wait.origin.owner;
    let run = record.wait.origin.run;
    if let Some(prior) = read(tx, owner, run, transition)? {
        require(
            mutation.outcome == crate::WriteOutcome::AlreadyApplied
                && prior.identity.wait == record.wait.wait
                && prior
                    .identity
                    .decision
                    .as_ref()
                    .is_some_and(|d| d.decision == record.receipt.decision),
        )?;
        return Ok(record);
    }
    require(mutation.outcome == crate::WriteOutcome::Applied)?;
    if matches!(action, DecisionControlAction::Preserve {})
        && record.wait.phase == BotWaitPhase::CleanupPending
        && record.wait.cleanup.is_none()
    {
        let latest = rows::latest(tx, run)?.ok_or(BotLedgerError::Stale)?;
        let latest = read(tx, owner, run, latest)?.ok_or(BotLedgerError::Stale)?;
        if let Some(origin) = cleanup_owner::origin(tx, &latest)? {
            action = cleanup_owner::action(&origin)
        } else {
            require(
                record.receipt.state.kind.resumes_by_successor()
                    && record.receipt.state.authorization == DecisionAuthorization::Denied,
            )?;
        }
    }
    let context = control::context(tx, owner, run, record.wait.wait)?;
    let target = build_target(
        context.clone(),
        &record.wait,
        Some(&record.receipt),
        action,
        now,
    )?;
    insert::insert_on(
        tx,
        owner,
        transition,
        record.wait.wait,
        &context.base,
        &target,
        now,
    )?;
    Ok(record)
}

pub(super) fn build_target(
    context: control::ControlContext,
    wait: &DecisionWaitRecord,
    receipt: Option<&DecisionReceipt>,
    action: DecisionControlAction,
    now: i64,
) -> Result<RunProjectionTargetV1, BotLedgerError> {
    let run = wait.origin.run;
    let fence = context
        .bookkeeping
        .as_ref()
        .map(|b| b.boundary.fence)
        .unwrap_or(wait.origin);
    let mut event = None;
    let mut steps = Vec::new();
    let mut publications = Vec::new();
    if !context.predecessor.status.is_terminal() {
        match action {
            DecisionControlAction::Preserve {} | DecisionControlAction::CleanupRequested { .. } => {
            }
            DecisionControlAction::DecisionResolved { outcome } => {
                event = Some(RunEvent::ApprovalResolved);
                // Koru has no native prompt/ApprovalId to invent. Its exact
                // disposition resolves attention without a native answer fact.
                if let Some(DecisionReceipt {
                    binding: DecisionBindingV1::NativeApproval(binding),
                    ..
                }) = receipt
                {
                    let step = TimelineEntry::new(
                        StepKind::ApprovalResolved { outcome },
                        TimelineRefs::new([
                            TimelineRef::Turn {
                                turn: binding.origin_turn,
                            },
                            TimelineRef::Approval {
                                approval: binding.prompt.clone(),
                            },
                        ])
                        .map_err(|_| BotLedgerError::Invalid)?,
                        now,
                    );
                    publications.push(DecisionProjectionPublication {
                        event: EventId::mint(),
                        step: Some(step.id),
                        fact: RunFact::ApprovalResolved {
                            run,
                            attempt: binding.origin_attempt,
                            turn: binding.origin_turn,
                            approval: binding.prompt.clone(),
                            outcome,
                        },
                    });
                    steps.push(step);
                }
            }
            DecisionControlAction::Stop {} => {
                event = Some(RunEvent::Cancel);
                let step = TimelineEntry::new(
                    StepKind::Cancelled {
                        reason: CancelReason::Run,
                    },
                    TimelineRefs::new([TimelineRef::Attempt {
                        attempt: fence.attempt,
                    }])
                    .map_err(|_| BotLedgerError::Invalid)?,
                    now,
                );
                publications.push(DecisionProjectionPublication {
                    event: EventId::mint(),
                    step: Some(step.id),
                    fact: RunFact::Cancelled {
                        run,
                        attempt: Some(fence.attempt),
                        generation: fence.generation,
                        reason: CancelReason::Run,
                    },
                });
                steps.push(step);
            }
            // The Run is interrupted already: the Source that reported its
            // process end said so before this cleanup was confirmed. A
            // second interrupt is not a transition the status has, and
            // refusing it here would keep the wait `cleanup_pending`, the
            // admission executing and the Bot busy for ever. The final
            // Control then records the cleanup and changes nothing else.
            DecisionControlAction::Interrupt { .. }
                if matches!(context.predecessor.status, RunStatus::Interrupted { .. }) => {}
            DecisionControlAction::Interrupt { prior_attempt } => {
                event = Some(RunEvent::Interrupt { prior_attempt });
                let step = TimelineEntry::new(
                    StepKind::Interrupted { prior_attempt },
                    TimelineRefs::new([TimelineRef::Attempt {
                        attempt: fence.attempt,
                    }])
                    .map_err(|_| BotLedgerError::Invalid)?,
                    now,
                );
                publications.push(DecisionProjectionPublication {
                    event: EventId::mint(),
                    step: Some(step.id),
                    fact: RunFact::Interrupted {
                        run,
                        attempt: fence.attempt,
                        generation: fence.generation,
                        prior_attempt,
                    },
                });
                steps.push(step);
            }
            _ => return Err(BotLedgerError::Invalid),
        }
    }
    let mut flush = Vec::new();
    if matches!(
        action,
        DecisionControlAction::Stop {} | DecisionControlAction::Interrupt { .. }
    ) {
        if let Some(bookkeeping) = &context.bookkeeping {
            for tally in bookkeeping
                .unflushed
                .as_slice()
                .iter()
                .filter(|t| t.tool_calls > 0)
            {
                let step = TimelineEntryId::mint();
                let event = EventId::mint();
                flush.push(DecisionControlFlush {
                    turn: tally.turn,
                    step,
                    event,
                    at_ms: now,
                });
                publications.push(DecisionProjectionPublication {
                    event,
                    step: Some(step),
                    fact: RunFact::ToolsCompleted {
                        run,
                        attempt: fence.attempt,
                        turn: tally.turn,
                        calls: tally.tool_calls,
                        failed: tally.tool_failures,
                    },
                });
            }
        }
    }
    let status = match event {
        Some(event) => transition_bot(&context.predecessor.status, &event)
            .map_err(|_| BotLedgerError::Fenced)?,
        None => context.predecessor.status.clone(),
    };
    let target = RunProjectionTargetV1::Control {
        version: DecisionVersionV1,
        predecessor: context.predecessor.clone(),
        effects: DecisionProjectionEffects {
            event,
            status,
            failure_reason: context.predecessor.failure_reason,
            economics_at_ms: None,
            steps: DecisionList::new(steps).map_err(|_| BotLedgerError::Invalid)?,
            models: DecisionList::default(),
            publications: DecisionList::new(publications).map_err(|_| BotLedgerError::Invalid)?,
        },
        fence,
        disposition: receipt.map(control::disposition).transpose()?,
        wait_phase: wait.phase,
        reason: wait.reason,
        action,
        retained_bookkeeping: context.bookkeeping,
        flush: DecisionList::new(flush).map_err(|_| BotLedgerError::Invalid)?,
        delivery: None,
    };
    Ok(target)
}

/// M6: the two Controls a child wait owes besides its cleanup. `ChildWaiting`
/// is the wait's first projection, composed over the document owner's `base`
/// in the transaction that admits the ask; it marks the Run as waiting and
/// writes one step. `Resume` clears the marker when the successor's input is
/// acknowledged, over the wait's own retained context. Neither carries a
/// decision, a fact or any content.
pub(crate) fn commit_child_control_on(
    tx: &Transaction<'_>,
    wait: &crate::ChildWaitRecord,
    transition: DecisionTransitionId,
    action: DecisionControlAction,
    base: Option<&DecisionPublicationBase>,
    now: i64,
) -> Result<DecisionProjectionRecord, BotLedgerError> {
    let owner = wait.origin.owner;
    let run = wait.origin.run;
    let current = super::super::rows::by_wait(tx, run, wait.wait)?.ok_or(BotLedgerError::Stale)?;
    require(current.reason == (BotWaitReason::Child {}))?;
    let context = match (&action, base) {
        (DecisionControlAction::ChildWaiting {}, Some(base)) => control::ControlContext {
            base: base.clone(),
            predecessor: base.predecessor.clone(),
            bookkeeping: base.bookkeeping.clone(),
        },
        (DecisionControlAction::Resume { .. }, None) => {
            control::context(tx, owner, run, wait.wait)?
        }
        _ => return Err(BotLedgerError::Invalid),
    };
    require(!context.predecessor.status.is_terminal())?;
    let fence = context
        .bookkeeping
        .as_ref()
        .map(|b| b.boundary.fence)
        .unwrap_or(wait.origin);
    let (event, kind) = match action {
        DecisionControlAction::ChildWaiting {} => {
            (RunEvent::WaitingForBot, StepKind::WaitingForBot)
        }
        DecisionControlAction::Resume { via } => (RunEvent::Resume, StepKind::Resumed { via }),
        _ => return Err(BotLedgerError::Invalid),
    };
    let status =
        transition_bot(&context.predecessor.status, &event).map_err(|_| BotLedgerError::Fenced)?;
    let step = TimelineEntry::new(
        kind,
        TimelineRefs::new([TimelineRef::Attempt {
            attempt: fence.attempt,
        }])
        .map_err(|_| BotLedgerError::Invalid)?,
        now,
    );
    let target = RunProjectionTargetV1::Control {
        version: DecisionVersionV1,
        predecessor: context.predecessor.clone(),
        effects: DecisionProjectionEffects {
            event: Some(event),
            status,
            failure_reason: context.predecessor.failure_reason,
            economics_at_ms: None,
            steps: DecisionList::new(vec![step]).map_err(|_| BotLedgerError::Invalid)?,
            models: DecisionList::default(),
            publications: DecisionList::default(),
        },
        fence,
        disposition: None,
        wait_phase: current.phase,
        reason: current.reason,
        action,
        retained_bookkeeping: context.bookkeeping.clone(),
        flush: DecisionList::default(),
        delivery: None,
    };
    insert::insert_on(
        tx,
        owner,
        transition,
        wait.wait,
        &context.base,
        &target,
        now,
    )
}

impl RunsDb {
    pub fn consume_decision_edit(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        request: &PrepareNativeResult,
        payload: &[u8],
        fence: &BotExecutionFence,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<crate::NativeLeaseOutcome, BotLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            let (mutation, outcome) =
                mutation::consume_on(tx, expected, request, payload, fence, proof, boot, now)?;
            // Consumed replay is read/reconcile-only (contract section 2): it
            // carries no Execute and records no new projection, whatever
            // transition the retrying caller minted. The first consume's
            // Control was committed with that consume.
            if mutation.outcome == crate::WriteOutcome::AlreadyApplied {
                return Ok(outcome);
            }
            commit_control(
                tx,
                transition,
                mutation,
                DecisionControlAction::Preserve {},
                now,
            )?;
            Ok(outcome)
        })
    }
    /// `ConsumePrivateDecision` (M5): the acknowledged consumer spends the
    /// one-use answer. Resolution of other dispatcher instances' rows commits
    /// first, in its own transaction. The consume, its effect row at
    /// `prepared` and its Control then commit together or not at all; the
    /// first fresh commit answers the one `Execute`, every replay
    /// `ReconcileOnly` with no new projection.
    pub fn consume_private_decision(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        dispatcher: PrivateDispatcherId,
        proposal: &PrivateOperationProposal,
        fence: &BotExecutionFence,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<crate::PrivateResolved<crate::PrivateEffectOutcome>, crate::PrivateLedgerError>
    {
        let resolved = self.resolve_private_others(expected.owner, dispatcher)?;
        let outcome = self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            let (mutation, outcome) = super::super::private::consume_on(
                tx, expected, dispatcher, proposal, fence, proof, boot, now,
            )?;
            if mutation.outcome == crate::WriteOutcome::AlreadyApplied {
                return Ok(outcome);
            }
            commit_control(
                tx,
                transition,
                mutation,
                DecisionControlAction::Preserve {},
                now,
            )?;
            Ok::<_, crate::PrivateLedgerError>(outcome)
        });
        Ok(crate::PrivateResolved { resolved, outcome })
    }
    pub fn settle_decision_effect(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        outcome: &DecisionRetainedEffect,
    ) -> Result<DecisionRecord, BotLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            let result = mutation::settle_effect_on(tx, expected, outcome, now)?;
            // A settled replay is retained truth (a lost reply, or a second
            // caller that recorded the same save): it records no projection,
            // whatever transition the retrying caller minted. The first
            // settlement's Control was committed with that settlement.
            if result.outcome == crate::WriteOutcome::AlreadyApplied {
                return Ok(result.record);
            }
            commit_control(
                tx,
                transition,
                result,
                DecisionControlAction::DecisionResolved {
                    outcome: ApprovalOutcome::Approved,
                },
                now,
            )
        })
    }
    pub fn record_decision_consumer_cleanup(
        &mut self,
        expected: &DecisionWaitExpected,
        cleanup_projection: DecisionTransitionId,
        transition: DecisionTransitionId,
        observation: &ConsumerCleanup,
    ) -> Result<DecisionRecord, BotLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection,|tx,now|{
            let requested=read(tx,expected.owner,expected.run,cleanup_projection)?.ok_or(BotLedgerError::Stale)?;
            require(requested.identity.wait==expected.wait)?;
            let expected_decision=expected.decision.as_ref().ok_or(BotLedgerError::Invalid)?;
            require(requested.identity.decision.as_ref().is_some_and(|d|d.decision==expected_decision.decision))?;
            let offered_origin=cleanup_owner::origin(tx,&requested)?.ok_or(BotLedgerError::Fenced)?;
            let (cause,minimum_cleanup_revision)=match Some(offered_origin.clone()) {
                Some(DecisionCleanupOrigin::Requested {cause,expected_wait_revision,..})=>(cause,next(expected_wait_revision)?),
                Some(DecisionCleanupOrigin::Denied {decision,..}) if decision==expected_decision.decision=>(DecisionClosureReason::Denied,DecisionRevision::new(1).map_err(|_|BotLedgerError::Invalid)?),
                _=>return Err(BotLedgerError::Conflict),
            };
            let ConsumerCleanup::Confirmed {cleanup_revision,creation_retirement,retained_effect_outcome,..}=observation else {return Err(BotLedgerError::Invalid)};
            require(*cleanup_revision>=minimum_cleanup_revision)?;
            let action=if cause==DecisionClosureReason::Canceled {DecisionControlAction::Stop {}} else {
                DecisionControlAction::Interrupt {prior_attempt:match creation_retirement.result {
                    ObservedRetirementResult::NoEvidence {}=>PriorAttempt::Unknown,
                    _=>PriorAttempt::Ended,
                }}
            };
            if let Some(retained)=read(tx,expected.owner,expected.run,transition)? {
                require(retained.identity.wait==expected.wait && retained.identity.sequence==next(requested.identity.sequence)?)?;
                let RunProjectionTargetV1::Control {action:stored,..}=&retained.target else {return Err(BotLedgerError::Conflict)};
                require(*stored==action)?;
            }else{
                let current=cleanup_owner::current(tx,expected.owner,expected.run)?.ok_or(BotLedgerError::Stale)?;
                require(current.expected==*expected && current.projection==cleanup_projection && current.origin==offered_origin)?;
            }
            let result=mutation::consumer_cleanup_on(tx,expected,observation,now)?;
            if result.outcome==crate::WriteOutcome::AlreadyApplied {return commit_control(tx,transition,result,action,now)}
            let mut record=result.record;
            if record.receipt.state.phase!=DecisionPhase::Settled {
                require(record.receipt.state.phase==DecisionPhase::StopPending)?;
                let reason=retained_effect_outcome.as_ref().map_or(cause,DecisionRetainedEffect::closure_reason);
                tx.execute("UPDATE bot_decisions SET phase='settled',revision=?2,closure_reason=?3,settled_at_ms=?4,updated_at_ms=?4 WHERE decision_id=?1 AND revision=?5",
                    params![record.receipt.decision.to_storage(),next(record.receipt.state.revision)?.get(),token(reason)?,now.max(record.receipt.updated_at_ms),record.receipt.state.revision.get()]).map_err(sql)?;
                record=super::super::rows::read(tx,expected.owner,expected.run,record.receipt.decision)?.ok_or(BotLedgerError::Stale)?;
            }
            commit_control(tx,transition,mutation::DecisionMutation {record,outcome:crate::WriteOutcome::Applied},action,now)
        })
    }
    pub fn answer_bot_decision(
        &mut self,
        input: &AnswerDecision,
        transition: DecisionTransitionId,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<DecisionRecord, BotLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            let result = mutation::answer_on(tx, input, proof, boot, now)?;
            commit_control(
                tx,
                transition,
                result,
                DecisionControlAction::Preserve {},
                now,
            )
        })
    }
    pub fn record_decision_native_answer(
        &mut self,
        owner: BotOwner,
        run: RunId,
        transition: DecisionTransitionId,
        response: &DecisionNativeResponse,
    ) -> Result<DecisionRecord, BotLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            let result = mutation::record_native_answer_on(tx, owner, run, response, now)?;
            let action = if result.record.receipt.state.phase == DecisionPhase::Settled
                && matches!(response, DecisionNativeResponse::Acknowledged { .. })
            {
                DecisionControlAction::DecisionResolved {
                    outcome: if result.record.receipt.state.authorization
                        == DecisionAuthorization::Consumed
                    {
                        ApprovalOutcome::Approved
                    } else {
                        ApprovalOutcome::Rejected
                    },
                }
            } else {
                DecisionControlAction::Preserve {}
            };
            commit_control(tx, transition, result, action, now)
        })
    }
    pub fn enroll_decision_cleanup(
        &mut self,
        expected: &DecisionWaitExpected,
        transition: DecisionTransitionId,
    ) -> Result<DecisionRecord, BotLedgerError> {
        let decision = expected.decision.as_ref().ok_or(BotLedgerError::Invalid)?;
        require(decision.owner == expected.owner && decision.run == expected.run)?;
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            let result = mutation::enroll_on(
                tx,
                expected.owner,
                expected.run,
                decision.decision,
                decision.revision,
                expected.revision,
                now,
            )?;
            require(
                result.record.wait.wait == expected.wait
                    && result.record.receipt.binding_digest == decision.binding_digest,
            )?;
            commit_control(
                tx,
                transition,
                result,
                DecisionControlAction::Preserve {},
                now,
            )
        })
    }
    pub fn record_decision_enrollment(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        reply: &BotWaitEnrollmentReply,
    ) -> Result<DecisionRecord, BotLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            let result = mutation::record_enrollment_on(
                tx,
                expected.owner,
                expected.run,
                expected.decision,
                reply,
                now,
            )?;
            require(result.record.receipt.binding_digest == expected.binding_digest)?;
            commit_control(
                tx,
                transition,
                result,
                DecisionControlAction::Preserve {},
                now,
            )
        })
    }
    pub fn record_decision_origin_stop(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        release: &OriginWaitStop,
    ) -> Result<DecisionRecord, BotLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            let result = mutation::origin_stop_on(tx, expected, release, now)?;
            commit_control(
                tx,
                transition,
                result,
                DecisionControlAction::Preserve {},
                now,
            )
        })
    }
    pub fn reserve_decision_successor(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        host: HostInstanceId,
        fingerprint: [u8; 32],
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<DecisionRecord, BotLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            let result =
                mutation::reserve_successor_on(tx, expected, host, fingerprint, proof, boot, now)?;
            commit_control(
                tx,
                transition,
                result,
                DecisionControlAction::Preserve {},
                now,
            )
        })
    }
    pub fn bind_decision_successor(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        creation: IdempotencyKey,
        host: HostInstanceId,
        session: SessionId,
    ) -> Result<DecisionRecord, BotLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            let result =
                mutation::bind_resume_session_on(tx, expected, creation, host, session, now)?;
            commit_control(
                tx,
                transition,
                result,
                DecisionControlAction::Preserve {},
                now,
            )
        })
    }
    pub fn record_decision_resume_input(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        input: IdempotencyKey,
        fingerprint: [u8; 32],
        acknowledgment: Option<&DecisionInputAcknowledgment>,
    ) -> Result<DecisionRecord, BotLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection, |tx, now| {
            let result = mutation::record_resume_input_on(
                tx,
                expected,
                input,
                fingerprint,
                acknowledgment,
                now,
            )?;
            commit_control(
                tx,
                transition,
                result,
                DecisionControlAction::Preserve {},
                now,
            )
        })
    }
}
