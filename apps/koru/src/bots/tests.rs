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
    }
}

/// **Bots follows agents and Koru AI together, and nothing else** — not the
/// Agent-views preference, which hides the Agents and Artifacts views.
#[test]
fn bots_show_wherever_agents_and_koru_ai_are_on_whatever_agent_views_says() {
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
            false,
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
