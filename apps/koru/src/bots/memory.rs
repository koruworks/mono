// SPDX-License-Identifier: MPL-2.0

//! One bounded Memory controller per Bot view. No Store access or background poll.
use super::link::Failure;
use koru_ai_core::bots::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Status,
    Ensure,
    Retry,
    Abandon,
    List,
    Detail,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub instance: u64,
    pub serial: u64,
    pub bot: BotId,
    pub revision: u64,
    pub kind: Kind,
    pub command: BotCommand,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Notice {
    Unavailable,
    Unknown,
    Reconcile,
    Refused,
    Expired,
    Unsupported,
    NotFound,
}

#[derive(Default)]
pub(crate) struct Memory {
    subject: Option<(BotId, BotOwner, koru_store_core::ids::WorkspaceId)>,
    known_revision: u64,
    serial: u64,
    visible: bool,
    owed: bool,
    pub status: Option<BotMemoryStatus>,
    status_valid: bool,
    ensure: Option<BotCommand>,
    pending: Option<Request>,
    retired: Option<Request>,
    suppress_detail: bool,
    reconcile_used: bool,
    confirmation: Option<Request>,
    pub notice: Option<Notice>,
    pub list: Option<BotMemoryListPage>,
    pub detail: Option<BotMemoryDetailPage>,
    pub list_stale: bool,
    pub detail_stale: bool,
    pub body_serial: u64,
}
impl Memory {
    pub fn know(&mut self, bot: &Bot) {
        let subject = (
            bot.id,
            BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            },
            bot.home_workspace,
        );
        if self.subject.as_ref().is_some_and(|old| old != &subject) {
            let serial = self.serial;
            let retired = self.pending.take().or_else(|| self.retired.take());
            *self = Self::default();
            self.serial = serial;
            self.retired = retired;
        }
        self.subject = Some(subject);
        if bot.revision > self.known_revision {
            self.known_revision = bot.revision;
            self.status_valid = false;
            self.owed = true;
            self.reconcile_used = false;
            self.list_stale = self.list.is_some();
            self.detail_stale = self.detail.is_some();
        }
    }
    pub fn known_revision(&self) -> u64 {
        self.known_revision
    }
    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.retired.is_some()
    }
    pub fn show(&mut self, visible: bool) {
        if visible && !self.visible {
            self.owed = true;
        }
        self.visible = visible;
    }
    pub fn refresh(&mut self) {
        self.owed = true;
        self.reconcile_used = false;
    }
    pub fn uncertain_ensure(&self) -> bool {
        self.ensure.is_some()
    }
    pub fn current(&self, bot: &Bot) -> bool {
        self.status_valid
            && self.status.as_ref().is_some_and(|s| {
                s.bot_id == bot.id
                    && s.revision == bot.revision
                    && s.revision >= self.known_revision
            })
    }
    fn editable(&self, bot: &Bot) -> bool {
        self.current(bot)
            && bot.lifecycle == BotLifecycle::Active
            && bot.pending_change.is_none()
            && bot.held_by.is_none()
            && !self.busy()
            && self.confirmation.is_none()
    }
    pub fn can_create(&self, bot: &Bot) -> bool {
        self.editable(bot)
            && self.ensure.is_none()
            && matches!(
                self.status.as_ref().map(|s| &s.state),
                Some(BotMemoryStatusState::NotStarted { can_create: true })
            )
    }
    pub fn can_retry(&self, bot: &Bot) -> bool {
        self.editable(bot)
            && matches!(
                self.status.as_ref().map(|s| &s.state),
                Some(BotMemoryStatusState::Candidate {
                    can_retry: true,
                    ..
                })
            )
    }
    pub fn can_abandon(&self, bot: &Bot) -> bool {
        self.editable(bot)
            && matches!(
                self.status.as_ref().map(|s| &s.state),
                Some(BotMemoryStatusState::Candidate {
                    can_abandon: true,
                    ..
                })
            )
    }
    pub fn page(&self) -> Option<&BotPage> {
        match &self.status.as_ref()?.state {
            BotMemoryStatusState::Candidate { candidate, .. } => Some(candidate),
            _ => None,
        }
    }
    pub fn bound(&self) -> bool {
        matches!(
            self.status.as_ref().map(|s| &s.state),
            Some(BotMemoryStatusState::Candidate {
                state: BotMemoryState::Bound,
                ..
            })
        )
    }
    fn request(&mut self, instance: u64, bot: BotId, kind: Kind, command: BotCommand) -> Request {
        self.serial = self
            .serial
            .checked_add(1)
            .expect("Memory operation serial exhausted");
        Request {
            instance,
            serial: self.serial,
            bot,
            revision: self.known_revision,
            kind,
            command,
        }
    }
    fn start(
        &mut self,
        instance: u64,
        bot: BotId,
        kind: Kind,
        command: BotCommand,
    ) -> Option<Request> {
        if self.busy() || self.confirmation.is_some() {
            return None;
        }
        let request = self.request(instance, bot, kind, command);
        self.pending = Some(request.clone());
        self.suppress_detail = false;
        if kind == Kind::Status {
            self.status_valid = false;
        }
        self.notice = None;
        Some(request)
    }
    pub fn read(&mut self, instance: u64, bot: &Bot) -> Option<Request> {
        if !self.visible || !self.owed || bot.lifecycle == BotLifecycle::Trashed {
            return None;
        }
        let request = self.start(
            instance,
            bot.id,
            Kind::Status,
            BotCommand::ReadMemory { bot_id: bot.id },
        )?;
        self.owed = false;
        Some(request)
    }
    pub fn create(&mut self, instance: u64, bot: &Bot, title: String) -> Option<Request> {
        if !self.can_create(bot) {
            return None;
        }
        let command = BotCommand::EnsureMemory {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: bot.revision,
            title,
        };
        self.ensure = Some(command.clone());
        self.start(instance, bot.id, Kind::Ensure, command)
    }
    pub fn retry_ensure(&mut self, instance: u64, bot: &Bot) -> Option<Request> {
        if bot.lifecycle != BotLifecycle::Active
            || bot.pending_change.is_some()
            || bot.held_by.is_some()
        {
            return None;
        }
        self.start(instance, bot.id, Kind::Ensure, self.ensure.clone()?)
    }
    fn control(&self, bot: BotId, abandon: bool) -> Option<BotCommand> {
        let BotMemoryStatusState::Candidate {
            creation_request,
            state,
            observed_updated_at_ms,
            ..
        } = &self.status.as_ref()?.state
        else {
            return None;
        };
        Some(if abandon {
            BotCommand::AbandonMemory {
                bot_id: bot,
                creation_request: *creation_request,
                observed_state: *state,
                observed_updated_at_ms: *observed_updated_at_ms,
            }
        } else {
            BotCommand::RetryMemory {
                bot_id: bot,
                creation_request: *creation_request,
                observed_state: *state,
                observed_updated_at_ms: *observed_updated_at_ms,
            }
        })
    }
    pub fn retry(&mut self, instance: u64, bot: &Bot) -> Option<Request> {
        if !self.can_retry(bot) {
            return None;
        }
        self.start(instance, bot.id, Kind::Retry, self.control(bot.id, false)?)
    }
    pub fn ask_abandon(&mut self, instance: u64, bot: &Bot) -> Option<Request> {
        if !self.can_abandon(bot) {
            return None;
        }
        let request = self.request(instance, bot.id, Kind::Abandon, self.control(bot.id, true)?);
        self.confirmation = Some(request.clone());
        Some(request)
    }
    pub fn confirm(
        &mut self,
        instance: u64,
        bot: &Bot,
        request: &Request,
        confirmed: bool,
    ) -> Option<Request> {
        if request.instance != instance
            || request.bot != bot.id
            || self.confirmation.as_ref() != Some(request)
        {
            return None;
        }
        self.confirmation = None;
        if !confirmed {
            return None;
        }
        if !self.can_abandon(bot) || self.control(bot.id, true).as_ref() != Some(&request.command) {
            self.notice = Some(Notice::Reconcile);
            self.owed = true;
            return None;
        }
        self.pending = Some(request.clone());
        Some(request.clone())
    }
    pub fn facts(&mut self, instance: u64, bot: &Bot, next: bool) -> Option<Request> {
        if !self.current(bot) || !self.bound() || bot.lifecycle == BotLifecycle::Trashed {
            return None;
        }
        let cursor = if next {
            if self.list_stale {
                return None;
            }
            match self.list.as_ref()? {
                BotMemoryListPage::Observed {
                    continuation: Some(cursor),
                    ..
                } => Some(cursor.clone()),
                _ => return None,
            }
        } else {
            None
        };
        self.start(
            instance,
            bot.id,
            Kind::List,
            BotCommand::ListMemory {
                bot_id: bot.id,
                cursor,
                limit: 20,
            },
        )
    }
    pub fn fact(
        &mut self,
        instance: u64,
        bot: &Bot,
        fact_id: BotMemoryNodeId,
        next: bool,
    ) -> Option<Request> {
        if !self.current(bot) || !self.bound() || bot.lifecycle == BotLifecycle::Trashed {
            return None;
        }
        let cursor = if next {
            if self.detail_stale {
                return None;
            }
            match self.detail.as_ref()? {
                BotMemoryDetailPage::Observed {
                    fact_id: id,
                    continuation: Some(cursor),
                    ..
                } if *id == fact_id => Some(cursor.clone()),
                _ => return None,
            }
        } else {
            if !matches!(self.list.as_ref(), Some(BotMemoryListPage::Observed { items, .. }) if items.iter().any(|item| item.fact_id == fact_id))
            {
                return None;
            }
            None
        };
        self.start(
            instance,
            bot.id,
            Kind::Detail,
            BotCommand::ReadMemoryFact {
                bot_id: bot.id,
                fact_id,
                cursor,
            },
        )
    }
    pub fn back(&mut self) -> bool {
        let changed = self.detail.take().is_some();
        // A late detail reply may not reopen a detail the person closed.
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.kind == Kind::Detail)
        {
            self.suppress_detail = true;
        }
        changed
    }
    pub fn unsent(&mut self, request: &Request) {
        if self.pending.as_ref() == Some(request) {
            self.pending = None;
            self.notice = Some(Notice::Unavailable);
        }
    }
    pub fn answer(
        &mut self,
        instance: u64,
        bot: &Bot,
        request: Request,
        result: Result<BotResponse, Failure>,
    ) -> bool {
        if request.instance == instance && self.retired.as_ref() == Some(&request) {
            self.retired = None;
            return true;
        }
        if request.instance != instance
            || request.bot != bot.id
            || self.pending.as_ref() != Some(&request)
        {
            return false;
        }
        self.pending = None;
        if std::mem::take(&mut self.suppress_detail) {
            return true;
        }
        if matches!(request.kind, Kind::List | Kind::Detail)
            && (request.revision != bot.revision || !self.current(bot))
        {
            self.notice = Some(Notice::Reconcile);
            return true;
        }
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                if matches!(
                    request.kind,
                    Kind::Status | Kind::Ensure | Kind::Retry | Kind::Abandon
                ) {
                    self.status_valid = false;
                }
                self.notice = Some(match (&request.kind, error) {
                    (Kind::Retry | Kind::Abandon, _) => {
                        self.owed = true;
                        Notice::Reconcile
                    }
                    (Kind::Ensure, Failure::Unanswered) => Notice::Unknown,
                    (_, Failure::Refused(code)) if code == "memory_fact_not_found" => {
                        Notice::NotFound
                    }
                    (_, Failure::Refused(code))
                        if matches!(
                            code.as_str(),
                            "invalid_memory_cursor"
                                | "memory_cursor_expired"
                                | "memory_history_unavailable"
                        ) =>
                    {
                        Notice::Expired
                    }
                    (_, Failure::Refused(code))
                        if code == "context_too_large"
                            || code == "memory_detail_unsupported"
                            || code == "memory_detail_too_large" =>
                    {
                        Notice::Unsupported
                    }
                    (_, Failure::Refused(_)) => {
                        if request.kind == Kind::Ensure {
                            self.owed = true;
                        }
                        Notice::Refused
                    }
                    _ => Notice::Unavailable,
                });
                if request.kind == Kind::List {
                    self.list_stale = true;
                }
                if request.kind == Kind::Detail {
                    self.detail_stale = true;
                }
                return true;
            }
        };
        if !serde_json::to_vec(&response)
            .is_ok_and(|bytes| bytes.len() <= koru_ai_core::bots::memory::BOT_MEMORY_RESPONSE_BYTES)
        {
            self.notice = Some(Notice::Unsupported);
            return true;
        }
        let valid = match response {
            BotResponse::MemoryStatus { status }
                if matches!(
                    request.kind,
                    Kind::Status | Kind::Ensure | Kind::Retry | Kind::Abandon
                ) =>
            {
                let stale_token = match (self.status.as_ref().map(|s| &s.state), &status.state) {
                    (
                        Some(BotMemoryStatusState::Candidate {
                            creation_request: old,
                            observed_updated_at_ms: time,
                            ..
                        }),
                        BotMemoryStatusState::Candidate {
                            creation_request,
                            observed_updated_at_ms,
                            ..
                        },
                    ) => old != creation_request || time > observed_updated_at_ms,
                    (
                        Some(BotMemoryStatusState::Candidate { .. }),
                        BotMemoryStatusState::NotStarted { .. },
                    ) => true,
                    _ => false,
                };
                if status.bot_id != bot.id
                    || status.revision < self.known_revision
                    || stale_token
                    || matches!(&status.state, BotMemoryStatusState::Candidate { candidate, .. } if candidate.workspace != bot.home_workspace)
                {
                    if !self.reconcile_used {
                        self.owed = true;
                        self.reconcile_used = true;
                    }
                    false
                } else {
                    if matches!(status.state, BotMemoryStatusState::Candidate { .. }) {
                        self.ensure = None;
                    }
                    self.known_revision = status.revision;
                    self.status_valid = true;
                    self.status = Some(status);
                    true
                }
            }
            BotResponse::MemoryList { page } if request.kind == Kind::List => {
                let continuation = matches!(
                    &request.command,
                    BotCommand::ListMemory {
                        cursor: Some(_),
                        ..
                    }
                );
                let valid = match &page {
                    BotMemoryListPage::Absent {} => !self.bound(),
                    BotMemoryListPage::Observed {
                        page,
                        version,
                        items,
                        complete,
                        continuation: cursor,
                        ..
                    } => {
                        self.page() == Some(page)
                            && items.len() <= 20
                            && *complete == cursor.is_none()
                            && (!continuation
                                || matches!(&self.list, Some(BotMemoryListPage::Observed { version: old, .. }) if old == version))
                    }
                };
                if valid {
                    self.list = Some(page);
                    self.list_stale = false;
                    self.detail = None;
                }
                valid
            }
            BotResponse::MemoryDetail { page } if request.kind == Kind::Detail => {
                let BotCommand::ReadMemoryFact {
                    fact_id: expected,
                    cursor,
                    ..
                } = &request.command
                else {
                    unreachable!()
                };
                let valid = match &page {
                    BotMemoryDetailPage::Absent {} => !self.bound(),
                    BotMemoryDetailPage::Observed {
                        page,
                        version,
                        fact_id,
                        complete,
                        continuation,
                        ..
                    } => {
                        self.page() == Some(page)
                            && expected == fact_id
                            && *complete == continuation.is_none()
                            && (cursor.is_none()
                                || matches!(&self.detail, Some(BotMemoryDetailPage::Observed { version: old, .. }) if old == version))
                    }
                };
                if valid {
                    self.detail = Some(page);
                    self.detail_stale = false;
                }
                valid
            }
            _ => false,
        };
        self.notice = (!valid).then_some(Notice::Reconcile);
        if valid {
            self.body_serial = self.body_serial.wrapping_add(1);
        }
        true
    }
}

#[cfg(test)]
mod tests;
