// SPDX-License-Identifier: MPL-2.0

//! **The approval-mode picker** of a local agent's chat (koru-ai D7).
//!
//! A ghost chip in the composer's toolbar, left of the model chip, names the
//! mode the chat's next message runs in — its icon, its name and a caret —
//! and opens a list upward, like the composer's other panels: each of the
//! agent's modes with an icon, a title, a line saying what it does, and a
//! check on the current one. *Full access* is in the warning color.
//!
//! **The agent's own modes, in Koru's words.** An agent lists its modes
//! with its catalog (`AgentModelsResponse::modes`); the ones Koru knows —
//! Claude Code's, Codex's three, OpenCode's two — are worded here, and any
//! other shows the agent's own name and description with a neutral icon. An
//! agent that lists none has no chip.
//!
//! **Remembered per chat.** The choice is the chat's (`Chat::agent_mode`). A
//! new chat starts from the mode of the last chat with the same agent —
//! never from *Full access*, which is always chosen again by hand.
//!
//! **Shift+Tab** steps to the next mode and says which beside the chip. It
//! skips *Full access*. The composer is asked first, so an outdent would
//! win where there is something to outdent (`koru_keys`' `CycleAgentMode`).
//!
//! A mode chosen while a reply runs applies to the next message, and the
//! note beside the chip says so.

use std::time::{Duration, Instant};

use koru_ai_core::AiProviderId;
use koru_ai_wire::{AgentModeView, UpdateChatAgentSettingsRequest};
use koru_draw::geometry::Rect;
use koru_draw::{icons, scene, theme, theme::role};
use koru_i18n::{Key as Words, fill, t};
use koru_ui::button::ActionButton;
use koru_ui::tooltip::TooltipContent;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::agents::is_agent_provider;
use crate::reader::Command;

/// One row of the list: an icon, a title and a line under it.
pub(crate) const MODE_ROW_H: f32 = 46.0;
/// The air around the rows inside the panel.
const PANEL_PAD: f32 = 4.0;
/// How long the note beside the chip stays.
const NOTE_HOLD: Duration = Duration::from_millis(2200);
/// The note's tooltip key, apart from every hover tooltip's.
const NOTE_KEY: u64 = 0xA9_0D_E5;

/// An agent's modes as its listing carried them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ListedModes {
    pub modes: Vec<AgentModeView>,
    /// The mode the agent reports it starts in: a chat that chose none.
    pub default: Option<String>,
}

/// One mode as the picker shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModeRow {
    pub id: String,
    pub title: String,
    pub help: Option<String>,
    pub icon: char,
    /// *Full access*: drawn in the warning color, never stepped to by
    /// Shift+Tab, never carried into a new chat.
    pub full_access: bool,
}

/// The modes Koru words: the agent's kind, the mode's id, its title, its
/// line, its icon, and whether it is full access.
const KNOWN: &[(&str, &str, Words, Words, char, bool)] = &[
    (
        "agent-claude",
        "default",
        Words::AiAgentModeAskFirst,
        Words::AiAgentModeAskFirstHelp,
        icons::QUESTION,
        false,
    ),
    (
        "agent-claude",
        "acceptEdits",
        Words::AiAgentModeAcceptEdits,
        Words::AiAgentModeAcceptEditsHelp,
        icons::PENCIL,
        false,
    ),
    (
        "agent-claude",
        "plan",
        Words::AiAgentModePlan,
        Words::AiAgentModePlanHelp,
        icons::LIST_CHECKS,
        false,
    ),
    (
        "agent-claude",
        "auto",
        Words::AiAgentModeAuto,
        Words::AiAgentModeAutoHelp,
        icons::SPARKLE,
        false,
    ),
    (
        "agent-claude",
        "bypassPermissions",
        Words::AiAgentModeFullAccess,
        Words::AiAgentModeFullAccessHelp,
        icons::WARNING,
        true,
    ),
    (
        "agent-codex",
        "ask",
        Words::AiAgentModeAskApproval,
        Words::AiAgentModeAskApprovalHelp,
        icons::QUESTION,
        false,
    ),
    (
        "agent-codex",
        "auto-review",
        Words::AiAgentModeApproveForMe,
        Words::AiAgentModeApproveForMeHelp,
        icons::ROBOT,
        false,
    ),
    (
        "agent-codex",
        "full-access",
        Words::AiAgentModeFullAccess,
        Words::AiAgentModeCodexFullAccessHelp,
        icons::WARNING,
        true,
    ),
    (
        "agent-opencode",
        "build",
        Words::AiAgentModeBuild,
        Words::AiAgentModeBuildHelp,
        icons::WRENCH,
        false,
    ),
    (
        "agent-opencode",
        "plan",
        Words::AiAgentModePlan,
        Words::AiAgentModeOpenCodePlanHelp,
        icons::LIST_CHECKS,
        false,
    ),
];

