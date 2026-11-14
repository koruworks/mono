// SPDX-License-Identifier: MPL-2.0
//! Private occurrence encodings and stored state, not execution authority.
use super::super::{BotCapturedMemory, BotKnowledgeCapture};
use super::*;

/// The existing scheduler-owned encoding, available to the private AI adapter
/// without adding another Schedule dependency or a second hash implementation.
pub fn bot_routine_semantic_hash(
    slot: &RoutineSlot,
    definition: &ScheduleDefinition,
    home: WorkspaceId,
    owner_handoff: Option<HandoffId>,
) -> Result<RoutineDigest32, BotError> {
    koru_delegate_schedule::routine_semantic_digest(slot, definition, home, owner_handoff)
        .map_err(|_| BotError::Invalid)
}

/// Trusted capture inputs before any execution IDs exist. This value is not
/// serializable and is never accepted from a bridge request.
#[derive(Clone)]
pub struct BotRoutineCapturedSources {
    pub slot: RoutineSlot,
    pub definition: ScheduleDefinition,
    pub owner_handoff: Option<HandoffId>,
    pub creation: RoutineRouteIdentity,
    pub canonical: BotRoutineCanonical,
    pub profile: ProfileId,
    pub grant: BotRoutineGrant,
    pub role: BotKnowledgeCapture,
    pub knowledge: Vec<BotKnowledgeCapture>,
    pub task: Vec<BotKnowledgeCapture>,
    pub memory: BotCapturedMemory,
    pub index_digest: [u8; 32],
}
impl std::fmt::Debug for BotRoutineCapturedSources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotRoutineCapturedSources([REDACTED])")
    }
}
impl BotRoutineCapturedSources {
    pub fn semantic_hash(&self) -> Result<RoutineDigest32, BotError> {
        bot_routine_semantic_hash(
            &self.slot,
            &self.definition,
            self.canonical.home,
            self.owner_handoff,
        )
    }
    /// Called only after the writer has resolved the existing slot and cap.
    pub fn freeze(
        &self,
        ids: RoutineExecutionIds,
        now: i64,
    ) -> Result<BotRoutineStoredPreparation, BotError> {
        self.canonical.validate()?;
        if !self.creation.matches_slot(&self.slot)
            || !ids.valid()
            || self.definition.template.requested_by
                != (ActorRef::Person {
                    profile: self.profile,
                })
            || self.creation.workspace != self.canonical.home
            || self.grant.canonical_digest()? != self.grant.digest
        {
            return Err(BotError::Invalid);
        }
        let definition = RoutineDefinitionIdentity {
            revision: self.definition.revision,
            digest: koru_delegate_schedule::routine_definition_digest(
                &self.definition,
                self.canonical.home,
                self.owner_handoff,
            )
            .map_err(|_| BotError::Invalid)?,
            owner_handoff: self.owner_handoff,
        };
        let mut pins = std::collections::BTreeMap::new();
        for pin in std::iter::once(&self.role)
            .chain(&self.knowledge)
            .chain(&self.task)
        {
            if pin.page.workspace != self.canonical.home {
                return Err(BotError::Invalid);
            }
            if pins
                .insert(pin.page.document, pin.version)
                .is_some_and(|old| old != pin.version)
            {
                return Err(BotError::Invalid);
            }
        }
        let context = pins
            .into_iter()
            .map(|(document, version)| RunContextItem::Document {
                workspace: self.canonical.home,
                document,
                version,
            })
            .collect();
        let config = &self.canonical.config;
        let mut template = self.definition.template.clone();
        template.agent = SessionProfileKey::BuiltIn(
            config
                .model
                .provider
                .as_str()
                .strip_prefix("agent-")
                .ok_or(BotError::Invalid)?
                .parse()
                .map_err(|_| BotError::Invalid)?,
        );
        template.requested_model =
            Some(ModelName::new(&config.model.requested_model).map_err(|_| BotError::Invalid)?);
        template.scope = WorkspaceScope::BotFolder {
            folder: BotWorkingFolder::new(config.working_folder.clone())
                .map_err(|_| BotError::Invalid)?,
        };
        template.grants = [self.canonical.home].into();
        let mut request = template.into_request(ids.run, self.slot.owner.device, context);
        request.origin = RunOrigin::Scheduled {
            schedule: self.slot.schedule,
            occurrence_ms: self.slot.occurrence_ms,
            revision: definition.revision,
        };
        let mut prepared = RoutinePrepared {
            slot: self.slot,
            definition,
            ids,
            semantic_hash: self.semantic_hash()?,
            preparation_hash: RoutineDigest32::from_bytes([0; 32]),
            submission_version: 2,
            submission: BotSubmission {
                provenance: BotProvenance {
                    owner: self.slot.owner,
                    bot: self.slot.bot,
                    submission: ids.submission,
                    chat_id: ids.chat,
                    ai_attempt_id: Some(ids.attempt),
                    assistant_message_id: Some(ids.assistant_message),
                    config_revision: self.canonical.config_revision,
                    policy_revision: self.canonical.policy_revision,
                    continued_from: None,
                    routine: Some(RoutineProvenance {
                        schedule: self.slot.schedule,
                        occurrence_ms: self.slot.occurrence_ms,
                        definition,
                        grant_revision: self.grant.grant_revision,
                    }),
                },
                policy: self.grant.policy.clone(),
                workspace: self.canonical.home,
                request,
                input: PendingInput {
                    idempotency_key: ids
                        .submission
                        .to_string()
                        .parse()
                        .map_err(|_| BotError::Invalid)?,
                    text: self.definition.template.objective.as_str().into(),
                    attachment_ids: vec![],
                    references: vec![],
                    mode: config
                        .mode
                        .as_deref()
                        .map(str::parse)
                        .transpose()
                        .map_err(|_| BotError::Invalid)?,
                    fast: config.fast,
                    reasoning: match config.model.default_reasoning.as_token() {
                        "default" => None,
                        token => Some(token.parse().map_err(|_| BotError::Invalid)?),
                    },
                    skills: vec![],
                    model: Some(
                        config
                            .model
                            .requested_model
                            .parse()
                            .map_err(|_| BotError::Invalid)?,
                    ),
                },
            },
        };
        let capture = BotRoutineCaptureV1 {
            version: 1,
            creation: self.creation.clone(),
            schedule_definition: self.definition.clone(),
            canonical: self.canonical.clone(),
            grant: self.grant.clone(),
            library: BotLibraryCapture {
                version: 1,
                owner: super::super::BotOwner {
                    user_scope: self.slot.owner.user_scope,
                    device: self.slot.owner.device,
                },
                bot: self.slot.bot,
                submission: ids.submission,
                config_revision: self.canonical.config_revision,
                knowledge: self.knowledge.clone(),
                memory: self.memory.clone(),
                index_digest: self.index_digest,
            },
            role: self.role.clone(),
            task: self.task.clone(),
        };
        prepared.preparation_hash = bot_routine_preparation_digest(&prepared, &capture)?;
        let row = BotRoutineStoredPreparation {
            prepared,
            capture,
            stage: BotRoutineStoredStage::Prepared,
            created_at_ms: now,
            updated_at_ms: now,
        };
        row.validate()?;
        Ok(row)
    }
}

