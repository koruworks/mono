// SPDX-License-Identifier: MPL-2.0

//! **The model picker's vocabulary** (agent-models §9, koru-ai D13, D46, D64,
//! D67, D76): the rail's entries, the list's rows, where the keyboard is, the
//! picker's own state, the parts its frame is read back into, its sizes, and
//! the pure rules over them — search, the highlight's steps, the list's
//! scroll, a favorite's toggle and the footer's readout. Generic over the
//! host's agent id `P`, which the control only compares, hashes and writes
//! out (`Display`): a chat's is its provider id, a standalone host's can be a
//! `String`.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Display;
use std::rc::Rc;

use koru_draw::el::FrameCx;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;
use koru_i18n::{Key, Lang, t};
use koru_ui::Typed;
use koru_ui::text_input::{InputStyle, TextInput};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key as WinitKey, ModifiersState};

use crate::effort::{self, Effort, Levels};

pub mod context;
pub mod elements;
pub mod inputs;
pub mod mark;
pub mod paint;
pub mod tokens;
pub mod view;

pub use inputs::{
    Agent, AgentId, Applies, Builtin, BuiltinModel, Context, Current, DEFAULT_MODEL, FooterOffer,
    Inputs, Listing, ModelRecord, Outcome, Place, Readiness,
};
pub use paint::{SearchLeaf, SearchPaint};
pub use view::{ModelPicker, Picker};

/// The rail's width, and the square of each of its marks.
pub const RAIL_W: f32 = 44.0;
/// **Air between the rail and the picker's content** — the search row, the
/// list and the footer all start this far right of the rail, so its marks
/// are not flush against them (the composer's toolbar keeps the same 8 px
/// between its controls). The picker lays out with the element token
/// `elements::CONTENT_GAP`; this value is what the tests hold it to.
#[cfg(any(test, feature = "test-support"))]
pub const RAIL_CONTENT_GAP: f32 = 8.0;
pub const RAIL_BUTTON: f32 = 32.0;
pub const RAIL_GAP: f32 = 6.0;
/// The room a divider takes after the chat's own built-in provider (S1).
pub const RAIL_DIVIDER: f32 = 9.0;
/// Keep the rail shorter than the model list panel; the chevrons page through
/// further agents without letting the popover grow past the viewport.
pub const RAIL_PAGE: usize = 6;
/// The search row, with ↻ at its end.
pub const SEARCH_H: f32 = 40.0;
/// A row of the list: a heading, a note or a model, one line each. A
/// model's description is not shown under its name (D43, taken back by the
/// user: only Codex gave its models one), so every row is this tall.
pub const ROW_H: f32 = 30.0;
/// **The picker's one size** (D13): the search row, six rows of the list
/// and the footer — the box the person chose. The list scrolls inside it
/// when there are more, and a short list leaves room under it rather than
/// shrinking the picker, so the picker keeps its size from agent to agent.
/// A model with no footer gives the room to the list.
pub const LIST_ROWS: usize = 6;
/// The ☆ at a model row's end (D46): its square, and how far its right edge
/// keeps from the list's, clear of the scrollbar's lane
/// (`elements::STAR_INSET`).
pub const STAR: f32 = 22.0;
#[cfg(any(test, feature = "test-support"))]
pub const STAR_INSET: f32 = 12.0;
/// A favorite's agent mark at its row's start.
pub const ROW_MARK: f32 = 16.0;
/// An effort segment's height, and the effort strip's: the segments and
/// the strip's 2 round them (D76).
pub const SEGMENT_H: f32 = 28.0;
pub const STRIP_H: f32 = SEGMENT_H + 4.0;
/// A pill toggle's height: *⚡ Fast*, *1M context*.
pub const PILL_H: f32 = 28.0;
/// **The footer at its tallest** (D76): the divider, 10 above the effort
/// strip, 8 between it and the pills' row, and 8 under that (12 from the
/// panel's foot with [`PAD`]). Without levels it is the divider and the
/// pills' row alone (`FOOTER_TOGGLES_H`).
pub const FOOTER_H: f32 = 1.0 + 10.0 + STRIP_H + 8.0 + PILL_H + 8.0;
/// The footer of a model with Fast or a longer window and no levels.
#[cfg(any(test, feature = "test-support"))]
pub const FOOTER_TOGGLES_H: f32 = 1.0 + 10.0 + PILL_H + 8.0;
pub const PAD: f32 = 4.0;
/// The picker is as wide as the composer, up to this.
pub const MAX_W: f32 = 360.0;
/// The tooltips' keys, so moving from one mark or row to the next is a new
/// tooltip: the rail's first mark, ↻, the read-only strip and context
/// pill. A model row has none (D25).
pub const TOOLTIP_RAIL: u64 = 0x5049_0000;
pub const TOOLTIP_REFRESH: u64 = 0x5049_1000;
pub const TOOLTIP_LOCKED: u64 = 0x5049_1002;
pub const TOOLTIP_CONTEXT_LOCKED: u64 = 0x5049_1003;

