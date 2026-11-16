// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{
    AgentTurnId, ApprovalChoiceId, ApprovalId, AttemptGeneration, BotId, BotOwner,
    BotPolicyBarrierKey, BotRequestId, CapturedBotPolicy, ConnectionId, ConnectionItemId,
    DocumentId, NativePolicyRequestId, PrivateOperationKind, PrivateOperationProposal, ProfileId,
    RoutineClaimIdentity, RoutineProvenance, RoutineRouteIdentity, RunAttemptId, RunId, ScheduleId,
    SessionId, WorkspaceId,
};
use koru_session_protocol::{ApprovalDecision, ApprovalIntent};
use serde::ser::SerializeMap;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionGlobalPolicy {
    pub key: BotPolicyBarrierKey,
    pub config_revision: DecisionRevision,
    pub policy_revision: DecisionRevision,
    pub digest: DecisionDigest,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRoutinePolicy {
    pub owner: BotOwner,
    pub bot: BotId,
    pub schedule: ScheduleId,
    pub request: NativePolicyRequestId,
    pub grant_revision: DecisionRevision,
    pub digest: DecisionDigest,
}

/// Complete retained Routine subject, without copying the original task body.
/// Its AI IDs and claim stay fixed when the delegate consumer has a successor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRoutineSubject {
    pub provenance: RoutineProvenance,
    pub claim: RoutineClaimIdentity,
    pub route: RoutineRouteIdentity,
}
impl DecisionValidate for DecisionRoutineSubject {
    fn validate(&self) -> Result<(), DecisionError> {
        require(
            self.provenance.valid()
                && self.claim.valid()
                && self.route.matches_slot(&self.claim.slot)
                && self.provenance.schedule == self.claim.slot.schedule
                && self.provenance.occurrence_ms == self.claim.slot.occurrence_ms
                && self.provenance.definition == self.claim.definition,
        )
    }
}

