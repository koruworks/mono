// SPDX-License-Identifier: MPL-2.0

//! Current policy evidence and access-change coordination on the existing writer.
pub(in crate::worker::bots) mod dispatch;
pub(in crate::worker) mod epoch;
mod recovery;
pub(in crate::worker::bots) mod routine;
use super::*;
use koru_delegate_api::bots::{BotCommand as DelegateCommand, BotDelegatePort, BotReply};
use koru_delegate_core::{
    BotPolicyBarrier, BotPolicyBarrierKey, BotPolicyBarrierState, BotProvenance, CapturedBotPolicy,
    FreshBotPolicyProof, NativePolicyRequestId, RunId,
};
pub(in crate::worker) use recovery::PolicyRecovery;

/// Internal composition request. No public API or provider principal can create it.
#[derive(Clone)]
pub struct NativePolicyRequest {
    pub run: RunId,
    pub provenance: BotProvenance,
    pub captured: CapturedBotPolicy,
    pub target: Option<koru_ai_core::DocumentId>,
}
#[derive(Clone)]
pub struct NativePolicyEvidence {
    pub proof: FreshBotPolicyProof,
    pub profile: koru_ai_core::ProfileId,
}

#[cfg(test)]
fn epoch_key(bot: &Bot) -> ApiResult<NativePolicyRequestId> {
    epoch::key(owner(bot), bot.id, bot.revision)
}
pub(super) fn barrier(bot: &Bot) -> ApiResult<BotPolicyBarrier> {
    global_barrier(
        owner(bot),
        bot.id,
        bot.revision,
        bot.policy_revision,
        bot_native_policy(bot),
    )
}

/// Construct the same global tuple from canonical identity/policy inputs.
/// The caller owns lifecycle/currentness checks; this value is not authority.
pub(in crate::worker) fn global_barrier(
    owner: BotOwner,
    bot: BotId,
    config_revision: u64,
    policy_revision: u64,
    policy: CapturedBotPolicy,
) -> ApiResult<BotPolicyBarrier> {
    Ok(BotPolicyBarrier {
        key: BotPolicyBarrierKey {
            owner: koru_delegate_core::BotOwner {
                user_scope: owner.user_scope,
                device: owner.device,
            },
            bot,
            request: epoch::key(owner, bot, config_revision)?,
        },
        config_revision,
        policy_revision,
        policy,
        effects_allowed: true, // Current lifecycle is caller-checked; Pause is admission-only.
    })
}

pub(in crate::worker) use routine::current_barrier as current_routine_barrier;
#[cfg(test)]
pub(in crate::worker::bots) use routine::tests::{dispatch_job_fixture, preclaim_fixture};
fn owner(bot: &Bot) -> BotOwner {
    BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    }
}

pub(in crate::worker) enum PolicyWork {
    Lookup {
        request: NativePolicyRequest,
        routine: Option<Box<routine::RoutineLookup>>,
        reply: SyncSender<ApiResult<NativePolicyEvidence>>,
        deadline: Instant,
    },
    Coordinate(BotAccessReceipt),
}
pub(in crate::worker) struct BotPolicyJob {
    bot: Bot,
    work: PolicyWork,
    authority: Arc<dyn BotAuthority>,
    delegate: Option<Arc<dyn BotDelegatePort>>,
}
pub(in crate::worker) struct BotPolicyResult {
    bot: Bot,
    work: PolicyWork,
    result: ApiResult<(
        koru_ai_core::ProfileId,
        Option<koru_delegate_core::BotPolicyBarrierReceipt>,
    )>,
}
impl BotPolicyJob {
    pub(in crate::worker) fn run(self) -> BotPolicyResult {
        let result = catch_unwind(AssertUnwindSafe(|| {
            if matches!(&self.work, PolicyWork::Lookup { deadline, .. } if Instant::now() >= *deadline) {
                return Err(unavailable_failure());
            }
            let pages = match &self.work {
                PolicyWork::Lookup { request, .. } => request.target.map(|document| BotPage { workspace: self.bot.home_workspace, document }).into_iter().collect::<Vec<_>>(),
                PolicyWork::Coordinate(_) => Vec::new(),
            };
            let checked = self.authority.inspect(&[self.bot.home_workspace], &pages).map_err(bot_failure)?;
            if checked.owner != owner(&self.bot) { return Err(bot_failure(BotError::Forbidden)); }
            let profile = checked.workspace(self.bot.home_workspace).map_err(bot_failure)?;
            for page in pages { checked.page(&page).map_err(bot_failure)?; }
            let receipt = match &self.work {
                PolicyWork::Lookup { routine, deadline, .. } => {
                    if let Some(snapshot) = routine {
                        snapshot.verify_delegate(self.delegate.as_deref().ok_or_else(unavailable_failure)?, *deadline)?;
                    }
                    None
                },
                PolicyWork::Coordinate(r) => {
                    let expected = r.barrier.as_ref().ok_or_else(|| bot_failure(BotError::Invalid))?;
                    match self.delegate.as_ref().ok_or_else(unavailable_failure)?.execute(DelegateCommand::SynchronizePolicy { barrier: expected.clone() }).map_err(|_| unavailable_failure())? {
                        BotReply::PolicyBarrier { receipt } if receipt.barrier == *expected => Some(receipt),
                        _ => return Err(unavailable_failure()),
                    }
                }
            };
            Ok((profile, receipt))
        })).unwrap_or_else(|_| Err(unavailable_failure()));
        BotPolicyResult {
            bot: self.bot,
            work: self.work,
            result,
        }
    }
}

