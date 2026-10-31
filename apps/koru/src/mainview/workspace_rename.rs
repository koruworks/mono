// SPDX-License-Identifier: MPL-2.0

//! Renaming a workspace in place, in its switcher row.
//!
//! A field drawn over the row rather than a prompt hanging off it: you are
//! editing a label in a list, and a panel that covers the list to do it makes
//! you lose your place. The row’s rectangle comes from the pane
//! (`popover_workspace_rect`); everything else is here, because [`TextField`] is
//! the editor's and the pane cannot see it.
//!
//! ## The refusal is inline
//!
//! Empty, or a name another workspace in this profile already has. Neither is a
//! failure of the filesystem, so neither raises the error dialog — the field
//! stays open with the text intact and a line of red beside it. A modal here
//! would take away the thing you were asked to fix.
//!
//! `NamePrompt` already had half of this: *"an empty name keeps the prompt open
//! rather than closing on a keystroke the user meant as done"*. This is that,
//! generalised to say **why**.

use koru_core::{DocumentId, ProfileId, WorkspaceId};
use koru_delegate_core::{ScheduleId, TextError};
use koru_delegate_schedule::ScheduleName;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use koru_editor::components::text_input::{InputStyle, TextInput};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// How tall the box is, whatever the row under it happens to be.
const HEIGHT: f32 = 26.0;

/// What the field wants done.
pub(super) enum Outcome {
    /// Still editing.
    Keep,
    Cancel,
    /// Enter, on a name that is allowed.
    Commit(String),
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum Target {
    Workspace(WorkspaceId),
    Profile(ProfileId),
    File(DocumentId),
    Schedule(ScheduleId),
    /// A chat in the Chats navigator, renamed in its row.
    Chat(koru_ai_ui::ChatId),
}

pub(super) struct WorkspaceRename {
    pub(super) target: Target,
    input: TextInput,
    /// Why the last Enter was refused, if it was. Cleared by the next keystroke
    /// — you are already fixing it, so the complaint has served its purpose.
    hint: Option<String>,
    sending: bool,
    schedule_expected: Option<ScheduleName>,
}

impl WorkspaceRename {
    /// The name box, for a dictation the container is routing.
    pub(super) fn dictation_field(&mut self) -> &mut TextInput {
        &mut self.input
    }

    pub(super) fn new(engine: &mut TextEngine, id: WorkspaceId, name: &str) -> Self {
        let mut input = TextInput::with_text(engine, theme::role::row(), name);
        // Selected, not just focused: renaming usually means replacing, and the
        // one case where it does not is one arrow key away.
        input.select_all(engine);
        Self {
            target: Target::Workspace(id),
            input,
            hint: None,
            sending: false,
            schedule_expected: None,
        }
    }

    pub(super) fn for_profile(engine: &mut TextEngine, id: ProfileId, name: &str) -> Self {
        let mut input = TextInput::with_text(engine, theme::role::row(), name);
        input.select_all(engine);
        Self {
            target: Target::Profile(id),
            input,
            hint: None,
            sending: false,
            schedule_expected: None,
        }
    }

    pub(super) fn for_file(engine: &mut TextEngine, id: DocumentId, name: &str) -> Self {
        let mut input = TextInput::with_text(engine, theme::role::row(), name);
        input.select_all(engine);
        Self {
            target: Target::File(id),
            input,
            hint: None,
            sending: false,
            schedule_expected: None,
        }
    }

    pub(super) fn for_schedule(engine: &mut TextEngine, id: ScheduleId, name: &str) -> Self {
        let mut input = TextInput::with_text(engine, theme::role::row(), name);
        input.select_all(engine);
        Self {
            target: Target::Schedule(id),
            input,
            hint: None,
            sending: false,
            schedule_expected: ScheduleName::new(name).ok(),
        }
    }

    /// A chat's row in the Chats navigator. The title is the chat's own;
    /// the rename it commits goes through the chat surface's rename command.
    pub(super) fn for_chat(engine: &mut TextEngine, id: koru_ai_ui::ChatId, title: &str) -> Self {
        let mut input = TextInput::with_text(engine, theme::role::row(), title);
        input.select_all(engine);
        Self {
            target: Target::Chat(id),
            input,
            hint: None,
            sending: false,
            schedule_expected: None,
        }
    }

    pub(super) fn schedule_expected(&self) -> Option<&ScheduleName> {
        self.schedule_expected.as_ref()
    }

    pub(super) fn refresh_schedule_expected(&mut self, name: ScheduleName) {
        self.schedule_expected = Some(name);
    }

    pub(super) fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    pub(super) fn set_sending(&mut self, sending: bool) {
        self.sending = sending;
    }

    pub(super) fn set_hint(&mut self, hint: impl Into<String>) {
        self.hint = Some(hint.into());
    }

