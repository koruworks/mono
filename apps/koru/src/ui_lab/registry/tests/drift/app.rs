// SPDX-License-Identifier: MPL-2.0

//! **The apps, held to their source**: every non-test file of the app
//! crates that draw (`apps/koru`, without the Lab itself; `apps/koru-document`;
//! `apps/koru-agent`) compiled in as text, and the module lists of the app
//! files that declare children, module by module. A module added to or
//! removed from one of those files fails until it is classified here — a
//! record of its own, folded into another record, or excluded for a stated
//! reason. What an app adds inside a module it already has, and a painter
//! that registers no identity, are the manifest's hand review, not this.
//!
//! App items are mostly `pub(crate)` or private, so the checks here read a
//! declaration whatever its visibility; the crates' checks beside this file
//! still require `pub`.

use super::*;

/// Every non-test source file of the app crates that draw, from the
/// repository root: what the symbol, option and identity checks read. The
/// test-only modules (`#[cfg(test)]` under a name that does not say so) and
/// `mainview/shell_conformance.rs`, which no `mod` line declares, are left
/// out; the manifest lists them.
pub(super) const SOURCES: &[(&str, &str)] = &[
    (
        "apps/koru/src/ai_chat_view.rs",
        include_str!("../../../../../../../apps/koru/src/ai_chat_view.rs"),
    ),
    (
        "apps/koru/src/canvas_view.rs",
        include_str!("../../../../../../../apps/koru/src/canvas_view.rs"),
    ),
    (
        "apps/koru/src/canvas_view/accessibility.rs",
        include_str!("../../../../../../../apps/koru/src/canvas_view/accessibility.rs"),
    ),
    (
        "apps/koru/src/canvas_view/agents.rs",
        include_str!("../../../../../../../apps/koru/src/canvas_view/agents.rs"),
    ),
    (
        "apps/koru/src/canvas_view/decor.rs",
        include_str!("../../../../../../../apps/koru/src/canvas_view/decor.rs"),
    ),
    (
        "apps/koru/src/canvas_view/dialog.rs",
        include_str!("../../../../../../../apps/koru/src/canvas_view/dialog.rs"),
    ),
    (
        "apps/koru/src/canvas_view/perform.rs",
        include_str!("../../../../../../../apps/koru/src/canvas_view/perform.rs"),
    ),
    (
        "apps/koru/src/diff/mod.rs",
        include_str!("../../../../../../../apps/koru/src/diff/mod.rs"),
    ),
    (
        "apps/koru/src/feedback.rs",
        include_str!("../../../../../../../apps/koru/src/feedback.rs"),
    ),
    (
        "apps/koru/src/feedback/draft.rs",
        include_str!("../../../../../../../apps/koru/src/feedback/draft.rs"),
    ),
    (
        "apps/koru/src/feedback/logs.rs",
        include_str!("../../../../../../../apps/koru/src/feedback/logs.rs"),
    ),
    (
        "apps/koru/src/feedback/outbox.rs",
        include_str!("../../../../../../../apps/koru/src/feedback/outbox.rs"),
    ),
    (
        "apps/koru/src/feedback/submit.rs",
        include_str!("../../../../../../../apps/koru/src/feedback/submit.rs"),
    ),
    (
        "apps/koru/src/leaf.rs",
        include_str!("../../../../../../../apps/koru/src/leaf.rs"),
    ),
    (
        "apps/koru/src/mainview/accessibility.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/accessibility.rs"),
    ),
    (
        "apps/koru/src/mainview/accessibility/chrome.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/accessibility/chrome.rs"),
    ),
    (
        "apps/koru/src/mainview/accessibility/commands.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/accessibility/commands.rs"),
    ),
    (
        "apps/koru/src/mainview/accessibility/document.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/accessibility/document.rs"),
    ),
    (
        "apps/koru/src/mainview/accessibility/launcher.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/accessibility/launcher.rs"),
    ),
    (
        "apps/koru/src/mainview/accessibility/menus.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/accessibility/menus.rs"),
    ),
    (
        "apps/koru/src/mainview/accessibility/persistence.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/accessibility/persistence.rs"),
    ),
    (
        "apps/koru/src/mainview/accessibility/place_chooser.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/accessibility/place_chooser.rs"),
    ),
    (
        "apps/koru/src/mainview/accessibility/reminders.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/accessibility/reminders.rs"),
    ),
    (
        "apps/koru/src/mainview/accessibility/viewer.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/accessibility/viewer.rs"),
    ),
    (
        "apps/koru/src/mainview/agent_cli_updates.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/agent_cli_updates.rs"),
    ),
    (
        "apps/koru/src/mainview/agent_install.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/agent_install.rs"),
    ),
    (
        "apps/koru/src/mainview/agent_mcp.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/agent_mcp.rs"),
    ),
    (
        "apps/koru/src/mainview/agent_providers.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/agent_providers.rs"),
    ),
    (
        "apps/koru/src/mainview/canvas.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/canvas.rs"),
    ),
    (
        "apps/koru/src/mainview/canvas/wire.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/canvas/wire.rs"),
    ),
    (
        "apps/koru/src/mainview/canvas_chain.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/canvas_chain.rs"),
    ),
    (
        "apps/koru/src/mainview/canvas_runs.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/canvas_runs.rs"),
    ),
    (
        "apps/koru/src/mainview/chat_status.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/chat_status.rs"),
    ),
    (
        "apps/koru/src/mainview/chrome_debug.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/chrome_debug.rs"),
    ),
    (
        "apps/koru/src/mainview/chrome_focus.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/chrome_focus.rs"),
    ),
    (
        "apps/koru/src/mainview/closed_tabs.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/closed_tabs.rs"),
    ),
    (
        "apps/koru/src/mainview/cloud_sync.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/cloud_sync.rs"),
    ),
    (
        "apps/koru/src/mainview/context_menu.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/context_menu.rs"),
    ),
    (
        "apps/koru/src/mainview/delegate.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/delegate.rs"),
    ),
    (
        "apps/koru/src/mainview/delegate/job.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/delegate/job.rs"),
    ),
    (
        "apps/koru/src/mainview/delegate/modal.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/delegate/modal.rs"),
    ),
    (
        "apps/koru/src/mainview/delegate/modal/models.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/delegate/modal/models.rs"),
    ),
    (
        "apps/koru/src/mainview/delegate/modal/readiness.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/delegate/modal/readiness.rs"),
    ),
    (
        "apps/koru/src/mainview/delegate/modal/repeat.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/delegate/modal/repeat.rs"),
    ),
    (
        "apps/koru/src/mainview/delegate/paint.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/delegate/paint.rs"),
    ),
    (
        "apps/koru/src/mainview/delegate/pills.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/delegate/pills.rs"),
    ),
    (
        "apps/koru/src/mainview/delegate/world.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/delegate/world.rs"),
    ),
    (
        "apps/koru/src/mainview/dictation.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/dictation.rs"),
    ),
    (
        "apps/koru/src/mainview/dictation/chat.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/dictation/chat.rs"),
    ),
    (
        "apps/koru/src/mainview/document_appearance.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/document_appearance.rs"),
    ),
    (
        "apps/koru/src/mainview/draw.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/draw.rs"),
    ),
    (
        "apps/koru/src/mainview/error_dialog.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/error_dialog.rs"),
    ),
    (
        "apps/koru/src/mainview/file_preview.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/file_preview.rs"),
    ),
    (
        "apps/koru/src/mainview/font_picker.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/font_picker.rs"),
    ),
    (
        "apps/koru/src/mainview/help_menu.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/help_menu.rs"),
    ),
    (
        "apps/koru/src/mainview/history.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/history.rs"),
    ),
    (
        "apps/koru/src/mainview/icon_action.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/icon_action.rs"),
    ),
    (
        "apps/koru/src/mainview/import_dialog.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/import_dialog.rs"),
    ),
    (
        "apps/koru/src/mainview/keys.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/keys.rs"),
    ),
    (
        "apps/koru/src/mainview/launcher.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/launcher.rs"),
    ),
    (
        "apps/koru/src/mainview/layout.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/layout.rs"),
    ),
    (
        "apps/koru/src/mainview/link_device.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/link_device.rs"),
    ),
    (
        "apps/koru/src/mainview/linked_devices.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/linked_devices.rs"),
    ),
    (
        "apps/koru/src/mainview/mini_app.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/mini_app.rs"),
    ),
    (
        "apps/koru/src/mainview/mini_app_artifacts.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/mini_app_artifacts.rs"),
    ),
    (
        "apps/koru/src/mainview/mini_app_pins.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/mini_app_pins.rs"),
    ),
    (
        "apps/koru/src/mainview/mod.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/mod.rs"),
    ),
    (
        "apps/koru/src/mainview/move_to.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/move_to.rs"),
    ),
    (
        "apps/koru/src/mainview/nav.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/nav.rs"),
    ),
    (
        "apps/koru/src/mainview/nav_debug.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/nav_debug.rs"),
    ),
    (
        "apps/koru/src/mainview/new_document.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/new_document.rs"),
    ),
    (
        "apps/koru/src/mainview/new_list.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/new_list.rs"),
    ),
    (
        "apps/koru/src/mainview/open.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/open.rs"),
    ),
    (
        "apps/koru/src/mainview/panes.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/panes.rs"),
    ),
    (
        "apps/koru/src/mainview/place_chooser.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/place_chooser.rs"),
    ),
    (
        "apps/koru/src/mainview/profile_probe.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/profile_probe.rs"),
    ),
    (
        "apps/koru/src/mainview/profile_wait.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/profile_wait.rs"),
    ),
    (
        "apps/koru/src/mainview/rail_overflow.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/rail_overflow.rs"),
    ),
    (
        "apps/koru/src/mainview/recording.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/recording.rs"),
    ),
    (
        "apps/koru/src/mainview/recording/modal.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/recording/modal.rs"),
    ),
    (
        "apps/koru/src/mainview/recording/paint.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/recording/paint.rs"),
    ),
    (
        "apps/koru/src/mainview/recording/telemetry.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/recording/telemetry.rs"),
    ),
    (
        "apps/koru/src/mainview/reminder_creation.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/reminder_creation.rs"),
    ),
    (
        "apps/koru/src/mainview/reminder_management.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/reminder_management.rs"),
    ),
    (
        "apps/koru/src/mainview/reminder_shell.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/reminder_shell.rs"),
    ),
    (
        "apps/koru/src/mainview/reminders.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/reminders.rs"),
    ),
    (
        "apps/koru/src/mainview/reminders_view.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/reminders_view.rs"),
    ),
    (
        "apps/koru/src/mainview/routing.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/routing.rs"),
    ),
    (
        "apps/koru/src/mainview/run_trash.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/run_trash.rs"),
    ),
    (
        "apps/koru/src/mainview/runs.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/runs.rs"),
    ),
    (
        "apps/koru/src/mainview/schedules.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/schedules.rs"),
    ),
    (
        "apps/koru/src/mainview/session.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/session.rs"),
    ),
    (
        "apps/koru/src/mainview/session_backend.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/session_backend.rs"),
    ),
    (
        "apps/koru/src/mainview/session_lanes.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/session_lanes.rs"),
    ),
    (
        "apps/koru/src/mainview/snapshot_prompt.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/snapshot_prompt.rs"),
    ),
    (
        "apps/koru/src/mainview/tab.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/tab.rs"),
    ),
    (
        "apps/koru/src/mainview/tab_rules.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/tab_rules.rs"),
    ),
    (
        "apps/koru/src/mainview/taskcounts.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/taskcounts.rs"),
    ),
    (
        "apps/koru/src/mainview/tasks.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/tasks.rs"),
    ),
    (
        "apps/koru/src/mainview/terminal_look.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/terminal_look.rs"),
    ),
    (
        "apps/koru/src/mainview/trash.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/trash.rs"),
    ),
    (
        "apps/koru/src/mainview/update_ui.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/update_ui.rs"),
    ),
    (
        "apps/koru/src/mainview/welcome_progress.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/welcome_progress.rs"),
    ),
    (
        "apps/koru/src/mainview/workspace_controller.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/workspace_controller.rs"),
    ),
    (
        "apps/koru/src/mainview/workspace_icon.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/workspace_icon.rs"),
    ),
    (
        "apps/koru/src/mainview/workspace_menu.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/workspace_menu.rs"),
    ),
    (
        "apps/koru/src/mainview/workspace_rename.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/workspace_rename.rs"),
    ),
    (
        "apps/koru/src/mainview/zoomlabel.rs",
        include_str!("../../../../../../../apps/koru/src/mainview/zoomlabel.rs"),
    ),
    (
        "apps/koru/src/manage.rs",
        include_str!("../../../../../../../apps/koru/src/manage.rs"),
    ),
    (
        "apps/koru/src/manage/accessibility.rs",
        include_str!("../../../../../../../apps/koru/src/manage/accessibility.rs"),
    ),
    (
        "apps/koru/src/manage/cloud.rs",
        include_str!("../../../../../../../apps/koru/src/manage/cloud.rs"),
    ),
    (
        "apps/koru/src/manage/cloud/copy.rs",
        include_str!("../../../../../../../apps/koru/src/manage/cloud/copy.rs"),
    ),
    (
        "apps/koru/src/manage/cloud/inventory.rs",
        include_str!("../../../../../../../apps/koru/src/manage/cloud/inventory.rs"),
    ),
    (
        "apps/koru/src/manage/cloud/progress.rs",
        include_str!("../../../../../../../apps/koru/src/manage/cloud/progress.rs"),
    ),
    (
        "apps/koru/src/manage/devices.rs",
        include_str!("../../../../../../../apps/koru/src/manage/devices.rs"),
    ),
    (
        "apps/koru/src/manage/devices/history.rs",
        include_str!("../../../../../../../apps/koru/src/manage/devices/history.rs"),
    ),
    (
        "apps/koru/src/manage/dialogs.rs",
        include_str!("../../../../../../../apps/koru/src/manage/dialogs.rs"),
    ),
    (
        "apps/koru/src/manage/nav.rs",
        include_str!("../../../../../../../apps/koru/src/manage/nav.rs"),
    ),
    (
        "apps/koru/src/manage/new_profile.rs",
        include_str!("../../../../../../../apps/koru/src/manage/new_profile.rs"),
    ),
    (
        "apps/koru/src/manage/page.rs",
        include_str!("../../../../../../../apps/koru/src/manage/page.rs"),
    ),
    (
        "apps/koru/src/manage/profile_page.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profile_page.rs"),
    ),
    (
        "apps/koru/src/manage/profile_page/paint.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profile_page/paint.rs"),
    ),
    (
        "apps/koru/src/manage/profiles.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles.rs"),
    ),
    (
        "apps/koru/src/manage/profiles/attach.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles/attach.rs"),
    ),
    (
        "apps/koru/src/manage/profiles/cloud.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles/cloud.rs"),
    ),
    (
        "apps/koru/src/manage/profiles/cloud/inspection.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles/cloud/inspection.rs"),
    ),
    (
        "apps/koru/src/manage/profiles/cloud/inventory.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles/cloud/inventory.rs"),
    ),
    (
        "apps/koru/src/manage/profiles/cloud/storage.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles/cloud/storage.rs"),
    ),
    (
        "apps/koru/src/manage/profiles/creation.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles/creation.rs"),
    ),
    (
        "apps/koru/src/manage/profiles/effect.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles/effect.rs"),
    ),
    (
        "apps/koru/src/manage/profiles/inline_connect.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles/inline_connect.rs"),
    ),
    (
        "apps/koru/src/manage/profiles/intent.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles/intent.rs"),
    ),
    (
        "apps/koru/src/manage/profiles/progress.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles/progress.rs"),
    ),
    (
        "apps/koru/src/manage/profiles/removal.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles/removal.rs"),
    ),
    (
        "apps/koru/src/manage/profiles/results.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles/results.rs"),
    ),
    (
        "apps/koru/src/manage/profiles/scopes.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles/scopes.rs"),
    ),
    (
        "apps/koru/src/manage/profiles/surface.rs",
        include_str!("../../../../../../../apps/koru/src/manage/profiles/surface.rs"),
    ),
    (
        "apps/koru/src/manage/recovery.rs",
        include_str!("../../../../../../../apps/koru/src/manage/recovery.rs"),
    ),
    (
        "apps/koru/src/manage/sign_in.rs",
        include_str!("../../../../../../../apps/koru/src/manage/sign_in.rs"),
    ),
    (
        "apps/koru/src/manage/workspaces.rs",
        include_str!("../../../../../../../apps/koru/src/manage/workspaces.rs"),
    ),
    (
        "apps/koru/src/manager_view.rs",
        include_str!("../../../../../../../apps/koru/src/manager_view.rs"),
    ),
    (
        "apps/koru/src/mini_app.rs",
        include_str!("../../../../../../../apps/koru/src/mini_app.rs"),
    ),
    (
        "apps/koru/src/mini_app/artifact.rs",
        include_str!("../../../../../../../apps/koru/src/mini_app/artifact.rs"),
    ),
    (
        "apps/koru/src/mini_app/chrome.rs",
        include_str!("../../../../../../../apps/koru/src/mini_app/chrome.rs"),
    ),
    (
        "apps/koru/src/mini_app/composition.rs",
        include_str!("../../../../../../../apps/koru/src/mini_app/composition.rs"),
    ),
    (
        "apps/koru/src/mini_app/declaration.rs",
        include_str!("../../../../../../../apps/koru/src/mini_app/declaration.rs"),
    ),
    (
        "apps/koru/src/mini_app/pin.rs",
        include_str!("../../../../../../../apps/koru/src/mini_app/pin.rs"),
    ),
    (
        "apps/koru/src/mini_app/refinement.rs",
        include_str!("../../../../../../../apps/koru/src/mini_app/refinement.rs"),
    ),
    (
        "apps/koru/src/panes/context.rs",
        include_str!("../../../../../../../apps/koru/src/panes/context.rs"),
    ),
    (
        "apps/koru/src/panes/explore.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore.rs"),
    ),
    (
        "apps/koru/src/panes/explore/accessibility.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/accessibility.rs"),
    ),
    (
        "apps/koru/src/panes/explore/accessibility/chats.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/accessibility/chats.rs"),
    ),
    (
        "apps/koru/src/panes/explore/actions.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/actions.rs"),
    ),
    (
        "apps/koru/src/panes/explore/apps_rail_drag.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/apps_rail_drag.rs"),
    ),
    (
        "apps/koru/src/panes/explore/apps_rail_drag/workspaces.rs",
        include_str!(
            "../../../../../../../apps/koru/src/panes/explore/apps_rail_drag/workspaces.rs"
        ),
    ),
    (
        "apps/koru/src/panes/explore/apps_rail_elements.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/apps_rail_elements.rs"),
    ),
    (
        "apps/koru/src/panes/explore/canvases.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/canvases.rs"),
    ),
    (
        "apps/koru/src/panes/explore/heading_elements.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/heading_elements.rs"),
    ),
    (
        "apps/koru/src/panes/explore/help.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/help.rs"),
    ),
    (
        "apps/koru/src/panes/explore/home_nav.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/home_nav.rs"),
    ),
    (
        "apps/koru/src/panes/explore/organization_sync.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/organization_sync.rs"),
    ),
    (
        "apps/koru/src/panes/explore/popover_elements.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/popover_elements.rs"),
    ),
    (
        "apps/koru/src/panes/explore/popover_elements/profiles.rs",
        include_str!(
            "../../../../../../../apps/koru/src/panes/explore/popover_elements/profiles.rs"
        ),
    ),
    (
        "apps/koru/src/panes/explore/rail.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/rail.rs"),
    ),
    (
        "apps/koru/src/panes/explore/rail/accessibility.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/rail/accessibility.rs"),
    ),
    (
        "apps/koru/src/panes/explore/rail_overflow.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/rail_overflow.rs"),
    ),
    (
        "apps/koru/src/panes/explore/slides.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/slides.rs"),
    ),
    (
        "apps/koru/src/panes/explore/workspace_menu_elements.rs",
        include_str!("../../../../../../../apps/koru/src/panes/explore/workspace_menu_elements.rs"),
    ),
    (
        "apps/koru/src/panes/fold.rs",
        include_str!("../../../../../../../apps/koru/src/panes/fold.rs"),
    ),
    (
        "apps/koru/src/panes/home/mod.rs",
        include_str!("../../../../../../../apps/koru/src/panes/home/mod.rs"),
    ),
    (
        "apps/koru/src/panes/mod.rs",
        include_str!("../../../../../../../apps/koru/src/panes/mod.rs"),
    ),
    (
        "apps/koru/src/panes/sidebar/apps_rail.rs",
        include_str!("../../../../../../../apps/koru/src/panes/sidebar/apps_rail.rs"),
    ),
    (
        "apps/koru/src/panes/sidebar/mod.rs",
        include_str!("../../../../../../../apps/koru/src/panes/sidebar/mod.rs"),
    ),
    (
        "apps/koru/src/panes/sidebar/rail_elements.rs",
        include_str!("../../../../../../../apps/koru/src/panes/sidebar/rail_elements.rs"),
    ),
    (
        "apps/koru/src/panes/sidebar/rail_order.rs",
        include_str!("../../../../../../../apps/koru/src/panes/sidebar/rail_order.rs"),
    ),
    (
        "apps/koru/src/result_document.rs",
        include_str!("../../../../../../../apps/koru/src/result_document.rs"),
    ),
    (
        "apps/koru/src/result_view.rs",
        include_str!("../../../../../../../apps/koru/src/result_view.rs"),
    ),
    (
        "apps/koru/src/run_view.rs",
        include_str!("../../../../../../../apps/koru/src/run_view.rs"),
    ),
    (
        "apps/koru/src/search.rs",
        include_str!("../../../../../../../apps/koru/src/search.rs"),
    ),
    (
        "apps/koru/src/session_offer.rs",
        include_str!("../../../../../../../apps/koru/src/session_offer.rs"),
    ),
    (
        "apps/koru/src/session_tab.rs",
        include_str!("../../../../../../../apps/koru/src/session_tab.rs"),
    ),
    (
        "apps/koru/src/settings_view.rs",
        include_str!("../../../../../../../apps/koru/src/settings_view.rs"),
    ),
    (
        "apps/koru/src/settings_view/reminders.rs",
        include_str!("../../../../../../../apps/koru/src/settings_view/reminders.rs"),
    ),
    (
        "apps/koru/src/socials_view.rs",
        include_str!("../../../../../../../apps/koru/src/socials_view.rs"),
    ),
    (
        "apps/koru/src/support/fab.rs",
        include_str!("../../../../../../../apps/koru/src/support/fab.rs"),
    ),
    (
        "apps/koru/src/support/mod.rs",
        include_str!("../../../../../../../apps/koru/src/support/mod.rs"),
    ),
    (
        "apps/koru/src/support/session.rs",
        include_str!("../../../../../../../apps/koru/src/support/session.rs"),
    ),
    (
        "apps/koru/src/support/surface.rs",
        include_str!("../../../../../../../apps/koru/src/support/surface.rs"),
    ),
    (
        "apps/koru/src/support/url.rs",
        include_str!("../../../../../../../apps/koru/src/support/url.rs"),
    ),
    (
        "apps/koru/src/support/view.rs",
        include_str!("../../../../../../../apps/koru/src/support/view.rs"),
    ),
    (
        "apps/koru/src/taskbar.rs",
        include_str!("../../../../../../../apps/koru/src/taskbar.rs"),
    ),
    (
        "apps/koru/src/tasks_view/accessibility.rs",
        include_str!("../../../../../../../apps/koru/src/tasks_view/accessibility.rs"),
    ),
    (
        "apps/koru/src/tasks_view/body.rs",
        include_str!("../../../../../../../apps/koru/src/tasks_view/body.rs"),
    ),
    (
        "apps/koru/src/tasks_view/capture.rs",
        include_str!("../../../../../../../apps/koru/src/tasks_view/capture.rs"),
    ),
    (
        "apps/koru/src/tasks_view/draw.rs",
        include_str!("../../../../../../../apps/koru/src/tasks_view/draw.rs"),
    ),
    (
        "apps/koru/src/tasks_view/echo.rs",
        include_str!("../../../../../../../apps/koru/src/tasks_view/echo.rs"),
    ),
    (
        "apps/koru/src/tasks_view/heading.rs",
        include_str!("../../../../../../../apps/koru/src/tasks_view/heading.rs"),
    ),
    (
        "apps/koru/src/tasks_view/menu.rs",
        include_str!("../../../../../../../apps/koru/src/tasks_view/menu.rs"),
    ),
    (
        "apps/koru/src/tasks_view/mod.rs",
        include_str!("../../../../../../../apps/koru/src/tasks_view/mod.rs"),
    ),
    (
        "apps/koru/src/tasks_view/pickers.rs",
        include_str!("../../../../../../../apps/koru/src/tasks_view/pickers.rs"),
    ),
    (
        "apps/koru/src/tasks_view/query.rs",
        include_str!("../../../../../../../apps/koru/src/tasks_view/query.rs"),
    ),
    (
        "apps/koru/src/tasks_view/rank.rs",
        include_str!("../../../../../../../apps/koru/src/tasks_view/rank.rs"),
    ),
    (
        "apps/koru/src/tasks_view/row.rs",
        include_str!("../../../../../../../apps/koru/src/tasks_view/row.rs"),
    ),
    (
        "apps/koru/src/tasks_view/states.rs",
        include_str!("../../../../../../../apps/koru/src/tasks_view/states.rs"),
    ),
    (
        "apps/koru/src/tasks_view/view.rs",
        include_str!("../../../../../../../apps/koru/src/tasks_view/view.rs"),
    ),
    (
        "apps/koru/src/trash/draw.rs",
        include_str!("../../../../../../../apps/koru/src/trash/draw.rs"),
    ),
    (
        "apps/koru/src/trash/menu.rs",
        include_str!("../../../../../../../apps/koru/src/trash/menu.rs"),
    ),
    (
        "apps/koru/src/trash/mod.rs",
        include_str!("../../../../../../../apps/koru/src/trash/mod.rs"),
    ),
    (
        "apps/koru/src/trash/view.rs",
        include_str!("../../../../../../../apps/koru/src/trash/view.rs"),
    ),
    (
        "apps/koru/src/welcome/build.rs",
        include_str!("../../../../../../../apps/koru/src/welcome/build.rs"),
    ),
    (
        "apps/koru/src/welcome/draw.rs",
        include_str!("../../../../../../../apps/koru/src/welcome/draw.rs"),
    ),
    (
        "apps/koru/src/welcome/hints.rs",
        include_str!("../../../../../../../apps/koru/src/welcome/hints.rs"),
    ),
    (
        "apps/koru/src/welcome/layout.rs",
        include_str!("../../../../../../../apps/koru/src/welcome/layout.rs"),
    ),
    (
        "apps/koru/src/welcome/mcp.rs",
        include_str!("../../../../../../../apps/koru/src/welcome/mcp.rs"),
    ),
    (
        "apps/koru/src/welcome/mod.rs",
        include_str!("../../../../../../../apps/koru/src/welcome/mod.rs"),
    ),
    (
        "apps/koru/src/welcome/rows.rs",
        include_str!("../../../../../../../apps/koru/src/welcome/rows.rs"),
    ),
];

