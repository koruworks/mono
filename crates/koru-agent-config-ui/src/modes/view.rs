// SPDX-License-Identifier: MPL-2.0

//! **The approval-mode list, laid out and drawn** (koru-ai D7, D105): the
//! open list's panel above the control that opened it, its rows as the
//! element tree ([`super::elements`]), and its footer — *Applies to this
//! chat* and *Change default* — painted as it always was. A host hands it
//! the rows, the current mode, the keyboard's row and where it opens, and
//! keeps the choosing (what a mode means, its keys, Shift+Tab) its own.
//!
//! The footer is imperative paint with raw lengths and the global palette,
//! moved byte for byte (P1.8.PICKER.notes.md R10): redrawing it as
//! elements is a change a person sees in F12, for its own review. So this
//! file is not on `ui_scan`'s migrated list; `elements.rs` is.

use koru_draw::el::{Frame, FrameCx, Interaction};
use koru_draw::geometry::Rect;
use koru_draw::inspect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme, theme::role};
use koru_i18n::{Key as Words, Lang, t};
use koru_ui::popover::{PopoverAlign, PopoverShell};
use vello::Scene;
use vello::kurbo::Affine;

use super::{MODE_ROW_H, ModeRow, elements};

/// The air around the rows inside the panel.
pub const PANEL_PAD: f32 = 4.0;
/// The panel's corners, as the composer's other panels have them.
pub const PANEL_RADIUS: f32 = 8.0;
/// The footer under the rows: *Applies to this chat*, *Change default*.
pub const FOOTER_H: f32 = 38.0;

/// **The open list**, as its host has it this call.
pub struct ModeList<'a> {
    pub lang: Lang,
    /// The agent's modes as rows ([`super::rows`]).
    pub rows: &'a [ModeRow],
    /// The mode the next message runs in.
    pub current: Option<&'a str>,
    /// The row the keyboard is on; one past the last is the footer's
    /// *Change default*.
    pub focus: Option<usize>,
    /// The control that opened the list.
    pub anchor: Rect,
    pub viewport: Rect,
    /// The composer's width: the list is as wide, up to 340.
    pub width: f32,
    pub pointer: (f32, f32),
    /// Paint the chat's footer — *Applies to this chat* and *Change
    /// default* — under the rows. A host whose choice is not a chat's (a
    /// Bot's next conversations) leaves it out and says what the choice
    /// applies to itself.
    pub footer: bool,
}

