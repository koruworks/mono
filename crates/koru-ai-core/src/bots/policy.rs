// SPDX-License-Identifier: MPL-2.0

//! Current native Bot policy projection.
use super::*;
use koru_delegate_core::CapturedBotPolicy;

/// Canonical current policy only. Task capture is intersected by the delegate.
pub fn bot_native_policy(bot: &Bot) -> CapturedBotPolicy {
    bot_native_policy_from(
        bot.home_workspace,
        bot.access,
        &bot.selected_pages,
        &bot.connections,
    )
}

/// Shared policy construction for full Bot and bounded authority-only reads.
/// `connections` is the Bot's grant set as ai.db holds it. An empty set is
/// not serialized, so a policy without a grant keeps its pre-M5 bytes and
/// every digest computed over them.
pub fn bot_native_policy_from(
    home: WorkspaceId,
    access: BotAccess,
    pages: &[BotPage],
    connections: &std::collections::BTreeSet<koru_delegate_core::ConnectionGrantRef>,
) -> CapturedBotPolicy {
    CapturedBotPolicy {
        workspace: home,
        destination: home,
        access: match access {
            BotAccess::Default => koru_delegate_core::BotAccess::Default,
            BotAccess::Workspace => koru_delegate_core::BotAccess::Workspace,
        },
        selected_pages: pages.iter().map(|p| p.document).collect(),
        connections: connections.clone(),
    }
}
