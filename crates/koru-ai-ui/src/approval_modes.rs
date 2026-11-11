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
//! new chat starts from the person's **default permission** (Settings →
//! Coding → Agent providers, D39): *Manual*, *Auto* or *Yolo* for an agent
//! with an auto mode, *Manual* or *Yolo* for any other, mapped onto the
//! agent's own modes by [`default_mode`] — the one place that mapping is
//! made. A change inside a chat stays that chat's. *Yolo* is the only way a
//! new chat starts in *Full access*: the person chose it there.
//!
//! **Shift+Tab** steps to the next mode, which the chip then names. It
//! skips *Full access*. The composer is asked first, so an outdent would
//! win where there is something to outdent (`koru_keys`' `CycleAgentMode`).
//! No note repeats the mode's name beside the chip (D65, the user: *the name
//! is clear*): the chip's label already reads it.
//!
//! A mode chosen while a reply runs applies to the next message, and the
//! note beside the chip says so — the one thing the chip does not.

use std::time::{Duration, Instant};

use koru_agent_config_ui::modes as cfg;
use koru_ai_core::AiProviderId;
use koru_ai_wire::{AgentModeView, UpdateChatAgentSettingsRequest};
use koru_draw::geometry::Rect;
use koru_i18n::{Key as Words, fill, t};
use koru_ui::split_button::SplitTone;
use koru_ui::tooltip::TooltipContent;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::Focus;
use crate::agents::is_agent_provider;
use crate::reader::Command;

// Read only by the suite, through `use super::*`, since the mode table
// moved; it goes when the suite moves into the control's crate.
#[cfg(test)]
use koru_draw::el::Frame;
#[cfg(test)]
use koru_draw::icons;

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

// **The modes' vocabulary is the agent-configuration control's**
// (`koru_agent_config_ui::modes`, Bots P1.8): the rows, Koru's words for the
// agents' own modes, what each is for, a new chat's mode and Shift+Tab's
// order. The chat names them here over its own types — the provider id and
// the listing it keeps — so this crate's code and its suite read as they did.
pub use cfg::NewChatPermission;
pub(crate) use cfg::{MODE_ROW_H, ModeList, ModeRow, next_mode};
// The list's sizes and the modes' roles, which only the suite reads now.
#[cfg(test)]
pub(crate) use cfg::Role;
#[cfg(test)]
use cfg::view::{FOOTER_H, PANEL_PAD};

/// `listed`'s modes as the control takes them, borrowed.
fn offers(listed: &ListedModes) -> Vec<cfg::ModeOffer<'_>> {
    listed
        .modes
        .iter()
        .map(|mode| cfg::ModeOffer {
            id: &mode.id,
            name: mode.name.as_deref(),
            description: mode.description.as_deref(),
        })
        .collect()
}

/// `id`'s role for `agent` ([`cfg::role_of`]).
#[cfg(test)]
pub(crate) fn role_of(agent: &AiProviderId, id: &str) -> Role {
    cfg::role_of(agent.as_str(), id)
}

/// **A mode's color** ([`cfg::tone_of`], the one place it is decided): the
/// send button wears it, and the list's row for the mode wears it on its
/// icon — the control fills it into every row it words ([`cfg::rows`]).
pub(crate) fn tone_of(agent: &AiProviderId, id: &str) -> SplitTone {
    cfg::tone_of(agent.as_str(), id)
}

/// Whether `id` is `agent`'s *Full access* ([`cfg::is_full_access`]).
#[cfg(test)]
pub(crate) fn is_full_access(agent: &AiProviderId, id: &str) -> bool {
    cfg::is_full_access(agent.as_str(), id)
}

/// Whether `agent` offers a mode that approves safe actions on its own
/// ([`cfg::has_auto_mode`]).
#[cfg(test)]
pub(crate) fn has_auto_mode(agent: &AiProviderId, listed: &ListedModes) -> bool {
    cfg::has_auto_mode(agent.as_str(), &offers(listed))
}

