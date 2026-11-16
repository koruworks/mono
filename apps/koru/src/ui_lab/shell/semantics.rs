// SPDX-License-Identifier: MPL-2.0

//! **What the Lab's window offers a screen reader, and what a request to one
//! of its controls does.**
//!
//! The tree is the last frame's: the same [`Frame`] that painted the window
//! and answers the pointer gives each control its rectangle here, so where a
//! control is drawn, pressed and announced cannot disagree. A request comes
//! back as the press or the focus move the pointer and the keyboard already
//! make ([`Lab::activate`], [`Lab::set_focus`]); nothing here is a second way
//! to change the story.
//!
//! ```text
//! Window        UI Lab                       s:window#window
//! ├── Button    Minimize / Maximize / Close  s:window#minimize, #maximize, #close
//! ├── Button    Reset                        s:ui-lab#reset
//! ├── ListItem  an entry                     s:ui-lab.navigator#<catalog id>
//! ├── Button    the component under test     s:ui-lab#subject      named by its label
//! ├── Label     what pressing it did         s:ui-lab#result       spoken when it changes
//! ├── TextInput Label                        s:ui-lab.control#label     value = the text
//! └── Group     Style                        s:ui-lab.control#style
//!     └── RadioButton  a style               s:ui-lab.style#<style>     checked on the one worn
//! ```
//!
//! Identity is the element's own key, a code keyword and never a label, so a
//! locator is the same in every language. Only what a request can actually
//! do is advertised: Minimize and Maximize are named and placed but offer no
//! action, because the runner performs a window request only inside a pointer
//! or key event and a screen reader's request is neither.

use accesskit::{Action, ActionData, Live, Role as NativeRole};
use koru_ui::button::ButtonRole;
use koru_workspace_host::accessibility::{
    CaptionGeometry, ControlPart, ObjectKey, Occurrence, Resolved, Revision, Scope, SemanticNode,
    ViewSlot, WindowSemantics, WindowSlot, part, window_commands,
};

use super::*;

/// The surface the style control's options are identified under.
const STYLE_OPTIONS: &str = "ui-lab.style";
/// The line under the preview that says what pressing the button did.
const RESULT: Key = Key::surface("ui-lab", "result");

/// A dialog style's identity: a code keyword, the same in every language.
/// Exhaustive, so a style added to the production button cannot be offered
/// here without one.
const fn style_part(style: ButtonRole) -> &'static str {
    match style {
        ButtonRole::Secondary => "secondary",
        ButtonRole::Primary => "primary",
        ButtonRole::Destructive => "destructive",
    }
}

/// Which window and view an occurrence in this tree belongs to.
#[derive(Copy, Clone)]
struct Place {
    window: WindowSlot,
    view: ViewSlot,
}

impl Place {
    fn of(self, key: Key) -> Option<Occurrence> {
        match key.object {
            el::Object::Surface(surface) => Some(Occurrence::surface(
                self.window,
                self.view,
                surface,
                ControlPart::new(key.part),
            )),
            // The Lab's tree has no model objects: its content is synthetic.
            el::Object::Model(_) => None,
        }
    }
}

impl Lab {
    /// The client size the last frame was laid out at, in logical pixels.
    pub(in crate::ui_lab) fn frame_size(&self) -> Option<(f32, f32)> {
        self.laid.as_ref().map(|laid| laid.size)
    }

    /// Where the label field's caret is, in the window's logical pixels,
    /// while the field has the keyboard: where an input method should open
    /// its candidate list. `None` whenever the field is not being typed in.
    pub(in crate::ui_lab) fn ime_cursor_area(&self) -> Option<Rect> {
        let field = control_key(Control::Label);
        if self.focus != Some(field) || !self.active {
            return None;
        }
        self.label.ime_cursor_area(self.rect(field)?)
    }

    /// A number that changes when anything this window publishes changes,
    /// and not when the pointer merely moves: the state the tree is built
    /// from, folded together.
    pub(in crate::ui_lab) fn semantics_revision(&self) -> u64 {
        let mut revision = Revision::new();
        revision
            .str(self.entry.catalog_id)
            .str(self.story.label())
            .u64(self.story.presses())
            .u64(self.story.style_slot().map_or(u64::MAX, |slot| slot as u64))
            .bool(self.maximised)
            .bool(self.active);
        match self.focus {
            Some(Key {
                object: el::Object::Surface(surface),
                part,
            }) => revision.byte(1).str(surface).str(part),
            _ => revision.byte(0),
        };
        let (width, height) = self.frame_size().unwrap_or_default();
        revision.f32(width).f32(height);
        revision.finish()
    }

    /// Everything the window offers a screen reader, from the last frame.
    /// `None` before the first frame, when there is no layout to describe.
    pub(in crate::ui_lab) fn window_semantics(
        &self,
        window: WindowSlot,
        view: ViewSlot,
        scale: f64,
    ) -> Option<WindowSemantics> {
        let laid = self.laid.as_ref()?;
        let place = Place { window, view };
        let mut children = self.caption_commands(window);
        children.extend(
            laid.frame
                .semantics()
                .iter()
                .filter_map(|node| self.control_node(place, laid, node)),
        );
        children.extend(self.result_node(place, laid));
        Some(WindowSemantics {
            window,
            label: text(koru_i18n::Key::UiLabTitle).to_owned(),
            scale,
            client: Rect::new(0.0, 0.0, laid.size.0, laid.size.1),
            focus: self
                .focus
                .filter(|_| self.active)
                .and_then(|key| place.of(key)),
            children,
            scope: Scope::Whole,
            retired: Vec::new(),
        })
    }