/// The app crates, as Rust code names them, and where their source is.
const CRATES: &[(&str, &str)] = &[
    ("koru", "apps/koru/src/"),
    ("koru_document", "apps/koru-document/src/"),
    ("koru_agent", "apps/koru-agent/src/"),
];

/// Whether a record's owner is one of the app crates.
pub(super) fn owns(owner: &str) -> bool {
    CRATES
        .iter()
        .any(|(krate, _)| krate.replace('_', "-") == owner)
}

/// Whether a file is an app crate's, which says how its declarations read.
pub(super) fn is_app_file(file: &str) -> bool {
    CRATES.iter().any(|(_, root)| file.starts_with(root))
}

fn app_source(path: &str) -> Option<&'static str> {
    SOURCES
        .iter()
        .find(|(file, _)| *file == path)
        .map(|(_, text)| *text)
}

/// The files a symbol's module covers, and the item path left after it:
/// `koru::mini_app::chrome::Chrome::open_selector` is `mini_app/chrome.rs`
/// (and any children) with `Chrome::open_selector` left over. The longest
/// module prefix that is a compiled-in file wins; the crate root is its
/// `main.rs` or `lib.rs` alone.
fn module_of(symbol: &str) -> Option<(Vec<&'static str>, Vec<&str>)> {
    let segments: Vec<&str> = symbol.split("::").collect();
    let root = CRATES
        .iter()
        .find(|(krate, _)| *krate == segments[0])
        .map(|(_, root)| *root)?;
    for split in (2..segments.len()).rev() {
        let path = segments[1..split].join("/");
        let own = [format!("{root}{path}.rs"), format!("{root}{path}/mod.rs")];
        if own.iter().any(|file| app_source(file).is_some()) {
            let children = format!("{root}{path}/");
            let files = SOURCES
                .iter()
                .filter(|(file, _)| own.contains(&file.to_string()) || file.starts_with(&children))
                .map(|(_, text)| *text)
                .collect();
            return Some((files, segments[split..].to_vec()));
        }
    }
    let files: Vec<&'static str> = ["main.rs", "lib.rs"]
        .iter()
        .filter_map(|file| app_source(&format!("{root}{file}")))
        .collect();
    (!files.is_empty()).then(|| (files, segments[1..].to_vec()))
}