/// **The mode a new chat with `agent` starts in** ([`cfg::default_mode`]):
/// the person's two default permissions mapped onto the agent's own modes.
pub(crate) fn default_mode(
    agent: &AiProviderId,
    listed: &ListedModes,
    supported: NewChatPermission,
    other: NewChatPermission,
) -> Option<String> {
    cfg::default_mode(
        agent.as_str(),
        &offers(listed),
        listed.default.as_deref(),
        supported,
        other,
    )
}

/// `agent`'s modes as rows, in the agent's order ([`cfg::rows`]).
pub(crate) fn rows(
    lang: koru_i18n::Lang,
    agent: &AiProviderId,
    listed: &ListedModes,
) -> Vec<ModeRow> {
    cfg::rows(lang, agent.as_str(), &offers(listed))
}

/// How long after an agent's listing came without modes it is asked again.
const MODES_ASK_AGAIN: Duration = Duration::from_secs(2);
/// How many times, at most, per agent: the probe that reads them takes
/// seconds, up to about fifteen for the slowest agent, and an agent that
/// offers none is not asked forever.
const MODES_ASKS: u8 = 8;

impl crate::ChatSurface {
    /// The agent of the chat on screen, for an agent chat.
    fn shown_agent(&self) -> Option<&AiProviderId> {
        let provider = &self.transcript.as_ref()?.chat.current_model.provider;
        is_agent_provider(provider).then_some(provider)
    }

    /// **An agent's modes can come after its models.** The session host
    /// keeps an agent's modes in memory only, read by its probe's session
    /// start, while its models come from the list it kept on disk at once.
    /// A listing answered before that probe — the one asked as the window
    /// opens, against a host that has just started — carries no modes, and
    /// the chip was missing until the model picker happened to ask again.
    /// So while an agent chat is shown and its agent's modes are not known,
    /// its listing is asked again every [`MODES_ASK_AGAIN`], at most
    /// [`MODES_ASKS`] times; the host answers from memory.
    pub(crate) fn follow_agent_modes(&mut self, now: Instant) {
        let Some(agent) = self.modes_owed() else {
            return;
        };
        if self
            .modes_asked
            .get(&agent)
            .is_some_and(|(_, last)| now.duration_since(*last) < MODES_ASK_AGAIN)
        {
            return;
        }
        let asked = self.modes_asked.entry(agent.clone()).or_insert((0, now));
        *asked = (asked.0 + 1, now);
        self.ask_agent_models(agent);
    }

    /// When [`Self::follow_agent_modes`] asks next, if it will.
    pub(crate) fn modes_deadline(&self) -> Option<Instant> {
        let agent = self.modes_owed()?;
        Some(
            self.modes_asked
                .get(&agent)
                .map_or_else(Instant::now, |(_, last)| *last + MODES_ASK_AGAIN),
        )
    }

    /// The shown chat's agent, while its modes are owed: not known, no ask
    /// out for them, and asks left.
    fn modes_owed(&self) -> Option<AiProviderId> {
        let agent = self.shown_agent()?;
        let owed = !self.agent_modes.contains_key(agent)
            && !self.agent_models_asked.contains(agent)
            && self
                .modes_asked
                .get(agent)
                .is_none_or(|(count, _)| *count < MODES_ASKS);
        owed.then(|| agent.clone())
    }

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

    /// The selected mode's badge and send-button tone. Codex's automatic
    /// review and the agents' accept-edits modes approve safe work, while a
    /// plain Auto mode stays neutral. Unknown modes keep the accent role.
    pub(crate) fn approval_presentation(&self) -> Option<(ModeRow, SplitTone)> {
        let (agent, _) = self.chat_modes()?;
        let row = self.current_approval_row()?;
        let tone = tone_of(agent, &row.id);
        Some((row, tone))
    }

    /// **The mode a new chat with `agent` starts in**: the person's default
    /// permission mapped onto the agent's own modes ([`default_mode`]) —
    /// not the mode it was last used in. `None` until the agent has listed
    /// its modes; the chat takes its default when they arrive
    /// ([`Self::adopt_default_mode`]).
    pub(crate) fn default_approval_mode(&self, agent: &AiProviderId) -> Option<String> {
        let listed = self.agent_modes.get(agent)?;
        default_mode(
            agent,
            listed,
            self.agent_defaults.permission,
            self.agent_defaults.permission_other,
        )
    }

