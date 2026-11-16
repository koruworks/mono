// SPDX-License-Identifier: MPL-2.0
//! Private creation semantics and canonical digest owners, without Store access.
use super::*;

/// Trusted adapter evidence, never decoded from a public request.
pub enum PreparedRoutineDocument {
    Confirmed(BotRoutineStoreConfirmation),
    Disagreement(BotRoutineCreationRefusal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotRoutineRecoveryCursor {
    pub owner: crate::bots::BotOwner,
    pub after: i64,
    pub through: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotRoutineRecoveryIdentity {
    pub bot: BotId,
    pub schedule: ScheduleId,
    pub observed: BotRoutineCreationObserved,
}
#[derive(Debug)]
pub struct BotRoutineRecoveryPage {
    pub entries: Vec<Result<BotRoutineRecoveryIdentity, BotError>>,
    pub next: Option<BotRoutineRecoveryCursor>,
}
use koru_delegate_schedule::{DelegationTemplate, RecurrenceRule, ScheduleContextPointer};
use std::collections::BTreeSet;

/// Semantic caller input. IDs for the Schedule and Store are allocated only
/// after receipt lookup by the repository, never by a caller of prepare.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRoutineCreate {
    pub request: BotRequestId,
    pub bot: BotId,
    pub expected_bot_revision: u64,
    pub name: ScheduleName,
    pub objective: Objective,
    pub documents: Vec<DocumentId>,
    pub recurrence: RecurrenceRule,
}
impl std::fmt::Debug for BotRoutineCreate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotRoutineCreate([REDACTED])")
    }
}

/// Canonical SQL snapshot, not caller authority or a wire object. The repository
/// reconstructs this inside its transaction. A future trusted caller must still
/// prove current human/Profile authority before a Store effect or initial bind.
#[derive(Clone, PartialEq, Eq)]
pub struct BotRoutineCreationSource {
    pub owner: BotOwner,
    pub bot: BotId,
    pub home: WorkspaceId,
    pub profile: ProfileId,
    pub revision: u64,
    pub policy_revision: u64,
    pub model: crate::ModelSelection,
    pub folder: String,
    /// The Bot's own access now. A new Routine does not start from it
    /// (D4a): it is kept only so a creation prepared before that change,
    /// which copied it, is still recognized while it finishes.
    pub policy: CapturedBotPolicy,
}
impl std::fmt::Debug for BotRoutineCreationSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotRoutineCreationSource([REDACTED])")
    }
}
impl BotRoutineCreationSource {
    pub fn validate_for(&self, command: &BotRoutineCreate) -> Result<(), BotError> {
        command.validate()?;
        if self.bot != command.bot || self.revision != command.expected_bot_revision {
            return Err(BotError::RevisionConflict);
        }
        if !(RoutineAssociation {
            owner: self.owner,
            bot: self.bot,
            home: self.home,
            creation_request: command.request,
        })
        .valid()
        {
            return Err(BotError::Invalid);
        }
        self.template(command)?;
        self.grant()?;
        Ok(())
    }
    fn template(&self, command: &BotRoutineCreate) -> Result<DelegationTemplate, BotError> {
        let key = self
            .model
            .provider
            .as_str()
            .strip_prefix("agent-")
            .ok_or(BotError::Invalid)?;
        let agent = SessionProfileKey::BuiltIn(key.parse().map_err(|_| BotError::Invalid)?);
        Ok(DelegationTemplate {
            requested_by: ActorRef::Person {
                profile: self.profile,
            },
            objective: command.objective.clone(),
            agent,
            requested_model: Some(
                ModelName::new(&self.model.requested_model).map_err(|_| BotError::Invalid)?,
            ),
            scope: WorkspaceScope::BotFolder {
                folder: BotWorkingFolder::new(self.folder.clone())
                    .map_err(|_| BotError::Invalid)?,
            },
            grants: [self.home].into(),
            context: command
                .documents
                .iter()
                .map(|document| ScheduleContextPointer::Document {
                    workspace: self.home,
                    document: *document,
                })
                .collect(),
        })
    }
    /// The grant a new Routine starts from (D4a): the approved default
    /// access, with no selected pages, in the Bot's home. It is not a copy
    /// of the Bot's access; only the Bot policy revision observed at
    /// creation is recorded. The model cannot supply a grant.
    fn grant(&self) -> Result<BotRoutineGrant, BotError> {
        self.grant_of(CapturedBotPolicy {
            workspace: self.home,
            destination: self.home,
            access: BotAccess::Default,
            selected_pages: Default::default(),
            connections: Default::default(),
        })
    }
    /// The grant a creation prepared before D4a froze: a copy of the Bot's
    /// access. Never given to a new Routine.
    fn copied_grant(&self) -> Result<BotRoutineGrant, BotError> {
        self.grant_of(self.policy.clone())
    }
    fn grant_of(&self, policy: CapturedBotPolicy) -> Result<BotRoutineGrant, BotError> {
        let mut grant = BotRoutineGrant {
            source_bot_policy_revision: self.policy_revision,
            grant_revision: 1,
            policy,
            digest: RoutineDigest32::from_bytes([0; 32]),
        };
        grant.validate(self.home)?;
        grant.digest = grant.canonical_digest()?;
        Ok(grant)
    }
}
impl BotRoutineCreate {
    pub fn validate(&self) -> Result<(), BotError> {
        if !v4(self.request.as_uuid())
            || !v4(self.bot.as_uuid())
            || !revision(self.expected_bot_revision)
            || self.name.as_str().len() > 256
            || self.documents.len() > 16
            || self.documents.iter().collect::<BTreeSet<_>>().len() != self.documents.len()
        {
            return Err(BotError::Invalid);
        }
        bounded(self, BOT_ROUTINE_SEED_BYTES)
    }

