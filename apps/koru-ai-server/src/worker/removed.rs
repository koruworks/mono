// SPDX-License-Identifier: MPL-2.0

//! **A removed profile's chats are hidden** (the user's call, 2026-11-04:
//! "yes, hide"). From the moment a profile is removed from this computer
//! until it is restored, none of its chats is listed, searched, read, or
//! changed through any surface — the app's lists and recents, a search
//! across profiles, an outside MCP client — and one named by its id, its
//! attempt's, a message's, a tool row's or an export's is answered as a
//! deleted chat is: not found. The chats themselves stay in `ai.db`
//! untouched, so a restore brings them back at once, and they are retired
//! only when the removal is final ([`WorkerState::retire_removed_profiles`]).
//!
//! **Which profiles are removed is the Store's answer and only the Store's**
//! (`profiles.removed_at`, through [`StoreContextPort::removed_profiles`]).
//! The Store sends nothing when a profile is removed or restored, so the
//! worker asks again before every command that lists, searches or names a
//! chat: a removal or a restore made while it runs counts from the next
//! command, with no restart. The set it holds is that last answer, for the
//! length of one command, never a copy of its own.
//!
//! **A Store that does not answer shows nothing it could be hiding.** The
//! command is refused as retryable — "Koru's profiles are temporarily
//! unavailable." — rather than served from an older answer, which a removal
//! may have outdated. Only Stop is served on the last answer, because
//! stopping a reply shows nothing.
//!
//! [`StoreContextPort::removed_profiles`]: koru_ai_core::StoreContextPort::removed_profiles

use std::collections::HashSet;
use std::sync::mpsc::SyncSender;

use koru_ai_core::{
    AiPersistence, ChatId, DocumentExportId, InferenceAttemptId, MessageId, ProfileId,
    ToolInvocationId,
};

use super::{ApiResult, Failure, WorkerCommand, WorkerState, ai_failure, failure, not_found};

/// How a command names the chat it reads or changes.
enum Named {
    Chat(ChatId),
    /// A new chat: hidden when its profile is removed, or when the id is
    /// already a removed profile's chat.
    NewChat {
        chat: ChatId,
        profile: Option<ProfileId>,
    },
    Attempt(InferenceAttemptId),
    Message(MessageId),
    Tool(ToolInvocationId),
    Export(DocumentExportId),
}

impl WorkerCommand {
    /// Whether serving this reads or changes chats, so the Store is asked
    /// which profiles are removed first. Stop is not: it is served on the
    /// last answer.
    fn asks_removed(&self) -> bool {
        match self {
            Self::Usage { chat, .. } => chat.is_some(),
            Self::CreateChat { .. }
            | Self::ListChats { .. }
            | Self::ReadChat { .. }
            | Self::RenameChat { .. }
            | Self::PinChat { .. }
            | Self::UpdateChatModel { .. }
            | Self::UpdateChatPrivacy { .. }
            | Self::UpdateChatFolder { .. }
            | Self::UpdateChatAgentSettings { .. }
            | Self::ToolListChats { .. }
            | Self::ToolSearchChats { .. }
            | Self::ToolReadChat { .. }
            | Self::UpdateChatToolPolicy { .. }
            | Self::AddChatWorkspaceGrants { .. }
            | Self::ReadChatToolPolicy { .. }
            | Self::DeleteChat { .. }
            | Self::ClearHistory { .. }
            | Self::Search { .. }
            | Self::Feedback { .. }
            | Self::BranchChat { .. }
            | Self::SendTurn { .. }
            | Self::ReadAttempt { .. }
            | Self::SteerAttempt { .. }
            | Self::ApproveToolCall { .. }
            | Self::DenyToolCall { .. }
            | Self::RetryTurn { .. }
            | Self::ExportChat { .. }
            | Self::ReadExport { .. }
            | Self::AttachmentAdmission { .. }
            | Self::StageAttachment { .. }
            | Self::RemoveStagedAttachment { .. }
            | Self::ResolveAttachments { .. } => true,
            _ => false,
        }
    }

