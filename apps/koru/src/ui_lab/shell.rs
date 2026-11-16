// SPDX-License-Identifier: MPL-2.0

//! **The Lab's one window**, on the shared single-window runner.
//!
//! [`Lab`] is a `koru_window::WindowApp` and nothing more: it is handed a
//! text engine, draws into a scene, and answers pointer and key events. It is
//! not a workspace view and is built from none of the workspace host's
//! startup, so it has no profile, no Store session, no settings file and no
//! background worker of its own. Closing the window ends the process.
//!
//! ```text
//! col  UI Lab                         the window, on the ground surface
//!   row  Caption                      the title; the shared window controls
//!   row  Toolbar                      the story's name; Reset
//!   row  Workbench
//!     col  Navigator                  a heading a family, an entry a component
//!     col  Preview
//!       col  Preview stage            the production component, on a panel
//!       Click result                  what pressing it did
//!     col  Properties                 name, identifiers, purpose
//!       col  Property                 an input the registration lists
//! ```
//!
//! The surface is one element tree ([`koru_draw::el`]); every size in it is a
//! token ([`super::tokens`]). One layout gives the frame that paint, the
//! pointer and the keyboard all read, so where a control is drawn and where
//! it is pressed cannot disagree.
//!
//! **The controls are the production ones.** The button under test is
//! `koru_ui::button::draw_button`; the label is typed into a `TextInput`, the
//! style is chosen on a `Segmented`, Reset is an `ActionButton` and the window
//! controls are the shared caption's. The tree gives each a slot and this
//! file draws the real control in it; nothing here paints a control of its
//! own.

use koru_draw::el::{
    self, Align, El, Frame, FrameCx, Interaction, Key, Leaf, Length, Look, Ring, Role, StateRecipe,
    TextCache,
};
use koru_draw::geometry::Rect;
use koru_draw::scene;
use koru_draw::text::TextEngine;
use koru_draw::theme::{self, Ink, Line, Profile, Radius, Surface, alias, metrics, roles};
use koru_ui::button::{self, ActionButton, ActionStyle};
use koru_ui::{InputStyle, Segmented, TextInput, Typed, caption};
use koru_window::{ImeEvent, RichKeyEvent, WindowApp, WindowRequest};
use vello::Scene;
use vello::kurbo::Affine;
use winit::dpi::PhysicalSize;
use winit::keyboard::{Key as KeyboardKey, ModifiersState, NamedKey};
use winit::window::{CursorIcon, ResizeDirection, WindowAttributes};

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

/// An entry's row in the navigator.
fn navigator_key(entry: &Entry) -> Key {
    Key::surface("ui-lab.navigator", entry.catalog_id)
}

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

/// **A choice control's frame**: nothing of its own but the focus ring. The
/// shared segmented control draws no focus state, so the element that holds
/// it wears the frame's.
static CHOICE_STATES: StateRecipe = StateRecipe {
    name: "ui lab choice",
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
}

impl Laid {
    fn rect(&self, key: Key) -> Option<Rect> {
        self.frame.node(key).map(|node| node.rect)
    }

