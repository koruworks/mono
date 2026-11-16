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
//!   row  Caption                      the Koru mark; the title; the navigator's
//!                                     toggle; then, at the end, the properties'
//!                                     toggle and the shared window controls
//!   row  Workbench                    from the caption to the bottom of the window
//!     col  Navigator                  the catalog              (navigator.rs)
//!     col  Centre
//!       Toolbar                       the record's name; theme, width, language; Reset
//!       scroll  Preview               the record's page        (page.rs)
//!     scroll  Properties              what is recorded, and a story's inputs
//! ```
//!
//! **The preview is always there; the columns beside it fold away.** Each
//! has a toggle in the caption and a chord: the primary
//! modifier with B for the navigator and J for the properties, wherever the
//! keyboard is. A window too narrow for both shows the
//! navigator and hides the properties until asked, and opening one in a
//! narrow window closes the other, so the preview never has less than
//! [`PREVIEW_MIN_WIDTH`]. When a column closes with the keyboard inside it,
//! the keyboard goes to the toggle that opens it again.
//!
//! **The navigator's edge drags**, between the width it opens at and twice
//! that, by the app shell's own grip: an undrawn band over the line between
//! the navigator and the centre, with the column-resize pointer. As in the
//! app it is the pointer's only. A window too narrow for the dragged width
//! shows the navigator narrower, down to its narrowest, before the rule
//! above closes a column; the dragged width is kept and comes back with the
//! room.
//!
//! The surface is one element tree ([`koru_draw::el`]); every size in it is a
//! token ([`super::tokens`]). One layout gives the frame that paint, the
//! pointer and the keyboard all read, so where a control is drawn and where
//! it is pressed cannot disagree.
//!
//! **The controls are the production ones.** A story's component is the
//! production one ([`preview`]); free text is typed into a `TextInput`, a
//! choice is made on a `Segmented` or a `Dropdown`, a yes or a no on a
//! `Toggle`, Reset is an `ActionButton`, the three viewports are the element layer's scroll
//! element with the shared scrollbar, and the window controls are the shared
//! caption's. The tree
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
use koru_ui::{InputStyle, Segmented, TextInput, Toggle, Typed, caption};
use koru_window::{ImeEvent, RichKeyEvent, WindowApp, WindowRequest};
use vello::Scene;
use vello::kurbo::Affine;
use winit::dpi::PhysicalSize;
use winit::keyboard::{Key as KeyboardKey, ModifiersState, NamedKey};
use winit::window::{CursorIcon, ResizeDirection, WindowAttributes};

use self::navigator::{LIST, Navigator, RESET_SEARCH, Row, SEARCH};
use self::page::{PAGE, PANEL};
use self::settings::{OpenMenu, PreviewWidth, Setting};
use super::placement::Placement;
use super::registry::{self, Control, Entry, Family};
use super::stories::{Canvas, Part, Pointed};
use super::story::{Story, WidgetStory};
use super::tokens::*;

/// The caption row: the band that drags the window and holds its controls.
const CAPTION: Key = Key::surface("ui-lab", "caption");
/// The Koru mark in the caption's leading slot.
const MARK: Key = Key::surface("ui-lab", "mark");
/// The toolbar's Reset.
const RESET: Key = Key::surface("ui-lab", "reset");
/// The panel the component under test stands on, and the box that clips it.
const STAGE: Key = Key::surface("ui-lab", "stage");
/// The component under test.
const SUBJECT: Key = Key::surface("ui-lab", "subject");
/// The surface a part of the component under test is identified under: a
/// split button's two halves, a stepper's two buttons.
const PART: &str = "ui-lab.part";

/// The segments inside a choice: where the shared segmented control is
/// drawn. The control's own key, the one that is pressed and focused, is
/// [`control_key`] of the input it chooses.
fn segments_key(control: Control) -> Key {
    Key::surface("ui-lab.segments", control.part())
}

/// One of the two columns beside the preview.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Side {
    Navigator,
    Properties,
}

impl Side {
    const ALL: [Self; 2] = [Self::Navigator, Self::Properties];

    /// **The chord that shows and hides the column**: the primary modifier
    /// (Ctrl, or Cmd on a Mac) with B for the navigator and J for the
    /// properties. Matched and written out by `koru-keys`, so the key that
    /// fires and the key a description names are one value on every
    /// platform.
    const fn chord(self) -> koru_keys::Chord {
        koru_keys::Chord::new(
            koru_keys::On::Letter(match self {
                Self::Navigator => 'b',
                Self::Properties => 'j',
            }),
            koru_keys::PRIMARY,
        )
    }

    /// The caption button that shows and hides the column.
    const fn toggle(self) -> Key {
        match self {
            Self::Navigator => Key::surface("ui-lab", "toggle-navigator"),
            Self::Properties => Key::surface("ui-lab", "toggle-properties"),
        }
    }

    /// What the column, and so its toggle, is called.
    const fn name(self) -> koru_i18n::Key {
        match self {
            Self::Navigator => koru_i18n::Key::UiLabNavigator,
            Self::Properties => koru_i18n::Key::UiLabProperties,
        }
    }

    /// The pane toggle's own glyph: the side the column is on.
    const fn glyph(self) -> char {
        match self {
            Self::Navigator => koru_draw::icons::PANEL_LEFT,
            Self::Properties => koru_draw::icons::PANEL_RIGHT,
        }
    }

    /// The column a control belongs to, by its key: where the keyboard goes
    /// back to when the column closes under it.
    fn of(key: Key) -> Option<Self> {
        match key.object {
            el::Object::Surface("ui-lab.navigator" | "ui-lab.group") => Some(Self::Navigator),
            el::Object::Surface("ui-lab.control") => Some(Self::Properties),
            _ if key == SEARCH || key == RESET_SEARCH => Some(Self::Navigator),
            _ => None,
        }
    }
}

/// Which side columns a window shows, and how wide the navigator is in it.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Columns {
    navigator: bool,
    properties: bool,
    /// The navigator's width while it shows: the width it was dragged to,
    /// or as much of that as leaves the preview its least.
    navigator_width: f32,
}

