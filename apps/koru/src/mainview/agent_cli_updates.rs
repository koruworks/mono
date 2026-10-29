// SPDX-License-Identifier: MPL-2.0

//! The coding-agent CLI cards: one per installed CLI that is behind its
//! channel, being updated, just updated, or failed to update — stacked at the
//! top right, each with the button that answers it.
//!
//! **This window draws the conversation and holds none of it**, the rule
//! [`super::update_ui`] states for Koru's own update. The agent checks, runs
//! the updater and reports; a card is a function of the last
//! [`AgentCliReport`], and every button sends one [`AgentCliChoice`]. What
//! lives here is only what a drawing needs: hover, a press in flight, the
//! moving bars, and when a finished card appeared so it can put itself away.
//!
//! Composed from the shared pieces rather than a new component: a
//! `floating_panel` for the card, `ActionButton` for *Update* / *Try again*,
//! `IconButton` for the close, `ProgressBar` while the updater runs. The
//! reminder bell's attention popover is the same composition one surface over.

use std::time::{Duration, Instant};

use koru_agent_wire::{AgentCliChoice, AgentCliReport, AgentCliRow, AgentCliStage};
use koru_draw::geometry::Rect;
use koru_draw::{scene, theme};
use koru_i18n::{Key, Lang, fill, t};
use koru_ui::button::{ActionButton, ActionStyle};
use koru_ui::icon_button::{self, IconButton};
use koru_ui::progress_bar::{ProgressBar, ProgressMode};
use vello::Scene;
use vello::kurbo::Affine;

use super::MainView;

const CARD_W: f32 = 360.0;
const CARD_H: f32 = 66.0;
const GAP: f32 = 8.0;
const MARGIN: f32 = 16.0;
/// Below the caption strip, beside where the reminder bell's popover hangs.
const TOP: f32 = 48.0;
const PAD: f32 = 14.0;
const RADIUS: f32 = 10.0;
const CLOSE: f32 = 22.0;
const BAR_H: f32 = 3.0;
/// A finished card says what happened, then goes. Long enough to read a
/// version; short enough that four updates do not wall off the corner.
const SHOWN_WHEN_DONE: Duration = Duration::from_secs(8);
/// How long a pressed button claims its card while the agent's answer is in
/// flight — `update_ui::UPDATE_ACTION_TIMEOUT`'s reasoning.
const CHOICE_TIMEOUT: Duration = Duration::from_secs(10);

/// What a press on the stack landed on. `pub(super)` for the tests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Hit {
    Action(usize),
    Dismiss(usize),
    Close(usize),
}

/// The cards' own state. Nothing in here is about a CLI; all of it is about
/// a drawing.
#[derive(Default)]
pub(super) struct Cards {
    hover: Option<Hit>,
    /// A choice on its way to the agent — the CLI and when it was sent. The
    /// card keeps claiming presses and does not queue a second run.
    pending: Option<(String, Instant)>,
    /// One moving bar per CLI whose updater is running.
    bars: Vec<(String, ProgressBar)>,
    /// When each *updated* card appeared, for its auto-dismiss.
    done_since: Vec<(String, Instant)>,
}

/// One card, laid out. The action and the close are `None` where the stage
/// offers none — an updater that is running can be neither re-run nor
/// dismissed.
pub(super) struct Card {
    pub(super) row: AgentCliRow,
    pub(super) rect: Rect,
    pub(super) action: Option<(Key, Rect)>,
    /// The quiet text button left of the action: *Dismiss*, on the cards that
    /// have an action to put it beside. The ✕ stays at the corner.
    pub(super) dismiss: Option<Rect>,
    pub(super) close: Option<Rect>,
}

/// The rows a card is drawn for, in the agent's order.
fn attention(report: &AgentCliReport) -> impl Iterator<Item = &AgentCliRow> {
    report
        .clis
        .iter()
        .filter(|row| row.stage != AgentCliStage::UpToDate)
}