    /// The window's own three controls, where the shared caption draws them.
    fn caption_commands(&self, window: WindowSlot) -> Vec<SemanticNode> {
        let rect_of = |action: caption::Action| {
            let caption = self.caption_rect()?;
            caption::direct_controls(caption)
                .into_iter()
                .find(|control| control.action == action)
                .map(|control| control.rect)
        };
        let mut commands = window_commands(
            window,
            self.maximised,
            koru_i18n::current(),
            CaptionGeometry {
                minimize: rect_of(caption::Action::Minimise),
                maximize: rect_of(caption::Action::ToggleMaximise),
                close: rect_of(caption::Action::Close),
            },
        );
        for command in &mut commands {
            // See the module note: only Close can be done from a request.
            if command.occurrence.part != part::CLOSE {
                command.actions.clear();
            }
        }
        commands
    }

    /// One element of the frame's own semantic tree, as the host publishes
    /// it.
    fn control_node(&self, place: Place, laid: &Laid, node: &el::SemNode) -> Option<SemanticNode> {
        let key = node.key?;
        let field = control_key(Control::Label);
        let role = if key == field {
            NativeRole::TextInput
        } else {
            match node.role {
                Role::Button => NativeRole::Button,
                Role::Heading => NativeRole::Heading,
                Role::List => NativeRole::List,
                Role::ListItem => NativeRole::ListItem,
                Role::Menu => NativeRole::Menu,
                Role::MenuItem => NativeRole::MenuItem,
                Role::Image => NativeRole::Image,
                Role::Group => NativeRole::Group,
            }
        };
        let mut result = SemanticNode::new(place.of(key)?, role).with_bounds(node.bounds);
        result = match &node.name {
            Some(el::Name::Key(name)) => result.with_label(text(*name)),
            Some(el::Name::Text(content)) => result.with_label(content.clone()),
            None => result,
        };
        result.disabled = node.disabled;
        result = result.with_actions(
            node.actions
                .iter()
                .filter(|action| !node.disabled || **action == el::Action::Focus)
                .map(|action| match action {
                    el::Action::Click => Action::Click,
                    el::Action::Focus => Action::Focus,
                }),
        );
        if key == field {
            // A value makes a client able to set it, so setting it is done.
            result = result
                .with_value(self.label.raw_text())
                .with_action(Action::SetValue);
        }
        let mut children: Vec<SemanticNode> = node
            .children
            .iter()
            .filter_map(|child| self.control_node(place, laid, child))
            .collect();
        if key == control_key(Control::Style) {
            children.extend(self.style_options(place, laid));
        }
        Some(result.with_children(children))
    }

    /// The style control's options, each where the shared segmented control
    /// drew it. The control paints them itself, so the frame has no element
    /// for one; its measured segments are the same rectangles the pointer is
    /// answered from.
    fn style_options(&self, place: Place, laid: &Laid) -> Vec<SemanticNode> {
        let Some(segments) = &laid.segments else {
            return Vec::new();
        };
        self.story
            .styles()
            .iter()
            .zip(&segments.segments)
            .map(|(style, rect)| {
                SemanticNode::new(
                    Occurrence::surface(
                        place.window,
                        place.view,
                        STYLE_OPTIONS,
                        ControlPart::new(style_part(*style)),
                    ),
                    NativeRole::RadioButton,
                )
                .with_label(text(registry::dialog_style_label(*style)))
                .with_bounds(*rect)
                .with_toggled(*style == self.story.style())
                .with_action(Action::Click)
            })
            .collect()
    }

    /// What pressing the button did, spoken when it changes: the only
    /// feedback a press has, so it must not be the one thing left unsaid.
    fn result_node(&self, place: Place, laid: &Laid) -> Option<SemanticNode> {
        let rect = laid.frame.rect_of("Click result")?;
        Some(
            SemanticNode::new(place.of(RESULT)?, NativeRole::Label)
                .with_label(self.result_line())
                .with_bounds(rect)
                .with_live(Live::Polite),
        )
    }

    /// A screen reader's request on one of this window's controls. Returns
    /// whether anything changed. A request naming a control the last frame
    /// does not show does nothing.
    pub(in crate::ui_lab) fn accessibility_action(
        &mut self,
        engine: &mut TextEngine,
        request: &Resolved,
    ) -> bool {
        let occurrence = request.occurrence;
        if occurrence == Occurrence::window_command(occurrence.window, part::CLOSE) {
            if request.action == Action::Click {
                self.closed = true;
                return true;
            }
            return false;
        }
        let ObjectKey::Surface(surface) = occurrence.object else {
            return false;
        };
        if surface == STYLE_OPTIONS {
            let slot = self
                .story
                .styles()
                .iter()
                .position(|style| style_part(*style) == occurrence.part.name());
            return match (request.action, slot) {
                (Action::Click, Some(slot)) => {
                    let focused = self.set_focus(engine, Some(control_key(Control::Style)));
                    self.story.choose(slot) || focused
                }
                _ => false,
            };
        }
        let key = Key::surface(surface, occurrence.part.name());
        let Some(laid) = self.laid.as_ref() else {
            return false;
        };
        if !laid.frame.contains(key) {
            return false;
        }
        let stop = laid.stops().contains(&key);
        match &request.action {
            Action::Focus => stop && self.set_focus(engine, Some(key)),
            Action::Click => {
                let focused = stop && self.set_focus(engine, Some(key));
                self.activate(engine, key) || focused
            }
            Action::SetValue if key == control_key(Control::Label) => {
                let Some(ActionData::Value(value)) = &request.data else {
                    return false;
                };
                let focused = self.set_focus(engine, Some(key));
                if self.label.raw_text() != &**value {
                    self.label.ime_cancel(engine);
                    self.label.set_text(engine, value);
                    self.label.move_to_end(engine);
                }
                self.adopt_label() || focused
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