/// **One favorite model** (D46): an agent, by its key (`claude`, `codex`,
/// …), and the model id that agent takes — what the host's roaming
/// `favorite_models` setting holds, in the order they were starred.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FavoriteModel {
    pub agent: String,
    pub model: String,
}

impl FavoriteModel {
    pub fn new(agent: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
            model: model.into(),
        }
    }
}

/// One mark on the rail: *Favorites*, first, while there is one (D46); a
/// local agent; or — first after it, and only on a chat that has begun on
/// one — a built-in provider (S1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RailEntry<P> {
    Favorites,
    Provider(P),
    Agent(P),
}

impl<P> RailEntry<P> {
    /// Whose models the entry lists; `None` for *Favorites*, which lists
    /// several agents' — each row says whose.
    pub fn provider(&self) -> Option<&P> {
        match self {
            Self::Favorites => None,
            Self::Provider(provider) | Self::Agent(provider) => Some(provider),
        }
    }
}

/// Where the keyboard is inside the picker; Tab and Shift+Tab move through
/// them in this order, the footer's only while the model offers them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PickerFocus {
    Search,
    Rail,
    List,
    /// The effort strip (D76): Left and Right move the level.
    Effort,
    /// The *⚡ Fast* pill (koru-ai D11): Space or Enter toggles it.
    Fast,
    /// The *1M context* pill (koru-ai D10): Space or Enter toggles it.
    Context,
}

/// **The picker's own state**, which its host keeps and lends it. It lives
/// only while the picker is open; opening resets it. A host reaches it only
/// through the methods below — its fields are the picker's, open to a
/// host's suite under the `test-support` feature alone.
macro_rules! picker_state {
    ($vis:vis) => {
        pub struct PickerState<P> {
            /// The rail's selected entry: whose models the list shows.
            $vis rail: Option<RailEntry<P>>,
            /// First agent mark on the visible rail page.
            $vis rail_offset: usize,
            $vis query: SharedInput,
            /// The row the keyboard is on, by index into the rows.
            $vis highlight: Option<usize>,
            /// The first row shown.
            $vis offset: usize,
            $vis scroll_drag: Option<f32>,
            $vis focus: PickerFocus,
            /// Agents whose ↻ is under way: the list reads *Checking…*
            /// meanwhile.
            $vis refreshing: HashSet<P>,
            /// *Legacy models (N)* is expanded: the agent's older models
            /// follow it in the list (QUESTIONS M4, the user's design,
            /// 2026-11-03).
            $vis legacy_open: bool,
            /// What the person chose, for the host to carry out.
            pub(crate) outcomes: Vec<inputs::Outcome<P>>,
            /// What the picker owes once the host has.
            pub(crate) pending: Option<view::Pending<P>>,
        }
    };
}

#[cfg(feature = "test-support")]
picker_state!(pub);
#[cfg(not(feature = "test-support"))]
picker_state!(pub(crate));

impl<P> PickerState<P> {
    pub fn new(engine: &mut TextEngine) -> Self {
        Self {
            rail: None,
            rail_offset: 0,
            query: SharedInput::new(engine),
            highlight: None,
            offset: 0,
            scroll_drag: None,
            focus: PickerFocus::Search,
            refreshing: HashSet::new(),
            legacy_open: false,
            outcomes: Vec::new(),
            pending: None,
        }
    }

    /// What the person chose since the last drain, in order, for the host
    /// to carry out.
    pub fn take_outcomes(&mut self) -> Vec<inputs::Outcome<P>> {
        std::mem::take(&mut self.outcomes)
    }

    /// Whether the picker still owes something once the host has carried
    /// out the outcomes: then call [`ModelPicker::resume`].
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Whether the list's thumb is held.
    pub fn dragging(&self) -> bool {
        self.scroll_drag.is_some()
    }

