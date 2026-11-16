// SPDX-License-Identifier: MPL-2.0

//! **The domain crates, held to their source**: every non-test file of the
//! thirteen domain UI crates compiled in as text, and each crate's `lib.rs`
//! module by module. A module added to or removed from one of those roots
//! fails until it is classified here — a record of its own, folded into
//! another record, or excluded for a stated reason. What a crate adds
//! inside a module it already has is the manifest's hand review, not this.

use super::*;

/// Every non-test source file of the domain UI crates, from the repository
/// root: what the symbol, option and identity checks read.
pub(super) const SOURCES: &[(&str, &str)] = &[
    (
        "crates/koru-ai-ui/src/agents.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/agents.rs"),
    ),
    (
        "crates/koru-ai-ui/src/approval_modes.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/approval_modes.rs"),
    ),
    (
        "crates/koru-ai-ui/src/approval_modes/elements.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/approval_modes/elements.rs"),
    ),
    (
        "crates/koru-ai-ui/src/approval_modes/tokens.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/approval_modes/tokens.rs"),
    ),
    (
        "crates/koru-ai-ui/src/attachments.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/attachments.rs"),
    ),
    (
        "crates/koru-ai-ui/src/chat_status.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/chat_status.rs"),
    ),
    (
        "crates/koru-ai-ui/src/chats.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/chats.rs"),
    ),
    (
        "crates/koru-ai-ui/src/composer.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/composer.rs"),
    ),
    (
        "crates/koru-ai-ui/src/composer_band.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/composer_band.rs"),
    ),
    (
        "crates/koru-ai-ui/src/composer_plus.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/composer_plus.rs"),
    ),
    (
        "crates/koru-ai-ui/src/composer_shell.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/composer_shell.rs"),
    ),
    (
        "crates/koru-ai-ui/src/context_window.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/context_window.rs"),
    ),
    (
        "crates/koru-ai-ui/src/diagrams.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/diagrams.rs"),
    ),
    (
        "crates/koru-ai-ui/src/dictation.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/dictation.rs"),
    ),
    (
        "crates/koru-ai-ui/src/doc_links.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/doc_links.rs"),
    ),
    (
        "crates/koru-ai-ui/src/effort.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/effort.rs"),
    ),
    (
        "crates/koru-ai-ui/src/file_citations.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/file_citations.rs"),
    ),
    (
        "crates/koru-ai-ui/src/footer.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/footer.rs"),
    ),
    (
        "crates/koru-ai-ui/src/layout_cache.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/layout_cache.rs"),
    ),
    (
        "crates/koru-ai-ui/src/lib.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/lib.rs"),
    ),
    (
        "crates/koru-ai-ui/src/link_card.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/link_card.rs"),
    ),
    (
        "crates/koru-ai-ui/src/live_markdown.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/live_markdown.rs"),
    ),
    (
        "crates/koru-ai-ui/src/microphone.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/microphone.rs"),
    ),
    (
        "crates/koru-ai-ui/src/microphone/elements.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/microphone/elements.rs"),
    ),
    (
        "crates/koru-ai-ui/src/microphone/tokens.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/microphone/tokens.rs"),
    ),
    (
        "crates/koru-ai-ui/src/mini_apps.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/mini_apps.rs"),
    ),
    (
        "crates/koru-ai-ui/src/model_picker.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/model_picker.rs"),
    ),
    (
        "crates/koru-ai-ui/src/model_picker/context.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/model_picker/context.rs"),
    ),
    (
        "crates/koru-ai-ui/src/model_picker/elements.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/model_picker/elements.rs"),
    ),
    (
        "crates/koru-ai-ui/src/model_picker/fast.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/model_picker/fast.rs"),
    ),
    (
        "crates/koru-ai-ui/src/model_picker/paint.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/model_picker/paint.rs"),
    ),
    (
        "crates/koru-ai-ui/src/model_picker/tokens.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/model_picker/tokens.rs"),
    ),
    (
        "crates/koru-ai-ui/src/new_divider.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/new_divider.rs"),
    ),
    (
        "crates/koru-ai-ui/src/privacy.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/privacy.rs"),
    ),
    (
        "crates/koru-ai-ui/src/privacy/elements.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/privacy/elements.rs"),
    ),
    (
        "crates/koru-ai-ui/src/privacy/tokens.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/privacy/tokens.rs"),
    ),
    (
        "crates/koru-ai-ui/src/questions.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/questions.rs"),
    ),
    (
        "crates/koru-ai-ui/src/questions/elements.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/questions/elements.rs"),
    ),
    (
        "crates/koru-ai-ui/src/questions/tokens.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/questions/tokens.rs"),
    ),
    (
        "crates/koru-ai-ui/src/queue.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/queue.rs"),
    ),
    (
        "crates/koru-ai-ui/src/quote_pills.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/quote_pills.rs"),
    ),
    (
        "crates/koru-ai-ui/src/reader.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/reader.rs"),
    ),
    (
        "crates/koru-ai-ui/src/restored_scroll.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/restored_scroll.rs"),
    ),
    (
        "crates/koru-ai-ui/src/skills.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/skills.rs"),
    ),
    (
        "crates/koru-ai-ui/src/skills/elements.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/skills/elements.rs"),
    ),
    (
        "crates/koru-ai-ui/src/skills/tokens.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/skills/tokens.rs"),
    ),
    (
        "crates/koru-ai-ui/src/sources.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/sources.rs"),
    ),
    (
        "crates/koru-ai-ui/src/tool_groups.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/tool_groups.rs"),
    ),
    (
        "crates/koru-ai-ui/src/tool_output.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/tool_output.rs"),
    ),
    (
        "crates/koru-ai-ui/src/tool_output/command.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/tool_output/command.rs"),
    ),
    (
        "crates/koru-ai-ui/src/tool_output/diff.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/tool_output/diff.rs"),
    ),
    (
        "crates/koru-ai-ui/src/tool_output/file.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/tool_output/file.rs"),
    ),
    (
        "crates/koru-ai-ui/src/tool_output/layout.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/tool_output/layout.rs"),
    ),
    (
        "crates/koru-ai-ui/src/tool_output/open_path.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/tool_output/open_path.rs"),
    ),
    (
        "crates/koru-ai-ui/src/tool_output/picture.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/tool_output/picture.rs"),
    ),
    (
        "crates/koru-ai-ui/src/tool_output/search.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/tool_output/search.rs"),
    ),
    (
        "crates/koru-ai-ui/src/tool_output/structured.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/tool_output/structured.rs"),
    ),
    (
        "crates/koru-ai-ui/src/usage.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/usage.rs"),
    ),
    (
        "crates/koru-ai-ui/src/work_view.rs",
        include_str!("../../../../../../../crates/koru-ai-ui/src/work_view.rs"),
    ),
    (
        "crates/koru-agents-ui/src/interaction.rs",
        include_str!("../../../../../../../crates/koru-agents-ui/src/interaction.rs"),
    ),
    (
        "crates/koru-agents-ui/src/lib.rs",
        include_str!("../../../../../../../crates/koru-agents-ui/src/lib.rs"),
    ),
    (
        "crates/koru-agents-ui/src/surface.rs",
        include_str!("../../../../../../../crates/koru-agents-ui/src/surface.rs"),
    ),
    (
        "crates/koru-agents-ui/src/watching.rs",
        include_str!("../../../../../../../crates/koru-agents-ui/src/watching.rs"),
    ),
    (
        "crates/koru-appearance-ui/src/lib.rs",
        include_str!("../../../../../../../crates/koru-appearance-ui/src/lib.rs"),
    ),
    (
        "crates/koru-artifacts-ui/src/draw.rs",
        include_str!("../../../../../../../crates/koru-artifacts-ui/src/draw.rs"),
    ),
    (
        "crates/koru-artifacts-ui/src/gateway.rs",
        include_str!("../../../../../../../crates/koru-artifacts-ui/src/gateway.rs"),
    ),
    (
        "crates/koru-artifacts-ui/src/input.rs",
        include_str!("../../../../../../../crates/koru-artifacts-ui/src/input.rs"),
    ),
    (
        "crates/koru-artifacts-ui/src/lib.rs",
        include_str!("../../../../../../../crates/koru-artifacts-ui/src/lib.rs"),
    ),
    (
        "crates/koru-artifacts-ui/src/model.rs",
        include_str!("../../../../../../../crates/koru-artifacts-ui/src/model.rs"),
    ),
    (
        "crates/koru-artifacts-ui/src/reader.rs",
        include_str!("../../../../../../../crates/koru-artifacts-ui/src/reader.rs"),
    ),
    (
        "crates/koru-artifacts-ui/src/result.rs",
        include_str!("../../../../../../../crates/koru-artifacts-ui/src/result.rs"),
    ),
    (
        "crates/koru-artifacts-ui/src/rows.rs",
        include_str!("../../../../../../../crates/koru-artifacts-ui/src/rows.rs"),
    ),
    (
        "crates/koru-artifacts-ui/src/tab.rs",
        include_str!("../../../../../../../crates/koru-artifacts-ui/src/tab.rs"),
    ),
    (
        "crates/koru-artifacts-ui/src/tab/draw.rs",
        include_str!("../../../../../../../crates/koru-artifacts-ui/src/tab/draw.rs"),
    ),
    (
        "crates/koru-artifacts-ui/src/tab/input.rs",
        include_str!("../../../../../../../crates/koru-artifacts-ui/src/tab/input.rs"),
    ),
    (
        "crates/koru-artifacts-ui/src/text.rs",
        include_str!("../../../../../../../crates/koru-artifacts-ui/src/text.rs"),
    ),
    (
        "crates/koru-canvas-ui/src/arrow.rs",
        include_str!("../../../../../../../crates/koru-canvas-ui/src/arrow.rs"),
    ),
    (
        "crates/koru-canvas-ui/src/camera.rs",
        include_str!("../../../../../../../crates/koru-canvas-ui/src/camera.rs"),
    ),
    (
        "crates/koru-canvas-ui/src/dependency.rs",
        include_str!("../../../../../../../crates/koru-canvas-ui/src/dependency.rs"),
    ),
    (
        "crates/koru-canvas-ui/src/draw.rs",
        include_str!("../../../../../../../crates/koru-canvas-ui/src/draw.rs"),
    ),
    (
        "crates/koru-canvas-ui/src/input.rs",
        include_str!("../../../../../../../crates/koru-canvas-ui/src/input.rs"),
    ),
    (
        "crates/koru-canvas-ui/src/layout.rs",
        include_str!("../../../../../../../crates/koru-canvas-ui/src/layout.rs"),
    ),
    (
        "crates/koru-canvas-ui/src/lib.rs",
        include_str!("../../../../../../../crates/koru-canvas-ui/src/lib.rs"),
    ),
    (
        "crates/koru-canvas-ui/src/menu.rs",
        include_str!("../../../../../../../crates/koru-canvas-ui/src/menu.rs"),
    ),
    (
        "crates/koru-canvas-ui/src/model.rs",
        include_str!("../../../../../../../crates/koru-canvas-ui/src/model.rs"),
    ),
    (
        "crates/koru-canvas-ui/src/order.rs",
        include_str!("../../../../../../../crates/koru-canvas-ui/src/order.rs"),
    ),
    (
        "crates/koru-canvas-ui/src/words.rs",
        include_str!("../../../../../../../crates/koru-canvas-ui/src/words.rs"),
    ),
    (
        "crates/koru-chats-ui/src/draw.rs",
        include_str!("../../../../../../../crates/koru-chats-ui/src/draw.rs"),
    ),
    (
        "crates/koru-chats-ui/src/gateway.rs",
        include_str!("../../../../../../../crates/koru-chats-ui/src/gateway.rs"),
    ),
    (
        "crates/koru-chats-ui/src/input.rs",
        include_str!("../../../../../../../crates/koru-chats-ui/src/input.rs"),
    ),
    (
        "crates/koru-chats-ui/src/lib.rs",
        include_str!("../../../../../../../crates/koru-chats-ui/src/lib.rs"),
    ),
    (
        "crates/koru-chats-ui/src/reader.rs",
        include_str!("../../../../../../../crates/koru-chats-ui/src/reader.rs"),
    ),
    (
        "crates/koru-chats-ui/src/rows.rs",
        include_str!("../../../../../../../crates/koru-chats-ui/src/rows.rs"),
    ),
    (
        "crates/koru-chats-ui/src/seen.rs",
        include_str!("../../../../../../../crates/koru-chats-ui/src/seen.rs"),
    ),
    (
        "crates/koru-chats-ui/src/status.rs",
        include_str!("../../../../../../../crates/koru-chats-ui/src/status.rs"),
    ),
    (
        "crates/koru-chats-ui/src/text.rs",
        include_str!("../../../../../../../crates/koru-chats-ui/src/text.rs"),
    ),
    (
        "crates/koru-runs-ui/src/action.rs",
        include_str!("../../../../../../../crates/koru-runs-ui/src/action.rs"),
    ),
    (
        "crates/koru-runs-ui/src/column.rs",
        include_str!("../../../../../../../crates/koru-runs-ui/src/column.rs"),
    ),
    (
        "crates/koru-runs-ui/src/dependency.rs",
        include_str!("../../../../../../../crates/koru-runs-ui/src/dependency.rs"),
    ),
    (
        "crates/koru-runs-ui/src/index.rs",
        include_str!("../../../../../../../crates/koru-runs-ui/src/index.rs"),
    ),
    (
        "crates/koru-runs-ui/src/lib.rs",
        include_str!("../../../../../../../crates/koru-runs-ui/src/lib.rs"),
    ),
    (
        "crates/koru-runs-ui/src/links.rs",
        include_str!("../../../../../../../crates/koru-runs-ui/src/links.rs"),
    ),
    (
        "crates/koru-runs-ui/src/response.rs",
        include_str!("../../../../../../../crates/koru-runs-ui/src/response.rs"),
    ),
    (
        "crates/koru-runs-ui/src/state.rs",
        include_str!("../../../../../../../crates/koru-runs-ui/src/state.rs"),
    ),
    (
        "crates/koru-runs-ui/src/summary.rs",
        include_str!("../../../../../../../crates/koru-runs-ui/src/summary.rs"),
    ),
    (
        "crates/koru-runs-ui/src/views.rs",
        include_str!("../../../../../../../crates/koru-runs-ui/src/views.rs"),
    ),
    (
        "crates/koru-runs-ui/src/words.rs",
        include_str!("../../../../../../../crates/koru-runs-ui/src/words.rs"),
    ),
    (
        "crates/koru-search-ui/src/catalog.rs",
        include_str!("../../../../../../../crates/koru-search-ui/src/catalog.rs"),
    ),
    (
        "crates/koru-search-ui/src/debug.rs",
        include_str!("../../../../../../../crates/koru-search-ui/src/debug.rs"),
    ),
    (
        "crates/koru-search-ui/src/due_picker.rs",
        include_str!("../../../../../../../crates/koru-search-ui/src/due_picker.rs"),
    ),
    (
        "crates/koru-search-ui/src/entity_search.rs",
        include_str!("../../../../../../../crates/koru-search-ui/src/entity_search.rs"),
    ),
    (
        "crates/koru-search-ui/src/filters.rs",
        include_str!("../../../../../../../crates/koru-search-ui/src/filters.rs"),
    ),
    (
        "crates/koru-search-ui/src/lib.rs",
        include_str!("../../../../../../../crates/koru-search-ui/src/lib.rs"),
    ),
    (
        "crates/koru-search-ui/src/modal.rs",
        include_str!("../../../../../../../crates/koru-search-ui/src/modal.rs"),
    ),
    (
        "crates/koru-search-ui/src/reader.rs",
        include_str!("../../../../../../../crates/koru-search-ui/src/reader.rs"),
    ),
    (
        "crates/koru-search-ui/src/result_layout.rs",
        include_str!("../../../../../../../crates/koru-search-ui/src/result_layout.rs"),
    ),
    (
        "crates/koru-search-ui/src/scope.rs",
        include_str!("../../../../../../../crates/koru-search-ui/src/scope.rs"),
    ),
    (
        "crates/koru-search-ui/src/state.rs",
        include_str!("../../../../../../../crates/koru-search-ui/src/state.rs"),
    ),
    (
        "crates/koru-search-ui/src/store.rs",
        include_str!("../../../../../../../crates/koru-search-ui/src/store.rs"),
    ),
    (
        "crates/koru-settings-ui/src/action.rs",
        include_str!("../../../../../../../crates/koru-settings-ui/src/action.rs"),
    ),
    (
        "crates/koru-settings-ui/src/agents.rs",
        include_str!("../../../../../../../crates/koru-settings-ui/src/agents.rs"),
    ),
    (
        "crates/koru-settings-ui/src/dependency.rs",
        include_str!("../../../../../../../crates/koru-settings-ui/src/dependency.rs"),
    ),
    (
        "crates/koru-settings-ui/src/global.rs",
        include_str!("../../../../../../../crates/koru-settings-ui/src/global.rs"),
    ),
    (
        "crates/koru-settings-ui/src/lib.rs",
        include_str!("../../../../../../../crates/koru-settings-ui/src/lib.rs"),
    ),
    (
        "crates/koru-settings-ui/src/links.rs",
        include_str!("../../../../../../../crates/koru-settings-ui/src/links.rs"),
    ),
    (
        "crates/koru-settings-ui/src/page.rs",
        include_str!("../../../../../../../crates/koru-settings-ui/src/page.rs"),
    ),
    (
        "crates/koru-settings-ui/src/state.rs",
        include_str!("../../../../../../../crates/koru-settings-ui/src/state.rs"),
    ),
    (
        "crates/koru-settings-ui/src/terminal.rs",
        include_str!("../../../../../../../crates/koru-settings-ui/src/terminal.rs"),
    ),
    (
        "crates/koru-shell-chrome/src/banner.rs",
        include_str!("../../../../../../../crates/koru-shell-chrome/src/banner.rs"),
    ),
    (
        "crates/koru-shell-chrome/src/branch.rs",
        include_str!("../../../../../../../crates/koru-shell-chrome/src/branch.rs"),
    ),
    (
        "crates/koru-shell-chrome/src/caption.rs",
        include_str!("../../../../../../../crates/koru-shell-chrome/src/caption.rs"),
    ),
    (
        "crates/koru-shell-chrome/src/lane.rs",
        include_str!("../../../../../../../crates/koru-shell-chrome/src/lane.rs"),
    ),
    (
        "crates/koru-shell-chrome/src/lib.rs",
        include_str!("../../../../../../../crates/koru-shell-chrome/src/lib.rs"),
    ),
    (
        "crates/koru-shell-chrome/src/regions.rs",
        include_str!("../../../../../../../crates/koru-shell-chrome/src/regions.rs"),
    ),
    (
        "crates/koru-shell-chrome/src/reorder.rs",
        include_str!("../../../../../../../crates/koru-shell-chrome/src/reorder.rs"),
    ),
    (
        "crates/koru-shell-chrome/src/status_chip.rs",
        include_str!("../../../../../../../crates/koru-shell-chrome/src/status_chip.rs"),
    ),
    (
        "crates/koru-shell-chrome/src/status_mark.rs",
        include_str!("../../../../../../../crates/koru-shell-chrome/src/status_mark.rs"),
    ),
    (
        "crates/koru-shell-chrome/src/tabs.rs",
        include_str!("../../../../../../../crates/koru-shell-chrome/src/tabs.rs"),
    ),
    (
        "crates/koru-shell-chrome/src/title_icon.rs",
        include_str!("../../../../../../../crates/koru-shell-chrome/src/title_icon.rs"),
    ),
    (
        "crates/koru-socials-ui/src/backend.rs",
        include_str!("../../../../../../../crates/koru-socials-ui/src/backend.rs"),
    ),
    (
        "crates/koru-socials-ui/src/lib.rs",
        include_str!("../../../../../../../crates/koru-socials-ui/src/lib.rs"),
    ),
    (
        "crates/koru-socials-ui/src/navigation.rs",
        include_str!("../../../../../../../crates/koru-socials-ui/src/navigation.rs"),
    ),
    (
        "crates/koru-socials-ui/src/paint.rs",
        include_str!("../../../../../../../crates/koru-socials-ui/src/paint.rs"),
    ),
    (
        "crates/koru-socials-ui/src/preview.rs",
        include_str!("../../../../../../../crates/koru-socials-ui/src/preview.rs"),
    ),
    (
        "crates/koru-voice-ui/src/hud.rs",
        include_str!("../../../../../../../crates/koru-voice-ui/src/hud.rs"),
    ),
    (
        "crates/koru-voice-ui/src/lib.rs",
        include_str!("../../../../../../../crates/koru-voice-ui/src/lib.rs"),
    ),
    (
        "crates/koru-workspaces-ui/src/action.rs",
        include_str!("../../../../../../../crates/koru-workspaces-ui/src/action.rs"),
    ),
    (
        "crates/koru-workspaces-ui/src/controller.rs",
        include_str!("../../../../../../../crates/koru-workspaces-ui/src/controller.rs"),
    ),
    (
        "crates/koru-workspaces-ui/src/controller/pins.rs",
        include_str!("../../../../../../../crates/koru-workspaces-ui/src/controller/pins.rs"),
    ),
    (
        "crates/koru-workspaces-ui/src/dependency.rs",
        include_str!("../../../../../../../crates/koru-workspaces-ui/src/dependency.rs"),
    ),
    (
        "crates/koru-workspaces-ui/src/lib.rs",
        include_str!("../../../../../../../crates/koru-workspaces-ui/src/lib.rs"),
    ),
    (
        "crates/koru-workspaces-ui/src/links.rs",
        include_str!("../../../../../../../crates/koru-workspaces-ui/src/links.rs"),
    ),
    (
        "crates/koru-workspaces-ui/src/links/authority.rs",
        include_str!("../../../../../../../crates/koru-workspaces-ui/src/links/authority.rs"),
    ),
    (
        "crates/koru-workspaces-ui/src/links/fixtures.rs",
        include_str!("../../../../../../../crates/koru-workspaces-ui/src/links/fixtures.rs"),
    ),
    (
        "crates/koru-workspaces-ui/src/pending.rs",
        include_str!("../../../../../../../crates/koru-workspaces-ui/src/pending.rs"),
    ),
    (
        "crates/koru-workspaces-ui/src/state.rs",
        include_str!("../../../../../../../crates/koru-workspaces-ui/src/state.rs"),
    ),
    // Batch 2b: the editor, document and host crates.
    (
        "crates/koru-editor/src/accessibility.rs",
        include_str!("../../../../../../../crates/koru-editor/src/accessibility.rs"),
    ),
    (
        "crates/koru-editor/src/accessibility/actions.rs",
        include_str!("../../../../../../../crates/koru-editor/src/accessibility/actions.rs"),
    ),
    (
        "crates/koru-editor/src/accessibility/events.rs",
        include_str!("../../../../../../../crates/koru-editor/src/accessibility/events.rs"),
    ),
    (
        "crates/koru-editor/src/accessibility/geometry.rs",
        include_str!("../../../../../../../crates/koru-editor/src/accessibility/geometry.rs"),
    ),
    (
        "crates/koru-editor/src/accessibility/projection.rs",
        include_str!("../../../../../../../crates/koru-editor/src/accessibility/projection.rs"),
    ),
    (
        "crates/koru-editor/src/accessibility/text_positions.rs",
        include_str!("../../../../../../../crates/koru-editor/src/accessibility/text_positions.rs"),
    ),
    (
        "crates/koru-editor/src/body.rs",
        include_str!("../../../../../../../crates/koru-editor/src/body.rs"),
    ),
    (
        "crates/koru-editor/src/browser.rs",
        include_str!("../../../../../../../crates/koru-editor/src/browser.rs"),
    ),
    (
        "crates/koru-editor/src/chips.rs",
        include_str!("../../../../../../../crates/koru-editor/src/chips.rs"),
    ),
    (
        "crates/koru-editor/src/clipboard.rs",
        include_str!("../../../../../../../crates/koru-editor/src/clipboard.rs"),
    ),
    (
        "crates/koru-editor/src/components/attr_menu.rs",
        include_str!("../../../../../../../crates/koru-editor/src/components/attr_menu.rs"),
    ),
    (
        "crates/koru-editor/src/components/color_picker.rs",
        include_str!("../../../../../../../crates/koru-editor/src/components/color_picker.rs"),
    ),
    (
        "crates/koru-editor/src/components/command_menu.rs",
        include_str!("../../../../../../../crates/koru-editor/src/components/command_menu.rs"),
    ),
    (
        "crates/koru-editor/src/components/command_menu/accessibility.rs",
        include_str!(
            "../../../../../../../crates/koru-editor/src/components/command_menu/accessibility.rs"
        ),
    ),
    (
        "crates/koru-editor/src/components/date_picker.rs",
        include_str!("../../../../../../../crates/koru-editor/src/components/date_picker.rs"),
    ),
    (
        "crates/koru-editor/src/components/draw.rs",
        include_str!("../../../../../../../crates/koru-editor/src/components/draw.rs"),
    ),
    (
        "crates/koru-editor/src/components/emoji_picker.rs",
        include_str!("../../../../../../../crates/koru-editor/src/components/emoji_picker.rs"),
    ),
    (
        "crates/koru-editor/src/components/entity_picker.rs",
        include_str!("../../../../../../../crates/koru-editor/src/components/entity_picker.rs"),
    ),
    (
        "crates/koru-editor/src/components/language_picker.rs",
        include_str!("../../../../../../../crates/koru-editor/src/components/language_picker.rs"),
    ),
    (
        "crates/koru-editor/src/components/link_prompt.rs",
        include_str!("../../../../../../../crates/koru-editor/src/components/link_prompt.rs"),
    ),
    (
        "crates/koru-editor/src/components/list_picker.rs",
        include_str!("../../../../../../../crates/koru-editor/src/components/list_picker.rs"),
    ),
    (
        "crates/koru-editor/src/components/mod.rs",
        include_str!("../../../../../../../crates/koru-editor/src/components/mod.rs"),
    ),
    (
        "crates/koru-editor/src/components/name_prompt.rs",
        include_str!("../../../../../../../crates/koru-editor/src/components/name_prompt.rs"),
    ),
    (
        "crates/koru-editor/src/components/priority_picker.rs",
        include_str!("../../../../../../../crates/koru-editor/src/components/priority_picker.rs"),
    ),
    (
        "crates/koru-editor/src/components/tag_picker.rs",
        include_str!("../../../../../../../crates/koru-editor/src/components/tag_picker.rs"),
    ),
    (
        "crates/koru-editor/src/diagram.rs",
        include_str!("../../../../../../../crates/koru-editor/src/diagram.rs"),
    ),
    (
        "crates/koru-editor/src/dictation.rs",
        include_str!("../../../../../../../crates/koru-editor/src/dictation.rs"),
    ),
    (
        "crates/koru-editor/src/draw.rs",
        include_str!("../../../../../../../crates/koru-editor/src/draw.rs"),
    ),
    (
        "crates/koru-editor/src/editor.rs",
        include_str!("../../../../../../../crates/koru-editor/src/editor.rs"),
    ),
    (
        "crates/koru-editor/src/emoji.rs",
        include_str!("../../../../../../../crates/koru-editor/src/emoji.rs"),
    ),
    (
        "crates/koru-editor/src/input/action.rs",
        include_str!("../../../../../../../crates/koru-editor/src/input/action.rs"),
    ),
    (
        "crates/koru-editor/src/input/keymap.rs",
        include_str!("../../../../../../../crates/koru-editor/src/input/keymap.rs"),
    ),
    (
        "crates/koru-editor/src/input/mod.rs",
        include_str!("../../../../../../../crates/koru-editor/src/input/mod.rs"),
    ),
    (
        "crates/koru-editor/src/lib.rs",
        include_str!("../../../../../../../crates/koru-editor/src/lib.rs"),
    ),
    (
        "crates/koru-editor/src/objective.rs",
        include_str!("../../../../../../../crates/koru-editor/src/objective.rs"),
    ),
    (
        "crates/koru-editor/src/paragraph.rs",
        include_str!("../../../../../../../crates/koru-editor/src/paragraph.rs"),
    ),
    (
        "crates/koru-editor/src/pending_image.rs",
        include_str!("../../../../../../../crates/koru-editor/src/pending_image.rs"),
    ),
    (
        "crates/koru-editor/src/pillinfo.rs",
        include_str!("../../../../../../../crates/koru-editor/src/pillinfo.rs"),
    ),
    (
        "crates/koru-editor/src/policy.rs",
        include_str!("../../../../../../../crates/koru-editor/src/policy.rs"),
    ),
    (
        "crates/koru-editor/src/reminders.rs",
        include_str!("../../../../../../../crates/koru-editor/src/reminders.rs"),
    ),
    (
        "crates/koru-editor/src/rowlayout.rs",
        include_str!("../../../../../../../crates/koru-editor/src/rowlayout.rs"),
    ),
    (
        "crates/koru-editor/src/table/mod.rs",
        include_str!("../../../../../../../crates/koru-editor/src/table/mod.rs"),
    ),
    (
        "crates/koru-editor/src/tasks.rs",
        include_str!("../../../../../../../crates/koru-editor/src/tasks.rs"),
    ),
    (
        "crates/koru-editor/src/trigger.rs",
        include_str!("../../../../../../../crates/koru-editor/src/trigger.rs"),
    ),
    (
        "crates/koru-editor/src/utils/dates.rs",
        include_str!("../../../../../../../crates/koru-editor/src/utils/dates.rs"),
    ),
    (
        "crates/koru-editor/src/utils/mod.rs",
        include_str!("../../../../../../../crates/koru-editor/src/utils/mod.rs"),
    ),
    (
        "crates/koru-editor/src/utils/when.rs",
        include_str!("../../../../../../../crates/koru-editor/src/utils/when.rs"),
    ),
    (
        "crates/koru-editor/src/utils/zone.rs",
        include_str!("../../../../../../../crates/koru-editor/src/utils/zone.rs"),
    ),
    (
        "crates/koru-editor/src/view_state.rs",
        include_str!("../../../../../../../crates/koru-editor/src/view_state.rs"),
    ),
    (
        "crates/koru-markdown-view/src/document.rs",
        include_str!("../../../../../../../crates/koru-markdown-view/src/document.rs"),
    ),
    (
        "crates/koru-markdown-view/src/lib.rs",
        include_str!("../../../../../../../crates/koru-markdown-view/src/lib.rs"),
    ),
    (
        "crates/koru-markdown-view/src/text.rs",
        include_str!("../../../../../../../crates/koru-markdown-view/src/text.rs"),
    ),
    (
        "crates/koru-markdown-view/src/view.rs",
        include_str!("../../../../../../../crates/koru-markdown-view/src/view.rs"),
    ),
    (
        "crates/koru-session-view/src/cursor.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/cursor.rs"),
    ),
    (
        "crates/koru-session-view/src/geometry.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/geometry.rs"),
    ),
    (
        "crates/koru-session-view/src/history.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/history.rs"),
    ),
    (
        "crates/koru-session-view/src/input.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/input.rs"),
    ),
    (
        "crates/koru-session-view/src/interaction.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/interaction.rs"),
    ),
    (
        "crates/koru-session-view/src/lib.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/lib.rs"),
    ),
    (
        "crates/koru-session-view/src/managed/interaction.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/managed/interaction.rs"),
    ),
    (
        "crates/koru-session-view/src/managed/layout.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/managed/layout.rs"),
    ),
    (
        "crates/koru-session-view/src/managed/mod.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/managed/mod.rs"),
    ),
    (
        "crates/koru-session-view/src/managed/model.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/managed/model.rs"),
    ),
    (
        "crates/koru-session-view/src/managed/render.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/managed/render.rs"),
    ),
    (
        "crates/koru-session-view/src/model.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/model.rs"),
    ),
    (
        "crates/koru-session-view/src/pending_input/interaction.rs",
        include_str!(
            "../../../../../../../crates/koru-session-view/src/pending_input/interaction.rs"
        ),
    ),
    (
        "crates/koru-session-view/src/pending_input/layout.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/pending_input/layout.rs"),
    ),
    (
        "crates/koru-session-view/src/pending_input/mod.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/pending_input/mod.rs"),
    ),
    (
        "crates/koru-session-view/src/pending_input/model.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/pending_input/model.rs"),
    ),
    (
        "crates/koru-session-view/src/pending_input/render.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/pending_input/render.rs"),
    ),
    (
        "crates/koru-session-view/src/render.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/render.rs"),
    ),
    (
        "crates/koru-session-view/src/row.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/row.rs"),
    ),
    (
        "crates/koru-session-view/src/selection.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/selection.rs"),
    ),
    (
        "crates/koru-session-view/src/style.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/style.rs"),
    ),
    (
        "crates/koru-session-view/src/surface.rs",
        include_str!("../../../../../../../crates/koru-session-view/src/surface.rs"),
    ),
    (
        "crates/koru-app-shell/src/banner_state.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/banner_state.rs"),
    ),
    (
        "crates/koru-app-shell/src/chrome.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/chrome.rs"),
    ),
    (
        "crates/koru-app-shell/src/command.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/command.rs"),
    ),
    (
        "crates/koru-app-shell/src/element.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/element.rs"),
    ),
    (
        "crates/koru-app-shell/src/host.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/host.rs"),
    ),
    (
        "crates/koru-app-shell/src/keys.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/keys.rs"),
    ),
    (
        "crates/koru-app-shell/src/lib.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/lib.rs"),
    ),
    (
        "crates/koru-app-shell/src/panel.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/panel.rs"),
    ),
    (
        "crates/koru-app-shell/src/popover.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/popover.rs"),
    ),
    (
        "crates/koru-app-shell/src/state.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/state.rs"),
    ),
    (
        "crates/koru-app-shell/src/surface.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/surface.rs"),
    ),
    (
        "crates/koru-app-shell/src/tabset.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/tabset.rs"),
    ),
    (
        "crates/koru-app-shell/src/tabslide.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/tabslide.rs"),
    ),
    (
        "crates/koru-app-shell/src/tabstrip.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/tabstrip.rs"),
    ),
    (
        "crates/koru-app-shell/src/zoom_label.rs",
        include_str!("../../../../../../../crates/koru-app-shell/src/zoom_label.rs"),
    ),
    (
        "crates/koru-webview/src/debug.rs",
        include_str!("../../../../../../../crates/koru-webview/src/debug.rs"),
    ),
    (
        "crates/koru-webview/src/engine.rs",
        include_str!("../../../../../../../crates/koru-webview/src/engine.rs"),
    ),
    (
        "crates/koru-webview/src/js.rs",
        include_str!("../../../../../../../crates/koru-webview/src/js.rs"),
    ),
    (
        "crates/koru-webview/src/lib.rs",
        include_str!("../../../../../../../crates/koru-webview/src/lib.rs"),
    ),
    (
        "crates/koru-webview/src/stub.rs",
        include_str!("../../../../../../../crates/koru-webview/src/stub.rs"),
    ),
    (
        "crates/koru-window/src/accessibility.rs",
        include_str!("../../../../../../../crates/koru-window/src/accessibility.rs"),
    ),
    (
        "crates/koru-window/src/activation.rs",
        include_str!("../../../../../../../crates/koru-window/src/activation.rs"),
    ),
    (
        "crates/koru-window/src/app.rs",
        include_str!("../../../../../../../crates/koru-window/src/app.rs"),
    ),
    (
        "crates/koru-window/src/appearance.rs",
        include_str!("../../../../../../../crates/koru-window/src/appearance.rs"),
    ),
    (
        "crates/koru-window/src/console.rs",
        include_str!("../../../../../../../crates/koru-window/src/console.rs"),
    ),
    (
        "crates/koru-window/src/desktop_preferences.rs",
        include_str!("../../../../../../../crates/koru-window/src/desktop_preferences.rs"),
    ),
    (
        "crates/koru-window/src/dispatch.rs",
        include_str!("../../../../../../../crates/koru-window/src/dispatch.rs"),
    ),
    (
        "crates/koru-window/src/dock.rs",
        include_str!("../../../../../../../crates/koru-window/src/dock.rs"),
    ),
    (
        "crates/koru-window/src/frame.rs",
        include_str!("../../../../../../../crates/koru-window/src/frame.rs"),
    ),
    (
        "crates/koru-window/src/framestats.rs",
        include_str!("../../../../../../../crates/koru-window/src/framestats.rs"),
    ),
    (
        "crates/koru-window/src/gpu.rs",
        include_str!("../../../../../../../crates/koru-window/src/gpu.rs"),
    ),
    (
        "crates/koru-window/src/hairline.rs",
        include_str!("../../../../../../../crates/koru-window/src/hairline.rs"),
    ),
    (
        "crates/koru-window/src/input.rs",
        include_str!("../../../../../../../crates/koru-window/src/input.rs"),
    ),
    (
        "crates/koru-window/src/key_quarantine.rs",
        include_str!("../../../../../../../crates/koru-window/src/key_quarantine.rs"),
    ),
    (
        "crates/koru-window/src/lib.rs",
        include_str!("../../../../../../../crates/koru-window/src/lib.rs"),
    ),
    (
        "crates/koru-window/src/multi.rs",
        include_str!("../../../../../../../crates/koru-window/src/multi.rs"),
    ),
    (
        "crates/koru-window/src/non_activating.rs",
        include_str!("../../../../../../../crates/koru-window/src/non_activating.rs"),
    ),
    (
        "crates/koru-window/src/offscreen.rs",
        include_str!("../../../../../../../crates/koru-window/src/offscreen.rs"),
    ),
    (
        "crates/koru-window/src/open_bytes.rs",
        include_str!("../../../../../../../crates/koru-window/src/open_bytes.rs"),
    ),
    (
        "crates/koru-window/src/open_uri.rs",
        include_str!("../../../../../../../crates/koru-window/src/open_uri.rs"),
    ),
    (
        "crates/koru-window/src/renderer.rs",
        include_str!("../../../../../../../crates/koru-window/src/renderer.rs"),
    ),
    (
        "crates/koru-window/src/request.rs",
        include_str!("../../../../../../../crates/koru-window/src/request.rs"),
    ),
    (
        "crates/koru-window/src/resize.rs",
        include_str!("../../../../../../../crates/koru-window/src/resize.rs"),
    ),
    (
        "crates/koru-window/src/reveal.rs",
        include_str!("../../../../../../../crates/koru-window/src/reveal.rs"),
    ),
    (
        "crates/koru-window/src/rounded_region.rs",
        include_str!("../../../../../../../crates/koru-window/src/rounded_region.rs"),
    ),
    (
        "crates/koru-window/src/search_geometry.rs",
        include_str!("../../../../../../../crates/koru-window/src/search_geometry.rs"),
    ),
    (
        "crates/koru-window/src/snap.rs",
        include_str!("../../../../../../../crates/koru-window/src/snap.rs"),
    ),
    (
        "crates/koru-window/src/system_locale.rs",
        include_str!("../../../../../../../crates/koru-window/src/system_locale.rs"),
    ),
    (
        "crates/koru-window/src/system_theme.rs",
        include_str!("../../../../../../../crates/koru-window/src/system_theme.rs"),
    ),
    (
        "crates/koru-window/src/system_version.rs",
        include_str!("../../../../../../../crates/koru-window/src/system_version.rs"),
    ),
    (
        "crates/koru-window/src/traffic_lights.rs",
        include_str!("../../../../../../../crates/koru-window/src/traffic_lights.rs"),
    ),
    (
        "crates/koru-window/src/window_geometry.rs",
        include_str!("../../../../../../../crates/koru-window/src/window_geometry.rs"),
    ),
];

