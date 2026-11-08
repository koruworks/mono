// SPDX-License-Identifier: MPL-2.0

//! Per-chat privacy and tool-access controls.

use koru_ai_core::ProfileId;
use koru_ai_wire::UpdateChatPrivacyRequest;
use koru_draw::el::{Frame, FrameCx, Interaction};
use koru_i18n::Key;

use super::*;
use crate::model_picker::SearchPaint;

mod elements;
#[cfg(test)]
mod tests;
mod tokens;

const WORKSPACE_PANEL_FIXED_H: f32 = 35.0;
const WORKSPACE_LIST_Y: f32 = 31.0;
const WORKSPACE_SEARCH_H: f32 = 34.0;
const WORKSPACE_SEARCH_GAP: f32 = 8.0;
const WORKSPACE_SEARCH_THRESHOLD: usize = 5;
/// The heading, the two modes and two lines of help.
const MODE_PANEL_H: f32 = 100.0;
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
            + if self.tool_access_filtered() {
                WORKSPACE_SEARCH_H + WORKSPACE_SEARCH_GAP
            } else {
                0.0
            }
    }

    /// **The profile this chat is in**: the window's, as its host says
    /// ([`ChatSurface::set_profile`]), else the one that owns the window's
    /// workspace. `None` while neither is known.
    pub(super) fn current_profile(&self) -> Option<ProfileId> {
        if let Some(profile) = self.profile {
            return Some(profile);
        }
        let workspace = self.context_workspace?;
        self.destinations
            .iter()
            .find(|destination| destination.workspace_id == workspace)
            .map(|destination| destination.profile_id)
    }

    /// **The workspaces the access panel offers**, as indices into the
    /// catalog: the current profile's only — none until the catalog has
    /// placed the window's workspace.
    ///
    /// A grant to another profile's workspace is left in the chat's policy —
    /// not shown, not counted, and kept, so choosing a workspace here sends
    /// it back unchanged. The service never reads it either: a chat reads
    /// only its own profile's workspaces, by the profile it is stored with
    /// (AI schema 13).
    pub(super) fn scoped_workspace_indices(&self) -> Vec<usize> {
        let Some(profile) = self.current_profile() else {
            return Vec::new();
        };
        self.destinations
            .iter()
            .enumerate()
            .filter(|(_, destination)| destination.profile_id == profile)
            .map(|(index, _)| index)
            .collect()
    }

    /// How many of the chat's workspaces the panel and the chip count: the
    /// granted ones among those the panel offers.
    pub(super) fn granted_workspace_count(&self, granted: &[WorkspaceId]) -> usize {
        self.scoped_workspace_indices()
            .into_iter()
            .filter(|index| granted.contains(&self.destinations[*index].workspace_id))
            .count()
    }

    /// Whether the panel has its filter field: over five workspaces offered.
    pub(super) fn tool_access_filtered(&self) -> bool {
        self.scoped_workspace_indices().len() > WORKSPACE_SEARCH_THRESHOLD
    }

    /// The filter field, where the frame draws it: over five workspaces.
    pub(super) fn tool_access_search_rect(&self) -> Option<Rect> {
        if !self.tool_access_filtered() {
            return None;
        }
        let (_, _, frame) = self.tool_access_frame()?;
        Some(frame.node(elements::FILTER_KEY)?.rect)
    }

    /// What the open panel's tree declares (D106): its heading and count,
    /// the filter field over five workspaces, a row for each workspace the
    /// filter lets through — the chosen ones marked — or the note that
    /// takes the list's place when none does.
    fn tool_access_spec(&self) -> Option<elements::Spec> {
        let policy = &self.transcript.as_ref()?.tool_policy;
        let lang = self.lang;
        let matching = self.matching_workspace_indices();
        let offered = self.scoped_workspace_indices().len();
        let empty = matching.is_empty().then(|| {
            if offered == 0 {
                self.workspace_catalog_error
                    .clone()
                    .unwrap_or_else(|| koru_i18n::t(lang, Key::AiToolWorkspacesNone).to_owned())
            } else {
                koru_i18n::t(lang, Key::AiToolWorkspacesNoMatch).to_owned()
            }
        });
        Some(elements::Spec {
            heading: koru_i18n::t(lang, Key::AiToolWorkspaceAccessHeading).to_owned(),
            count: format!(
                "{}/{offered}",
                self.granted_workspace_count(&policy.workspace_ids)
            ),
            filter: self.tool_access_filtered(),
            rows: matching
                .iter()
                .map(|index| {
                    let workspace = &self.destinations[*index];
                    elements::AccessRow {
                        key: elements::row_key(workspace.workspace_id),
                        label: workspace.name.clone(),
                        chosen: policy.workspace_ids.contains(&workspace.workspace_id),
                    }
                })
                .collect(),
            offset: self.tool_workspace_offset,
            empty,
        })
    }

    /// **The open panel's frame** (D106): where a press lands, which row it
    /// is, the list's scrollbar — what is drawn, laid out without the
    /// pointer.
    fn tool_access_frame(&self) -> Option<(Rect, elements::Spec, Frame)> {
        let panel = self.tool_access_panel()?;
        let spec = self.tool_access_spec()?;
        let frame = elements::layout(
            &elements::tree(&spec, elements::Painted::default()),
            panel,
            FrameCx::root(),
        );
        Some((panel, spec, frame))
    }

    /// The list's scrollbar, as the frame draws it: while there are more
    /// workspaces than rows.
    #[cfg(test)]
    pub(super) fn tool_access_scrollbar(&self) -> Option<koru_draw::scrollbar::ScrollbarGeom> {
        self.tool_access_frame()?.2.scrollbar(elements::LIST_KEY)
    }

    /// The first row a drag of the list's thumb to `y` shows, the thumb
    /// held `grab` below its top: the frame's whole rows.
    fn tool_access_drag_to(&self, frame: &Frame, y: f32, grab: f32) -> Option<usize> {
        let scroll = frame.drag_offset(elements::LIST_KEY, y, grab)?;
        let max = self
            .matching_workspace_indices()
            .len()
            .saturating_sub(self.tool_access_visible_rows());
        Some(((scroll / TOOL_ACCESS_WORKSPACE_ROW_H).round().max(0.0) as usize).min(max))
    }

    /// A press on the list's scrollbar: it holds the thumb and the list
    /// follows it. `false` for a press anywhere else.
    pub(super) fn tool_access_press_scrollbar(&mut self, x: f32, y: f32) -> bool {
        let Some((_, _, frame)) = self.tool_access_frame() else {
            return false;
        };
        let Some(bar) = frame
            .scrollbar(elements::LIST_KEY)
            .filter(|bar| bar.in_gutter(x, y))
        else {
            return false;
        };
        let grab = bar.grab_at(x, y);
        self.tool_workspace_drag = Some(grab);
        if let Some(offset) = self.tool_access_drag_to(&frame, y, grab) {
            self.tool_workspace_offset = offset;
        }
        true
    }

    /// The pointer moved while the list's thumb is held. `None` without a
    /// drag.
    pub(super) fn tool_access_drag(&mut self, y: f32) -> Option<bool> {
        let grab = self.tool_workspace_drag?;
        let next = self
            .tool_access_frame()
            .and_then(|(_, _, frame)| self.tool_access_drag_to(&frame, y, grab));
        let Some(next) = next else {
            return Some(true);
        };
        let changed = next != self.tool_workspace_offset;
        self.tool_workspace_offset = next;
        Some(changed)
    }

    /// The offered workspaces the filter lets through, matched on the name
    /// their row shows.
    pub(super) fn matching_workspace_indices(&self) -> Vec<usize> {
        let query = self.workspace_search.raw_text().trim().to_lowercase();
        self.scoped_workspace_indices()
            .into_iter()
            .filter(|index| {
                query.is_empty()
                    || self.destinations[*index]
                        .name
                        .to_lowercase()
                        .contains(&query)
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

    /// The workspace whose row is at `x`, `y`, from the frame's targets:
    /// none over the scrollbar's gutter, the filter or the heading.
    pub(super) fn tool_access_workspace_at(&self, x: f32, y: f32) -> Option<WorkspaceId> {
        let (_, _, frame) = self.tool_access_frame()?;
        let key = frame.hit_at(x, y)?;
        self.destinations
            .iter()
            .find(|workspace| elements::row_key(workspace.workspace_id) == key)
            .map(|workspace| workspace.workspace_id)
    }

    pub(super) fn draw_tool_mode_picker(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(panel) = self.tool_mode_panel() else {
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
    }

    /// **The open panel** (D106): its element tree ([`elements`]) — the
    /// heading and count, the filter field, the rows with the chosen ones'
    /// plate and ✓, the scrollbar — laid out with the pointer on it and
    /// painted while the panel's `FloatingPanel` record is open, so
    /// everything drawn in the panel nests under it in the inspector.
    pub(super) fn draw_tool_access_picker(&mut self, canvas: &mut Scene, root: Affine) {
        let Some((panel, spec, targets)) = self.tool_access_frame() else {
            return;
        };
        // The host's pointer, by key: the frame's own target under it, and
        // the list while its thumb is held.
        let hot = targets.hit_at(self.pointer.0, self.pointer.1);
        let painted = elements::Painted {
            filter: spec.filter.then(|| SearchPaint {
                input: self.workspace_search.clone(),
                placeholder: koru_i18n::t(self.lang, Key::AiToolWorkspaceFilter).to_owned(),
                focused: self.focus == Focus::WorkspaceSearch,
            }),
        };
        let tree = elements::tree(&spec, painted);
        let frame = elements::layout(
            &tree,
            panel,
            FrameCx::root().with_interaction(Interaction {
                hot,
                pressed: self.tool_workspace_drag.map(|_| elements::LIST_KEY),
                focused: None,
            }),
        );
        let _panel = scene::floating_panel_scope(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(8.0),
        );
        frame.paint(&tree, canvas, root, &mut self.engine);
    }
}