    /// The pointer let go: the list's thumb with it.
    pub fn release_drag(&mut self) {
        self.scroll_drag = None;
    }
}

impl<P: Eq + std::hash::Hash> PickerState<P> {
    /// The host asked for `agent`'s ↻ ([`Outcome::Refresh`]): its list reads
    /// *Checking…* until it lands.
    pub fn refresh_started(&mut self, agent: P) {
        self.refreshing.insert(agent);
    }

    /// An answer about `agent`'s models landed: its ↻ is over.
    pub fn refresh_answered(&mut self, agent: &P) {
        self.refreshing.remove(agent);
    }
}

/// **The search field's input, shared with the leaf that draws it.** An
/// element leaf owns what it paints (`koru_draw::el::Leaf` is `'static`)
/// and the input is the picker's state, so the two hold one `TextInput`;
/// nothing else keeps it.
#[derive(Clone)]
pub struct SharedInput(Rc<RefCell<TextInput>>);

impl SharedInput {
    fn new(engine: &mut TextEngine) -> Self {
        Self::with_text(engine, theme::role::row(), "")
    }

    /// An input in `style` holding `text`: a host's filter field that shares
    /// the search field's leaf (the chat's workspace-access panel, D106).
    pub fn with_text(engine: &mut TextEngine, style: theme::TextStyle, text: &str) -> Self {
        Self(Rc::new(RefCell::new(TextInput::with_text(
            engine, style, text,
        ))))
    }

    /// What is typed, unmasked.
    pub fn raw_text(&self) -> String {
        self.0.borrow().raw_text().to_owned()
    }

    /// What is typed, as a field's value.
    pub fn value(&self) -> String {
        self.0.borrow().value()
    }

    pub fn set_text(&self, engine: &mut TextEngine, text: &str) {
        self.0.borrow_mut().set_text(engine, text);
    }

    pub fn drag(&self, engine: &mut TextEngine, r: Rect, x: f32) -> bool {
        self.0.borrow_mut().drag(engine, r, x)
    }

    pub fn release(&self) {
        self.0.borrow_mut().release();
    }

    pub fn key(
        &self,
        engine: &mut TextEngine,
        key: &WinitKey,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Typed {
        self.0.borrow_mut().key(engine, key, text, mods)
    }

    pub fn click(&self, engine: &mut TextEngine, r: Rect, x: f32, y: f32) -> bool {
        self.0.borrow_mut().click(engine, r, x, y)
    }

    /// The field in `r`, with its placeholder while it is empty.
    pub fn draw(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        r: Rect,
        placeholder: &str,
        focused: bool,
    ) {
        self.0.borrow_mut().draw(
            canvas,
            root,
            engine,
            r,
            InputStyle::search(),
            Some(placeholder),
            focused,
        );
    }
}

/// One row of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row<P> {
    /// A group's heading, shown only when the agent has more than one.
    Heading(String),
    /// The agent's own default: its `default` entry when it lists one.
    Default,
    /// A model, by the id the agent (or provider) takes.
    Model(String),
    /// A favorite under *Favorites* (D46): an agent's model, beside the
    /// agent's mark.
    Favorite(P, String),
    /// A word in place of models: listing, checking, failed, no match.
    Note(String),
    /// *Legacy models (N)*, at the foot: the models the agent marks as
    /// older (D11) follow it in the list while it is expanded — a click or
    /// Enter expands it, again collapses it (M4, the user's design). Only
    /// while not searching; a search shows them inline, tagged.
    LegacyToggle,
    /// *Manage agent providers*, under the note of an agent that cannot run
    /// here — not installed, a version Koru does not support, its chat
    /// program missing. Opens the host's agent settings, as the rail's **+**
    /// does: the note says what is wrong, and this is where it is put right.
    /// Only while the host offers that page.
    ManageAgents,
    /// **A search's heading for an entry** (D67): its mark and name, above
    /// what the query found in its list.
    EntryHeading(RailEntry<P>),
    /// **A row a search found in an entry's list** (D67): the row as that
    /// entry lists it, which it is named, chosen and starred by — the
    /// entry need not be the one selected on the rail.
    Found(RailEntry<P>, Box<Row<P>>),
}

