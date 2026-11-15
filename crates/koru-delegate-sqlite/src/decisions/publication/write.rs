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
                && old.target.effects().status == record.target.predecessor().status
                && old.target.effects().failure_reason
                    == record.target.predecessor().failure_reason
                && matches!(old.state, DecisionProjectionState::Applied { .. }),
        )?;
    }
    Ok(())
}

impl RunsDb {
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
