// SPDX-License-Identifier: MPL-2.0
//! Live-only native mini-app requests. Transcript/recovery cannot enqueue one.
use super::*;
use koru_ai_core::mini_apps::{Declaration, Envelope, HostContext, RefinementContext};

#[derive(Clone, Debug)]
pub struct MiniAppRefinement {
    pub host: HostContext,
    pub profile: ProfileId,
    pub chat: ChatId,
    pub envelope: koru_ai_core::mini_apps::UpdateEnvelope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QueuedTarget {
    profile: Option<ProfileId>,
    workspace: Option<WorkspaceId>,
    target: Option<koru_ai_core::mini_apps::RefinementTarget>,
    context: Option<std::sync::Arc<RefinementContext>>,
}

#[derive(Clone, Debug)]
pub struct MiniAppCreation {
    pub host: HostContext,
    pub profile: ProfileId,
    pub chat: ChatId,
    pub attempt: koru_ai_core::InferenceAttemptId,
    pub envelope: Envelope,
}
struct Pending {
    host: HostContext,
    refinement: Option<std::sync::Arc<RefinementContext>>,
    chat: ChatId,
    message: Option<MessageId>,
    attempt: Option<koru_ai_core::InferenceAttemptId>,
    received: bool,
    applied: bool,
    retired: bool,
}
pub(crate) struct NativeApps {
    renderer: uuid::Uuid,
    pub(super) enabled: bool,
    pending: Option<Pending>,
    ready: Option<MiniAppCreation>,
    update: Option<MiniAppRefinement>,
    target: Option<koru_ai_core::mini_apps::RefinementTarget>,
    context: Option<std::sync::Arc<RefinementContext>>,
    queued: Option<QueuedTarget>,
    pub(super) unavailable: bool,
    interrupted: bool,
    refinement_refused: bool,
}
impl Default for NativeApps {
    fn default() -> Self {
        Self {
            renderer: uuid::Uuid::new_v4(),
            enabled: false,
            pending: None,
            ready: None,
            update: None,
            target: None,
            context: None,
            queued: None,
            unavailable: false,
            interrupted: false,
            refinement_refused: false,
        }
    }
}
impl NativeApps {
    pub(super) fn clear(&mut self) {
        self.pending = None;
        self.ready = None;
        self.update = None;
        self.unavailable = false;
        self.interrupted = false;
        self.refinement_refused = false;
    }
}

impl ChatSurface {
    pub(crate) fn queued_mini_app_target(&self) -> QueuedTarget {
        if let Some(queued) = &self.native_apps.queued {
            return queued.clone();
        }
        QueuedTarget {
            profile: self.profile,
            workspace: self.context_workspace,
            target: self.native_apps.target,
            context: self.native_apps.context.clone(),
        }
    }
    pub(crate) fn capture_queued_mini_app(&mut self, captured: Option<QueuedTarget>) {
        self.native_apps.queued = captured;
    }
    pub fn mini_app_refinement_target(&self) -> Option<koru_ai_core::mini_apps::RefinementTarget> {
        self.native_apps.target
    }
    pub fn set_mini_app_refinement_context(&mut self, context: Option<RefinementContext>) {
        let context = context.filter(|context| context.validate().is_ok());
        self.set_mini_app_refinement_target(context.as_ref().map(|context| context.target));
        self.native_apps.context = context.map(std::sync::Arc::new);
    }
    pub(super) fn mini_app_request_refinement(
        &self,
        host: Option<HostContext>,
    ) -> Option<RefinementContext> {
        let pending = self.native_apps.pending.as_ref()?;
        (Some(pending.host) == host)
            .then(|| pending.refinement.as_deref().cloned())
            .flatten()
    }
    pub(crate) fn accepts_queued_mini_app_target(&mut self, captured: QueuedTarget) -> bool {
        let current = self.queued_mini_app_target();
        if (captured.target.is_some() || current.target.is_some()) && captured != current {
            self.native_apps.refinement_refused = true;
            self.refresh_mini_app_status();
            return false;
        }
        true
    }
    /// The containing native host supplies only its currently visible mini app.
    /// A request captures this value; later refreshes cannot retarget it.
    pub fn set_mini_app_refinement_target(
        &mut self,
        target: Option<koru_ai_core::mini_apps::RefinementTarget>,
    ) {
        if self.native_apps.pending.as_ref().is_some_and(|pending| {
            !pending.applied
                && pending.host.refinement.is_some_and(|old| {
                    target.is_none_or(|new| {
                        old.tab_id != new.tab_id || old.surface_id != new.surface_id
                    })
                })
        }) {
            self.interrupt_mini_app();
            self.native_apps.refinement_refused = true;
            self.refresh_mini_app_status();
        }
        self.native_apps.target = target;
        if self
            .native_apps
            .context
            .as_ref()
            .is_some_and(|context| Some(context.target) != target)
        {
            self.native_apps.context = None;
        }
    }
    pub fn mini_app_refined(&mut self, request: uuid::Uuid, applied: bool) {
        self.mini_app_opened(request, applied);
        self.native_apps.refinement_refused = !applied;
        self.refresh_mini_app_status();
    }
    pub fn take_mini_app_refinement(&mut self) -> Option<MiniAppRefinement> {
        let update = self.native_apps.update.take()?;
        (self.native_apps.enabled
            && self.profile == Some(update.profile)
            && self.context_workspace == Some(update.host.workspace_id)
            && self.current_chat_id() == Some(update.chat))
        .then_some(update)
    }
    /// Explicit opt-in by the native tab host; remote/other hosts default off.
    pub fn offer_mini_apps(&mut self, enabled: bool) {
        if self.native_apps.enabled != enabled {
            if !enabled {
                self.interrupt_mini_app();
                self.native_apps.ready = None;
            }
            self.native_apps.enabled = enabled;
        }
    }
    pub fn mini_app_renderer_id(&self) -> uuid::Uuid {
        self.native_apps.renderer
    }
    pub fn take_mini_app_creation(&mut self) -> Option<MiniAppCreation> {
        let creation = self.native_apps.ready.take()?;
        (self.native_apps.enabled
            && self.profile == Some(creation.profile)
            && self.context_workspace == Some(creation.host.workspace_id)
            && self.current_chat_id() == Some(creation.chat))
        .then_some(creation)
    }
    /// Local application receipt, never a provider/transport acknowledgement.
    pub fn mini_app_opened(&mut self, request: uuid::Uuid, opened: bool) {
        if let Some(pending) = &mut self.native_apps.pending
            && pending.host.request_id == request
        {
            pending.applied |= opened;
            self.native_apps.interrupted = !opened;
            if opened {
                self.native_apps.refinement_refused = false;
                // Selecting a just-created tab can retire the previous target
                // during relayout before this matching local receipt arrives.
                // Remove only our outcome wording, not an unrelated status.
                if self.status.as_deref().is_some_and(|status| {
                    [
                        koru_i18n::Key::MiniAppRefinementRefused,
                        koru_i18n::Key::MiniAppGenerationInterrupted,
                    ]
                    .into_iter()
                    .any(|key| status == koru_i18n::t(self.lang, key))
                }) {
                    self.status = None;
                }
            }
            if !opened {
                self.status = Some(
                    koru_i18n::t(self.lang, koru_i18n::Key::MiniAppGenerationInterrupted).into(),
                );
            }
        }
    }
    pub(super) fn mini_app_available(&self) -> bool {
        if !self.native_apps.enabled || self.context_workspace.is_none() {
            return false;
        }
        let Some(model) = self
            .transcript
            .as_ref()
            .map(|t| t.chat.current_model.clone())
            .or_else(|| self.current_selection())
        else {
            return false;
        };
        // The service's managed-provider catalog owns this list. A prefix
        // alone does not establish that a runtime can carry the MCP bridge.
        if self.settings.as_ref().is_some_and(|settings| {
            settings
                .agents
                .iter()
                .any(|agent| agent.id == model.provider)
        }) {
            return true;
        }
        match model.provider.as_str() {
            AiProviderId::OPENAI | AiProviderId::KORU => true,
            AiProviderId::OLLAMA => self
                .validation
                .as_ref()
                .and_then(|v| v.selected_model.as_ref())
                .is_some_and(|m| {
                    m.selection.provider == model.provider
                        && m.selection.requested_model == model.requested_model
                        && m.capabilities.native_tools
                }),
            _ => false,
        }
    }
    pub(super) fn begin_mini_app_request(&mut self, chat: ChatId) -> Option<HostContext> {
        let captured = self.queued_mini_app_target();
        self.native_apps.clear();
        if !self.mini_app_available() {
            return None;
        }
        let host = HostContext {
            renderer_id: self.native_apps.renderer,
            request_id: uuid::Uuid::new_v4(),
            workspace_id: self.context_workspace?,
            refinement: captured.target,
        };
        self.native_apps.pending = Some(Pending {
            host,
            refinement: captured.context,
            chat,
            message: None,
            attempt: None,
            received: false,
            applied: false,
            retired: false,
        });
        Some(host)
    }
    pub(super) fn retry_mini_app_request(
        &mut self,
        chat: ChatId,
        message: MessageId,
    ) -> Option<HostContext> {
        if !self.mini_app_available() {
            return None;
        }
        if !self.can_retry_mini_app_request(chat, message) {
            return None;
        }
        let pending = self.native_apps.pending.as_mut()?;
        if pending.chat != chat || pending.message != Some(message) || pending.applied {
            return None;
        }
        // Explicit user retry after a never-applied reply gets a fresh logical
        // request. A transport retry reuses the original SendTurnRequest.
        pending.host.request_id = uuid::Uuid::new_v4();
        pending.attempt = None;
        pending.message = None;
        pending.received = false;
        pending.retired = false;
        self.native_apps.ready = None;
        self.native_apps.update = None;
        self.native_apps.interrupted = false;
        self.native_apps.refinement_refused = false;
        self.native_apps.unavailable = false;
        Some(pending.host)
    }
    pub(super) fn can_retry_mini_app_request(&mut self, chat: ChatId, message: MessageId) -> bool {
        let changed = self.native_apps.pending.as_ref().is_some_and(|pending| {
            pending.chat == chat
                && pending.message == Some(message)
                && pending.host.refinement.is_some()
                && (pending.host.refinement != self.native_apps.target
                    || self.context_workspace != Some(pending.host.workspace_id))
        });
        if changed {
            self.native_apps.refinement_refused = true;
            self.refresh_mini_app_status();
        }
        !changed
    }
    pub(super) fn accept_mini_app_attempt(
        &mut self,
        accepted: &koru_ai_wire::SendTurnAccepted,
        host: HostContext,
    ) {
        if let Some(pending) = &mut self.native_apps.pending
            && pending.host == host
        {
            // Ownership may arrive after hide/stop. Retain it for an explicit
            // never-applied retry, without reviving the retired delivery.
            // RetryTurnRequest addresses the failed assistant response,
            // rather than the user message that prompted it.
            pending.message.get_or_insert(accepted.assistant_message_id);
            if accepted.replayed {
                pending.retired = true;
                pending.attempt = None;
                self.native_apps.ready = None;
                self.native_apps.update = None;
                return;
            }
            if pending.retired
                || pending.applied
                || pending.attempt.is_some()
                || !self.native_apps.enabled
            {
                return;
            }
            pending.attempt = Some(accepted.attempt_id);
        }
    }
    pub(super) fn receive_mini_app(&mut self, event: &PublicAiEvent) {
        if let PublicAiEvent::MiniAppUpdated {
            attempt_id,
            chat_id,
            profile_id,
            call_id,
            host,
            envelope,
            ..
        } = event
        {
            let Some(pending) = &mut self.native_apps.pending else {
                return;
            };
            if pending.host != *host
                || !self.native_apps.enabled
                || pending.retired
                || pending.chat != *chat_id
                || pending.attempt != Some(*attempt_id)
                || pending.received
                || pending.applied
                || self.profile != Some(*profile_id)
                || self.context_workspace != Some(host.workspace_id)
                || call_id.is_empty()
                || call_id.len() > 256
                || host.refinement.is_none_or(|target| {
                    target.surface_id.is_nil()
                        || envelope.update.surface_id != target.surface_name()
                })
                || self
                    .transcript
                    .as_ref()
                    .is_none_or(|t| t.chat.id != *chat_id)
                || envelope.arrangement().is_err()
            {
                return;
            }
            pending.received = true;
            self.native_apps.update = Some(MiniAppRefinement {
                host: *host,
                profile: *profile_id,
                chat: *chat_id,
                envelope: envelope.clone(),
            });
            return;
        }
        let PublicAiEvent::MiniAppCreated {
            attempt_id,
            chat_id,
            profile_id,
            invocation_id,
            call_id,
            host,
            envelope,
        } = event
        else {
            return;
        };
        let Some(pending) = &mut self.native_apps.pending else {
            return;
        };
        if pending.host != *host
            || !self.native_apps.enabled
            || pending.retired
            || pending.chat != *chat_id
            || pending.attempt != Some(*attempt_id)
            || pending.received
            || pending.applied
            || self.profile != Some(*profile_id)
            || self.context_workspace != Some(host.workspace_id)
            || call_id.is_empty()
            || call_id.len() > 256
            || envelope.surface.surface_id != format!("mini-app-{invocation_id}")
            || self
                .transcript
                .as_ref()
                .is_none_or(|t| t.chat.id != *chat_id)
            || Declaration::validate(envelope.clone()).is_err()
        {
            return;
        }
        pending.received = true;
        self.native_apps.ready = Some(MiniAppCreation {
            host: *host,
            profile: *profile_id,
            chat: *chat_id,
            attempt: *attempt_id,
            envelope: envelope.clone(),
        });
    }
    pub(super) fn interrupt_mini_app(&mut self) {
        if self
            .native_apps
            .pending
            .as_ref()
            .is_some_and(|p| !p.applied)
        {
            self.native_apps.interrupted = true;
            self.native_apps.ready = None;
            self.native_apps.update = None;
            // Keep only local message ownership, for an explicit retry.
            if let Some(pending) = &mut self.native_apps.pending {
                pending.attempt = None;
                pending.retired = true;
            }
            self.status =
                Some(koru_i18n::t(self.lang, koru_i18n::Key::MiniAppGenerationInterrupted).into());
        }
    }
    pub(super) fn mini_app_attempt_matches(&self, attempt: InferenceAttemptId) -> bool {
        self.native_apps
            .pending
            .as_ref()
            .is_some_and(|p| p.attempt == Some(attempt))
    }
    pub(super) fn owns_mini_app_outcome(&self, attempt: InferenceAttemptId) -> bool {
        self.native_apps
            .pending
            .as_ref()
            .is_some_and(|p| p.attempt == Some(attempt) && !p.applied && !p.retired)
    }
    pub(super) fn complete_mini_app_attempt(&mut self) {
        // Advertising this capability does not mean an ordinary chat reply
        // attempted creation. Forget a completed no-creation request quietly.
        if self
            .native_apps
            .pending
            .as_ref()
            .is_some_and(|p| !p.received)
            && !self.native_apps.interrupted
            && !self.native_apps.unavailable
        {
            self.native_apps.pending = None;
            return;
        }
        if let Some(pending) = &mut self.native_apps.pending {
            pending.attempt = None;
            pending.retired = true;
        }
    }
    pub(super) fn refresh_mini_app_status(&mut self) {
        let key = if self.native_apps.refinement_refused {
            Some(koru_i18n::Key::MiniAppRefinementRefused)
        } else if self.native_apps.unavailable {
            Some(koru_i18n::Key::MiniAppToolsUnavailable)
        } else if self.native_apps.interrupted {
            Some(koru_i18n::Key::MiniAppGenerationInterrupted)
        } else {
            None
        };
        if let Some(key) = key {
            self.status = Some(koru_i18n::t(self.lang, key).into());
        }
    }

