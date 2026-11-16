// SPDX-License-Identifier: MPL-2.0

//! **The Lab's own window**: the gallery, the record on show and its controls.
//!
//! [`Lab`] is a `koru_window::WindowApp` and nothing more: it is handed a
//! text engine, draws into a scene, and answers pointer and key events. It is
//! not a workspace view and is built from none of the workspace host's
//! startup, so it has no profile, no Store session, no settings file and no
//! background worker of its own. Closing the window ends the process.
//!
//! It is written against one window and stays that way. The process has a
//! second one, the inspector's panel, and [`super::windows`] is what runs the
//! two side by side: it hands this window's events and frames to `Lab`
//! unchanged, so the story's state is the same value whether or not the
//! inspector is open. What the window offers a screen reader is
//! [`semantics`]'s, read from the same frame.
//!
//! ```text
//! col  UI Lab                         the window, on the ground surface
//!   row  Caption                      the title; the shared window controls
//!   row  Toolbar                      the record's name; Reset
//!   row  Workbench
//!     col  Navigator                  the catalog              (navigator.rs)
//!     scroll  Preview                 the record's page        (page.rs)
//!     scroll  Properties              what is recorded, and a story's inputs
//! ```
//!
//! The surface is one element tree ([`koru_draw::el`]); every size in it is a
//! token ([`super::tokens`]). One layout gives the frame that paint, the
//! pointer and the keyboard all read, so where a control is drawn and where
//! it is pressed cannot disagree.
//!
//! **The controls are the production ones.** The button under test is
//! `koru_ui::button::draw_button`; the label is typed into a `TextInput`, the
//! style is chosen on a `Segmented`, Reset is an `ActionButton`, the three
//! viewports are the element layer's scroll element with the shared
//! scrollbar, and the window controls are the shared caption's. The tree
//! gives each a slot and this file draws the real control in it; nothing here
//! paints a control of its own.
//!
//! **Nothing happens for a record that is not on show.** Showing another
//! record drops the story that was running, edits and all, and builds the
//! new one; the catalog itself is constant data.

use koru_draw::el::{
    self, Align, El, Frame, FrameCx, Interaction, Key, Leaf, Length, Look, Ring, Role, StateRecipe,
    TextCache,
};
use koru_draw::geometry::Rect;
use koru_draw::scene;
use koru_draw::text::TextEngine;
use koru_draw::theme::{self, Ink, Line, Profile, Radius, Surface, alias, roles};
use koru_ui::button::{self, ActionButton, ActionStyle};
use koru_ui::{InputStyle, Segmented, TextInput, Typed, caption};
use koru_window::{ImeEvent, RichKeyEvent, WindowApp, WindowRequest};
use vello::Scene;
use vello::kurbo::Affine;
use winit::dpi::PhysicalSize;
use winit::keyboard::{Key as KeyboardKey, ModifiersState, NamedKey};
use winit::window::{CursorIcon, ResizeDirection, WindowAttributes};

use self::navigator::{LIST, Navigator, Row};
use self::page::{PAGE, PANEL};
use super::placement::Placement;
use super::registry::{self, Control, Entry, Family};
use super::story::ButtonStory;
use super::tokens::*;

/// The caption row: the band that drags the window and holds its controls.
const CAPTION: Key = Key::surface("ui-lab", "caption");
/// The toolbar's Reset.
const RESET: Key = Key::surface("ui-lab", "reset");
/// The panel the component under test stands on, and the box that clips it.
const STAGE: Key = Key::surface("ui-lab", "stage");
/// The component under test.
const SUBJECT: Key = Key::surface("ui-lab", "subject");
/// The segments inside the style control: where the shared control is drawn.
/// The control's own key, the one that is pressed and focused, is
/// [`control_key`]`(Control::Style)`.
const STYLE_SEGMENTS: Key = Key::surface("ui-lab", "style-segments");

/// The three viewports that scroll, front to back for a point in more than
/// one (there is never more than one, but the order is the answer if there
/// were).
const VIEWPORTS: [Key; 3] = [PANEL, PAGE, LIST];

/// How far an arrow key scrolls the page: a couple of lines of text.
const LINE_STEP: f32 = 40.0;

/// A property's control.
fn control_key(control: Control) -> Key {
    Key::surface("ui-lab.control", control.part())
}

/// **A navigator entry's states**: plain at rest, the hover plate under the
/// pointer, the selected plate while it is the one on show; the focus ring
/// while the keyboard is on it.
static ENTRY_STATES: StateRecipe = StateRecipe {
    name: "ui lab navigator entry",
    base: Look::NONE,
    selected: Look::bg(Surface::Selected),
    hover: Look::bg(Surface::Hover),
    pressed: Look::bg(Surface::Hover),
    selected_hover: Look::bg(Surface::Selected),
    selected_pressed: Look::bg(Surface::Selected),
    disabled: Look::NONE,
    focus: Some(Ring::Line(Line::Focus)),
};

