// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_ai_core::bots::{Bot, BotAccess, BotConfig, BotId, BotPage, DeviceId, UserScopeId};
use koru_ai_core::{AiProviderId, ChatId, DocumentId, ModelSelection, ReasoningChoice};
use koru_features::{Features, testing};
use koru_store_core::ids::WorkspaceId;

/// A Bot as the service lists it: only the name and the pause matter to the
/// roster, everything else is a valid fixture.
pub(crate) fn bot(id: BotId, home: WorkspaceId, name: &str, paused: bool) -> Bot {
    Bot {
        id,
        home_workspace: home,
        user_scope: UserScopeId::new(),
        owner_device: DeviceId::new(),
        config: BotConfig {
            name: name.to_owned(),
            role: BotPage {
                workspace: home,
                document: DocumentId::new(),
            },
            working_folder: "C:\\fixture".to_owned(),
            model: ModelSelection {
                provider: AiProviderId::parse("agent-codex").expect("a provider id"),
                requested_model: "fixture".to_owned(),
                default_reasoning: ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
        },
        access: BotAccess::Default,
        selected_pages: Vec::new(),
        revision: 1,
        policy_revision: 1,
        main_chat: ChatId::new(),
        paused,
        folder_lock_request: None,
        folder_frozen_at_ms: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        lifecycle: Default::default(),
        open_operation: None,
        held_by: None,
        pending_change: None,
    }
}

/// **A Bot conversation in Trash, as `list_trashed_bot_chats` answers it**
/// (`apps/koru-ai-server/src/worker/bots/reads.rs`, over
/// `trashed_bot_chat_page`): the conversation with its Bot's label, the
/// operation that hid it, and which Restore brings it back.
pub(crate) fn trashed_chat(
    bot: BotId,
    bot_name: &str,
    chat: ChatId,
    title: &str,
    trashed_by: (
        koru_ai_core::bots::BotRequestId,
        koru_ai_core::bots::BotLifecycleKind,
    ),
    restore: koru_ai_core::bots::BotChatRestore,
) -> koru_ai_core::bots::BotTrashedChat {
    koru_ai_core::bots::BotTrashedChat {
        conversation: koru_ai_core::bots::BotConversation {
            chat: koru_ai_core::Chat {
                id: chat,
                title: title.to_owned(),
                private: false,
                current_model: ModelSelection {
                    provider: AiProviderId::parse("agent-codex").expect("a provider id"),
                    requested_model: "default".to_owned(),
                    default_reasoning: ReasoningChoice::Default,
                },
                branched_from: None,
                agent_working_folder: None,
                agent_mode: None,
                agent_fast: None,
                pinned_at: None,
                profile_id: None,
                bot: None,
                created_at: koru_ai_core::Timestamp::from_millis(1),
                updated_at: koru_ai_core::Timestamp::from_millis(2),
            },
            label: koru_ai_core::bots::BotChatLabel {
                bot_id: bot,
                bot_name: bot_name.to_owned(),
                purpose: koru_ai_core::bots::BotChatPurpose::Additional,
            },
        },
        trashed_by: koru_ai_core::bots::BotTrashedBy {
            request_id: trashed_by.0,
            kind: trashed_by.1,
        },
        trashed_at_ms: 10,
        restore,
    }
}

/// **Bots follows agents, and nothing else** (QUESTIONS O32) — not the
/// Koru AI switch, and not the Agent-views preference, which hides the
/// Agents and Artifacts views.
#[test]
fn bots_show_wherever_agents_are_on_whatever_ai_and_agent_views_say() {
    let on = Features {
        agents: true,
        ai: true,
        ..Features::ALL_OFF
    };
    for (features, expected) in [
        (Features::ALL_ON, true),
        (Features::ALL_OFF, false),
        (on, true),
        (
            Features {
                agent_views: false,
                ..Features::ALL_ON
            },
            true,
        ),
        (
            Features {
                agents: false,
                ..Features::ALL_ON
            },
            false,
        ),
        (
            Features {
                ai: false,
                ..Features::ALL_ON
            },
            true,
        ),
        (
            Features {
                agents: true,
                ..Features::ALL_OFF
            },
            true,
        ),
        (
            Features {
                agent_views: true,
                ..Features::ALL_OFF
            },
            false,
        ),
    ] {
        testing::with(features, || {
            assert_eq!(shown(), expected, "{features}");
            assert_eq!(
                crate::panes::explore::NavigatorMode::Bots.shown_in_product(),
                expected,
                "the rail asks the same question: {features}"
            );
        });
    }
}