/// The two lines, localized. `pub(super)` for the tests.
pub(super) fn copy(row: &AgentCliRow, lang: Lang) -> (String, String) {
    match row.stage {
        AgentCliStage::Available => (
            fill(t(lang, Key::AgentCliUpdateAvailable), &[&row.name]),
            fill(
                t(lang, Key::AgentCliVersions),
                &[&row.installed, &row.latest],
            ),
        ),
        AgentCliStage::Updating => (
            fill(t(lang, Key::AgentCliUpdating), &[&row.name]),
            row.message
                .clone()
                .unwrap_or_else(|| t(lang, Key::AgentCliStarting).to_owned()),
        ),
        AgentCliStage::Updated => (
            fill(t(lang, Key::AgentCliUpdated), &[&row.name]),
            fill(t(lang, Key::AgentCliVersion), &[&row.installed]),
        ),
        AgentCliStage::Failed => (
            fill(t(lang, Key::AgentCliUpdateFailed), &[&row.name]),
            row.message.clone().unwrap_or_default(),
        ),
        AgentCliStage::UpToDate => (String::new(), String::new()),
    }
}

fn action_key(stage: AgentCliStage) -> Option<Key> {
    match stage {
        AgentCliStage::Available => Some(Key::AgentCliUpdate),
        AgentCliStage::Failed => Some(Key::TryAgain),
        _ => None,
    }
}

impl MainView {
    /// Card rectangles only — no text measured, so `&self` is enough for a
    /// pointer test.
    fn agent_cli_rects(&self) -> Vec<(AgentCliRow, Rect)> {
        let Some(report) = self.agent_clis.report() else {
            return Vec::new();
        };
        let x = (self.viewport.0 - MARGIN - CARD_W).max(MARGIN);
        attention(&report)
            .enumerate()
            .map(|(index, row)| {
                let y = TOP + index as f32 * (CARD_H + GAP);
                (row.clone(), Rect::new(x, y, CARD_W, CARD_H))
            })
            .collect()
    }

    /// Every card with its controls placed. Measures the button label, so it
    /// needs the engine.
    pub(super) fn agent_cli_cards(&mut self) -> Vec<Card> {
        let lang = crate::format::lang();
        let cards: Vec<Card> = self
            .agent_cli_rects()
            .into_iter()
            .map(|(row, rect)| {
                let close = (row.stage != AgentCliStage::Updating).then(|| {
                    Rect::new(
                        rect.x + rect.w - PAD - CLOSE + 4.0,
                        rect.y + 8.0,
                        CLOSE,
                        CLOSE,
                    )
                });
                let action = action_key(row.stage).map(|key| {
                    let width = ActionButton::new(t(lang, key), ActionStyle::Primary, None)
                        .width(&mut self.engine);
                    let right = close.map_or(rect.x + rect.w - PAD, |c| c.x - 8.0);
                    let y = rect.y + (rect.h - ActionButton::H) * 0.5;
                    (key, Rect::new(right - width, y, width, ActionButton::H))
                });
                let dismiss = action.as_ref().map(|(_, action_rect)| {
                    let width =
                        ActionButton::new(t(lang, Key::AgentCliDismiss), ActionStyle::Ghost, None)
                            .width(&mut self.engine);
                    Rect::new(
                        action_rect.x - 4.0 - width,
                        action_rect.y,
                        width,
                        ActionButton::H,
                    )
                });
                Card {
                    row,
                    rect,
                    action,
                    dismiss,
                    close,
                }
            })
            .collect();
        // A press is in flight until the agent's report moves its card off the
        // stage that was pressed, or the round-trip times out.
        if let Some((id, sent)) = &self.agent_cli_cards.pending {
            let still_pressable = cards.iter().any(|card| {
                card.row.id == *id
                    && matches!(
                        card.row.stage,
                        AgentCliStage::Available | AgentCliStage::Failed
                    )
            });
            if !still_pressable || sent.elapsed() > CHOICE_TIMEOUT {
                self.agent_cli_cards.pending = None;
            }
        }
        cards
    }

