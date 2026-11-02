// SPDX-License-Identifier: MPL-2.0

//! **The picker's Context row** (koru-ai D10): *Context · Auto | 1M*, above
//! the level's footer, for a chat whose model has a longer context window.
//!
//! An agent may list a model twice, once per window (Claude's `opus` and
//! `opus[1m]`). The runtime folds the two into one model that carries the
//! longer window's id ([`AgentModelView::context`]), so the list shows one
//! *Opus* and this row chooses the window. Choosing only chooses which id
//! the chat takes as its model, through the same update as a row: *Auto*
//! is the model's own id, the other the window's. Nothing here knows how an
//! agent spells either.
//!
//! Like the level, the window is chosen before an agent chat's first
//! message: the agent starts with it, so afterwards the row only shows it.

use koru_ai_core::AiProviderId;
use koru_draw::geometry::Rect;
use koru_draw::{scene, theme};
use koru_i18n::{Key, Lang, fill, number, t};
use koru_ui::Segmented;
use vello::Scene;
use vello::kurbo::Affine;

use super::{Geometry, PickerFocus};
use crate::agents::{find_listed, is_agent_provider};

/// The row's height: a segmented control and room around it.
pub(crate) const CONTEXT_H: f32 = Segmented::H + 6.0;

/// The chat's model's context choice.
pub(crate) struct ContextChoice {
    pub(super) agent: AiProviderId,
    /// The model's own id: *Auto*.
    pub(super) base: String,
    /// The id that runs it with the longer window.
    pub(super) long: String,
    pub(super) tokens: u32,
    /// The chat is on the longer window.
    pub(super) chosen_long: bool,
}

/// A window's size as the row names it: *1M*, or *200k*.
pub(crate) fn window_label(lang: Lang, tokens: u32) -> String {
    let tokens = u64::from(tokens);
    if tokens >= 1_000_000 && tokens % 1_000_000 == 0 {
        fill(
            t(lang, Key::AiModelContextMillions),
            &[&number::decimal(lang, tokens / 1_000_000)],
        )
    } else {
        fill(
            t(lang, Key::AiModelContextThousands),
            &[&number::decimal(lang, tokens / 1_000)],
        )
    }
}

impl crate::ChatSurface {
    /// The chat's model's context choice: an agent chat on a listed model
    /// that has a longer window. `None` for every other chat and model.
    pub(crate) fn picker_context(&self) -> Option<ContextChoice> {
        let model = &self.transcript.as_ref()?.chat.current_model;
        if !is_agent_provider(&model.provider) {
            return None;
        }
        let (listed, chosen_long) = find_listed(
            self.listed_agent_models(&model.provider),
            &model.requested_model,
        )?;
        let context = listed.context.as_ref()?;
        Some(ContextChoice {
            agent: model.provider.clone(),
            base: listed.id.clone(),
            long: context.id.clone(),
            tokens: context.tokens,
            chosen_long,
        })
    }

    /// Choose the window: the longer one when `long`, else *Auto*. Read
    /// only once the chat has begun, as the level is.
    pub(crate) fn choose_context(&mut self, long: bool) -> bool {
        let Some(choice) = self.picker_context() else {
            return false;
        };
        if self.effort_locked() {
            self.status = Some(t(self.lang, Key::AiModelContextLockedAfterStart).to_owned());
            return true;
        }
        if choice.chosen_long == long {
            return true;
        }
        let id = if long { choice.long } else { choice.base };
        self.choose_agent_model(choice.agent, id)
    }

    /// The row's two words and its control, laid out at the row's end.
    pub(super) fn context_control(&mut self, row: Rect, tokens: u32) -> ([String; 2], Segmented) {
        let labels = [
            t(self.lang, Key::AiModelContextAuto).to_owned(),
            window_label(self.lang, tokens),
        ];
        let words = [labels[0].as_str(), labels[1].as_str()];
        let width = Segmented::width(&mut self.engine, &words);
        let control = Segmented::layout(
            &mut self.engine,
            &words,
            row.x + row.w - width - 4.0,
            row.y + (row.h - Segmented::H) * 0.5,
        );
        (labels, control)
    }

    /// A press on the row: a choice is chosen. `None` when the press is
    /// not on the row.
    pub(crate) fn context_mouse_down(
        &mut self,
        geometry: &Geometry,
        x: f32,
        y: f32,
    ) -> Option<bool> {
        let row = geometry.context.filter(|row| row.contains(x, y))?;
        let choice = self.picker_context()?;
        let (_, control) = self.context_control(row, choice.tokens);
        self.picker.focus = PickerFocus::Context;
        Some(match control.segment_at(x, y) {
            Some(segment) => self.choose_context(segment == 1),
            None => true,
        })
    }

    /// Draw the row: a rule above it, *Context*, and the two choices.
    pub(crate) fn draw_context_row(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        geometry: &Geometry,
    ) {
        let (Some(row), Some(choice)) = (geometry.context, self.picker_context()) else {
            return;
        };
        let p = theme::p();
        let locked = self.effort_locked();
        scene::fill(
            canvas,
            root,
            p.panel_border,
            &scene::rrect(row.x, row.y, row.w, 1.0, 0.5),
        );
        let title = self
            .engine
            .simple_layout(t(self.lang, Key::AiModelContext), theme::role::row());
        scene::draw_layout_first_line(
            canvas,
            root,
            &title,
            row.x + 8.0,
            row.y + (row.h - title.height()) * 0.5,
            Some(if locked { p.text_dim } else { p.text }),
        );
        let (labels, control) = self.context_control(row, choice.tokens);
        let words = [labels[0].as_str(), labels[1].as_str()];
        let hovered = if locked {
            None
        } else {
            control.segment_at(self.pointer.0, self.pointer.1)
        };
        control.draw(
            canvas,
            root,
            &mut self.engine,
            &words,
            Some(usize::from(choice.chosen_long)),
            hovered,
            if locked { 0.5 } else { 1.0 },
        );
        if self.picker.focus == PickerFocus::Context {
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
}
