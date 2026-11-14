// SPDX-License-Identifier: MPL-2.0

//! Typed access-change receipt. A desired policy is not an effective barrier.
use super::*;
use koru_delegate_core::{BotPolicyBarrier, CapturedBotPolicy};

/// Current authority metadata only. No execution configuration or content.
/// Repository readers validate the bounded projection; this is not a grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotPolicyMetadata {
    pub owner: BotOwner,
    pub id: BotId,
    pub home_workspace: WorkspaceId,
    pub main_chat: ChatId,
    pub revision: u64,
    pub policy_revision: u64,
    pub lifecycle: BotLifecycle,
    pub paused: bool,
    pub access: BotAccess,
    pub selected_pages: Vec<BotPage>,
    /// An unfinished whole-Bot operation, including a parked operation.
    /// Chat-only lifecycle operations do not fence a fresh Routine.
    pub pending_lifecycle: Option<BotRequestId>,
    pub pending_change: Option<(BotRequestId, BotOperationKind)>,
}
impl BotPolicyMetadata {
    pub fn policy(&self) -> CapturedBotPolicy {
        bot_native_policy_from(self.home_workspace, self.access, &self.selected_pages)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotAccessReceipt {
    pub request: BotRequestId,
    pub fingerprint: String,
    pub bot: Bot,
    /// None only for historical completed Configuration receipts.
    pub barrier: Option<BotPolicyBarrier>,
    pub completed: bool,
}
impl BotAccessReceipt {
    pub fn operation(&self) -> BotOperation {
        BotOperation {
            request_id: self.request,
            bot_id: self.bot.id,
            kind: BotOperationKind::UpdateAccess,
            run_id: None,
            status: if self.completed {
                BotOperationStatus::Accepted
            } else {
                BotOperationStatus::Pending
            },
        }
    }
}

/// Canonical current policy only. Task capture is intersected by the delegate.
pub fn bot_native_policy(bot: &Bot) -> CapturedBotPolicy {
    bot_native_policy_from(bot.home_workspace, bot.access, &bot.selected_pages)
}

/// Shared policy construction for full Bot and bounded authority-only reads.
pub fn bot_native_policy_from(
    home: WorkspaceId,
    access: BotAccess,
    pages: &[BotPage],
) -> CapturedBotPolicy {
    CapturedBotPolicy {
        workspace: home,
        destination: home,
        access: match access {
            BotAccess::Default => koru_delegate_core::BotAccess::Default,
            BotAccess::Workspace => koru_delegate_core::BotAccess::Workspace,
        },
        selected_pages: pages.iter().map(|p| p.document).collect(),
    }
}

pub fn validate_access_policy(bot: &Bot, barrier: &BotPolicyBarrier) -> Result<(), BotError> {
    let mut selected = bot.selected_pages.clone();
    normalize_pages(&mut selected, bot.home_workspace)?;
    if selected != bot.selected_pages
        || bot.revision == 0
        || bot.revision > i64::MAX as u64
        || bot.policy_revision == 0
        || bot.policy_revision > i64::MAX as u64
        || barrier.key.owner.user_scope != bot.user_scope
        || barrier.key.owner.device != bot.owner_device
        || barrier.key.bot != bot.id
        || barrier.config_revision != bot.revision
        || barrier.policy_revision != bot.policy_revision
        || barrier.policy != bot_native_policy(bot)
        || !barrier.effects_allowed
    {
        return Err(BotError::Invalid);
    }
    Ok(())
}
