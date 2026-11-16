// SPDX-License-Identifier: MPL-2.0

//! Capacity/expired/overlap cleanup is an occupied Run obligation, never a
//! fifty-first decision or a fabricated AI enrollment receipt.
use super::*;

/// A prompt fallback on a Run that already owns a decision wait cancels fresh
/// authorization of that same decision. Only an unsettled decision whose native
/// prompt expired is an expiry; every other case is an authority change.
fn fallback_cause(reason: BotWaitReason, open: bool) -> DecisionClosureReason {
    if reason == (BotWaitReason::NativeExpired {}) && open {
        DecisionClosureReason::Expired
    } else {
        DecisionClosureReason::AuthorityChanged
    }
}

/// Exact retry of a fallback that strengthened an existing decision's cleanup.
/// Everything compared is retained and immutable: the request target and its
/// disposition at that time, and the physical subject the caller named, which
/// is the recorded consumer when a resume exists and the origin otherwise.
fn replayed_decision_request(
    conn: &Connection,
    fence: &DecisionFence,
    reason: BotWaitReason,
    wait: &DecisionWaitRecord,
    decision: DecisionId,
    prior: &DecisionProjectionRecord,
) -> Result<(), BotLedgerError> {
    let RunProjectionTargetV1::Control {
        action: DecisionControlAction::CleanupRequested { cause, .. },
        disposition: Some(disposition),
        fence: retained,
        ..
    } = &prior.target
    else {
        return Err(BotLedgerError::Conflict);
    };
    let open = matches!(disposition, DecisionDisposition::Open { .. });
    require(
        prior
            .identity
            .decision
            .as_ref()
            .is_some_and(|d| d.decision == decision)
            && *retained == wait.origin
            && *cause == fallback_cause(reason, open)
            && fence.owner == wait.origin.owner
            && fence.bot == wait.origin.bot
            && fence.run == wait.origin.run,
    )?;
    let record = super::super::rows::read(conn, fence.owner, fence.run, decision)?
        .ok_or(BotLedgerError::Stale)?;
    let (attempt, generation, session) = match &record.resume {
        Some(resume) => {
            let (attempt, generation, session, _) = resume.consumer_identity();
            (attempt, generation, session)
        }
        None => (
            wait.origin.attempt,
            wait.origin.generation,
            Some(wait.origin.session),
        ),
    };
    require(
        fence.attempt == attempt
            && fence.generation == generation
            && Some(fence.session) == session,
    )
}