/// **A frame that only shows the keyboard**: nothing of its own but the
/// focus ring. The shared segmented control and the scroll viewport draw no
/// focus state, so the element that holds each wears the frame's.
static FRAME_STATES: StateRecipe = StateRecipe {
    name: "ui lab focus frame",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::NONE,
    pressed: Look::NONE,
    selected_hover: Look::NONE,
    selected_pressed: Look::NONE,
    disabled: Look::NONE,
    focus: Some(Ring::Line(Line::Focus)),
};

/// **A slot for a production control**: it takes the control's own measured
/// size in the layout and paints nothing. The window draws the real control
/// in the rectangle the frame gives the slot, inside the slot's inspector
/// scope, because the control's state lives in the window and not in the
/// tree.
struct Slot {
    width: f32,
    height: f32,
}

impl Leaf for Slot {
    fn measure(&self, width: Option<f32>) -> (f32, f32) {
        (width.unwrap_or(self.width), self.height)
    }
}

/// What the last frame drew, kept so the pointer and the keyboard are
/// answered from the same layout the person is looking at.
struct Laid {
    frame: Frame,
    /// The style control's segments, measured where they were drawn.
    segments: Option<Segmented>,
    /// The client size the frame was laid out at, in logical pixels.
    size: (f32, f32),
}

impl Laid {
    fn rect(&self, key: Key) -> Option<Rect> {
        self.frame.node(key).map(|node| node.rect)
    }

    /// The keys Tab visits, in reading order. A control that is switched
    /// off is not one of them.
    fn stops(&self) -> Vec<Key> {
        self.frame
            .nodes
            .iter()
            .filter(|node| node.focusable && !node.disabled)
            .filter_map(|node| node.key)
            .collect()
    }

    /// How far `key`'s viewport can scroll, and how much of its content it
    /// shows at once. `None` when the frame has no such viewport.
    fn scroll_range(&self, key: Key) -> Option<(f32, f32)> {
        let node = self.frame.node(key)?;
        Some(match node.scrollbar {
            Some(bar) => (bar.max_scroll, bar.track_bottom - bar.track_top),
            // Everything fits: nowhere to scroll to.
            None => (0.0, node.viewport.unwrap_or(node.rect).h),
        })
    }
}

/// The UI Lab window.
pub(crate) struct Lab {
    placement: Placement,
    /// Set by the caption's ✕ or the system's close; the runner ends the
    /// process when it reads it.
    closed: bool,
    maximised: bool,
    /// Whether this window has the keyboard. A caret and a focus ring are
    /// drawn only while it does.
    active: bool,
    /// A caption control's request, taken by the runner after the event.
    request: Option<WindowRequest>,
    caption_hover: Option<caption::Action>,
    /// Where the pointer last was, for the wheel: a notch scrolls the
    /// viewport under it.
    pointer: Option<(f32, f32)>,
    /// The control under the pointer, and the style segment when it is one.
    hot: Option<Key>,
    style_hover: Option<usize>,
    /// The control the keyboard is on.
    focus: Option<Key>,
    /// The catalog's list: which groups are folded, where it is scrolled,
    /// which row the keyboard is on.
    nav: Navigator,
    /// The next frame scrolls the list to the record on show: set when the
    /// Lab opens, because only a layout knows how tall the list is.
    bring_shown_into_view: bool,
    /// A scrollbar's thumb being dragged: its viewport, and how far below
    /// the thumb's top it is held.
    scroll_drag: Option<(Key, f32)>,
    /// The record on show.
    entry: &'static Entry,
    /// The record's story, when it has one. Dropped, edits and all, when
    /// another record is shown.
    story: Option<ButtonStory>,
    /// The label property's field. It owns the text being edited — caret,
    /// selection, undo, composition — and the story is told after each edit.
    label: TextInput,
    /// Where the centre and the properties column are scrolled.
    page_scroll: f32,
    panel_scroll: f32,
    cache: TextCache,
    laid: Option<Laid>,
}

impl Lab {
    /// A Lab showing `entry`, its story at its defaults.
    pub(crate) fn new(
        engine: &mut TextEngine,
        placement: Placement,
        entry: &'static Entry,
    ) -> Self {
        let story = Self::story_for(entry);
        let label = Self::label_field(engine, story.as_ref());
        Self {
            placement,
            closed: false,
            maximised: false,
            active: true,
            request: None,
            caption_hover: None,
            pointer: None,
            hot: None,
            style_hover: None,
            focus: None,
            nav: Navigator::new(entry),
            bring_shown_into_view: true,
            scroll_drag: None,
            entry,
            story,
            label,
            page_scroll: 0.0,
            panel_scroll: 0.0,
            cache: TextCache::default(),
            laid: None,
        }
    }

    /// The story `entry`'s page runs, at its defaults: `None` for a record
    /// the registry lists nothing runnable for.
    fn story_for(entry: &'static Entry) -> Option<ButtonStory> {
        (entry.story_id() == Some(registry::BUTTON_DIALOG_STORY))
            .then(|| ButtonStory::new(text(koru_i18n::Key::UiLabButtonSampleLabel)))
    }

    /// The field the label property is typed in, holding the story's label.
    fn label_field(engine: &mut TextEngine, story: Option<&ButtonStory>) -> TextInput {
        TextInput::with_text(
            engine,
            theme::role::row(),
            story.map_or("", ButtonStory::label),
        )
    }