    /// `taken` is every other workspace's name, so a duplicate can be refused
    /// without this module knowing what a profile is.
    pub(super) fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
        taken: &[String],
    ) -> Outcome {
        match key {
            Key::Named(NamedKey::Enter) => {
                if self.sending {
                    return Outcome::Keep;
                }
                let name = self.input.value();
                if matches!(self.target, Target::Schedule(_)) {
                    return match ScheduleName::new(name) {
                        Ok(name) => Outcome::Commit(name.as_str().to_owned()),
                        Err(error) => {
                            let key = match error {
                                TextError::Empty => koru_i18n::Key::DelegateScheduleNameNeeded,
                                TextError::TooLong { .. } => {
                                    koru_i18n::Key::DelegateScheduleNameTooLong
                                }
                                TextError::ControlCharacter => {
                                    koru_i18n::Key::DelegateScheduleNameControl
                                }
                            };
                            self.hint = Some(koru_i18n::t(crate::format::lang(), key).to_owned());
                            Outcome::Keep
                        }
                    };
                }
                // A chat's title is trimmed when it is sent, so one that is
                // only spaces is as empty as one that is nothing.
                let empty = match self.target {
                    Target::Chat(_) => name.trim().is_empty(),
                    _ => name.is_empty(),
                };
                if empty {
                    self.hint = Some(match self.target {
                        Target::Workspace(_) => "A workspace needs a name".to_owned(),
                        Target::Profile(_) => "A profile needs a name".to_owned(),
                        Target::File(_) => "A file needs a name".to_owned(),
                        Target::Chat(_) => {
                            koru_i18n::t(crate::format::lang(), koru_i18n::Key::ChatsNameNeeded)
                                .to_owned()
                        }
                        Target::Schedule(_) => unreachable!(),
                    });
                    return Outcome::Keep;
                }
                // Case-insensitively: two workspaces called `Research` and
                // `research` are indistinguishable in a list, which is the
                // only place either name is ever read.
                if matches!(self.target, Target::Workspace(_))
                    && taken.iter().any(|t| t.eq_ignore_ascii_case(&name))
                {
                    self.hint = Some("Another workspace already has that name".to_owned());
                    return Outcome::Keep;
                }
                Outcome::Commit(name)
            }
            Key::Named(NamedKey::Escape) if self.sending => Outcome::Keep,
            Key::Named(NamedKey::Escape) => Outcome::Cancel,
            _ => {
                if self.sending {
                    return Outcome::Keep;
                }
                // Any keystroke clears the complaint: you are already fixing it.
                self.hint = None;
                self.input.key(engine, key, text, mods);
                Outcome::Keep
            }
        }
    }

    /// A press inside the row moves the caret; one outside commits.
    ///
    /// **Outside commits rather than cancels.** Clicking away from a field you
    /// have typed into means "done" everywhere else in this app — the document
    /// title behaves the same way — and losing the edit would be the surprising
    /// reading.
    pub(super) fn mouse_down(
        &mut self,
        engine: &mut TextEngine,
        row: Rect,
        x: f32,
        y: f32,
        taken: &[String],
    ) -> Outcome {
        if self.sending {
            return Outcome::Keep;
        }
        if !row.contains(x, y) {
            let name = self.input.value();
            let clashes = taken.iter().any(|t| t.eq_ignore_ascii_case(&name));
            let invalid_schedule = matches!(self.target, Target::Schedule(_))
                && ScheduleName::new(name.clone()).is_err();
            return if name.is_empty() || clashes || invalid_schedule {
                // ...unless it would not have been allowed. Committing a name
                // that Enter refuses would make the mouse a way around the rule.
                Outcome::Cancel
            } else {
                Outcome::Commit(name)
            };
        }
        self.input.click(engine, Self::box_of(row), x, y);
        Outcome::Keep
    }

    pub(super) fn mouse_move(&mut self, engine: &mut TextEngine, row: Rect, x: f32) -> bool {
        self.input.drag(engine, Self::box_of(row), x)
    }

    pub(super) fn mouse_up(&mut self) {
        self.input.release();
    }

    pub(super) fn double_click(&mut self, engine: &mut TextEngine, row: Rect, x: f32, y: f32) {
        self.input.double_click(engine, Self::box_of(row), x, y);
    }

    /// The box, **centred** in whatever row it covers.
    ///
    /// A fixed height rather than the row's, because the two rows this appears
    /// over are different sizes — a switcher row is 26 px and a table row is 40 —
    /// and a field stretched to fill the taller one is a text box with a hand's
    /// width of empty above and below the words. One function, so the box that
    /// is drawn and the one clicks are measured against are the same rectangle.
    fn box_of(row: Rect) -> Rect {
        let h = HEIGHT.min(row.h);
        Rect::new(row.x, row.y + (row.h - h) * 0.5, row.w, h)
    }

    /// Draw the box and the field over the row, and the complaint under it.
    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        row: Rect,
    ) {
        let p = theme::p();
        // The box says it too: a red border, so the line under it confirms
        // rather than announces.
        let style = if self.hint.is_some() {
            InputStyle::inline().invalid()
        } else {
            InputStyle::inline()
        };
        self.input
            .draw(scene, root, engine, Self::box_of(row), style, None, true);
        // Under the row, in the red the app already uses for "this is the bad
        // one" — and only while it applies.
        if let Some(hint) = self.hint() {
            let label = engine.simple_layout(hint, theme::role::hint());
            scene::draw_layout_first_line(
                scene,
                root,
                &label,
                row.x + 2.0,
                row.y + row.h - 2.0,
                Some(p.close_hover),
            );
        }
    }
}

#[cfg(test)]
mod tests;