/// One domain crate's root: its records declared in `lib.rs` itself, and
/// each module `lib.rs` declares, in order.
pub(super) struct Crate {
    pub(super) owner: &'static str,
    pub(super) lib: &'static str,
    pub(super) root: &'static [&'static str],
    pub(super) modules: &'static [(&'static str, Module)],
}

/// The baselines, a crate at a time.
pub(super) const CRATES: &[Crate] = &[
    Crate {
        owner: "koru-ai-ui",
        lib: "crates/koru-ai-ui/src/lib.rs",
        root: &[
            "C-AI-CHAT-SURFACE",
            "C-AI-COMPOSER",
            "C-AI-CONVERSATION",
            "C-AI-PROVIDER-SETUP",
        ],
        modules: &[
            (
                "agents",
                Module::Records(&[
                    "H-AI-AGENT-PROVIDERS",
                    "C-AI-CHAT-SURFACE",
                    "C-AI-PROVIDER-SETUP",
                ]),
            ),
            (
                "approval_modes",
                Module::Records(&["C-AI-CHAT-SURFACE", "H-AI-AGENT-PROVIDERS"]),
            ),
            (
                "attachments",
                Module::Records(&["C-AI-ATTACHMENT-STRIP", "H-AI-VIEWER-LIFT"]),
            ),
            (
                "chat_status",
                Module::Records(&["H-AI-CHAT-STATUS", "C-AI-CHAT-SURFACE"]),
            ),
            (
                "mini_apps",
                Module::Excluded(
                    "orchestration: live-only delivery of mini-app declarations and refinements from the chat's stream to the native host (MiniAppCreation, MiniAppRefinement and the ChatSurface methods that hand them over); draws nothing but status-line words, and the host, not this crate, builds the mini app",
                ),
            ),
            (
                "chats",
                Module::Excluded(
                    "orchestration: what the surface tells its host about the Chats list (ChatElsewhere, set_profile, take_chat_list_changed, rename_chat, pin_chat, take_wants_chat_step, listed_chats); the list itself is koru-chats-ui's; draws nothing",
                ),
            ),
            ("composer", Module::Records(&["C-AI-COMPOSER-DOCUMENT"])),
            ("composer_band", Module::Records(&["C-AI-COMPOSER"])),
            (
                "composer_plus",
                Module::Records(&["C-AI-COMPOSER-PLUS-MENU"]),
            ),
            ("composer_shell", Module::Records(&["C-AI-COMPOSER"])),
            ("context_window", Module::Records(&["C-AI-CHAT-SURFACE"])),
            ("diagrams", Module::Records(&["H-AI-CHAT-DIAGRAMS"])),
            ("dictation", Module::Records(&["C-AI-DICTATION-ROW"])),
            ("doc_links", Module::Records(&["C-AI-CONVERSATION"])),
            ("effort", Module::Records(&["C-AI-MODEL-PICKER"])),
            ("file_citations", Module::Records(&["C-AI-CONVERSATION"])),
            ("footer", Module::Records(&["C-AI-CHAT-SURFACE"])),
            ("layout_cache", Module::Records(&["C-AI-CONVERSATION"])),
            ("link_card", Module::Records(&["C-AI-CHAT-SURFACE"])),
            ("live_markdown", Module::Records(&["C-AI-CONVERSATION"])),
            ("microphone", Module::Records(&["C-AI-MICROPHONE-PANEL"])),
            ("model_picker", Module::Records(&["C-AI-MODEL-PICKER"])),
            ("new_divider", Module::Records(&["C-AI-NEW-DIVIDER"])),
            ("privacy", Module::Records(&["C-AI-CHAT-SURFACE"])),
            ("questions", Module::Records(&["C-AI-CHAT-SURFACE"])),
            ("queue", Module::Records(&["C-AI-CHAT-SURFACE"])),
            ("quote_pills", Module::Records(&["C-AI-CHAT-SURFACE"])),
            (
                "reader",
                Module::Excluded(
                    "io: the bounded background client thread that talks to the AI service (boot, send, stream, export, settings, agent catalogs); every service call crosses it",
                ),
            ),
            ("restored_scroll", Module::Records(&["C-AI-CONVERSATION"])),
            ("skills", Module::Records(&["C-AI-CHAT-SURFACE"])),
            ("sources", Module::Records(&["C-AI-CONVERSATION"])),
            ("tool_groups", Module::Records(&["C-AI-CHAT-SURFACE"])),
            ("tool_output", Module::Records(&["C-AI-TOOL-CALL-DETAIL"])),
            ("usage", Module::Records(&["C-AI-CONVERSATION"])),
            ("work_view", Module::Records(&["C-AI-CHAT-SURFACE"])),
        ],
    },
    Crate {
        owner: "koru-agents-ui",
        lib: "crates/koru-agents-ui/src/lib.rs",
        root: &[],
        modules: &[
            ("interaction", Module::Records(&["C-SESSION-SURFACE"])),
            ("surface", Module::Records(&["C-SESSION-SURFACE"])),
            ("watching", Module::Records(&["C-WATCH-BAND"])),
        ],
    },
    Crate {
        owner: "koru-appearance-ui",
        lib: "crates/koru-appearance-ui/src/lib.rs",
        root: &["C-EMOJI-COLOR-PANEL"],
        modules: &[],
    },
    Crate {
        owner: "koru-artifacts-ui",
        lib: "crates/koru-artifacts-ui/src/lib.rs",
        root: &["C-ARTIFACTS-COLUMN"],
        modules: &[
            ("draw", Module::Records(&["C-ARTIFACTS-COLUMN"])),
            (
                "gateway",
                Module::Excluded(
                    "io: the gateway client the column's worker threads share, with its rediscovery and backoff",
                ),
            ),
            ("input", Module::Records(&["C-ARTIFACTS-COLUMN"])),
            ("model", Module::Records(&["C-ARTIFACTS-COLUMN"])),
            (
                "reader",
                Module::Excluded("io: the read and register threads, off the window thread"),
            ),
            (
                "result",
                Module::Excluded(
                    "io: read_result and ResultBody, a blocking gateway read a host calls from its own worker; draws nothing",
                ),
            ),
            ("rows", Module::Records(&["C-ARTIFACTS-COLUMN"])),
            ("tab", Module::Records(&["C-ARTIFACT-TAB"])),
            ("text", Module::Records(&["C-ARTIFACTS-COLUMN"])),
        ],
    },
    Crate {
        owner: "koru-canvas-ui",
        lib: "crates/koru-canvas-ui/src/lib.rs",
        root: &["C-CANVAS-SURFACE"],
        modules: &[
            ("arrow", Module::Records(&["C-CANVAS-SURFACE"])),
            ("camera", Module::Records(&["H-CANVAS-CAMERA"])),
            ("draw", Module::Records(&["C-CANVAS-SURFACE"])),
            ("input", Module::Records(&["C-CANVAS-SURFACE"])),
            ("layout", Module::Records(&["C-CANVAS-SURFACE"])),
            ("menu", Module::Records(&["C-CANVAS-SURFACE"])),
            (
                "model",
                Module::Records(&["C-CANVAS-SURFACE", "C-CANVAS-OBJECTS"]),
            ),
            ("order", Module::Records(&["C-CANVAS-SURFACE"])),
            ("words", Module::Records(&["C-CANVAS-SURFACE"])),
            (
                "dependency",
                Module::Excluded("test-only: the dependency fence, compiled only under cfg(test)"),
            ),
        ],
    },
    Crate {
        owner: "koru-chats-ui",
        lib: "crates/koru-chats-ui/src/lib.rs",
        root: &["C-CHATS-LIST"],
        modules: &[
            ("draw", Module::Records(&["C-CHATS-LIST"])),
            (
                "gateway",
                Module::Excluded(
                    "io: the gateway client the list's reader threads share, with its rediscovery and backoff",
                ),
            ),
            ("input", Module::Records(&["C-CHATS-LIST"])),
            (
                "reader",
                Module::Excluded(
                    "io: the two threads that list and delete chats off the window thread",
                ),
            ),
            ("rows", Module::Records(&["C-CHATS-LIST"])),
            (
                "seen",
                Module::Excluded(
                    "data: the per-device book of what the person has seen of each chat, parsed from and written to one host-stored line; draws nothing",
                ),
            ),
            ("status", Module::Records(&["H-CHAT-STATUS"])),
            ("text", Module::Records(&["C-CHATS-LIST"])),
        ],
    },
    Crate {
        owner: "koru-runs-ui",
        lib: "crates/koru-runs-ui/src/lib.rs",
        root: &["C-RUNS-SURFACE"],
        modules: &[
            ("action", Module::Records(&["C-RUNS-SURFACE"])),
            ("column", Module::Records(&["C-RUNS-SURFACE"])),
            (
                "dependency",
                Module::Excluded(
                    "private helper: the crate's dependency fence; its only items are two #[test] functions that read Cargo.toml and run cargo tree, nothing callable",
                ),
            ),
            (
                "index",
                Module::Excluded(
                    "io: RunIndex lists a workspace's runs over GET /v1/runs and performs trash/restore/purge on its own koru-run-index thread; RunRow, TrashedRunRow, Motion, motion, DeletionVerb and DeletionAnswer are the data rows and answers a host navigator draws from. Its own docs say it draws nothing and lays nothing out",
                ),
            ),
            (
                "links",
                Module::Excluded(
                    "io: RunLinks, Command, Answer, Scripted and RUN_SCOPES/WINDOW/MAX_HELD are the run views' gateway worker (koru-run-view thread) and its channels; RunLinks::inert and RunLinks::scripted are the service-free seams a story would hand RunsSurface::with_links",
                ),
            ),
            ("response", Module::Records(&["C-RUNS-SURFACE"])),
            ("state", Module::Records(&["C-RUNS-SURFACE"])),
            ("summary", Module::Records(&["H-RUN-SUMMARY"])),
            ("views", Module::Records(&["C-RUNS-SURFACE"])),
            (
                "words",
                Module::Records(&["C-RUNS-SURFACE", "H-RUN-SUMMARY"]),
            ),
        ],
    },
    Crate {
        owner: "koru-search-ui",
        lib: "crates/koru-search-ui/src/lib.rs",
        root: &["C-SEARCH-SURFACE"],
        modules: &[
            (
                "catalog",
                Module::Excluded(
                    "data: the workspace and profile catalog the surface reads from the store (GET /v1/workspaces and /v1/profiles) to scope queries by profile and lifecycle; every item is pub(super)",
                ),
            ),
            ("due_picker", Module::Records(&["C-SEARCH-SURFACE"])),
            ("entity_search", Module::Records(&["H-ENTITY-SEARCH"])),
            (
                "filters",
                Module::Excluded(
                    "private helper: the /key attribute-filter grammar the query box parses and splits into wire filters; every item is pub(crate), no painter",
                ),
            ),
            ("modal", Module::Records(&["C-SEARCH-MODAL"])),
            (
                "reader",
                Module::Excluded(
                    "io: the two background reader threads that dial the store, coalesce keystrokes and answer by seq; every item is pub(super)",
                ),
            ),
            ("result_layout", Module::Records(&["C-SEARCH-SURFACE"])),
            ("scope", Module::Records(&["C-SEARCH-SURFACE"])),
            (
                "state",
                Module::Excluded(
                    "io: process-local search session state and the persisted profile and sort file; its one public item, persist_choices_in (re-exported at the root, lib.rs:49), is startup persistence plumbing a host calls once before the first surface: apps/koru/src/main.rs:906 `koru_search_ui::persist_choices_in(koru_window::search_state_path());` and apps/koru-agent/src/app.rs:962; the UI Lab's own startup test (apps/koru/src/ui_lab/tests.rs:79) requires the Lab to be dispatched before it",
                ),
            ),
            (
                "store",
                Module::Excluded(
                    "io: the store client handle the reader threads share, with re-discovery on failure; every item is pub(super)",
                ),
            ),
            (
                "debug",
                Module::Excluded(
                    "io: KORU_SEARCH_DEBUG routing diagnostics to stderr (enabled, write) behind the exported search_debug! macro",
                ),
            ),
        ],
    },
    Crate {
        owner: "koru-settings-ui",
        lib: "crates/koru-settings-ui/src/lib.rs",
        root: &["C-SETTINGS-SURFACE"],
        modules: &[
            ("action", Module::Records(&["C-SETTINGS-SURFACE"])),
            (
                "links",
                Module::Excluded(
                    "io: the surface's own use of the typed settings client (SettingsLinks: Gateway discovery through koru_api_client::SectionLink, per-field patches, outstanding writes, the overdue inference); a host builds one and hands it to SettingsSurface::with_links",
                ),
            ),
            ("page", Module::Records(&["C-SETTINGS-SURFACE"])),
            ("state", Module::Records(&["C-SETTINGS-SURFACE"])),
            ("agents", Module::Records(&["C-SETTINGS-SURFACE"])),
            ("global", Module::Records(&["C-SETTINGS-SURFACE"])),
            ("terminal", Module::Records(&["C-SETTINGS-SURFACE"])),
            (
                "dependency",
                Module::Excluded("test-only: the dependency fence, compiled only under cfg(test)"),
            ),
        ],
    },
    Crate {
        owner: "koru-shell-chrome",
        lib: "crates/koru-shell-chrome/src/lib.rs",
        root: &[],
        modules: &[
            ("banner", Module::Records(&["H-BANNER-SLOTS"])),
            ("branch", Module::Records(&["C-BRANCH-CHIP"])),
            ("caption", Module::Records(&["H-CAPTION-LAYOUT"])),
            ("lane", Module::Records(&["H-LANE-LAYOUT"])),
            ("regions", Module::Records(&["H-WINDOW-REGIONS"])),
            ("reorder", Module::Records(&["H-DRAG-REORDER"])),
            ("status_chip", Module::Records(&["C-STATUS-CHIP"])),
            ("status_mark", Module::Records(&["C-STATUS-MARK"])),
            ("tabs", Module::Records(&["C-TAB-STRIP"])),
            ("title_icon", Module::Records(&["P-PAGE-EMOJI"])),
        ],
    },
    Crate {
        owner: "koru-socials-ui",
        lib: "crates/koru-socials-ui/src/lib.rs",
        root: &["C-SOCIALS-SURFACE"],
        modules: &[
            (
                "backend",
                Module::Excluded(
                    "io: the koru-socials-store worker thread (Backend, Request, Reply, Listing, Failure) that connects to the Store through koru_api_client, watches the workspace, loads/saves posts and channels, imports documents and runs the account-connection flow; public and #[doc(hidden)] only so examples/store-smoke.rs can drive it",
                ),
            ),
            ("navigation", Module::Records(&["C-SOCIALS-NAVIGATOR"])),
            ("paint", Module::Records(&["C-SOCIALS-SURFACE"])),
            (
                "preview",
                Module::Excluded(
                    "private helper: synthetic visual fixtures (fixture(view) -> Surface), compiled only with the offline no-reader feature and re-exported as preview_fixture for examples/preview.rs",
                ),
            ),
        ],
    },
    Crate {
        owner: "koru-voice-ui",
        lib: "crates/koru-voice-ui/src/lib.rs",
        root: &["C-VOICE-SURFACE"],
        modules: &[("hud", Module::Records(&["C-VOICE-HUD"]))],
    },
    Crate {
        owner: "koru-workspaces-ui",
        lib: "crates/koru-workspaces-ui/src/lib.rs",
        root: &[],
        modules: &[
            (
                "action",
                Module::Excluded(
                    "orchestration: the controller's intent and host-request vocabularies; the drawing that speaks them is the app's Manage tab",
                ),
            ),
            (
                "controller",
                Module::Excluded(
                    "orchestration: WorkspacesController, the workspace-management state machine, which draws nothing",
                ),
            ),
            (
                "links",
                Module::Excluded(
                    "io: the one thread that talks to the gateway, and its fixtures feature",
                ),
            ),
            (
                "pending",
                Module::Excluded("orchestration: command correlation ids"),
            ),
            (
                "state",
                Module::Excluded(
                    "orchestration: the rows and states the controller projects for a drawing",
                ),
            ),
            (
                "dependency",
                Module::Excluded("test-only: the dependency fence, compiled only under cfg(test)"),
            ),
        ],
    },
    Crate {
        owner: "koru-editor",
        lib: "crates/koru-editor/src/lib.rs",
        root: &[],
        modules: &[
            (
                "accessibility",
                Module::Records(&["H-EDITOR-ACCESSIBILITY"]),
            ),
            ("body", Module::Records(&["C-DOCUMENT-EDITOR"])),
            (
                "browser",
                Module::Excluded(
                    "io: open and open_without_activating hand a URL to the system browser (rundll32, open, xdg-open) as a child process; draws nothing",
                ),
            ),
            ("chips", Module::Records(&["C-ROW-ATTRIBUTE-CHIPS"])),
            ("clipboard", Module::Records(&["H-EDITOR-CLIPBOARD"])),
            (
                "components",
                Module::Records(&[
                    "C-EDITOR-OVERLAY",
                    "C-COMMAND-MENU",
                    "C-ENTITY-PICKER",
                    "C-DATE-PICKER",
                    "C-EMOJI-LIST",
                    "C-LANGUAGE-PICKER",
                    "C-LINK-PROMPT",
                    "C-LIST-PICKER",
                    "C-NAME-PROMPT",
                    "C-PRIORITY-PICKER",
                    "C-TAG-PICKER",
                    "C-ACCENT-COLOR-PICKER",
                    "C-ATTR-MENU",
                ]),
            ),
            ("diagram", Module::Records(&["C-DOCUMENT-DIAGRAM"])),
            ("dictation", Module::Records(&["H-EDITOR-DICTATION"])),
            (
                "draw",
                Module::Records(&[
                    "C-DOCUMENT-EDITOR",
                    "H-EDITOR-EMBEDDED",
                    "C-DOCUMENT-TEXT",
                    "C-DOCUMENT-IMAGE",
                    "C-ROW-GUTTER",
                    "C-LINK-CARD",
                ]),
            ),
            ("editor", Module::Records(&["C-DOCUMENT-EDITOR"])),
            ("input", Module::Records(&["H-EDITOR-KEYMAP"])),
            ("objective", Module::Records(&["H-EDITOR-TASKS"])),
            ("paragraph", Module::Records(&["C-DOCUMENT-TEXT"])),
            ("pending_image", Module::Records(&["C-DOCUMENT-IMAGE"])),
            ("pillinfo", Module::Records(&["H-PILL-INFO"])),
            ("policy", Module::Records(&["H-EDITOR-POLICY"])),
            ("reminders", Module::Records(&["H-EDITOR-REMINDERS"])),
            ("rowlayout", Module::Records(&["C-DOCUMENT-TEXT"])),
            ("table", Module::Records(&["C-DOCUMENT-TABLE"])),
            ("tasks", Module::Records(&["H-EDITOR-TASKS"])),
            ("trigger", Module::Records(&["H-EDITOR-TRIGGERS"])),
            ("utils", Module::Records(&["H-EDITOR-DATES"])),
            (
                "view_state",
                Module::Records(&[
                    "C-DOCUMENT-EDITOR",
                    "H-EDITOR-EMBEDDED",
                    "H-EDITOR-POLICY",
                    "C-DOCUMENT-IMAGE",
                    "C-LINK-CARD",
                ]),
            ),
            ("emoji", Module::Records(&["H-EMOJI-CATALOG"])),
        ],
    },
    Crate {
        owner: "koru-markdown-view",
        lib: "crates/koru-markdown-view/src/lib.rs",
        root: &["C-MARKDOWN-VIEW", "C-MARKDOWN-DOCUMENT"],
        modules: &[
            ("document", Module::Records(&["C-MARKDOWN-DOCUMENT"])),
            ("text", Module::Records(&["C-MARKDOWN-VIEW"])),
            ("view", Module::Records(&["C-MARKDOWN-VIEW"])),
        ],
    },
    Crate {
        owner: "koru-session-view",
        lib: "crates/koru-session-view/src/lib.rs",
        root: &[],
        modules: &[
            ("cursor", Module::Records(&["H-TERMINAL-STYLE"])),
            ("geometry", Module::Records(&["C-TERMINAL-VIEW"])),
            ("history", Module::Records(&["C-TERMINAL-VIEW"])),
            ("input", Module::Records(&["C-TERMINAL-VIEW"])),
            ("interaction", Module::Records(&["C-TERMINAL-VIEW"])),
            (
                "managed",
                Module::Records(&["C-MANAGED-SESSION-VIEW", "C-MANAGED-TRANSCRIPT"]),
            ),
            ("model", Module::Records(&["C-TERMINAL-CONTENT"])),
            ("pending_input", Module::Records(&["C-PENDING-INPUT-VIEW"])),
            (
                "render",
                Module::Records(&["C-TERMINAL-VIEW", "C-TERMINAL-CONTENT"]),
            ),
            ("row", Module::Records(&["C-TERMINAL-VIEW"])),
            ("selection", Module::Records(&["C-TERMINAL-VIEW"])),
            ("style", Module::Records(&["H-TERMINAL-STYLE"])),
            ("surface", Module::Records(&["H-SESSION-INTERACTION"])),
        ],
    },
    Crate {
        owner: "koru-app-shell",
        lib: "crates/koru-app-shell/src/lib.rs",
        root: &[],
        modules: &[
            ("banner_state", Module::Records(&["H-SHELL-BANNER-STATE"])),
            ("chrome", Module::Records(&["C-SHELL-CAPTION"])),
            ("command", Module::Records(&["H-SHELL-HOST"])),
            ("element", Module::Records(&["H-SHELL-TABS"])),
            (
                "host",
                Module::Records(&["H-SHELL-HOST", "C-SHELL-CAPTION"]),
            ),
            ("keys", Module::Records(&["H-SHELL-HOST"])),
            ("panel", Module::Records(&["H-SHELL-PANEL"])),
            ("popover", Module::Records(&["C-SHELL-POPOVER"])),
            ("state", Module::Records(&["H-SHELL-HOST"])),
            ("surface", Module::Records(&["H-SHELL-SURFACE"])),
            ("tabset", Module::Records(&["H-SHELL-TABS"])),
            ("tabslide", Module::Records(&["H-SHELL-TABS"])),
            ("tabstrip", Module::Records(&["H-SHELL-TABS"])),
            ("zoom_label", Module::Records(&["C-SHELL-ZOOM-LABEL"])),
        ],
    },
    Crate {
        owner: "koru-webview",
        lib: "crates/koru-webview/src/lib.rs",
        root: &["C-WEB-SURFACE"],
        modules: &[
            ("debug", Module::Records(&["C-WEB-SURFACE"])),
            // Declared twice: the WebView2/WKWebView engine, and `stub.rs`
            // under `no-webview` and wherever wry is not linked (Linux).
            ("engine", Module::Records(&["C-WEB-SURFACE"])),
            ("engine", Module::Records(&["C-WEB-SURFACE"])),
            ("js", Module::Records(&["C-WEB-SURFACE"])),
        ],
    },
    Crate {
        owner: "koru-window",
        lib: "crates/koru-window/src/lib.rs",
        root: &[],
        modules: &[
            (
                "accessibility",
                Module::Records(&["H-WINDOW-ACCESSIBILITY"]),
            ),
            (
                "activation",
                Module::Excluded(
                    "io: brings the application to the front, and on macOS makes it an accessory app; draws nothing",
                ),
            ),
            ("app", Module::Records(&["H-WINDOW-RUNNERS"])),
            ("appearance", Module::Records(&["H-SYSTEM-APPEARANCE"])),
            (
                "console",
                Module::Excluded("io: attaches a GUI-subsystem binary to its parent console"),
            ),
            (
                "desktop_preferences",
                Module::Excluded(
                    "data: one read of desktop-v1.json's remaining facts (close behavior, global-search height, reminder startup); draws nothing",
                ),
            ),
            ("dispatch", Module::Records(&["H-WINDOW-RUNNERS"])),
            (
                "dock",
                Module::Excluded("io: sets the macOS Dock icon winit ignores"),
            ),
            ("frame", Module::Records(&["C-WINDOW-FRAME"])),
            ("framestats", Module::Records(&["H-WINDOW-RENDERER"])),
            ("gpu", Module::Records(&["H-WINDOW-RUNNERS"])),
            ("hairline", Module::Records(&["C-WINDOW-FRAME"])),
            (
                "input",
                Module::Records(&["H-WINDOW-INPUT", "C-IME-COMPOSITION"]),
            ),
            ("key_quarantine", Module::Records(&["H-WINDOW-INPUT"])),
            ("multi", Module::Records(&["H-WINDOW-RUNNERS"])),
            ("non_activating", Module::Records(&["H-WINDOW-RUNNERS"])),
            ("offscreen", Module::Records(&["H-WINDOW-RENDERER"])),
            (
                "open_bytes",
                Module::Excluded(
                    "io: writes a temporary copy of bytes Koru holds and asks the OS to open it",
                ),
            ),
            (
                "open_uri",
                Module::Excluded("io: hands a URI to the desktop to open"),
            ),
            ("renderer", Module::Records(&["H-WINDOW-RENDERER"])),
            ("request", Module::Records(&["H-WINDOW-RUNNERS"])),
            ("resize", Module::Records(&["C-WINDOW-FRAME"])),
            ("reveal", Module::Records(&["H-WINDOW-RUNNERS"])),
            ("rounded_region", Module::Records(&["C-WINDOW-FRAME"])),
            (
                "search_geometry",
                Module::Excluded(
                    "data: the global-search row's value schema in the shared window-geometry file",
                ),
            ),
            ("snap", Module::Records(&["C-WINDOW-FRAME"])),
            (
                "system_locale",
                Module::Excluded("io: reads the machine's language before any window exists"),
            ),
            ("system_theme", Module::Records(&["H-SYSTEM-APPEARANCE"])),
            (
                "system_version",
                Module::Excluded("io: reads the macOS version; compiled only on macOS"),
            ),
            ("traffic_lights", Module::Records(&["C-WINDOW-FRAME"])),
            (
                "window_geometry",
                Module::Excluded(
                    "io: machine-local paths and the shared window-geometry row store (read, merge, write); the Lab must never read or write it",
                ),
            ),
        ],
    },
];