    /// Called by the sole writer only after exact receipt/kind lookup and
    /// canonical guards. This function itself neither allocates nor authorizes.
    pub fn freeze(
        &self,
        source: &BotRoutineCreationSource,
        schedule: ScheduleId,
        store_request: uuid::Uuid,
        fingerprint: RoutineDigest32,
        now: i64,
    ) -> Result<BotRoutineCreationIdentity, BotError> {
        source.validate_for(self)?;
        let association = RoutineAssociation {
            owner: source.owner,
            bot: self.bot,
            home: source.home,
            creation_request: self.request,
        };
        let seed = BotRoutineSeed {
            version: 1,
            name: self.name.clone(),
            header: ScheduleHeader {
                schedule,
                created_at_ms: now,
                initial_owner: source.owner.device,
                definition: ScheduleDefinition {
                    revision: 1,
                    effective_at_ms: now,
                    template: source.template(self)?,
                    recurrence: self.recurrence.clone(),
                    lifecycle: ScheduleLifecycle::Paused {
                        reason: SchedulePauseReason::Manual,
                    },
                    routine: Some(association),
                },
            },
        };
        let identity = BotRoutineCreationIdentity {
            owner: source.owner,
            bot: self.bot,
            home: source.home,
            schedule,
            creation_request: self.request,
            store_request,
            expected_bot_revision: self.expected_bot_revision,
            command_fingerprint: fingerprint,
            seed_hash: seed.canonical_digest()?,
            seed,
            grant: source.grant()?,
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn matches_retained(&self, identity: &BotRoutineCreationIdentity) -> bool {
        let definition = &identity.seed.header.definition;
        self.request == identity.creation_request
            && self.bot == identity.bot
            && self.expected_bot_revision == identity.expected_bot_revision
            && self.name == identity.seed.name
            && self.objective == definition.template.objective
            && self.recurrence == definition.recurrence
            && definition.template.context
                == self
                    .documents
                    .iter()
                    .map(|document| ScheduleContextPointer::Document {
                        workspace: identity.home,
                        document: *document,
                    })
                    .collect::<Vec<_>>()
    }
}

impl BotRoutineCreationIdentity {
    /// Checks persisted bytes against the shared digest owners, independently of
    /// structural validation (which remains useful for pure contract fixtures).
    pub fn validate_digests(&self) -> Result<(), BotError> {
        self.validate()?;
        if self.seed_hash != self.seed.canonical_digest()?
            || self.grant.digest != self.grant.canonical_digest()?
        {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
    fn semantic_command(&self) -> Result<BotRoutineCreate, BotError> {
        Ok(BotRoutineCreate {
            request: self.creation_request,
            bot: self.bot,
            expected_bot_revision: self.expected_bot_revision,
            name: self.seed.name.clone(),
            objective: self.seed.header.definition.template.objective.clone(),
            documents: self
                .seed
                .header
                .definition
                .template
                .context
                .iter()
                .map(|item| match item {
                    ScheduleContextPointer::Document { document, .. } => Ok(*document),
                    _ => Err(BotError::Invalid),
                })
                .collect::<Result<_, _>>()?,
            recurrence: self.seed.header.definition.recurrence.clone(),
        })
    }
    /// Validate the current source fully before treating any mismatch as
    /// positive drift. A changed revision must not mask a corrupt template.
    pub fn validate_current_source(
        &self,
        source: &BotRoutineCreationSource,
    ) -> Result<(), BotError> {
        let mut command = self.semantic_command()?;
        command.expected_bot_revision = source.revision;
        source.validate_for(&command)
    }
    pub fn matches_source(&self, source: &BotRoutineCreationSource) -> Result<bool, BotError> {
        let command = self.semantic_command()?;
        Ok(self.owner == source.owner
            && self.bot == source.bot
            && self.home == source.home
            && self.expected_bot_revision == source.revision
            // A Routine keeps the grant it was given: the default, or the
            // copy of the Bot's access a creation froze before D4a.
            && (self.grant == source.grant()? || self.grant == source.copied_grant()?)
            && self.seed.header.definition.template == source.template(&command)?)
    }
}

impl BotRoutineSeed {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, BotError> {
        // BotRoutineSeed is already the explicit v1 {version,name,header}
        // encoding, with strict header fields and no self-digest.
        bounded(self, BOT_ROUTINE_SEED_BYTES)?;
        serde_json::to_vec(self).map_err(|_| BotError::Invalid)
    }
    pub fn canonical_digest(&self) -> Result<RoutineDigest32, BotError> {
        koru_delegate_core::routine_digest_json(
            b"koru.bot.routine.seed.v1",
            self,
            BOT_ROUTINE_SEED_BYTES,
        )
        .map_err(|_| BotError::Invalid)
    }
}
#[derive(Serialize)]
struct GrantEncoding<'a> {
    version: u8,
    source_bot_policy_revision: u64,
    grant_revision: u64,
    policy: &'a CapturedBotPolicy,
}
impl BotRoutineGrant {
    fn encoding(&self) -> GrantEncoding<'_> {
        GrantEncoding {
            version: 1,
            source_bot_policy_revision: self.source_bot_policy_revision,
            grant_revision: self.grant_revision,
            policy: &self.policy,
        }
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, BotError> {
        let grant = self.encoding();
        bounded(&grant, BOT_ROUTINE_GRANT_BYTES)?;
        serde_json::to_vec(&grant).map_err(|_| BotError::Invalid)
    }
    pub fn canonical_digest(&self) -> Result<RoutineDigest32, BotError> {
        koru_delegate_core::routine_digest_json(
            b"koru.bot.routine.grant.v1",
            &self.encoding(),
            BOT_ROUTINE_GRANT_BYTES,
        )
        .map_err(|_| BotError::Invalid)
    }
}

/// Trusted persistence transition, deliberately not deserializable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BotRoutineCreationTransition {
    Unconfirmed,
    Confirmed(BotRoutineStoreConfirmation),
    Conflict(BotRoutineCreationRefusal),
}

/// Retained creation projection, not current live Schedule lifecycle or access.
#[derive(Clone, PartialEq, Eq)]
pub struct BotRoutineCreationSummary {
    pub route: RoutineRouteIdentity,
    pub name: String,
    pub observed: BotRoutineCreationObserved,
    pub refusal: Option<BotRoutineCreationRefusal>,
    pub grant_revision: u64,
    pub source_bot_policy_revision: u64,
}
impl std::fmt::Debug for BotRoutineCreationSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotRoutineCreationSummary([REDACTED])")
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotRoutineCreationPage {
    pub items: Vec<BotRoutineCreationSummary>,
    pub next: Option<ScheduleId>,
}

#[cfg(test)]
mod tests;