// Generate the two strict records with the same original subject fields, not
// serde flatten (which cannot reliably close nested unknown-field boundaries).
macro_rules! binding_record {
    ($name:ident { $($field:ident : $ty:ty,)* }) => {
        #[derive(Clone, PartialEq, Eq, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            pub version: DecisionVersionV1,
            pub owner: BotOwner,
            pub bot: BotId,
            pub run: RunId,
            pub origin_attempt: RunAttemptId,
            #[serde(deserialize_with="identity::generation")]
            pub origin_generation: AttemptGeneration,
            pub origin_session: SessionId,
            pub origin_turn: AgentTurnId,
            pub submission: BotRequestId,
            pub routine: Option<DecisionRoutineSubject>,
            pub human_profile: ProfileId,
            pub workspace: WorkspaceId,
            $(pub $field: $ty,)*
            pub captured_policy: CapturedBotPolicy,
            pub global_policy: DecisionGlobalPolicy,
            pub routine_policy: Option<DecisionRoutinePolicy>,
        }
        impl $name {
            pub fn origin(&self) -> DecisionFence {
                DecisionFence { owner:self.owner, bot:self.bot, run:self.run,
                    attempt:self.origin_attempt, generation:self.origin_generation,
                    session:self.origin_session, turn:self.origin_turn }
            }
            fn valid_subject(&self) -> Result<(), DecisionError> {
                self.origin().validate()?;
                require(self.submission.to_storage().parse::<BotRequestId>() == Ok(self.submission)
                    && self.captured_policy.valid()
                    && self.captured_policy.workspace == self.workspace
                    && self.global_policy.key.owner == self.owner
                    && self.global_policy.key.bot == self.bot)?;
                match (&self.routine, &self.routine_policy) {
                    (None,None) => Ok(()),
                    (Some(r),Some(p)) => {
                        r.validate()?;
                        require(r.claim.slot.owner==self.owner && r.claim.slot.bot==self.bot
                            && r.claim.ids.run==self.run && r.claim.ids.submission==self.submission
                            && r.route.workspace==self.workspace && p.owner == self.owner && p.bot == self.bot
                            && p.schedule == r.provenance.schedule && p.grant_revision.get() >= r.provenance.grant_revision)
                    },
                    _ => Err(DecisionError::Invalid),
                }
            }
        }
        impl std::fmt::Debug for $name {
            fn fmt(&self, f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {
                f.write_str(concat!(stringify!($name),"([REDACTED])"))
            }
        }
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionEditAction {
    EditDocument,
}
binding_record!(DecisionEditBinding {
    target: DocumentId,
    action: DecisionEditAction,
    operation_id: Uuid,
    edit_fingerprint: [u8; 32],
    expected_revision: DecisionVersionVector,
    payload_version: DecisionVersionV1,
    payload: DecisionPayload,
});

/// Native choice labels remain with the retained session prompt. This binding
/// retains its exact offered identities/intents, not a lossy Allow/Reject bool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionNativeChoice {
    pub id: ApprovalChoiceId,
    pub intent: ApprovalIntent,
}
impl DecisionNativeChoice {
    pub fn decision(&self) -> ApprovalDecision {
        self.intent.decision()
    }
}
binding_record!(DecisionNativeBinding {
    prompt: ApprovalId,
    // No new product choice ceiling: complete-frame bytes are decisive. This
    // count ceiling only bounds allocation even with a hostile serde sequence.
    choices: DecisionList<DecisionNativeChoice, DECISION_FRAME_BYTES>,
    native_deadline_at_ms: Option<i64>,
});

// One read of one picked item on a private connection (M5). A read is fully
// described by its scalars: identifiers and digests only, with no account
// label, title, vendor reference, link or secret, and no payload.
binding_record!(DecisionPrivateBinding {
    connection: ConnectionId,
    connection_revision: DecisionRevision,
    account: DecisionDigest,
    item: ConnectionItemId,
    item_ref: DecisionDigest,
    operation: PrivateOperationKind,
    operation_id: Uuid,
    request_fingerprint: [u8; 32],
});
impl DecisionPrivateBinding {
    /// The operation exactly as it was proposed and bound.
    pub fn proposal(&self) -> PrivateOperationProposal {
        PrivateOperationProposal {
            operation_id: self.operation_id,
            connection: self.connection,
            connection_revision: self.connection_revision,
            account: self.account,
            item: self.item,
            item_ref: self.item_ref,
            operation: self.operation,
            request_fingerprint: self.request_fingerprint,
        }
    }
}

/// Koru and native inputs are closed alternatives. No nullable edit/prompt bag.
/// Serialization below is the canonical digest-free encoding itself.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionBindingV1 {
    KoruEdit(DecisionEditBinding),
    NativeApproval(DecisionNativeBinding),
    PrivateOperation(DecisionPrivateBinding),
}
// The fields every binding record shares, read without naming a kind.
macro_rules! shared {
    ($self:ident, $v:ident => $e:expr) => {
        match $self {
            Self::KoruEdit($v) => $e,
            Self::NativeApproval($v) => $e,
            Self::PrivateOperation($v) => $e,
        }
    };
}
impl DecisionBindingV1 {
    pub fn kind(&self) -> DecisionKind {
        match self {
            Self::KoruEdit(_) => DecisionKind::KoruEdit,
            Self::NativeApproval(_) => DecisionKind::NativeApproval,
            Self::PrivateOperation(_) => DecisionKind::PrivateOperation,
        }
    }
    pub fn origin(&self) -> DecisionFence {
        shared!(self, v => v.origin())
    }
    pub fn human_profile(&self) -> ProfileId {
        shared!(self, v => v.human_profile)
    }
    pub fn submission(&self) -> BotRequestId {
        shared!(self, v => v.submission)
    }
    pub fn workspace(&self) -> WorkspaceId {
        shared!(self, v => v.workspace)
    }
    pub fn captured_policy(&self) -> &CapturedBotPolicy {
        shared!(self, v => &v.captured_policy)
    }
    pub fn global_policy(&self) -> &DecisionGlobalPolicy {
        shared!(self, v => &v.global_policy)
    }
    pub fn routine(&self) -> Option<&DecisionRoutineSubject> {
        shared!(self, v => v.routine.as_ref())
    }
    pub fn routine_policy(&self) -> Option<&DecisionRoutinePolicy> {
        shared!(self, v => v.routine_policy.as_ref())
    }
    /// The operation id of an operation-keyed decision; `None` for a native
    /// prompt, which is keyed by its prompt.
    pub fn operation_id(&self) -> Option<Uuid> {
        match self {
            Self::KoruEdit(v) => Some(v.operation_id),
            Self::PrivateOperation(v) => Some(v.operation_id),
            Self::NativeApproval(_) => None,
        }
    }
    /// Exact comparison of a retained private binding with a proposal,
    /// including the connection revision it was planned against.
    pub fn matches_private(&self, proposal: &PrivateOperationProposal) -> bool {
        matches!(self, Self::PrivateOperation(v) if v.proposal() == *proposal)
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, DecisionError> {
        self.validate()?;
        bounded::encode(self, DECISION_FRAME_BYTES)
    }
    pub fn digest(&self) -> Result<DecisionDigest, DecisionError> {
        self.validate()?;
        crate::routine_digest_json(b"koru.bot.decision.binding.v1", self, DECISION_FRAME_BYTES)
            .map_err(Into::into)
    }
    /// Only original operation comparison. The caller separately establishes
    /// semantic VV equality with same_native_edit_revision and verifies current
    /// consumer/permission. A byte-different VV is not itself a conflict.
    pub fn matches_edit_except_revision(
        &self,
        operation_id: Uuid,
        target: DocumentId,
        fingerprint: [u8; 32],
        payload: &[u8],
    ) -> bool {
        matches!(self, Self::KoruEdit(v) if v.operation_id==operation_id && v.target==target
            && v.edit_fingerprint==fingerprint && v.payload.as_slice()==payload)
    }
}
impl DecisionValidate for DecisionBindingV1 {
    fn validate(&self) -> Result<(), DecisionError> {
        match self {
            Self::KoruEdit(v) => {
                v.valid_subject()?;
                require(identity::v4(v.operation_id))
            }
            Self::NativeApproval(v) => {
                v.valid_subject()?;
                let choices = v.choices.as_slice();
                let mut ids = std::collections::BTreeSet::new();
                require(!choices.is_empty() && choices.iter().all(|c| ids.insert(c.id)))
            }
            Self::PrivateOperation(v) => {
                v.valid_subject()?;
                // The fingerprint is recomputed from the binding's own fields;
                // it is never a caller's assertion.
                require(v.proposal().valid())
            }
        }
    }
}
impl Serialize for DecisionBindingV1 {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        // Fixed field order, no self-digest, maps or locale. No skipped defaults, with
        // one exception: `captured_policy.connections` is omitted when it is empty, so
        // a policy without connection grants keeps its pre-M5 bytes and digest. No
        // other field may ever be skipped.
        // Version precedes kind even though serde's tagged derive emits kind first.
        macro_rules! subject {
            ($v:ident) => {
                m.serialize_entry("version", &$v.version)?;
                m.serialize_entry("kind", &self.kind())?;
                m.serialize_entry("owner", &$v.owner)?;
                m.serialize_entry("bot", &$v.bot)?;
                m.serialize_entry("run", &$v.run)?;
                m.serialize_entry("origin_attempt", &$v.origin_attempt)?;
                m.serialize_entry("origin_generation", &$v.origin_generation)?;
                m.serialize_entry("origin_session", &$v.origin_session)?;
                m.serialize_entry("origin_turn", &$v.origin_turn)?;
                m.serialize_entry("submission", &$v.submission)?;
                m.serialize_entry("routine", &$v.routine)?;
                m.serialize_entry("human_profile", &$v.human_profile)?;
                m.serialize_entry("workspace", &$v.workspace)?;
            };
        }
        macro_rules! policy {
            ($v:ident) => {
                m.serialize_entry("captured_policy", &$v.captured_policy)?;
                m.serialize_entry("global_policy", &$v.global_policy)?;
                m.serialize_entry("routine_policy", &$v.routine_policy)?;
            };
        }
        match self {
            Self::KoruEdit(v) => {
                subject!(v);
                m.serialize_entry("target", &v.target)?;
                m.serialize_entry("action", &v.action)?;
                m.serialize_entry("operation_id", &v.operation_id)?;
                m.serialize_entry("edit_fingerprint", &v.edit_fingerprint)?;
                m.serialize_entry("expected_revision", &v.expected_revision)?;
                policy!(v);
                m.serialize_entry("payload_version", &v.payload_version)?;
                m.serialize_entry("payload", &v.payload)?;
            }
            Self::NativeApproval(v) => {
                subject!(v);
                m.serialize_entry("prompt", &v.prompt)?;
                m.serialize_entry("choices", &v.choices)?;
                m.serialize_entry("native_deadline_at_ms", &v.native_deadline_at_ms)?;
                policy!(v);
            }
            Self::PrivateOperation(v) => {
                subject!(v);
                m.serialize_entry("connection", &v.connection)?;
                m.serialize_entry("connection_revision", &v.connection_revision)?;
                m.serialize_entry("account", &v.account)?;
                m.serialize_entry("item", &v.item)?;
                m.serialize_entry("item_ref", &v.item_ref)?;
                m.serialize_entry("operation", &v.operation)?;
                m.serialize_entry("operation_id", &v.operation_id)?;
                m.serialize_entry("request_fingerprint", &v.request_fingerprint)?;
                policy!(v);
            }
        }
        m.end()
    }
}