/// Whether `text` declares `item`, whatever its visibility.
fn declares_any(text: &str, item: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    [
        format!("struct {item}"),
        format!("enum {item}"),
        format!("trait {item}"),
        format!("type {item}"),
        format!("mod {item}"),
        format!("fn {item}("),
        format!("fn {item}<"),
        format!("const {item}:"),
        format!("static {item}:"),
    ]
    .iter()
    .any(|pattern| {
        text.match_indices(pattern.as_str()).any(|(at, _)| {
            let before = text[..at].chars().next_back().is_none_or(|c| !ident(c));
            let after = pattern.ends_with(['(', '<', ':'])
                || text[at + pattern.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !ident(c));
            before && after
        })
    }) || text.lines().map(str::trim_start).any(|line| {
        (line.starts_with("pub use ") || line.starts_with("pub(crate) use "))
            && line.split(|c: char| !ident(c)).any(|word| word == item)
    })
}

/// Every app symbol a record names is declared where its path says: the
/// item in its module's files, and for `Type::method` both the type and the
/// method.
pub(super) fn declared(symbol: &str) -> bool {
    let Some((files, item)) = module_of(symbol) else {
        return false;
    };
    let found = |name: &str| files.iter().any(|text| declares_any(text, name));
    match item.as_slice() {
        [item] => found(item),
        [parent, item] if parent.starts_with(|c: char| c.is_ascii_uppercase()) => {
            found(parent) && found(item)
        }
        _ => false,
    }
}