    fn caption_rect(&self) -> Option<Rect> {
        self.laid.as_ref()?.rect(CAPTION)
    }

    fn caption_action_at(&self, x: f32, y: f32) -> Option<caption::Action> {
        caption::direct_hit(self.caption_rect()?, x, y)
    }

    fn rect(&self, key: Key) -> Option<Rect> {
        self.laid.as_ref()?.rect(key)
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<Key> {
        self.laid.as_ref()?.frame.hit_at(x, y)
    }

    fn segment_at(&self, x: f32, y: f32) -> Option<usize> {
        self.laid.as_ref()?.segments.as_ref()?.segment_at(x, y)
    }

    /// What pressing the button has done so far, as the line under it.
    fn result_line(&self) -> String {
        let lang = koru_i18n::current();
        match self.story.as_ref().map_or(0, ButtonStory::presses) {
            0 => koru_i18n::t(lang, koru_i18n::Key::UiLabNotPressed).to_owned(),
            presses => koru_i18n::fill(
                koru_i18n::t(lang, koru_i18n::Key::UiLabPressCount),
                &[&koru_i18n::number::decimal(lang, presses)],
            ),
        }
    }

    /// The style control's option labels, in the registration's order.
    fn style_labels(&self) -> Vec<&'static str> {
        self.story
            .as_ref()
            .map_or(&[][..], ButtonStory::styles)
            .iter()
            .map(|style| text(registry::dialog_style_label(*style)))
            .collect()
    }

    /// Whether the keyboard can be on `key`: a Tab stop of the last frame,
    /// or any row of the navigator, which shares one stop between its rows.
    fn takes_focus(&self, key: Key) -> bool {
        self.nav.row_of(key).is_some()
            || self
                .laid
                .as_ref()
                .is_some_and(|laid| laid.stops().contains(&key))
    }

    /// Give the keyboard to `key`, or to nothing. Leaving the label field
    /// ends whatever was in progress in it: a drag, a selection, a
    /// composition.
    fn set_focus(&mut self, engine: &mut TextEngine, key: Option<Key>) -> bool {
        if self.focus == key {
            return false;
        }
        let field = control_key(Control::Label);
        if self.focus == Some(field) {
            self.label.ime_cancel(engine);
            self.label.blur(engine);
            self.adopt_label();
        }
        self.focus = key;
        if let Some(row) = key.and_then(|key| self.nav.row_of(key)) {
            self.nav.set_cursor(row);
        }
        true
    }

    /// Tell the story what the field now holds.
    fn adopt_label(&mut self) -> bool {
        let text = self.label.raw_text();
        self.story
            .as_mut()
            .is_some_and(|story| story.set_label(text))
    }

    /// The toolbar's Reset: the story back to how it opened, and the field
    /// showing the label it went back to. The record on show, the navigator
    /// and where anything is scrolled are left as they are.
    fn reset(&mut self, engine: &mut TextEngine) -> bool {
        let Some(story) = self.story.as_mut() else {
            return false;
        };
        let changed = story.reset();
        self.label.ime_cancel(engine);
        if self.label.raw_text() != story.label() {
            self.label.set_text(engine, story.label());
        }
        self.label.move_to_end(engine);
        changed
    }

    /// Show `entry`'s page. The story that was running is dropped with every
    /// edit made to it; the new record's story, when it has one, starts at
    /// its defaults.
    fn show(&mut self, engine: &mut TextEngine, entry: &'static Entry) -> bool {
        if entry.catalog_id == self.entry.catalog_id {
            return false;
        }
        self.label.ime_cancel(engine);
        self.entry = entry;
        self.story = Self::story_for(entry);
        self.label = Self::label_field(engine, self.story.as_ref());
        self.style_hover = None;
        self.page_scroll = 0.0;
        self.panel_scroll = 0.0;
        true
    }

    /// Fold or unfold a group. The keyboard stays on a row that still
    /// exists: a record that folded away hands it to its group's heading.
    fn fold(&mut self, engine: &mut TextEngine, family: Family, folded: bool) -> bool {
        if !self.nav.set_folded(family, folded) {
            return false;
        }
        if self
            .focus
            .is_some_and(|key| self.is_navigator_key(key) && self.nav.row_of(key).is_none())
        {
            self.set_focus(engine, Some(Row::Group(family).key()));
        }
        true
    }

    /// Whether `key` names a navigator row, on show or not.
    fn is_navigator_key(&self, key: Key) -> bool {
        matches!(
            key.object,
            el::Object::Surface("ui-lab.navigator" | "ui-lab.group")
        )
    }

    /// Do what a press on `key` does. A pointer press and Enter or Space on
    /// the focused control both come here, so they cannot disagree.
    fn activate(&mut self, engine: &mut TextEngine, key: Key) -> bool {
        if key == SUBJECT {
            match self.story.as_mut() {
                Some(story) => {
                    story.press();
                    true
                }
                None => false,
            }
        } else if key == RESET {
            self.reset(engine)
        } else {
            match self.nav.row_of(key) {
                Some(Row::Group(family)) => {
                    let folded = !self.nav.is_folded(family);
                    self.fold(engine, family, folded)
                }
                Some(Row::Entry(entry)) => self.show(engine, entry),
                // A control with nothing to activate.
                None => false,
            }
        }
    }

