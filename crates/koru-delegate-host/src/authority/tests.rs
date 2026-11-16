// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::{
    BotAccess, BotId, BotOwner, BotProvenance, BotRequestId, CapturedBotPolicy, RunToolAuthority,
};

#[test]
fn launch_selection_allows_thirteen_bot_tools_and_preserves_six_ordinary_tools() {
    let workspace = WorkspaceId::new();
    let bot = RunToolAuthority::Bot {
        provenance: BotProvenance {
            parent: None,
            routine: None,
            owner: BotOwner {
                user_scope: koru_store_core::UserScopeId::new(),
                device: DeviceId::new(),
            },
            bot: BotId::mint(),
            submission: BotRequestId::mint(),
            chat_id: uuid::Uuid::new_v4(),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        },
        policy: CapturedBotPolicy {
            workspace,
            destination: workspace,
            access: BotAccess::Default,
            selected_pages: Default::default(),
            connections: Default::default(),
        },
    };
    let ordinary = allowlist();
    // The actual launch method calls this selector before issuing authority.
    // Issuing a bearer through HTTP is outside this no-service unit fixture.
    let selected = launch_tools(&bot, &ordinary);
    let names: std::collections::BTreeSet<_> = selected.iter().map(|name| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "search_documents",
            "read_document",
            "create_document",
            "edit_document",
            "list_memory",
            "read_memory",
            "remember_memory",
            "correct_memory",
            "forget_memory",
            "create_routine",
            "read_routine_creation",
            "retry_routine_creation",
            "list_routines",
        ]
        .into()
    );
    let selected = launch_tools(&RunToolAuthority::Ordinary {}, &ordinary);
    assert_eq!(selected, ordinary);
    assert_eq!(selected.len(), 6);
    assert_eq!(
        selected
            .iter()
            .map(|name| name.as_str())
            .collect::<Vec<_>>(),
        RUN_TOOLS
    );
}
