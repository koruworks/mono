// SPDX-License-Identifier: MPL-2.0

//! Per-chat privacy and tool-access controls.

use koru_ai_wire::UpdateChatPrivacyRequest;
use koru_i18n::Key;

use super::*;

const WORKSPACE_PANEL_FIXED_H: f32 = 35.0;
const WORKSPACE_LIST_Y: f32 = 31.0;
const WORKSPACE_SEARCH_H: f32 = 34.0;
const WORKSPACE_SEARCH_GAP: f32 = 8.0;
const WORKSPACE_SEARCH_THRESHOLD: usize = 5;
const MODE_PANEL_H: f32 = 130.0;
/// The room between a toolbar panel and the composer, and the window's edge.
const COMPOSER_PANEL_GAP: f32 = 4.0;

impl ChatSurface {
    pub(super) fn toggle_chat_privacy(&mut self) -> bool {
        let Some(chat) = self.transcript.as_ref().map(|transcript| &transcript.chat) else {
            return false;
        };
        self.send_command(Command::UpdatePrivacy(
            chat.id,
            UpdateChatPrivacyRequest {
                private: !chat.private,
            },
        ))
    }

    /// **A panel opened from the composer's toolbar**: above the composer
    /// box, clear of it, starting at `x` as far as the window lets it. The
    /// workspace, tool-approval and model panels all open here, so a chip's
    /// panel never covers the text being written.
    pub(super) fn above_composer(&self, x: f32, width: f32, height: f32) -> Rect {
        let x = x
            .min(self.viewport.x + self.viewport.w - width - COMPOSER_PANEL_GAP)
            .max(self.viewport.x + COMPOSER_PANEL_GAP);
        Rect::new(
            x,
            self.composer_hit_rect.y - height - COMPOSER_PANEL_GAP,
            width,
            height,
        )
    }

    /// Whether a panel `height` tall fits above the composer, below the
    /// chat's top.
    pub(super) fn fits_above_composer(&self, height: f32) -> bool {
        self.composer_hit_rect.y - height - COMPOSER_PANEL_GAP >= self.chat_top()
    }

    pub(super) fn tool_access_panel(&self) -> Option<Rect> {
        if !self.tool_access_open || self.transcript.is_none() {
            return None;
        }
        let button = self.tool_access_button();
        let rows = self.tool_access_visible_rows();
        let height = self.tool_access_fixed_height() + rows as f32 * TOOL_ACCESS_WORKSPACE_ROW_H;
        let width = self.composer_hit_rect.w.min(360.0).max(1.0);
        Some(self.above_composer(button.x, width, height))
    }

    pub(super) fn tool_access_visible_rows(&self) -> usize {
        let available = (self.composer_hit_rect.y - self.chat_top() - 8.0).max(0.0);
        let by_height = ((available - self.tool_access_fixed_height())
            / TOOL_ACCESS_WORKSPACE_ROW_H)
            .floor()
            .max(1.0) as usize;
        self.matching_workspace_indices()
            .len()
            .min(TOOL_ACCESS_VISIBLE_ROWS)
            .min(by_height)
            .max(1)
    }

    fn tool_access_fixed_height(&self) -> f32 {
        WORKSPACE_PANEL_FIXED_H
            + if self.destinations.len() > WORKSPACE_SEARCH_THRESHOLD {
                WORKSPACE_SEARCH_H + WORKSPACE_SEARCH_GAP
            } else {
                0.0
            }
    }

    pub(super) fn tool_access_search_rect(&self) -> Option<Rect> {
        if self.destinations.len() <= WORKSPACE_SEARCH_THRESHOLD {
            return None;
        }
        let panel = self.tool_access_panel()?;
        Some(Rect::new(
            panel.x + 8.0,
            panel.y + WORKSPACE_LIST_Y,
            panel.w - 16.0,
            WORKSPACE_SEARCH_H,
        ))
    }

    pub(super) fn matching_workspace_indices(&self) -> Vec<usize> {
        let query = self.workspace_search.raw_text().trim().to_lowercase();
        self.destinations
            .iter()
            .enumerate()
            .filter_map(|(index, workspace)| {
                (query.is_empty() || workspace.label.to_lowercase().contains(&query))
                    .then_some(index)
            })
            .collect()
    }

    pub(super) fn tool_mode_panel(&self) -> Option<Rect> {
        if !self.tool_mode_open || self.transcript.is_none() {
            return None;
        }
        let button = self.tool_mode_button()?;
        let width = self.composer_hit_rect.w.min(320.0).max(1.0);
        Some(self.above_composer(button.x + button.w - width, width, MODE_PANEL_H))
    }

    pub(super) fn tool_mode_buttons(&self) -> Option<[Rect; 2]> {
        let panel = self.tool_mode_panel()?;
        let x = panel.x + 8.0;
        let width = ((panel.w - 22.0) * 0.5).max(1.0);
        Some([
            Rect::new(x, panel.y + 30.0, width, BUTTON_H),
            Rect::new(x + width + 6.0, panel.y + 30.0, width, BUTTON_H),
        ])
    }

    pub(super) fn tool_mode_session_button(&self) -> Option<Rect> {
        let panel = self.tool_mode_panel()?;
        Some(Rect::new(
            panel.x + 8.0,
            panel.y + 96.0,
            panel.w - 16.0,
            BUTTON_H,
        ))
    }

    pub(super) fn tool_access_workspace_list(&self) -> Option<Rect> {
        let panel = self.tool_access_panel()?;
        let list_y = panel.y + self.tool_access_fixed_height() - 4.0;
        Some(Rect::new(
            panel.x + 4.0,
            list_y,
            panel.w - 8.0,
            panel.y + panel.h - list_y - 4.0,
        ))
    }