impl RunsDb {
    pub fn begin_bot_wait_cleanup(
        &mut self,
        fence: &DecisionFence,
        reason: BotWaitReason,
        observed: StreamSequence,
        transition: DecisionTransitionId,
        base: &DecisionPublicationBase,
    ) -> Result<DecisionWaitRecord, BotLedgerError> {
        valid(fence)?;
        let reason_name = match reason {
            BotWaitReason::Capacity {} => "capacity",
            BotWaitReason::NativeExpired {} => "native_expired",
            BotWaitReason::OverlappingPrompt {} => "overlapping_prompt",
            BotWaitReason::Decision { .. } => return Err(BotLedgerError::Invalid),
            // A child wait is born in the transaction that admits its ask
            // (`request_child`), never through this cleanup-only entry.
            BotWaitReason::Child {} => return Err(BotLedgerError::Invalid),
        };
        self.write_with_error(crate::WriteOp::RecordDecisionProjection,|tx,now|{
            authority::owner(tx,fence.owner,fence.run)?;
            if let Some(prior)=read(tx,fence.owner,fence.run,transition)? {
                let old=super::super::rows::by_wait(tx,fence.run,prior.identity.wait)?.ok_or(BotLedgerError::Stale)?;
                if let BotWaitReason::Decision {decision}=old.reason {
                    // The shared CleanupRequested writer retained this under the
                    // existing decision. Its owner and cause stay immutable; the
                    // retry is checked against that retained target, never
                    // against a no-decision Preserve or today's wait state.
                    replayed_decision_request(tx,fence,reason,&old,decision,&prior)?;
                    return Ok(old)
                }
                require(old.origin==*fence && old.reason==reason && old.observed_sequence==Some(observed.get())
                    && matches!(prior.target,RunProjectionTargetV1::Control {action:DecisionControlAction::Preserve {},..}))?;
                return Ok(old)
            }
            if let Some(current)=super::super::observation::execution(tx,fence.owner,fence.run)? {
                require(current.current_attempt.attempt==fence.attempt
                    && current.current_attempt.generation==fence.generation
                    && current.cleanup_target.session()==Some(fence.session))?;
                if matches!(current.wait.reason,BotWaitReason::Decision {..}) {
                    if current.wait.phase!=BotWaitPhase::CleanupPending {
                        let open=current.decision_state.as_ref().is_some_and(|s|s.phase!=DecisionPhase::Settled);
                        control::request_on(tx,&current.expected,transition,fallback_cause(reason,open),now)?;
                    }
                    return super::super::rows::by_wait(tx,fence.run,current.wait.wait)?.ok_or(BotLedgerError::Stale)
                }
                // Repeated overflow coalesces into the one retained obligation:
                // its immutable reason and first observed cursor are kept, and
                // the native session observations keep the additional prompt.
                require(current.wait.origin==*fence)?;
                return Ok(current.wait)
            }
            let admitted=crate::bots::routines::native_admission(tx,fence.run)?;
            require(admitted.submission.provenance.owner==fence.owner && admitted.submission.provenance.bot==fence.bot
                && admitted.state==BotAdmissionState::Executing)?;
            let current=crate::attempts::latest(tx,fence.run)?.ok_or(BotLedgerError::Fenced)?;
            require(current.attempt==fence.attempt && current.generation==fence.generation && !current.retired
                && current.phase==crate::AttemptPhase::Running && current.host_instance.is_some()
                && current.provider_session.and_then(|s|s.session)==Some(fence.session))?;
            let key=NativeResultKey{run:fence.run,attempt:fence.attempt,generation:fence.generation,turn:fence.turn,operation_id:uuid::Uuid::nil()};
            super::super::current::fresh_native_input_on(tx,&key,&admitted,&current)?;
            let bookkeeping=base.bookkeeping.as_ref().ok_or(BotLedgerError::Fenced)?;
            require(bookkeeping.boundary.fence==*fence && bookkeeping.boundary.resulting.session_after==Some(observed))?;
            let inventory=super::super::occupancy::inventory(tx,fence.owner.device)?;
            require(inventory.occupied<=crate::BOT_HOST_EXECUTION_LIMIT && inventory.owned.iter().any(|r|r.execution.run==fence.run && r.capacity==crate::BotExecutionCapacity::Occupied))?;
            // Two cleanup-only waits at most. A child wait (M6) is neither a
            // decision nor a cleanup wait and is counted with the decision cap.
            let count:i64=tx.query_row("SELECT count(*) FROM (SELECT wait_id FROM bot_wait_runtime INDEXED BY bot_wait_runtime_recovery WHERE device_id=?1 AND phase<>'settled' AND reason NOT IN ('decision','child') LIMIT 3)",[fence.owner.device.to_storage()],|r|r.get(0)).map_err(sql)?;
            if count>=2 {return Err(BotLedgerError::Capacity)}
            let wait=BotWaitId::mint();
            tx.execute("INSERT INTO bot_wait_runtime(wait_id,run_id,user_scope,device_id,bot_id,attempt_id,generation,session_id,turn_id,observed_sequence,reason,phase,revision,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'cleanup_pending',1,?12,?12)",params![wait.to_storage(),fence.run.to_storage(),fence.owner.user_scope.to_storage(),fence.owner.device.to_storage(),fence.bot.to_storage(),fence.attempt.to_storage(),fence.generation.get(),fence.session.to_string(),fence.turn.to_string(),observed.get(),reason_name,now]).map_err(sql)?;
            let record=super::super::rows::by_wait(tx,fence.run,wait)?.ok_or(BotLedgerError::Stale)?;
            let target=lifecycle::build_target(control::ControlContext {base:base.clone(),predecessor:base.predecessor.clone(),bookkeeping:base.bookkeeping.clone()},&record,None,DecisionControlAction::Preserve {},now)?;
            insert::insert_on(tx,fence.owner,transition,wait,base,&target,now)?;
            Ok(record)
        })
    }

