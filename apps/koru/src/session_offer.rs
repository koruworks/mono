// SPDX-License-Identifier: MPL-2.0

//! What a restored terminal tab offers when its session did not survive
//! ([ADR-474](../../../docs/decisions/ADR-474-sessionrecovery.md)).
//!
//! The tab's layout, its agent and its folder were saved; the process was not,
//! because the Agent that owned it stopped (a reboot, a crash, **Quit** from
//! the tray). Closing the tab loses where the person was working, so it stays,
//! and says so: *Resume* — continue the agent's most recent conversation in
//! that folder, for an agent that can — and *Start new*, a fresh session in
//! the same place. Neither happens by itself, because a long conversation
//! reopened unasked is as unwelcome as one that was lost.
//!
//! This module is the panel only: layout, paint and input. Carrying out the
//! choice is the container's (`mainview/session_backend.rs`), which owns the
//! backend and replaces the tab's view.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use koru_i18n::{Key as I18n, Lang, fill, t};
use koru_ui::button::{ActionButton, ActionStyle};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, NamedKey};
use winit::window::CursorIcon;

const COLUMN_W: f32 = 440.0;
const MARGIN: f32 = 24.0;
const GAP: f32 = 8.0;
const LINE: f32 = 24.0;

/// What the person picked.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Continue the agent's most recent conversation in the folder.
    Resume,
    /// A fresh session in the same folder and project.
    StartNew,
}

pub struct Offer {
    /// The agent's name, for the title.
    name: String,
    /// Where the session was, when the saved tab knew.
    folder: Option<String>,
    /// Whether this agent has a continue form (a shell or Muse does not).
    resumable: bool,
    hover: Option<Choice>,
    /// Set by a press; the container takes it and starts the session.
    chosen: Option<Choice>,
}

/// Every rectangle the panel draws and answers, from one place so paint and
/// hit-testing cannot disagree.
struct Layout {
    title: Rect,
    folder: Rect,
    hint: Rect,
    resume: Option<Rect>,
    start_new: Rect,
}

impl Offer {
    pub fn new(name: String, folder: Option<String>, resumable: bool) -> Self {
        Self {
            name,
            folder,
            resumable,
            hover: None,
            chosen: None,
        }
    }

    #[cfg(test)]
    pub fn resumable(&self) -> bool {
        self.resumable
    }

    #[cfg(test)]
    pub fn force_choice_for_test(&mut self, choice: Choice) {
        self.chosen = Some(choice);
    }

    /// The choice a press made, once, for the container to act on.
    pub fn take_choice(&mut self) -> Option<Choice> {
        self.chosen.take()
    }

    fn layout(&self, engine: &mut TextEngine, lang: Lang, bounds: Rect) -> Layout {
        let w = COLUMN_W.min((bounds.w - MARGIN * 2.0).max(120.0));
        let x = bounds.x + (bounds.w - w) * 0.5;
        let button_h = ActionButton::H;
        let lines = 2.0 + f32::from(self.resumable);
        let total = lines * LINE + GAP * 2.0 + button_h;
        let top = bounds.y + ((bounds.h - total) * 0.45).max(MARGIN);
        let title = Rect::new(x, top, w, LINE);
        let folder = Rect::new(x, top + LINE, w, LINE);
        let hint = Rect::new(
            x,
            top + LINE * 2.0,
            w,
            if self.resumable { LINE } else { 0.0 },
        );
        let row_y = top + LINE * lines + GAP * 2.0;
        let start_w = ActionButton::new(
            t(lang, I18n::TerminalRecoverStartNew),
            ActionStyle::Secondary,
            None,
        )
        .width(engine);
        let resume = self.resumable.then(|| {
            let width = ActionButton::new(
                t(lang, I18n::TerminalRecoverResume),
                ActionStyle::Primary,
                None,
            )
            .width(engine);
            Rect::new(x, row_y, width, button_h)
        });
        let start_x = resume.map_or(x, |rect| rect.x + rect.w + GAP);
        Layout {
            title,
            folder,
            hint,
            resume,
            start_new: Rect::new(start_x, row_y, start_w, button_h),
        }
    }