/// The members of `enum <name>` in `source`, whatever its visibility, read
/// the way the crates' option checks read a `pub enum`.
pub(super) fn enum_members(source: &'static str, name: &str) -> Vec<&'static str> {
    let opening = [format!("enum {name} {{"), format!("enum {name}<")]
        .iter()
        .find_map(|pattern| source.find(pattern.as_str()))
        .unwrap_or_else(|| panic!("the source no longer declares `enum {name}`"));
    let body = &source[opening..];
    let body = &body[body.find('{').expect("the enum opens") + 1..];
    let body = &body[..body.find("\n}").expect("the enum closes")];
    body.lines()
        .filter(|line| line.starts_with("    ") && !line.starts_with("     "))
        .map(str::trim)
        .filter(|line| !line.starts_with("//") && !line.starts_with('#'))
        .filter_map(|line| {
            let end = line
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(line.len());
            let ident = &line[..end];
            (!ident.is_empty() && ident.starts_with(|c: char| c.is_ascii_uppercase()))
                .then_some(ident)
        })
        .collect()
}

/// Whether `source` declares a constant or a function of this name, whatever
/// its visibility.
pub(super) fn declares_value(source: &str, ident: &str, declared: Declared) -> bool {
    match declared {
        Declared::Consts => source.contains(&format!("const {ident}:")),
        _ => source.contains(&format!("fn {ident}(")),
    }
}

/// The modules an app file declares, whatever their visibility, in order,
/// leaving out the test modules named as such. An inline `mod x { … }` is
/// not a file and is not read.
fn declared_modules(text: &'static str) -> Vec<&'static str> {
    code(text)
        .filter_map(|line| {
            let rest = ["pub(crate) mod ", "pub(super) mod ", "pub mod ", "mod "]
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))?;
            let name = rest.strip_suffix(';')?;
            (!name.ends_with("tests")).then_some(name)
        })
        .collect()
}

