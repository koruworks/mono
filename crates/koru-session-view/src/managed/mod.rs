// SPDX-License-Identifier: MPL-2.0

mod interaction;
mod layout;
mod model;
mod render;

use koru_draw::text::TextEngine;
use koru_session_protocol::{
    AgentUsage, ApprovalDecision, ApprovalId, ErrorCode, ManagedState, SequencedAgentEvent,
    StreamSequence,
};
use vello::{
    Scene,
    kurbo::{Affine, Rect},
};

use crate::{SessionSurfaceIntent, SessionSurfaceInteraction};

pub use interaction::{ApprovalFocus, ManagedHit, ManagedInputOutcome, ManagedIntent, ManagedKey};
pub use layout::{ManagedApprovalLayout, ManagedItemLayout, ManagedLayout};
pub use model::{
    MANAGED_ITEM_LIMIT, MANAGED_MESSAGE_BLOCK_BYTE_LIMIT, MANAGED_TEXT_BYTE_LIMIT,
    ManagedApplyOutcome, ManagedApprovalState, ManagedAttachmentState, ManagedItemContent,
    ManagedItemKind, ManagedToolState, ManagedTranscriptItem, ManagedTurnState,
};
pub use render::ManagedRenderWork;

/// Localized copy supplied by the embedding surface. Provider response text is
/// content; tool and approval strings are closed category tokens and are
/// always mapped through this copy before display.
#[derive(Clone)]
pub struct ManagedSessionCopy {
    pub tool_running: String,
    pub tool_completed: String,
    pub tool_failed: String,
    pub tool_command: String,
    pub tool_file_change: String,
    pub tool_permissions: String,
    pub tool_mcp: String,
    pub tool_dynamic: String,
    pub tool_web_search: String,
    pub tool_image_generation: String,
    pub tool_action: String,
    pub approval_required: String,
    pub approval_too_large: String,
    pub approve: String,
    pub reject: String,
    pub allow_remembered: String,
    pub reject_remembered: String,
    pub approval_submitting: String,
    pub approval_unrepresentable: String,
    pub attachment_accepted: String,
    pub attachment_rejected: String,
    pub usage: String,
    pub input_tokens: String,
    pub output_tokens: String,
    pub cached_tokens: String,
    pub turn_running: String,
    pub waiting_for_input: String,
    pub turn_completed: String,
    pub turn_cancelled: String,
    pub turn_failed: String,
    pub provider_idle: String,
    pub provider_running: String,
    pub provider_waiting_for_input: String,
    pub provider_waiting_for_approval: String,
    pub provider_completed: String,
    pub provider_cancelled: String,
    pub provider_failed: String,
    pub stream_gap: String,
    pub transcript_truncated: String,
    pub provider_unavailable: String,
    pub provider_incompatible: String,
    pub model_unavailable: String,
    pub generic_error: String,
    pub cancel_turn: String,
    /// Locale-aware integer formatting supplied by the host.
    pub number: fn(u64) -> String,
    /// Locale-aware label/value ordering and punctuation supplied by the host.
    pub usage_value: fn(&str, &str) -> String,
    /// Locale-aware list punctuation supplied by the host.
    pub usage_list: fn(&[String]) -> String,
}