    /// The one chat this command is about, when it names one. A listing,
    /// a search and a clear name none: they leave the hidden ones out.
    fn named(&self) -> Option<Named> {
        Some(match self {
            Self::CreateChat { request, .. } => Named::NewChat {
                chat: request.chat_id,
                profile: request.profile_id,
            },
            Self::ReadChat { chat, .. }
            | Self::RenameChat { chat, .. }
            | Self::PinChat { chat, .. }
            | Self::UpdateChatModel { chat, .. }
            | Self::UpdateChatPrivacy { chat, .. }
            | Self::UpdateChatFolder { chat, .. }
            | Self::UpdateChatAgentSettings { chat, .. }
            | Self::UpdateChatToolPolicy { chat, .. }
            | Self::AddChatWorkspaceGrants { chat, .. }
            | Self::ReadChatToolPolicy { chat, .. }
            | Self::DeleteChat { chat, .. }
            | Self::BranchChat { chat, .. }
            | Self::RetryTurn { chat, .. }
            | Self::ExportChat { chat, .. }
            | Self::AttachmentAdmission { chat, .. }
            | Self::RemoveStagedAttachment { chat, .. }
            | Self::ResolveAttachments { chat, .. } => Named::Chat(*chat),
            Self::Usage {
                chat: Some(chat), ..
            } => Named::Chat(*chat),
            Self::ToolReadChat { request, .. } => Named::Chat(request.chat_id),
            Self::SendTurn { request, .. } => Named::Chat(request.chat_id),
            Self::StageAttachment { record, .. } => Named::Chat(record.chat_id),
            Self::Feedback { message, .. } => Named::Message(*message),
            Self::ReadAttempt { attempt, .. }
            | Self::CancelAttempt { attempt, .. }
            | Self::SteerAttempt { attempt, .. } => Named::Attempt(*attempt),
            Self::ApproveToolCall { tool, .. } | Self::DenyToolCall { tool, .. } => {
                Named::Tool(*tool)
            }
            Self::ReadExport { export, .. } => Named::Export(*export),
            _ => return None,
        })
    }

    /// What a missing chat is answered with on this command's surface.
    fn missing(&self) -> Failure {
        match self {
            // The MCP tool's own answer, which a private chat shares.
            Self::ToolReadChat { .. } => failure("chat_not_found", "Chat not found.", false),
            _ => not_found(),
        }
    }

    /// Answer this command with `failure` instead of serving it.
    fn refuse(self, failure: Failure) {
        fn answer<T>(reply: SyncSender<ApiResult<T>>, failure: Failure) {
            let _ = reply.send(Err(failure));
        }
        match self {
            Self::CreateChat { reply, .. } => answer(reply, failure),
            Self::ListChats { reply, .. } => answer(reply, failure),
            Self::ReadChat { reply, .. } => answer(reply, failure),
            Self::RenameChat { reply, .. } => answer(reply, failure),
            Self::PinChat { reply, .. } => answer(reply, failure),
            Self::UpdateChatModel { reply, .. } => answer(reply, failure),
            Self::UpdateChatPrivacy { reply, .. } => answer(reply, failure),
            Self::UpdateChatFolder { reply, .. } => answer(reply, failure),
            Self::UpdateChatAgentSettings { reply, .. } => answer(reply, failure),
            Self::ToolListChats { reply, .. } => answer(reply, failure),
            Self::ToolSearchChats { reply, .. } => answer(reply, failure),
            Self::ToolReadChat { reply, .. } => answer(reply, failure),
            Self::UpdateChatToolPolicy { reply, .. } => answer(reply, failure),
            Self::AddChatWorkspaceGrants { reply, .. } => answer(reply, failure),
            Self::ReadChatToolPolicy { reply, .. } => answer(reply, failure),
            Self::Usage { reply, .. } => answer(reply, failure),
            Self::DeleteChat { reply, .. } => answer(reply, failure),
            Self::ClearHistory { reply, .. } => answer(reply, failure),
            Self::Search { reply, .. } => answer(reply, failure),
            Self::Feedback { reply, .. } => answer(reply, failure),
            Self::BranchChat { reply, .. } => answer(reply, failure),
            Self::SendTurn { reply, .. } => answer(reply, failure),
            Self::ReadAttempt { reply, .. } => answer(reply, failure),
            Self::CancelAttempt { reply, .. } => answer(reply, failure),
            Self::SteerAttempt { reply, .. } => answer(reply, failure),
            Self::ApproveToolCall { reply, .. } => answer(reply, failure),
            Self::DenyToolCall { reply, .. } => answer(reply, failure),
            Self::RetryTurn { reply, .. } => answer(reply, failure),
            Self::ExportChat { reply, .. } => answer(reply, failure),
            Self::ReadExport { reply, .. } => answer(reply, failure),
            Self::AttachmentAdmission { reply, .. } => answer(reply, failure),
            Self::StageAttachment { reply, .. } => answer(reply, failure),
            Self::RemoveStagedAttachment { reply, .. } => answer(reply, failure),
            Self::ResolveAttachments { reply, .. } => answer(reply, failure),
            other => {
                // Nothing else is ever gated; serving nothing here would
                // leave its caller waiting for the reply timeout.
                tracing::error!(
                    command = other.diagnostic_name(),
                    "a command with no chat was refused for a removed profile"
                );
            }
        }
    }
}

