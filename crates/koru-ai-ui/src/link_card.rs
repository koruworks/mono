// SPDX-License-Identifier: MPL-2.0

//! **A link in the conversation shows the document's link card** (koru-ai
//! D95): rest the pointer on a link in a reply — in its text or in one of
//! its tables' cells — and the card a document shows over a link comes up
//! under it. It is the document's own card
//! ([`koru_editor::draw::draw_link_card`]): the same panel, place, wait and
//! buttons. Only the second button differs. A reply cannot be edited, so
//! where the document has its pencil the chat has **open**, which does what
//! clicking the link does ([`LinkCardAction::Open`]).
//!
//! The card reads `[Title](url)`, or the address alone when the link's
//! text *is* its address. **Copy** copies the address, as the document's
//! does. The card goes when the pointer leaves both the link and the card,
//! on Escape or any other key, and on a scroll.

use std::time::{Duration, Instant};

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;
use koru_editor::draw::{
    LinkCardAction, LinkCardGeom, LinkCardHit, draw_link_card, link_card_geom, link_card_label,
};
use koru_i18n::{Key, Lang, t};
use parley::editing::{Cursor, Selection};
use parley::layout::Affinity;
use vello::Scene;
use vello::kurbo::Affine;

/// A link in the conversation, as its card needs it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConversationLink {
    pub url: String,
    /// What the link reads as on screen.
    pub title: String,
    /// The line box of the link the pointer is on, which the card hangs
    /// from.
    pub anchor: Rect,
}

impl ConversationLink {
    /// The link `hit` found in a layout drawn at `(ox, oy)`, anchored on
    /// the line of it under `pointer`.
    pub fn from_hit(
        hit: &koru_markdown::LinkHit<'_>,
        (ox, oy): (f32, f32),
        pointer: (f32, f32),
    ) -> Self {
        let (x, y) = (ox + hit.origin.0, oy + hit.origin.1);
        let selection = Selection::new(
            Cursor::from_byte_index(hit.layout, hit.range.start, Affinity::Downstream),
            Cursor::from_byte_index(hit.layout, hit.range.end, Affinity::Upstream),
        );
        let mut boxes = Vec::new();
        selection.geometry_with(hit.layout, |bb, _| {
            boxes.push(Rect::new(
                x + bb.x0 as f32,
                y + bb.y0 as f32,
                (bb.x1 - bb.x0) as f32,
                (bb.y1 - bb.y0) as f32,
            ));
        });
        let anchor = boxes
            .iter()
            .find(|line| pointer.1 >= line.y && pointer.1 < line.y + line.h)
            .or(boxes.first())
            .copied()
            .unwrap_or(Rect::new(pointer.0, pointer.1, 1.0, 1.0));
        Self {
            url: hit.url.to_owned(),
            title: hit.title.to_owned(),
            anchor,
        }
    }

    /// What the card says: `[Title](url)`, or the address alone when the
    /// link's text is its address.
    pub fn label(&self) -> String {
        let title = self.title.trim();
        if title.is_empty() || title == self.url {
            self.url.clone()
        } else {
            format!("[{title}]({})", self.url)
        }
    }
}

/// The card's state: which link the pointer rests on, since when, and
/// where the card was drawn once it showed.
#[derive(Default)]
pub(crate) struct LinkCard {
    hovered: Option<Hovered>,
}

struct Hovered {
    link: ConversationLink,
    since: Instant,
    /// Set once [`LinkCard::maintain`] has seen the wait end, so the frame
    /// that shows the card is asked for exactly once.
    shown: bool,
    drawn: Option<LinkCardGeom>,
}

/// How long the pointer rests before the card comes up — the document's
/// wait (`theme::motion::tooltip`).
fn wait() -> Duration {
    theme::motion::tooltip().hover_delay
}