    pub fn record_nondecision_cleanup(
        &mut self,
        expected: &DecisionWaitExpected,
        cleanup_projection: DecisionTransitionId,
        transition: DecisionTransitionId,
        cleanup: &ConsumerCleanup,
    ) -> Result<DecisionWaitRecord, BotLedgerError> {
        valid(cleanup)?;
        require(expected.decision.is_none())?;
        self.write_with_error(crate::WriteOp::RecordDecisionProjection,|tx,now|{
            authority::owner(tx,expected.owner,expected.run)?;
            let old=super::super::rows::by_wait(tx,expected.run,expected.wait)?.ok_or(BotLedgerError::Stale)?;
            require(old.origin.owner==expected.owner && !matches!(old.reason,BotWaitReason::Decision {..}))?;
            let owner=read(tx,expected.owner,expected.run,cleanup_projection)?.ok_or(BotLedgerError::Stale)?;
            require(owner.identity.wait==expected.wait)?;
            let origin=cleanup_owner::origin(tx,&owner)?.ok_or(BotLedgerError::Fenced)?;
            let canceled=matches!(origin,DecisionCleanupOrigin::Requested {cause:DecisionClosureReason::Canceled,..});
            let ConsumerCleanup::Confirmed {creation_retirement,retained_effect_outcome,..}=cleanup else {return Err(BotLedgerError::Invalid)};
            require(retained_effect_outcome.is_none())?;
            let action=if canceled {DecisionControlAction::Stop {}}else{DecisionControlAction::Interrupt {prior_attempt:match creation_retirement.result {ObservedRetirementResult::NoEvidence {}=>PriorAttempt::Unknown,_=>PriorAttempt::Ended}}};
            if let Some(retained)=&old.cleanup {
                require(retained==cleanup)?;
                let projected=read(tx,expected.owner,expected.run,transition)?.ok_or(BotLedgerError::Stale)?;
                require(projected.identity.wait==expected.wait && projected.identity.sequence==next(owner.identity.sequence)?
                    && matches!(&projected.target,RunProjectionTargetV1::Control {action:stored,..} if *stored==action))?;
                return Ok(old)
            }
            let current=cleanup_owner::current(tx,expected.owner,expected.run)?.ok_or(BotLedgerError::Stale)?;
            require(current.expected==*expected && current.projection==cleanup_projection && current.origin==origin)?;
            let observed=super::super::observation::execution(tx,expected.owner,expected.run)?.ok_or(BotLedgerError::Stale)?;
            super::super::write::validate_cleanup_on(tx,&observed,cleanup,now)?;
            let admitted=crate::bots::routines::native_admission(tx,expected.run)?;
            let elapsed=admitted.active_elapsed_at(now).min(i64::MAX as u64) as i64;
            tx.execute("UPDATE bot_admissions SET active_elapsed_ms=?2,active_since_ms=NULL,updated_at_ms=max(updated_at_ms,?3) WHERE run_id=?1",params![expected.run.to_storage(),elapsed,now]).map_err(sql)?;
            tx.execute("UPDATE bot_wait_runtime SET cleanup_evidence_json=?2,revision=?3,updated_at_ms=max(updated_at_ms,?4) WHERE wait_id=?1 AND revision=?5",params![expected.wait.to_storage(),encoded(cleanup,DECISION_EVIDENCE_BYTES)?,next(expected.revision)?.get(),now,expected.revision.get()]).map_err(sql)?;
            let result=super::super::rows::by_wait(tx,expected.run,expected.wait)?.ok_or(BotLedgerError::Stale)?;
            let context=control::context(tx,expected.owner,expected.run,expected.wait)?;
            let target=lifecycle::build_target(context.clone(),&result,None,action,now)?;
            insert::insert_on(tx,expected.owner,transition,expected.wait,&context.base,&target,now)?;
            Ok(result)
        })
    }
}
