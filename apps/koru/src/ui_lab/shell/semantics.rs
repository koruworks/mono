// SPDX-License-Identifier: MPL-2.0

//! **What the Lab's window offers a screen reader, and what a request to one
//! of its controls does.**
//!
//! The tree is the last frame's: the same [`Frame`] that painted the window
//! and answers the pointer gives each control its rectangle here, so where a
//! control is drawn, pressed and announced cannot disagree. A request comes
//! back as the press, the focus move or the scroll the pointer and the
//! keyboard already make ([`Lab::activate`], [`Lab::set_focus`],
//! [`Lab::scroll_to`]); nothing here is a second way to change anything.
//!
//! ```text
//! Window        UI Lab                       s:window#window
//! ├── Button    Minimize / Maximize / Close  s:window#minimize, #maximize, #close
//! ├── Button    Reset                        s:ui-lab#reset
//! ├── TextInput Search components            s:ui-lab#search           value = the query
//! ├── Label     nothing was found            s:ui-lab#no-results       spoken when it changes
//! ├── Button    Reset search                 s:ui-lab#reset-search     only while nothing is found
//! ├── ListBox   Components                   s:ui-lab#navigator        how many rows it has
//! │   ├── Button         a group             s:ui-lab.group#<group>    expanded or collapsed
//! │   └── ListBoxOption  a record            s:ui-lab.navigator#<catalog id>   selected, n of N
//! ├── Group     Details                      s:ui-lab#page
//! │   ├── Button    the component under test s:ui-lab#subject      named by its label
//! │   ├── Label     what pressing it did     s:ui-lab#result       spoken when it changes
//! │   ├── Heading   the record's status      s:ui-lab#status
//! │   ├── Label     why nothing runs         s:ui-lab#status-reason
//! │   └── Label     a symbol                 s:ui-lab.symbol#<path>    its status and options
//! └── Group     Properties                   s:ui-lab#properties
//!     ├── Heading   the record's name        s:ui-lab.fact#name
//!     ├── Label     its purpose              s:ui-lab.fact#purpose
//!     ├── TextInput Label                    s:ui-lab.control#label     value = the text
//!     ├── Group     Style                    s:ui-lab.control#style
//!     │   └── RadioButton  a style           s:ui-lab.style#<style>     checked on the one worn
//!     └── Label     each fact recorded       s:ui-lab.fact#<fact>
//! ```
//!
//! Identity is the element's own key, a code keyword and never a label, so a
//! locator is the same in every language. Only what a request can actually
//! do is advertised: Minimize and Maximize are named and placed but offer no
//! action, because the runner performs a window request only inside a pointer
//! or key event and a screen reader's request is neither.
//!
//! **Only what the frame holds is published.** A navigator row scrolled out
//! of view is not an element of the frame and so is not a node; the list
//! says how many rows it has and each row where it sits among them, and the
//! list's scroll actions bring the rest. Everything else a viewport holds is
//! a node whether or not it shows: placed where the viewport shows it, or as
//! an empty box on the edge it left by.

use accesskit::{Action, ActionData, Live, Role as NativeRole};
use koru_ui::button::ButtonRole;
use koru_workspace_host::accessibility::{
    CaptionGeometry, ControlPart, ObjectKey, Occurrence, Resolved, Revision, Scope, SemanticNode,
    ViewSlot, WindowSemantics, WindowSlot, part, window_commands,
};

use super::navigator::NOTHING_FOUND;
use super::page::{NAME, PURPOSE, REASON, STATUS, SYMBOL, facts, option_line, symbol_status};
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

/// The part of `rect` a viewport shows; `None` when it shows none of it.
fn showing(rect: Rect, viewport: Rect) -> Option<Rect> {
    let left = rect.x.max(viewport.x);
    let top = rect.y.max(viewport.y);
    let right = (rect.x + rect.w).min(viewport.x + viewport.w);
    let bottom = (rect.y + rect.h).min(viewport.y + viewport.h);
    (right > left && bottom > top).then(|| Rect::new(left, top, right - left, bottom - top))
}

