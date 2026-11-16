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
//! ├── Button    Components / Properties      s:ui-lab#toggle-navigator, #toggle-properties
//! │                                          on while shown; described by its chord
//! ├── TextInput Search components            s:ui-lab#search           value = the query
//! ├── Label     nothing was found            s:ui-lab#no-results       spoken when it changes
//! ├── Button    Reset search                 s:ui-lab#reset-search     only while nothing is found
//! ├── ListBox   Components                   s:ui-lab#navigator        how many rows it has
//! │   ├── Button         a group             s:ui-lab.group#<group>    expanded or collapsed
//! │   └── ListBoxOption  a record            s:ui-lab.navigator#<catalog id>   selected, n of N
//! ├── ComboBox  Theme, Preview width, Language  s:ui-lab.setting#<setting>   value = the one in force
//! ├── Button    Reset                        s:ui-lab#reset
//! ├── Group     Details                      s:ui-lab#page
//! │   ├── Button    the component under test s:ui-lab#subject      a button named by its label,
//! │   │                                                            a text field with its value,
//! │   │                                                            or a group for a layout or table
//! │   │   └── a part of a widget             s:ui-lab.part#<part>      a button, switch, option,
//! │   │                                                            list field or value, where
//! │   │                                                            the component puts it
//! │   ├── Label     a token and its value    s:ui-lab.token#<token>    a token table's rows
//! │   ├── Label     what using it did        s:ui-lab#result       spoken when it changes
//! │   ├── Heading   the record's status      s:ui-lab#status
//! │   ├── Label     why nothing runs         s:ui-lab#status-reason
//! │   └── Label     a symbol                 s:ui-lab.symbol#<path>    its status and options
//! └── Group     Properties                   s:ui-lab#properties
//!     ├── Heading   the record's name        s:ui-lab.fact#name
//!     ├── Label     its purpose              s:ui-lab.fact#purpose
//!     ├── TextInput a free-text input        s:ui-lab.control#<input>   value = the text
//!     ├── Group     a choice among a few     s:ui-lab.control#<input>
//!     │   └── RadioButton  an option         s:ui-lab.<input>#<option>  checked on the one in force
//!     ├── Switch    a yes or a no            s:ui-lab.control#<input>   on or off
//!     ├── ComboBox  a choice among many      s:ui-lab.control#<input>   value = the one in force
//!     └── Label     each fact recorded       s:ui-lab.fact#<fact>
//! └── Menu      the list that is open        s:ui-lab#menu             named for its field
//!     └── MenuItemRadio  an option           s:ui-lab.option#<option>  checked on the one in force
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
use koru_workspace_host::accessibility::{
    CaptionGeometry, ControlPart, ObjectKey, Occurrence, Resolved, Revision, Scope, SemanticNode,
    ViewSlot, WindowSemantics, WindowSlot, part, window_commands,
};

use super::navigator::NOTHING_FOUND;
use super::page::{NAME, PURPOSE, REASON, STATUS, SYMBOL, facts, option_line, symbol_status};
use super::preview::{TOKEN, option_text};
use super::settings::{MENU, OPTION};
use super::*;

use crate::ui_lab::stories::PartRole;