    /// Tab and Shift+Tab: the next or previous stop, round the ends. The
    /// navigator is one stop however many rows it has.
    fn move_focus(&mut self, engine: &mut TextEngine, forward: bool) -> bool {
        let Some(stops) = self.laid.as_ref().map(Laid::stops) else {
            return false;
        };
        if stops.is_empty() {
            return false;
        }
        let at = self.focus.and_then(|key| {
            stops.iter().position(|stop| *stop == key).or_else(|| {
                // A row the list scrolled past: the list's stop stands for
                // it.
                self.is_navigator_key(key)
                    .then(|| stops.iter().position(|stop| self.is_navigator_key(*stop)))
                    .flatten()
            })
        });
        let next = match (at, forward) {
            (Some(at), true) => (at + 1) % stops.len(),
            (Some(at), false) => (at + stops.len() - 1) % stops.len(),
            (None, true) => 0,
            (None, false) => stops.len() - 1,
        };
        let changed = self.set_focus(engine, Some(stops[next]));
        // Arriving at a field by keyboard selects what is in it, because the
        // next keystroke is most often a replacement.
        if changed && stops[next] == control_key(Control::Label) {
            self.label.select_all(engine);
        }
        if changed {
            self.reveal(stops[next]);
        }
        changed
    }

    /// Scroll `key`'s viewport so the control shows whole, when it is in one
    /// and is cut off.
    fn reveal(&mut self, key: Key) {
        if let Some(row) = self.nav.row_of(key) {
            if let Some((_, viewport)) = self.scroll_range(LIST) {
                self.nav.reveal(row, viewport);
            }
            return;
        }
        let Some(laid) = self.laid.as_ref() else {
            return;
        };
        if let Some((index, offset)) = laid.frame.reveal(key)
            && let Some(viewport) = laid.frame.nodes[index].key
        {
            self.scroll_to(viewport, offset);
        }
    }

    fn scroll_range(&self, key: Key) -> Option<(f32, f32)> {
        self.laid.as_ref()?.scroll_range(key)
    }

    fn scroll_of(&self, key: Key) -> f32 {
        if key == LIST {
            self.nav.scroll
        } else if key == PAGE {
            self.page_scroll
        } else if key == PANEL {
            self.panel_scroll
        } else {
            0.0
        }
    }

    /// Scroll `key`'s viewport to `offset`, held to what the last frame says
    /// it can scroll. Returns whether it moved.
    fn scroll_to(&mut self, key: Key, offset: f32) -> bool {
        let Some((max, _)) = self.scroll_range(key) else {
            return false;
        };
        let offset = offset.clamp(0.0, max);
        let slot = if key == LIST {
            &mut self.nav.scroll
        } else if key == PAGE {
            &mut self.page_scroll
        } else if key == PANEL {
            &mut self.panel_scroll
        } else {
            return false;
        };
        let changed = *slot != offset;
        *slot = offset;
        changed
    }

    /// The viewport a point of the last frame is in.
    fn viewport_at(&self, x: f32, y: f32) -> Option<Key> {
        let laid = self.laid.as_ref()?;
        VIEWPORTS.into_iter().find(|key| {
            laid.frame
                .node(*key)
                .is_some_and(|node| node.viewport.unwrap_or(node.rect).contains(x, y))
        })
    }

    /// The scrollbar a press at a point of the last frame takes hold of:
    /// its viewport, and how far below the thumb's top it is held.
    fn scrollbar_at(&self, x: f32, y: f32) -> Option<(Key, f32)> {
        let laid = self.laid.as_ref()?;
        VIEWPORTS.into_iter().find_map(|key| {
            let bar = laid.frame.scrollbar(key)?;
            bar.in_gutter(x, y).then(|| (key, bar.grab_at(x, y)))
        })
    }

    /// Move the held thumb to the pointer.
    fn drag_scroll(&mut self, y: f32) -> bool {
        let Some((key, grab)) = self.scroll_drag else {
            return false;
        };
        let offset = self
            .laid
            .as_ref()
            .and_then(|laid| laid.frame.drag_offset(key, y, grab));
        offset.is_some_and(|offset| self.scroll_to(key, offset))
    }

    /// A press at a point of the last frame: focus follows it, then it does
    /// whatever that control does.
    fn press_at(&mut self, engine: &mut TextEngine, x: f32, y: f32, double: bool) -> bool {
        match self.caption_action_at(x, y) {
            Some(caption::Action::Minimise) => {
                self.request = Some(WindowRequest::Minimise);
                return true;
            }
            Some(caption::Action::ToggleMaximise) => {
                self.request = Some(WindowRequest::ToggleMaximise);
                return true;
            }
            Some(caption::Action::Close) => {
                self.closed = true;
                return true;
            }
            // The Lab's caption has no window menu.
            Some(caption::Action::WindowMenu) | None => {}
        }
        // A scrollbar is pressed before whatever is under it.
        if let Some(held) = self.scrollbar_at(x, y) {
            self.scroll_drag = Some(held);
            self.drag_scroll(y);
            return true;
        }
        let key = self.hit_at(x, y);
        let stop = key.filter(|key| self.takes_focus(*key));
        let mut changed = self.set_focus(engine, stop);
        let Some(key) = key else {
            return changed;
        };
        if key == control_key(Control::Label) {
            if let Some(rect) = self.rect(key) {
                changed |= if double {
                    self.label.double_click(engine, rect, x, y)
                } else {
                    self.label.click(engine, rect, x, y)
                };
            }
        } else if key == control_key(Control::Style) {
            if let Some(slot) = self.segment_at(x, y)
                && let Some(story) = self.story.as_mut()
            {
                changed |= story.choose(slot);
            }
        } else {
            changed |= self.activate(engine, key);
        }
        changed
    }