    /// In-memory host integration fixture. No reader, Store, or provider I/O.
    #[cfg(any(test, feature = "test-support"))]
    pub fn arm_mini_app_for_test(
        &mut self,
        profile: ProfileId,
        workspace: WorkspaceId,
        chat: ChatId,
        attempt: InferenceAttemptId,
    ) -> HostContext {
        self.profile = Some(profile);
        self.context_workspace = Some(workspace);
        self.offer_mini_apps(true);
        let mut transcript = unmade_transcript(ModelSelection {
            provider: AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
            requested_model: "gpt-5".into(),
            default_reasoning: ReasoningChoice::Default,
        });
        transcript.chat.id = chat;
        transcript.chat.profile_id = Some(profile);
        self.transcript = Some(transcript);
        self.unmade_chat = None;
        self.attempt = Some(attempt);
        let host = self.begin_mini_app_request(chat).unwrap();
        self.accept_mini_app_attempt(
            &koru_ai_wire::SendTurnAccepted {
                attempt_id: attempt,
                user_message_id: MessageId::new(),
                assistant_message_id: MessageId::new(),
                replayed: false,
            },
            host,
        );
        host
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn receive_mini_app_event_for_test(&mut self, event: koru_ai_wire::StreamEvent) {
        self.apply_update(Update::Event(event));
    }
}

#[cfg(test)]
mod tests;