impl WorkerState {
    /// The gate in front of every command: ask the Store which profiles
    /// are removed when the command reads chats, and answer one that names
    /// a removed profile's chat as missing. `None` when it was answered.
    pub(super) fn hide_removed_profiles(
        &mut self,
        command: WorkerCommand,
    ) -> Option<WorkerCommand> {
        if command.asks_removed()
            && let Err(failure) = self.refresh_removed()
        {
            command.refuse(failure);
            return None;
        }
        let Some(named) = command.named() else {
            return Some(command);
        };
        match self.names_hidden(&named) {
            Ok(false) => Some(command),
            Ok(true) => {
                let missing = command.missing();
                command.refuse(missing);
                None
            }
            Err(failure) => {
                command.refuse(failure);
                None
            }
        }
    }

    /// Whether a chat of `profile` is hidden: its profile is removed.
    pub(super) fn is_hidden(&self, profile: Option<ProfileId>) -> bool {
        profile.is_some_and(|profile| self.removed.contains(&profile))
    }

    /// The removed profiles, for a search or a tool listing to leave out.
    pub(super) fn removed_profiles(&self) -> Vec<ProfileId> {
        self.removed.iter().copied().collect()
    }

    /// Ask the Store which profiles are removed. A harness with no Store
    /// has none. A profile newly removed has its replies stopped.
    fn refresh_removed(&mut self) -> ApiResult<()> {
        let Some(store) = self.store.clone() else {
            return Ok(());
        };
        let removed: HashSet<ProfileId> = match store.removed_profiles() {
            Ok(removed) => removed.into_iter().collect(),
            Err(error) => {
                tracing::warn!(class = ?error.class, "AI chats are not served: the Store did not say which profiles are removed");
                return Err(ai_failure(error));
            }
        };
        let newly = removed.difference(&self.removed).next().is_some();
        self.removed = removed;
        if newly {
            self.stop_hidden_replies();
        }
        Ok(())
    }

    /// Whether the chat `named` names is a removed profile's. Something
    /// that names no chat any more — a deleted one's attempt — is not.
    fn names_hidden(&self, named: &Named) -> ApiResult<bool> {
        if self.removed.is_empty() {
            return Ok(false);
        }
        let chat = match named {
            Named::Chat(chat) => Some(*chat),
            Named::NewChat { chat, profile } => {
                if self.is_hidden(*profile) {
                    return Ok(true);
                }
                Some(*chat)
            }
            Named::Attempt(attempt) => self
                .repository
                .attempt(*attempt)
                .map_err(ai_failure)?
                .and_then(|attempt| attempt.chat_id),
            Named::Message(message) => self
                .repository
                .message(*message)
                .map_err(ai_failure)?
                .map(|message| message.chat_id),
            Named::Tool(tool) => self
                .repository
                .tool_activity(*tool)
                .map_err(ai_failure)?
                .map(|activity| activity.chat_id),
            Named::Export(export) => self
                .repository
                .export(*export)
                .map_err(ai_failure)?
                .map(|export| export.chat_id),
        };
        let Some(chat) = chat else {
            return Ok(false);
        };
        Ok(self.is_hidden(
            self.repository
                .chat(chat)
                .map_err(ai_failure)?
                .and_then(|chat| chat.profile_id),
        ))
    }

    /// **A reply running in a chat whose profile was just removed is
    /// stopped**, as the person's Stop would: what it wrote is kept, and
    /// comes back with the chat on a restore. Nothing runs, waits for an
    /// approval, or holds Clear history back in a chat nobody can see.
    fn stop_hidden_replies(&mut self) {
        let running: Vec<InferenceAttemptId> = self.active_work.keys().copied().collect();
        let mut stopped = 0usize;
        for attempt_id in running {
            let hidden = self
                .names_hidden(&Named::Attempt(attempt_id))
                .unwrap_or(false);
            if !hidden {
                continue;
            }
            match self.stop_attempt(attempt_id) {
                Ok(_) => stopped += 1,
                Err(error) => {
                    tracing::warn!(attempt = %attempt_id, code = %error.code, "a removed profile's reply could not be stopped");
                }
            }
        }
        if stopped > 0 {
            tracing::info!(stopped, "a removed profile's replies were stopped");
        }
    }
}