    /// The keys the keyboard visits, in reading order.
    fn stops(&self) -> Vec<Key> {
        self.frame
            .nodes
            .iter()
            .filter(|node| node.focusable)
            .filter_map(|node| node.key)
            .collect()
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
    /// The control under the pointer, and the style segment when it is one.
    hot: Option<Key>,
    style_hover: Option<usize>,
    /// The control the keyboard is on.
    focus: Option<Key>,
    /// The entry on show. There is one; the registry decides which.
    entry: &'static Entry,
    story: ButtonStory,
    /// The label property's field. It owns the text being edited — caret,
    /// selection, undo, composition — and the story is told after each edit.
    label: TextInput,
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
        let story = ButtonStory::new(text(koru_i18n::Key::UiLabButtonSampleLabel));
        let label = TextInput::with_text(engine, theme::role::row(), story.label());
        Self {
            placement,
            closed: false,
            maximised: false,
            active: true,
            request: None,
            caption_hover: None,
            hot: None,
            style_hover: None,
            focus: None,
            entry,
            story,
            label,
            cache: TextCache::default(),
            laid: None,
        }
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
        match self.story.presses() {
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
            .styles()
            .iter()
            .map(|style| text(registry::dialog_style_label(*style)))
            .collect()
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
        true
    }

    /// Tell the story what the field now holds.
    fn adopt_label(&mut self) -> bool {
        self.story.set_label(self.label.raw_text())
    }

    /// The toolbar's Reset: the story back to how it opened, and the field
    /// showing the label it went back to.
    fn reset(&mut self, engine: &mut TextEngine) -> bool {
        let changed = self.story.reset();
        self.label.ime_cancel(engine);
        if self.label.raw_text() != self.story.label() {
            self.label.set_text(engine, self.story.label());
        }
        self.label.move_to_end(engine);
        changed
    }

    /// Do what a press on `key` does. A pointer press and Enter or Space on
    /// the focused control both come here, so they cannot disagree.
    fn activate(&mut self, engine: &mut TextEngine, key: Key) -> bool {
        if key == SUBJECT {
            self.story.press();
            true
        } else if key == RESET {
            self.reset(engine)
        } else {
            // An entry that is already the one on show, or a control with
            // nothing to activate.
            false
        }
    }

    /// Tab and Shift+Tab: the next or previous stop, round the ends.
    fn move_focus(&mut self, engine: &mut TextEngine, forward: bool) -> bool {
        let Some(stops) = self.laid.as_ref().map(Laid::stops) else {
            return false;
        };
        if stops.is_empty() {
            return false;
        }
        let at = self
            .focus
            .and_then(|key| stops.iter().position(|stop| *stop == key));
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
        changed
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
        let key = self.hit_at(x, y);
        let stop = key.filter(|key| {
            self.laid
                .as_ref()
                .is_some_and(|laid| laid.stops().contains(key))
        });
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
            if let Some(slot) = self.segment_at(x, y) {
                changed |= self.story.choose(slot);
            }
        } else {
            changed |= self.activate(engine, key);
        }
        changed
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
        if focus == control_key(Control::Style) {
            return match key {
                KeyboardKey::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => {
                    self.story.step_style(true)
                }
                KeyboardKey::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => {
                    self.story.step_style(false)
                }
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

    /// The window as elements. `width` is the window's, for the one runtime
    /// bound in the tree: how wide the component under test may grow before
    /// it would leave its column.
    fn tree(&self, engine: &mut TextEngine, width: f32) -> El {
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
                    .child(self.navigator())
                    .child(el::divider(Line::Divider))
                    .child(self.preview(engine, width))
                    .child(el::divider(Line::Divider))
                    .child(self.properties(engine)),
            )
    }

    /// The left column: a heading a family, a row a registered entry.
    fn navigator(&self) -> El {
        let mut column = el::col("Navigator")
            .w(NAVIGATOR_WIDTH)
            .pad(alias::NAV_PAD)
            .gap(NAVIGATOR_GAP)
            .child(
                el::text(text(koru_i18n::Key::UiLabNavigator))
                    .named("Navigator heading")
                    .role(roles::POPOVER_HEADING)
                    .truncate(),
            );
        for family in Family::ALL {
            let mut entries = registry::ENTRIES
                .iter()
                .filter(|entry| entry.family == family)
                .peekable();
            // A group with nothing registered in it is not shown as an empty
            // heading.
            if entries.peek().is_none() {
                continue;
            }
            column = column.child(
                el::text(text(family.heading()))
                    .named("Family heading")
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .truncate(),
            );
            column = column.children(entries.map(|entry| {
                el::row("Navigator entry")
                    .min_h(metrics::ROW_MIN)
                    .pad_x(alias::NAV_PAD)
                    .radius(Radius::M)
                    .items_center()
                    .states(&ENTRY_STATES)
                    .selected(entry.catalog_id == self.entry.catalog_id)
                    .id(navigator_key(entry))
                    .hit()
                    .focusable()
                    .semantics(Role::ListItem)
                    .name(entry.name)
                    .action(el::Action::Click)
                    .action(el::Action::Focus)
                    .child(
                        el::text(text(entry.name))
                            .named("Entry name")
                            .role(roles::ROW)
                            .fill()
                            .truncate(),
                    )
            }));
        }
        column
    }

    /// The centre: the production component on a panel, and under it what
    /// pressing it did.
    fn preview(&self, engine: &mut TextEngine, width: f32) -> El {
        let profile = Profile::Desktop;
        // What the column has inside its inset once the two side columns and
        // the lines between them are taken out, and what that leaves the
        // component inside the stage's. A label typed past it is clipped at
        // the stage rather than drawn over a neighbour.
        let column = width
            - NAVIGATOR_WIDTH.value
            - PROPERTIES_WIDTH.value
            - 2.0 * theme::BORDER_W as f32
            - 2.0 * PREVIEW_INSET.token.px(profile);
        let room = column - 2.0 * STAGE_INSET.token.px(profile);
        let subject = el::leaf(
            "Component under test",
            Slot {
                width: button::button_width(engine, self.story.label()),
                height: button::H,
            },
        )
        .semantic()
        .id(SUBJECT)
        .max_w(Length::Runtime(
            room.max(button::MIN_W),
            "preview column less its insets",
        ))
        .hit()
        .focusable()
        .semantics(Role::Button)
        .name_text(self.story.label())
        .action(el::Action::Click)
        .action(el::Action::Focus);
        el::col("Preview")
            .fill()
            .pad(PREVIEW_INSET)
            .gap(PREVIEW_GAP)
            .child(el::spacer())
            .child(
                el::col("Preview stage")
                    .id(STAGE)
                    .align_self(Align::Centre)
                    .pad(STAGE_INSET)
                    .bg(Surface::Raised)
                    .radius(Radius::L)
                    .child(subject),
            )
            .child(
                el::text(self.result_line())
                    .named("Click result")
                    .role(roles::ROW)
                    .ink(Ink::Secondary)
                    .wrap()
                    .max_w(Length::Runtime(
                        column.max(button::MIN_W),
                        "preview column less its inset",
                    ))
                    .align_self(Align::Centre),
            )
            .child(el::spacer())
    }

    /// The right column: what the component is, and an input for each thing
    /// the registration says the recipe takes.
    fn properties(&self, engine: &mut TextEngine) -> El {
        let entry = self.entry;
        let symbol = registry::story(registry::BUTTON_DIALOG_STORY)
            .map_or(entry.source.module, |(_, recipe)| recipe.symbol);
        let identity = el::col("Component identity")
            .gap(IDENTITY_GAP)
            .child(
                el::text(text(entry.name))
                    .named("Component name")
                    .role(roles::PANE_TITLE)
                    .truncate(),
            )
            // Technical identifiers: the same in every language.
            .child(
                el::text(entry.public_name)
                    .named("Public name")
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .truncate(),
            )
            .child(
                el::text(entry.catalog_id)
                    .named("Catalog ID")
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .truncate(),
            )
            .child(
                el::text(symbol)
                    .named("Source symbol")
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .truncate(),
            );
        let mut column = el::col("Properties")
            .w(PROPERTIES_WIDTH)
            .pad(PANEL_INSET)
            .gap(PANEL_GAP)
            .child(identity)
            .child(
                el::text(text(entry.purpose))
                    .named("Component purpose")
                    .role(roles::ROW)
                    .ink(Ink::Secondary)
                    .wrap(),
            )
            .child(el::divider(Line::Divider));
        for control in self.story.controls() {
            column = column.child(
                el::col("Property")
                    .gap(PROPERTY_GAP)
                    .child(
                        el::text(text(control.name()))
                            .named("Property name")
                            .role(roles::META)
                            .ink(Ink::Secondary)
                            .truncate(),
                    )
                    .child(self.control(engine, *control)),
            );
        }
        column
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
            .name(control.name())
            .action(el::Action::Focus),
            Control::Style => el::boxed("Style choice")
                .align_self(Align::Start)
                .pad(FOCUS_INSET)
                .radius(Radius::M)
                .states(&CHOICE_STATES)
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
        }
        true
    }

    fn wants_drag(&self) -> bool {
        // A drag extends the selection in the label field.
        true
    }

    fn pointer_moved(&mut self, x: f32, y: f32) -> bool {
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
        false
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
        let tree = self.tree(engine, w);
        let interaction = Interaction {
            hot: self.hot,
            pressed: None,
            focused: self.focus.filter(|_| self.active),
        };
        let frame = tree.layout(
            engine,
            &mut self.cache,
            Rect::new(0.0, 0.0, w, h),
            FrameCx::root().with_interaction(interaction),
        );
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
                    ActionButton::new(reset, ActionStyle::Secondary, None).draw_focused(
                        scene,
                        root,
                        engine,
                        rect,
                        state.hovered,
                        state.focused,
                    );
                } else if key == SUBJECT {
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
        self.laid = Some(Laid { frame, segments });
    }
}

#[cfg(test)]
mod tests;
