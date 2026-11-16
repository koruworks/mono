// SPDX-License-Identifier: MPL-2.0

//! Making a list where it lives: one field in the sidebar, one colour, Enter.
//!
//! A field drawn over the rail's "+ New list" row rather than a dialog hanging
//! off it — the approved design says so in as many words (*"A list is created
//! where it lives — one field in the sidebar, one colour picked from the
//! existing swatch set, Enter to keep it. No dialog, no settings page."*), and
//! it is the same shape [`WorkspaceRename`] already has: the row's rectangle
//! comes from the rail (`panes::explore::rail`), everything else is here, because
//! [`TextInput`] is the editor's and the panes keep to geometry.
//!
//! ## The colour is offered, not asked for
//!
//! The rail picks the first swatch no live list is wearing and draws it in the
//! row while the name is typed. A picker here would be the settings page the
//! design refuses; recolouring a list is a separate, later gesture on a list
//! that exists.
//!
//! ## A name that is taken
//!
//! Empty cancels — nothing typed is nothing meant, and a complaint about a box
//! you have not filled in is a scolding. **A new list typed with a sibling's
//! name selects that sibling**: nothing is made and nothing moves, because
//! what the person asked for already exists and is now in front of them. A
//! **rename** onto a sibling's name stays open with a line of red beside it,
//! `WorkspaceRename`'s rule: neither is a failure of anything, so neither
//! raises a dialog. A name is compared among the lists beside this one, not
//! across the workspace — two lists under different parents may share one.
//!
//! ## Renaming is the same field, one row up
//!
//! *Rename…* on a list's menu opens **this** box over the row the list is
//! already on, filled and selected — [`WorkspaceRename`]'s idiom, and one field
//! rather than two because everything the rename needs is what making one needs:
//! the same rectangle from the rail, the same duplicate check, the same
//! *outside commits* rule. [`Target`] is the whole of the difference, and it is
//! read once, at the commit, by the container.
//!
//! [`WorkspaceRename`]: super::workspace_rename::WorkspaceRename

use koru_core::ids::ListId;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use koru_editor::components::text_input::{InputStyle, TextInput};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// What the field wants done.
#[derive(PartialEq, Eq, Debug)]
pub(super) enum Outcome {
    /// Still typing.
    Keep,
    Cancel,
    /// Enter, on a name that is allowed.
    Commit(String),
}

/// Which list the field is about: one that does not exist yet, or one that
/// does.
///
/// **By id**, the menu subject's rule: the table under the field is pushed from
/// a document a peer also writes, so a row index would name whatever slid into
/// that position while the name was being typed.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum Target {
    /// A list that does not exist yet, under this parent (`None` is the top
    /// level).
    New(Option<ListId>),
    Existing(ListId),
}

pub(super) struct NewList {
    input: TextInput,
    target: Target,
    /// The swatch the list will wear, as an `AccentColor` token. Fixed when the
    /// field opened, so the square drawn beside it is the one it gets. On a
    /// rename it is the colour the list already has, and nothing writes it.
    color: String,
    hint: Option<&'static str>,
}

impl NewList {
    pub(super) fn new(engine: &mut TextEngine, color: &str, parent: Option<ListId>) -> Self {
        Self {
            input: TextInput::new(engine, theme::role::row()),
            target: Target::New(parent),
            color: color.to_owned(),
            hint: None,
        }
    }

    /// The same field over a list that exists, **filled and selected**:
    /// renaming usually means replacing, and the one case where it does not is
    /// one arrow key away (`WorkspaceRename::new`'s words, and its behaviour).
    pub(super) fn renaming(
        engine: &mut TextEngine,
        id: ListId,
        name: &str,
        color: Option<&str>,
    ) -> Self {
        let mut input = TextInput::with_text(engine, theme::role::row(), name);
        input.select_all(engine);
        Self {
            input,
            target: Target::Existing(id),
            color: color.unwrap_or_default().to_owned(),
            hint: None,
        }
    }

    pub(super) fn target(&self) -> Target {
        self.target
    }

    pub(super) fn color(&self) -> &str {
        &self.color
    }

    pub(super) fn hint(&self) -> Option<&'static str> {
        self.hint
    }

    /// `taken` is the names a **rename** may not take — the list's siblings'
    /// — so a duplicate can be refused without this module knowing what a
    /// workspace is. Empty for a new list: typing a sibling's name there
    /// selects that list instead of complaining (nested-lists §3, D8), which
    /// is the container's to do with the name this commits.
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
                let name = self.input.value();
                if name.is_empty() {
                    return Outcome::Cancel;
                }
                if Self::clashes(&name, taken) {
                    self.hint = Some(koru_i18n::t(
                        crate::format::lang(),
                        koru_i18n::Key::TaskListNameTaken,
                    ));
                    return Outcome::Keep;
                }
                Outcome::Commit(name)
            }
            Key::Named(NamedKey::Escape) => Outcome::Cancel,
            _ => {
                // Any keystroke clears the complaint: you are already fixing it.
                self.hint = None;
                self.input.key(engine, key, text, mods);
                Outcome::Keep
            }
        }
    }

    /// A press inside the field moves the caret; one outside commits.
    ///
    /// **Outside commits rather than cancels**, the rule every field in this
    /// app already follows — except where committing would break the rule Enter
    /// enforces, which is what the clash check is doing here.
    pub(super) fn mouse_down(
        &mut self,
        engine: &mut TextEngine,
        field: Rect,
        x: f32,
        y: f32,
        taken: &[String],
    ) -> Outcome {
        if !field.contains(x, y) {
            let name = self.input.value();
            return if name.is_empty() || Self::clashes(&name, taken) {
                Outcome::Cancel
            } else {
                Outcome::Commit(name)
            };
        }
        self.input.click(engine, field, x, y);
        Outcome::Keep
    }

    pub(super) fn mouse_move(&mut self, engine: &mut TextEngine, field: Rect, x: f32) -> bool {
        self.input.drag(engine, field, x)
    }

    pub(super) fn mouse_up(&mut self) {
        self.input.release();
    }

    /// Case-insensitively, and on the whole name: two lists called `Wedding`
    /// and `wedding` are indistinguishable in the one place either name is
    /// ever read, which is the rail.
    fn clashes(name: &str, taken: &[String]) -> bool {
        taken.iter().any(|t| t.eq_ignore_ascii_case(name))
    }

    /// Draw the field over the row, and the complaint under it.
    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        field: Rect,
    ) {
        let style = if self.hint.is_some() {
            InputStyle::inline().invalid()
        } else {
            InputStyle::inline()
        };
        self.input.draw(
            scene,
            root,
            engine,
            field,
            style,
            Some(koru_i18n::t(
                crate::format::lang(),
                koru_i18n::Key::TaskListNameHint,
            )),
            true,
        );
        if let Some(hint) = self.hint() {
            let label = engine.simple_layout(hint, theme::role::hint());
            scene::draw_layout_first_line(
                scene,
                root,
                &label,
                field.x + 2.0,
                field.y + field.h - 2.0,
                Some(theme::p().close_hover),
            );
        }
    }
}

#[cfg(test)]
mod tests;