/// Whether `id` is `agent`'s *Full access*, which is never carried over.
pub(crate) fn is_full_access(agent: &AiProviderId, id: &str) -> bool {
    KNOWN
        .iter()
        .any(|known| known.0 == agent.as_str() && known.1 == id && known.5)
}

/// `agent`'s modes as rows, in the agent's order: Koru's words for the ones
/// it knows, the agent's own name and description, and a neutral icon, for
/// the rest.
pub(crate) fn rows(
    lang: koru_i18n::Lang,
    agent: &AiProviderId,
    listed: &ListedModes,
) -> Vec<ModeRow> {
    listed
        .modes
        .iter()
        .map(|mode| {
            match KNOWN
                .iter()
                .find(|known| known.0 == agent.as_str() && known.1 == mode.id)
            {
                Some((_, _, title, help, icon, full_access)) => ModeRow {
                    id: mode.id.clone(),
                    title: t(lang, *title).to_owned(),
                    help: Some(t(lang, *help).to_owned()),
                    icon: *icon,
                    full_access: *full_access,
                },
                None => ModeRow {
                    id: mode.id.clone(),
                    title: mode.name.clone().unwrap_or_else(|| mode.id.clone()),
                    help: mode.description.clone(),
                    icon: icons::FADERS,
                    full_access: false,
                },
            }
        })
        .collect()
}

/// The mode after `current` among the ones Shift+Tab steps through — every
/// mode but *Full access* — wrapping at the end. From *Full access*, or a
/// mode no longer listed, the first.
pub(crate) fn next_mode<'a>(rows: &'a [ModeRow], current: Option<&str>) -> Option<&'a ModeRow> {
    let steps: Vec<&ModeRow> = rows.iter().filter(|row| !row.full_access).collect();
    let at = current.and_then(|current| steps.iter().position(|row| row.id == current));
    match at {
        Some(at) => steps.get((at + 1) % steps.len()).copied(),
        None => steps.first().copied(),
    }
}

impl crate::ChatSurface {
    /// The chat's agent and its listed modes: an agent chat whose agent
    /// lists any. `None` for every other chat.
    fn chat_modes(&self) -> Option<(&AiProviderId, &ListedModes)> {
        let provider = &self.transcript.as_ref()?.chat.current_model.provider;
        if !is_agent_provider(provider) {
            return None;
        }
        let listed = self.agent_modes.get(provider)?;
        (!listed.modes.is_empty()).then_some((provider, listed))
    }

    /// The chat's modes as the picker shows them; empty when it has none.
    pub(crate) fn approval_mode_rows(&self) -> Vec<ModeRow> {
        self.chat_modes()
            .map(|(agent, listed)| rows(self.lang, agent, listed))
            .unwrap_or_default()
    }

