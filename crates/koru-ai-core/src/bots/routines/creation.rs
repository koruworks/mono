// SPDX-License-Identifier: MPL-2.0

use super::*;

/// A separate grant copied once by the trusted owner. Global pause is not a
/// frozen grant field; current global and Routine barriers fence every effect.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRoutineGrant {
    pub source_bot_policy_revision: u64,
    pub grant_revision: u64,
    pub policy: CapturedBotPolicy,
    pub digest: RoutineDigest32,
}
impl std::fmt::Debug for BotRoutineGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotRoutineGrant([REDACTED])")
    }
}
impl BotRoutineGrant {
    pub fn validate(&self, home: WorkspaceId) -> Result<(), BotError> {
        if !revision(self.source_bot_policy_revision)
            || !revision(self.grant_revision)
            || !self.policy.valid()
            || self.policy.workspace != home
        {
            return Err(BotError::Invalid);
        }
        bounded(self, BOT_ROUTINE_GRANT_BYTES)
    }
}

/// The complete original document seed, not reconstructed from current settings.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRoutineSeed {
    pub version: u8,
    pub name: ScheduleName,
    #[serde(with = "strict::Header")]
    pub header: ScheduleHeader,
}
impl std::fmt::Debug for BotRoutineSeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotRoutineSeed([REDACTED])")
    }
}
impl BotRoutineSeed {
    pub fn validate(&self, association: &RoutineAssociation) -> Result<(), BotError> {
        let header = &self.header;
        if self.version != 1
            || !association.valid()
            || !v4(header.schedule.as_uuid())
            || header.initial_owner != association.owner.device
            || header.definition.revision != 1
            || header.definition.effective_at_ms != header.created_at_ms
            || header.definition.routine.as_ref() != Some(association)
            || header.definition.lifecycle
                != (ScheduleLifecycle::Paused {
                    reason: SchedulePauseReason::Manual,
                })
            || self.name.as_str().len() > 256
        {
            return Err(BotError::Invalid);
        }
        validate_definition(&header.definition, association)?;
        bounded(self, BOT_ROUTINE_SEED_BYTES)
    }
}

pub(super) fn validate_definition(
    definition: &ScheduleDefinition,
    association: &RoutineAssociation,
) -> Result<(), BotError> {
    use koru_delegate_schedule::ScheduleContextPointer;
    let template = &definition.template;
    if definition.revision == 0
        || !association.valid()
        || definition.routine.as_ref() != Some(association)
        || !matches!(template.scope, WorkspaceScope::BotFolder { .. })
        || template.grants.len() != 1
        || !template.grants.contains(&association.home)
        || template.context.len() > 16
    {
        return Err(BotError::Invalid);
    }
    let mut documents = std::collections::BTreeSet::new();
    for item in &template.context {
        match item {
            ScheduleContextPointer::Document {
                workspace,
                document,
            } if *workspace == association.home && documents.insert(*document) => {}
            _ => return Err(BotError::Invalid),
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRoutineCreationIdentity {
    pub owner: BotOwner,
    pub bot: BotId,
    pub home: WorkspaceId,
    pub schedule: ScheduleId,
    pub creation_request: BotRequestId,
    pub store_request: uuid::Uuid,
    pub expected_bot_revision: u64,
    pub command_fingerprint: RoutineDigest32,
    pub seed: BotRoutineSeed,
    pub grant: BotRoutineGrant,
    pub seed_hash: RoutineDigest32,
}
impl BotRoutineCreationIdentity {
    pub fn association(&self) -> RoutineAssociation {
        RoutineAssociation {
            owner: self.owner,
            bot: self.bot,
            home: self.home,
            creation_request: self.creation_request,
        }
    }
    pub fn route(&self) -> RoutineRouteIdentity {
        RoutineRouteIdentity {
            owner: self.owner,
            bot: self.bot,
            schedule: self.schedule,
            workspace: self.home,
            creation_request: self.creation_request,
            seed_hash: self.seed_hash,
            created_at_ms: self.seed.header.created_at_ms,
        }
    }
    pub fn validate(&self) -> Result<(), BotError> {
        if !self.route().valid()
            || !v4(self.store_request)
            || !revision(self.expected_bot_revision)
            || self.seed.header.schedule != self.schedule
            || self.grant.grant_revision != 1
        {
            return Err(BotError::Invalid);
        }
        self.seed.validate(&self.association())?;
        self.grant.validate(self.home)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotRoutineCreationState {
    Prepared,
    Unconfirmed,
    Bound,
    Conflict,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotRoutineCreationRefusal {
    OriginMismatch,
    DocumentChanged,
    OwnerChanged,
    AuthorityChanged,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRoutineCreation {
    pub identity: BotRoutineCreationIdentity,
    pub state: BotRoutineCreationState,
    /// Encoded original update version vector, never a history-version UUID.
    pub origin_edit_revision: Option<Vec<u8>>,
    pub refusal: Option<BotRoutineCreationRefusal>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
impl BotRoutineCreation {
    pub fn validate(&self) -> Result<(), BotError> {
        self.identity.validate()?;
        if self.created_at_ms != self.identity.seed.header.created_at_ms
            || (self.state == BotRoutineCreationState::Bound) != self.origin_edit_revision.is_some()
            || (self.state == BotRoutineCreationState::Conflict) != self.refusal.is_some()
            || self
                .origin_edit_revision
                .as_ref()
                .is_some_and(|vv| vv.is_empty() || vv.len() > BOT_ROUTINE_VV_BYTES)
        {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
    pub fn observed(&self) -> BotRoutineCreationObserved {
        BotRoutineCreationObserved {
            state: self.state,
            updated_at_ms: self.updated_at_ms,
        }
    }
}
/// Optimistic concurrency token, never proof of Store or owner authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRoutineCreationObserved {
    pub state: BotRoutineCreationState,
    pub updated_at_ms: i64,
}

/// Immutable small outcome in the shared command receipt; read current mutable
/// state separately. The frozen seed lives in bot_routines, never this outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRoutineCreationReceipt {
    pub version: u8,
    pub route: RoutineRouteIdentity,
    pub state: BotRoutineCreationState,
    pub refusal: Option<BotRoutineCreationRefusal>,
}
impl BotRoutineCreationReceipt {
    pub fn validate(&self, identity: &BotRoutineCreationIdentity) -> Result<(), BotError> {
        identity.validate()?;
        if self.version != 1
            || self.route != identity.route()
            || (self.state == BotRoutineCreationState::Conflict) != self.refusal.is_some()
        {
            return Err(BotError::Invalid);
        }
        bounded(self, BOT_ROUTINE_RECEIPT_BYTES)
    }
}

/// Trusted Store adapter result only, deliberately not deserializable. That
/// adapter must verify scope/request/hash/kind/location, decode the retained
/// original update and compare the live VV before constructing this evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotRoutineStoreConfirmation {
    pub identity: BotRoutineCreationIdentity,
    pub origin_edit_revision: Vec<u8>,
}
impl BotRoutineStoreConfirmation {
    pub fn validate(&self, expected: &BotRoutineCreationIdentity) -> Result<(), BotError> {
        expected.validate()?;
        if self.identity != *expected
            || self.origin_edit_revision.is_empty()
            || self.origin_edit_revision.len() > BOT_ROUTINE_VV_BYTES
        {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
}