/// One app file that declares modules: the records whose items it declares
/// itself, and how each module it declares is accounted for.
pub(super) struct Root {
    pub(super) owner: &'static str,
    pub(super) file: &'static str,
    pub(super) own: &'static [&'static str],
    pub(super) modules: &'static [(&'static str, Module)],
}

/// The baselines, an app file at a time.
pub(super) const ROOTS: &[Root] = &[
    Root {
        owner: "koru",
        file: "apps/koru/src/panes/mod.rs",
        own: &[],
        modules: &[
            ("context", Module::Records(&["C-CONTEXT-COLUMN"])),
            ("explore", Module::Records(&["C-EXPLORE"])),
            (
                "fold",
                Module::Excluded(
                    "dead code: every painting item is #[allow(dead_code)], unconstructed since the ADR-262 move",
                ),
            ),
            ("home", Module::Records(&["C-HOME-PAGE"])),
            ("sidebar", Module::Records(&["C-APPS-RAIL"])),
        ],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/panes/explore.rs",
        own: &["C-EXPLORE"],
        modules: &[
            ("accessibility", Module::Records(&["C-EXPLORE"])),
            ("actions", Module::Records(&["C-EXPLORE"])),
            (
                "apps_rail_drag",
                Module::Excluded("orchestration: dragging a rail square to reorder it"),
            ),
            ("apps_rail_elements", Module::Records(&["C-APPS-RAIL"])),
            (
                "canvases",
                Module::Excluded("data: the canvas rows the explore list paints"),
            ),
            ("heading_elements", Module::Records(&["C-EXPLORE"])),
            ("home_nav", Module::Records(&["C-HOME-NAV"])),
            (
                "organization_sync",
                Module::Excluded("data: the sync state behind the visible sync word"),
            ),
            ("rail", Module::Records(&["C-TASKS-RAIL"])),
            (
                "rail_overflow",
                Module::Excluded("data: which rail items fit and which go to More views"),
            ),
            (
                "slides",
                Module::Excluded("support: the rows' vertical slide offsets; draws nothing"),
            ),
            (
                "workspace_menu_elements",
                Module::Records(&["C-EXPLORE-POPOVERS"]),
            ),
            ("help", Module::Records(&["C-EXPLORE-POPOVERS"])),
            (
                "popover_baseline",
                Module::Excluded("test-only: #[cfg(test)] baseline of the popovers' geometry"),
            ),
            ("popover_elements", Module::Records(&["C-EXPLORE-POPOVERS"])),
        ],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/panes/sidebar/mod.rs",
        own: &[],
        modules: &[
            ("apps_rail", Module::Records(&["C-APPS-RAIL"])),
            ("rail_elements", Module::Records(&["C-APPS-RAIL"])),
            ("rail_order", Module::Records(&["C-APPS-RAIL"])),
        ],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/mainview/mod.rs",
        own: &["H-MAIN-VIEW"],
        modules: &[
            (
                "harness",
                Module::Excluded("test-only: #[cfg(test)] fixtures for the container's suite"),
            ),
            ("accessibility", Module::Records(&["H-MAIN-VIEW"])),
            ("agent_cli_updates", Module::Records(&["C-AGENT-CLI-CARDS"])),
            (
                "agent_install",
                Module::Excluded(
                    "orchestration: installs a coding agent's command-line tool; draws nothing",
                ),
            ),
            (
                "agent_mcp",
                Module::Excluded(
                    "orchestration: registers the MCP bridge with a coding agent; draws nothing",
                ),
            ),
            (
                "agent_providers",
                Module::Excluded(
                    "data: the coding-agent provider catalog the container offers; draws nothing",
                ),
            ),
            (
                "canvas",
                Module::Excluded(
                    "orchestration: the canvas tab's wiring to its document; draws nothing",
                ),
            ),
            (
                "canvas_chain",
                Module::Excluded(
                    "orchestration: runs a canvas workflow chain for the canvas tab; draws nothing",
                ),
            ),
            (
                "canvas_runs",
                Module::Excluded("orchestration: the canvas tab's agent runs; draws nothing"),
            ),
            (
                "chat_status",
                Module::Excluded(
                    "data: the chats' status book behind the tab strip's status marks",
                ),
            ),
            (
                "chrome_debug",
                Module::Excluded("io: chrome diagnostics to stderr"),
            ),
            ("chrome_focus", Module::Records(&["C-LANE"])),
            (
                "closed_tabs",
                Module::Excluded("data: the reopen list of closed tabs"),
            ),
            (
                "cloud_sync",
                Module::Excluded("data: the Cloud sync panel's state, drawn by the context column"),
            ),
            ("context_menu", Module::Records(&["C-APP-MENUS"])),
            ("delegate", Module::Records(&["C-DELEGATE-FORM"])),
            ("dictation", Module::Records(&["C-DICTATION-STRIP"])),
            (
                "document_appearance",
                Module::Excluded(
                    "orchestration: applies a document's page appearance; draws nothing",
                ),
            ),
            (
                "draw",
                Module::Records(&[
                    "H-MAIN-VIEW",
                    "C-LANE",
                    "C-APP-CAPTION",
                    "C-APP-ZOOM-FLASH",
                    "C-APP-MENUS",
                ]),
            ),
            ("error_dialog", Module::Records(&["C-ERROR-DIALOG"])),
            ("file_preview", Module::Records(&["C-CHAT-FILE-PREVIEW"])),
            ("font_picker", Module::Records(&["C-FONT-PICKER"])),
            (
                "help_menu",
                Module::Excluded(
                    "data: the help menu's rows; its popover is panes/explore/help.rs",
                ),
            ),
            (
                "history",
                Module::Excluded(
                    "data: the document history panel's state, drawn by the context column",
                ),
            ),
            ("icon_action", Module::Records(&["C-APP-CAPTION"])),
            ("import_dialog", Module::Records(&["C-IMPORT-DIALOG"])),
            (
                "keys",
                Module::Excluded(
                    "orchestration: the chords the container claims before a view sees them",
                ),
            ),
            ("launcher", Module::Records(&["C-APP-MENUS"])),
            ("layout", Module::Records(&["H-MAIN-VIEW"])),
            ("link_device", Module::Records(&["C-LINK-DEVICE-PANEL"])),
            (
                "linked_devices",
                Module::Excluded("data: the linked-devices list the Manage tab draws"),
            ),
            (
                "mini_app",
                Module::Excluded(
                    "orchestration: opens, lands and refines mini-app tabs; the mini app draws itself",
                ),
            ),
            (
                "mini_app_artifacts",
                Module::Excluded("orchestration: drains the mini app's save and open answers"),
            ),
            ("mini_app_pins", Module::Records(&["C-APP-MENUS"])),
            ("move_to", Module::Records(&["C-MOVE-TO"])),
            (
                "nav",
                Module::Excluded(
                    "orchestration: where the window is standing and how it gets back",
                ),
            ),
            (
                "nav_debug",
                Module::Excluded("io: navigation diagnostics to stderr"),
            ),
            ("new_document", Module::Records(&["C-NAME-PROMPTS"])),
            ("new_list", Module::Records(&["C-NAME-PROMPTS"])),
            (
                "open",
                Module::Excluded(
                    "orchestration: how a document arrives in a tab; the drop preview it computes is painted by draw",
                ),
            ),
            (
                "panes",
                Module::Records(&["C-PANE-SPLITS", "C-APP-CAPTION"]),
            ),
            ("place_chooser", Module::Records(&["C-APP-MENUS"])),
            (
                "profile_probe",
                Module::Excluded("io: probes whether a Profile is reachable"),
            ),
            ("profile_wait", Module::Records(&["C-PROFILE-WAIT"])),
            ("rail_overflow", Module::Records(&["C-APP-MENUS"])),
            ("recording", Module::Records(&["C-RECORDING-SETUP"])),
            ("reminder_creation", Module::Records(&["C-REMINDER-EDITOR"])),
            (
                "reminder_management",
                Module::Records(&["C-REMINDER-EDITOR"]),
            ),
            ("reminder_shell", Module::Records(&["C-REMINDER-EDITOR"])),
            (
                "reminders",
                Module::Excluded(
                    "orchestration: drains the editor's reminder requests (H-EDITOR-REMINDERS) and the Agent's reminder state into the reminder surfaces",
                ),
            ),
            ("reminders_view", Module::Records(&["C-REMINDER-EDITOR"])),
            (
                "routing",
                Module::Excluded("orchestration: which surface an input goes to"),
            ),
            (
                "run_trash",
                Module::Excluded(
                    "orchestration: the confirm and toast content for trashed runs, drawn by trash.rs",
                ),
            ),
            (
                "runs",
                Module::Excluded("orchestration: the run index the Runs navigator reads"),
            ),
            (
                "schedules",
                Module::Excluded("orchestration: scheduled delegations"),
            ),
            ("session", Module::Excluded("data: what survives a restart")),
            (
                "session_backend",
                Module::Excluded("io: the terminal sessions' backend"),
            ),
            (
                "session_lanes",
                Module::Excluded("data: the terminal sessions' lane grouping"),
            ),
            ("snapshot_prompt", Module::Records(&["C-NAME-PROMPTS"])),
            ("tab", Module::Records(&["C-APP-CAPTION"])),
            (
                "tab_rules",
                Module::Excluded("data: which views may share or replace a tab"),
            ),
            (
                "taskcounts",
                Module::Excluded(
                    "data: the task counts panel's state, drawn by the context column",
                ),
            ),
            (
                "tasks",
                Module::Excluded(
                    "orchestration: the task commands that need the workspace a view never holds",
                ),
            ),
            (
                "terminal_look",
                Module::Excluded("data: a terminal's resolved look"),
            ),
            ("trash", Module::Records(&["H-MAIN-VIEW"])),
            ("update_ui", Module::Records(&["C-UPDATE-PROMPTS"])),
            (
                "welcome_progress",
                Module::Excluded("data: the Welcome page's ticks"),
            ),
            ("workspace_controller", Module::Records(&["H-MAIN-VIEW"])),
            (
                "workspace_icon",
                Module::Excluded(
                    "hosting: opens koru-appearance-ui's emoji and color panel (C-EMOJI-COLOR-PANEL), which draws itself",
                ),
            ),
            ("workspace_menu", Module::Records(&["C-APP-MENUS"])),
            ("workspace_rename", Module::Records(&["C-NAME-PROMPTS"])),
            ("zoomlabel", Module::Records(&["C-APP-ZOOM-FLASH"])),
        ],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/welcome/mod.rs",
        own: &["C-APP-WELCOME"],
        modules: &[
            (
                "build",
                Module::Excluded(
                    "data: the build facts (release date, channel notes) the update surface reads; the page does not draw them",
                ),
            ),
            ("draw", Module::Records(&["C-APP-WELCOME"])),
            (
                "hints",
                Module::Excluded(
                    "data: the shortcut hint words of the Learn row, which is hidden in the product",
                ),
            ),
            ("layout", Module::Records(&["C-APP-WELCOME"])),
            (
                "mcp",
                Module::Excluded(
                    "data: the MCP command text and the agent choices the page's command field shows",
                ),
            ),
            ("rows", Module::Records(&["C-APP-WELCOME"])),
        ],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/manage.rs",
        own: &["C-MANAGE"],
        modules: &[
            ("accessibility", Module::Records(&["C-MANAGE"])),
            ("cloud", Module::Records(&["C-MANAGE-CLOUD"])),
            ("devices", Module::Records(&["C-MANAGE-DEVICES"])),
            ("dialogs", Module::Records(&["C-MANAGE"])),
            ("nav", Module::Records(&["C-MANAGE"])),
            ("new_profile", Module::Records(&["C-MANAGE"])),
            ("page", Module::Records(&["C-MANAGE"])),
            ("profile_page", Module::Records(&["C-MANAGE"])),
            (
                "profiles",
                Module::Excluded(
                    "orchestration: ProfilesController, the Manage tab's Profile state, intents, effects and results; the words it produces are painted by the page records",
                ),
            ),
            ("recovery", Module::Records(&["C-MANAGE"])),
            ("sign_in", Module::Records(&["C-MANAGE"])),
            ("workspaces", Module::Records(&["C-MANAGE-WORKSPACES"])),
        ],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/trash/mod.rs",
        own: &["C-TRASH-PAGE"],
        modules: &[
            ("draw", Module::Records(&["C-TRASH-PAGE"])),
            (
                "menu",
                Module::Excluded(
                    "data: the Trash rows' and header's menu items, drawn by the container's context menu",
                ),
            ),
            ("view", Module::Records(&["C-TRASH-PAGE"])),
        ],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/feedback.rs",
        own: &["C-FEEDBACK-PAGE"],
        modules: &[
            (
                "draft",
                Module::Excluded("data: the unsent report kept for the session"),
            ),
            (
                "logs",
                Module::Excluded("io: zips the recent application logs"),
            ),
            (
                "outbox",
                Module::Excluded("io: the background send and its retry deadline"),
            ),
            (
                "submit",
                Module::Excluded(
                    "io: payload validation, screenshot compression and the HTTP transport",
                ),
            ),
        ],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/diff/mod.rs",
        own: &["C-VERSION-COMPARE"],
        modules: &[],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/canvas_view.rs",
        own: &["C-CANVAS-TAB"],
        modules: &[
            ("accessibility", Module::Records(&["C-CANVAS-TAB"])),
            ("agents", Module::Records(&["C-CANVAS-TAB"])),
            ("decor", Module::Records(&["C-CANVAS-TAB"])),
            ("dialog", Module::Records(&["C-CANVAS-TEXT-DIALOG"])),
            (
                "perform",
                Module::Excluded(
                    "orchestration: applies the canvas surface's requests to the canvas document",
                ),
            ),
        ],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/support/mod.rs",
        own: &[],
        modules: &[
            ("fab", Module::Records(&["C-SUPPORT-FAB"])),
            ("session", Module::Records(&["C-SUPPORT-VIEW"])),
            ("surface", Module::Records(&["C-SUPPORT-VIEW"])),
            (
                "url",
                Module::Excluded(
                    "data: the support page's URL, its navigation policy and the web engine's data directory",
                ),
            ),
            ("view", Module::Records(&["C-SUPPORT-VIEW"])),
        ],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/session_offer.rs",
        own: &["C-SESSION-OFFER"],
        modules: &[],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/settings_view.rs",
        own: &["H-APP-TAB-HOSTS"],
        modules: &[("reminders", Module::Records(&["C-REMINDER-SETTINGS"]))],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/manager_view.rs",
        own: &["C-POPOUT-CLOSE-DIALOG", "H-MANAGER-VIEW"],
        modules: &[],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/taskbar.rs",
        own: &["C-JUMP-LIST"],
        modules: &[],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/mini_app.rs",
        own: &["C-MINI-APP", "C-DOCUMENT-HOST"],
        modules: &[
            ("artifact", Module::Records(&["C-MINI-APP"])),
            (
                "chrome",
                Module::Records(&["C-MINI-APP", "C-TASK-LIST-SELECTOR", "C-DOCUMENT-SEARCH"]),
            ),
            ("composition", Module::Records(&["C-COMPOSITION-HOST"])),
            (
                "declaration",
                Module::Excluded(
                    "data: the fixed catalog's declaration adapted to the app's bucket or list target, arrangement and document; draws nothing",
                ),
            ),
            ("pin", Module::Records(&["C-MINI-APP"])),
            (
                "refinement",
                Module::Records(&["C-MINI-APP", "C-TASK-LIST-SELECTOR"]),
            ),
        ],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/leaf.rs",
        own: &["H-APP-LEAF"],
        modules: &[],
    },
    Root {
        owner: "koru",
        file: "apps/koru/src/tasks_view/mod.rs",
        own: &["C-TASKS-VIEW"],
        modules: &[
            ("accessibility", Module::Records(&["C-TASKS-VIEW"])),
            ("body", Module::Records(&["C-TASKS-VIEW"])),
            ("capture", Module::Records(&["C-TASKS-VIEW"])),
            ("draw", Module::Records(&["C-TASKS-VIEW"])),
            (
                "echo",
                Module::Excluded(
                    "data: the optimistic echo of a task command (patches, count deltas, bucket membership) applied before the store answers; draws nothing",
                ),
            ),
            ("heading", Module::Records(&["C-TASKS-VIEW"])),
            ("menu", Module::Records(&["C-TASKS-VIEW"])),
            ("pickers", Module::Records(&["C-TASKS-VIEW"])),
            (
                "query",
                Module::Excluded(
                    "io: the store's tasks query, paging, filters and write waits the container runs; its row type TaskEntry is re-exported and recorded at the module root",
                ),
            ),
            (
                "rank",
                Module::Excluded("data: rank arithmetic for a drag reorder; draws nothing"),
            ),
            ("row", Module::Records(&["C-TASKS-VIEW"])),
            ("states", Module::Records(&["C-TASKS-VIEW"])),
            ("view", Module::Records(&["C-TASKS-VIEW"])),
            (
                "perf",
                Module::Excluded("test-only: #[cfg(test)] client-side baselines, ignored as a set"),
            ),
        ],
    },
];