    pub(super) fn tool_access_workspace_at(&self, x: f32, y: f32) -> Option<WorkspaceId> {
        let list = self.tool_access_workspace_list()?;
        if !list.contains(x, y) {
            return None;
        }
        let visible = ((y - list.y) / TOOL_ACCESS_WORKSPACE_ROW_H).floor() as usize;
        let matches = self.matching_workspace_indices();
        matches
            .get(self.tool_workspace_offset + visible)
            .and_then(|index| self.destinations.get(*index))
            .map(|workspace| workspace.workspace_id)
    }

    pub(super) fn draw_tool_mode_picker(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(panel) = self.tool_mode_panel() else {
            return;
        };
        let Some((chat_id, policy)) = self
            .transcript
            .as_ref()
            .map(|transcript| (transcript.chat.id, transcript.tool_policy.clone()))
        else {
            return;
        };
        scene::floating_panel(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(8.0),
        );
        let p = theme::p();
        let lang = self.lang;
        let heading = self
            .engine
            .simple_layout(koru_i18n::t(lang, Key::AiToolApprovalHeading), role::meta());
        scene::draw_layout_first_line(
            canvas,
            root,
            &heading,
            panel.x + 10.0,
            panel.y + 9.0,
            Some(p.text_dim),
        );
        let modes = self
            .tool_mode_buttons()
            .expect("an open mode panel has mode buttons");
        for (rect, label, mode) in [
            (modes[0], Key::AiToolModeManualApproval, ToolMode::Manual),
            (modes[1], Key::AiToolModeAutomatic, ToolMode::Auto),
        ] {
            let label = koru_i18n::t(lang, label);
            draw_surface_button(
                canvas,
                root,
                &mut self.engine,
                rect,
                label,
                ButtonRole::Secondary,
                rect.contains(self.pointer.0, self.pointer.1),
                policy.mode == mode,
            );
        }
        let help = self.engine.simple_layout_wrapped(
            koru_i18n::t(
                lang,
                if policy.mode == ToolMode::Manual {
                    Key::AiToolModeManualHelp
                } else {
                    Key::AiToolModeAutoHelp
                },
            ),
            role::meta(),
            panel.w - 20.0,
        );
        scene::draw_layout(
            canvas,
            root,
            &help,
            panel.x + 10.0,
            panel.y + 63.0,
            Some(p.text_dim),
        );
        let session = self
            .tool_mode_session_button()
            .expect("an open mode panel has a session button");
        draw_surface_button(
            canvas,
            root,
            &mut self.engine,
            session,
            koru_i18n::t(lang, Key::AiToolAutoApproveSession),
            ButtonRole::Secondary,
            session.contains(self.pointer.0, self.pointer.1),
            self.session_auto_approve_chats.contains(&chat_id),
        );
    }

    pub(super) fn draw_tool_access_picker(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(panel) = self.tool_access_panel() else {
            return;
        };
        let Some(policy) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.tool_policy.clone())
        else {
            return;
        };
        scene::floating_panel(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(8.0),
        );
        let p = theme::p();
        let heading = self.engine.simple_layout(
            &format!(
                "WORKSPACE ACCESS    {}/{}",
                policy.workspace_ids.len(),
                self.destinations.len()
            ),
            role::meta(),
        );
        scene::draw_layout_first_line(
            canvas,
            root,
            &heading,
            panel.x + 10.0,
            panel.y + 9.0,
            Some(p.text_dim),
        );
        if let Some(search) = self.tool_access_search_rect() {
            self.workspace_search.draw(
                canvas,
                root,
                &mut self.engine,
                search,
                InputStyle::search(),
                Some("Filter workspaces"),
                self.focus == Focus::WorkspaceSearch,
            );
        }
        let list = self
            .tool_access_workspace_list()
            .expect("an open access panel has a workspace list");
        let matching = self.matching_workspace_indices();
        if matching.is_empty() {
            let empty = self.engine.simple_layout_wrapped(
                if self.destinations.is_empty() {
                    self.workspace_catalog_error
                        .as_deref()
                        .unwrap_or("No Koru workspaces are available.")
                } else {
                    "No matching workspaces."
                },
                role::row(),
                list.w - 12.0,
            );
            scene::draw_layout(
                canvas,
                root,
                &empty,
                list.x + 6.0,
                list.y + 7.0,
                Some(p.text_dim),
            );
            return;
        }
        for (visible, index) in matching
            .iter()
            .skip(self.tool_workspace_offset)
            .take(self.tool_access_visible_rows())
            .enumerate()
        {
            let workspace = &self.destinations[*index];
            let row = Rect::new(
                list.x,
                list.y + visible as f32 * TOOL_ACCESS_WORKSPACE_ROW_H,
                list.w,
                TOOL_ACCESS_WORKSPACE_ROW_H,
            );
            let selected = policy.workspace_ids.contains(&workspace.workspace_id);
            if selected || row.contains(self.pointer.0, self.pointer.1) {
                scene::fill(
                    canvas,
                    root,
                    if selected {
                        p.row_selected
                    } else {
                        p.hover_tint
                    },
                    &scene::rrect(row.x, row.y, row.w, row.h, 5.0),
                );
            }
            let label = self.engine.simple_layout_wrapped(
                &workspace.label,
                role::row(),
                (row.w - 38.0).max(1.0),
            );
            scene::draw_layout_first_line(
                canvas,
                root,
                &label,
                row.x + 8.0,
                row.y + (row.h - label.height()) * 0.5,
                Some(p.text),
            );
            if selected {
                let check = self.engine.icon_layout(icons::CHECK, 14.0);
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &check,
                    row.x + row.w - check.width() - 8.0,
                    row.y + (row.h - check.height()) * 0.5,
                    Some(p.accent),
                );
            }
        }
    }
}
