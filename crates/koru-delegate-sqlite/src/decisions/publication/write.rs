// SPDX-License-Identifier: MPL-2.0

use super::*;

pub(super) fn required(
    conn: &Connection,
    owner: BotOwner,
    identity: &DecisionProjectionIdentity,
) -> Result<DecisionProjectionRecord, BotLedgerError> {
    let old = read(conn, owner, identity.run, identity.transition)?.ok_or(BotLedgerError::Stale)?;
    require(old.identity == *identity)?;
    Ok(old)
}

fn first_open(
    conn: &Connection,
    identity: &DecisionProjectionIdentity,
) -> Result<(), BotLedgerError> {
    require(rows::open(conn, identity.run)?.first() == Some(&identity.transition))
}

pub(super) fn prior_applied(
    conn: &Connection,
    owner: BotOwner,
    record: &DecisionProjectionRecord,
) -> Result<(), BotLedgerError> {
    if let Some(prior) = &record.target.predecessor().projection {
        let old = read(conn, owner, record.identity.run, prior.transition)?
            .ok_or_else(|| corrupt("bot_decision_publications", "predecessor"))?;
        require(
            old.identity.sequence == prior.sequence
                && old.identity.target_digest == prior.target_digest
                && old.identity.workspace == record.identity.workspace
                && old.identity.document == record.identity.document
                && old.identity.subject_digest == record.identity.subject_digest
                && matches!(old.state, DecisionProjectionState::Applied { .. }),
        )?;
        if old.identity.wait == record.identity.wait {
            require(
                old.target.effects().status == record.target.predecessor().status
                    && old.target.effects().failure_reason
                        == record.target.predecessor().failure_reason
                    && old.target.predecessor().observed_created_at_ms
                        == record.target.predecessor().observed_created_at_ms
                    && old
                        .target
                        .timeline_end()
                        .map_err(|_| BotLedgerError::Invalid)?
                        == record.target.predecessor().timeline_start,
            )?;
        } else {
            let prior_wait =
                super::super::rows::by_wait(conn, record.identity.run, old.identity.wait)?
                    .ok_or_else(|| corrupt("bot_decision_publications", "prior_wait"))?;
            require(
                prior_wait.phase == BotWaitPhase::Settled && prior_wait.settled_at_ms.is_some(),
            )?;
        }
    }
    Ok(())
}

impl RunsDb {
    /// Apply acknowledgment and bus enqueue share this writer transaction.
    /// Only D's internal checked application adapter may supply the observed
    /// version; SQLite compares identity and never interprets opaque VV bytes.
    pub fn acknowledge_decision_projection(
        &mut self,
        owner: BotOwner,
        acknowledgment: &DecisionProjectionAcknowledgment,
    ) -> Result<DecisionProjectionRecord, BotLedgerError> {
        self.write_with_error(crate::WriteOp::AcknowledgeDecisionProjection, |tx, now| {
            let old = required(tx, owner, &acknowledgment.identity)?;
            let prepared = old.prepared().ok_or(BotLedgerError::Stale)?;
            require(prepared.delta_digest == acknowledgment.delta_digest)?;
            if matches!(old.state, DecisionProjectionState::Applied { .. }) {
                // The first observed vector is immutable. A later qualifying
                // vector may cover additional operations; do not replace it.
                return Ok(old);
            }
            require(matches!(old.state, DecisionProjectionState::Prepared { .. }))?;
            first_open(tx, &old.identity)?;
            prior_applied(tx, owner, &old)?;
            // Target validation already correlated every stable envelope with
            // its exact typed source/control effect before retention. No ack
            // argument can substitute a new event, body or completion fact.
            for publication in old.target.publications() {
                let body = encoded(&publication.fact, DECISION_FRAME_BYTES)?;
                crate::publications::enqueue_exact_on(tx, publication.event, old.identity.run,
                    publication.fact.publication_kind(), &body, now)?;
            }
            let changed = tx.execute(
                "UPDATE bot_decision_publications SET state='applied',observed_version=?2,completed_at_ms=?3
                 WHERE transition_id=?1 AND state='prepared'",
                params![old.identity.transition.to_storage(), acknowledgment.observed_version.as_slice(),
                    now.max(old.created_at_ms)],
            ).map_err(sql)?;
            require(changed == 1)?;
            initial::activate_on(tx,owner,&old,now)?;
            required(tx, owner, &old.identity)
        })
    }

    /// Retain the first checked preparation exactly. The typed adapter did the
    /// Loro work off-writer. Here neither equality nor a digest proves import
    /// coverage; this transaction proves only retained identity and ordering.
    pub fn record_decision_preparation(
        &mut self,
        owner: BotOwner,
        prepared: &DecisionPreparedProjection,
    ) -> Result<DecisionProjectionRecord, BotLedgerError> {
        valid(prepared)?;
        require(
            decision_projection_delta_digest(&prepared.delta)
                .map_err(|_| BotLedgerError::Invalid)?
                == prepared.delta_digest,
        )?;
        self.write_with_error(crate::WriteOp::PrepareDecisionProjection, |tx, _now| {
            let old = required(tx, owner, &prepared.identity)?;
            require(old.target.canonical_bytes().map_err(|_| BotLedgerError::Invalid)?.as_slice()
                == prepared.target.as_slice())?;
            if let Some(retained) = old.prepared() {
                require(retained == prepared)?;
                return Ok(old);
            }
            require(matches!(old.state, DecisionProjectionState::Pending))?;
            first_open(tx, &old.identity)?;
            prior_applied(tx, owner, &old)?;
            let changed = tx.execute(
                "UPDATE bot_decision_publications SET state='prepared',base_version=?2,result_version=?3,
                 prepared_delta=?4,delta_digest=?5 WHERE transition_id=?1 AND state='pending'",
                params![prepared.identity.transition.to_storage(), prepared.base_version.as_slice(),
                    prepared.result_version.as_slice(), prepared.delta.as_slice(), prepared.delta_digest.as_bytes()],
            ).map_err(sql)?;
            require(changed == 1)?;
            required(tx, owner, &prepared.identity)
        })
    }
}