    fn agent_cli_choice_in_flight(&self, id: &str) -> bool {
        self.agent_cli_cards
            .pending
            .as_ref()
            .is_some_and(|(pending, _)| pending == id)
    }

    pub(super) fn agent_cli_mouse_down(&mut self, x: f32, y: f32) -> bool {
        let cards = self.agent_cli_cards();
        let Some(card) = cards.iter().find(|card| card.rect.contains(x, y)) else {
            return false;
        };
        let id = card.row.id.clone();
        if let Some((_, rect)) = &card.action
            && rect.contains(x, y)
        {
            if self.agent_cli_choice_in_flight(&id) {
                return true;
            }
            if self
                .agent_clis
                .choose(AgentCliChoice::Update { id: id.clone() })
            {
                self.agent_cli_cards.pending = Some((id, Instant::now()));
            }
            return true;
        }
        if card
            .dismiss
            .into_iter()
            .chain(card.close)
            .any(|rect| rect.contains(x, y))
        {
            let _ = self.agent_clis.choose(AgentCliChoice::Dismiss { id });
            return true;
        }
        true
    }

    /// A card is a floating surface, not decoration on the document below;
    /// claiming movement keeps the editor's hovers from showing through it.
    pub(super) fn agent_cli_mouse_move(&mut self, x: f32, y: f32) -> bool {
        let cards = self.agent_cli_cards();
        let mut hover = None;
        let mut inside = false;
        for (index, card) in cards.iter().enumerate() {
            if !card.rect.contains(x, y) {
                continue;
            }
            inside = true;
            if card.action.as_ref().is_some_and(|(_, r)| r.contains(x, y)) {
                hover = Some(Hit::Action(index));
            } else if card.dismiss.is_some_and(|r| r.contains(x, y)) {
                hover = Some(Hit::Dismiss(index));
            } else if card.close.is_some_and(|r| r.contains(x, y)) {
                hover = Some(Hit::Close(index));
            }
        }
        self.agent_cli_cards.hover = hover;
        inside
    }

    /// The wheel is routed by the last pointer position; a card under it owns
    /// it, exactly as the update banner does.
    pub(super) fn agent_cli_contains_pointer(&self) -> bool {
        let (x, y) = self.pointer;
        self.agent_cli_rects()
            .iter()
            .any(|(_, rect)| rect.contains(x, y))
    }

    /// Over a card the pointer is the plain arrow, buttons included
    /// (`koru_ui::cursor::PRESS`): without this the document's text cursor
    /// showed through the card.
    pub(super) fn agent_cli_cursor(&self) -> Option<winit::window::CursorIcon> {
        self.agent_cli_contains_pointer()
            .then_some(koru_ui::cursor::PRESS)
    }

    /// A moving bar, or a finished card waiting out its time, needs frames.
    pub(super) fn agent_cli_animating(&self) -> bool {
        self.agent_clis.report().is_some_and(|report| {
            attention(&report)
                .any(|row| matches!(row.stage, AgentCliStage::Updating | AgentCliStage::Updated))
        })
    }

