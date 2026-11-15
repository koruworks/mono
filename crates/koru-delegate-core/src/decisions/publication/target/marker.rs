// SPDX-License-Identifier: MPL-2.0

use super::*;

/// Shared shape and bound, not a caller assertion that an import was applied.
/// Only the document owner inspects causal coverage and the attached value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionProjectionMarker {
    pub identity: DecisionProjectionIdentity,
    #[serde(deserialize_with = "strict_leaf")]
    pub status: RunStatus,
    pub bookkeeping: Option<DecisionSourceBookkeeping>,
    pub timeline_start: u64,
    pub timeline_end: u64,
}
impl std::fmt::Debug for DecisionProjectionMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DecisionProjectionMarker([REDACTED])")
    }
}
impl DecisionProjectionMarker {
    pub fn for_target(
        identity: &DecisionProjectionIdentity,
        target: &RunProjectionTargetV1,
        timeline_start: u64,
    ) -> Result<Self, DecisionError> {
        target.matches_identity(identity)?;
        let steps =
            u64::try_from(target.projected_steps()?.len()).map_err(|_| DecisionError::TooLarge)?;
        let count = steps
            .checked_add(u64::from(
                target
                    .source_bookkeeping()
                    .is_some_and(|b| b.usage.is_some()),
            ))
            .ok_or(DecisionError::TooLarge)?;
        let result = Self {
            identity: identity.clone(),
            status: target.effects().status.clone(),
            bookkeeping: target.resulting_bookkeeping()?,
            timeline_start,
            timeline_end: timeline_start
                .checked_add(count)
                .ok_or(DecisionError::TooLarge)?,
        };
        result.validate_bound()?;
        Ok(result)
    }
    pub fn validate_bound(&self) -> Result<(), DecisionError> {
        require(self.timeline_start <= self.timeline_end)?;
        if let Some(bookkeeping) = &self.bookkeeping {
            bookkeeping.validate()?;
        }
        crate::routine_digest_json(
            b"koru.bot.decision.projection.marker-shape.v1",
            self,
            DECISION_PAYLOAD_BYTES,
        )?;
        Ok(())
    }
}