    /// The mode the chat's next message runs in: the one it chose when the
    /// agent still lists it, else the agent's default, else its first.
    pub(crate) fn current_approval_mode(&self) -> Option<String> {
        let (_, listed) = self.chat_modes()?;
        let listed_id = |id: &str| listed.modes.iter().any(|mode| mode.id == id);
        self.transcript
            .as_ref()
            .and_then(|transcript| transcript.chat.agent_mode.clone())
            .filter(|id| listed_id(id))
            .or_else(|| listed.default.clone().filter(|id| listed_id(id)))
            .or_else(|| listed.modes.first().map(|mode| mode.id.clone()))
    }

    fn current_approval_row(&self) -> Option<ModeRow> {
        let current = self.current_approval_mode()?;
        self.approval_mode_rows()
            .into_iter()
            .find(|row| row.id == current)
    }

    /// **The mode a new chat with `agent` starts in**: the mode of the last
    /// chat with the same agent, in the chats list's order — unless that is
    /// *Full access*, which is never carried over. `None` is the agent's
    /// own default.
    pub(crate) fn remembered_approval_mode(&self, agent: &AiProviderId) -> Option<String> {
        let last = self
            .chats
            .iter()
            .find(|chat| chat.current_model.provider == *agent)?;
        last.agent_mode
            .clone()
            .filter(|mode| !is_full_access(agent, mode))
    }

    /// The chip, left of the model chip where an API chat has its
    /// Manual/Automatic chip: an agent chat whose agent lists modes.
    pub(crate) fn approval_mode_button(&self) -> Option<Rect> {
        self.chat_modes()?;
        let toolbar = self.composer_toolbar_rect();
        let model = self.chat_model_button();
        let width = self
            .approval_mode_chip_w
            .clamp(72.0, (toolbar.w * 0.4).max(72.0));
        Some(Rect::new(
            (model.x - 6.0 - width).max(toolbar.x),
            toolbar.y,
            width.min((model.x - toolbar.x - 6.0).max(1.0)),
            toolbar.h,
        ))
    }

    /// The open list: above the composer, its right edge on the chip's.
    pub(crate) fn approval_mode_panel(&self) -> Option<Rect> {
        if !self.approval_mode_open {
            return None;
        }
        let chip = self.approval_mode_button()?;
        let count = self.approval_mode_rows().len();
        let width = self.composer_hit_rect.w.min(340.0).max(1.0);
        let height = count as f32 * MODE_ROW_H + PANEL_PAD * 2.0;
        Some(self.above_composer(chip.x + chip.w - width, width, height))
    }

    fn approval_mode_row_rect(&self, panel: Rect, index: usize) -> Rect {
        Rect::new(
            panel.x + PANEL_PAD,
            panel.y + PANEL_PAD + index as f32 * MODE_ROW_H,
            panel.w - PANEL_PAD * 2.0,
            MODE_ROW_H,
        )
    }

    fn approval_mode_row_at(&self, x: f32, y: f32) -> Option<usize> {
        let panel = self.approval_mode_panel()?;
        (0..self.approval_mode_rows().len())
            .find(|index| self.approval_mode_row_rect(panel, *index).contains(x, y))
    }

    /// The hover part for redraws: the chip, and each row of the open list.
    pub(crate) fn approval_mode_hover_at(&self, x: f32, y: f32) -> Option<(u8, usize)> {
        if let Some(index) = self.approval_mode_row_at(x, y) {
            return Some((17, index));
        }
        self.approval_mode_button()
            .filter(|chip| chip.contains(x, y))
            .map(|_| (5, 13))
    }

    pub(crate) fn close_approval_modes(&mut self) -> bool {
        let was = self.approval_mode_open;
        self.approval_mode_open = false;
        self.approval_mode_focus = None;
        was
    }