    /// A key while the keyboard is on a row of the navigator.
    fn navigator_key(
        &mut self,
        engine: &mut TextEngine,
        row: Row,
        key: &KeyboardKey,
        repeat: bool,
    ) -> bool {
        self.nav.set_cursor(row);
        let page = self.scroll_range(LIST).map_or(1, |(_, viewport)| {
            ((viewport / NAVIGATOR_ROW.value).floor() as isize - 1).max(1)
        });
        let target = match key {
            KeyboardKey::Named(NamedKey::ArrowDown) => self.nav.stepped(1),
            KeyboardKey::Named(NamedKey::ArrowUp) => self.nav.stepped(-1),
            KeyboardKey::Named(NamedKey::PageDown) => self.nav.stepped(page),
            KeyboardKey::Named(NamedKey::PageUp) => self.nav.stepped(-page),
            KeyboardKey::Named(NamedKey::Home) => self.nav.first(),
            KeyboardKey::Named(NamedKey::End) => self.nav.last(),
            // Right unfolds a group, then steps into it; Left steps out of a
            // group, then folds it.
            KeyboardKey::Named(NamedKey::ArrowRight) => match row {
                Row::Group(family) if self.nav.is_folded(family) => {
                    return self.fold(engine, family, false);
                }
                Row::Group(family) => self.nav.first_in(family),
                Row::Entry(_) => None,
            },
            KeyboardKey::Named(NamedKey::ArrowLeft) => match row {
                Row::Group(family) if !self.nav.is_folded(family) => {
                    return self.fold(engine, family, true);
                }
                Row::Group(_) => None,
                Row::Entry(entry) => Some(Row::Group(entry.family)),
            },
            KeyboardKey::Named(NamedKey::Enter | NamedKey::Space) if !repeat => {
                return self.activate(engine, row.key());
            }
            KeyboardKey::Named(NamedKey::Escape) => return self.set_focus(engine, None),
            _ => None,
        };
        let Some(target) = target.filter(|target| *target != row) else {
            return false;
        };
        self.set_focus(engine, Some(target.key()));
        self.reveal(target.key());
        true
    }

    /// A key while the keyboard is on the page: it scrolls.
    fn page_key(&mut self, engine: &mut TextEngine, key: &KeyboardKey) -> bool {
        let (max, viewport) = self.scroll_range(PAGE).unwrap_or_default();
        let screen = (viewport - LINE_STEP).max(LINE_STEP);
        let offset = match key {
            KeyboardKey::Named(NamedKey::ArrowDown) => self.page_scroll + LINE_STEP,
            KeyboardKey::Named(NamedKey::ArrowUp) => self.page_scroll - LINE_STEP,
            KeyboardKey::Named(NamedKey::PageDown) => self.page_scroll + screen,
            KeyboardKey::Named(NamedKey::PageUp) => self.page_scroll - screen,
            KeyboardKey::Named(NamedKey::Home) => 0.0,
            KeyboardKey::Named(NamedKey::End) => max,
            KeyboardKey::Named(NamedKey::Escape) => return self.set_focus(engine, None),
            _ => return false,
        };
        self.scroll_to(PAGE, offset)
    }

    /// A key going down, with whether it is the keyboard repeating a held
    /// one. A held Enter must not press a button thirty times a second, so
    /// activation takes the first press only; typing and moving repeat.
    fn key_down(
        &mut self,
        engine: &mut TextEngine,
        key: &KeyboardKey,
        typed: Option<&str>,
        mods: ModifiersState,
        repeat: bool,
    ) -> bool {
        // Ctrl+I arrives as Tab on Windows; only a bare or shifted Tab moves
        // the focus.
        let chord = mods.control_key() || mods.alt_key() || mods.super_key();
        if matches!(key, KeyboardKey::Named(NamedKey::Tab)) && !chord {
            return self.move_focus(engine, !mods.shift_key());
        }
        let Some(focus) = self.focus else {
            return false;
        };
        if focus == control_key(Control::Label) {
            return match self.label.key(engine, key, typed, mods) {
                // The caret may have moved without the text changing; either
                // way the field has to be drawn again.
                Typed::Handled => {
                    self.adopt_label();
                    true
                }
                Typed::Passed if matches!(key, KeyboardKey::Named(NamedKey::Escape)) => {
                    self.set_focus(engine, None)
                }
                Typed::Passed => false,
            };
        }
        if chord {
            return false;
        }
        if let Some(row) = self.nav.row_of(focus) {
            return self.navigator_key(engine, row, key, repeat);
        }
        if focus == PAGE {
            return self.page_key(engine, key);
        }
        if focus == control_key(Control::Style) {
            let Some(story) = self.story.as_mut() else {
                return false;
            };
            return match key {
                KeyboardKey::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => {
                    story.step_style(true)
                }
                KeyboardKey::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => {
                    story.step_style(false)
                }
                KeyboardKey::Named(NamedKey::Escape) => self.set_focus(engine, None),
                _ => false,
            };
        }
        match key {
            KeyboardKey::Named(NamedKey::Enter | NamedKey::Space) if !repeat => {
                self.activate(engine, focus)
            }
            KeyboardKey::Named(NamedKey::Escape) => self.set_focus(engine, None),
            _ => false,
        }
    }

