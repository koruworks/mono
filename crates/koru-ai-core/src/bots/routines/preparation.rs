// SPDX-License-Identifier: MPL-2.0

use super::super::{BotCapturedMemory, BotKnowledgeCapture};
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRoutineCanonical {
    pub home: WorkspaceId,
    pub config: BotConfig,
    pub config_revision: u64,
    pub policy_revision: u64,
}
impl BotRoutineCanonical {
    pub fn validate(&self) -> Result<(), BotError> {
        if !revision(self.config_revision) || !revision(self.policy_revision) {
            return Err(BotError::Invalid);
        }
        validate_config(&self.config, self.home)
    }
    pub fn matches_bot(&self, bot: &Bot) -> bool {
        self.validate().is_ok()
            && self.home == bot.home_workspace
            && self.config == bot.config
            && self.config_revision == bot.revision
            && self.policy_revision == bot.policy_revision
    }
}

/// Private capture_json envelope. It is never sent over the reverse bridge.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRoutineCaptureV1 {
    pub version: u8,
    pub creation: RoutineRouteIdentity,
    #[serde(with = "strict::Definition")]
    pub schedule_definition: ScheduleDefinition,
    pub canonical: BotRoutineCanonical,
    pub grant: BotRoutineGrant,
    pub library: BotLibraryCapture,
    pub role: BotKnowledgeCapture,
    pub task: Vec<BotKnowledgeCapture>,
}
impl std::fmt::Debug for BotRoutineCaptureV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotRoutineCaptureV1([REDACTED])")
    }
}
impl BotRoutineCaptureV1 {
    pub fn validate(&self, prepared: &RoutinePrepared) -> Result<(), BotError> {
        let submission = &prepared.submission;
        let p = &submission.provenance;
        let home = self.canonical.home;
        let association = RoutineAssociation {
            owner: self.creation.owner,
            bot: self.creation.bot,
            home: self.creation.workspace,
            creation_request: self.creation.creation_request,
        };
        self.canonical.validate()?;
        self.grant.validate(home)?;
        self.library.validate(home)?;
        super::creation::validate_definition(&self.schedule_definition, &association)?;
        if self.version != 1
            || !prepared.valid()
            || !self.creation.matches_slot(&prepared.slot)
            || self.creation.workspace != home
            || submission.workspace != home
            || self.schedule_definition.revision != prepared.definition.revision
            || self.schedule_definition.effective_at_ms > prepared.slot.occurrence_ms
            || self.schedule_definition.lifecycle != ScheduleLifecycle::Active
            || self.canonical.config_revision != p.config_revision
            || self.canonical.policy_revision != p.policy_revision
            || self.grant.policy != submission.policy
            || self.grant.source_bot_policy_revision > self.canonical.policy_revision
            || p.routine
                .is_none_or(|r| r.grant_revision != self.grant.grant_revision)
            || self.library.owner.user_scope != p.owner.user_scope
            || self.library.owner.device != p.owner.device
            || self.library.bot != p.bot
            || self.library.submission != p.submission
            || self.library.config_revision != p.config_revision
            || self.role.page != self.canonical.config.role
            || self.role.page.workspace != home
            || self.task.len() > 16
            || self.schedule_definition.template.context.len() != self.task.len()
            || submission.request.objective != self.schedule_definition.template.objective
            || Objective::new(&submission.input.text).ok().as_ref()
                != Some(&submission.request.objective)
            || !matches!(&submission.request.scope, WorkspaceScope::BotFolder { folder } if folder.as_str() == self.canonical.config.working_folder)
            || submission
                .request
                .requested_model
                .as_ref()
                .map(|m| m.as_str())
                != Some(self.canonical.config.model.requested_model.as_str())
            || submission.input.model.as_ref().map(|m| m.as_str())
                != Some(self.canonical.config.model.requested_model.as_str())
            || submission.input.mode.as_ref().map(|m| m.as_str())
                != self.canonical.config.mode.as_deref()
            || submission.input.fast != self.canonical.config.fast
        {
            return Err(BotError::Invalid);
        }
        // This is the managed provider's existing id/profile correspondence,
        // not a new availability registry or permission check.
        if !matches!(&submission.request.agent, SessionProfileKey::BuiltIn(key)
            if Some(key.as_str()) == self.canonical.config.model.provider.as_str().strip_prefix("agent-"))
            || submission
                .input
                .reasoning
                .map(|r| r.as_str())
                .unwrap_or("default")
                != self.canonical.config.model.default_reasoning.as_token()
            || !submission.input.attachment_ids.is_empty()
            || !submission.input.references.is_empty()
            || !submission.input.skills.is_empty()
        {
            return Err(BotError::Invalid);
        }
        let mut expected = BTreeMap::new();
        let mut insert = |pin: &BotKnowledgeCapture| -> Result<(), BotError> {
            if pin.page.workspace != home {
                return Err(BotError::Invalid);
            }
            match expected.insert(pin.page.document, pin.version) {
                Some(prior) if prior != pin.version => Err(BotError::Invalid),
                _ => Ok(()),
            }
        };
        insert(&self.role)?;
        for pin in &self.library.knowledge {
            insert(pin)?;
        }
        let mut task_documents = std::collections::BTreeSet::new();
        for (pin, pointer) in self
            .task
            .iter()
            .zip(&self.schedule_definition.template.context)
        {
            if !task_documents.insert(pin.page.document)
                || !matches!(pointer, koru_delegate_schedule::ScheduleContextPointer::Document { workspace, document }
                    if *workspace == pin.page.workspace && *document == pin.page.document)
            {
                return Err(BotError::Invalid);
            }
            insert(pin)?;
        }
        if expected.len() > 33 || submission.request.context.len() != expected.len() {
            return Err(BotError::Invalid);
        }
        let mut actual = BTreeMap::new();
        for pin in &submission.request.context {
            match pin {
                RunContextItem::Document {
                    workspace,
                    document,
                    version,
                } if *workspace == home && actual.insert(*document, *version).is_none() => {}
                _ => return Err(BotError::Invalid),
            }
        }
        if actual != expected {
            return Err(BotError::Invalid);
        }
        if let BotCapturedMemory::Captured { page, .. } = &self.library.memory {
            if expected.contains_key(&page.document) {
                return Err(BotError::Invalid);
            }
        }
        bounded(submission, BOT_ROUTINE_SUBMISSION_BYTES)?;
        bounded(self, BOT_ROUTINE_CAPTURE_BYTES)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotRoutinePreparationStage {
    Prepared {},
    Linked {
        acknowledgement: RoutineLinkAcknowledgement,
    },
    Closed {
        reason: BotRoutineCloseReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRoutinePreparation {
    pub prepared: RoutinePrepared,
    pub capture: BotRoutineCaptureV1,
    pub stage: BotRoutinePreparationStage,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
impl BotRoutinePreparation {
    pub fn validate(&self) -> Result<(), BotError> {
        self.capture.validate(&self.prepared)?;
        if let BotRoutinePreparationStage::Linked { acknowledgement } = &self.stage {
            if !acknowledgement.claim.matches_prepared(&self.prepared) {
                return Err(BotError::Invalid);
            }
        }
        Ok(())
    }

    /// Pure identity/deadline check on independently authenticated evidence.
    /// Callers must still obtain the observation from the current delegate owner.
    pub fn can_close(
        &self,
        reason: BotRoutineCloseReason,
        observed: &RoutineClaimObservation,
        now_ms: i64,
    ) -> bool {
        if self.validate().is_err()
            || !matches!(self.stage, BotRoutinePreparationStage::Prepared {})
            || !observed.valid()
            || observed.slot() != &self.prepared.slot
        {
            return false;
        }
        match observed {
            RoutineClaimObservation::Skipped { .. } => reason == BotRoutineCloseReason::SlotSkipped,
            RoutineClaimObservation::Claimed { record } => {
                reason == BotRoutineCloseReason::OtherWinner
                    && !record.claim.matches_prepared(&self.prepared)
            }
            RoutineClaimObservation::Absent { observed_at_ms, .. } => {
                !matches!(
                    reason,
                    BotRoutineCloseReason::SlotSkipped | BotRoutineCloseReason::OtherWinner
                ) && self
                    .prepared
                    .slot
                    .occurrence_ms
                    .checked_add(60_000)
                    .is_some_and(|deadline| {
                        *observed_at_ms >= deadline && now_ms >= *observed_at_ms
                    })
            }
        }
    }
}
