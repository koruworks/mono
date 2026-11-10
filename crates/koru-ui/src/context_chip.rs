// SPDX-License-Identifier: MPL-2.0

//! A removable context chip in a composer. It composes the shared label
//! chip's measure, paint and remove lane; the host owns the document or
//! attachment identity and decides what removal means.

use koru_draw::geometry::Rect;
use koru_draw::scene;
use koru_draw::text::TextEngine;
use vello::Scene;
use vello::kurbo::Affine;

use crate::label_chip::{LabelChip, Lead, Size, State, Tone, Trail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextState {
    Ready,
    Pending,
    Refused,
}

#[derive(Clone, Copy, Debug)]
pub struct ContextChip<'a> {
    pub label: &'a str,
    pub secondary: &'a str,
    pub icon: char,
    pub state: ContextState,
}

impl<'a> ContextChip<'a> {
    fn label_chip(self) -> LabelChip<'a> {
        let chip = LabelChip::new(self.label);
        let chip = if self.secondary.is_empty() {
            chip
        } else {
            chip.secondary(self.secondary)
        };
        chip.size(Size::Regular)
            .lead(Lead::Glyph(self.icon))
            .trail(Trail::Remove)
            .tone(match self.state {
                ContextState::Ready | ContextState::Pending => Tone::Neutral,
                ContextState::Refused => Tone::Accent("red"),
            })
    }

    pub fn height(self) -> f32 {
        self.label_chip().height()
    }

    /// The complete rect's width, capped to the host's available line.
    pub fn width(self, engine: &mut TextEngine, max_width: f32) -> f32 {
        self.label_chip().width(engine).min(max_width.max(1.0))
    }

    pub fn remove_rect(self, rect: Rect) -> Rect {
        self.label_chip()
            .trail_rect(rect)
            .expect("context chips always have a remove lane")
    }

    #[track_caller]
    pub fn draw(
        self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        pointer: (f32, f32),
    ) {
        let _inspection = koru_draw::inspect::scope(
            "KoruContextChip",
            "component",
            root,
            scene::rect(rect.x, rect.y, rect.w, rect.h),
            |r| {
                r.property("state", format!("{:?}", self.state), "instance");
                r.property("remove lane", true, "shared label-chip geometry");
                r.property("secondary", !self.secondary.is_empty(), "instance");
                r.property("ink", "chip tokens", "theme");
            },
        );
        self.label_chip().draw(
            canvas,
            root,
            engine,
            rect,
            State {
                hovered: rect.contains(pointer.0, pointer.1),
                reveal: true,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_context_name_is_bounded_and_remove_stays_inside() {
        let mut engine = TextEngine::new();
        let chip = ContextChip {
            label: "A long Unicode document title 日本語 budget notes",
            secondary: "Another workspace",
            icon: koru_draw::icons::DOCUMENT,
            state: ContextState::Pending,
        };
        let width = chip.width(&mut engine, 120.0);
        assert_eq!(width, 120.0);
        let rect = Rect::new(10.0, 4.0, width, chip.height());
        let remove = chip.remove_rect(rect);
        assert!(remove.x >= rect.x && remove.x + remove.w <= rect.x + rect.w);
        assert_eq!(remove.h, rect.h);
    }

    #[test]
    fn an_empty_secondary_keeps_the_chip_on_one_line() {
        let chip = ContextChip {
            label: "notes.pdf",
            secondary: "",
            icon: koru_draw::icons::FILE,
            state: ContextState::Ready,
        };
        assert_eq!(chip.height(), crate::label_chip::Size::Regular.height());
    }
}