impl LinkCard {
    /// The pointer moved to `pointer`, over `under`. Whether what is drawn
    /// changed: the link lit, or the card shown or gone.
    pub fn track(
        &mut self,
        under: Option<ConversationLink>,
        pointer: (f32, f32),
        now: Instant,
    ) -> bool {
        // On its way onto the card the pointer leaves the link: the card
        // stays while it is over it, as the document's does.
        if self
            .card_rect(now)
            .is_some_and(|card| card.contains(pointer.0, pointer.1))
        {
            return false;
        }
        let same = match (&self.hovered, &under) {
            (Some(hovered), Some(link)) => hovered.link == *link,
            (None, None) => true,
            _ => false,
        };
        if same {
            return false;
        }
        self.hovered = under.map(|link| Hovered {
            link,
            since: now,
            shown: false,
            drawn: None,
        });
        true
    }

    /// Whether the card is up: the pointer has rested on the link for the
    /// document's wait.
    pub fn visible(&self, now: Instant) -> bool {
        self.hovered
            .as_ref()
            .is_some_and(|hovered| now.duration_since(hovered.since) > wait())
    }

    /// Where the card is, once it is up and has been drawn.
    pub fn card_rect(&self, now: Instant) -> Option<Rect> {
        self.visible(now)
            .then(|| self.hovered.as_ref()?.drawn.map(|card| card.rect))
            .flatten()
    }

    /// When the card should come up, while the pointer is still waiting.
    pub fn deadline(&self) -> Option<Instant> {
        self.hovered
            .as_ref()
            .filter(|hovered| !hovered.shown)
            .map(|hovered| hovered.since + wait() + Duration::from_millis(1))
    }

    /// Whether the card has just come up and needs a frame.
    pub fn maintain(&mut self, now: Instant) -> bool {
        let visible = self.visible(now);
        match &mut self.hovered {
            Some(hovered) if visible && !hovered.shown => {
                hovered.shown = true;
                true
            }
            _ => false,
        }
    }

    /// Pretend the pointer has rested `for_` longer than it has, so a test
    /// need not sleep through the wait.
    #[cfg(test)]
    pub fn rest(&mut self, for_: Duration) {
        if let Some(hovered) = &mut self.hovered {
            hovered.since -= for_;
        }
    }

    /// Let the card and the lit link go. Whether one was there.
    pub fn hide(&mut self) -> bool {
        self.hovered.take().is_some()
    }

    /// What of the card is under `(x, y)`, and the link it is for.
    pub fn hit(&self, x: f32, y: f32, now: Instant) -> Option<(LinkCardHit, &ConversationLink)> {
        if !self.visible(now) {
            return None;
        }
        let hovered = self.hovered.as_ref()?;
        let hit = hovered.drawn?.hit(x, y);
        (hit != LinkCardHit::None).then_some((hit, &hovered.link))
    }

    /// The card's two buttons and their names, for a screen reader: copy,
    /// then open.
    pub fn buttons(&self, lang: Lang, now: Instant) -> Vec<(Rect, &'static str)> {
        let Some(card) = self
            .visible(now)
            .then(|| self.hovered.as_ref()?.drawn)
            .flatten()
        else {
            return Vec::new();
        };
        vec![
            (card.copy, t(lang, Key::AiLinkCopy)),
            (card.action, t(lang, Key::AiLinkOpen)),
        ]
    }

    /// Draw the card, once it is up, inside `bounds`.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        bounds: Rect,
        pointer: (f32, f32),
        now: Instant,
    ) {
        if !self.visible(now) {
            return;
        }
        let Some(hovered) = &mut self.hovered else {
            return;
        };
        let label = link_card_label(engine, &hovered.link.label(), bounds.w);
        let card = match hovered.drawn {
            Some(card) => LinkCardGeom::in_rect(card.rect),
            None => link_card_geom(label.width(), label.height(), hovered.link.anchor, bounds),
        };
        hovered.drawn = Some(card);
        draw_link_card(
            scene,
            root,
            engine,
            &card,
            &label,
            LinkCardAction::Open,
            pointer,
        );
    }
}

#[cfg(test)]
mod tests;