    fn hit(
        &self,
        engine: &mut TextEngine,
        lang: Lang,
        bounds: Rect,
        x: f32,
        y: f32,
    ) -> Option<Choice> {
        let layout = self.layout(engine, lang, bounds);
        if layout.resume.is_some_and(|rect| rect.contains(x, y)) {
            Some(Choice::Resume)
        } else if layout.start_new.contains(x, y) {
            Some(Choice::StartNew)
        } else {
            None
        }
    }

    pub fn draw(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        lang: Lang,
        bounds: Rect,
        terminal_dark: bool,
    ) {
        // The words sit on the **terminal's** ground, not the application's: a
        // light application around a dark terminal would otherwise set dark
        // text on dark, and the panel — the only way to resume — was invisible.
        // The palette is chosen, never switched: painting must not change the
        // global theme.
        let p = if terminal_dark {
            &theme::DARK
        } else {
            &theme::LIGHT
        };
        let layout = self.layout(engine, lang, bounds);
        let title = fill(t(lang, I18n::TerminalRecoveredTitle), &[&self.name]);
        let title = engine.elided(&title, theme::role::ui(), layout.title.w);
        let title = engine.simple_layout(&title, theme::role::ui());
        scene::draw_layout(
            canvas,
            root,
            &title,
            layout.title.x,
            layout.title.y,
            Some(p.text),
        );
        if let Some(folder) = &self.folder {
            let line = fill(t(lang, I18n::TerminalRecoveredFolder), &[folder]);
            let line = engine.elided(&line, theme::role::meta(), layout.folder.w);
            let line = engine.simple_layout(&line, theme::role::meta());
            scene::draw_layout(
                canvas,
                root,
                &line,
                layout.folder.x,
                layout.folder.y,
                Some(p.text_dim),
            );
        }
        if self.resumable {
            let hint = engine.elided(
                t(lang, I18n::TerminalRecoveredResumeHint),
                theme::role::meta(),
                layout.hint.w,
            );
            let hint = engine.simple_layout(&hint, theme::role::meta());
            scene::draw_layout(
                canvas,
                root,
                &hint,
                layout.hint.x,
                layout.hint.y,
                Some(p.text_dim),
            );
        }
        if let Some(rect) = layout.resume {
            ActionButton::new(
                t(lang, I18n::TerminalRecoverResume),
                ActionStyle::Primary,
                None,
            )
            .draw(
                canvas,
                root,
                engine,
                rect,
                self.hover == Some(Choice::Resume),
            );
        }
        ActionButton::new(
            t(lang, I18n::TerminalRecoverStartNew),
            ActionStyle::Secondary,
            None,
        )
        .draw(
            canvas,
            root,
            engine,
            layout.start_new,
            self.hover == Some(Choice::StartNew),
        );
    }

    pub fn mouse_down(
        &mut self,
        engine: &mut TextEngine,
        lang: Lang,
        bounds: Rect,
        x: f32,
        y: f32,
    ) -> bool {
        if let Some(choice) = self.hit(engine, lang, bounds, x, y) {
            self.chosen = Some(choice);
        }
        // The panel owns the pane: a press beside a button is still its, so it
        // never reaches a terminal grid that is not there.
        true
    }

    pub fn mouse_move(
        &mut self,
        engine: &mut TextEngine,
        lang: Lang,
        bounds: Rect,
        x: f32,
        y: f32,
    ) -> bool {
        let was = self.hover;
        self.hover = self.hit(engine, lang, bounds, x, y);
        was != self.hover
    }

    /// Enter takes the first button, as in any dialog: *Resume* where there is
    /// one, otherwise *Start new*. Every other key is swallowed — there is no
    /// terminal behind the panel to type into.
    pub fn key(&mut self, key: &Key) -> bool {
        if matches!(key, Key::Named(NamedKey::Enter)) {
            self.chosen = Some(if self.resumable {
                Choice::Resume
            } else {
                Choice::StartNew
            });
        }
        true
    }

    pub fn cursor(&self) -> CursorIcon {
        match self.hover {
            Some(_) => koru_ui::cursor::PRESS,
            None => CursorIcon::Default,
        }
    }
}

#[cfg(test)]
mod tests;
