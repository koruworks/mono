// SPDX-License-Identifier: MPL-2.0

//! **The first story's state**: the dialog button, pressed for real.
//!
//! Everything a person can change about the button under test lives here and
//! nowhere else — its label, which of the recipe's styles it wears, and how
//! many times it has been pressed. It is plain data with no window, text
//! engine or clock behind it, which is what lets the story be driven in a
//! test exactly as the window drives it.
//!
//! The styles it offers are the registration's ([`registry::Recipe::styles`]),
//! not a list of its own, so the story cannot offer a style the production
//! button does not have. Nothing here is saved: a reset, or closing the Lab,
//! is the end of it.

use koru_ui::button::ButtonRole;

use super::registry::{self, Control, Styles};

/// The button under test, and what has happened to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ButtonStory {
    /// The label a fresh or reset story starts with: synthetic content, in
    /// the display language the Lab opened in.
    default_label: String,
    label: String,
    styles: &'static [ButtonRole],
    controls: &'static [Control],
    style: ButtonRole,
    presses: u64,
}

impl ButtonStory {
    /// The registered dialog story, starting from `default_label`.
    pub(crate) fn new(default_label: &str) -> Self {
        let recipe = registry::story(registry::BUTTON_DIALOG_STORY).map(|(_, recipe)| recipe);
        let styles = match recipe.map(|recipe| recipe.styles) {
            Some(Styles::Dialog(styles)) => styles,
            Some(Styles::Compact(_)) | None => &[],
        };
        let controls = recipe.map_or(&[][..], |recipe| recipe.controls);
        Self {
            default_label: default_label.to_owned(),
            label: default_label.to_owned(),
            styles,
            controls,
            style: Self::default_style(styles),
            presses: 0,
        }
    }

    /// The first registered style, which the registration keeps equal to the
    /// production default.
    fn default_style(styles: &[ButtonRole]) -> ButtonRole {
        styles.first().copied().unwrap_or_default()
    }

    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    pub(crate) fn style(&self) -> ButtonRole {
        self.style
    }

    /// The styles on offer, in the registration's order.
    pub(crate) fn styles(&self) -> &'static [ButtonRole] {
        self.styles
    }

    /// The inputs the properties panel offers, in the registration's order.
    pub(crate) fn controls(&self) -> &'static [Control] {
        self.controls
    }

    /// Where the current style sits among [`styles`](Self::styles).
    pub(crate) fn style_slot(&self) -> Option<usize> {
        self.styles.iter().position(|style| *style == self.style)
    }

    pub(crate) fn presses(&self) -> u64 {
        self.presses
    }

    /// Take the label field's text, exactly as typed: an empty label is a
    /// real state of the production button and is shown as one.
    pub(crate) fn set_label(&mut self, label: &str) -> bool {
        if self.label == label {
            return false;
        }
        label.clone_into(&mut self.label);
        true
    }

    /// Wear the style at `slot`. A slot the registration does not have
    /// changes nothing.
    pub(crate) fn choose(&mut self, slot: usize) -> bool {
        match self.styles.get(slot) {
            Some(style) if *style != self.style => {
                self.style = *style;
                true
            }
            _ => false,
        }
    }

    /// Move to the next or previous style, stopping at either end: what the
    /// arrow keys do on the style control.
    pub(crate) fn step_style(&mut self, forward: bool) -> bool {
        let Some(slot) = self.style_slot() else {
            return false;
        };
        let next = if forward {
            slot + 1
        } else {
            match slot.checked_sub(1) {
                Some(previous) => previous,
                None => return false,
            }
        };
        self.choose(next)
    }

    /// The button was activated, by pointer or by keyboard.
    pub(crate) fn press(&mut self) {
        self.presses = self.presses.saturating_add(1);
    }

    /// Whether a reset would change anything.
    pub(crate) fn is_pristine(&self) -> bool {
        self.label == self.default_label
            && self.style == Self::default_style(self.styles)
            && self.presses == 0
    }

    /// Back to how the story opened: the default label and style, and no
    /// presses.
    pub(crate) fn reset(&mut self) -> bool {
        if self.is_pristine() {
            return false;
        }
        self.label.clone_from(&self.default_label);
        self.style = Self::default_style(self.styles);
        self.presses = 0;
        true
    }
}

#[cfg(test)]
mod tests;