impl BotRoutineCaptureV1 {
    pub fn canonical_digest(&self) -> Result<RoutineDigest32, BotError> {
        routine_digest_json(
            b"koru.bot.routine.capture.v1",
            self,
            BOT_ROUTINE_CAPTURE_BYTES,
        )
        .map_err(|_| BotError::Invalid)
    }
}

/// Digest-free projection: never hash RoutinePrepared including its own hash.
#[derive(Serialize)]
struct PreparationV1<'a> {
    version: u8,
    slot: &'a RoutineSlot,
    definition: &'a RoutineDefinitionIdentity,
    ids: &'a RoutineExecutionIds,
    semantic_hash: RoutineDigest32,
    submission_version: u8,
    submission: &'a BotSubmission,
    config_revision: u64,
    policy_revision: u64,
    grant_revision: u64,
    capture_digest: RoutineDigest32,
}

pub fn bot_routine_preparation_digest(
    prepared: &RoutinePrepared,
    capture: &BotRoutineCaptureV1,
) -> Result<RoutineDigest32, BotError> {
    capture.validate(prepared)?;
    if capture.grant.canonical_digest()? != capture.grant.digest {
        return Err(BotError::Invalid);
    }
    let semantic = koru_delegate_schedule::routine_semantic_digest(
        &prepared.slot,
        &capture.schedule_definition,
        capture.canonical.home,
        prepared.definition.owner_handoff,
    )
    .map_err(|_| BotError::Invalid)?;
    let definition = koru_delegate_schedule::routine_definition_digest(
        &capture.schedule_definition,
        capture.canonical.home,
        prepared.definition.owner_handoff,
    )
    .map_err(|_| BotError::Invalid)?;
    if semantic != prepared.semantic_hash || definition != prepared.definition.digest {
        return Err(BotError::Invalid);
    }
    routine_digest_json(
        b"koru.bot.routine.preparation.v1",
        &PreparationV1 {
            version: 1,
            slot: &prepared.slot,
            definition: &prepared.definition,
            ids: &prepared.ids,
            semantic_hash: prepared.semantic_hash,
            submission_version: prepared.submission_version,
            submission: &prepared.submission,
            config_revision: capture.canonical.config_revision,
            policy_revision: capture.canonical.policy_revision,
            grant_revision: capture.grant.grant_revision,
            capture_digest: capture.canonical_digest()?,
        },
        BOT_ROUTINE_SUBMISSION_BYTES + BOT_ROUTINE_RECEIPT_BYTES,
    )
    .map_err(|_| BotError::Invalid)
}

