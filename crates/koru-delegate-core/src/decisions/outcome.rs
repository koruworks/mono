// SPDX-License-Identifier: MPL-2.0

use super::*;

#[derive(Serialize)]
struct Outcome<'a> {
    version: DecisionVersionV1,
    decision: DecisionId,
    binding_digest: DecisionDigest,
    deadline: &'a DecisionDeadline,
    state: &'a DecisionState,
    answer: &'a Option<DecisionAnswerReceipt>,
    consumed_at_ms: Option<i64>,
    closure: &'a Option<DecisionClosure>,
}

fn value(receipt: &DecisionReceipt) -> Result<Outcome<'_>, DecisionError> {
    receipt.validate()?;
    require(receipt.state.phase == DecisionPhase::Settled)?;
    Ok(Outcome {
        version: DecisionVersionV1,
        decision: receipt.decision,
        binding_digest: receipt.binding_digest,
        deadline: &receipt.deadline,
        state: &receipt.state,
        answer: &receipt.answer,
        consumed_at_ms: receipt.consumed_at_ms,
        closure: &receipt.closure,
    })
}

/// Commitment to immutable terminal receipt semantics, not Store/native effect
/// proof. Mutable observation timestamps and the already committed full binding
/// are excluded. This encoding never grants execution or cleanup authority.
pub fn decision_outcome_digest(receipt: &DecisionReceipt) -> Result<DecisionDigest, DecisionError> {
    crate::routine_digest_json(
        b"koru.bot.decision.outcome.v1",
        &value(receipt)?,
        DECISION_RECEIPT_BYTES,
    )
    .map_err(Into::into)
}

#[cfg(test)]
pub(super) fn canonical_bytes(receipt: &DecisionReceipt) -> Result<Vec<u8>, DecisionError> {
    bounded::encode(&value(receipt)?, DECISION_RECEIPT_BYTES)
}