impl Columns {
    fn shows(self, side: Side) -> bool {
        match side {
            Side::Navigator => self.navigator,
            Side::Properties => self.properties,
        }
    }

    /// How much of a window's width the columns showing take, each with the
    /// line beside it.
    fn width(self) -> f32 {
        let line = theme::BORDER_W as f32;
        let navigator = if self.navigator {
            self.navigator_width + line
        } else {
            0.0
        };
        let properties = if self.properties {
            PROPERTIES_WIDTH.value + line
        } else {
            0.0
        };
        navigator + properties
    }
}

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

/// **A column toggle's states**: nothing at rest, whether its column shows
/// or not; the hover plate under the pointer; the focus ring while the
/// keyboard is on it. A shown column is said by the glyph's filled form and
/// by nothing else: no accent, no lit plate.
static TOGGLE_STATES: StateRecipe = StateRecipe {
    name: "ui lab column toggle",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::bg(Surface::Hover),
    pressed: Look::bg(Surface::Hover),
    selected_hover: Look::bg(Surface::Hover),
    selected_pressed: Look::bg(Surface::Hover),
    disabled: Look::NONE,
    focus: Some(Ring::Line(Line::Focus)),
};

/// **A column toggle's glyph**: the caption's ordinary icon ink at rest and
/// the text ink under the pointer, the same whether its column shows or not.
static TOGGLE_INK: StateRecipe = StateRecipe {
    name: "ui lab column toggle glyph",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look {
        bg: None,
        ink: Some(Ink::Primary),
    },
    pressed: Look {
        bg: None,
        ink: Some(Ink::Primary),
    },
    selected_hover: Look::NONE,
    selected_pressed: Look::NONE,
    disabled: Look::NONE,
    focus: None,
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

/// A control that draws no focus state of its own, in a box that wears the
/// focus ring while `key` has the keyboard. The control keeps its own key,
/// rectangle and press; the box is only the mark.
fn ringed(name: &'static str, key: Key, control: El) -> El {
    el::boxed(name)
        .pad(FOCUS_INSET)
        .radius(Radius::M)
        .states(&FRAME_STATES)
        .activity([key])
        .child(control)
}

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
    /// Each choice shown as segments, measured where it was drawn.
    segments: Vec<(Control, Segmented)>,
    /// The parts of the component under test, where its own geometry put
    /// them: what the pointer, the keyboard and the screen reader's tree
    /// are answered from. Empty for a story whose component is one piece.
    parts: Vec<Part>,
    /// The client size the frame was laid out at, in logical pixels.
    size: (f32, f32),
}

impl Laid {
    fn rect(&self, key: Key) -> Option<Rect> {
        self.frame.node(key).map(|node| node.rect)
    }