    /// The window as elements, for a client area `width` by `height`. The
    /// width bounds how wide a story's component may grow before it would
    /// leave its column; the height bounds how many navigator rows can be in
    /// view.
    fn tree(&self, engine: &mut TextEngine, width: f32, height: f32) -> El {
        let entry = self.entry;
        let caption = el::row("Caption")
            .id(CAPTION)
            .h(CAPTION_HEIGHT)
            .pad_start(CAPTION_INSET)
            .items_center()
            .child(
                el::text(text(koru_i18n::Key::UiLabTitle))
                    .named("Window title")
                    .role(roles::ROW)
                    .ink(Ink::Secondary)
                    .fill()
                    .truncate(),
            )
            .child(el::boxed("Window controls").w(CAPTION_CONTROLS));

        let reset = text(koru_i18n::Key::UiLabReset);
        let toolbar = el::row("Toolbar")
            .pad_x(TOOLBAR_INSET_X)
            .pad_y(TOOLBAR_INSET_Y)
            .gap(TOOLBAR_GAP)
            .items_center()
            .child(
                el::text(text(entry.name))
                    .named("Story title")
                    .role(roles::ROW)
                    .emphasis(true)
                    .fill()
                    .truncate(),
            )
            .child(
                el::leaf(
                    "Reset",
                    Slot {
                        width: ActionButton::new(reset, ActionStyle::Secondary, None).width(engine),
                        height: ActionButton::H,
                    },
                )
                .semantic()
                .id(RESET)
                .hit()
                .focusable()
                // Nothing to put back for a record with no story.
                .disabled(self.story.is_none())
                .semantics(Role::Button)
                .name(koru_i18n::Key::UiLabReset)
                .action(el::Action::Click)
                .action(el::Action::Focus),
            );

        el::col("UI Lab")
            .bg(Surface::Ground)
            .child(caption)
            .child(el::divider(Line::Divider))
            .child(toolbar)
            .child(el::divider(Line::Divider))
            .child(
                el::row("Workbench")
                    .fill()
                    .child(self.navigator(height))
                    .child(el::divider(Line::Divider))
                    .child(self.page(engine, width))
                    .child(el::divider(Line::Divider))
                    .child(self.properties(engine)),
            )
    }

    /// The left column: its heading, and the catalog under it.
    fn navigator(&self, height: f32) -> El {
        el::col("Navigator")
            .w(NAVIGATOR_WIDTH)
            .pad(alias::NAV_PAD)
            .gap(NAVIGATOR_GAP)
            .child(
                el::text(text(koru_i18n::Key::UiLabNavigator))
                    .named("Navigator heading")
                    .role(roles::POPOVER_HEADING)
                    .truncate(),
            )
            .child(self.nav.element(self.entry, height))
    }

    /// One property's control, by what kind of input it is.
    fn control(&self, engine: &mut TextEngine, control: Control) -> El {
        let key = control_key(control);
        match control {
            Control::Label => el::leaf(
                "Label field",
                Slot {
                    width: 0.0,
                    height: FIELD_HEIGHT.value,
                },
            )
            .semantic()
            .h(FIELD_HEIGHT)
            .id(key)
            .hit()
            .focusable()
            // The element layer has no text-field role; the window's
            // semantics publish this node as one, with its value.
            .semantics(Role::Group)
            .name(control.name())
            .action(el::Action::Focus),
            Control::Style => el::boxed("Style choice")
                .align_self(Align::Start)
                .pad(FOCUS_INSET)
                .radius(Radius::M)
                .states(&FRAME_STATES)
                .id(key)
                .hit()
                .focusable()
                .semantics(Role::Group)
                .name(control.name())
                .action(el::Action::Focus)
                .child(
                    el::leaf(
                        "Style segments",
                        Slot {
                            width: Segmented::width(engine, &self.style_labels()),
                            height: Segmented::H,
                        },
                    )
                    .decorative()
                    .id(STYLE_SEGMENTS),
                ),
        }
    }

    /// Take the offsets the layout clamped. Returns whether the frame can
    /// stand: the navigator chose its rows from its offset before the layout
    /// could clamp it, so a clamped one means the rows must be chosen again.
    fn adopt_offsets(&mut self, frame: &Frame) -> bool {
        let clamped = |key: Key| frame.node(key).and_then(|node| node.scroll).map(|s| s.1);
        if let Some(offset) = clamped(PAGE) {
            self.page_scroll = offset;
        }
        if let Some(offset) = clamped(PANEL) {
            self.panel_scroll = offset;
        }
        match clamped(LIST) {
            Some(offset) if offset != self.nav.scroll => {
                self.nav.scroll = offset;
                false
            }
            _ => true,
        }
    }
}

