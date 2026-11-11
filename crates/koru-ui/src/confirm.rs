// SPDX-License-Identifier: MPL-2.0

//! A modal question with two answers, one of which destroys something --
//! or, when the question genuinely has three, with a second answer beside
//! them (see [`Confirm::with_alternative`]).
//!
//! Generic rather than trash-specific on purpose: `ErrorDialog` and
//! `CloseDialog` are already two copies of one recipe, and a third would make it
//! hard to argue against a fourth. It carries strings; the **owner** holds what
//! the question is about and matches on the answer.
//!
//! ### Enter executes the declared default
//!
//! `ErrorDialog` says Enter runs the primary — the button in the corner. Here
//! the corner button destroys twenty-four documents. A modal that appears under
//! somebody's hands and reads the next Enter as *yes, bin all of that* is
//! precisely the accident it was put up to prevent, so **this dialog's declared
//! default is Cancel** — the safe, non-destructive answer in
//! [ADR-265](../../../docs/decisions/ADR-251-275.md)'s table — and the
//! destructive one is reached by Tab or by the pointer. That is what this type
//! exists for rather than a third `ErrorDialog`, and it is unchanged.
//!
//! What ADR-265 adds is that the default is now **declared rather than
//! implied**, so a confirmation that should be acceptable by Enter can say so
//! instead of the rule being global:
//!
//! - [`Confirm::with_default`] — a *reversible* action the person explicitly
//!   invoked. Archiving and soft-deleting a workspace are the first, in
//!   `koru-workspaces-ui`.
//! - [`Confirm::declaring_no_default`] — irreversible or high-impact, where
//!   Enter must answer nothing at all until a button is deliberately focused.
//!
//! **A deliberately focused button always wins.** Tab moves the focus, and from
//! then on Enter activates what is focused rather than what was declared —
//! which is the last row of the DESIGN table and the reason the declaration
//! cannot make the visible focus ring lie.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, NamedKey};
use winit::window::CursorIcon;

use crate::button::{self, ButtonRole};

const WIDTH: f32 = 420.0;
const PAD: f32 = 20.0;
const RADIUS: f32 = 10.0;
/// The exclamation in its tinted disc.
const BADGE: f32 = 34.0;
const EDGE: f32 = 16.0;

const TITLE: theme::TextStyle = theme::role::section_heading();
const BODY: theme::TextStyle = theme::role::row();

/// What the dialog wants done.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Still open. A press missed, or a key moved the focus.
    Keep,
    Cancel,
    /// The destructive answer. The owner knows what it is about.
    Confirm,
    /// **The second answer**, for a question that has one — see
    /// [`Confirm::with_alternative`]. Never produced by a dialog that
    /// declared none, so a caller that did not ask for it can leave the arm
    /// with the others that change nothing.
    Alternative,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Hit {
    Cancel,
    Alternative,
    Confirm,
}

pub struct Confirm {
    title: String,
    body: String,
    /// The answering button's label. Always the right-hand one.
    confirm_label: String,
    /// What the answering button *is*. Destructive by default, because that is
    /// what this type was written for; see [`Confirm::with_role`] for the one
    /// other question that belongs here.
    confirm_role: ButtonRole,
    /// Whether the badge says *something is about to go*.
    badge: bool,
    /// The second answer's label, for a question that has three. `None` is
    /// every other question, and then nothing about this type changes.
    alternative_label: Option<String>,
    cancel_label: String,
    /// **Cancel, on arrival.** See the module docs.
    focus: Hit,
    /// Whether the focus above was *moved there* rather than merely started
    /// there. Enter follows a deliberately focused button and the declared
    /// default otherwise, and those are only the same answer by coincidence.
    focus_moved: bool,
    /// **What Enter does before anything is focused** (ADR-265). `Some(Cancel)`
    /// unless a caller says otherwise: this dialog's answer destroys something,
    /// and the safe answer is the one an in-flight keystroke may reach.
    default_action: Option<Outcome>,
    hover: Option<Hit>,
}

