// SPDX-License-Identifier: MPL-2.0
//! Vouched owner metadata commands. Store effects use the existing recovery lane.
use super::*;

/// Count and hash the complete owner command with the existing bare JSON hash
/// convention. This adds no new domain and changes no ordinary command bytes.
pub(in crate::worker) fn create_body(
    command: &BotCommand,
) -> ApiResult<(BotRoutineCreate, koru_delegate_core::RoutineDigest32)> {
    let BotCommand::CreateRoutine {
        request_id,
        bot_id,
        expected_bot_revision,
        name,
        objective,
        documents,
        recurrence,
    } = command
    else {
        return Err(bot_failure(BotError::Invalid));
    };
    if documents.len() > 16 {
        return Err(bot_failure(BotError::Invalid));
    }
    let input = BotRoutineCreate {
        request: *request_id,
        bot: *bot_id,
        expected_bot_revision: *expected_bot_revision,
        name: name.clone(),
        objective: objective.clone(),
        documents: documents.clone(),
        recurrence: recurrence.clone(),
    };
    input.validate().map_err(bot_failure)?;
    let fingerprint = koru_delegate_core::routine_digest_json(b"", command, BOT_ROUTINE_SEED_BYTES)
        .map_err(|error| {
            bot_failure(match error {
                koru_delegate_core::RoutineDigestError::TooLarge => BotError::ContextTooLarge,
                koru_delegate_core::RoutineDigestError::Invalid => BotError::Invalid,
            })
        })?;
    Ok((input, fingerprint))
}

/// Resolve immutable identity before any fresh page/capture/source work. Owner
/// and home metadata authorization still applies to every command.
pub(in crate::worker) fn retained_create(
    repository: &Repository,
    owner: BotOwner,
    command: &BotCommand,
) -> ApiResult<bool> {
    let (input, fingerprint) = create_body(command)?;
    let Some(creation) = repository
        .bot_routine_creation_by_request(owner, input.bot, input.request)
        .map_err(bot_failure)?
    else {
        return Ok(false);
    };
    if creation.identity.command_fingerprint != fingerprint
        || !input.matches_retained(&creation.identity)
    {
        return Err(bot_failure(BotError::Conflict));
    }
    Ok(true)
}

impl WorkerState {
    pub(in crate::worker) fn prepare_routine_creation_intent(
        &mut self,
        owner: BotOwner,
        input: &BotRoutineCreate,
        fingerprint: koru_delegate_core::RoutineDigest32,
    ) -> ApiResult<BotRoutineCreationReceipt> {
        let retained = self
            .repository
            .bot_routine_creation_by_request(owner, input.bot, input.request)
            .map_err(bot_failure)?
            .is_some();
        let receipt = self
            .repository
            .prepare_bot_routine_creation(owner, input, fingerprint, Timestamp::now().millis())
            .map_err(bot_failure)?;
        if !retained {
            self.routine_recovery.observe(owner);
            self.routine_recovery.wake();
        }
        Ok(receipt)
    }

    pub(in crate::worker) fn bot_routine_creation_command(
        &mut self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> Option<ApiResult<BotResponse>> {
        let id = match command {
            BotCommand::CreateRoutine { bot_id, .. }
            | BotCommand::ReadRoutineCreation { bot_id, .. }
            | BotCommand::RetryRoutineCreation { bot_id, .. } => *bot_id,
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
            match command {
                BotCommand::CreateRoutine { .. } => {
                    let (input, fingerprint) = create_body(command)?;
                    if !retained_create(&self.repository, owner, command)? {
                        for document in &input.documents {
                            proof
                                .authority
                                .page(&BotPage {
                                    workspace: bot.home_workspace,
                                    document: *document,
                                })
                                .map_err(bot_failure)?;
                        }
                    }
                    let receipt =
                        self.prepare_routine_creation_intent(owner, &input, fingerprint)?;
                    return Ok(BotResponse::RoutineCreated {
                        receipt: (&receipt).into(),
                    });
                }
                BotCommand::ReadRoutineCreation { request_id, .. } => self
                    .routine_creation_status(owner, &bot, *request_id)
                    .map(|creation| BotResponse::RoutineCreation { creation }),
                BotCommand::RetryRoutineCreation {
                    original_request_id,
                    observed_token,
                    ..
                } => {
                    let creation = self
                        .repository
                        .bot_routine_creation_by_request(owner, id, *original_request_id)
                        .map_err(bot_failure)?
                        .ok_or_else(not_found)?;
                    if creation.observed() != *observed_token {
                        return Err(bot_failure(BotError::Conflict));
                    }
                    match creation.state {
                        BotRoutineCreationState::Prepared
                        | BotRoutineCreationState::Unconfirmed => {
                            self.repository
                                .bot_routine_recovery_candidate(
                                    owner,
                                    &BotRoutineRecoveryIdentity {
                                        bot: id,
                                        schedule: creation.identity.schedule,
                                        observed: *observed_token,
                                    },
                                )
                                .map_err(bot_failure)?;
                            self.routine_recovery.observe(owner);
                            self.routine_recovery.wake();
                        }
                        BotRoutineCreationState::Bound => {}
                        BotRoutineCreationState::Conflict => {
                            return Err(bot_failure(BotError::Conflict));
                        }
                    }
                    self.routine_creation_status(owner, &bot, *original_request_id)
                        .map(|creation| BotResponse::RoutineCreation { creation })
                }
                _ => unreachable!(),
            }
        })())
    }

    fn routine_creation_status(
        &self,
        owner: BotOwner,
        bot: &Bot,
        request: BotRequestId,
    ) -> ApiResult<BotRoutineCreationStatus> {
        let creation = self
            .repository
            .bot_routine_creation_by_request(owner, bot.id, request)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        let can_retry = if bot.lifecycle == BotLifecycle::Active
            && matches!(
                creation.state,
                BotRoutineCreationState::Prepared | BotRoutineCreationState::Unconfirmed
            ) {
            match self.repository.bot_routine_recovery_candidate(
                owner,
                &BotRoutineRecoveryIdentity {
                    bot: bot.id,
                    schedule: creation.identity.schedule,
                    observed: creation.observed(),
                },
            ) {
                Ok(_) => true,
                Err(
                    BotError::Forbidden
                    | BotError::Conflict
                    | BotError::Settling
                    | BotError::RevisionConflict,
                ) => false,
                Err(error) => return Err(bot_failure(error)),
            }
        } else {
            false
        };
        Ok(BotRoutineCreationStatus {
            request_id: request,
            bot_id: bot.id,
            schedule_id: creation.identity.schedule,
            observed_token: creation.observed(),
            refusal: creation.refusal,
            can_retry,
            source_bot_policy_revision: creation.identity.grant.source_bot_policy_revision,
            grant_revision: creation.identity.grant.grant_revision,
        })
    }
}