/// A string of the Lab's, in the display language.
fn text(key: koru_i18n::Key) -> &'static str {
    koru_i18n::t(koru_i18n::current(), key)
}

/// The runner's composition event, as the shared field takes it.
fn composition(event: ImeEvent<'_>) -> winit::event::Ime {
    match event {
        ImeEvent::Enabled => winit::event::Ime::Enabled,
        ImeEvent::Preedit { text, cursor } => winit::event::Ime::Preedit(text.to_owned(), cursor),
        ImeEvent::Commit { text } => winit::event::Ime::Commit(text.to_owned()),
        ImeEvent::Disabled => winit::event::Ime::Disabled,
    }
}

impl WindowApp for Lab {
    type Event = ();

    fn window(&self) -> WindowAttributes {
        self.placement.attributes(text(koru_i18n::Key::UiLabTitle))
    }

    fn should_exit(&self) -> bool {
        self.closed
    }

    fn close_requested(&mut self) {
        self.closed = true;
    }

    fn is_caption(&self, x: f32, y: f32) -> bool {
        self.caption_rect()
            .is_some_and(|caption| caption.contains(x, y))
            && self.caption_action_at(x, y).is_none()
    }

    fn cursor_icon(&self, x: f32, y: f32) -> CursorIcon {
        if self.hit_at(x, y) == Some(control_key(Control::Label)) {
            CursorIcon::Text
        } else {
            koru_ui::cursor::PRESS
        }
    }

    fn resize_direction(
        &self,
        width: f32,
        height: f32,
        x: f32,
        y: f32,
        maximised: bool,
    ) -> Option<ResizeDirection> {
        koru_window::resize::edge(width, height, x, y, maximised)
    }

    fn resized(&mut self, _size: PhysicalSize<u32>, _scale_factor: f64) {
        // The next frame is laid out at the size the runner hands `draw`;
        // until then the last frame no longer matches the window.
        self.laid = None;
        self.hot = None;
        self.style_hover = None;
        self.caption_hover = None;
        self.scroll_drag = None;
    }

    fn paint_before_showing(&self) -> bool {
        true
    }

    fn take_window_request(&mut self) -> Option<WindowRequest> {
        self.request.take()
    }

    fn set_maximised(&mut self, maximised: bool) -> bool {
        let changed = self.maximised != maximised;
        self.maximised = maximised;
        changed
    }

    fn focus_changed(&mut self, focused: bool) -> bool {
        if self.active == focused {
            return false;
        }
        self.active = focused;
        if !focused {
            // The button that began a drag was released somewhere else.
            self.label.release();
            self.scroll_drag = None;
        }
        true
    }

    fn wants_drag(&self) -> bool {
        // A drag extends the selection in the label field and moves a
        // scrollbar's thumb.
        true
    }

    fn pointer_moved(&mut self, x: f32, y: f32) -> bool {
        self.pointer = Some((x, y));
        let caption = self.caption_action_at(x, y);
        let hot = self.hit_at(x, y);
        let segment = (hot == Some(control_key(Control::Style)))
            .then(|| self.segment_at(x, y))
            .flatten();
        let changed =
            self.caption_hover != caption || self.hot != hot || self.style_hover != segment;
        self.caption_hover = caption;
        self.hot = hot;
        self.style_hover = segment;
        changed
    }

    fn pointer_left(&mut self) -> bool {
        let changed =
            self.caption_hover.is_some() || self.hot.is_some() || self.style_hover.is_some();
        self.pointer = None;
        self.caption_hover = None;
        self.hot = None;
        self.style_hover = None;
        changed
    }

    fn click(&mut self, engine: &mut TextEngine, x: f32, y: f32, _ctrl: bool) -> bool {
        self.press_at(engine, x, y, false)
    }

    /// The second of two quick presses. In the label field it selects the
    /// word; anywhere else it is simply another press, so pressing the button
    /// twice quickly counts twice.
    fn double_click(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.press_at(engine, x, y, true)
    }

    fn drag(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        if self.scroll_drag.is_some() {
            self.pointer = Some((x, y));
            return self.drag_scroll(y);
        }
        if self.label.is_dragging() {
            let field = control_key(Control::Label);
            return self
                .rect(field)
                .is_some_and(|rect| self.label.drag(engine, rect, x));
        }
        self.pointer_moved(x, y)
    }

    fn mouse_up(&mut self) -> bool {
        self.label.release();
        self.scroll_drag = None;
        false
    }

    /// A wheel notch scrolls the viewport under the pointer, and nothing
    /// when the pointer is over none.
    fn scroll_by(&mut self, delta: f32, _mods: ModifiersState) -> bool {
        let Some((x, y)) = self.pointer else {
            return false;
        };
        let Some(key) = self.viewport_at(x, y) else {
            return false;
        };
        let offset = self.scroll_of(key) - delta;
        self.scroll_to(key, offset)
    }

    fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &KeyboardKey,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        self.key_down(engine, key, text, mods, false)
    }

    fn rich_key(&mut self, engine: &mut TextEngine, event: RichKeyEvent<'_>) -> bool {
        if !event.state.is_pressed() {
            return false;
        }
        self.key_down(
            engine,
            event.logical_key,
            event.text,
            event.modifiers,
            event.repeat,
        )
    }

    fn wants_ime(&self) -> bool {
        // The label field takes composed text. The runner asks once, when the
        // window is created, so this cannot follow the focus.
        true
    }

    fn ime(&mut self, engine: &mut TextEngine, event: ImeEvent<'_>) -> bool {
        if self.focus != Some(control_key(Control::Label)) {
            return false;
        }
        let changed = self.label.ime(engine, &composition(event));
        // A composition in progress is not the label yet: the field's
        // authored text only changes when one is committed.
        self.adopt_label();
        changed
    }

    fn draw(&mut self, scene: &mut Scene, engine: &mut TextEngine, w: f32, h: f32) {
        // Once, as a rule. Again when the layout clamped the navigator's
        // offset, because its rows were chosen for the offset it had before;
        // when what is under a pointer that has not moved is no longer what
        // was, because the content scrolled under it; and once more for the
        // first frame, which is the first to know how tall the list is and
        // so where the record on show sits in it.
        let mut passes = 0;
        let (tree, frame) = loop {
            let interaction = Interaction {
                hot: self.hot,
                pressed: None,
                focused: self.focus.filter(|_| self.active),
            };
            let tree = self.tree(engine, w, h);
            let frame = tree.layout(
                engine,
                &mut self.cache,
                Rect::new(0.0, 0.0, w, h),
                FrameCx::root().with_interaction(interaction),
            );
            passes += 1;
            let mut settled = self.adopt_offsets(&frame);
            if std::mem::take(&mut self.bring_shown_into_view)
                && let Some(viewport) = frame.node(LIST).and_then(|node| node.viewport)
            {
                settled &= !self.nav.centre(Row::Entry(self.entry), viewport.h);
            }
            if let Some((x, y)) = self.pointer
                && self.scroll_drag.is_none()
            {
                let hot = frame.hit_at(x, y);
                settled &= hot == self.hot;
                self.hot = hot;
            }
            if settled || passes == 3 {
                break (tree, frame);
            }
        };
        // The keyboard cannot stay on a control this frame no longer has. A
        // navigator row scrolled out of view is still there.
        if let Some(key) = self.focus
            && !frame.contains(key)
            && self.nav.row_of(key).is_none()
        {
            self.focus = None;
        }
        let labels = self.style_labels();
        let segments = frame
            .node(STYLE_SEGMENTS)
            .map(|node| Segmented::layout(engine, &labels, node.rect.x, node.rect.y));
        let stage = frame.node(STAGE).map(|node| node.rect);
        let caption_state = caption::State {
            hovered: self.caption_hover,
            maximized: self.maximised,
            menu: caption::MenuVisual::default(),
        };
        let reset = text(koru_i18n::Key::UiLabReset);
        let field = control_key(Control::Label);
        let Self {
            story,
            label,
            style_hover,
            ..
        } = self;
        frame.paint_with_host(
            &tree,
            scene,
            Affine::IDENTITY,
            engine,
            |node, scene, root, engine| {
                let Some(key) = node.key else {
                    return;
                };
                let (rect, state) = (node.rect, node.state);
                if key == CAPTION {
                    caption::draw_direct_controls(scene, root, engine, rect, caption_state);
                } else if key == RESET {
                    let button = ActionButton::new(reset, ActionStyle::Secondary, None);
                    if state.disabled {
                        button.draw_disabled(scene, root, engine, rect);
                    } else {
                        button.draw_focused(
                            scene,
                            root,
                            engine,
                            rect,
                            state.hovered,
                            state.focused,
                        );
                    }
                } else if key == SUBJECT
                    && let Some(story) = story.as_ref()
                {
                    // The production button does not elide its label, so a
                    // label longer than the column is clipped at the stage.
                    let clip = stage.unwrap_or(rect);
                    scene.push_clip_layer(
                        vello::peniko::Fill::NonZero,
                        root,
                        &scene::rect(clip.x, clip.y, clip.w, clip.h),
                    );
                    button::draw_button(
                        scene,
                        root,
                        engine,
                        rect,
                        story.label(),
                        story.style(),
                        state.hovered,
                        state.focused,
                    );
                    scene.pop_layer();
                } else if key == field {
                    label.draw(
                        scene,
                        root,
                        engine,
                        rect,
                        InputStyle::form(state.focused),
                        None,
                        state.focused,
                    );
                } else if key == STYLE_SEGMENTS
                    && let Some(segments) = &segments
                    && let Some(story) = story.as_ref()
                {
                    segments.draw(
                        scene,
                        root,
                        engine,
                        &labels,
                        story.style_slot(),
                        *style_hover,
                        1.0,
                    );
                }
            },
        );
        self.laid = Some(Laid {
            frame,
            segments,
            size: (w, h),
        });
    }
}

mod navigator;
mod page;
mod semantics;

#[cfg(test)]
mod tests;