impl<P> Row<P> {
    /// A row that chooses something: a model, *Default*, a favorite, or
    /// *Legacy models*.
    pub fn choosable(&self) -> bool {
        match self {
            Self::Found(_, row) => row.choosable(),
            other => matches!(
                other,
                Self::Default | Self::Model(_) | Self::Favorite(..) | Self::LegacyToggle
            ),
        }
    }
}

/// The picker's parts, read back from its element frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub panel: Rect,
    pub rail: Rect,
    /// The search row's box: the field and ↻ at its end.
    pub search: Rect,
    /// The search field itself, short of ↻.
    pub field: Rect,
    pub refresh: Rect,
    pub list: Rect,
    /// The footer under the list (D76): its divider, the effort strip and
    /// the pills' row. `None` for a model that offers none of them.
    pub footer: Option<Rect>,
    /// The effort strip, while the model offers levels.
    pub strip: Option<Rect>,
    /// The *1M context* pill (koru-ai D10).
    pub context: Option<Rect>,
    /// The *⚡ Fast* pill (koru-ai D11), first in its row.
    pub fast: Option<Rect>,
}

impl Geometry {
    /// How many rows the list has room for.
    pub fn visible_rows(&self) -> usize {
        (self.list.h / ROW_H).floor().max(0.0) as usize
    }
}

/// The picker's width and height: as wide as the composer up to
/// [`MAX_W`]; as tall as the search row, [`LIST_ROWS`] rows and the footer
/// at its tallest, whatever the list holds and whether or not the chat's
/// model has a footer — never shorter than the rail needs.
pub fn size(composer_w: f32, rail_h: f32) -> (f32, f32) {
    let width = composer_w.min(MAX_W).max(RAIL_W + 160.0);
    let height = PAD * 2.0 + SEARCH_H + LIST_ROWS as f32 * ROW_H + FOOTER_H;
    (width, height.max(rail_h))
}

/// The picker's parts inside `panel`: the footer at the foot when the
/// model offers `levels`, `context` or `fast` — the strip for levels, the
/// pills' row for any — and the list in what is left. Laid out by the
/// picker's element tree ([`elements::tree`]), whose marks take no room of
/// their own: the rail is as tall as the panel.
#[cfg(any(test, feature = "test-support"))]
pub fn geometry(panel: Rect, levels: bool, context: bool, fast: bool) -> Geometry {
    geometry_with(panel, elements::Footer::shape(levels, context, fast), false)
}

/// The picker's parts inside `panel` with `footer` at its foot — and no
/// rail at its start when `hide_rail`, the content in its room.
pub fn geometry_with(panel: Rect, footer: Option<elements::Footer>, hide_rail: bool) -> Geometry {
    let spec = elements::Spec {
        footer,
        hide_rail,
        ..elements::Spec::default()
    };
    let tree = elements::tree(&spec, elements::Painted::default());
    elements::geometry_of(&elements::layout(&tree, panel, FrameCx::root()), panel)
}

/// The rail's marks, top to bottom; a divider follows each mark whose index
/// is in `dividers` — *Favorites*, and the chat's own built-in provider
/// (S1), each set off from the agents after it.
pub fn rail_rects(rail: Rect, count: usize, dividers: &[usize]) -> Vec<Rect> {
    let column = elements::rail_column(
        (0..count).map(|index| (elements::placeholder_mark(), dividers.contains(&index))),
    );
    elements::layout(&column, rail, FrameCx::root())
        .named(elements::MARK)
        .map(|node| node.rect)
        .collect()
}

/// The height the rail's marks need, with `dividers` dividers among them.
pub fn rail_height(count: usize, dividers: usize) -> f32 {
    PAD * 2.0 + 8.0 + count as f32 * (RAIL_BUTTON + RAIL_GAP) + dividers as f32 * RAIL_DIVIDER
}

/// The ☆ of a model row drawn at `row` (D46): a square at its end, in the
/// middle of the row whatever its height. Where the arithmetic put it before
/// the rows were elements: the record the frame is held to.
#[cfg(any(test, feature = "test-support"))]
pub fn star_rect(row: Rect) -> Rect {
    Rect::new(
        row.x + row.w - STAR_INSET - STAR,
        row.y + (row.h - STAR) * 0.5,
        STAR,
        STAR,
    )
}

/// `favorites` with `favorite` added at the end, or taken out where it was
/// — what one press of a ☆ makes of the list.
pub fn toggled(favorites: &[FavoriteModel], favorite: FavoriteModel) -> Vec<FavoriteModel> {
    if favorites.contains(&favorite) {
        favorites
            .iter()
            .filter(|known| **known != favorite)
            .cloned()
            .collect()
    } else {
        let mut next = favorites.to_vec();
        next.push(favorite);
        next
    }
}