    /// A new chat not made yet whose agent had not listed its modes when it
    /// was begun takes its default now that they have arrived — unless a
    /// mode was chosen on it meanwhile.
    pub(crate) fn adopt_default_mode(&mut self, agent: &AiProviderId) {
        if !self.chat_unmade() {
            return;
        }
        let mode = self.default_approval_mode(agent);
        if let Some(transcript) = self.transcript.as_mut()
            && transcript.chat.current_model.provider == *agent
            && transcript.chat.agent_mode.is_none()
        {
            transcript.chat.agent_mode = mode;
        }
    }

    /// The split send button's menu target when an agent lists modes.
    pub(crate) fn approval_mode_button(&self) -> Option<Rect> {
        self.chat_modes()?;
        if self.send_mode() == crate::queue::SendMode::Stop {
            return None;
        }
        self.composer_shell.as_ref()?.mode()
    }

    /// **The open list, as the control lays it out** (`ModeList`): the
    /// chat's rows, its mode, the keyboard's row and the split button that
    /// opened it. `None` while the list is shut or has no button.
    fn with_mode_list<R>(&self, f: impl FnOnce(&ModeList<'_>) -> R) -> Option<R> {
        if !self.approval_mode_open {
            return None;
        }
        let anchor = self.approval_mode_button()?;
        let rows = self.approval_mode_rows();
        let current = self.current_approval_mode();
        let list = ModeList {
            lang: self.lang,
            rows: &rows,
            current: current.as_deref(),
            focus: self.approval_mode_focus,
            anchor,
            viewport: self.viewport,
            width: self.composer_hit_rect.w,
            pointer: self.pointer,
            footer: true,
        };
        Some(f(&list))
    }

    /// The open list follows the split send control that opened it.
    pub(crate) fn approval_mode_panel(&self) -> Option<Rect> {
        self.with_mode_list(|list| list.panel())
    }

    #[cfg(test)]
    fn approval_mode_footer_rect(&self) -> Option<Rect> {
        self.with_mode_list(|list| list.footer_rect())
    }

    fn approval_mode_default_rect(&self) -> Option<Rect> {
        self.with_mode_list(|list| list.default_rect())
    }

    fn change_approval_default(&mut self) -> bool {
        self.close_approval_modes();
        self.focus = Focus::Composer;
        self.ask_for_agent_settings();
        true
    }

    /// **The open list's frame** (D105): where a press lands and which row
    /// it is — what is drawn, laid out without the pointer.
    #[cfg(test)]
    fn approval_mode_frame(&self) -> Option<(Vec<elements::ModeSpec>, Frame)> {
        self.with_mode_list(|list| list.frame())
    }

    /// Where a screen reader's focus is in the open list: what a host that
    /// publishes it publishes. The chat publishes no tree of its own.
    #[cfg(test)]
    fn approval_mode_accessibility_focus(&self) -> Option<koru_draw::el::Key> {
        self.with_mode_list(|list| list.focused_key()).flatten()
    }

    /// Row `index` where it is drawn.
    #[cfg(test)]
    pub(crate) fn approval_mode_row_rect(&self, index: usize) -> Option<Rect> {
        self.with_mode_list(|list| list.row_rect(index)).flatten()
    }

    /// The row a press or the pointer at `x`, `y` is on, from the frame's
    /// targets.
    fn approval_mode_row_at(&self, x: f32, y: f32) -> Option<usize> {
        self.with_mode_list(|list| list.row_at(x, y)).flatten()
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

    /// **Choose a mode for the chat's next message**. The chip names it; a
    /// note beside the chip speaks only while a reply runs, to say it
    /// applies to the next message (D65).
    pub(crate) fn choose_approval_mode(&mut self, id: &str) -> bool {
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
        self.focus = Focus::Composer;
        if self.current_approval_mode().as_deref() != Some(id) {
            // Shown at once; the service's answer confirms it.
            if let Some(transcript) = &mut self.transcript {
                transcript.chat.agent_mode = Some(id.to_owned());
            }
            self.send_command(Command::UpdateAgentSettings(
                chat,
                UpdateChatAgentSettingsRequest {
                    mode: Some(id.to_owned()),
                    fast: None,
                },
            ));
        }
        if self.attempt.is_some() {
            self.announce_next_message(&row);
        }
        true
    }

    /// The short note beside the chip while a reply runs: the mode applies
    /// to the next message. With no reply running there is none — the
    /// chip's label already names the mode (D65).
    fn announce_next_message(&mut self, row: &ModeRow) {
        let chip = self
            .approval_mode_button()
            .unwrap_or_else(|| self.send_button());
        let label = fill(t(self.lang, Words::AiAgentModeNextMessage), &[&row.title]);
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
        self.choose_approval_mode(&next.id)
    }

    /// The open list's keys: Up and Down move, Enter chooses, Escape closes.
    /// `None` when the list is shut or the key is not one of them.
    pub(crate) fn approval_modes_key(&mut self, key: &Key, mods: ModifiersState) -> Option<bool> {
        if !self.approval_mode_open || !mods.is_empty() {
            return None;
        }
        let rows = self.approval_mode_rows();
        let count = rows.len() + 1; // The footer's Change default is the last stop.
        let current = self.current_approval_mode();
        let focused = self
            .approval_mode_focus
            .or_else(|| {
                rows.iter()
                    .position(|row| Some(&row.id) == current.as_ref())
            })
            .unwrap_or(0);
        match key {
            Key::Named(NamedKey::Escape) => {
                let closed = self.close_approval_modes();
                self.focus = Focus::ModeChevron;
                Some(closed)
            }
            Key::Named(NamedKey::ArrowDown) if !rows.is_empty() => {
                self.approval_mode_focus = Some((focused + 1) % count);
                Some(true)
            }
            Key::Named(NamedKey::ArrowUp) if !rows.is_empty() => {
                self.approval_mode_focus = Some((focused + count - 1) % count);
                Some(true)
            }
            Key::Named(NamedKey::Tab) => {
                if focused == rows.len() {
                    self.close_approval_modes();
                    self.focus = Focus::Composer;
                } else {
                    self.approval_mode_focus = Some(rows.len());
                }
                Some(true)
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if focused == rows.len() {
                    return Some(self.change_approval_default());
                }
                let id = rows.get(focused).map(|row| row.id.clone())?;
                Some(self.choose_approval_mode(&id))
            }
            _ => None,
        }
    }

    /// A press: the menu target opens and shuts the list, a row chooses its mode,
    /// and a press anywhere else shuts the list and goes on (`None`).
    pub(crate) fn approval_mode_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        if self
            .approval_mode_default_rect()
            .is_some_and(|button| button.contains(x, y))
        {
            return Some(self.change_approval_default());
        }
        if let Some(index) = self.approval_mode_row_at(x, y) {
            let id = self.approval_mode_rows().get(index)?.id.clone();
            return Some(self.choose_approval_mode(&id));
        }
        if self
            .approval_mode_button()
            .is_some_and(|chip| chip.contains(x, y))
        {
            let open = !self.approval_mode_open;
            self.tool_access_open = false;
            self.tool_mode_open = false;
            if self.chat_model_open {
                self.close_picker();
            }
            self.approval_mode_open = open;
            self.approval_mode_focus = None;
            self.focus = Focus::ModeChevron;
            return Some(true);
        }
        if self.approval_mode_panel().is_some() {
            self.close_approval_modes();
        }
        None
    }

    /// **The open list** (D105), drawn by the control (`ModeList::draw`)
    /// inside the panel's `FloatingPanel` record.
    pub(crate) fn draw_approval_mode_picker(&mut self, canvas: &mut Scene, root: Affine) {
        if !self.approval_mode_open {
            return;
        }
        let Some(anchor) = self.approval_mode_button() else {
            return;
        };
        let rows = self.approval_mode_rows();
        let current = self.current_approval_mode();
        let list = ModeList {
            lang: self.lang,
            rows: &rows,
            current: current.as_deref(),
            focus: self.approval_mode_focus,
            anchor,
            viewport: self.viewport,
            width: self.composer_hit_rect.w,
            pointer: self.pointer,
            footer: true,
        };
        list.draw(canvas, root, &mut self.engine);
    }
}

mod elements;
#[cfg(test)]
mod pins;
#[cfg(test)]
mod tests;
