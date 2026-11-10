// SPDX-License-Identifier: MPL-2.0
//! Branch a Bot conversation into another conversation with the same Bot
//! (P1.6). It reuses the shared Branch copy and launches nothing: no attempt,
//! Run, replay position, tool activity or approval is copied or created.
use super::*;

impl WorkerState {
    pub(in crate::worker) fn branch_bot_chat(
        &mut self,
        context: CommandContext,
        bot_id: BotId,
        source_chat: ChatId,
        request: BranchChatRequest,
    ) -> ApiResult<BranchPlan> {
        authorize(&context, AiScope::ManageBots)?;
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        let bot = self
            .repository
            .bot(bot_id)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        validate_owner(&proof.authority, &bot)?;
        Self::require_branch_title(&request)?;
        let destination = request.branch_chat_id;
        let origin = BranchOrigin {
            chat_id: source_chat,
            through_message_id: request.through_message_id,
        };
        require_bot_branch_destination(&self.repository, bot.id, destination)?;
        if self
            .repository
            .bot_for_chat(destination)
            .map_err(bot_failure)?
            == Some(bot.id)
        {
            // An exact retry finds the branch it made; any other reuse conflicts.
            let existing = self
                .repository
                .chat_with_access(destination, &ChatReadAccess::authorized(vec![destination]))
                .map_err(ai_failure)?
                .ok_or_else(not_found)?;
            if existing.branched_from.as_ref() != Some(&origin) || existing.title != request.title {
                return Err(destination_conflict());
            }
            return self
                .read_chat(context, destination)
                .map(|transcript| BranchPlan::Done(BranchChatResponse { transcript }));
        }
        // An acknowledged steer may still be writing its history row (O20).
        self.require_bot_steering_settled(source_chat)?;
        let access = self.bot_read_access(&context)?;
        let source = self
            .repository
            .chat_with_access(source_chat, &access)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        let profile = proof
            .authority
            .workspace(bot.home_workspace)
            .map_err(bot_failure)?;
        if source.profile_id != Some(profile) {
            return Err(bot_failure(BotError::Forbidden));
        }
        let now = Timestamp::now();
        let mut chat = Chat {
            id: destination,
            title: request.title,
            private: source.private,
            current_model: source.current_model,
            branched_from: Some(origin),
            agent_working_folder: source.agent_working_folder,
            agent_mode: None,
            agent_fast: None,
            pinned_at: None,
            profile_id: source.profile_id,
            created_at: now,
            updated_at: now,
        };
        seed_bot_branch_settings(&mut chat, &bot.config);
        let (copy, folders) = self.branch_copy(source_chat, chat)?;
        if !folders.is_empty() {
            // Bot attachment delivery is not built; nothing is staged or written.
            return Err(failure(
                "unsupported_capability",
                "Branching a Bot conversation with attached files is not available yet.",
                false,
            ));
        }
        self.repository
            .create_bot_branch(&bot, &copy)
            .map_err(bot_failure)?;
        self.read_chat(context, destination)
            .map(|transcript| BranchPlan::Done(BranchChatResponse { transcript }))
    }
}

/// O24: a Bot branch keeps the source's model and reasoning, as every branch
/// does, but starts in the Bot's current mode and Fast setting instead of the
/// provider defaults an ordinary branch resets to. This is the only Bot-specific
/// seeding; removing it restores the shared reset.
fn seed_bot_branch_settings(chat: &mut Chat, config: &BotConfig) {
    chat.agent_mode = config.mode.clone();
    chat.agent_fast = config.fast;
}

/// Ordinary-safe Chat reads hide Bot rows, so a destination is checked by its
/// raw association: an ordinary source keeps the ordinary rule, and a Bot
/// source may only reach an unused identity or its own earlier branch.
pub(super) fn require_branch_destination(
    repository: &Repository,
    source: ChatId,
    destination: ChatId,
) -> ApiResult<()> {
    match repository.bot_for_chat(source).map_err(bot_failure)? {
        None => {
            super::super::attachments::require_ordinary_branch_destination(repository, destination)
        }
        Some(bot) => require_bot_branch_destination(repository, bot, destination),
    }
}

fn require_bot_branch_destination(
    repository: &Repository,
    bot: BotId,
    destination: ChatId,
) -> ApiResult<()> {
    match repository.bot_for_chat(destination).map_err(bot_failure)? {
        Some(owner) if owner == bot => Ok(()),
        Some(_) => Err(destination_conflict()),
        None if repository.chat(destination).map_err(ai_failure)?.is_some() => {
            Err(destination_conflict())
        }
        None => Ok(()),
    }
}

fn destination_conflict() -> Failure {
    failure(
        "idempotency_conflict",
        "This chat identity was already used for another branch.",
        false,
    )
}