impl Default for ManagedSessionCopy {
    fn default() -> Self {
        Self {
            tool_running: "Running".into(),
            tool_completed: "Completed".into(),
            tool_failed: "Failed".into(),
            tool_command: "Command".into(),
            tool_file_change: "File change".into(),
            tool_permissions: "Permissions".into(),
            tool_mcp: "MCP tool".into(),
            tool_dynamic: "Tool".into(),
            tool_web_search: "Web search".into(),
            tool_image_generation: "Image generation".into(),
            tool_action: "Action".into(),
            approval_required: "Approval required".into(),
            approval_too_large:
                "These details are too large to review safely. Reject this request.".into(),
            approve: "Approve".into(),
            reject: "Reject".into(),
            allow_remembered: "Allow and remember".into(),
            reject_remembered: "Reject and remember".into(),
            approval_submitting: "Sending answer…".into(),
            approval_unrepresentable:
                "This approval cannot be answered. Update Koru or the agent, then try again.".into(),
            attachment_accepted: "Image attached".into(),
            attachment_rejected: "Image rejected".into(),
            usage: "Usage".into(),
            input_tokens: "Input".into(),
            output_tokens: "Output".into(),
            cached_tokens: "Cached input".into(),
            turn_running: "Working".into(),
            waiting_for_input: "Waiting for input".into(),
            turn_completed: "Completed".into(),
            turn_cancelled: "Cancelled".into(),
            turn_failed: "Failed".into(),
            provider_idle: "Ready".into(),
            provider_running: "Working".into(),
            provider_waiting_for_input: "Waiting for input".into(),
            provider_waiting_for_approval: "Waiting for approval".into(),
            provider_completed: "Completed".into(),
            provider_cancelled: "Cancelled".into(),
            provider_failed: "Provider failed".into(),
            stream_gap: "Some managed-session activity is unavailable".into(),
            transcript_truncated: "Earlier transcript items are no longer retained".into(),
            provider_unavailable: "This provider is unavailable".into(),
            provider_incompatible: "This provider version is not compatible".into(),
            model_unavailable:
                "The selected model is unavailable. Choose another model or the agent default."
                    .into(),
            generic_error: "The managed session failed".into(),
            cancel_turn: "Cancel turn".into(),
            number: |value| value.to_string(),
            usage_value: |label, value| format!("{label}: {value}"),
            usage_list: |values| values.join(" \u{00b7} "),
        }
    }
}

impl ManagedSessionCopy {
    pub fn provider_state(&self, state: ManagedState) -> String {
        match state {
            ManagedState::Idle => &self.provider_idle,
            ManagedState::Running => &self.provider_running,
            ManagedState::WaitingForInput => &self.provider_waiting_for_input,
            ManagedState::WaitingForApproval => &self.provider_waiting_for_approval,
            ManagedState::Completed => &self.provider_completed,
            ManagedState::Cancelled => &self.provider_cancelled,
            ManagedState::Failed => &self.provider_failed,
        }
        .clone()
    }

    pub fn error(&self, code: &ErrorCode) -> String {
        match code {
            ErrorCode::ProviderModelUnavailable => &self.model_unavailable,
            ErrorCode::ProviderUnavailable | ErrorCode::ProfileMissing => {
                &self.provider_unavailable
            }
            ErrorCode::ProviderIncompatible
            | ErrorCode::ProfileIncompatible
            | ErrorCode::CapabilityNotNegotiated => &self.provider_incompatible,
            _ => &self.generic_error,
        }
        .clone()
    }

    pub fn choice_label(&self, option: &koru_session_protocol::ApprovalChoice) -> &str {
        use koru_session_protocol::ApprovalIntent;
        match option.intent {
            ApprovalIntent::Allow => &self.approve,
            ApprovalIntent::AllowRemembered => &self.allow_remembered,
            ApprovalIntent::Reject => &self.reject,
            ApprovalIntent::RejectRemembered => &self.reject_remembered,
        }
    }

    pub fn approval_summary(&self, text: &str) -> String {
        match text {
            "command_execution" | "file_change" | "permissions" | "provider_action" => {
                self.category(text)
            }
            _ => text.to_owned(),
        }
    }

    pub fn category(&self, token: &str) -> String {
        match token {
            "command_execution" => &self.tool_command,
            "file_change" => &self.tool_file_change,
            "permissions" => &self.tool_permissions,
            "mcp_tool" => &self.tool_mcp,
            "dynamic_tool" => &self.tool_dynamic,
            "web_search" => &self.tool_web_search,
            "image_generation" => &self.tool_image_generation,
            "provider_action" => &self.tool_action,
            _ => &self.tool_action,
        }
        .clone()
    }

