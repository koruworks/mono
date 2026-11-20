// SPDX-License-Identifier: MPL-2.0

//! Synced vocabulary for work that repeats (M2).
//!
//! Schedule context is live; run context is pinned. A definition is one
//! value so its template, rule, lifecycle, and revision never merge field by
//! field. Retirement and handoff are append-only facts, so stale edits cannot
//! resurrect work or establish a second owner.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

pub use koru_recurrence::{CanonicalZone, RecurrenceRule, next_zoned_after_at_least};
use serde::{Deserialize, Serialize};

use koru_delegate_core::{
    ActorRef, BlobHash, BlobKind, BlockNodeId, DeviceId, DocumentId, HandoffId, Objective,
    RunContextItem, RunId, ScheduleId, SessionProfileKey, TextError, WorkspaceId, WorkspaceScope,
};

mod routine;
pub use routine::RoutineAssociation;
/// The longest authored schedule name, in Unicode scalar values.
pub const MAX_SCHEDULE_NAME_CHARS: usize = 128;

/// A person-authored schedule label. It is presentation metadata, never
/// schedule identity, and is redacted anywhere a derived `Debug` can reach.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "String", into = "String")]
pub struct ScheduleName(String);

impl ScheduleName {
    pub fn new(value: impl Into<String>) -> Result<Self, TextError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(TextError::Empty);
        }
        if trimmed.chars().count() > MAX_SCHEDULE_NAME_CHARS {
            return Err(TextError::TooLong {
                max_chars: MAX_SCHEDULE_NAME_CHARS,
            });
        }
        if trimmed.chars().any(char::is_control) {
            return Err(TextError::ControlCharacter);
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Derive the compatibility label for an unreleased schema-1 document
    /// written before the independent `name` root key existed.
    pub fn from_objective(objective: &Objective) -> Self {
        let line = objective
            .as_str()
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or(objective.as_str().trim());
        let value = line
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .take(MAX_SCHEDULE_NAME_CHARS)
            .collect::<String>();
        Self(value.trim_end().to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn char_count(&self) -> usize {
        self.0.chars().count()
    }
}

impl TryFrom<String> for ScheduleName {
    type Error = TextError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ScheduleName> for String {
    fn from(value: ScheduleName) -> Self {
        value.0
    }
}

impl fmt::Debug for ScheduleName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ScheduleName(<redacted>)")
    }
}

// The result vocabulary lives in the shared leaf so documents and events use
// the same enums. Re-export it to preserve this crate's schedule-facing API.
pub use koru_delegate_core::{
    ScheduleContextFailureStage, ScheduleFiringOutcome, SchedulePauseReason,
};

/// A live pointer selected for every future firing. This deliberately cannot
/// be placed in a [`koru_delegate_core::DelegationRequest`], which requires pinned bytes.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleContextPointer {
    Document {
        workspace: WorkspaceId,
        document: DocumentId,
    },
    Block {
        workspace: WorkspaceId,
        document: DocumentId,
        node: BlockNodeId,
    },
    Blob {
        workspace: WorkspaceId,
        blob: BlobHash,
        blob_kind: BlobKind,
    },
}

/// The reusable request fields. `RunId`, executing device, and document
/// version pins exist only for one claimed firing and cannot be stored here.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DelegationTemplate {
    pub requested_by: ActorRef,
    pub objective: Objective,
    pub agent: SessionProfileKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<koru_delegate_core::ModelName>,
    pub scope: WorkspaceScope,
    #[serde(default)]
    pub grants: BTreeSet<WorkspaceId>,
    #[serde(default)]
    pub context: Vec<ScheduleContextPointer>,
}