    pub(super) fn draw_agent_cli_cards(&mut self, scene: &mut Scene, root: Affine) {
        let now = Instant::now();
        let cards = self.agent_cli_cards();
        if cards.is_empty() {
            self.agent_cli_cards.bars.clear();
            self.agent_cli_cards.done_since.clear();
            return;
        }
        self.agent_cli_tend(&cards, now);
        let lang = crate::format::lang();
        let p = theme::p();
        for (index, card) in cards.iter().enumerate() {
            let rect = card.rect;
            // Held to the end of the card: the text, buttons and bar nest under
            // the panel in the inspector instead of landing beside it.
            let _panel = scene::floating_panel_scope(
                scene,
                root,
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                &scene::Panel::popover(RADIUS),
            );
            let controls_w = card.action.as_ref().map_or(0.0, |(_, r)| r.w + 8.0)
                + card.dismiss.map_or(0.0, |r| r.w + 4.0)
                + card.close.map_or(0.0, |r| r.w + 8.0);
            let text_w = (rect.w - PAD * 2.0 - controls_w).max(40.0);
            let (title, line) = copy(&card.row, lang);
            let title = self.engine.elided(&title, theme::role::ui(), text_w);
            let title = self.engine.simple_layout(&title, theme::role::ui());
            scene::draw_layout(
                scene,
                root,
                &title,
                rect.x + PAD,
                rect.y + 12.0,
                Some(p.text),
            );
            if !line.is_empty() {
                let line = self.engine.elided(&line, theme::role::meta(), text_w);
                let layout = self.engine.simple_layout(&line, theme::role::meta());
                scene::draw_layout(
                    scene,
                    root,
                    &layout,
                    rect.x + PAD,
                    rect.y + 34.0,
                    Some(p.text_dim),
                );
            }
            if let Some((key, action_rect)) = &card.action {
                let hovered = self.agent_cli_cards.hover == Some(Hit::Action(index));
                ActionButton::new(t(lang, *key), ActionStyle::Primary, None).draw(
                    scene,
                    root,
                    &mut self.engine,
                    *action_rect,
                    hovered,
                );
            }
            if let Some(dismiss) = card.dismiss {
                let hovered = self.agent_cli_cards.hover == Some(Hit::Dismiss(index));
                ActionButton::new(t(lang, Key::AgentCliDismiss), ActionStyle::Ghost, None).draw(
                    scene,
                    root,
                    &mut self.engine,
                    dismiss,
                    hovered,
                );
            }
            if let Some(close) = card.close {
                let hovered = self.agent_cli_cards.hover == Some(Hit::Close(index));
                IconButton::new(icon_button::CLOSE, close)
                    .with_state(icon_button::State {
                        hovered,
                        active: false,
                        disabled: false,
                    })
                    .draw(scene, root, &mut self.engine);
            }
            if let Some((_, bar)) = self
                .agent_cli_cards
                .bars
                .iter()
                .find(|(id, _)| *id == card.row.id)
            {
                bar.draw(
                    scene,
                    root,
                    Rect::new(
                        rect.x + PAD,
                        rect.y + rect.h - PAD + 2.0,
                        rect.w - PAD * 2.0,
                        BAR_H,
                    ),
                    now,
                    1.0,
                );
            }
        }
    }

    /// Keep the drawing state in step with the report: a bar per running
    /// updater, a clock per finished card, and the dismissal a finished card
    /// sends itself when its time is up.
    fn agent_cli_tend(&mut self, cards: &[Card], now: Instant) {
        let state = &mut self.agent_cli_cards;
        state.bars.retain(|(id, _)| {
            cards
                .iter()
                .any(|card| card.row.id == *id && card.row.stage == AgentCliStage::Updating)
        });
        state.done_since.retain(|(id, _)| {
            cards
                .iter()
                .any(|card| card.row.id == *id && card.row.stage == AgentCliStage::Updated)
        });
        let mut expired = Vec::new();
        for card in cards {
            match card.row.stage {
                AgentCliStage::Updating => {
                    if !state.bars.iter().any(|(id, _)| *id == card.row.id) {
                        state.bars.push((
                            card.row.id.clone(),
                            ProgressBar::new(ProgressMode::Indeterminate, now),
                        ));
                    }
                }
                AgentCliStage::Updated => {
                    match state.done_since.iter().find(|(id, _)| *id == card.row.id) {
                        Some((_, since)) if now.duration_since(*since) >= SHOWN_WHEN_DONE => {
                            expired.push(card.row.id.clone());
                        }
                        Some(_) => {}
                        None => state.done_since.push((card.row.id.clone(), now)),
                    }
                }
                _ => {}
            }
        }
        for id in expired {
            // Sent once: the clock entry goes with it, and the agent's next
            // report removes the card.
            state.done_since.retain(|(done, _)| *done != id);
            let _ = self.agent_clis.choose(AgentCliChoice::Dismiss { id });
        }
    }
}

#[cfg(test)]
mod tests;
