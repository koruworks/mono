// SPDX-License-Identifier: MPL-2.0
//! A Routine's own access after creation: the grant it runs under now, and
//! the retained receipt of an edit (ai.db 23). The creation grant stays
//! frozen in the creation row; an edit is a separate, later grant.
use super::*;
use crate::bots::{BotAccess, BotOperation, BotOperationKind, BotOperationStatus, BotPage};
use crate::bots::{BotReceiptStage, normalize_pages};

impl BotRoutineGrant {
    /// The bounded public projection of this grant. Never the page list.
    pub fn summary(&self) -> Result<BotRoutineGrantSummary, BotError> {
        Ok(BotRoutineGrantSummary {
            source_bot_policy_revision: self.source_bot_policy_revision,
            grant_revision: self.grant_revision,
            access: match self.policy.access {
                koru_delegate_core::BotAccess::Default => BotAccess::Default,
                koru_delegate_core::BotAccess::Workspace => BotAccess::Workspace,
            },
            selected_page_count: self
                .policy
                .selected_pages
                .len()
                .try_into()
                .map_err(|_| BotError::Invalid)?,
        })
    }
}

/// The grant a Routine runs under now, and the request that produced it.
///
/// Revision 1 is the creation grant, produced by the creation request. A
/// higher revision is an edit, produced by that edit's own request. The
/// request is the identity the delegate's Routine policy barrier is installed
/// under, so it is part of what "current" means.
#[derive(Clone, PartialEq, Eq)]
pub struct BotRoutineCurrentGrant {
    pub grant: BotRoutineGrant,
    pub request: BotRequestId,
}
impl std::fmt::Debug for BotRoutineCurrentGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotRoutineCurrentGrant([REDACTED])")
    }
}
impl BotRoutineCurrentGrant {
    /// A Routine whose access was never edited runs under its creation grant.
    pub fn original(identity: &BotRoutineCreationIdentity) -> Self {
        Self {
            grant: identity.grant.clone(),
            request: identity.creation_request,
        }
    }
    pub fn is_edited(&self) -> bool {
        self.grant.grant_revision > 1
    }
    /// Structure and digest, against the Routine this grant is read for.
    pub fn validate(&self, identity: &BotRoutineCreationIdentity) -> Result<(), BotError> {
        self.grant.validate(identity.home)?;
        if self.grant.canonical_digest()? != self.grant.digest || !v4(self.request.as_uuid()) {
            return Err(BotError::Invalid);
        }
        // Revision 1 is exactly the creation grant under the creation
        // request. An edit is a later revision under its own request.
        let valid = if self.is_edited() {
            self.request != identity.creation_request
        } else {
            self.grant == identity.grant && self.request == identity.creation_request
        };
        if !valid {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
}

/// Trusted writer input for one access edit. The caller has already proved
/// the person's rights to the home and to each selected page; the writer
/// rechecks owner, lifecycle, binding and the grant revision in its
/// transaction. `selected_pages` is normalized (sorted, without duplicates).
#[derive(Clone, PartialEq, Eq)]
pub struct BotRoutineAccessUpdate {
    pub request: BotRequestId,
    pub bot: BotId,
    pub schedule: ScheduleId,
    pub expected_grant_revision: u64,
    pub access: BotAccess,
    pub selected_pages: Vec<BotPage>,
}
impl std::fmt::Debug for BotRoutineAccessUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotRoutineAccessUpdate([REDACTED])")
    }
}
impl BotRoutineAccessUpdate {
    /// The identities and revision alone; the pages need the Routine's home.
    pub fn validate(&self) -> Result<(), BotError> {
        if !v4(self.request.as_uuid())
            || !v4(self.bot.as_uuid())
            || !v4(self.schedule.as_uuid())
            || !revision(self.expected_grant_revision)
            || self.expected_grant_revision >= i64::MAX as u64
            || self.selected_pages.len() > 1024
        {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
    /// The grant this edit asks for: the next revision, in the Routine's
    /// home, recording the Bot policy revision the writer observed. The same
    /// bounds as a Bot's own access change (at most 1024 pages, all in the
    /// home, already normalized).
    pub fn grant(
        &self,
        home: WorkspaceId,
        source_bot_policy_revision: u64,
    ) -> Result<BotRoutineGrant, BotError> {
        self.validate()?;
        let mut normalized = self.selected_pages.clone();
        normalize_pages(&mut normalized, home)?;
        if normalized != self.selected_pages {
            return Err(BotError::Invalid);
        }
        let mut grant = BotRoutineGrant {
            source_bot_policy_revision,
            grant_revision: self.expected_grant_revision + 1,
            policy: CapturedBotPolicy {
                workspace: home,
                destination: home,
                access: match self.access {
                    BotAccess::Default => koru_delegate_core::BotAccess::Default,
                    BotAccess::Workspace => koru_delegate_core::BotAccess::Workspace,
                },
                selected_pages: self
                    .selected_pages
                    .iter()
                    .map(|page| page.document)
                    .collect(),
            },
            digest: RoutineDigest32::from_bytes([0; 32]),
        };
        grant.validate(home)?;
        grant.digest = grant.canonical_digest()?;
        Ok(grant)
    }
}

/// Why an access edit was refused for good. Only a refusal that the same
/// request would meet again is retained: a retry of a busy or pending Routine
/// must be able to succeed, so those are answered and never stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotRoutineAccessRefusal {
    /// The Routine was on.
    NotPaused,
    /// The person may not edit a page the request selected.
    PageForbidden,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotRoutineAccessOutcome {
    /// The grant row was written at `grant_revision` in the same transaction
    /// as this receipt. `grant_digest` is that grant's canonical digest.
    Updated {
        source_bot_policy_revision: u64,
        grant_revision: u64,
        access: BotAccess,
        selected_page_count: u16,
        grant_digest: RoutineDigest32,
    },
    Refused {
        reason: BotRoutineAccessRefusal,
    },
}

/// The small immutable outcome in the shared command receipt. It carries no
/// page list: the grant row holds the grant, and a later edit replaces that
/// row while this receipt stays, so an exact retry still gets its answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRoutineAccessReceipt {
    pub version: u8,
    pub bot: BotId,
    pub schedule: ScheduleId,
    pub expected_grant_revision: u64,
    pub outcome: BotRoutineAccessOutcome,
}
impl BotRoutineAccessReceipt {
    pub fn updated(command: &BotRoutineAccessUpdate, grant: &BotRoutineGrant) -> Result<Self, BotError> {
        let summary = grant.summary()?;
        let receipt = Self {
            version: 1,
            bot: command.bot,
            schedule: command.schedule,
            expected_grant_revision: command.expected_grant_revision,
            outcome: BotRoutineAccessOutcome::Updated {
                source_bot_policy_revision: summary.source_bot_policy_revision,
                grant_revision: summary.grant_revision,
                access: summary.access,
                selected_page_count: summary.selected_page_count,
                grant_digest: grant.digest,
            },
        };
        receipt.validate()?;
        Ok(receipt)
    }
    pub fn refused(
        command: &BotRoutineAccessUpdate,
        reason: BotRoutineAccessRefusal,
    ) -> Result<Self, BotError> {
        command.validate()?;
        let receipt = Self {
            version: 1,
            bot: command.bot,
            schedule: command.schedule,
            expected_grant_revision: command.expected_grant_revision,
            outcome: BotRoutineAccessOutcome::Refused { reason },
        };
        receipt.validate()?;
        Ok(receipt)
    }
    pub fn validate(&self) -> Result<(), BotError> {
        if self.version != 1
            || !v4(self.bot.as_uuid())
            || !v4(self.schedule.as_uuid())
            || !revision(self.expected_grant_revision)
        {
            return Err(BotError::Invalid);
        }
        if let BotRoutineAccessOutcome::Updated {
            source_bot_policy_revision,
            grant_revision,
            selected_page_count,
            ..
        } = &self.outcome
        {
            if !revision(*source_bot_policy_revision)
                || self.expected_grant_revision.checked_add(1) != Some(*grant_revision)
                || !revision(*grant_revision)
                || *selected_page_count > 1024
            {
                return Err(BotError::Invalid);
            }
        }
        bounded(self, BOT_ROUTINE_RECEIPT_BYTES)
    }
    /// Whether `command` is the request this receipt answered: the same
    /// Routine and expected revision and, for an applied edit, the same
    /// resulting grant, recomputed from the command and compared by digest.
    pub fn answers(&self, command: &BotRoutineAccessUpdate, home: WorkspaceId) -> bool {
        if self.bot != command.bot
            || self.schedule != command.schedule
            || self.expected_grant_revision != command.expected_grant_revision
        {
            return false;
        }
        match &self.outcome {
            BotRoutineAccessOutcome::Updated {
                source_bot_policy_revision,
                grant_digest,
                ..
            } => command
                .grant(home, *source_bot_policy_revision)
                .is_ok_and(|grant| grant.digest == *grant_digest),
            BotRoutineAccessOutcome::Refused { .. } => command.validate().is_ok(),
        }
    }
    /// The public summary of the grant an applied edit wrote.
    pub fn summary(&self) -> Option<BotRoutineGrantSummary> {
        match self.outcome {
            BotRoutineAccessOutcome::Updated {
                source_bot_policy_revision,
                grant_revision,
                access,
                selected_page_count,
                ..
            } => Some(BotRoutineGrantSummary {
                source_bot_policy_revision,
                grant_revision,
                access,
                selected_page_count,
            }),
            BotRoutineAccessOutcome::Refused { .. } => None,
        }
    }
}

/// One retained access edit: its receipt and where it stands. `Prepared`
/// means the grant row is written and the delegate's Routine policy barrier
/// for that revision is not yet confirmed installed; `Completed` means it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotRoutineAccessRecord {
    pub request: BotRequestId,
    pub fingerprint: RoutineDigest32,
    pub receipt: BotRoutineAccessReceipt,
    pub stage: BotReceiptStage,
}
impl BotRoutineAccessRecord {
    /// A stage and an outcome that no writer produces together is refused.
    pub fn validate(&self) -> Result<(), BotError> {
        self.receipt.validate()?;
        match (self.stage, &self.receipt.outcome) {
            (
                BotReceiptStage::Prepared | BotReceiptStage::Completed,
                BotRoutineAccessOutcome::Updated { .. },
            )
            | (BotReceiptStage::Rejected, BotRoutineAccessOutcome::Refused { .. }) => Ok(()),
            _ => Err(BotError::Invalid),
        }
    }
    pub fn operation(&self) -> BotOperation {
        BotOperation {
            request_id: self.request,
            bot_id: self.receipt.bot,
            kind: BotOperationKind::UpdateRoutineAccess,
            run_id: None,
            status: match self.stage {
                BotReceiptStage::Prepared => BotOperationStatus::Pending,
                BotReceiptStage::Rejected => BotOperationStatus::Rejected,
                _ => BotOperationStatus::Accepted,
            },
        }
    }
}

/// What the access writer did with one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BotRoutineAccessAdmission {
    /// This call wrote the receipt, `prepared`, and the grant row.
    Prepared(BotRoutineAccessRecord),
    /// The same request with the same content was retained earlier; nothing
    /// was written. Its stage says whether its barrier install is confirmed.
    Retained(BotRoutineAccessRecord),
    /// Another edit of this Routine is still `prepared`. Nothing was written.
    Pending { request: BotRequestId },
}
