// SPDX-License-Identifier: MPL-2.0

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use koru_session_protocol::{
    AgentEvent, AgentTurnId, AgentUsage, ApprovalDecision, ApprovalId, AttachmentId, ErrorCode,
    ManagedState, ProviderKind, SequencedAgentEvent, SessionId, StreamSequence, ToolCallId,
};

pub const MANAGED_ITEM_LIMIT: usize = 2_048;
pub const MANAGED_TEXT_BYTE_LIMIT: usize = 4 * 1024 * 1024;
pub const MANAGED_MESSAGE_BLOCK_BYTE_LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedItemKind {
    Message,
    Tool,
    Approval,
    Attachment,
    Usage,
    TurnState,
    ProviderState,
    StreamGap,
    Truncated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedToolState {
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedApprovalState {
    Pending,
    Submitting(ApprovalDecision),
    Resolved(ApprovalDecision),
    Expired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedAttachmentState {
    Accepted,
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedTurnState {
    Running,
    WaitingForInput,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, PartialEq, Eq)]
pub enum ManagedItemContent {
    Message {
        turn_id: AgentTurnId,
        text: String,
    },
    Tool {
        turn_id: AgentTurnId,
        tool_call_id: ToolCallId,
        label: String,
        state: ManagedToolState,
    },
    Approval {
        turn_id: AgentTurnId,
        approval_id: ApprovalId,
        summary: String,
        choices: Vec<koru_session_protocol::ApprovalChoice>,
        choice: Option<koru_session_protocol::ApprovalChoiceId>,
        state: ManagedApprovalState,
        sequence: StreamSequence,
        safely_reviewable: bool,
    },
    Attachment {
        turn_id: AgentTurnId,
        attachment_id: AttachmentId,
        state: ManagedAttachmentState,
        error: Option<ErrorCode>,
    },
    Usage {
        turn_id: AgentTurnId,
        usage: AgentUsage,
    },
    TurnState {
        turn_id: AgentTurnId,
        state: ManagedTurnState,
        error: Option<ErrorCode>,
        approval_unrepresentable: bool,
    },
    ProviderState {
        provider: ProviderKind,
        state: ManagedState,
    },
    StreamGap {
        expected: StreamSequence,
        actual: StreamSequence,
    },
    Truncated,
}

impl ManagedItemContent {
    pub fn kind(&self) -> ManagedItemKind {
        match self {
            Self::Message { .. } => ManagedItemKind::Message,
            Self::Tool { .. } => ManagedItemKind::Tool,
            Self::Approval { .. } => ManagedItemKind::Approval,
            Self::Attachment { .. } => ManagedItemKind::Attachment,
            Self::Usage { .. } => ManagedItemKind::Usage,
            Self::TurnState { .. } => ManagedItemKind::TurnState,
            Self::ProviderState { .. } => ManagedItemKind::ProviderState,
            Self::StreamGap { .. } => ManagedItemKind::StreamGap,
            Self::Truncated => ManagedItemKind::Truncated,
        }
    }

    fn text_bytes(&self) -> usize {
        match self {
            Self::Message { text, .. } => text.len(),
            Self::Tool { label, .. } => label.len(),
            Self::Approval {
                summary, choices, ..
            } => {
                summary.len()
                    + choices
                        .iter()
                        .map(|choice| {
                            choice.label.len()
                                + std::mem::size_of::<koru_session_protocol::ApprovalChoice>()
                        })
                        .sum::<usize>()
            }
            _ => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedTranscriptItem {
    pub id: u64,
    pub content: ManagedItemContent,
    pub(crate) revision: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ManagedApplyOutcome {
    pub events: u64,
    pub inserted_items: u64,
    pub updated_items: u64,
    pub coalesced_deltas: u64,
    pub evicted_items: u64,
    pub attention_transitions: u64,
    pub redraw: bool,
}

pub(crate) struct ManagedModel {
    session_id: Option<SessionId>,
    items: VecDeque<ManagedTranscriptItem>,
    next_id: u64,
    text_bytes: usize,
    active_message: Option<(AgentTurnId, u64)>,
    tool_items: BTreeMap<ToolCallId, u64>,
    approval_items: BTreeMap<ApprovalId, u64>,
    usage_items: BTreeMap<AgentTurnId, u64>,
    turn_items: BTreeMap<AgentTurnId, u64>,
    provider_item: Option<u64>,
    active_turn: Option<AgentTurnId>,
    cancelling_turns: BTreeSet<AgentTurnId>,
    last_sequence: Option<StreamSequence>,
    unpresented_through: Option<StreamSequence>,
    background_unread: bool,
    newest_pending_approval: Option<ApprovalId>,
    truncated: bool,
}

impl Default for ManagedModel {
    fn default() -> Self {
        Self {
            session_id: None,
            items: VecDeque::new(),
            next_id: 1,
            text_bytes: 0,
            active_message: None,
            tool_items: BTreeMap::new(),
            approval_items: BTreeMap::new(),
            usage_items: BTreeMap::new(),
            turn_items: BTreeMap::new(),
            provider_item: None,
            active_turn: None,
            cancelling_turns: BTreeSet::new(),
            last_sequence: None,
            unpresented_through: None,
            background_unread: false,
            newest_pending_approval: None,
            truncated: false,
        }
    }
}

impl ManagedModel {
    pub(crate) fn items(&self) -> &VecDeque<ManagedTranscriptItem> {
        &self.items
    }

    pub(crate) fn active_turn(&self) -> Option<AgentTurnId> {
        self.active_turn
    }

    pub(crate) fn begin_cancel(&mut self, turn_id: AgentTurnId) -> bool {
        self.cancelling_turns.insert(turn_id)
    }

    pub(crate) fn newest_pending_approval(&self) -> Option<ApprovalId> {
        self.newest_pending_approval
    }

    pub(crate) fn unpresented_through(&self) -> Option<StreamSequence> {
        self.unpresented_through
    }

    pub(crate) fn mark_presented(&mut self, through: StreamSequence) {
        if self
            .unpresented_through
            .is_some_and(|pending| through >= pending)
        {
            self.unpresented_through = None;
            self.background_unread = false;
        }
    }

    pub(crate) fn approval(&self, id: ApprovalId) -> Option<&ManagedTranscriptItem> {
        let item_id = self.approval_items.get(&id)?;
        self.item(*item_id)
    }

    pub(crate) fn approval_actionable(&self, id: ApprovalId) -> bool {
        self.approval(id).is_some_and(|item| {
            matches!(
                item.content,
                ManagedItemContent::Approval {
                    state: ManagedApprovalState::Pending | ManagedApprovalState::Submitting(_),
                    ..
                }
            )
        })
    }

    pub(crate) fn approval_mut(&mut self, id: ApprovalId) -> Option<&mut ManagedTranscriptItem> {
        let item_id = *self.approval_items.get(&id)?;
        self.item_mut(item_id)
    }

    pub(crate) fn apply_batch(
        &mut self,
        events: impl IntoIterator<Item = SequencedAgentEvent>,
        visible: bool,
    ) -> Result<ManagedApplyOutcome, ErrorCode> {
        let events: Vec<_> = events.into_iter().collect();
        let mut session_id = self.session_id;
        let mut last_sequence = self.last_sequence;
        for sequenced in &events {
            match session_id {
                None => session_id = Some(sequenced.session_id),
                Some(current) if current != sequenced.session_id => {
                    return Err(ErrorCode::StaleGeneration);
                }
                Some(_) => {}
            }
            if let Some(last) = last_sequence
                && last.next() != Some(sequenced.sequence)
            {
                return Err(ErrorCode::StreamGap);
            }
            last_sequence = Some(sequenced.sequence);
        }

        let mut outcome = ManagedApplyOutcome::default();
        self.session_id = session_id;
        for sequenced in events {
            outcome.events = outcome.events.saturating_add(1);
            self.last_sequence = Some(sequenced.sequence);
            let attention_worthy =
                !visible || matches!(&sequenced.event, AgentEvent::ApprovalRequired { .. });
            self.apply_event(sequenced.event, sequenced.sequence, &mut outcome);
            if attention_worthy {
                self.unpresented_through = Some(sequenced.sequence);
                if !self.background_unread {
                    self.background_unread = true;
                    outcome.attention_transitions = outcome.attention_transitions.saturating_add(1);
                }
            }
        }
        outcome.evicted_items = self.enforce_bounds() as u64;
        outcome.redraw = visible && outcome.events > 0;
        Ok(outcome)
    }

    pub(crate) fn apply_gap(
        &mut self,
        expected: StreamSequence,
        actual: StreamSequence,
        visible: bool,
    ) -> ManagedApplyOutcome {
        self.active_message = None;
        self.last_sequence = Some(actual);
        self.unpresented_through = Some(actual);
        let attention_transitions = u64::from(!self.background_unread);
        self.background_unread = true;
        self.push(
            ManagedItemContent::StreamGap { expected, actual },
            &mut ManagedApplyOutcome::default(),
        );
        let evicted = self.enforce_bounds();
        ManagedApplyOutcome {
            events: 1,
            inserted_items: 1,
            evicted_items: evicted as u64,
            attention_transitions,
            redraw: visible,
            ..ManagedApplyOutcome::default()
        }
    }

    pub(crate) fn mark_approval_submitting(
        &mut self,
        id: ApprovalId,
        decision: ApprovalDecision,
        choice: Option<koru_session_protocol::ApprovalChoiceId>,
    ) -> bool {
        let Some(item) = self.approval_mut(id) else {
            return false;
        };
        let ManagedItemContent::Approval {
            state,
            choices,
            choice: selected,
            safely_reviewable,
            ..
        } = &mut item.content
        else {
            return false;
        };
        let decision = if choices.is_empty() && choice.is_none() {
            decision
        } else {
            let Ok(option) =
                koru_session_protocol::resolve_approval_choice(choices, decision, choice)
            else {
                return false;
            };
            option.intent.decision()
        };
        if *state != ManagedApprovalState::Pending
            || (decision == ApprovalDecision::Approve && !*safely_reviewable)
        {
            return false;
        }
        *selected = choice;
        *state = ManagedApprovalState::Submitting(decision);
        item.revision = item.revision.saturating_add(1);
        true
    }

    pub(crate) fn approval_failed(&mut self, id: ApprovalId, stale: bool) -> bool {
        let Some(item) = self.approval_mut(id) else {
            return false;
        };
        let ManagedItemContent::Approval { state, .. } = &mut item.content else {
            return false;
        };
        if !matches!(state, ManagedApprovalState::Submitting(_)) {
            return false;
        }
        *state = if stale {
            ManagedApprovalState::Expired
        } else {
            ManagedApprovalState::Pending
        };
        item.revision = item.revision.saturating_add(1);
        self.refresh_newest_approval();
        true
    }

    fn apply_event(
        &mut self,
        event: AgentEvent,
        sequence: StreamSequence,
        outcome: &mut ManagedApplyOutcome,
    ) {
        if let AgentEvent::MessageDelta { turn_id, text } = event {
            self.append_message(turn_id, &text, outcome);
            return;
        }
        self.active_message = None;
        match event {
            AgentEvent::TurnStarted { turn_id } => {
                self.active_turn = Some(turn_id);
                self.cancelling_turns.remove(&turn_id);
                self.upsert_turn(turn_id, ManagedTurnState::Running, None, outcome);
            }
            AgentEvent::ToolStarted {
                turn_id,
                tool_call_id,
                label,
                ..
            } => {
                let id = self.push(
                    ManagedItemContent::Tool {
                        turn_id,
                        tool_call_id,
                        label,
                        state: ManagedToolState::Running,
                    },
                    outcome,
                );
                self.tool_items.insert(tool_call_id, id);
            }
            AgentEvent::ToolCompleted {
                tool_call_id,
                succeeded,
                ..
            } => {
                if let Some(id) = self.tool_items.get(&tool_call_id).copied()
                    && let Some(item) = self.item_mut(id)
                    && let ManagedItemContent::Tool { state, .. } = &mut item.content
                {
                    *state = if succeeded {
                        ManagedToolState::Completed
                    } else {
                        ManagedToolState::Failed
                    };
                    item.revision = item.revision.saturating_add(1);
                    outcome.updated_items = outcome.updated_items.saturating_add(1);
                }
            }
            AgentEvent::ApprovalRequired {
                turn_id,
                approval_id,
                summary,
                mut choices,
            } => {
                let safely_reviewable = summary.len() <= MANAGED_MESSAGE_BLOCK_BYTE_LIMIT
                    && choices.len() <= MANAGED_ITEM_LIMIT
                    && choices
                        .iter()
                        .try_fold(summary.len(), |total, option| {
                            total.checked_add(
                                option.label.len()
                                    + std::mem::size_of::<koru_session_protocol::ApprovalChoice>(),
                            )
                        })
                        .is_some_and(|total| total <= MANAGED_MESSAGE_BLOCK_BYTE_LIMIT);
                choices.truncate(MANAGED_ITEM_LIMIT);
                let mut label_budget =
                    MANAGED_MESSAGE_BLOCK_BYTE_LIMIT.saturating_sub(summary.len());
                for option in &mut choices {
                    option.label = truncate_utf8(&option.label, label_budget).to_owned();
                    label_budget = label_budget.saturating_sub(option.label.len());
                }
                let summary = if safely_reviewable {
                    summary
                } else {
                    truncate_utf8(&summary, MANAGED_MESSAGE_BLOCK_BYTE_LIMIT).to_owned()
                };
                let id = self.push(
                    ManagedItemContent::Approval {
                        turn_id,
                        approval_id,
                        summary,
                        choices,
                        choice: None,
                        state: ManagedApprovalState::Pending,
                        sequence,
                        safely_reviewable,
                    },
                    outcome,
                );
                self.approval_items.insert(approval_id, id);
                self.newest_pending_approval = Some(approval_id);
            }
            AgentEvent::ApprovalResolved {
                approval_id,
                decision,
                choice,
                ..
            } => {
                if let Some(item) = self.approval_mut(approval_id)
                    && let ManagedItemContent::Approval {
                        choice: selected, ..
                    } = &mut item.content
                {
                    *selected = choice;
                }
                self.set_approval_state(
                    approval_id,
                    ManagedApprovalState::Resolved(decision),
                    outcome,
                );
            }
            AgentEvent::ApprovalExpired { approval_id, .. } => {
                self.set_approval_state(approval_id, ManagedApprovalState::Expired, outcome);
            }
            AgentEvent::WaitingForInput { turn_id } => {
                self.upsert_turn(turn_id, ManagedTurnState::WaitingForInput, None, outcome);
            }
            AgentEvent::AttachmentAccepted {
                turn_id,
                attachment_id,
            } => {
                self.push(
                    ManagedItemContent::Attachment {
                        turn_id,
                        attachment_id,
                        state: ManagedAttachmentState::Accepted,
                        error: None,
                    },
                    outcome,
                );
            }
            AgentEvent::AttachmentRejected {
                turn_id,
                attachment_id,
                code,
            } => {
                self.push(
                    ManagedItemContent::Attachment {
                        turn_id,
                        attachment_id,
                        state: ManagedAttachmentState::Rejected,
                        error: Some(code),
                    },
                    outcome,
                );
            }
            AgentEvent::Usage { turn_id, usage } => {
                if let Some(id) = self.usage_items.get(&turn_id).copied()
                    && let Some(item) = self.item_mut(id)
                    && let ManagedItemContent::Usage { usage: current, .. } = &mut item.content
                {
                    *current = usage;
                    item.revision = item.revision.saturating_add(1);
                    outcome.updated_items = outcome.updated_items.saturating_add(1);
                } else {
                    let id = self.push(ManagedItemContent::Usage { turn_id, usage }, outcome);
                    self.usage_items.insert(turn_id, id);
                }
            }
            AgentEvent::ChildSessionCreated { .. } => {}
            AgentEvent::TurnCompleted { turn_id } => {
                self.finish_turn(turn_id, ManagedTurnState::Completed, None, outcome);
            }
            AgentEvent::TurnCancelled { turn_id, .. } => {
                self.finish_turn(turn_id, ManagedTurnState::Cancelled, None, outcome);
            }
            AgentEvent::TurnFailed { turn_id, error } => {
                let approval_unrepresentable = error.code == ErrorCode::ProviderFailed
                    && error.message == koru_session_protocol::APPROVAL_UNREPRESENTABLE_MESSAGE;
                self.finish_turn(turn_id, ManagedTurnState::Failed, Some(error.code), outcome);
                if let Some(id) = self.turn_items.get(&turn_id).copied()
                    && let Some(item) = self.item_mut(id)
                    && let ManagedItemContent::TurnState {
                        approval_unrepresentable: detail,
                        ..
                    } = &mut item.content
                {
                    *detail = approval_unrepresentable;
                }
            }
            AgentEvent::ProviderState { provider, state } => {
                if let Some(id) = self.provider_item
                    && let Some(item) = self.item_mut(id)
                {
                    item.content = ManagedItemContent::ProviderState { provider, state };
                    item.revision = item.revision.saturating_add(1);
                    outcome.updated_items = outcome.updated_items.saturating_add(1);
                } else {
                    let id = self.push(
                        ManagedItemContent::ProviderState { provider, state },
                        outcome,
                    );
                    self.provider_item = Some(id);
                }
            }
            // A chat's approval mode (koru-ai D7): the AI chat shows it, a
            // terminal session's transcript has nothing to draw for it.
            AgentEvent::ModeChanged { .. } => {}
            // An agent's status line (koru-ai D69): the AI chat shows it, and
            // it is not part of the reply this transcript draws.
            AgentEvent::Notice { .. } => {}
            // An agent's questions (koru-ai D108): the AI chat draws them; a
            // terminal's session is not created to, and none arrive.
            AgentEvent::QuestionsRequired { .. } | AgentEvent::QuestionsResolved { .. } => {}
            AgentEvent::CommandsUpdated { .. } => {}
            AgentEvent::ContextUsage { .. } => {}
            AgentEvent::MessageDelta { .. } => unreachable!("handled above"),
        }
    }

    fn append_message(
        &mut self,
        turn_id: AgentTurnId,
        mut text: &str,
        outcome: &mut ManagedApplyOutcome,
    ) {
        while !text.is_empty() {
            let mut appended = false;
            if let Some((active_turn, id)) = self.active_message
                && active_turn == turn_id
                && let Some(item) = self.item_mut(id)
                && let ManagedItemContent::Message { text: current, .. } = &mut item.content
            {
                let room = MANAGED_MESSAGE_BLOCK_BYTE_LIMIT.saturating_sub(current.len());
                if room > 0 {
                    let take = utf8_prefix_len(text, room);
                    if take > 0 {
                        current.push_str(&text[..take]);
                        item.revision = item.revision.saturating_add(1);
                        self.text_bytes = self.text_bytes.saturating_add(take);
                        text = &text[take..];
                        outcome.coalesced_deltas = outcome.coalesced_deltas.saturating_add(1);
                        outcome.updated_items = outcome.updated_items.saturating_add(1);
                        appended = true;
                    }
                }
            }
            if appended {
                continue;
            }
            let take = utf8_prefix_len(text, MANAGED_MESSAGE_BLOCK_BYTE_LIMIT);
            let take = take.max(text.chars().next().map_or(0, char::len_utf8));
            let block = text[..take].to_owned();
            text = &text[take..];
            let id = self.push(
                ManagedItemContent::Message {
                    turn_id,
                    text: block,
                },
                outcome,
            );
            self.active_message = Some((turn_id, id));
        }
    }

    fn upsert_turn(
        &mut self,
        turn_id: AgentTurnId,
        state: ManagedTurnState,
        error: Option<ErrorCode>,
        outcome: &mut ManagedApplyOutcome,
    ) {
        if let Some(id) = self.turn_items.get(&turn_id).copied()
            && let Some(item) = self.item_mut(id)
        {
            item.content = ManagedItemContent::TurnState {
                turn_id,
                state,
                error,
                approval_unrepresentable: false,
            };
            item.revision = item.revision.saturating_add(1);
            outcome.updated_items = outcome.updated_items.saturating_add(1);
        } else {
            let id = self.push(
                ManagedItemContent::TurnState {
                    turn_id,
                    state,
                    error,
                    approval_unrepresentable: false,
                },
                outcome,
            );
            self.turn_items.insert(turn_id, id);
        }
    }

    fn finish_turn(
        &mut self,
        turn_id: AgentTurnId,
        state: ManagedTurnState,
        error: Option<ErrorCode>,
        outcome: &mut ManagedApplyOutcome,
    ) {
        self.upsert_turn(turn_id, state, error, outcome);
        if self.active_turn == Some(turn_id) {
            self.active_turn = None;
        }
        self.cancelling_turns.remove(&turn_id);
    }

    fn set_approval_state(
        &mut self,
        approval_id: ApprovalId,
        next: ManagedApprovalState,
        outcome: &mut ManagedApplyOutcome,
    ) {
        if let Some(item) = self.approval_mut(approval_id)
            && let ManagedItemContent::Approval { state, .. } = &mut item.content
        {
            *state = next;
            item.revision = item.revision.saturating_add(1);
            outcome.updated_items = outcome.updated_items.saturating_add(1);
        }
        self.refresh_newest_approval();
    }

    fn refresh_newest_approval(&mut self) {
        self.newest_pending_approval = self.items.iter().rev().find_map(|item| {
            let ManagedItemContent::Approval {
                approval_id,
                state: ManagedApprovalState::Pending,
                ..
            } = item.content
            else {
                return None;
            };
            Some(approval_id)
        });
    }

    fn push(&mut self, content: ManagedItemContent, outcome: &mut ManagedApplyOutcome) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1).max(1);
        self.text_bytes = self.text_bytes.saturating_add(content.text_bytes());
        self.items.push_back(ManagedTranscriptItem {
            id,
            content,
            revision: 1,
        });
        outcome.inserted_items = outcome.inserted_items.saturating_add(1);
        id
    }

    fn item(&self, id: u64) -> Option<&ManagedTranscriptItem> {
        self.items.iter().find(|item| item.id == id)
    }

    fn item_mut(&mut self, id: u64) -> Option<&mut ManagedTranscriptItem> {
        self.items.iter_mut().find(|item| item.id == id)
    }

    fn enforce_bounds(&mut self) -> usize {
        let sentinel = self
            .items
            .front()
            .is_some_and(|item| matches!(item.content, ManagedItemContent::Truncated))
            .then(|| self.items.pop_front().expect("front checked above"));
        let mut evicted = 0usize;
        let reserved = usize::from(sentinel.is_some() || self.truncated);
        while self.items.len().saturating_add(reserved) > MANAGED_ITEM_LIMIT
            || self.text_bytes > MANAGED_TEXT_BYTE_LIMIT
        {
            let Some(item) = self.items.pop_front() else {
                break;
            };
            self.text_bytes = self.text_bytes.saturating_sub(item.content.text_bytes());
            self.forget(&item);
            evicted += 1;
        }
        if evicted > 0 {
            self.truncated = true;
        }
        if self.truncated {
            if sentinel.is_none()
                && self.items.len() >= MANAGED_ITEM_LIMIT
                && let Some(item) = self.items.pop_front()
            {
                self.text_bytes = self.text_bytes.saturating_sub(item.content.text_bytes());
                self.forget(&item);
                evicted += 1;
            }
            let sentinel = sentinel.unwrap_or_else(|| {
                let id = self.next_id;
                self.next_id = self.next_id.saturating_add(1).max(1);
                ManagedTranscriptItem {
                    id,
                    content: ManagedItemContent::Truncated,
                    revision: 1,
                }
            });
            self.items.push_front(sentinel);
        }
        self.refresh_newest_approval();
        evicted
    }

    fn forget(&mut self, item: &ManagedTranscriptItem) {
        match &item.content {
            ManagedItemContent::Message { .. } => {
                if self.active_message.is_some_and(|(_, id)| id == item.id) {
                    self.active_message = None;
                }
            }
            ManagedItemContent::Tool { tool_call_id, .. } => {
                self.tool_items.remove(tool_call_id);
            }
            ManagedItemContent::Approval { approval_id, .. } => {
                self.approval_items.remove(approval_id);
            }
            ManagedItemContent::Usage { turn_id, .. } => {
                self.usage_items.remove(turn_id);
            }
            ManagedItemContent::TurnState { turn_id, .. } => {
                self.turn_items.remove(turn_id);
            }
            ManagedItemContent::ProviderState { .. } => {
                if self.provider_item == Some(item.id) {
                    self.provider_item = None;
                }
            }
            _ => {}
        }
    }
}

fn utf8_prefix_len(value: &str, maximum: usize) -> usize {
    if value.len() <= maximum {
        return value.len();
    }
    let mut end = maximum;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn truncate_utf8(value: &str, maximum: usize) -> &str {
    &value[..utf8_prefix_len(value, maximum)]
}

impl std::fmt::Debug for ManagedItemContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedItemContent")
            .field("kind", &self.kind())
            .finish_non_exhaustive()
    }
}