    /// **Choose a mode for the chat's next message**. Said beside the chip
    /// with `announce` (Shift+Tab), and always while a reply runs, when it
    /// applies to the next message.
    pub(crate) fn choose_approval_mode(&mut self, id: &str, announce: bool) -> bool {
        let Some(row) = self
            .approval_mode_rows()
            .into_iter()
            .find(|row| row.id == id)
        else {
            return false;
        };
        let Some(chat) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.id)
        else {
            return false;
        };
        self.close_approval_modes();
        if self.current_approval_mode().as_deref() != Some(id) {
            // Shown at once; the service's answer confirms it.
            if let Some(transcript) = &mut self.transcript {
                transcript.chat.agent_mode = Some(id.to_owned());
            }
            self.send_command(Command::UpdateAgentSettings(
                chat,
                UpdateChatAgentSettingsRequest {
                    mode: Some(id.to_owned()),
                },
            ));
        }
        if announce || self.attempt.is_some() {
            self.announce_approval_mode(&row);
        }
        true
    }

    /// The short note beside the chip: the mode's name, and while a reply
    /// runs that it applies to the next message.
    fn announce_approval_mode(&mut self, row: &ModeRow) {
        let Some(chip) = self.approval_mode_button() else {
            return;
        };
        let label = if self.attempt.is_some() {
            fill(t(self.lang, Words::AiAgentModeNextMessage), &[&row.title])
        } else {
            row.title.clone()
        };
        self.copy_feedback.show_transient(
            TooltipContent::new(NOTE_KEY, chip, label),
            Instant::now(),
            NOTE_HOLD,
        );
    }

    /// **Shift+Tab**: the next mode, skipping *Full access*. `false` for a
    /// chat with no modes, so the key goes on as it would have.
    pub(crate) fn cycle_approval_mode(&mut self) -> bool {
        let rows = self.approval_mode_rows();
        let current = self.current_approval_mode();
        let Some(next) = next_mode(&rows, current.as_deref()).cloned() else {
            return false;
        };
        self.choose_approval_mode(&next.id, true)
    }

    /// The open list's keys: Up and Down move, Enter chooses, Escape closes.
    /// `None` when the list is shut or the key is not one of them.
    pub(crate) fn approval_modes_key(&mut self, key: &Key, mods: ModifiersState) -> Option<bool> {
        if !self.approval_mode_open || !mods.is_empty() {
            return None;
        }
        let rows = self.approval_mode_rows();
        let current = self.current_approval_mode();
        let focused = self
            .approval_mode_focus
            .or_else(|| {
                rows.iter()
                    .position(|row| Some(&row.id) == current.as_ref())
            })
            .unwrap_or(0);
        match key {
            Key::Named(NamedKey::Escape) => Some(self.close_approval_modes()),
            Key::Named(NamedKey::ArrowDown) if !rows.is_empty() => {
                self.approval_mode_focus = Some((focused + 1) % rows.len());
                Some(true)
            }
            Key::Named(NamedKey::ArrowUp) if !rows.is_empty() => {
                self.approval_mode_focus = Some((focused + rows.len() - 1) % rows.len());
                Some(true)
            }
            Key::Named(NamedKey::Enter) => {
                let id = rows.get(focused).map(|row| row.id.clone())?;
                Some(self.choose_approval_mode(&id, false))
            }
            _ => None,
        }
    }

    /// A press: the chip opens and shuts the list, a row chooses its mode,
    /// and a press anywhere else shuts the list and goes on (`None`).
    pub(crate) fn approval_mode_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        if let Some(index) = self.approval_mode_row_at(x, y) {
            let id = self.approval_mode_rows().get(index)?.id.clone();
            return Some(self.choose_approval_mode(&id, false));
        }
        if self
            .approval_mode_button()
            .is_some_and(|chip| chip.contains(x, y))
        {
            let open = !self.approval_mode_open;
            self.history_open = false;
            self.tool_access_open = false;
            self.tool_mode_open = false;
            if self.chat_model_open {
                self.close_picker();
            }
            self.approval_mode_open = open;
            self.approval_mode_focus = None;
            return Some(true);
        }
        if self.approval_mode_panel().is_some() {
            self.close_approval_modes();
        }
        None
    }

    /// The chip: the mode's icon and name, and a caret, as a ghost button.
    pub(crate) fn draw_approval_mode_chip(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(row) = self.current_approval_row() else {
            return;
        };
        let p = theme::p();
        let icon = self.engine.icon_layout(row.icon, ActionButton::ICON_SIZE);
        let caret = self
            .engine
            .icon_layout(icons::CARET_DOWN, ActionButton::ICON_SIZE);
        let label = self.engine.simple_layout(&row.title, role::row());
        self.approval_mode_chip_w = ActionButton::PAD_X * 2.0
            + icon.width()
            + ActionButton::GAP
            + label.width()
            + ActionButton::GAP
            + caret.width();
        let Some(chip) = self.approval_mode_button() else {
            return;
        };
        let hovered = chip.contains(self.pointer.0, self.pointer.1);
        if hovered || self.approval_mode_open {
            scene::fill(
                canvas,
                root,
                p.hover_tint,
                &scene::rrect(chip.x, chip.y, chip.w, chip.h, ActionButton::RADIUS),
            );
        }
        let colour = if row.full_access { p.notice_fg } else { p.text };
        let mut x = chip.x + ActionButton::PAD_X;
        scene::draw_layout_first_line(
            canvas,
            root,
            &icon,
            x,
            chip.y + (chip.h - icon.height()) * 0.5,
            Some(colour),
        );
        x += icon.width() + ActionButton::GAP;
        let room = (chip.x + chip.w - ActionButton::PAD_X - caret.width() - ActionButton::GAP - x)
            .max(1.0);
        let shown = self.engine.elided(&row.title, role::row(), room);
        let label = self.engine.simple_layout(&shown, role::row());
        scene::draw_layout_first_line(
            canvas,
            root,
            &label,
            x,
            chip.y + (chip.h - label.height()) * 0.5,
            Some(colour),
        );
        scene::draw_layout_first_line(
            canvas,
            root,
            &caret,
            chip.x + chip.w - ActionButton::PAD_X - caret.width(),
            chip.y + (chip.h - caret.height()) * 0.5,
            Some(p.text_dim),
        );
    }

    /// The open list: each mode's icon, title and line, a check on the
    /// current one, *Full access* in the warning color.
    pub(crate) fn draw_approval_mode_picker(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(panel) = self.approval_mode_panel() else {
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
        let rows = self.approval_mode_rows();
        let current = self.current_approval_mode();
        let check = self
            .engine
            .icon_layout(icons::CHECK, ActionButton::ICON_SIZE);
        for (index, row) in rows.iter().enumerate() {
            let rect = self.approval_mode_row_rect(panel, index);
            let focused = self.approval_mode_focus == Some(index);
            if focused || rect.contains(self.pointer.0, self.pointer.1) {
                scene::fill(
                    canvas,
                    root,
                    p.hover_tint,
                    &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
                );
            }
            let colour = if row.full_access { p.notice_fg } else { p.text };
            let icon = self.engine.icon_layout(row.icon, ActionButton::ICON_SIZE);
            scene::draw_layout_first_line(
                canvas,
                root,
                &icon,
                rect.x + 10.0,
                rect.y + 8.0,
                Some(colour),
            );
            let text_x = rect.x + 34.0;
            let room = (rect.x + rect.w - 30.0 - text_x).max(1.0);
            let title = self.engine.elided(&row.title, role::row(), room);
            let title = self.engine.simple_layout(&title, role::row());
            scene::draw_layout_first_line(canvas, root, &title, text_x, rect.y + 5.0, Some(colour));
            if let Some(help) = &row.help {
                let help = self.engine.elided(help, role::meta(), room);
                let help = self.engine.simple_layout(&help, role::meta());
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &help,
                    text_x,
                    rect.y + 5.0 + title.height() + 2.0,
                    Some(p.text_dim),
                );
            }
            if current.as_ref() == Some(&row.id) {
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &check,
                    rect.x + rect.w - 10.0 - check.width(),
                    rect.y + (rect.h - check.height()) * 0.5,
                    Some(p.accent),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