/// **Search** (§9.4): case-insensitive, and every whitespace-separated term
/// must be found in one of the fields — a model's label, its id, its
/// group's label and its description. An empty query matches everything.
pub fn matches(query: &str, fields: &[&str]) -> bool {
    let fields: Vec<String> = fields.iter().map(|field| field.to_lowercase()).collect();
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .all(|term| fields.iter().any(|field| field.contains(&term)))
}

/// **The footer's readout** (§9.3, D76): *Effort: High*, the chosen level
/// in Koru's full words, and *(default)* after it when the level shown is
/// the one the model states as its own. A model that states none reads
/// *Effort: Default* while nothing is chosen.
///
/// Longest first: the readout, then shorter forms of it — *High (default)*,
/// *High* — for a row too narrow for the whole (the pills come first).
pub fn readout(levels: Option<&Levels>, choice: Effort, lang: Lang) -> Vec<String> {
    let shown = levels.map_or(Effort::Default, |levels| effort::resolve(levels, choice));
    let word = effort::word(lang, shown);
    let level = match levels.and_then(effort::default_level) {
        Some(default) if default == shown => {
            format!("{word} {}", t(lang, Key::AiEffortIsDefault))
        }
        _ => word.clone(),
    };
    let mut forms = vec![koru_i18n::fill(t(lang, Key::AiEffortReadout), &[&level])];
    if level != word {
        forms.push(level);
    }
    forms.push(word);
    forms
}

/// The next choosable row from `from` in `step`'s direction, wrapping at
/// neither end; `None` when there is none that way.
pub fn step_highlight(choosable: &[bool], from: Option<usize>, step: isize) -> Option<usize> {
    let len = choosable.len() as isize;
    let mut index = match from {
        Some(index) => index as isize + step,
        None if step >= 0 => 0,
        None => len - 1,
    };
    let direction = if step < 0 { -1 } else { 1 };
    while (0..len).contains(&index) {
        if choosable[index as usize] {
            return Some(index as usize);
        }
        index += direction;
    }
    None
}

/// The first row shown so that `index` is in view, starting from `offset`,
/// with room for `visible` rows.
pub fn scroll_into_view(index: usize, offset: usize, visible: usize) -> usize {
    if index < offset {
        index
    } else if visible > 0 && index >= offset + visible {
        index + 1 - visible
    } else {
        offset
    }
}

/// Where the rail's dividers fall: after *Favorites* and after the chat's
/// own built-in provider, each while an entry follows it.
pub fn rail_dividers<P>(rail: &[RailEntry<P>]) -> Vec<usize> {
    rail.iter()
        .enumerate()
        .filter(|(index, entry)| {
            matches!(entry, RailEntry::Favorites | RailEntry::Provider(_)) && index + 1 < rail.len()
        })
        .map(|(index, _)| index)
        .collect()
}

/// **A list row's identity** (M1): its entry and what it chooses — a
/// model, the agent's *Default*, *Legacy models* — never its place, so a
/// row keeps its key as the list scrolls, filters and reorders. `None` for
/// a heading or a note, which choose nothing.
pub fn row_id<P: Display>(entry: &RailEntry<P>, row: &Row<P>) -> Option<String> {
    let entry_id = |entry: &RailEntry<P>| match entry {
        RailEntry::Favorites => "favorites".to_owned(),
        RailEntry::Provider(provider) => format!("provider:{provider}"),
        RailEntry::Agent(agent) => format!("agent:{agent}"),
    };
    Some(match row {
        Row::Found(entry, row) => return row_id(entry, row),
        Row::Heading(_) | Row::Note(_) | Row::EntryHeading(_) => return None,
        Row::Default => format!("{}\u{1f}default", entry_id(entry)),
        Row::Model(id) => format!("{}\u{1f}model\u{1f}{id}", entry_id(entry)),
        Row::Favorite(agent, id) => {
            format!("{}\u{1f}{agent}\u{1f}{id}", entry_id(entry))
        }
        Row::LegacyToggle => format!("{}\u{1f}legacy", entry_id(entry)),
        Row::ManageAgents => format!("{}\u{1f}manage", entry_id(entry)),
    })
}