impl AiWorkerClient {
    /// Trusted host composition only; deliberately absent from AiServerPort/HTTP.
    pub fn native_policy(&self, request: NativePolicyRequest) -> ApiResult<NativePolicyEvidence> {
        let (reply, response) = sync_channel(1);
        self.sender
            .try_send(Envelope::NativePolicy {
                request,
                reply,
                deadline: Instant::now() + RESPONSE_TIMEOUT,
            })
            .map_err(queue_failure)?;
        receive_api(response)
    }
    /// A settled native effect may unblock a retained access-change receipt.
    pub fn wake_native_policy(&self) {
        let _ = self
            .sender
            .try_send(Envelope::BotPolicyRecover { restart: true });
    }
}

impl WorkerState {
    pub(in crate::worker) fn request_native_policy(
        &mut self,
        request: NativePolicyRequest,
        reply: SyncSender<ApiResult<NativePolicyEvidence>>,
        deadline: Instant,
    ) {
        let prepared = (|| -> ApiResult<_> {
            let p = &request.provenance;
            let bot = self
                .repository
                .bot(p.bot)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            if owner(&bot)
                != (BotOwner {
                    user_scope: p.owner.user_scope,
                    device: p.owner.device,
                })
            {
                return Err(bot_failure(BotError::Forbidden));
            }
            let routine = routine::snapshot(&self.repository, &bot, &request)?;
            let authority = self.bot_authority.clone().ok_or_else(unavailable_failure)?;
            let delegate = if routine.is_some() {
                Some(self.bot_delegate.clone().ok_or_else(unavailable_failure)?)
            } else {
                None
            };
            Ok((bot, authority, routine, delegate))
        })();
        match prepared {
            Ok((bot, authority, routine, delegate)) => {
                let job = BotPolicyJob {
                    bot,
                    authority,
                    delegate,
                    work: PolicyWork::Lookup {
                        request,
                        routine,
                        reply: reply.clone(),
                        deadline,
                    },
                };
                if self
                    .provider_admin
                    .as_ref()
                    .is_none_or(|pool| pool.try_submit(ProviderAdminJob::BotPolicy(job)).is_err())
                {
                    let _ = reply.send(Err(unavailable_failure()));
                }
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    pub(in crate::worker) fn bot_policy_command(
        &mut self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> Option<ApiResult<BotResponse>> {
        let (bot_id, request_id) = match command {
            BotCommand::UpdateAccess {
                bot_id, request_id, ..
            }
            | BotCommand::ReadOperation { bot_id, request_id }
            | BotCommand::ReconcileOperation { bot_id, request_id } => (*bot_id, *request_id),
            _ => return None,
        };
        let result = (|| -> ApiResult<Option<BotAccessReceipt>> {
            authorize(context, AiScope::ManageBots)?;
            let proof = context.prepared.as_ref().ok_or_else(unavailable_failure)?;
            let bot = self
                .repository
                .bot(bot_id)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            validate_owner(&proof.authority, &bot)?;
            let mut normalized = command.clone();
            if let BotCommand::UpdateAccess { selected_pages, .. } = &mut normalized {
                normalize_pages(selected_pages, bot.home_workspace).map_err(bot_failure)?;
                for page in selected_pages {
                    proof.authority.page(page).map_err(bot_failure)?;
                }
            }
            let fingerprint = blake3::hash(
                &serde_json::to_vec(&normalized).map_err(|_| bot_failure(BotError::Invalid))?,
            )
            .to_hex()
            .to_string();
            if let Some(receipt) = self
                .repository
                .bot_access_receipt(owner(&bot), bot_id, request_id)
                .map_err(bot_failure)?
            {
                if matches!(command, BotCommand::UpdateAccess { .. })
                    && receipt.fingerprint != fingerprint
                {
                    return Err(bot_failure(BotError::Conflict));
                }
                return Ok(Some(receipt));
            }
            let BotCommand::UpdateAccess {
                expected_revision,
                expected_policy_revision,
                access,
                selected_pages,
                connections,
                may_ask,
                ..
            } = normalized
            else {
                return Ok(None);
            };
            if bot.revision != expected_revision || bot.policy_revision != expected_policy_revision
            {
                return Err(bot_failure(BotError::RevisionConflict));
            }
            let profile = proof
                .authority
                .workspace(bot.home_workspace)
                .map_err(bot_failure)?;
            let mut desired = bot;
            desired.revision = next_revision(expected_revision)?;
            desired.policy_revision = next_revision(expected_policy_revision)?;
            desired.access = access;
            desired.selected_pages = selected_pages;
            // Absent changes no connection grant; present is the whole set
            // (M5). The repository refuses a new grant that is not usable.
            if let Some(connections) = connections {
                desired.connections = connections;
            }
            // "May ask other bots" (M6). Absent changes nothing; it is a Bot
            // setting, not part of the captured policy.
            if let Some(may_ask) = may_ask {
                desired.may_ask = may_ask;
            }
            desired.updated_at_ms = Timestamp::now().millis();
            let snapshot = barrier(&desired)?;
            self.repository
                .prepare_bot_access(
                    &desired,
                    profile,
                    request_id,
                    &fingerprint,
                    expected_revision,
                    &snapshot,
                )
                .map(Some)
                .map_err(bot_failure)
        })();
        match result {
            Ok(None) => None,
            Ok(Some(receipt)) => {
                if !matches!(command, BotCommand::ReadOperation { .. }) {
                    self.queue_bot_policy(receipt.clone());
                }
                Some(Ok(BotResponse::Operation {
                    operation: receipt.operation(),
                }))
            }
            Err(error) => Some(Err(error)),
        }
    }

    fn queue_bot_policy(&mut self, receipt: BotAccessReceipt) -> bool {
        if receipt.completed
            || self
                .bot_policy_pending
                .contains(&(receipt.bot.id, receipt.request))
        {
            return true;
        }
        let (Some(authority), Some(delegate), Some(pool)) = (
            &self.bot_authority,
            &self.bot_delegate,
            &self.provider_admin,
        ) else {
            self.bot_policy_recovery.defer(Instant::now());
            return false;
        };
        let key = (receipt.bot.id, receipt.request);
        let job = BotPolicyJob {
            bot: receipt.bot.clone(),
            work: PolicyWork::Coordinate(receipt),
            authority: authority.clone(),
            delegate: Some(delegate.clone()),
        };
        if pool.try_submit(ProviderAdminJob::BotPolicy(job)).is_err() {
            self.bot_policy_recovery.defer(Instant::now());
            return false;
        }
        self.bot_policy_pending.insert(key);
        true
    }
    pub(in crate::worker) fn complete_bot_policy(&mut self, result: BotPolicyResult) {
        let current = self.repository.bot(result.bot.id).ok().flatten();
        let same = current.as_ref().is_some_and(|b| {
            owner(b) == owner(&result.bot)
                && b.revision == result.bot.revision
                && b.policy_revision == result.bot.policy_revision
        });
        match result.work {
            PolicyWork::Lookup {
                request,
                routine,
                reply,
                deadline,
            } => {
                // While an Archive is settling the Bot, its own effects-off
                // barrier is the current evidence (P1.7).
                // A journal read that fails refuses the evidence: it never
                // falls back to the ordinary effects-allowed barrier.
                let lifecycle = self.lifecycle_policy_barrier(&result.bot);
                let value = if Instant::now() < deadline {
                    result.result.and_then(|(profile, _)| {
                        routine::recheck(
                            &self.repository,
                            current.as_ref(),
                            &result.bot,
                            &request,
                            routine.as_deref(),
                        )?;
                        Ok(NativePolicyEvidence {
                            proof: FreshBotPolicyProof {
                                global: match lifecycle? {
                                    Some(barrier) => barrier,
                                    None => barrier(&result.bot)?,
                                },
                                routine: routine.as_ref().map(|r| r.barrier()),
                            },
                            profile,
                        })
                    })
                } else {
                    Err(bot_failure(BotError::RevisionConflict))
                };
                let _ = reply.send(value);
            }
            PolicyWork::Coordinate(receipt) => {
                self.request_bot_boot_refresh();
                if let Ok((_, Some(observed))) = &result.result
                    && observed.state == BotPolicyBarrierState::Pending
                {
                    if !self.queue_native_recovery(
                        observed.barrier.key.owner,
                        observed.barrier.key.bot,
                        None,
                        None,
                        koru_delegate_core::NativeDrainReason::PolicyPending {
                            key: observed.barrier.key.clone(),
                        },
                    ) {
                        self.bot_policy_recovery.defer(Instant::now());
                    }
                }
                self.bot_policy_pending
                    .remove(&(receipt.bot.id, receipt.request));
                // What the owner answered, for the Bot read's
                // `pending_change`: a failure's code, or that native effects
                // still hold the barrier pending.
                self.bot_delivery_answers.record(
                    receipt.bot.id,
                    receipt.request,
                    match &result.result {
                        Err(error) => Some(error.code.as_str()),
                        Ok((_, Some(observed)))
                            if observed.state == BotPolicyBarrierState::Pending =>
                        {
                            Some("bot_native_effects_pending")
                        }
                        Ok(_) => None,
                    },
                );
                if !acknowledge_policy_result(&mut self.repository, &receipt, same, result.result) {
                    self.bot_policy_recovery.retry_needed = true;
                }
                if self.bot_policy_recovery.after.is_none() {
                    self.bot_policy_recovery.finish_pass(Instant::now());
                }
            }
        }
        self.recover_bot_policy(false);
    }
    pub(in crate::worker) fn recover_bot_policy(&mut self, restart: bool) {
        if restart {
            self.bot_policy_recovery.restart();
        }
        if !self.bot_policy_recovery.ready(Instant::now()) {
            return;
        }
        let Some(after) = self.bot_policy_recovery.after else {
            return;
        };
        if !self.bot_policy_pending.is_empty() {
            return;
        }
        let Ok(page) = self.repository.pending_bot_access(after, 16) else {
            self.bot_policy_recovery.defer(Instant::now());
            return;
        };
        if page.scanned_through.is_none() {
            self.bot_policy_recovery.finish_pass(Instant::now());
            return;
        }
        for (row, bot, scope, request) in page.pending {
            let previous = self.bot_policy_recovery.after;
            self.bot_policy_recovery.after = Some(row);
            let current = match self.repository.bot(bot) {
                Ok(Some(current)) => current,
                Err(BotError::Storage) => {
                    self.bot_policy_recovery.retry_needed = true;
                    continue;
                }
                _ => continue,
            };
            if current.user_scope != scope {
                continue;
            }
            let receipt = match self
                .repository
                .bot_access_receipt(owner(&current), bot, request)
            {
                Ok(Some(receipt)) => receipt,
                Err(_) => {
                    self.bot_policy_recovery.retry_needed = true;
                    continue;
                }
                Ok(None) => continue,
            };
            if self.queue_bot_policy(receipt) {
                return;
            }
            self.bot_policy_recovery.after = previous;
            self.bot_policy_recovery.defer(Instant::now());
            return;
        }
        self.bot_policy_recovery.after = page.scanned_through;
        if self
            .output
            .try_send(Envelope::BotPolicyRecover { restart: false })
            .is_err()
        {
            self.bot_policy_recovery.defer(Instant::now());
        }
    }
}

/// The sole AI writer acknowledges only current, exact, Effective policy evidence.
/// Any failed acknowledgement remains a durable pending receipt for a later pass.
fn acknowledge_policy_result(
    repository: &mut Repository,
    receipt: &BotAccessReceipt,
    same: bool,
    result: ApiResult<(
        koru_ai_core::ProfileId,
        Option<koru_delegate_core::BotPolicyBarrierReceipt>,
    )>,
) -> bool {
    if same
        && let Ok((_, Some(observed))) = result
        && observed.state == BotPolicyBarrierState::Effective
    {
        repository
            .acknowledge_bot_access(
                owner(&receipt.bot),
                receipt.bot.id,
                receipt.request,
                &observed,
                Timestamp::now().millis(),
            )
            .is_ok()
    } else {
        false
    }
}

#[cfg(test)]
mod tests;
