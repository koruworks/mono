// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{
    AgentTurnId, AmendmentRevision, DocumentId, EventId, ModelRef, RunId, StreamSequence,
    ToolFailures, TurnOriginator, UsageFactId, WorkspaceId,
};

mod target;
pub use target::*;
mod record;
pub use record::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionProjectionKind {
    Source,
    Control,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionProjectionIdentity {
    pub transition: DecisionTransitionId,
    pub run: RunId,
    pub wait: BotWaitId,
    pub decision: Option<DecisionProjectionRevision>,
    pub sequence: DecisionRevision,
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    pub source_schema: DecisionSourceSchema,
    pub subject_digest: DecisionDigest,
    pub kind: DecisionProjectionKind,
    pub target_digest: DecisionDigest,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionProjectionRevision {
    pub decision: DecisionId,
    pub revision: DecisionRevision,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct DecisionSourceSchema(u8);
impl TryFrom<u8> for DecisionSourceSchema {
    type Error = DecisionError;
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        require(matches!(v, 3 | 4 | 5))?;
        Ok(Self(v))
    }
}
impl From<DecisionSourceSchema> for u8 {
    fn from(v: DecisionSourceSchema) -> Self {
        v.0
    }
}

/// Prepared opaque data only. The document owner must decode its typed target,
/// verify hashes/causal coverage and inspect ImportStatus before use. This does
/// not provide an import method or accept model-authored delta capabilities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionPreparedProjection {
    pub identity: DecisionProjectionIdentity,
    pub target: DecisionBytes<65_536>,
    pub base_version: DecisionVersionVector,
    pub result_version: DecisionVersionVector,
    pub delta: DecisionPreparedDelta,
    pub delta_digest: DecisionDigest,
}
impl DecisionValidate for DecisionPreparedProjection {
    fn validate(&self) -> Result<(), DecisionError> {
        // Run UUID is the creation idempotency key, not necessarily its physical
        // Store document ID. C/D verifies the exact retained registered placement.
        bounded::encode(self, DECISION_FRAME_BYTES).map(|_| ())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "publication", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionPublication {
    Pending {
        latest_sequence: DecisionRevision,
        target_digest: DecisionDigest,
    },
    Applied {
        transition: DecisionTransitionId,
        sequence: DecisionRevision,
        target_digest: DecisionDigest,
        observed_version: DecisionVersionVector,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionProjectionPhase {
    Pending,
    Prepared,
    Applied,
    Superseded,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionOpenProjection {
    pub run: RunId,
    pub sequence: DecisionRevision,
    pub kind: DecisionProjectionKind,
    pub phase: DecisionProjectionPhase,
}
/// A probe of at most three narrow rows. No target body/history is decoded to
/// reject a third open row or illegal Source/Control ordering.
pub fn validate_open_decision_projections(
    rows: &[DecisionOpenProjection],
) -> Result<(), DecisionError> {
    use DecisionProjectionKind as K;
    use DecisionProjectionPhase as P;
    if rows.len() > 2 {
        return Err(DecisionError::Invalid);
    }
    if rows
        .iter()
        .any(|r| !matches!(r.phase, P::Pending | P::Prepared))
    {
        return Err(DecisionError::Invalid);
    }
    if let [first, last] = rows {
        require(
            first.run == last.run
                && first.sequence < last.sequence
                && last.kind == K::Control
                && last.phase == P::Pending,
        )?;
        require(matches!(
            (first.kind, first.phase),
            (K::Source, P::Pending | P::Prepared) | (K::Control, P::Prepared)
        ))?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSourceStream {
    Agent,
    Session,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionSourceFrontiers {
    pub agent_after: Option<StreamSequence>,
    pub session_after: Option<StreamSequence>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionSourceBoundary {
    pub fence: DecisionFence,
    pub producer_epoch: uuid::Uuid,
    pub stream: DecisionSourceStream,
    pub sequence: StreamSequence,
    pub resulting: DecisionSourceFrontiers,
}
impl DecisionValidate for DecisionSourceBoundary {
    fn validate(&self) -> Result<(), DecisionError> {
        self.fence.validate()?;
        require(
            identity::v4(self.producer_epoch)
                && match self.stream {
                    DecisionSourceStream::Agent => {
                        self.resulting.agent_after == Some(self.sequence)
                    }
                    DecisionSourceStream::Session => {
                        self.resulting.session_after == Some(self.sequence)
                    }
                },
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionTurnTally {
    pub turn: AgentTurnId,
    pub originator: TurnOriginator,
    pub tool_calls: u32,
    pub tool_failures: u32,
    #[serde(with = "strict_failures")]
    pub failure_breakdown: ToolFailures,
}
impl DecisionValidate for DecisionTurnTally {
    fn validate(&self) -> Result<(), DecisionError> {
        require(
            self.tool_failures <= self.tool_calls
                && self.failure_breakdown.total() == self.tool_failures,
        )
    }
}

/// Keep the existing ToolFailures owner but reject lossy compatibility decoding
/// at this new exact snapshot boundary. Ordinary legacy decoding is unchanged.
mod strict_failures {
    use super::*;
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Entry {
        tool: crate::ToolKind,
        failure: crate::ToolFailure,
        calls: u16,
    }
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Strict {
        #[serde(default)]
        tallies: DecisionList<Entry, { crate::MAX_TOOL_FAILURE_KINDS }>,
        #[serde(default)]
        elided: u16,
    }
    pub fn serialize<S: serde::Serializer>(v: &ToolFailures, s: S) -> Result<S::Ok, S::Error> {
        v.serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<ToolFailures, D::Error> {
        let v = Strict::deserialize(d)?;
        let tallies = v.tallies.as_slice();
        if tallies.iter().any(|t| t.calls == 0)
            || tallies.iter().enumerate().any(|(i, t)| {
                tallies[..i]
                    .iter()
                    .any(|p| p.tool == t.tool && p.failure == t.failure)
            })
        {
            return Err(serde::de::Error::custom(DecisionError::Invalid));
        }
        // Existing owning codec performs construction; bounded exact records
        // above prevent its legacy folding/skipping behavior from changing data.
        let bytes = serde_json::to_vec(&v).map_err(serde::de::Error::custom)?;
        serde_json::from_slice(&bytes).map_err(serde::de::Error::custom)
    }
}

/// A complete usage amendment including the stable EventId. It is the current
/// fact to apply, not a snapshot of all historical usage facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionUsageSnapshot {
    pub id: UsageFactId,
    pub revision: AmendmentRevision,
    pub event: EventId,
    pub attempt: crate::RunAttemptId,
    pub turn: AgentTurnId,
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cached_input: Option<u64>,
    pub cached_input_relationship: koru_session_protocol::CachedInputRelationship,
    pub model: ModelRef,
    pub completeness: DecisionUsageCompleteness,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionUsageCompleteness {
    Complete {},
    Incomplete { reason: crate::IncompleteReason },
}
impl DecisionUsageSnapshot {
    pub fn fact(&self) -> crate::UsageFact {
        crate::UsageFact {
            identity: self.id,
            attempt: self.attempt,
            turn: self.turn,
            revision: self.revision,
            input: self.input,
            output: self.output,
            cached_input: self.cached_input,
            model: self.model,
            completeness: match self.completeness {
                DecisionUsageCompleteness::Complete {} => crate::Completeness::Complete,
                DecisionUsageCompleteness::Incomplete { reason } => {
                    crate::Completeness::Incomplete { reason }
                }
            },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionSourceBookkeeping {
    pub boundary: DecisionSourceBoundary,
    pub unflushed: DecisionList<DecisionTurnTally, 65_536>,
    pub usage: Option<DecisionUsageSnapshot>,
    pub allocated_steps: DecisionList<crate::TimelineEntryId, 65_536>,
}
impl DecisionValidate for DecisionSourceBookkeeping {
    fn validate(&self) -> Result<(), DecisionError> {
        self.boundary.validate()?;
        let mut turns = std::collections::BTreeSet::new();
        for t in self.unflushed.as_slice() {
            t.validate()?;
            require(turns.insert(t.turn))?;
        }
        if let Some(u) = &self.usage {
            require(u.attempt == self.boundary.fence.attempt)?;
        }
        let mut steps = std::collections::BTreeSet::new();
        require(
            self.allocated_steps
                .as_slice()
                .iter()
                .all(|id| steps.insert(*id)),
        )?;
        bounded::encode(self, 65_536).map(|_| ())
    }
}
