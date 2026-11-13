// SPDX-License-Identifier: MPL-2.0
//! Metadata-only owner controls; Store effects are scheduled after durable intent.
use super::super::*;

impl WorkerState {
    pub(in crate::worker) fn bot_memory_command(
        &mut self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> Option<ApiResult<BotResponse>> {
        let id = match command {
            BotCommand::ReadMemory { bot_id }
            | BotCommand::EnsureMemory { bot_id, .. }
            | BotCommand::RetryMemory { bot_id, .. }
            | BotCommand::AbandonMemory { bot_id, .. } => *bot_id,
            _ => return None,
        };
        Some((|| {
            authorize(context, AiScope::ManageBots)?;
            let proof = context
                .prepared
                .as_ref()
                .ok_or_else(|| bot_failure(BotError::Unavailable))?;
            let bot = lifecycle::readable_bot(&self.repository, id)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            validate_owner(&proof.authority, &bot)?;
            let owner = BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            };
            self.memory_recovery.observe(owner);
            match command {
                BotCommand::ReadMemory { .. } => {}
                BotCommand::EnsureMemory {
                    request_id,
                    expected_revision,
                    title,
                    ..
                } => {
                    let seed =
                        crate::bots::memory_seed(bot.home_workspace, title).map_err(bot_failure)?;
                    let fingerprint = library::command_fingerprint(command)?;
                    self.repository
                        .prepare_bot_memory_creation(
                            owner,
                            id,
                            *request_id,
                            *expected_revision,
                            &seed,
                            &fingerprint,
                            Timestamp::now().millis(),
                        )
                        .map_err(bot_failure)?;
                    self.memory_recovery.wake();
                }
                BotCommand::RetryMemory {
                    creation_request,
                    observed_state,
                    observed_updated_at_ms,
                    ..
                }
                | BotCommand::AbandonMemory {
                    creation_request,
                    observed_state,
                    observed_updated_at_ms,
                    ..
                } => {
                    let creation = self
                        .repository
                        .bot_library(owner, id)
                        .map_err(bot_failure)?
                        .creation
                        .ok_or_else(|| bot_failure(BotError::NotFound))?;
                    let observed = BotMemoryObserved {
                        state: *observed_state,
                        updated_at_ms: *observed_updated_at_ms,
                    };
                    if creation.identity.creation_request != *creation_request
                        || creation.observed() != observed
                    {
                        return Err(bot_failure(BotError::Conflict));
                    }
                    let retry = matches!(command, BotCommand::RetryMemory { .. });
                    if retry
                        && matches!(
                            creation.state,
                            BotMemoryState::Prepared | BotMemoryState::Unconfirmed
                        )
                    {
                        self.repository
                            .bot_memory_recovery_candidate(
                                owner,
                                &BotMemoryRecoveryIdentity {
                                    bot: id,
                                    request: *creation_request,
                                    observed,
                                },
                            )
                            .map_err(bot_failure)?;
                    } else {
                        let transition = if retry {
                            BotMemoryTransition::Retry
                        } else {
                            BotMemoryTransition::Abandon
                        };
                        self.repository
                            .transition_bot_memory_creation(
                                &creation.identity,
                                observed,
                                transition,
                                Timestamp::now().millis(),
                            )
                            .map_err(bot_failure)?;
                    }
                    if retry {
                        self.memory_recovery.wake();
                    }
                }
                _ => unreachable!(),
            }
            self.memory_status(owner, &bot)
                .map(|status| BotResponse::MemoryStatus { status })
        })())
    }

    fn memory_status(&self, owner: BotOwner, bot: &Bot) -> ApiResult<BotMemoryStatus> {
        let library = self
            .repository
            .bot_library(owner, bot.id)
            .map_err(bot_failure)?;
        let writable = bot.lifecycle == BotLifecycle::Active
            && self
                .repository
                .bot_memory_mutation_ready(owner, bot.id)
                .map_err(bot_failure)?;
        let state = match library.creation {
            None => BotMemoryStatusState::NotStarted {
                can_create: writable,
            },
            Some(creation) => BotMemoryStatusState::Candidate {
                creation_request: creation.identity.creation_request,
                candidate: BotPage {
                    workspace: creation.identity.workspace,
                    document: creation.identity.candidate,
                },
                state: creation.state,
                observed_updated_at_ms: creation.updated_at_ms,
                refusal: creation.refusal,
                can_retry: writable && creation.state != BotMemoryState::Bound,
                can_abandon: writable
                    && matches!(
                        creation.state,
                        BotMemoryState::Prepared
                            | BotMemoryState::Unconfirmed
                            | BotMemoryState::Conflict
                    ),
            },
        };
        Ok(BotMemoryStatus {
            bot_id: bot.id,
            revision: bot.revision,
            state,
        })
    }
}