impl DelegationTemplate {
    /// Bind one planned firing to its identity, device, and freshly pinned
    /// context. Pin resolution itself belongs to P2.4; this conversion keeps
    /// every retained template field, including the selected model.
    pub fn into_request(
        self,
        run: RunId,
        executing_device: DeviceId,
        context: Vec<RunContextItem>,
    ) -> koru_delegate_core::DelegationRequest {
        koru_delegate_core::DelegationRequest {
            run,
            requested_by: self.requested_by,
            objective: self.objective,
            agent: self.agent,
            requested_model: self.requested_model,
            scope: self.scope,
            context,
            grants: self.grants,
            executing_device,
            origin: koru_delegate_core::RunOrigin::Manual,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ScheduleLifecycle {
    Active,
    Paused { reason: SchedulePauseReason },
}

/// One coherent definition edit. The document boundary rejects revision 0
/// and any edit that does not advance exactly once.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ScheduleDefinition {
    pub revision: u32,
    /// The inclusive lower bound for occurrences under this revision. Edits
    /// and resumes advance it, so a new rule cannot catch up older slots.
    pub effective_at_ms: i64,
    pub template: DelegationTemplate,
    pub recurrence: RecurrenceRule,
    pub lifecycle: ScheduleLifecycle,
    /// Retained Bot Schedule2 marker. Ordinary scheduling excludes these
    /// definitions; their bytes remain decodable and untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routine: Option<RoutineAssociation>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ScheduleHeader {
    pub schedule: ScheduleId,
    pub created_at_ms: i64,
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub initial_owner: DeviceId,
    pub definition: ScheduleDefinition,
}

/// One append-only handoff fact. Requests extend a confirmed epoch; only a
/// confirmation by the effective owner transfers authority.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandoffEvent {
    Requested {
        handoff: HandoffId,
        previous: Option<HandoffId>,
        #[cfg_attr(feature = "schema", schemars(with = "String"))]
        requested_by: DeviceId,
        #[cfg_attr(feature = "schema", schemars(with = "String"))]
        from: DeviceId,
        #[cfg_attr(feature = "schema", schemars(with = "String"))]
        to: DeviceId,
        at_ms: i64,
    },
    Confirmed {
        handoff: HandoffId,
        #[cfg_attr(feature = "schema", schemars(with = "String"))]
        confirmed_by: DeviceId,
        at_ms: i64,
    },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HandoffState {
    pub owner: DeviceId,
    pub epoch: Option<HandoffId>,
    pub pending: Vec<HandoffId>,
}

impl HandoffState {
    /// Reduce a merged event set. UUID order is the deterministic tie-breaker
    /// if one owner confirms competing requests from the same epoch.
    pub fn reduce(initial_owner: DeviceId, events: &[HandoffEvent]) -> Self {
        let mut requests = BTreeMap::new();
        let mut confirmations = BTreeMap::new();
        for event in events {
            match event {
                HandoffEvent::Requested { handoff, .. } => {
                    requests.entry(*handoff).or_insert(event);
                }
                HandoffEvent::Confirmed { handoff, .. } => {
                    confirmations.entry(*handoff).or_insert(event);
                }
            }
        }
        let mut owner = initial_owner;
        let mut epoch = None;
        loop {
            let next = requests.iter().find_map(|(id, event)| match event {
                HandoffEvent::Requested {
                    previous, from, to, ..
                } if *previous == epoch && *from == owner => match confirmations.get(id) {
                    Some(HandoffEvent::Confirmed { confirmed_by, .. })
                        if *confirmed_by == owner =>
                    {
                        Some((*id, *to))
                    }
                    _ => None,
                },
                _ => None,
            });
            let Some((handoff, to)) = next else { break };
            owner = to;
            epoch = Some(handoff);
        }
        let pending = requests
            .iter()
            .filter_map(|(id, event)| match event {
                HandoffEvent::Requested { previous, from, .. }
                    if *previous == epoch
                        && *from == owner
                        && !matches!(
                            confirmations.get(id),
                            Some(HandoffEvent::Confirmed { confirmed_by, .. })
                                if *confirmed_by == owner
                        ) =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .collect();
        Self {
            owner,
            epoch,
            pending,
        }
    }
}

/// One immutable occurrence. `at` is also the observation cursor for a
/// missed slot; other outcomes advance through `occurrence_ms`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ScheduleFiring {
    pub occurrence_ms: i64,
    pub revision: u32,
    /// The planned id retained even when the work was skipped. A skipped id
    /// has no `run_intents` row and therefore never denotes an actual run.
    pub run: RunId,
    pub outcome: ScheduleFiringOutcome,
    /// D34: the run holding the scope, present exactly for `skipped_busy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy_holder: Option<RunId>,
    pub at: i64,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ScheduleState {
    Active {
        #[cfg_attr(feature = "schema", schemars(with = "String"))]
        owner: DeviceId,
    },
    Paused {
        #[cfg_attr(feature = "schema", schemars(with = "String"))]
        owner: DeviceId,
        reason: SchedulePauseReason,
    },
    HandoffPending {
        #[cfg_attr(feature = "schema", schemars(with = "String"))]
        owner: DeviceId,
        requests: Vec<HandoffId>,
    },
    Retired,
}

#[cfg(test)]
mod tests;
