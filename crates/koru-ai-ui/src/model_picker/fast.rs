// SPDX-License-Identifier: MPL-2.0

//! **The picker's Fast row** (koru-ai D11): *Fast · Off | On*, in the
//! footer, for an agent chat whose model offers its agent's Fast mode —
//! Claude Code's `fast` option, Codex's *Fast* service tier. A model without
//! one has no row. There is no lightning icon in Koru's set, so the row is
//! its word and its switch.
//!
//! Unlike the level and the context window, Fast mode can change after the
//! chat has begun: the agent takes it with the next message (Claude's
//! wrapper sets it live; Codex takes a tier per turn). The choice is the
//! chat's (`Chat::agent_fast`); one never made reads as the model's own
//! default.

use koru_ai_core::AiProviderId;
use koru_ai_wire::UpdateChatAgentSettingsRequest;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::Palette;
use koru_draw::{scene, theme};
use koru_i18n::{Key, Lang, t};
use koru_ui::Segmented;
use vello::Scene;
use vello::kurbo::Affine;

use super::{Geometry, PickerFocus};
use crate::agents::{find_listed, is_agent_provider};
use crate::reader::Command;

/// The row's height: a segmented control and room around it.
pub(crate) const FAST_H: f32 = Segmented::H + 6.0;

/// The chat's model's Fast mode.
pub(crate) struct FastChoice {
    pub(super) agent: AiProviderId,
    /// Fast mode is on for the chat's next message.
    pub(super) on: bool,
}

impl crate::ChatSurface {
    /// The chat's model's Fast mode: an agent chat on a listed model that
    /// offers one. `None` for every other chat and model.
    pub(crate) fn picker_fast(&self) -> Option<FastChoice> {
        let chat = &self.transcript.as_ref()?.chat;
        let model = &chat.current_model;
        if !is_agent_provider(&model.provider) {
            return None;
        }
        let (listed, _) = find_listed(
            self.listed_agent_models(&model.provider),
            &model.requested_model,
        )?;
        let fast = listed.fast?;
        Some(FastChoice {
            agent: model.provider.clone(),
            on: chat.agent_fast.unwrap_or(fast.on_by_default),
        })
    }

    /// Turn Fast mode on or off for the chat's next message.
    pub(crate) fn choose_fast(&mut self, on: bool) -> bool {
        let Some(choice) = self.picker_fast() else {
            return false;
        };
        if choice.on == on {
            return true;
        }
        let Some(transcript) = self.transcript.as_mut() else {
            return false;
        };
        transcript.chat.agent_fast = Some(on);
        let chat = transcript.chat.id;
        tracing::debug!(target: "koru_ai::ui", agent = %choice.agent, on, "fast mode chosen");
        self.send_command(Command::UpdateAgentSettings(
            chat,
            UpdateChatAgentSettingsRequest {
                mode: None,
                fast: Some(on),
            },
        ))
    }

    /// The row's two words and its control, laid out at the row's end.
    pub(super) fn fast_control(&mut self, row: Rect) -> ([String; 2], Segmented) {
        fast_control(&mut self.engine, self.lang, row)
    }

    /// A press on the row: Off or On is chosen. `None` when the press is
    /// not on the row.
    pub(crate) fn fast_mouse_down(&mut self, geometry: &Geometry, x: f32, y: f32) -> Option<bool> {
        let row = geometry.fast.filter(|row| row.contains(x, y))?;
        self.picker_fast()?;
        let (_, control) = self.fast_control(row);
        self.picker.focus = PickerFocus::Fast;
        Some(match control.segment_at(x, y) {
            Some(segment) => self.choose_fast(segment == 1),
            None => true,
        })
    }
}

/// The row's two words and its control, laid out at the end of `row`.
pub(super) fn fast_control(
    engine: &mut TextEngine,
    lang: Lang,
    row: Rect,
) -> ([String; 2], Segmented) {
    let labels = [
        t(lang, Key::AiModelFastOff).to_owned(),
        t(lang, Key::AiModelFastOn).to_owned(),
    ];
    let words = [labels[0].as_str(), labels[1].as_str()];
    let width = Segmented::width(engine, &words);
    let control = Segmented::layout(
        engine,
        &words,
        row.x + row.w - width - 4.0,
        row.y + (row.h - Segmented::H) * 0.5,
    );
    (labels, control)
}

/// What the Fast row shows: whether Fast mode is on, where the pointer is
/// and whether the keyboard is on the row.
pub(super) struct FastPaint {
    pub(super) lang: Lang,
    pub(super) on: bool,
    pub(super) pointer: (f32, f32),
    pub(super) focused: bool,
}

/// Paint the row in `row`, the allocation the picker's element frame gave
/// it, in its palette `p`: a rule above it, *Fast*, and Off | On.
pub(super) fn paint_fast_row(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    row: Rect,
    fast: &FastPaint,
    p: &Palette,
) {
    scene::fill(
        canvas,
        root,
        p.panel_border,
        &scene::rrect(row.x, row.y, row.w, 1.0, 0.5),
    );
    let title = engine.simple_layout(t(fast.lang, Key::AiModelFast), theme::role::row());
    scene::draw_layout_first_line(
        canvas,
        root,
        &title,
        row.x + 8.0,
        row.y + (row.h - title.height()) * 0.5,
        Some(p.text),
    );
    let (labels, control) = fast_control(engine, fast.lang, row);
    let words = [labels[0].as_str(), labels[1].as_str()];
    let hovered = control.segment_at(fast.pointer.0, fast.pointer.1);
    control.draw(
        canvas,
        root,
        engine,
        &words,
        Some(usize::from(fast.on)),
        hovered,
        1.0,
    );
    if fast.focused {
        let bounds = control.bounds;
        scene::stroke(
            canvas,
            root,
            p.accent,
            1.0,
            &scene::rrect(bounds.x, bounds.y, bounds.w, bounds.h, 7.0),
        );
    }
}