/// Where something is, as far as its viewport shows it. What is scrolled
/// wholly out of view keeps its node, because a request to focus a control
/// is what brings it back and a line of text can still be read; it is placed
/// as an empty box on the edge it left by.
fn within(rect: Rect, viewport: Rect) -> Rect {
    showing(rect, viewport).unwrap_or_else(|| {
        let x = rect.x.clamp(viewport.x, viewport.x + viewport.w);
        let y = rect.y.clamp(viewport.y, viewport.y + viewport.h);
        Rect::new(x, y, 0.0, 0.0)
    })
}

impl Lab {
    /// The client size the last frame was laid out at, in logical pixels.
    pub(in crate::ui_lab) fn frame_size(&self) -> Option<(f32, f32)> {
        self.laid.as_ref().map(|laid| laid.size)
    }

    /// Where the caret of the field being typed in is, in the window's
    /// logical pixels: where an input method should open its candidate
    /// list. The search field and a property's field answer alike. `None`
    /// whenever no field has the keyboard.
    pub(in crate::ui_lab) fn ime_cursor_area(&self) -> Option<Rect> {
        let focus = self.focus.filter(|_| self.active)?;
        self.field(focus)?.ime_cursor_area(self.rect(focus)?)
    }

    /// A number that changes when anything this window publishes changes,
    /// and not when the pointer merely moves: the state the tree is built
    /// from, folded together.
    pub(in crate::ui_lab) fn semantics_revision(&self) -> u64 {
        let mut revision = Revision::new();
        revision
            .str(self.entry.catalog_id)
            .str(self.query())
            .bool(self.maximised)
            .bool(self.active);
        match &self.story {
            Some(story) => revision
                .byte(1)
                .str(story.label())
                .u64(story.presses())
                .u64(story.style_slot().map_or(u64::MAX, |slot| slot as u64)),
            None => revision.byte(0),
        };
        // The navigator: what it lists, how it is folded and what it shows.
        revision.u64(self.nav.rows().len() as u64);
        for family in Family::ALL {
            revision.bool(self.nav.is_folded(family));
        }
        revision
            .f32(self.nav.scroll)
            .f32(self.page_scroll)
            .f32(self.panel_scroll);
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
        for node in &laid.frame.semantics() {
            children.extend(self.control_node(place, laid, node));
            if node.key == Some(SEARCH) {
                // What the search found nothing for, said under the field it
                // was typed in.
                children.extend(self.nothing_found_node(place, laid));
            }
        }
        Some(WindowSemantics {
            window,
            label: text(koru_i18n::Key::UiLabTitle).to_owned(),
            scale,
            client: Rect::new(0.0, 0.0, laid.size.0, laid.size.1),
            // A row scrolled out of view has no node to be the focused one.
            focus: self
                .focus
                .filter(|key| self.active && laid.frame.contains(*key))
                .and_then(|key| place.of(key)),
            children,
            scope: Scope::Whole,
            retired: Vec::new(),
        })
    }