impl Confirm {
    pub fn new(
        title: impl Into<String>,
        body: impl Into<String>,
        confirm_label: impl Into<String>,
    ) -> Self {
        Self::localized(title, body, confirm_label, "Cancel")
    }

    /// Construct the shared confirmation with both actions localized by the
    /// owning product surface.
    pub fn localized(
        title: impl Into<String>,
        body: impl Into<String>,
        confirm_label: impl Into<String>,
        cancel_label: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            confirm_label: confirm_label.into(),
            confirm_role: ButtonRole::Destructive,
            badge: true,
            alternative_label: None,
            cancel_label: cancel_label.into(),
            focus: Hit::Cancel,
            focus_moved: false,
            default_action: Some(Outcome::Cancel),
            hover: None,
        }
    }

    /// **Declare the answering button as the default** (ADR-265, row two).
    ///
    /// For a confirmation the person explicitly invoked, of an action that is
    /// *reversible*: archiving a workspace, or moving one to the trash it can
    /// be restored from. Enter accepts; Escape still cancels.
    ///
    /// It is not for anything irreversible, and there is no way to ask for that
    /// by accident: the only other declaration is
    /// [`Self::declaring_no_default`], and a caller that wants Enter to reach a
    /// destructive answer has to write this line and be read on it.
    pub fn defaulting_to_confirm(mut self) -> Self {
        self.default_action = Some(Outcome::Confirm);
        // **The ring starts where Enter answers.** It once stayed on Cancel
        // whatever was declared, so a dialog that Enter confirmed was drawn
        // with Cancel ringed — and a person who pressed Enter on what they
        // saw as Cancel archived a Bot (the first window run of Bots).
        self.focus = Hit::Confirm;
        self
    }

    /// **Declare no default at all** (ADR-265): Enter answers nothing until a
    /// button is deliberately focused.
    ///
    /// For irreversible or high-impact actions. **Its caller is P1.16's
    /// *End the run***: a delegated run that is ended cannot be resumed and
    /// whatever it had not finished will not be produced, which is row four
    /// exactly. It waited a long time for that caller — Koru has no
    /// permanent-delete route anywhere, and the retention sweep is the only
    /// thing that removes bytes — and the wait was the decision rather than
    /// an oversight.
    pub fn declaring_no_default(mut self) -> Self {
        self.default_action = None;
        self
    }

    /// **A question with three answers**, because it has three.
    ///
    /// One asks it: putting a delegated run in the trash when the run
    /// produced results. Two things are being decided at once and they are
    /// not the same decision -- the run goes into the bin either way, and
    /// what happens to its results when the bin is emptied is the person's
    /// to choose *now*, while nothing has happened yet. Asking it as two
    /// dialogs in a row would make the second look like a dialog that had
    /// not taken no for an answer; asking it afterwards would mean asking
    /// about work that was already gone.
    ///
    /// The alternative is the **destructive** one and sits in the middle;
    /// the corner answer becomes [`ButtonRole::Primary`], because in a
    /// question shaped like this the corner is the safe half and outlining
    /// it in red would say the app thinks it is a mistake. Enter still
    /// answers the declared default -- which is still Cancel unless the
    /// caller says otherwise -- and Tab now walks three buttons rather than
    /// two.
    pub fn with_alternative(mut self, label: impl Into<String>) -> Self {
        self.alternative_label = Some(label.into());
        self.confirm_role = ButtonRole::Primary;
        self
    }

    /// What Enter would do right now, as a value a test can assert on.
    ///
    /// **A fact of the dialog, not of its trigger.** Two modals that look
    /// identical answer the keyboard identically, and this is how a suite pins
    /// that rather than driving a key and inferring it.
    pub fn default_action(&self) -> Option<Outcome> {
        match self.focus_moved {
            true => Some(answer(self.focus)),
            false => self.default_action,
        }
    }

    /// **The answer the focus ring is drawn on**, or `None` while none is.
    ///
    /// Always the answer [`Self::default_action`] gives — what Enter presses
    /// is what is ringed — and a test pins that rather than trusting it:
    /// before a key moves the focus the ring is on the declared default, and
    /// a dialog declaring none draws no ring until a button is chosen.
    pub fn focused(&self) -> Option<Outcome> {
        match self.focus_moved {
            true => Some(answer(self.focus)),
            false => self.default_action.map(|_| answer(self.focus)),
        }
    }

    /// Where the body is drawn, for a host that publishes it to an
    /// assistive technology as the dialog's text.
    pub fn body_rect(&self, engine: &mut TextEngine, viewport: (f32, f32)) -> Rect {
        let panel = self.panel(engine, viewport);
        let tw = self.text_w(panel.w);
        let title = engine
            .simple_layout_wrapped(&self.title, TITLE, tw)
            .height();
        let body = engine.simple_layout_wrapped(&self.body, BODY, tw).height();
        Rect::new(
            panel.x + self.text_inset(),
            panel.y + PAD + title + 8.0,
            tw,
            body,
        )
    }

    /// **The same dialog for a question that is not destructive**, with an
    /// accented answer and no warning badge.
    ///
    /// Two questions use it: the voice consent step, which asks whether to
    /// download a speech model, and the Cloud move prompt in Manage Profiles.
    /// It belongs here rather than in a fourth copy of the recipe — this type
    /// exists precisely because `ErrorDialog` and `CloseDialog` were already
    /// two — and everything that makes it *this* type still holds. **Enter is
    /// still Cancel**, and that is not an oversight carried over: a modal
    /// appearing under somebody's hands must not read the next Enter as *yes,
    /// spend six hundred megabytes*. What changes is only what the button
    /// looks like, because outlining "Download" in red would say the app
    /// thinks it is a mistake.
    ///
    /// **Both answers are the caller's.** This used to fill in an English
    /// *Cancel*, and a second constructor was added beside it to take a
    /// localized one — which left the voice consent dialog, the only caller of
    /// the first, with an English button in seven languages. Every string a
    /// person reads goes through `koru-i18n`, so there is one constructor and
    /// it asks for both labels (maintainers, 2026-09-23).
    pub fn asking(
        title: impl Into<String>,
        body: impl Into<String>,
        confirm_label: impl Into<String>,
        cancel_label: impl Into<String>,
    ) -> Self {
        Self {
            confirm_role: ButtonRole::Primary,
            badge: false,
            ..Self::localized(title, body, confirm_label, cancel_label)
        }
    }

    /// **The answers as a host publishes them to an assistive technology**:
    /// each answer, its label and where it is drawn, left to right. What
    /// Enter presses is [`Self::default_action`], so a host marks that answer
    /// as the default and the tree says what the keyboard will do. Read-only:
    /// it changes nothing about the dialog.
    pub fn answers(
        &self,
        engine: &mut TextEngine,
        viewport: (f32, f32),
    ) -> Vec<(Outcome, String, Rect)> {
        self.targets(engine, viewport)
            .into_iter()
            .map(|(hit, rect)| (answer(hit), self.label_of(hit).to_owned(), rect))
            .collect()
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    // ---------------------------------------------------------- geometry

    /// Where the text column starts, from the panel's left edge: beside the
    /// badge when there is one, at the padding when there is not, so a
    /// question without a badge does not leave an empty gutter for it.
    fn text_inset(&self) -> f32 {
        if self.badge { PAD + BADGE + 14.0 } else { PAD }
    }

    fn text_w(&self, panel_w: f32) -> f32 {
        (panel_w - self.text_inset() - PAD).max(80.0)
    }

    fn height(&self, engine: &mut TextEngine, panel_w: f32) -> f32 {
        let w = self.text_w(panel_w);
        let title = engine.simple_layout_wrapped(&self.title, TITLE, w).height();
        let body = engine.simple_layout_wrapped(&self.body, BODY, w).height();
        PAD + title + 8.0 + body + 18.0 + button::H + PAD
    }

    pub fn panel(&self, engine: &mut TextEngine, viewport: (f32, f32)) -> Rect {
        let w = WIDTH.min(viewport.0 - EDGE * 2.0).max(0.0);
        let h = self.height(engine, w).min(viewport.1 - EDGE * 2.0);
        Rect::new((viewport.0 - w) * 0.5, (viewport.1 - h) * 0.5, w, h)
    }

    /// Which answers this dialog has, left to right. Cancel is always
    /// leftmost and the corner answer always rightmost; the alternative,
    /// when there is one, sits between them.
    fn hits(&self) -> Vec<Hit> {
        match self.alternative_label {
            Some(_) => vec![Hit::Cancel, Hit::Alternative, Hit::Confirm],
            None => vec![Hit::Cancel, Hit::Confirm],
        }
    }

    fn label_of(&self, hit: Hit) -> &str {
        match hit {
            Hit::Cancel => &self.cancel_label,
            Hit::Alternative => self.alternative_label.as_deref().unwrap_or_default(),
            Hit::Confirm => &self.confirm_label,
        }
    }

    /// Every button, laid out **from the right** so the corner answer owns the
    /// corner however wide "Cancel" turns out to be in this language.
    fn targets(&self, engine: &mut TextEngine, viewport: (f32, f32)) -> Vec<(Hit, Rect)> {
        let p = self.panel(engine, viewport);
        let hits = self.hits();
        let widths: Vec<f32> = hits
            .iter()
            .map(|hit| button::button_width(engine, self.label_of(*hit)))
            .collect();
        let y = p.y + p.h - PAD - button::H;
        let rects = button::row_from_right(p.x + p.w - PAD, y, &widths);
        hits.into_iter().zip(rects).collect()
    }

    fn hit_at(&self, engine: &mut TextEngine, viewport: (f32, f32), x: f32, y: f32) -> Option<Hit> {
        self.targets(engine, viewport)
            .into_iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(h, _)| h)
    }

    // ------------------------------------------------------------- input

    /// **Enter is Cancel unless the focus has been moved.** See the module docs.
    /// Everything else is swallowed, for `ErrorDialog`'s reason: a modal that
    /// let keystrokes through would take dictation during a warning.
    pub fn key(&mut self, key: &Key) -> Outcome {
        match key {
            Key::Named(NamedKey::Escape) => Outcome::Cancel,
            Key::Named(NamedKey::Tab)
            | Key::Named(NamedKey::ArrowLeft)
            | Key::Named(NamedKey::ArrowRight) => {
                let hits = self.hits();
                let at = hits.iter().position(|hit| *hit == self.focus).unwrap_or(0);
                self.focus = hits[(at + 1) % hits.len()];
                self.focus_moved = true;
                Outcome::Keep
            }
            // The declared default, or the button that was deliberately
            // focused, and **nothing more** — a modal declaring none leaves
            // Enter inert until somebody has chosen a button.
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                self.default_action().unwrap_or(Outcome::Keep)
            }
            _ => Outcome::Keep,
        }
    }

    /// A press outside does **not** dismiss. The reasoning is `ErrorDialog`'s
    /// and it is stronger here: a stray click must not be able to answer a
    /// question about deleting somebody's work, in either direction.
    pub fn mouse_down(
        &mut self,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        x: f32,
        y: f32,
    ) -> Outcome {
        match self.hit_at(engine, viewport, x, y) {
            Some(hit) => answer(hit),
            None => Outcome::Keep,
        }
    }

    pub fn mouse_move(
        &mut self,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        x: f32,
        y: f32,
    ) -> bool {
        let was = self.hover;
        self.hover = self.hit_at(engine, viewport, x, y);
        was != self.hover
    }

    /// The plain arrow either way — Koru does not use the hand cursor
    /// (`docs/features/design/DESIGN.md`). The hit test survives because it is what says which
    /// button a press would land on, and it drives the hover treatment.
    pub fn cursor(&self) -> CursorIcon {
        match self.hover {
            Some(_) => crate::cursor::PRESS,
            None => crate::cursor::IDLE,
        }
    }

    // -------------------------------------------------------------- paint

    pub fn draw(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
    ) {
        let p = theme::p();
        // The scrim first: everything behind this is not answerable until this
        // is.
        scene::fill(
            scene,
            root,
            p.scrim,
            &scene::rect(0.0, 0.0, viewport.0, viewport.1),
        );

        let panel = self.panel(engine, viewport);
        // Held to the end of `draw`: the title, the description and the
        // buttons are the panel's contents, and nest under it in the inspector.
        let _panel = scene::floating_panel_scope(
            scene,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(RADIUS),
        );

        // The badge — a tinted disc with an exclamation, the same shape
        // `ErrorDialog` uses, because this is the same register of message.
        // A question that destroys nothing does not wear it: a warning badge
        // over *Set up voice input?* would say something is wrong.
        let bx = panel.x + PAD;
        let by = panel.y + PAD;
        if self.badge {
            scene::fill(
                scene,
                root,
                p.close_hover.multiply_alpha(0.18),
                &vello::kurbo::Circle::new(
                    ((bx + BADGE * 0.5) as f64, (by + BADGE * 0.5) as f64),
                    (BADGE * 0.5) as f64,
                ),
            );
            let bang = engine.simple_layout("!", TITLE);
            scene::draw_layout_first_line(
                scene,
                root,
                &bang,
                bx + (BADGE - bang.width()) * 0.5,
                by + (BADGE - bang.height()) * 0.5,
                Some(p.close_hover),
            );
        }

        let tx = panel.x + self.text_inset();
        let tw = self.text_w(panel.w);
        let title = engine.simple_layout_wrapped(&self.title, TITLE, tw);
        {
            let _part = crate::typography::part(
                "Dialog title",
                root,
                Rect::new(tx, by, tw, title.height()),
                TITLE,
                "theme::role::section_heading()",
            );
            scene::draw_layout(scene, root, &title, tx, by, Some(p.text));
        }
        let body = engine.simple_layout_wrapped(&self.body, BODY, tw);
        {
            let _part = crate::typography::part(
                "Dialog description",
                root,
                Rect::new(tx, by + title.height() + 8.0, tw, body.height()),
                BODY,
                "theme::role::row()",
            );
            scene::draw_layout(
                scene,
                root,
                &body,
                tx,
                by + title.height() + 8.0,
                Some(p.text_dim),
            );
        }

        let ringed = self.focused();
        for (hit, r) in self.targets(engine, viewport) {
            let role = match hit {
                Hit::Cancel => ButtonRole::Secondary,
                // The alternative is the destructive half of a
                // three-answer question; see `with_alternative`.
                Hit::Alternative => ButtonRole::Destructive,
                Hit::Confirm => self.confirm_role,
            };
            let label: &str = self.label_of(hit);
            button::draw_button(
                scene,
                root,
                engine,
                r,
                label,
                role,
                self.hover == Some(hit),
                ringed == Some(answer(hit)),
            );
        }
    }
}

#[cfg(test)]
mod tests;

/// The answer a button is. One function, so the pointer and the keyboard
/// cannot come to different conclusions about the same button.
fn answer(hit: Hit) -> Outcome {
    match hit {
        Hit::Cancel => Outcome::Cancel,
        Hit::Alternative => Outcome::Alternative,
        Hit::Confirm => Outcome::Confirm,
    }
}