    pub fn format_usage(&self, usage: &AgentUsage) -> Option<String> {
        let mut values = Vec::new();
        if let Some(value) = usage.input_tokens {
            values.push((self.usage_value)(
                &self.input_tokens,
                &(self.number)(value),
            ));
        }
        if let Some(value) = usage.output_tokens {
            values.push((self.usage_value)(
                &self.output_tokens,
                &(self.number)(value),
            ));
        }
        if let Some(value) = usage.cached_input_tokens {
            values.push((self.usage_value)(
                &self.cached_tokens,
                &(self.number)(value),
            ));
        }
        (!values.is_empty()).then(|| (self.usage_list)(&values))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManagedPresentationToken {
    nonce: u64,
    through: StreamSequence,
}

impl ManagedPresentationToken {
    pub fn through(self) -> StreamSequence {
        self.through
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ManagedDrawOutcome {
    pub work: ManagedRenderWork,
    pub presentation: Option<ManagedPresentationToken>,
}

/// Reusable provider-neutral transcript and approval pane.
///
/// The view owns bounded presentation state only. It performs no I/O, process
/// work, IPC, root resolution, provider protocol parsing, or window work.
pub struct ManagedSessionView<I: SessionSurfaceInteraction> {
    interaction: I,
    model: model::ManagedModel,
    renderer: render::ManagedRenderer,
    copy: ManagedSessionCopy,
    visible: bool,
    /// **Whether this transcript may be acted on.** A session another
    /// connection controls can be read — the observer verbs need no lease —
    /// but every control in here needs one: an approval answer, a cancel,
    /// even the focus ring that says one is reachable. So a read-only
    /// transcript lays none of them out at all rather than drawing controls
    /// that would be refused (ADR-354: absent, not disabled).
    read_only: bool,
    follow_tail: bool,
    scroll_offset: f32,
    approval_scroll: f32,
    reveal_approval_focus: bool,
    approval_focus: ApprovalFocus,
    reviewed_approval: Option<ApprovalId>,
    last_layout: ManagedLayout,
    next_presentation_nonce: u64,
    pending_presentation: Option<ManagedPresentationToken>,
}

impl<I: SessionSurfaceInteraction> ManagedSessionView<I> {
    /// Send through another connection from now on (a terminal that moves to
    /// its own session connection keeps its model and scroll).
    pub fn set_interaction(&mut self, interaction: I) {
        self.interaction = interaction;
    }

    pub fn new(interaction: I, copy: ManagedSessionCopy) -> Self {
        Self {
            interaction,
            model: model::ManagedModel::default(),
            renderer: render::ManagedRenderer::default(),
            copy,
            visible: true,
            read_only: false,
            follow_tail: true,
            scroll_offset: 0.0,
            approval_scroll: 0.0,
            reveal_approval_focus: false,
            approval_focus: ApprovalFocus::Reject,
            reviewed_approval: None,
            last_layout: ManagedLayout::default(),
            next_presentation_nonce: 1,
            pending_presentation: None,
        }
    }

    pub fn interaction_mut(&mut self) -> &mut I {
        &mut self.interaction
    }

    pub fn copy(&self) -> &ManagedSessionCopy {
        &self.copy
    }

    pub fn set_copy(&mut self, copy: ManagedSessionCopy) {
        self.copy = copy;
        self.renderer.copy_changed();
    }

    pub fn set_visible(&mut self, visible: bool) -> bool {
        let changed = self.visible != visible;
        self.visible = visible;
        if !visible {
            self.pending_presentation = None;
        } else if !self.read_only && self.reviewed_approval.is_none() {
            self.reviewed_approval = self.model.newest_pending_approval();
            self.approval_focus = ApprovalFocus::Reject;
        }
        changed
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Read the transcript without being able to answer it. Answers whether
    /// anything moved, so a host can decide to redraw.
    ///
    /// Turning it on drops the approval a person had opened: an approval
    /// panel on a session this connection cannot answer for is a question
    /// with no button under it.
    pub fn set_read_only(&mut self, read_only: bool) -> bool {
        if self.read_only == read_only {
            return false;
        }
        self.read_only = read_only;
        if read_only {
            self.reviewed_approval = None;
        } else if self.visible {
            self.reviewed_approval = self.model.newest_pending_approval();
            self.approval_focus = ApprovalFocus::Reject;
        }
        true
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    pub fn transcript(&self) -> impl ExactSizeIterator<Item = &ManagedTranscriptItem> {
        self.model.items().iter()
    }

    pub fn apply_batch(
        &mut self,
        events: impl IntoIterator<Item = SequencedAgentEvent>,
    ) -> Result<ManagedApplyOutcome, ErrorCode> {
        let outcome = self.model.apply_batch(events, self.visible)?;
        if self
            .reviewed_approval
            .is_some_and(|id| !self.model.approval_actionable(id))
        {
            self.reviewed_approval = None;
        }
        if self.visible && !self.read_only && self.reviewed_approval.is_none() {
            self.reviewed_approval = self.model.newest_pending_approval();
            self.approval_focus = ApprovalFocus::Reject;
        }
        Ok(outcome)
    }

    pub fn apply_gap(
        &mut self,
        expected: StreamSequence,
        actual: StreamSequence,
    ) -> ManagedApplyOutcome {
        self.model.apply_gap(expected, actual, self.visible)
    }

    pub fn reset(&mut self) {
        self.model = model::ManagedModel::default();
        self.renderer.clear();
        self.follow_tail = true;
        self.scroll_offset = 0.0;
        self.approval_focus = ApprovalFocus::Reject;
        self.reviewed_approval = None;
        self.last_layout = ManagedLayout::default();
        self.pending_presentation = None;
    }

    pub fn approval_failed(&mut self, approval_id: ApprovalId, code: ErrorCode) -> bool {
        let changed = self
            .model
            .approval_failed(approval_id, interaction::approval_is_stale(&code));
        if changed && interaction::approval_is_stale(&code) {
            self.reviewed_approval = self.model.newest_pending_approval();
        }
        changed
    }

    pub fn set_scroll_offset(&mut self, offset: f32) {
        if self.reviewed_approval.is_some() && !self.reviewed_choices().is_empty() {
            self.approval_scroll = offset.max(0.0);
            self.reveal_approval_focus = false;
            return;
        }
        self.follow_tail = false;
        self.scroll_offset = offset.max(0.0);
    }

    pub fn return_to_bottom(&mut self) {
        self.follow_tail = true;
    }

    pub fn scroll_offset(&self) -> f32 {
        if self.reviewed_approval.is_some() && !self.reviewed_choices().is_empty() {
            return self.approval_scroll;
        }
        self.scroll_offset
    }

    pub fn approval_focus(&self) -> ApprovalFocus {
        self.approval_focus
    }

    pub fn hit_test(&self, x: f32, y: f32) -> Option<ManagedHit> {
        self.last_layout.hit(x, y)
    }

    pub fn pointer_press(&mut self, x: f32, y: f32) -> ManagedInputOutcome {
        if self.read_only {
            return ManagedInputOutcome::default();
        }
        match self.hit_test(x, y) {
            Some(hit) => self.activate(hit),
            None if self.last_layout.approval.is_some() => ManagedInputOutcome {
                handled: true,
                redraw: false,
            },
            None => ManagedInputOutcome::default(),
        }
    }

    fn reviewed_choices(&self) -> &[koru_session_protocol::ApprovalChoice] {
        match self
            .reviewed_approval
            .and_then(|id| self.model.approval(id))
            .map(|item| &item.content)
        {
            Some(ManagedItemContent::Approval { choices, .. }) => choices,
            _ => &[],
        }
    }

    pub fn handle_key(&mut self, key: ManagedKey) -> ManagedInputOutcome {
        if self.read_only {
            return ManagedInputOutcome::default();
        }
        if self.reviewed_approval.is_some() {
            return match key {
                ManagedKey::Tab { reverse } => {
                    self.reveal_approval_focus = true;
                    let choices = self.reviewed_choices();
                    if choices.is_empty() {
                        self.approval_focus = self.approval_focus.toggled();
                    } else {
                        let current = choices
                            .iter()
                            .position(|option| {
                                self.approval_focus == ApprovalFocus::Choice(option.id)
                            })
                            .unwrap_or_else(|| {
                                choices
                                    .iter()
                                    .position(|option| !option.intent.grants())
                                    .unwrap_or(0)
                            });
                        let next = if reverse {
                            (current + choices.len() - 1) % choices.len()
                        } else {
                            (current + 1) % choices.len()
                        };
                        self.approval_focus = ApprovalFocus::Choice(choices[next].id);
                    }
                    ManagedInputOutcome {
                        handled: true,
                        redraw: true,
                    }
                }
                ManagedKey::Enter | ManagedKey::Space => {
                    let hit = match self.approval_focus {
                        ApprovalFocus::Choice(id) => ManagedHit::ApprovalChoice(id),
                        ApprovalFocus::Reject => self
                            .reviewed_choices()
                            .iter()
                            .find(|option| !option.intent.grants())
                            .or_else(|| self.reviewed_choices().first())
                            .map_or(ManagedHit::ApprovalReject, |option| {
                                ManagedHit::ApprovalChoice(option.id)
                            }),
                        ApprovalFocus::Approve => ManagedHit::ApprovalApprove,
                    };
                    self.activate(hit)
                }
                ManagedKey::Escape => {
                    self.reviewed_approval = None;
                    ManagedInputOutcome {
                        handled: true,
                        redraw: true,
                    }
                }
                ManagedKey::CancelTurn => self.activate(ManagedHit::CancelTurn),
            };
        }
        match key {
            ManagedKey::CancelTurn => self.activate(ManagedHit::CancelTurn),
            _ => ManagedInputOutcome::default(),
        }
    }

    pub fn draw(
        &mut self,
        engine: &mut TextEngine,
        scene: &mut Scene,
        root: Affine,
        bounds: Rect,
        scale: f32,
    ) -> ManagedDrawOutcome {
        self.last_layout = ManagedLayout::default();
        if !self.visible || bounds.width() <= 0.0 || bounds.height() <= 0.0 {
            return ManagedDrawOutcome::default();
        }
        let approval = self
            .reviewed_approval
            .and_then(|id| self.model.approval(id));
        let (work, layout) = self.renderer.draw(
            engine,
            scene,
            render::ManagedRenderFrame {
                root,
                bounds,
                scale,
                items: self.model.items(),
                copy: &self.copy,
                scroll_offset: &mut self.scroll_offset,
                approval_scroll: &mut self.approval_scroll,
                reveal_approval_focus: std::mem::take(&mut self.reveal_approval_focus),
                follow_tail: self.follow_tail,
                approval,
                approval_focus: self.approval_focus,
                // **No cancel control on a transcript this connection may
                // not act on.** Cancelling a turn is a lease-holder's verb,
                // and a button that would be refused is worse than none.
                active_turn: !self.read_only && self.model.active_turn().is_some(),
            },
        );
        self.last_layout = layout;
        let presentation = (work.items_appended_to_scene > 0)
            .then(|| self.model.unpresented_through())
            .flatten()
            .map(|through| {
                if self
                    .pending_presentation
                    .is_some_and(|token| token.through == through)
                {
                    return self.pending_presentation.expect("checked above");
                }
                let token = ManagedPresentationToken {
                    nonce: self.next_presentation_nonce,
                    through,
                };
                self.next_presentation_nonce =
                    self.next_presentation_nonce.saturating_add(1).max(1);
                self.pending_presentation = Some(token);
                token
            });
        ManagedDrawOutcome { work, presentation }
    }

    /// Confirm that the frame carrying this token reached the native surface.
    /// Drawing or selecting alone never acknowledges attention.
    pub fn presented(&mut self, token: ManagedPresentationToken) -> bool {
        if self.pending_presentation != Some(token) {
            return false;
        }
        self.pending_presentation = None;
        self.model.mark_presented(token.through);
        self.interaction.dispatch(SessionSurfaceIntent::Managed(
            ManagedIntent::AttentionPresented {
                through: token.through,
            },
        ));
        true
    }

    fn activate(&mut self, hit: ManagedHit) -> ManagedInputOutcome {
        match hit {
            ManagedHit::ReviewApproval(approval_id) => {
                if !self.model.approval_actionable(approval_id) {
                    return ManagedInputOutcome::default();
                }
                self.reviewed_approval = Some(approval_id);
                self.approval_scroll = 0.0;
                self.approval_focus = ApprovalFocus::Reject;
                ManagedInputOutcome {
                    handled: true,
                    redraw: true,
                }
            }
            ManagedHit::ApprovalReject
            | ManagedHit::ApprovalApprove
            | ManagedHit::ApprovalChoice(_) => {
                let Some(approval_id) = self.reviewed_approval else {
                    return ManagedInputOutcome::default();
                };
                let choice = match hit {
                    ManagedHit::ApprovalChoice(id) => Some(id),
                    _ => None,
                };
                let decision = if let Some(id) = choice {
                    let Some(option) = self
                        .reviewed_choices()
                        .iter()
                        .find(|option| option.id == id)
                    else {
                        return ManagedInputOutcome::default();
                    };
                    option.intent.decision()
                } else if hit == ManagedHit::ApprovalApprove {
                    ApprovalDecision::Approve
                } else {
                    ApprovalDecision::Reject
                };
                if !self
                    .model
                    .mark_approval_submitting(approval_id, decision, choice)
                {
                    return ManagedInputOutcome {
                        handled: true,
                        redraw: false,
                    };
                }
                self.renderer.clear();
                self.interaction.dispatch(SessionSurfaceIntent::Managed(
                    ManagedIntent::AnswerApproval {
                        approval_id,
                        decision,
                        choice,
                    },
                ));
                ManagedInputOutcome {
                    handled: true,
                    redraw: true,
                }
            }
            ManagedHit::CancelTurn => {
                let Some(turn_id) = self.model.active_turn() else {
                    return ManagedInputOutcome::default();
                };
                if !self.model.begin_cancel(turn_id) {
                    return ManagedInputOutcome {
                        handled: true,
                        redraw: false,
                    };
                }
                self.interaction.dispatch(SessionSurfaceIntent::Managed(
                    ManagedIntent::CancelTurn { turn_id },
                ));
                ManagedInputOutcome {
                    handled: true,
                    redraw: true,
                }
            }
        }
    }
}

#[cfg(test)]
mod dynamic_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_copy_has_content_for_every_managed_state() {
        let copy = ManagedSessionCopy::default();
        for state in [
            ManagedState::Idle,
            ManagedState::Running,
            ManagedState::WaitingForInput,
            ManagedState::WaitingForApproval,
            ManagedState::Completed,
            ManagedState::Cancelled,
            ManagedState::Failed,
        ] {
            assert!(!copy.provider_state(state).is_empty());
        }
    }

    #[test]
    fn category_tokens_never_become_display_copy() {
        let copy = ManagedSessionCopy {
            tool_command: "localized command".into(),
            tool_file_change: "localized file change".into(),
            tool_permissions: "localized permissions".into(),
            tool_mcp: "localized MCP tool".into(),
            tool_dynamic: "localized dynamic tool".into(),
            tool_web_search: "localized web search".into(),
            tool_image_generation: "localized image generation".into(),
            tool_action: "localized action".into(),
            ..ManagedSessionCopy::default()
        };

        assert_eq!(copy.category("command_execution"), "localized command");
        assert_eq!(copy.category("file_change"), "localized file change");
        assert_eq!(copy.category("permissions"), "localized permissions");
        assert_eq!(copy.category("mcp_tool"), "localized MCP tool");
        assert_eq!(copy.category("dynamic_tool"), "localized dynamic tool");
        assert_eq!(copy.category("web_search"), "localized web search");
        assert_eq!(
            copy.category("image_generation"),
            "localized image generation"
        );
        assert_eq!(copy.category("provider_action"), "localized action");
        assert_eq!(copy.category("do not display this"), "localized action");
    }
}