    /// The measured segments of `control`'s choice.
    fn segments_of(&self, control: Control) -> Option<&Segmented> {
        self.segments
            .iter()
            .find(|(owner, _)| *owner == control)
            .map(|(_, segments)| segments)
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
    /// The control under the pointer; the segment of a choice when it is
    /// one; and the part of the component under test when it is on that.
    hot: Option<Key>,
    style_hover: Option<(Control, usize)>,
    part_hover: Option<usize>,
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
    story: Option<Story>,
    /// Every field a person types in, by the key of the control it is drawn
    /// in: the navigator's search, and one for each free-text property of
    /// the story on show. A field owns the text being edited — caret,
    /// selection, undo, composition — and whoever the text is for is told
    /// after each edit ([`Lab::adopt`]).
    fields: Vec<(Key, TextInput)>,
    /// The language the Lab is shown in: the one it opened in until the
    /// toolbar's setting changes it. Every entry point reads its strings in
    /// this language, whatever the rest of the process is set to.
    lang: koru_i18n::Lang,
    /// How wide the preview is.
    width: PreviewWidth,
    /// The list that is open, over the window, and the field it hangs from.
    menu: Option<OpenMenu>,
    /// The theme or the language changed and the process's other window has
    /// not been told.
    appearance_changed: bool,
    /// How wide the person dragged the navigator, between
    /// [`NAVIGATOR_WIDTH`] and [`NAVIGATOR_MAX_WIDTH`]. Kept while the
    /// navigator is hidden and while a narrow window shows it narrower; held
    /// in memory only, so the next Lab opens at the default again.
    navigator_width: f32,
    /// The navigator's edge being dragged: how far from the edge it is held.
    edge_drag: Option<f32>,
    /// What the person asked of each side column: shown, hidden, or `None`
    /// while it follows the window's width. By [`Side`]: navigator, then
    /// properties.
    asked: [Option<bool>; 2],
    /// The column opened last: the one that stays when the window has no
    /// room for both.
    opened_last: Side,
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
        let mut fields = vec![(SEARCH, TextInput::new(engine, theme::role::row()))];
        fields.extend(Self::story_fields(engine, story.as_ref()));
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
            part_hover: None,
            focus: None,
            nav: Navigator::new(entry),
            bring_shown_into_view: true,
            scroll_drag: None,
            entry,
            story,
            fields,
            lang: koru_i18n::current(),
            width: PreviewWidth::Fit,
            menu: None,
            appearance_changed: false,
            navigator_width: NAVIGATOR_WIDTH.value,
            edge_drag: None,
            asked: [None; 2],
            opened_last: Side::Navigator,
            page_scroll: 0.0,
            panel_scroll: 0.0,
            cache: TextCache::default(),
            laid: None,
        }
    }

    /// The story `entry`'s page runs, at its defaults: `None` for a record
    /// the registry lists nothing runnable for.
    fn story_for(entry: &'static Entry) -> Option<Story> {
        entry.story_id().and_then(Story::open)
    }

    /// A field for each free-text property of `story`, holding what the
    /// story holds; and, for a story whose component is itself typed in,
    /// that component's own field.
    fn story_fields(engine: &mut TextEngine, story: Option<&Story>) -> Vec<(Key, TextInput)> {
        let Some(story) = story else {
            return Vec::new();
        };
        let mut fields: Vec<(Key, TextInput)> = story
            .controls()
            .iter()
            .filter_map(|control| {
                let held = story.text(*control)?;
                Some((
                    control_key(*control),
                    TextInput::with_text(engine, theme::role::row(), held),
                ))
            })
            .collect();
        if let Story::Field(field) = story {
            fields.push((
                SUBJECT,
                TextInput::with_text(engine, theme::role::row(), field.typed()),
            ));
        }
        fields
    }

    /// The field drawn in `key`'s control, when it is one.
    fn field(&self, key: Key) -> Option<&TextInput> {
        self.fields
            .iter()
            .find(|(owner, _)| *owner == key)
            .map(|(_, field)| field)
    }

    fn field_mut(&mut self, key: Key) -> Option<&mut TextInput> {
        self.fields
            .iter_mut()
            .find(|(owner, _)| *owner == key)
            .map(|(_, field)| field)
    }

    /// What the search field holds.
    fn query(&self) -> &str {
        self.field(SEARCH).map_or("", TextInput::raw_text)
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

    fn segment_at(&self, control: Control, x: f32, y: f32) -> Option<usize> {
        self.laid.as_ref()?.segments_of(control)?.segment_at(x, y)
    }

    /// The part of the component under test at a point of the last frame.
    fn part_at(&self, x: f32, y: f32) -> Option<usize> {
        let parts = &self.laid.as_ref()?.parts;
        parts.iter().position(|part| part.rect.contains(x, y))
    }

    /// **Which side columns a window `width` wide shows.** The preview is
    /// never left less than [`PREVIEW_MIN_WIDTH`]: a column shows when the
    /// person asked for it, or, unasked, while there is room for it. When
    /// both are asked for and the window has no room for both, the one
    /// opened last stays and the other gives way until there is room again.
    ///
    /// **The navigator narrows before a column closes.** Which columns show
    /// is decided with the navigator at its narrowest; shown, it is then as
    /// wide as it was dragged to, or as much of that as leaves the preview
    /// its least. The dragged width itself is kept, so it comes back when
    /// the window has room again.
    fn columns(&self, width: f32) -> Columns {
        let narrowest = |navigator: bool, properties: bool| Columns {
            navigator,
            properties,
            navigator_width: NAVIGATOR_WIDTH.value,
        };
        let fits = |columns: Columns| width - columns.width() >= PREVIEW_MIN_WIDTH.value;
        let [navigator, properties] = self.asked;
        let navigator = navigator.unwrap_or(true);
        let properties = properties.unwrap_or_else(|| fits(narrowest(navigator, true)));
        let mut shown = narrowest(navigator, properties);
        if navigator && properties && !fits(shown) {
            shown = match self.opened_last {
                Side::Navigator => narrowest(true, false),
                Side::Properties => narrowest(false, true),
            };
        }
        if shown.navigator {
            // What the window can give the navigator beyond its narrowest.
            let spare = (width - shown.width() - PREVIEW_MIN_WIDTH.value).max(0.0);
            shown.navigator_width = self
                .navigator_width
                .min(NAVIGATOR_WIDTH.value + spare)
                .max(NAVIGATOR_WIDTH.value);
        }
        shown
    }

    /// **The band that resizes the navigator**: the app shell's own grip,
    /// straddling the navigator's edge with the centre, half over each. It
    /// is not drawn; the pointer says what it is. `None` while the navigator
    /// is hidden.
    fn navigator_grip(&self) -> Option<Rect> {
        let navigator = self.laid.as_ref()?.frame.rect_of("Navigator")?;
        Some(koru_app_shell::panel::grip(navigator))
    }

    /// Drag the navigator's edge to `x`, held `grab` from the edge. The
    /// width is held between the narrowest and the widest the navigator
    /// goes, and to what leaves the preview its least: at any of the three
    /// the edge stops and the pointer goes on without it. Returns whether
    /// the navigator's width changed.
    fn drag_navigator(&mut self, x: f32, grab: f32) -> bool {
        let Some(size) = self.frame_size() else {
            return false;
        };
        // The most this window can show, found by asking for the widest.
        let before = self.columns(size.0).navigator_width;
        self.navigator_width = NAVIGATOR_MAX_WIDTH.value;
        let most = self.columns(size.0).navigator_width;
        self.navigator_width = (x - grab).clamp(NAVIGATOR_WIDTH.value, most);
        self.navigator_width != before
    }

    /// The columns the window showed in its last frame, or would in its
    /// first.
    fn shown_columns(&self) -> Columns {
        let width = self
            .laid
            .as_ref()
            .map_or(Placement::DEFAULT_SIZE.0, |laid| laid.size.0);
        self.columns(width)
    }

    /// Show or hide a side column, because the person asked. Returns
    /// whether the window changes.
    fn set_column(&mut self, side: Side, open: bool) -> bool {
        let before = self.shown_columns();
        self.asked[side as usize] = Some(open);
        if open {
            self.opened_last = side;
            if side == Side::Navigator {
                self.bring_shown_into_view = true;
            }
        }
        self.shown_columns() != before
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

    /// Give the keyboard to `key`, or to nothing. Leaving a field ends
    /// whatever was in progress in it: a drag, a selection, a composition.
    fn set_focus(&mut self, engine: &mut TextEngine, key: Option<Key>) -> bool {
        if self.focus == key {
            return false;
        }
        if let Some(left) = self.focus
            && let Some(field) = self.field_mut(left)
        {
            field.ime_cancel(engine);
            field.blur(engine);
            self.adopt(left);
        }
        self.focus = key;
        if let Some(row) = key.and_then(|key| self.nav.row_of(key)) {
            self.nav.set_cursor(row);
        }
        true
    }

    /// Tell whoever `key`'s field is for what it now holds: the navigator
    /// its query, the story its label. Returns whether that changed anything.
    fn adopt(&mut self, key: Key) -> bool {
        let Some(field) = self.fields.iter().find(|(owner, _)| *owner == key) else {
            return false;
        };
        let text = field.1.raw_text();
        if key == SEARCH {
            return self.nav.set_query(text);
        }
        let control = self.control_of(key);
        let Some(story) = self.story.as_mut() else {
            return false;
        };
        match control {
            Some(control) => story.set_text(control, text),
            // The component under test, when it is one that is typed in.
            None if key == SUBJECT => story.set_typed(text),
            None => false,
        }
    }

    /// Put `text` in `key`'s field, the caret after it, and tell whoever it
    /// is for. Returns whether that changed anything.
    fn set_field(&mut self, engine: &mut TextEngine, key: Key, text: &str) -> bool {
        let Some(field) = self.field_mut(key) else {
            return false;
        };
        field.ime_cancel(engine);
        if field.raw_text() != text {
            field.set_text(engine, text);
        }
        field.move_to_end(engine);
        self.adopt(key)
    }

    /// The toolbar's Reset: the story back to how it opened, and its fields
    /// showing what it went back to. The record on show, the search and what
    /// it found, the settings and where anything is scrolled are left as
    /// they are.
    fn reset(&mut self, engine: &mut TextEngine) -> bool {
        let Some(story) = self.story.as_mut() else {
            return false;
        };
        let changed = story.reset();
        self.restore_fields(engine);
        changed
    }

    /// Make every field of the story show what the story holds: after a
    /// reset, or after the story was started again.
    fn restore_fields(&mut self, engine: &mut TextEngine) {
        let Some(story) = self.story.as_ref() else {
            return;
        };
        let mut held: Vec<(Key, String)> = story
            .controls()
            .iter()
            .filter_map(|control| Some((control_key(*control), story.text(*control)?.to_owned())))
            .collect();
        if let Story::Field(field) = story {
            held.push((SUBJECT, field.typed().to_owned()));
        }
        for (key, text) in held {
            self.set_field(engine, key, &text);
        }
    }

    /// Empty the search, so the navigator lists the whole gallery again.
    /// The record on show stays on show.
    fn reset_search(&mut self, engine: &mut TextEngine) -> bool {
        let changed = self.set_field(engine, SEARCH, "");
        if changed {
            // The button that was pressed is gone with the empty result: the
            // keyboard goes back to the field it cleared.
            self.focus = Some(SEARCH);
            self.bring_shown_into_view = true;
        }
        changed
    }

    /// Show `entry`'s page. The story that was running is dropped with every
    /// edit made to it; the new record's story, when it has one, starts at
    /// its defaults.
    fn show(&mut self, engine: &mut TextEngine, entry: &'static Entry) -> bool {
        if entry.catalog_id == self.entry.catalog_id {
            return false;
        }
        self.entry = entry;
        self.start(engine, Self::story_for(entry));
        self.page_scroll = 0.0;
        self.panel_scroll = 0.0;
        true
    }

    /// Put `story` on the stage in place of the one that was running, with
    /// a field for each thing in it that is typed.
    fn start(&mut self, engine: &mut TextEngine, story: Option<Story>) {
        self.story = story;
        // The search field is the navigator's and stays; the story's fields
        // go with the story.
        self.fields.retain(|(owner, _)| *owner == SEARCH);
        let fields = Self::story_fields(engine, self.story.as_ref());
        self.fields.extend(fields);
        self.style_hover = None;
        self.part_hover = None;
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
            // A component with parts is pressed on the part the keyboard is
            // on; any other is pressed whole.
            let cursor = self
                .story
                .as_ref()
                .and_then(Story::as_widget)
                .map(WidgetStory::cursor);
            match cursor {
                Some(part) => self.press_part(engine, part),
                None => self.story.as_mut().is_some_and(Story::press),
            }
        } else if let Some(control) = self.toggle_of(key) {
            // A yes-or-no input flips.
            let on = self.story.as_ref().and_then(|story| story.flag(control));
            self.story
                .as_mut()
                .is_some_and(|story| story.set_flag(control, on != Some(true)))
        } else if key == RESET {
            self.reset(engine)
        } else if key == RESET_SEARCH {
            self.reset_search(engine)
        } else if self.is_dropdown(key) {
            self.open_menu(engine, key)
        } else if let Some(side) = Side::ALL.into_iter().find(|side| side.toggle() == key) {
            let open = !self.shown_columns().shows(side);
            self.set_column(side, open)
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
        if changed && let Some(field) = self.field_mut(stops[next]) {
            field.select_all(engine);
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
        // An open list takes every press: one outside it closes it and is
        // not also a press on whatever was under it.
        if self.menu.is_some() {
            return self.menu_press(engine, x, y);
        }
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
        // The navigator's edge is taken hold of before anything under the
        // band, which lies half over the list and half over the centre.
        if let Some(grip) = self.navigator_grip().filter(|grip| grip.contains(x, y)) {
            self.edge_drag = Some(x - (grip.x + grip.w * 0.5));
            return true;
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
        if self.field(key).is_some() {
            if let Some(rect) = self.rect(key)
                && let Some(field) = self.field_mut(key)
            {
                changed |= if double {
                    field.double_click(engine, rect, x, y)
                } else {
                    field.click(engine, rect, x, y)
                };
            }
        } else if let Some(control) = self.segmented_of(key) {
            if let Some(slot) = self.segment_at(control, x, y)
                && let Some(story) = self.story.as_mut()
            {
                changed |= story.choose(control, slot);
            }
        } else if key == SUBJECT && self.story.as_ref().is_some_and(|s| s.as_widget().is_some()) {
            // A press on a component with parts is a press on the part
            // under the pointer; between its parts it presses nothing.
            if let Some(part) = self.part_at(x, y) {
                changed |= self.press_part(engine, part);
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

    /// A key the search field did not take as typing. Escape empties the
    /// search, and gives the keyboard up once it is empty; Down goes to the
    /// first row found; Enter shows the first record found.
    fn search_key(&mut self, engine: &mut TextEngine, key: &KeyboardKey, repeat: bool) -> bool {
        match key {
            KeyboardKey::Named(NamedKey::Escape) if !self.query().is_empty() => {
                self.reset_search(engine)
            }
            KeyboardKey::Named(NamedKey::Escape) => self.set_focus(engine, None),
            KeyboardKey::Named(NamedKey::ArrowDown) => match self.nav.first() {
                Some(row) => {
                    self.set_focus(engine, Some(row.key()));
                    self.reveal(row.key());
                    true
                }
                None => false,
            },
            KeyboardKey::Named(NamedKey::Enter) if !repeat => match self.nav.first_entry() {
                Some(entry) => self.show(engine, entry),
                None => false,
            },
            _ => false,
        }
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
        // A column's chord works wherever the keyboard is, a field and an
        // open list included: it is checked before either is offered the
        // key, so it neither types a letter nor is taken as one. Not while
        // an input method is composing, when the keys are the composition's.
        if let Some(side) = Side::ALL
            .into_iter()
            .find(|side| side.chord().matches(key, typed, mods))
        {
            if repeat || self.fields.iter().any(|(_, field)| field.is_composing()) {
                return false;
            }
            self.close_menu();
            let open = !self.shown_columns().shows(side);
            self.set_column(side, open);
            return true;
        }
        if self.menu.is_some() {
            return self.menu_key(engine, key, repeat);
        }
        // Ctrl+I arrives as Tab on Windows; only a bare or shifted Tab moves
        // the focus.
        let chord = mods.control_key() || mods.alt_key() || mods.super_key();
        if matches!(key, KeyboardKey::Named(NamedKey::Tab)) && !chord {
            return self.move_focus(engine, !mods.shift_key());
        }
        let Some(focus) = self.focus else {
            return false;
        };
        if let Some(field) = self.field_mut(focus) {
            return match field.key(engine, key, typed, mods) {
                // The caret may have moved without the text changing; either
                // way the field has to be drawn again.
                Typed::Handled => {
                    self.adopt(focus);
                    true
                }
                Typed::Passed if focus == SEARCH => self.search_key(engine, key, repeat),
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
        // The arrows open a dropdown's list, as Enter and Space do.
        if self.is_dropdown(focus)
            && matches!(
                key,
                KeyboardKey::Named(NamedKey::ArrowDown | NamedKey::ArrowUp)
            )
        {
            return self.open_menu(engine, focus);
        }
        if focus == SUBJECT
            && let Some(forward) = match key {
                KeyboardKey::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => Some(true),
                KeyboardKey::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => Some(false),
                _ => None,
            }
            && let Some(parts) = self.laid.as_ref().map(|laid| laid.parts.clone())
            && let Some(story) = self.story.as_mut().and_then(Story::as_widget_mut)
        {
            // The arrows are the component's: between its parts, or
            // whatever its story says they do.
            return story.widget().arrow(story, &parts, forward);
        }
        if let Some(control) = self.segmented_of(focus) {
            let Some(story) = self.story.as_mut() else {
                return false;
            };
            return match key {
                KeyboardKey::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => {
                    story.step(control, true)
                }
                KeyboardKey::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => {
                    story.step(control, false)
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
        let columns = self.columns(width);
        // A column's toggle, as elements: a square that lights under the
        // pointer and wears the focus ring, holding the pane glyph. The
        // glyph is filled while the column shows and outlined while it does
        // not, in the same ink either way.
        let toggle = |side: Side| {
            let shown = columns.shows(side);
            let glyph = el::icon(side.glyph(), koru_draw::theme::IconSize::Caption)
                .decorative()
                .fill()
                .activity([side.toggle()])
                .states(&TOGGLE_INK);
            el::boxed("Column toggle")
                .w(TOGGLE_SIZE)
                .h(TOGGLE_SIZE)
                .radius(Radius::M)
                .states(&TOGGLE_STATES)
                .id(side.toggle())
                .hit()
                .focusable()
                // Published as a button that is on while its column shows.
                .semantics(Role::Button)
                .name(side.name())
                .selected(shown)
                .action(el::Action::Click)
                .action(el::Action::Focus)
                .child(if shown { glyph.filled() } else { glyph })
        };
        // The caption, as the ordinary Koru window leads its own: the Koru
        // mark in the leading slot, one caption square wide, then the title,
        // then the navigator's toggle; at the other end the properties'
        // toggle and the window's three controls. Everything that is not a
        // button drags the window, the mark and the title included.
        let caption = el::row("Caption")
            .id(CAPTION)
            .h(CAPTION_HEIGHT)
            .gap(CAPTION_GAP)
            .items_center()
            .child(
                el::leaf(
                    "Koru mark",
                    Slot {
                        width: CAPTION_HEIGHT.value,
                        height: CAPTION_HEIGHT.value,
                    },
                )
                .decorative()
                .id(MARK),
            )
            .child(
                el::text(text(koru_i18n::Key::UiLabTitle))
                    .named("Window title")
                    .role(roles::ROW)
                    .ink(Ink::Secondary)
                    .truncate(),
            )
            .child(toggle(Side::Navigator))
            .child(el::spacer())
            .child(toggle(Side::Properties))
            .child(el::boxed("Window controls").w(CAPTION_CONTROLS));

        // The side columns run from the caption to the bottom of the window;
        // the toolbar is the centre's own, over the page. The preview is
        // always there; a side column is there while the window has room for
        // it or the person asked for it.
        let centre = el::col("Centre")
            .fill()
            .child(self.toolbar(engine, width - columns.width()))
            .child(el::divider(Line::Divider))
            .child(self.page(engine, width));
        let mut workbench = el::row("Workbench").fill();
        if columns.navigator {
            workbench = workbench
                .child(self.navigator(engine, height, columns.navigator_width))
                // The edge the navigator is resized by: the grip is a band
                // over this line, not an element of its own.
                .child(el::divider(Line::Divider).named("Navigator edge"));
        }
        workbench = workbench.child(centre);
        if columns.properties {
            workbench = workbench
                .child(el::divider(Line::Divider))
                .child(self.properties(engine));
        }
        el::col("UI Lab")
            .bg(Surface::Ground)
            .child(caption)
            .child(el::divider(Line::Divider))
            .child(workbench)
    }

    /// **The centre's toolbar**: the record's name, the three settings and
    /// Reset, over the page and no wider than it.
    ///
    /// On one row while the name can have [`TOOLBAR_TITLE_MIN`] beside the
    /// settings at their full width. In a narrower centre the settings take
    /// a row of their own under the name and Reset, and share that row's
    /// width: the name is what gives way first, by being cut short, and then
    /// each setting's field narrows, its value clipped before its chevron.
    /// Nothing is ever laid out past the centre's edge.
    fn toolbar(&self, engine: &mut TextEngine, centre: f32) -> El {
        let profile = Profile::Desktop;
        let reset = text(koru_i18n::Key::UiLabReset);
        let reset_width = ActionButton::new(reset, ActionStyle::Secondary, None).width(engine);
        let gap = TOOLBAR_GAP.token.px(profile);
        let ring = 2.0 * FOCUS_INSET.token.px(profile);
        let settings: f32 = Setting::ALL
            .into_iter()
            .map(|setting| self.dropdown_width(engine, setting.key()) + ring + gap)
            .sum();
        let one_row = centre
            >= 2.0 * TOOLBAR_INSET_X.token.px(profile)
                + TOOLBAR_TITLE_MIN.value
                + settings
                + gap
                + reset_width;
        let title = el::text(text(self.entry.name))
            .named("Story title")
            .role(roles::ROW)
            .emphasis(true)
            .fill()
            .truncate();
        let reset = el::leaf(
            "Reset",
            Slot {
                width: reset_width,
                height: ActionButton::H,
            },
        )
        .semantic()
        .id(RESET)
        .hit()
        .focusable()
        // Nothing to put back for a record with no story, or for a
        // story with nothing to change.
        .disabled(self.story.as_ref().is_none_or(|story| !story.can_change()))
        .semantics(Role::Button)
        .name(koru_i18n::Key::UiLabReset)
        .action(el::Action::Click)
        .action(el::Action::Focus);
        if one_row {
            return el::row("Toolbar")
                .pad_x(TOOLBAR_INSET_X)
                .pad_y(TOOLBAR_INSET_Y)
                .gap(TOOLBAR_GAP)
                .items_center()
                .child(title)
                .children(self.settings(engine, false))
                .child(reset);
        }
        el::col("Toolbar")
            .pad_x(TOOLBAR_INSET_X)
            .pad_y(TOOLBAR_INSET_Y)
            .gap(TOOLBAR_ROWS_GAP)
            .child(
                el::row("Toolbar heading")
                    .gap(TOOLBAR_GAP)
                    .items_center()
                    .child(title)
                    .child(reset),
            )
            .child(
                el::row("Toolbar settings")
                    .gap(TOOLBAR_GAP)
                    .items_center()
                    .children(self.settings(engine, true)),
            )
    }

    /// The left column: its heading, the search, and under it the catalog
    /// or, when the search finds nothing, a line saying so.
    fn navigator(&self, engine: &mut TextEngine, height: f32, width: f32) -> El {
        let column = el::col("Navigator")
            // The token while it is at the width it opens at; the dragged
            // width, named for what it is, once it is not.
            .w(if width == NAVIGATOR_WIDTH.value {
                Length::Metric(NAVIGATOR_WIDTH)
            } else {
                Length::Runtime(width, "the navigator's dragged width")
            })
            .pad(alias::NAV_PAD)
            .gap(NAVIGATOR_GAP)
            .child(
                el::text(text(koru_i18n::Key::UiLabNavigator))
                    .named("Navigator heading")
                    .role(roles::POPOVER_HEADING)
                    .truncate(),
            )
            // The shared search box draws no focus state of its own.
            .child(ringed(
                "Search",
                SEARCH,
                el::leaf(
                    "Search field",
                    Slot {
                        width: 0.0,
                        height: FIELD_HEIGHT.value,
                    },
                )
                .semantic()
                .h(FIELD_HEIGHT)
                .id(SEARCH)
                .hit()
                .focusable()
                // Published as a text field with its value, like a
                // property's.
                .semantics(Role::Group)
                .name(koru_i18n::Key::UiLabSearch)
                .action(el::Action::Focus),
            ));
        if self.nav.rows().is_empty() {
            column.child(navigator::nothing_found(engine, self.query()))
        } else {
            column.child(self.nav.element(self.entry, height))
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
        Self::in_language(self.lang, || {
            self.placement.attributes(text(koru_i18n::Key::UiLabTitle))
        })
    }

    fn should_exit(&self) -> bool {
        self.closed
    }

    fn close_requested(&mut self) {
        self.closed = true;
    }

    /// The caption drags the window everywhere but on a button: the
    /// window's own three, and the two column toggles the caption holds. A
    /// press the caption takes never reaches [`click`](Self::click), so a
    /// button left in the drag band would be a button that cannot be
    /// pressed.
    fn is_caption(&self, x: f32, y: f32) -> bool {
        self.caption_rect()
            .is_some_and(|caption| caption.contains(x, y))
            && self.caption_action_at(x, y).is_none()
            && self.hit_at(x, y).is_none()
    }

    fn cursor_icon(&self, x: f32, y: f32) -> CursorIcon {
        // The navigator's edge says what it is before anything under it,
        // and goes on saying so for as long as it is held. Not under an open
        // list, which takes the press.
        if self.edge_drag.is_some()
            || (self.menu.is_none()
                && self
                    .navigator_grip()
                    .is_some_and(|grip| grip.contains(x, y)))
        {
            return CursorIcon::ColResize;
        }
        if self
            .hit_at(x, y)
            .is_some_and(|key| self.field(key).is_some())
        {
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
        self.part_hover = None;
        self.caption_hover = None;
        self.scroll_drag = None;
        self.edge_drag = None;
        // A list hangs from where its field was.
        self.menu = None;
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
            for (_, field) in &mut self.fields {
                field.release();
            }
            self.scroll_drag = None;
            self.edge_drag = None;
            self.menu = None;
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
        // Over an open list the pointer lights an option and nothing under
        // the list.
        if self.menu.is_some() {
            return self.menu_hover(x, y);
        }
        let caption = self.caption_action_at(x, y);
        // Nothing under the navigator's edge lights: the band is the edge's.
        let on_edge = self
            .navigator_grip()
            .is_some_and(|grip| grip.contains(x, y));
        let hot = self.hit_at(x, y).filter(|_| !on_edge);
        let segment = hot
            .and_then(|key| self.segmented_of(key))
            .and_then(|control| Some((control, self.segment_at(control, x, y)?)));
        let part = hot
            .filter(|key| *key == SUBJECT)
            .and_then(|_| self.part_at(x, y));
        let changed = self.caption_hover != caption
            || self.hot != hot
            || self.style_hover != segment
            || self.part_hover != part;
        self.caption_hover = caption;
        self.hot = hot;
        self.style_hover = segment;
        self.part_hover = part;
        changed
    }

    fn pointer_left(&mut self) -> bool {
        let changed = self.caption_hover.is_some()
            || self.hot.is_some()
            || self.style_hover.is_some()
            || self.part_hover.is_some();
        self.pointer = None;
        self.caption_hover = None;
        self.hot = None;
        self.style_hover = None;
        self.part_hover = None;
        changed
    }

    fn click(&mut self, engine: &mut TextEngine, x: f32, y: f32, _ctrl: bool) -> bool {
        Self::in_language(self.lang, || self.press_at(engine, x, y, false))
    }

    /// The second of two quick presses. In the label field it selects the
    /// word; anywhere else it is simply another press, so pressing the button
    /// twice quickly counts twice.
    fn double_click(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        Self::in_language(self.lang, || self.press_at(engine, x, y, true))
    }

    fn drag(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        if let Some(grab) = self.edge_drag {
            self.pointer = Some((x, y));
            return self.drag_navigator(x, grab);
        }
        if self.scroll_drag.is_some() {
            self.pointer = Some((x, y));
            return self.drag_scroll(y);
        }
        if let Some(key) = self
            .fields
            .iter()
            .find(|(_, field)| field.is_dragging())
            .map(|(owner, _)| *owner)
        {
            let rect = self.rect(key);
            return match (rect, self.field_mut(key)) {
                (Some(rect), Some(field)) => field.drag(engine, rect, x),
                _ => false,
            };
        }
        self.pointer_moved(x, y)
    }

    fn mouse_up(&mut self) -> bool {
        for (_, field) in &mut self.fields {
            field.release();
        }
        self.scroll_drag = None;
        self.edge_drag = None;
        false
    }

    /// A wheel notch scrolls the viewport under the pointer, and nothing
    /// when the pointer is over none.
    fn scroll_by(&mut self, delta: f32, _mods: ModifiersState) -> bool {
        // Nothing scrolls under an open list.
        if self.menu.is_some() {
            return false;
        }
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
        Self::in_language(self.lang, || self.key_down(engine, key, text, mods, false))
    }

    fn rich_key(&mut self, engine: &mut TextEngine, event: RichKeyEvent<'_>) -> bool {
        if !event.state.is_pressed() {
            return false;
        }
        Self::in_language(self.lang, || {
            self.key_down(
                engine,
                event.logical_key,
                event.text,
                event.modifiers,
                event.repeat,
            )
        })
    }

    fn wants_ime(&self) -> bool {
        // The fields take composed text. The runner asks once, when the
        // window is created, so this cannot follow the focus.
        true
    }

    fn ime(&mut self, engine: &mut TextEngine, event: ImeEvent<'_>) -> bool {
        let Some(focus) = self.focus else {
            return false;
        };
        let Some(field) = self.field_mut(focus) else {
            return false;
        };
        let changed = field.ime(engine, &composition(event));
        // A composition in progress is not the text yet: the field's
        // authored text only changes when one is committed.
        Self::in_language(self.lang, || self.adopt(focus)) || changed
    }

    fn draw(&mut self, scene: &mut Scene, engine: &mut TextEngine, w: f32, h: f32) {
        Self::in_language(self.lang, || self.paint(scene, engine, w, h));
    }
}

impl Lab {
    /// One frame: lay the window out, paint it, and keep the layout for the
    /// events that follow.
    fn paint(&mut self, scene: &mut Scene, engine: &mut TextEngine, w: f32, h: f32) {
        // A control that went with its column hands the keyboard back to the
        // button that opens the column, before the frame is laid out, so the
        // button is the one drawn focused.
        let columns = self.columns(w);
        if let Some(side) = self
            .focus
            .and_then(Side::of)
            .filter(|side| !columns.shows(*side))
        {
            self.focus = Some(side.toggle());
        }
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
                && self.edge_drag.is_none()
                && self.menu.is_none()
                && !koru_app_shell::panel::grip(
                    frame
                        .rect_of("Navigator")
                        .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0)),
                )
                .contains(x, y)
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
        // Each choice shown as segments: its labels, the one in force, and
        // the shared control measured where the frame put its slot.
        let segments: Vec<(Control, Vec<String>, Option<usize>, Segmented)> = self
            .segmented_controls()
            .into_iter()
            .filter_map(|(control, options)| {
                let node = frame.node(segments_key(control))?;
                let labels: Vec<String> = options
                    .iter()
                    .map(|option| preview::option_text(&option.label))
                    .collect();
                let words: Vec<&str> = labels.iter().map(String::as_str).collect();
                let measured = Segmented::layout(engine, &words, node.rect.x, node.rect.y);
                let chosen = options.iter().position(|option| option.selected);
                Some((control, labels, chosen, measured))
            })
            .collect();
        // Each yes-or-no input, and what it says.
        let toggles: Vec<(Key, bool)> = self
            .story
            .iter()
            .flat_map(|story| {
                story
                    .controls()
                    .iter()
                    .filter_map(|control| Some((control_key(*control), story.flag(*control)?)))
            })
            .collect();
        // The parts of a component that has them, from its own geometry in
        // the slot the frame gave it; and where the pointer and the
        // keyboard are among them.
        let widget = self.story.as_ref().and_then(Story::as_widget);
        let parts = match (frame.node(SUBJECT), widget) {
            (Some(node), Some(story)) => story.widget().parts(story, engine, node.rect),
            _ => Vec::new(),
        };
        let pointed = Pointed {
            hovered: self.part_hover.filter(|part| *part < parts.len()),
            focused: widget
                .filter(|_| self.active && self.focus == Some(SUBJECT))
                .map(WidgetStory::cursor),
            open: self.menu.as_ref().is_some_and(|open| open.owner == SUBJECT),
        };
        let rtl = koru_i18n::current().is_rtl();
        let stage = frame.node(STAGE).map(|node| node.rect);
        let caption_state = caption::State {
            hovered: self.caption_hover,
            maximized: self.maximised,
            menu: caption::MenuVisual::default(),
        };
        let reset = text(koru_i18n::Key::UiLabReset);
        let reset_search = text(koru_i18n::Key::UiLabResetSearch);
        let search = text(koru_i18n::Key::UiLabSearch);
        // Each dropdown's value in force, and whether its list is open.
        let dropdowns: Vec<(Key, String, bool)> = Setting::ALL
            .into_iter()
            .map(|setting| setting.key())
            .chain(
                self.story
                    .iter()
                    .flat_map(|story| story.controls())
                    .map(|control| control_key(*control))
                    .filter(|key| self.is_story_dropdown(*key)),
            )
            .filter(|key| frame.contains(*key))
            .map(|key| {
                let value = self
                    .chosen(key)
                    .map_or(String::new(), |choice| choice.label);
                let open = self.menu.as_ref().is_some_and(|open| open.owner == key);
                (key, value, open)
            })
            .collect();
        let Self {
            story,
            fields,
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
                    && let Some(Story::Button(story)) = story.as_ref()
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
                } else if key == SUBJECT
                    && let Some(Story::Widget(story)) = story.as_ref()
                {
                    // What would not fit the column is clipped at the stage.
                    let clip = stage.unwrap_or(rect);
                    scene.push_clip_layer(
                        vello::peniko::Fill::NonZero,
                        root,
                        &scene::rect(clip.x, clip.y, clip.w, clip.h),
                    );
                    let mut canvas = Canvas {
                        scene: &mut *scene,
                        root,
                        engine: &mut *engine,
                    };
                    story.widget().paint(story, &mut canvas, rect, pointed);
                    // A part whose component draws no focus mark of its own
                    // wears the window's ring while the keyboard is on it.
                    if let Some(part) = pointed.focused.and_then(|at| parts.get(at))
                        && !part.own_focus
                    {
                        let gap = FOCUS_INSET.token.px(Profile::Desktop);
                        scene::stroke(
                            scene,
                            root,
                            theme::p().focus_ring,
                            theme::BORDER_W,
                            &scene::rrect(
                                part.rect.x - gap,
                                part.rect.y - gap,
                                part.rect.w + 2.0 * gap,
                                part.rect.h + 2.0 * gap,
                                Radius::M.px(
                                    Profile::Desktop,
                                    part.rect.w + 2.0 * gap,
                                    part.rect.h + 2.0 * gap,
                                ),
                            ),
                        );
                    }
                    scene.pop_layer();
                } else if let Some((_, on)) = toggles.iter().find(|(owner, _)| *owner == key) {
                    Toggle::new(*on).draw(
                        scene,
                        root,
                        engine,
                        rect,
                        rtl,
                        state.hovered,
                        state.focused,
                    );
                } else if key == MARK {
                    // The shared mark, as the ordinary caption draws it: 16
                    // in the text color, centred in its slot. A logo is not a
                    // verb, so it has no plate and no hover.
                    let size = CAPTION_MARK.value;
                    koru_ui::logo::KoruLogo::new(size)
                        .tint(theme::p().text)
                        .draw(
                            scene,
                            root,
                            rect.x + (rect.w - size) * 0.5,
                            rect.y + (rect.h - size) * 0.5,
                        );
                } else if let Some((_, value, open)) =
                    dropdowns.iter().find(|(owner, _, _)| *owner == key)
                {
                    Self::draw_dropdown(value, *open, scene, root, engine, rect, state.hovered);
                } else if key == RESET_SEARCH {
                    ActionButton::new(reset_search, ActionStyle::Secondary, None).draw_focused(
                        scene,
                        root,
                        engine,
                        rect,
                        state.hovered,
                        state.focused,
                    );
                } else if let Some((_, field)) = fields.iter_mut().find(|(owner, _)| *owner == key)
                {
                    // The search is the shared search box, with the hint of
                    // what it searches while it is empty; a property is a
                    // form's field.
                    let (style, hint) = match story.as_ref() {
                        _ if key == SEARCH => (InputStyle::search(), Some(search)),
                        // The component under test: the box and the hint are
                        // the story's two inputs.
                        Some(Story::Field(story)) if key == SUBJECT => (
                            preview::field_box(story.style(), state.focused),
                            Some(story.placeholder()),
                        ),
                        _ => (InputStyle::form(state.focused), None),
                    };
                    field.draw(scene, root, engine, rect, style, hint, state.focused);
                } else if let Some((control, labels, chosen, measured)) = segments
                    .iter()
                    .find(|(control, ..)| segments_key(*control) == key)
                {
                    let words: Vec<&str> = labels.iter().map(String::as_str).collect();
                    let hovered = style_hover
                        .filter(|(owner, _)| owner == control)
                        .map(|(_, slot)| slot);
                    measured.draw(scene, root, engine, &words, *chosen, hovered, 1.0);
                }
            },
        );
        // The open list, over everything the window drew.
        if let Some(open) = &self.menu {
            let enabled = vec![true; open.choices.len()];
            open.menu
                .draw(scene, Affine::IDENTITY, engine, (w, h), &enabled);
        }
        self.laid = Some(Laid {
            frame,
            segments: segments
                .into_iter()
                .map(|(control, _, _, measured)| (control, measured))
                .collect(),
            parts,
            size: (w, h),
        });
    }
}

mod navigator;
mod page;
mod preview;
mod semantics;
mod settings;

#[cfg(test)]
mod tests;