impl ModeList<'_> {
    /// The panel: above the control that opened it, end-aligned with it.
    pub fn panel(&self) -> Rect {
        let count = self.rows.len();
        let width = self.width.min(340.0).max(1.0);
        let footer = if self.footer { FOOTER_H } else { 0.0 };
        let height = count as f32 * MODE_ROW_H + PANEL_PAD * 2.0 + footer;
        PopoverShell::anchored(self.anchor, self.viewport, width, height, PopoverAlign::End).rect
    }

    /// The footer across the panel's foot.
    pub fn footer_rect(&self) -> Rect {
        let panel = self.panel();
        Rect::new(panel.x, panel.y + panel.h - FOOTER_H, panel.w, FOOTER_H)
    }

    /// *Change default*: the footer's end half.
    pub fn default_rect(&self) -> Rect {
        let footer = self.footer_rect();
        Rect::new(
            footer.x + footer.w * 0.5,
            footer.y,
            footer.w * 0.5,
            footer.h,
        )
    }

    /// What the open list's tree declares (D105): a row a mode, the
    /// current one's ✓, the keyboard's row lit.
    pub fn specs(&self) -> Vec<elements::ModeSpec> {
        self.rows
            .iter()
            .enumerate()
            .map(|(index, row)| elements::ModeSpec {
                key: elements::row_key(&row.id),
                icon: row.icon,
                tone: row.tone,
                current: self.current == Some(row.id.as_str()),
                lit: self.focus == Some(index),
                full_access: row.full_access,
                title: row.title.clone(),
                help: row.help.clone(),
                spoken: match &row.help {
                    Some(help) => koru_i18n::fill(
                        t(self.lang, Words::AiPickerRowWith),
                        &[row.title.as_str(), help.as_str()],
                    ),
                    None => row.title.clone(),
                },
            })
            .collect()
    }

    /// **Where a screen reader's focus is** while the list is open: the
    /// row the keyboard is on. The footer's *Change default* is painted,
    /// not an element, so it is not published (P1.8.PICKER.notes.md R10).
    pub fn focused_key(&self) -> Option<koru_draw::el::Key> {
        self.rows
            .get(self.focus?)
            .map(|row| elements::row_key(&row.id))
    }

    /// **The open list's frame** (D105): where a press lands and which row
    /// it is — what is drawn, laid out without the pointer.
    pub fn frame(&self) -> (Vec<elements::ModeSpec>, Frame) {
        let specs = self.specs();
        let frame = elements::layout(&elements::tree(&specs), self.panel(), FrameCx::root());
        (specs, frame)
    }

    /// Row `index` where it is drawn.
    pub fn row_rect(&self, index: usize) -> Option<Rect> {
        let (_, frame) = self.frame();
        Some(frame.named(elements::MODE_ROW).nth(index)?.rect)
    }

    /// The row a press or the pointer at `x`, `y` is on, from the frame's
    /// targets.
    pub fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        let (specs, frame) = self.frame();
        let key = frame.hit_at(x, y)?;
        specs.iter().position(|spec| spec.key == key)
    }

    /// **The open list** (D105): its element tree — each mode's icon,
    /// title and line, the ✓ on the current one, *Full access* in the
    /// notice ink — laid out with the pointer and the keyboard on it and
    /// painted while the panel's `FloatingPanel` record is open, so every
    /// row, icon, word and ✓ nests under the panel in the inspector.
    pub fn draw(&self, canvas: &mut Scene, root: Affine, engine: &mut TextEngine) {
        let panel = self.panel();
        let tree = elements::tree(&self.specs());
        // The host's pointer, by key: the frame's own target under it.
        let hot =
            elements::layout(&tree, panel, FrameCx::root()).hit_at(self.pointer.0, self.pointer.1);
        let frame = elements::layout(
            &tree,
            panel,
            FrameCx::root().with_interaction(Interaction {
                hot,
                ..Interaction::default()
            }),
        );
        let _panel = scene::floating_panel_scope(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(PANEL_RADIUS),
        );
        frame.paint(&tree, canvas, root, engine);
        if self.footer {
            let footer = self.footer_rect();
            let _footer = inspect::scope(
                "ApprovalModeFooter",
                "part",
                root,
                scene::rect(footer.x, footer.y, footer.w, footer.h),
                |record| record.property("scope", "this chat", "mode persistence"),
            );
            let p = theme::p();
            scene::fill(
                canvas,
                root,
                p.panel_border,
                &scene::rect(
                    footer.x + 8.0,
                    footer.y,
                    footer.w - 16.0,
                    theme::BORDER_W as f32,
                ),
            );
            let label = t(self.lang, Words::AiAgentModeAppliesThisChat);
            let label = engine.elided(label, role::meta(), (footer.w * 0.5 - 16.0).max(1.0));
            let label = engine.simple_layout(&label, role::meta());
            scene::draw_layout_first_line(
                canvas,
                root,
                &label,
                footer.x + 8.0,
                footer.y + (footer.h - label.height()) * 0.5,
                Some(p.text_dim),
            );
            {
                let button = self.default_rect();
                let focused = self.focus == Some(self.rows.len());
                if focused || button.contains(self.pointer.0, self.pointer.1) {
                    scene::fill(
                        canvas,
                        root,
                        p.hover_tint,
                        &scene::rrect(
                            button.x,
                            button.y + 3.0,
                            button.w - 4.0,
                            button.h - 6.0,
                            4.0,
                        ),
                    );
                }
                let text = t(self.lang, Words::AiAgentModeChangeDefault);
                let text = engine.elided(text, role::meta(), (button.w - 16.0).max(1.0));
                let text = engine.simple_layout(&text, role::meta());
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &text,
                    button.x + button.w - text.width() - 8.0,
                    button.y + (button.h - text.height()) * 0.5,
                    Some(p.accent),
                );
            }
        }
    }
}