    /// The line that says a search found nothing, spoken when it appears and
    /// when what it quotes changes: typing is otherwise answered by silence.
    fn nothing_found_node(&self, place: Place, laid: &Laid) -> Option<SemanticNode> {
        let rect = laid.rect(NOTHING_FOUND)?;
        let label = koru_i18n::fill(text(koru_i18n::Key::UiLabNoResults), &[self.query().trim()]);
        Some(
            SemanticNode::new(place.of(NOTHING_FOUND)?, NativeRole::Label)
                .with_label(label)
                .with_bounds(rect)
                .with_live(Live::Polite),
        )
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
        let field = self.field(key);
        let row = self.nav.row_of(key);
        let role = if field.is_some() {
            NativeRole::TextInput
        } else if key == LIST {
            // A list one of whose items is the selected one.
            NativeRole::ListBox
        } else if matches!(row, Some(Row::Entry(_))) {
            NativeRole::ListBoxOption
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
        // A control inside a viewport is announced where the viewport shows
        // it, which is where the pointer can reach it.
        let bounds = match laid.frame.node(key).and_then(|placed| placed.clip) {
            Some(clip) => within(node.bounds, clip),
            None => node.bounds,
        };
        let mut result = SemanticNode::new(place.of(key)?, role).with_bounds(bounds);
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
        if let Some(field) = field {
            // A value makes a client able to set it, so setting it is done.
            result = result
                .with_value(field.raw_text())
                .with_action(Action::SetValue);
        }
        match row {
            // Selected or not is said of every record's row, and where it
            // sits among all the rows, in view or not.
            Some(row @ Row::Entry(entry)) => {
                result = result.with_selected(entry.catalog_id == self.entry.catalog_id);
                if let Some(at) = self.nav.rows().iter().position(|listed| *listed == row) {
                    result = result.with_position(at);
                }
            }
            Some(Row::Group(family)) => {
                result = result.with_description(text(if self.nav.is_folded(family) {
                    koru_i18n::Key::UiLabCollapsed
                } else {
                    koru_i18n::Key::UiLabExpanded
                }));
            }
            None => {}
        }
        if key == LIST {
            result = result.with_size_of_set(self.nav.rows().len());
        }
        if VIEWPORTS.contains(&key) {
            // Offered only while there is somewhere to scroll to.
            let (max, _) = laid.scroll_range(key).unwrap_or_default();
            let offset = self.scroll_of(key);
            if offset > 0.0 {
                result = result.with_action(Action::ScrollUp);
            }
            if offset < max {
                result = result.with_action(Action::ScrollDown);
            }
        }
        let mut children: Vec<SemanticNode> = Vec::new();
        if key == PANEL {
            children.extend(self.identity_nodes(place, laid));
        }
        children.extend(
            node.children
                .iter()
                .filter_map(|child| self.control_node(place, laid, child)),
        );
        if key == control_key(Control::Style) {
            children.extend(self.style_options(place, laid));
        }
        if key == PAGE {
            children.extend(self.page_nodes(place, laid));
        }
        if key == PANEL {
            children.extend(self.fact_nodes(place, laid));
        }
        Some(result.with_children(children))
    }

    /// A line of text the frame holds at `key`, as a node: where `viewport`
    /// shows it, and on the edge it left by once the viewport has scrolled
    /// past it. It stays a node either way, so a page can be read to its end
    /// without being scrolled, and what is published does not depend on how
    /// a language wraps.
    fn text_node(
        place: Place,
        laid: &Laid,
        viewport: Key,
        key: Key,
        role: NativeRole,
        label: String,
    ) -> Option<SemanticNode> {
        let rect = laid.rect(key)?;
        let viewport = laid.frame.node(viewport)?;
        let bounds = within(rect, viewport.viewport.unwrap_or(viewport.rect));
        Some(
            SemanticNode::new(place.of(key)?, role)
                .with_label(label)
                .with_bounds(bounds),
        )
    }

    /// What the properties column opens with: the record's name and its
    /// purpose.
    fn identity_nodes(&self, place: Place, laid: &Laid) -> Vec<SemanticNode> {
        let entry = self.entry;
        [
            (NAME, NativeRole::Heading, text(entry.name).to_owned()),
            (PURPOSE, NativeRole::Label, text(entry.purpose).to_owned()),
        ]
        .into_iter()
        .filter_map(|(key, role, label)| Self::text_node(place, laid, PANEL, key, role, label))
        .collect()
    }

    /// What is recorded about the record, a fact a node, as the column
    /// draws them under the story's inputs.
    fn fact_nodes(&self, place: Place, laid: &Laid) -> Vec<SemanticNode> {
        facts(self.entry)
            .into_iter()
            .filter_map(|fact| {
                let label = fact.spoken();
                Self::text_node(place, laid, PANEL, fact.key, NativeRole::Label, label)
            })
            .collect()
    }

    /// What the centre says beside the component: what using it did, or the
    /// record's status and the reason, then each symbol in view.
    fn page_nodes(&self, place: Place, laid: &Laid) -> Vec<SemanticNode> {
        let entry = self.entry;
        let mut nodes = Vec::new();
        nodes.extend(self.result_node(place, laid));
        let status = entry.status();
        nodes.extend(Self::text_node(
            place,
            laid,
            PAGE,
            STATUS,
            NativeRole::Heading,
            text(status.label()).to_owned(),
        ));
        if let Some(reason) = status.reason() {
            nodes.extend(Self::text_node(
                place,
                laid,
                PAGE,
                REASON,
                NativeRole::Label,
                text(reason).to_owned(),
            ));
        }
        nodes.extend(entry.symbols().filter_map(|symbol| {
            // The path is the name, as written; what is recorded beside it
            // is its description.
            let mut described = vec![symbol_status(&symbol)];
            described.extend(symbol.options.iter().map(option_line));
            Self::text_node(
                place,
                laid,
                PAGE,
                Key::surface(SYMBOL, symbol.symbol),
                NativeRole::Label,
                symbol.symbol.to_owned(),
            )
            .map(|node| node.with_description(described.join("; ")))
        }));
        nodes
    }

    /// The style control's options, each where the shared segmented control
    /// drew it. The control paints them itself, so the frame has no element
    /// for one; its measured segments are the same rectangles the pointer is
    /// answered from.
    fn style_options(&self, place: Place, laid: &Laid) -> Vec<SemanticNode> {
        let (Some(segments), Some(story)) = (&laid.segments, &self.story) else {
            return Vec::new();
        };
        story
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
                .with_toggled(*style == story.style())
                .with_action(Action::Click)
            })
            .collect()
    }

    /// What pressing the button did, spoken when it changes: the only
    /// feedback a press has, so it must not be the one thing left unsaid.
    fn result_node(&self, place: Place, laid: &Laid) -> Option<SemanticNode> {
        let rect = laid.frame.rect_of("Click result")?;
        let page = laid.frame.node(PAGE)?;
        let bounds = within(rect, page.viewport.unwrap_or(page.rect));
        Some(
            SemanticNode::new(place.of(RESULT)?, NativeRole::Label)
                .with_label(self.result_line())
                .with_bounds(bounds)
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
            let slot = self.story.as_ref().and_then(|story| {
                story
                    .styles()
                    .iter()
                    .position(|style| style_part(*style) == occurrence.part.name())
            });
            return match (request.action, slot) {
                (Action::Click, Some(slot)) => {
                    let focused = self.set_focus(engine, Some(control_key(Control::Style)));
                    self.story.as_mut().is_some_and(|story| story.choose(slot)) || focused
                }
                _ => false,
            };
        }
        let key = Key::surface(surface, occurrence.part.name());
        let Some(laid) = self.laid.as_ref() else {
            return false;
        };
        let Some(node) = laid.frame.node(key) else {
            return false;
        };
        // A control that is switched off does nothing when asked.
        let disabled = node.disabled;
        let stop = !disabled && self.takes_focus(key);
        match &request.action {
            Action::Focus => {
                let focused = stop && self.set_focus(engine, Some(key));
                if focused {
                    // As Tab does: a control cut off by its viewport is
                    // brought into it.
                    self.reveal(key);
                }
                focused
            }
            Action::Click if !disabled => {
                let focused = stop && self.set_focus(engine, Some(key));
                self.activate(engine, key) || focused
            }
            Action::ScrollDown | Action::ScrollUp if VIEWPORTS.contains(&key) => {
                let (_, viewport) = self.scroll_range(key).unwrap_or_default();
                let screen = (viewport - LINE_STEP).max(LINE_STEP);
                let offset = self.scroll_of(key);
                self.scroll_to(
                    key,
                    if request.action == Action::ScrollDown {
                        offset + screen
                    } else {
                        offset - screen
                    },
                )
            }
            Action::SetValue if self.field(key).is_some() => {
                let Some(ActionData::Value(value)) = &request.data else {
                    return false;
                };
                let focused = self.set_focus(engine, Some(key));
                self.set_field(engine, key, value) || focused
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
