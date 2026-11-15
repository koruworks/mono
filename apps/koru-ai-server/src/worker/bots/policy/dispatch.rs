// SPDX-License-Identifier: MPL-2.0

//! Per-Run execution synchronization on the existing boot/chat job lanes.
//! Exact acknowledgements are snapshots, not remote latest-policy authority.
use super::*;

#[derive(Clone)]
pub(in crate::worker::bots) struct RoutineDispatch {
    bot: Bot,
    request: NativePolicyRequest,
    lookup: Box<routine::RoutineLookup>,
    global: BotPolicyBarrier,
    profile: koru_ai_core::ProfileId,
}
impl RoutineDispatch {
    pub(in crate::worker::bots) fn snapshot(
        repository: &Repository,
        submission: &koru_delegate_core::BotSubmission,
    ) -> ApiResult<Self> {
        let bot = repository
            .bot(submission.provenance.bot)
            .map_err(bot_failure)?
            .filter(|bot| bot.lifecycle == BotLifecycle::Active)
            .ok_or_else(not_found)?;
        let request = NativePolicyRequest {
            run: submission.request.run,
            provenance: submission.provenance.clone(),
            captured: submission.policy.clone(),
            target: None,
        };
        let lookup = routine::snapshot(repository, &bot, &request)?
            .ok_or_else(|| bot_failure(BotError::Forbidden))?;
        let profile = lookup.execution_profile(submission)?;
        let global = barrier(&bot)?;
        Ok(Self {
            bot,
            request,
            lookup,
            global,
            profile,
        })
    }

    pub(in crate::worker::bots) fn recheck(&self, repository: &Repository) -> ApiResult<()> {
        let current = repository.bot(self.bot.id).map_err(bot_failure)?;
        if current
            .as_ref()
            .is_none_or(|bot| bot.lifecycle != BotLifecycle::Active)
        {
            return Err(bot_failure(BotError::Forbidden));
        }
        routine::recheck(
            repository,
            current.as_ref(),
            &self.bot,
            &self.request,
            Some(&self.lookup),
        )
    }

    pub(in crate::worker::bots) fn synchronize(
        &self,
        authority: &dyn BotAuthority,
        delegate: &dyn BotDelegatePort,
        deadline: Instant,
    ) -> ApiResult<()> {
        let independent = self.lookup.barrier();
        let pages = independent
            .policy
            .selected_pages
            .iter()
            .map(|document| BotPage {
                workspace: self.bot.home_workspace,
                document: *document,
            })
            .collect::<Vec<_>>();
        let human = || {
            if Instant::now() >= deadline {
                return Err(unavailable_failure());
            }
            let checked = authority
                .inspect(&[self.bot.home_workspace], &pages)
                .map_err(bot_failure)?;
            if checked.owner != owner(&self.bot)
                || checked
                    .workspace(self.bot.home_workspace)
                    .map_err(bot_failure)?
                    != self.profile
            {
                return Err(bot_failure(BotError::Forbidden));
            }
            for page in &pages {
                checked.page(page).map_err(bot_failure)?;
            }
            if Instant::now() >= deadline {
                return Err(unavailable_failure());
            }
            Ok(())
        };
        human()?;
        self.lookup.verify_delegate(delegate, deadline)?;
        let call = |command| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(unavailable_failure());
            }
            let reply = delegate
                .execute_within(command, remaining)
                .map_err(|_| unavailable_failure())?;
            if Instant::now() >= deadline {
                return Err(unavailable_failure());
            }
            Ok(reply)
        };
        match call(DelegateCommand::SynchronizePolicy {
            barrier: self.global.clone(),
        })? {
            BotReply::PolicyBarrier { receipt }
                if receipt.barrier == self.global
                    && receipt.state == BotPolicyBarrierState::Effective => {}
            _ => return Err(unavailable_failure()),
        }
        match call(DelegateCommand::SynchronizeRoutinePolicy {
            barrier: independent.clone(),
        })? {
            BotReply::RoutinePolicyBarrier { barrier, state }
                if barrier == independent && state == BotPolicyBarrierState::Effective => {}
            _ => return Err(unavailable_failure()),
        };
        human()
    }
}
