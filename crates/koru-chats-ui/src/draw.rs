// SPDX-License-Identifier: MPL-2.0

//! What the list puts on screen.
//!
//! Every rect comes from [`ChatsSurface::rows`] and
//! [`ChatsSurface::row_parts`] — the functions the hit test uses — and the
//! draw is clipped to the same band, so a row cannot be drawn anywhere other
//! than where it is pressable. A title that will not fit is **elided, not
//! wrapped**: a row is one line tall and stays one line tall.
//!
//! **The library's treatments, one column over**: the open chat and the
//! picked ones wear the card, the pointer's row the hover tint, the
//! keyboard's row the ring — and a title is dim at rest and full-strength on
//! the card, so the list does not tell the same story in two greys.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use koru_i18n::strings::{Key, t};
use koru_shell_chrome::tabs::draw_icon;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;

use crate::rows::{Notice, Row};
use crate::{ChatsSurface, Deleting};

/// The icon the navigator's mode row draws for this view. The same mark the
/// `+` menu's *Chats* row and an AI chat tab wear, so the three places a
/// chat is reached from agree about what one looks like — two bubbles (the
/// CTO's pick: `chats-circle`).
pub const MODE_ICON: char = icons::CHATS_CIRCLE;

impl ChatsSurface {
    /// The whole list below its host's heading, from `top` down to the
    /// area's floor.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        top: f32,
    ) {
        self.work.layouts = 0;
        if self.reveal_owed {
            self.reveal_owed = false;
            if let Some(row) = self.focus_row() {
                self.scroll_into_view(row, area, top);
            }
        }
        self.clamp_scroll(area, top);
        let p = theme::p();
        let band = self.band(area, top);
        scene.push_clip_layer(
            vello::peniko::Fill::NonZero,
            root,
            &scene::rect(band.x, band.y, band.w, band.h),
        );
        for (row, rect) in self.rows(area, top) {
            self.draw_row(scene, root, engine, area, row, rect, p);
        }
        scene.pop_layer();
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_row(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        row: Row,
        rect: Rect,
        p: &theme::Palette,
    ) {
        let parts = self.row_parts(row, rect);
        let lang = self.lang;
        let treatment = self.row_treatment(row);
        // One row, one named node — the same `ExploreRow` the library, the
        // rail and the Artifacts column answer to, in the same treatment
        // vocabulary, so the inspector reads the navigator as one column.
        let _row_scope = koru_draw::inspect::scope(
            "ExploreRow",
            "container",
            root,
            scene::rect(rect.x, rect.y, rect.w, rect.h),
            |record| {
                record.property("treatment", treatment, "row state");
            },
        );
        if treatment == "Card" {
            let chip = chip(rect, area);
            scene::fill(scene, root, p.panel_bg, &chip);
            scene::stroke(scene, root, p.panel_border, theme::BORDER_W, &chip);
        } else if treatment == "Hover" {
            scene::fill(scene, root, p.hover_tint, &chip(rect, area));
        }
        // The ring says *here, not yet chosen*; a ring on a card would read
        // as two things open, so the card wins.
        if treatment != "Card" && row.pressable() && self.focus_row() == Some(row) {
            scene::stroke(
                scene,
                root,
                p.panel_border,
                theme::BORDER_W,
                &chip(rect, area),
            );
        }
        match row {
            Row::Notice(notice) => {
                let key = match notice {
                    Notice::Loading => Key::ChatsLoading,
                    Notice::Unavailable => Key::ChatsUnavailable,
                    Notice::Empty => Key::ChatsEmpty,
                };
                let layout = engine.simple_layout_wrapped(
                    t(lang, key),
                    theme::role::meta(),
                    (area.w - theme::NAV_PAD * 4.0).max(1.0),
                );
                self.work.layouts += 1;
                scene::draw_layout(
                    scene,
                    root,
                    &layout,
                    area.x + theme::NAV_PAD * 2.0,
                    rect.y,
                    Some(p.text_dim),
                );
            }
            Row::Chat(index) => {
                let Some(chat) = self.chats.get(index) else {
                    return;
                };
                let private = chat.private;
                let title = crate::text::title(lang, chat).to_owned();
                let ink = if treatment == "Card" {
                    p.text
                } else {
                    p.text_dim
                };
                // The private mark left of the title, in a page icon's slot
                // (the CTO's note). Who answered and when is the accessible
                // name's, not the row's.
                if private && parts.lead.w > 0.0 {
                    draw_icon(scene, root, engine, icons::DETECTIVE, parts.lead, ink);
                }
                self.line(
                    scene,
                    root,
                    engine,
                    parts.title,
                    &title,
                    theme::role::row(),
                    ink,
                );
            }
            Row::Note(index) => {
                let Some(state) = self
                    .chats
                    .get(index)
                    .and_then(|chat| self.deleting.get(&chat.id))
                else {
                    return;
                };
                let (text, ink) = match state {
                    Deleting::Sending => (t(lang, Key::ChatsDeleting), p.text_dim),
                    Deleting::Refused(code) => (crate::text::refusal(lang, code), warm(p)),
                };
                self.line(
                    scene,
                    root,
                    engine,
                    parts.title,
                    text,
                    theme::role::meta(),
                    ink,
                );
            }
        }
    }

    /// What a row is wearing, in the library's vocabulary. Open **or**
    /// picked wear the card — one treatment, as the library's rows: a
    /// multi-selection and the chat you have open are the same claim.
    fn row_treatment(&self, row: Row) -> &'static str {
        let open = match row {
            Row::Chat(index) => self
                .chats
                .get(index)
                .is_some_and(|chat| self.current == Some(chat.id) || self.is_selected(chat.id)),
            _ => false,
        };
        if open {
            "Card"
        } else if row.pressable() && self.hover == Some(row) {
            "Hover"
        } else {
            "Idle"
        }
    }

    /// One line of text, elided to its rect and centred in it.
    #[allow(clippy::too_many_arguments)]
    fn line(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        text: &str,
        style: theme::TextStyle,
        ink: Color,
    ) {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let text = engine.elided(text, style, rect.w);
        let layout = engine.simple_layout(&text, style);
        self.work.layouts += 1;
        scene::draw_layout_first_line(
            scene,
            root,
            &layout,
            rect.x,
            rect.y + (rect.h - scene::first_line_height(&layout)) * 0.5,
            Some(ink),
        );
    }
}

/// The shape a card, a hover tint and a focus ring are drawn on — inset from
/// the list's edges the way every other nav row in the window is.
fn chip(rect: Rect, area: Rect) -> vello::kurbo::RoundedRect {
    scene::rrect(
        area.x + 4.0,
        rect.y + 1.0,
        (area.w - 8.0).max(0.0),
        (rect.h - 2.0).max(0.0),
        6.0,
    )
}

/// The one warm tone the list uses, for a refused delete. The accent set's
/// own red, never a new `Palette` field.
fn warm(p: &theme::Palette) -> Color {
    theme::accent_tone("red")
        .map(|tone| tone.ink)
        .unwrap_or(p.text)
}