/// SQL's coarse close_reason cannot distinguish AlreadyTerminal from Stopped.
/// The bounded receipt outcome retains this exact typed distinction. Public
/// preparation replies use Stale for AlreadyTerminal, never Closed(Stopped).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotRoutineStoredClosure {
    Unclaimed { reason: BotRoutineCloseReason },
    ClaimedTerminal { reason: RoutineTerminalReason },
}
impl BotRoutineStoredClosure {
    pub fn sql_reason(self) -> BotRoutineCloseReason {
        match self {
            Self::Unclaimed { reason } => reason,
            Self::ClaimedTerminal {
                reason: RoutineTerminalReason::ConfigurationChanged,
            } => BotRoutineCloseReason::ConfigurationChanged,
            Self::ClaimedTerminal {
                reason: RoutineTerminalReason::AuthorityChanged,
            } => BotRoutineCloseReason::AuthorityChanged,
            Self::ClaimedTerminal {
                reason: RoutineTerminalReason::Stopped | RoutineTerminalReason::AlreadyTerminal,
            } => BotRoutineCloseReason::Stopped,
        }
    }
    pub fn preparation_reason(self) -> Option<BotRoutineCloseReason> {
        match self {
            Self::ClaimedTerminal {
                reason: RoutineTerminalReason::AlreadyTerminal,
            } => None,
            _ => Some(self.sql_reason()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotRoutineStoredStage {
    Prepared,
    Linked,
    Closed(BotRoutineStoredClosure),
}

/// Strict private journal representation; never a bridge response or authority.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRoutineDraftReceipt {
    version: u8,
    slot: RoutineSlot,
    ids: RoutineExecutionIds,
    semantic_hash: RoutineDigest32,
    preparation_hash: RoutineDigest32,
    pub closure: Option<BotRoutineStoredClosure>,
}
impl BotRoutineDraftReceipt {
    pub fn for_row(row: &BotRoutineStoredPreparation) -> Self {
        Self {
            version: 1,
            slot: row.prepared.slot,
            ids: row.prepared.ids,
            semantic_hash: row.prepared.semantic_hash,
            preparation_hash: row.prepared.preparation_hash,
            closure: match row.stage {
                BotRoutineStoredStage::Closed(reason) => Some(reason),
                _ => None,
            },
        }
    }
    pub fn matches(&self, row: &BotRoutineStoredPreparation) -> bool {
        self.version == 1
            && self.slot == row.prepared.slot
            && self.ids == row.prepared.ids
            && self.semantic_hash == row.prepared.semantic_hash
            && self.preparation_hash == row.prepared.preparation_hash
    }
}

/// A row has no claim time. Assemble a full acknowledgement only from a fresh
/// authenticated delegate observation, never from either row timestamp.
#[derive(Clone, PartialEq, Eq)]
pub struct BotRoutineStoredPreparation {
    pub prepared: RoutinePrepared,
    pub capture: BotRoutineCaptureV1,
    pub stage: BotRoutineStoredStage,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
#[derive(Debug)]
pub enum BotRoutinePreparationAdmission {
    Ready(Box<BotRoutineStoredPreparation>),
    Capacity,
}

/// Ephemeral current authority prepared by the trusted Store adapter/worker.
/// SQL checks identity/placement; constructing this value is not authentication.
pub struct BotRoutineFinalizeAuthority {
    pub owner: BotOwner,
    pub home: WorkspaceId,
    pub profile: ProfileId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotRoutineOccurrenceCursor {
    pub owner: super::super::BotOwner,
    pub after: i64,
    pub through: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotRoutineOccurrenceRecoveryEntry {
    pub slot: RoutineSlot,
    pub semantic_hash: RoutineDigest32,
}
#[derive(Debug)]
pub struct BotRoutineOccurrenceRecoveryPage {
    pub entries: Vec<Result<BotRoutineOccurrenceRecoveryEntry, BotError>>,
    pub next: Option<BotRoutineOccurrenceCursor>,
}
impl std::fmt::Debug for BotRoutineStoredPreparation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotRoutineStoredPreparation([REDACTED])")
    }
}
impl BotRoutineStoredPreparation {
    /// Materialize the retained identities; this never mints another operation.
    /// The trusted worker supplies its existing prompt-template version.
    pub fn pending_turn(
        &self,
        now: crate::Timestamp,
        prompt_template_version: &str,
    ) -> Result<crate::PendingTurn, BotError> {
        use crate::*;
        self.validate()?;
        if prompt_template_version.is_empty() || prompt_template_version.len() > 128 {
            return Err(BotError::Invalid);
        }
        let ids = self.prepared.ids;
        let config = &self.capture.canonical.config;
        let chat = ChatId::from_uuid(ids.chat);
        let assistant = MessageId::from_uuid(ids.assistant_message);
        let user = Message {
            id: MessageId::from_uuid(ids.user_message),
            chat_id: chat,
            ordinal: 0,
            role: ChatRole::User,
            state: MessageState::Complete,
            markdown: self.prepared.submission.input.text.clone(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: now,
            completed_at: Some(now),
        };
        let reply = Message {
            id: assistant,
            chat_id: chat,
            ordinal: 1,
            role: ChatRole::Assistant,
            state: MessageState::Pending,
            markdown: String::new(),
            origin_message_id: None,
            provider: Some(config.model.provider.clone()),
            requested_model: Some(config.model.requested_model.clone()),
            resolved_model: None,
            error_class: None,
            created_at: now,
            completed_at: None,
        };
        let attempt = InferenceAttempt {
            id: InferenceAttemptId::from_uuid(ids.attempt),
            operation_id: ids.submission.as_uuid(),
            chat_id: Some(chat),
            assistant_message_id: Some(assistant),
            surface: AiSurface::Standalone,
            intent: AiIntent::Chat,
            provider: config.model.provider.clone(),
            requested_model: config.model.requested_model.clone(),
            resolved_model: None,
            requested_reasoning: config.model.default_reasoning,
            effective_reasoning: None,
            prompt_template_version: prompt_template_version.into(),
            status: AttemptStatus::Pending,
            queued_at: now,
            context_started_at: None,
            context_completed_at: None,
            provider_started_at: None,
            first_token_at: None,
            completed_at: None,
            request_payload_bytes: None,
            web_search_requested: false,
            web_search_calls: 0,
            context_payload_bytes: 0,
            message_count: 1,
            context_document_count: 0,
            context_message_block_count: 0,
            usage: Default::default(),
            context_used_tokens: None,
            context_limit_tokens: None,
            stop_reason: None,
            error_class: None,
            provider_response_id: None,
        };
        Ok(PendingTurn {
            user_message: user,
            assistant_message: reply,
            attempt,
            attachment_ids: vec![],
        })
    }
    pub fn validate(&self) -> Result<(), BotError> {
        if self.updated_at_ms < self.created_at_ms
            || bot_routine_preparation_digest(&self.prepared, &self.capture)?
                != self.prepared.preparation_hash
        {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
