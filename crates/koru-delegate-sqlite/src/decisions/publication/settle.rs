// SPDX-License-Identifier: MPL-2.0

use super::*;

/// A canonical status a confirmed physical cleanup may finish with. This is
/// never the normal live/positive-ended alternative, which excludes Interrupted.
fn cleanup_outcome(status: &RunStatus) -> bool {
    status.is_terminal() || matches!(status, RunStatus::Interrupted { .. })
}

impl RunsDb {
    /// Internal returned-handle owner only: D has installed the actual returned
    /// sole handle, verified its durable exact marker and cleared its ticket,
    /// with no staged Source retry. SQL independently proves all retained debt.
    /// Failure keeps D exclusive; a row absence is never proof of handle return.
    pub fn settle_decision_wait(
        &mut self,
        expected: &DecisionWaitExpected,
        ack: &DecisionProjectionAcknowledgment,
    ) -> Result<DecisionWaitRecord, BotLedgerError> {
        self.write_with_error(crate::WriteOp::RecordDecisionProjection,|tx,now|{
            authority::owner(tx,expected.owner,expected.run)?;
            require(ack.identity.run==expected.run && ack.identity.wait==expected.wait)?;
            let applied=write::required(tx,expected.owner,&ack.identity)?;
            let DecisionProjectionState::Applied {prepared,observed_version,..}=&applied.state else {return Err(BotLedgerError::Fenced)};
            require(prepared.delta_digest==ack.delta_digest && observed_version==&ack.observed_version)?;
            let wait=super::super::rows::by_wait(tx,expected.run,expected.wait)?.ok_or(BotLedgerError::Stale)?;
            require(wait.origin.owner==expected.owner)?;
            let mut cleanup_required=true;
            match (&wait.reason,&expected.decision,&ack.identity.decision) {
                (BotWaitReason::Decision {decision},Some(wanted),Some(projected))=>{
                    require(wanted.owner==expected.owner && wanted.run==expected.run && wanted.decision==*decision
                        && projected.decision==*decision && projected.revision==wanted.revision)?;
                    let record=super::super::rows::read(tx,expected.owner,expected.run,*decision)?.ok_or(BotLedgerError::Stale)?;
                    require(record.receipt.binding_digest==wanted.binding_digest && record.receipt.state.revision==wanted.revision
                        && record.receipt.state.phase==DecisionPhase::Settled)?;
                    if let RunProjectionTargetV1::Control {disposition,..}=&applied.target {
                        require(disposition.as_ref()==Some(&control::disposition(&record.receipt)?))?;
                    }
                    cleanup_required=record.wait.cleanup.is_some() || record.wait.phase==BotWaitPhase::CleanupPending
                        || match record.receipt.state.kind {
                            DecisionKind::NativeApproval=>!matches!(record.native_response,Some(DecisionNativeResponse::Acknowledged {..})),
                            DecisionKind::KoruEdit|DecisionKind::PrivateOperation=>!matches!(record.receipt.closure.as_ref().map(|c|c.reason),Some(DecisionClosureReason::Completed|DecisionClosureReason::EffectNotApplied)),
                        };
                }
                (BotWaitReason::Capacity {}|BotWaitReason::NativeExpired {}|BotWaitReason::OverlappingPrompt {},None,None)=>{}
                // M6: a child wait has no decision on either side. It settles
                // when its cleanup is confirmed, or, with no request open,
                // when its successor's continuation input was acknowledged.
                (BotWaitReason::Child {},None,None)=>{
                    let child=super::super::rows::child::wait(tx,expected.run,expected.wait)?.ok_or(BotLedgerError::Stale)?;
                    cleanup_required=wait.cleanup.is_some() || wait.phase==BotWaitPhase::CleanupPending;
                    if !cleanup_required && wait.phase!=BotWaitPhase::Settled {
                        let open:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bot_child_requests WHERE wait_id=?1 AND state='open')",[expected.wait.to_storage()],|r|r.get(0)).map_err(sql)?;
                        let acknowledged=super::super::rows::child::resume(tx,&child)?.is_some_and(|resume|resume.accepted_turn().is_some());
                        require(!open && acknowledged && wait.phase==BotWaitPhase::ResumeReserved)?;
                    }
                }
                _=>return Err(BotLedgerError::Conflict),
            }
            if cleanup_required {
                // Confirmed cleanup ends as Cancelled (Stop) or as the approved
                // Needed-you Interrupted outcome, which is deliberately not
                // `is_terminal()`; an earlier absorbing terminal status also
                // stands. The normal positive-ended path below stays separate.
                require(matches!(wait.cleanup,Some(ConsumerCleanup::Confirmed {..})) && cleanup_outcome(&applied.target.effects().status))?;
            }
            if wait.phase==BotWaitPhase::Settled {
                require(wait.revision==next(expected.revision)?)?;
                // Indexed immediate successor is enough: settled waits cannot
                // accept another target, and a new wait starts the next prefix.
                let following:Option<String>=tx.query_row("SELECT CASE WHEN typeof(wait_id)='text' AND length(CAST(wait_id AS BLOB))=36 THEN wait_id END FROM bot_decision_publications WHERE run_id=?1 AND run_sequence=?2",params![expected.run.to_storage(),next(ack.identity.sequence)?.get()],|r|r.get(0)).optional().map_err(sql)?;
                require(following.is_none_or(|id|id!=expected.wait.to_storage()))?;
                return Ok(wait)
            }
            let current=super::super::observation::execution(tx,expected.owner,expected.run)?.ok_or(BotLedgerError::Stale)?;
            require(current.expected==*expected && rows::open(tx,expected.run)?.is_empty()
                && rows::latest(tx,expected.run)?==Some(ack.identity.transition))?;
            if cleanup_required {
                require(current.current_attempt.retired && current.current_attempt.phase.is_terminal())?;
            }else{
                let live=!current.current_attempt.retired && !current.current_attempt.phase.is_terminal();
                let ended=current.current_attempt.retired && current.current_attempt.phase==crate::AttemptPhase::Ended
                    && applied.target.effects().status.is_terminal()
                    && applied.target.fence().attempt==current.current_attempt.attempt
                    && applied.target.fence().generation==current.current_attempt.generation
                    && Some(applied.target.fence().session)==current.cleanup_target.session();
                require(live || ended)?;
            }
            let held:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM native_result_intents WHERE run_id=?1 AND lease_state='held')",[expected.run.to_storage()],|r|r.get(0)).map_err(sql)?;
            require(!held)?;
            let changed=tx.execute("UPDATE bot_wait_runtime SET phase='settled',revision=?2,settled_at_ms=?3,updated_at_ms=?3 WHERE wait_id=?1 AND revision=?4 AND phase<>'settled'",params![expected.wait.to_storage(),next(expected.revision)?.get(),now.max(wait.updated_at_ms),expected.revision.get()]).map_err(sql)?;
            require(changed==1)?;
            super::super::rows::by_wait(tx,expected.run,expected.wait)?.ok_or(BotLedgerError::Stale)
        })
    }
}