/// Each baselined app file's modules against its classification, every
/// record it names owned by its app, and every app record reached from one.
#[test]
fn every_app_module_is_classified() {
    let mut reached: Vec<&str> = Vec::new();
    for root in ROOTS {
        let text = app_source(root.file)
            .unwrap_or_else(|| panic!("{} is not compiled into the drift checks", root.file));
        let listed: Vec<&str> = root.modules.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            declared_modules(text),
            listed,
            "{}'s modules changed: classify the new one or drop the removed one",
            root.file
        );
        for (name, module) in root.modules {
            match module {
                Module::Records(records) => {
                    assert!(!records.is_empty(), "{}: {name} names no record", root.file);
                    reached.extend(records.iter().copied());
                }
                Module::Excluded(reason) => assert!(!reason.is_empty(), "{name}"),
            }
        }
        reached.extend(root.own.iter().copied());
        for id in root
            .own
            .iter()
            .chain(root.modules.iter().flat_map(|(_, m)| match m {
                Module::Records(records) => *records,
                Module::Excluded(_) => &[],
            }))
        {
            assert_eq!(entry(id).source.owner, root.owner, "{id} in {}", root.file);
        }
    }
    for record in catalog().filter(|e| owns(e.source.owner)) {
        assert!(
            reached.contains(&record.catalog_id),
            "{} is an app record no baselined module accounts for",
            record.catalog_id
        );
        assert!(
            matches!(record.kind, Kind::AppLocal | Kind::Support),
            "{}: an app record is app-local or support",
            record.catalog_id
        );
        assert!(
            is_app_file(record.source.file),
            "{}: {} is not an app file",
            record.catalog_id,
            record.source.file
        );
    }
}