/// The line under the preview that says what pressing the button did.
const RESULT: Key = Key::surface("ui-lab", "result");

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
        let columns = self.shown_columns();
        revision
            .bool(columns.navigator)
            .bool(columns.properties)
            .f32(columns.navigator_width);
        // The settings: what each is, and the list that is open.
        for setting in Setting::ALL {
            if let Some(choice) = self.chosen(setting.key()) {
                revision.str(choice.part);
            }
        }
        match &self.menu {
            Some(open) => revision
                .byte(1)
                .str(open.owner.part)
                .u64(open.menu.hover.map_or(u64::MAX, |slot| slot as u64)),
            None => revision.byte(0),
        };
        // The story is plain data: everything a person changed about it is
        // in how it prints.
        match &self.story {
            Some(story) => revision.byte(1).str(&format!("{story:?}")),
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
        Self::in_language(self.lang, || self.published(window, view, scale))
    }

    fn published(&self, window: WindowSlot, view: ViewSlot, scale: f64) -> Option<WindowSemantics> {
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
        children.extend(self.menu_node(place));
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

    /// The list that is open: a menu named for the field it hangs from, each
    /// option where the shared menu draws it, the one in force checked.
    fn menu_node(&self, place: Place) -> Option<SemanticNode> {
        let open = self.menu.as_ref()?;
        let viewport = self.frame_size()?;
        let options = open
            .choices
            .iter()
            .enumerate()
            .map(|(slot, choice)| {
                SemanticNode::new(
                    Occurrence::surface(
                        place.window,
                        place.view,
                        OPTION,
                        ControlPart::new(choice.part),
                    ),
                    NativeRole::MenuItemRadio,
                )
                .with_label(choice.label.clone())
                .with_bounds(open.menu.item_rect(viewport, slot))
                .with_toggled(choice.selected)
                .with_action(Action::Click)
            })
            .collect();
        let mut menu = SemanticNode::new(place.of(MENU)?, NativeRole::Menu)
            .with_bounds(open.menu.panel(viewport))
            .with_children(options);
        if let Some(setting) = Setting::of(open.owner) {
            menu = menu.with_label(text(setting.name()));
        } else if open.owner == SUBJECT {
            menu = menu.with_label(text(self.entry.name));
        } else if let Some(control) = self.control_of(open.owner) {
            menu = menu.with_label(text(control.name()));
        }
        Some(menu)
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
        let dropdown = self.is_dropdown(key);
        let toggle = self.toggle_of(key);
        let role = if field.is_some() {
            NativeRole::TextInput
        } else if toggle.is_some() {
            NativeRole::Switch
        } else if dropdown {
            // A field that shows one value and opens the list of the rest.
            NativeRole::ComboBox
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
        if dropdown && let Some(choice) = self.chosen(key) {
            result = result.with_value(choice.label);
        }
        if toggle.is_some() {
            result = result.with_toggled(node.selected == Some(true));
        }
        if let Some(side) = Side::ALL.into_iter().find(|side| side.toggle() == key) {
            // On while its column shows; a request toggles it, as a press
            // does. Its chord is said with it, written as this platform
            // writes it.
            result = result
                .with_toggled(node.selected == Some(true))
                .with_description(koru_keys::label(&side.chord()));
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
        if let Some(control) = self.segmented_of(key) {
            children.extend(self.style_options(place, laid, control));
        }
        if key == SUBJECT {
            children.extend(self.part_nodes(place, laid));
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
        // A token table's rows: each token's name and what it comes to.
        if let Some(Story::Tokens(table)) = &self.story {
            nodes.extend(table.rows().into_iter().filter_map(|row| {
                Self::text_node(
                    place,
                    laid,
                    PAGE,
                    Key::surface(TOKEN, row.name),
                    NativeRole::Label,
                    koru_i18n::fill(text(koru_i18n::Key::UiLabFactLine), &[row.name, &row.value]),
                )
            }));
        }
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

    /// A segmented choice's options, each where the shared segmented control
    /// drew it. The control paints them itself, so the frame has no element
    /// for one; its measured segments are the same rectangles the pointer is
    /// answered from.
    fn style_options(&self, place: Place, laid: &Laid, control: Control) -> Vec<SemanticNode> {
        let (Some(segments), Some(story)) = (laid.segments_of(control), self.story.as_ref()) else {
            return Vec::new();
        };
        story
            .options(control)
            .iter()
            .zip(&segments.segments)
            .map(|(option, rect)| {
                SemanticNode::new(
                    Occurrence::surface(
                        place.window,
                        place.view,
                        control.options_surface(),
                        ControlPart::new(option.part),
                    ),
                    NativeRole::RadioButton,
                )
                .with_label(option_text(&option.label))
                .with_bounds(*rect)
                .with_toggled(option.selected)
                .with_action(Action::Click)
            })
            .collect()
    }

    /// The parts of a widget story's component, each where the component's
    /// own geometry put it in the last frame: the rectangles it was painted
    /// in and the pointer is answered from.
    fn part_nodes(&self, place: Place, laid: &Laid) -> Vec<SemanticNode> {
        let clip = laid.frame.node(SUBJECT).and_then(|placed| placed.clip);
        laid.parts
            .iter()
            .map(|part| {
                let (role, toggled) = match part.role {
                    PartRole::Button => (NativeRole::Button, None),
                    PartRole::Pressed(on) => (NativeRole::Button, Some(on)),
                    PartRole::Switch(on) => (NativeRole::Switch, Some(on)),
                    PartRole::Radio(on) => (NativeRole::RadioButton, Some(on)),
                    PartRole::Combo => (NativeRole::ComboBox, None),
                    PartRole::Readout => (NativeRole::Label, None),
                };
                let mut node = SemanticNode::new(
                    Occurrence::surface(
                        place.window,
                        place.view,
                        PART,
                        ControlPart::new(part.part),
                    ),
                    role,
                )
                .with_label(part.name.clone())
                .with_bounds(clip.map_or(part.rect, |clip| within(part.rect, clip)));
                if let Some(value) = &part.value {
                    node = node.with_value(value.clone());
                }
                if let Some(on) = toggled {
                    node = node.with_toggled(on);
                }
                node.disabled = part.disabled;
                if part.pressable() {
                    node = node.with_action(Action::Click);
                }
                node
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
                .with_label(self.result_line()?)
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
        Self::in_language(self.lang, || self.requested(engine, request))
    }

    fn requested(&mut self, engine: &mut TextEngine, request: &Resolved) -> bool {
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
        if surface == OPTION {
            // An option of the list that is open, and of no other.
            let slot = self.menu.as_ref().and_then(|open| {
                open.choices
                    .iter()
                    .position(|choice| choice.part == occurrence.part.name())
            });
            return match (request.action, slot) {
                (Action::Click, Some(slot)) => self.choose_from_menu(engine, slot),
                _ => false,
            };
        }
        // Any other request is for the window under an open list: the list
        // closes first, as it does for a press outside it.
        self.close_menu();
        if surface == PART {
            // A part of the component under test: pressed as the pointer
            // presses it, the keyboard going to the component.
            let part = self.laid.as_ref().and_then(|laid| {
                laid.parts
                    .iter()
                    .position(|part| part.part == occurrence.part.name())
            });
            return match (request.action, part) {
                (Action::Click, Some(part)) => {
                    let focused = self.set_focus(engine, Some(SUBJECT));
                    self.press_part(engine, part) || focused
                }
                _ => false,
            };
        }
        if let Some((control, options)) = self
            .segmented_controls()
            .into_iter()
            .find(|(control, _)| control.options_surface() == surface)
        {
            let slot = options
                .iter()
                .position(|option| option.part == occurrence.part.name());
            return match (request.action, slot) {
                (Action::Click, Some(slot)) => {
                    let focused = self.set_focus(engine, Some(control_key(control)));
                    self.story
                        .as_mut()
                        .is_some_and(|story| story.choose(control, slot))
                        || focused
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