/// The fixed catalog's five components, by the ID and name the parser
/// requires, in its order.
const FIXED_CATALOG_COMPONENTS: [(&str, &str); 5] = [
    ("root", "TaskDocument"),
    ("selector", "TaskListSelector"),
    ("tasks", "TaskCollection"),
    ("search", "DocumentSearch"),
    ("document", "DocumentHost"),
];

/// The fixed catalog's component names, which the A2UI claims check beside
/// the composition catalog's.
pub(super) fn fixed_catalog_names() -> impl Iterator<Item = &'static str> {
    FIXED_CATALOG_COMPONENTS.iter().map(|(_, name)| *name)
}

/// The fixed catalog is served by the workspace app's mini app: each of its
/// five components is claimed by exactly one app symbol, the catalog still
/// requires that component under that ID, and the host still builds it the
/// way the record says. `DocumentHost` is the composition catalog's too and
/// is claimed once for both.
#[test]
fn the_fixed_catalog_is_served_by_the_app_host() {
    for (id, name) in FIXED_CATALOG_COMPONENTS {
        assert!(
            FIXED_CATALOG.contains(&format!("(\"{id}\", \"{name}\")")),
            "the fixed catalog no longer requires {name} as `{id}`"
        );
        let claims: Vec<&str> = catalog()
            .filter(|e| owns(e.source.owner))
            .flat_map(symbols)
            .filter(|(_, a2ui, _)| *a2ui == A2ui::Component(name))
            .map(|(symbol, _, _)| symbol)
            .collect();
        assert_eq!(claims.len(), 1, "{name} is claimed by {claims:?}");
    }
    let mini_app = app_source("apps/koru/src/mini_app.rs").unwrap();
    let tasks = app_source("apps/koru/src/tasks_view/view.rs").unwrap();
    for (host, needle) in [
        // TaskCollection: the production Tasks view, without capture.
        (mini_app, "tasks: TasksView::new(target).collection_only(),"),
        (tasks, "pub(crate) fn collection_only(mut self) -> Self {"),
        // DocumentHost: a landed document becomes the retained Leaf.
        (mini_app, ".unwrap_or_else(|| Leaf::document(ws, root));"),
        (
            COMPOSITION_HOST,
            "model::NodeKind::DocumentHost { .. } => el::col(\"Document host\").fill(),",
        ),
        // TaskListSelector: the shared dropdown over the bucket and list menu.
        (
            MINI_APP_CHROME,
            "\"Task selector\" => Dropdown::new(&target_label)",
        ),
        (
            MINI_APP_CHROME,
            "pub fn open_selector(&mut self, target: RailTarget) {",
        ),
        // DocumentSearch: the shared search input and its floating results.
        (MINI_APP_CHROME, "InputStyle::search(),"),
        (
            MINI_APP_CHROME,
            "pub fn changed_query(&mut self, workspace: WorkspaceId) {",
        ),
    ] {
        assert!(
            host.contains(needle),
            "the mini app no longer reads `{needle}`"
        );
    }
}

/// What the app counts add up to, for the manifest's table.
pub(super) fn counts() -> Vec<(&'static str, usize)> {
    let modules: usize = ROOTS.iter().map(|root| root.modules.len()).sum();
    let excluded = ROOTS
        .iter()
        .flat_map(|root| root.modules)
        .filter(|(_, module)| matches!(module, Module::Excluded(_)))
        .count();
    let mut owners: Vec<&str> = ROOTS.iter().map(|root| root.owner).collect();
    owners.sort_unstable();
    owners.dedup();
    vec![
        ("app crates baselined", owners.len()),
        ("app source files", SOURCES.len()),
        ("app files baselined", ROOTS.len()),
        ("app modules classified", modules),
        ("app modules excluded", excluded),
        (
            "records: app crates",
            catalog().filter(|e| owns(e.source.owner)).count(),
        ),
    ]
}
