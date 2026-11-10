// SPDX-License-Identifier: MPL-2.0

//! **The composer's model picker** (agent-models §9, rung 5): the agents run
//! down a rail, one mark each; above the selected agent's models sits a
//! *Search models…* field with ↻; the footer (D76, the user's design *A1a*)
//! is the effort strip — one segment per level the model offers, the chat's
//! level raised, and a press on the model's default level the way back to
//! it — over a row of pill toggles, *⚡ Fast* ([`fast`]) and *1M context*
//! ([`context`]), each only when the model offers it, with *Effort: High
//! (default)* at the row's end. It opens upward, above the composer and end-aligned with the chip, as the
//! workspace and tool-approval panels do — below the chip only when there is
//! no room above — and it is driven fully from the keyboard.
//!
//! **Choosing a model keeps the picker open** (D66, the user: *I still
//! need to select the effort*): the ✓ moves and the Fast, Context and level
//! rows below become the new model's at once. A press outside the picker,
//! or Esc, closes it.
//!
//! **Search spans every agent** (D67, the user: *search models should be
//! across all models, not just the scoped selection*): with a query, the
//! list holds what it finds in every entry on the rail, in the rail's
//! order, each under a heading with the entry's mark and name; an empty
//! query is the rail's selected entry again.
//!
//! A model row is one line, the model's name; a ☆ at its end stars it
//! (D46): the rail then leads with *Favorites*, the starred models of every
//! agent turned on, each beside its agent's mark — and the picker opens on
//! it (the user: *by default, open favorites*), until the chat begins.
//! Only a click stars a model: there is no key for it (D47, taken back by
//! the user).
//!
//! **Once the chat has begun there is no rail** (the user: *once the chat
//! is running, hide the model rail since we don't support switching
//! models*): from its first message a chat keeps its provider, so the
//! picker shows that provider's list alone — no rail, no *Favorites*, no
//! **+** — and a search looks only there
//! (`ChatSurface::picker_rail_hidden`). In an agent chat those rows are
//! choosable, from the next message, only where the agent's session host
//! takes a model with each message (`ChatSurface::model_locked`); elsewhere
//! they only show the model the chat started on.
//!
//! **The chrome is an element tree** (D64, [`elements`]): the rail and the
//! content column, the search row, the list and the rows under it are
//! laid out by `koru_draw::el`, and [`Geometry`] is read back from that
//! frame; drawn ([`paint`]), the same tree nests inside the *Popover
//! surface* record. The rows, the search and the footer's words are pure
//! functions, tested beside this file. The surface keeps the state
//! ([`PickerState`]).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use koru_ai_core::{AGENT_DEFAULT_MODEL, AiProviderId, ReasoningChoice, ReasoningControl};
use koru_draw::el::FrameCx;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{brand, icons, scene, theme};
use koru_i18n::{Key, Lang, t};
use koru_ui::Typed;
use koru_ui::popover::PopoverAlign;
use koru_ui::text_input::{InputStyle, TextInput};
use koru_ui::tooltip::TooltipContent;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

use crate::agents::{
    AgentModels, AgentStatus, agent_key, default_model_label, fallback_label, find_listed,
    group_label, is_agent_provider, listed_models, model_groups, model_label, provider_mark,
};
use crate::effort;

mod context;
mod elements;
mod fast;
mod paint;
mod tokens;

/// **The element layout the composer's panels share** (D64, D105): one
/// engine and text cache per UI thread, and one way to key a row by its
/// model's id — the approval-mode list and the workspace-access panel are
/// laid out on them as the picker is.
pub(crate) use elements::{layout as layout_elements, stable_id};
/// The search field's adapter, which the workspace-access panel's filter
/// field is too (D106).
pub(crate) use paint::{SearchLeaf, SearchPaint};

/// The rail's width, and the square of each of its marks.
pub(crate) const RAIL_W: f32 = 44.0;
/// **Air between the rail and the picker's content** — the search row, the
/// list and the footer all start this far right of the rail, so its marks
/// are not flush against them (the composer's toolbar keeps the same 8 px
/// between its controls). The picker lays out with the element token
/// [`elements::CONTENT_GAP`]; this value is what the tests hold it to.
#[cfg(test)]
pub(crate) const RAIL_CONTENT_GAP: f32 = 8.0;
const RAIL_BUTTON: f32 = 32.0;
const RAIL_GAP: f32 = 6.0;
/// The room a divider takes after the chat's own built-in provider (S1).
const RAIL_DIVIDER: f32 = 9.0;
/// Keep the rail shorter than the model list panel; the chevrons page through
/// further agents without letting the popover grow past the viewport.
const RAIL_PAGE: usize = 6;
/// The search row, with ↻ at its end.
pub(crate) const SEARCH_H: f32 = 40.0;
/// A row of the list: a heading, a note or a model, one line each. A
/// model's description is not shown under its name (D43, taken back by the
/// user: only Codex gave its models one), so every row is this tall.
pub(crate) const ROW_H: f32 = 30.0;
/// **The picker's one size** (D13): the search row, six rows of the list
/// and the footer — the box the person chose. The list scrolls inside it
/// when there are more, and a short list leaves room under it rather than
/// shrinking the picker, so the picker keeps its size from agent to agent.
/// A model with no footer gives the room to the list.
pub(crate) const LIST_ROWS: usize = 6;
/// The ☆ at a model row's end (D46): its square, and how far its right edge
/// keeps from the list's, clear of the scrollbar's lane
/// ([`elements::STAR_INSET`]).
const STAR: f32 = 22.0;
#[cfg(test)]
const STAR_INSET: f32 = 12.0;
/// A favorite's agent mark at its row's start.
const ROW_MARK: f32 = 16.0;
/// An effort segment's height, and the effort strip's: the segments and
/// the strip's 2 round them (D76).
pub(crate) const SEGMENT_H: f32 = 28.0;
pub(crate) const STRIP_H: f32 = SEGMENT_H + 4.0;
/// A pill toggle's height: *⚡ Fast*, *1M context*.
pub(crate) const PILL_H: f32 = 28.0;
/// **The footer at its tallest** (D76): the divider, 10 above the effort
/// strip, 8 between it and the pills' row, and 8 under that (12 from the
/// panel's foot with [`PAD`]). Without levels it is the divider and the
/// pills' row alone ([`FOOTER_TOGGLES_H`]).
pub(crate) const FOOTER_H: f32 = 1.0 + 10.0 + STRIP_H + 8.0 + PILL_H + 8.0;
/// The footer of a model with Fast or a longer window and no levels.
#[cfg(test)]
pub(crate) const FOOTER_TOGGLES_H: f32 = 1.0 + 10.0 + PILL_H + 8.0;
const PAD: f32 = 4.0;
/// The picker is as wide as the composer, up to this.
pub(crate) const MAX_W: f32 = 360.0;
/// The tooltips' keys, so moving from one mark or row to the next is a new
/// tooltip: the rail's first mark, ↻, the read-only strip and context
/// pill. A model row has none (D25).
const TOOLTIP_RAIL: u64 = 0x5049_0000;
const TOOLTIP_REFRESH: u64 = 0x5049_1000;
const TOOLTIP_LOCKED: u64 = 0x5049_1002;
const TOOLTIP_CONTEXT_LOCKED: u64 = 0x5049_1003;
/// The note over the strip when a level is chosen during a reply (D70).
const LEVEL_NOTE_KEY: u64 = 0x5049_1004;
const LEVEL_NOTE_HOLD: std::time::Duration = std::time::Duration::from_millis(2200);

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
/// one — Koru AI, OpenAI or Ollama (S1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RailEntry {
    Favorites,
    Provider(AiProviderId),
    Agent(AiProviderId),
}

impl RailEntry {
    /// Whose models the entry lists; `None` for *Favorites*, which lists
    /// several agents' — each row says whose.
    pub(crate) fn provider(&self) -> Option<&AiProviderId> {
        match self {
            Self::Favorites => None,
            Self::Provider(provider) | Self::Agent(provider) => Some(provider),
        }
    }
}

/// Where the keyboard is inside the picker; Tab and Shift+Tab move through
/// them in this order, the footer's only while the model offers them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum PickerFocus {
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

/// The picker's own state on the surface. It lives only while the picker
/// is open; opening resets it.
pub(crate) struct PickerState {
    /// The rail's selected entry: whose models the list shows.
    pub(crate) rail: Option<RailEntry>,
    /// First agent mark on the visible rail page.
    pub(crate) rail_offset: usize,
    pub(crate) query: SharedInput,
    /// The row the keyboard is on, by index into the rows.
    pub(crate) highlight: Option<usize>,
    /// The first row shown.
    pub(crate) offset: usize,
    pub(crate) scroll_drag: Option<f32>,
    pub(crate) focus: PickerFocus,
    /// Agents whose ↻ is under way: the list reads *Checking…* meanwhile.
    pub(crate) refreshing: HashSet<AiProviderId>,
    /// *Legacy models (N)* is expanded: the agent's older models follow it
    /// in the list (QUESTIONS M4, the user's design, 2026-11-03).
    pub(crate) legacy_open: bool,
}

impl PickerState {
    pub(crate) fn new(engine: &mut TextEngine) -> Self {
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
        }
    }
}

/// **The search field's input, shared with the leaf that draws it.** An
/// element leaf owns what it paints (`koru_draw::el::Leaf` is `'static`)
/// and the input is the picker's state, so the two hold one `TextInput`;
/// nothing else keeps it.
#[derive(Clone)]
pub(crate) struct SharedInput(Rc<RefCell<TextInput>>);

impl SharedInput {
    fn new(engine: &mut TextEngine) -> Self {
        Self::with_text(engine, theme::role::row(), "")
    }

    /// An input in `style` holding `text`: the workspace-access panel's
    /// filter (D106), which shares the search field's leaf.
    pub(crate) fn with_text(engine: &mut TextEngine, style: theme::TextStyle, text: &str) -> Self {
        Self(Rc::new(RefCell::new(TextInput::with_text(
            engine, style, text,
        ))))
    }

    /// What is typed, unmasked.
    pub(crate) fn raw_text(&self) -> String {
        self.0.borrow().raw_text().to_owned()
    }

    /// What is typed, as a field's value.
    pub(crate) fn value(&self) -> String {
        self.0.borrow().value()
    }

    pub(crate) fn set_text(&self, engine: &mut TextEngine, text: &str) {
        self.0.borrow_mut().set_text(engine, text);
    }

    pub(crate) fn drag(&self, engine: &mut TextEngine, r: Rect, x: f32) -> bool {
        self.0.borrow_mut().drag(engine, r, x)
    }

    pub(crate) fn release(&self) {
        self.0.borrow_mut().release();
    }

    pub(crate) fn key(
        &self,
        engine: &mut TextEngine,
        key: &WinitKey,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Typed {
        self.0.borrow_mut().key(engine, key, text, mods)
    }

    pub(crate) fn click(&self, engine: &mut TextEngine, r: Rect, x: f32, y: f32) -> bool {
        self.0.borrow_mut().click(engine, r, x, y)
    }

    /// The field in `r`, with its placeholder while it is empty.
    fn draw(
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
pub(crate) enum Row {
    /// A group's heading, shown only when the agent has more than one.
    Heading(String),
    /// The agent's own default: its `default` entry when it lists one.
    Default,
    /// A model, by the id the agent (or provider) takes.
    Model(String),
    /// A favorite under *Favorites* (D46): an agent's model, beside the
    /// agent's mark.
    Favorite(AiProviderId, String),
    /// A word in place of models: listing, checking, failed, no match.
    Note(String),
    /// *Legacy models (N)*, at the foot: the models the agent marks as
    /// older (D11) follow it in the list while it is expanded — a click or
    /// Enter expands it, again collapses it (M4, the user's design). Only
    /// while not searching; a search shows them inline, tagged.
    LegacyToggle,
    /// *Manage agent providers*, under the note of an agent that cannot run
    /// here — not installed, a version Koru does not support, its chat
    /// program missing. Opens Settings → Coding → Agent providers, as the
    /// rail's **+** does: the note says what is wrong, and this is where it
    /// is put right. Only while the host offers that page.
    ManageAgents,
    /// **A search's heading for an entry** (D67): its mark and name, above
    /// what the query found in its list.
    EntryHeading(RailEntry),
    /// **A row a search found in an entry's list** (D67): the row as that
    /// entry lists it, which it is named, chosen and starred by — the
    /// entry need not be the one selected on the rail.
    Found(RailEntry, Box<Row>),
}

impl Row {
    fn choosable(&self) -> bool {
        match self {
            Self::Found(_, row) => row.choosable(),
            other => matches!(
                other,
                Self::Default | Self::Model(_) | Self::Favorite(..) | Self::LegacyToggle
            ),
        }
    }
}

/// The picker's parts, read back from its element frame ([`elements`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Geometry {
    pub(crate) panel: Rect,
    pub(crate) rail: Rect,
    /// The search row's box: the field and ↻ at its end.
    pub(crate) search: Rect,
    /// The search field itself, short of ↻.
    pub(crate) field: Rect,
    pub(crate) refresh: Rect,
    pub(crate) list: Rect,
    /// The footer under the list (D76): its divider, the effort strip and
    /// the pills' row. `None` for a model that offers none of them.
    pub(crate) footer: Option<Rect>,
    /// The effort strip, while the model offers levels.
    pub(crate) strip: Option<Rect>,
    /// The *1M context* pill (koru-ai D10).
    pub(crate) context: Option<Rect>,
    /// The *⚡ Fast* pill (koru-ai D11), first in its row.
    pub(crate) fast: Option<Rect>,
}

impl Geometry {
    /// How many rows the list has room for.
    pub(crate) fn visible_rows(&self) -> usize {
        (self.list.h / ROW_H).floor().max(0.0) as usize
    }
}

/// The picker's width and height: as wide as the composer up to
/// [`MAX_W`]; as tall as the search row, [`LIST_ROWS`] rows and the footer
/// at its tallest, whatever the list holds and whether or not the chat's
/// model has a footer — never shorter than the rail needs.
pub(crate) fn size(composer_w: f32, rail_h: f32) -> (f32, f32) {
    let width = composer_w.min(MAX_W).max(RAIL_W + 160.0);
    let height = PAD * 2.0 + SEARCH_H + LIST_ROWS as f32 * ROW_H + FOOTER_H;
    (width, height.max(rail_h))
}

/// The picker's parts inside `panel`: the footer at the foot when the
/// model offers `levels`, `context` or `fast` — the strip for levels, the
/// pills' row for any — and the list in what is left. Laid out by the
/// picker's element tree ([`elements::tree`]), whose marks take no room of
/// their own: the rail is as tall as the panel.
#[cfg(test)]
pub(crate) fn geometry(panel: Rect, levels: bool, context: bool, fast: bool) -> Geometry {
    geometry_with(panel, elements::Footer::shape(levels, context, fast), false)
}

/// The picker's parts inside `panel` with `footer` at its foot — and no
/// rail at its start when `hide_rail`, the content in its room.
pub(crate) fn geometry_with(
    panel: Rect,
    footer: Option<elements::Footer>,
    hide_rail: bool,
) -> Geometry {
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
pub(crate) fn rail_rects(rail: Rect, count: usize, dividers: &[usize]) -> Vec<Rect> {
    let column = elements::rail_column(
        (0..count).map(|index| (elements::placeholder_mark(), dividers.contains(&index))),
    );
    elements::layout(&column, rail, FrameCx::root())
        .named(elements::MARK)
        .map(|node| node.rect)
        .collect()
}

/// The height the rail's marks need, with `dividers` dividers among them.
pub(crate) fn rail_height(count: usize, dividers: usize) -> f32 {
    PAD * 2.0 + 8.0 + count as f32 * (RAIL_BUTTON + RAIL_GAP) + dividers as f32 * RAIL_DIVIDER
}

/// The ☆ of a model row drawn at `row` (D46): a square at its end, in the
/// middle of the row whatever its height. Where the arithmetic put it before
/// the rows were elements: the record the frame is held to.
#[cfg(test)]
pub(crate) fn star_rect(row: Rect) -> Rect {
    Rect::new(
        row.x + row.w - STAR_INSET - STAR,
        row.y + (row.h - STAR) * 0.5,
        STAR,
        STAR,
    )
}

/// `favorites` with `favorite` added at the end, or taken out where it was
/// — what one press of a ☆ makes of the list.
pub(crate) fn toggled(favorites: &[FavoriteModel], favorite: FavoriteModel) -> Vec<FavoriteModel> {
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
pub(crate) fn matches(query: &str, fields: &[&str]) -> bool {
    let fields: Vec<String> = fields.iter().map(|field| field.to_lowercase()).collect();
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .all(|term| fields.iter().any(|field| field.contains(&term)))
}

/// **The footer's readout** (§9.3, D76): *Effort: High*, the chat's level
/// in Koru's full words, and *(default)* after it when the level shown is
/// the one the model states as its own. A model that states none reads
/// *Effort: Default* while the chat chooses none.
///
/// Longest first: the readout, then shorter forms of it — *High (default)*,
/// *High* — for a row too narrow for the whole (the pills come first).
pub(crate) fn readout(
    control: &ReasoningControl,
    choice: ReasoningChoice,
    lang: Lang,
) -> Vec<String> {
    let shown = effort::resolve(control, choice).unwrap_or(ReasoningChoice::Default);
    let word = effort::word(lang, shown);
    let level = match effort::default_level(control) {
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
pub(crate) fn step_highlight(
    choosable: &[bool],
    from: Option<usize>,
    step: isize,
) -> Option<usize> {
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
pub(crate) fn scroll_into_view(index: usize, offset: usize, visible: usize) -> usize {
    if index < offset {
        index
    } else if visible > 0 && index >= offset + visible {
        index + 1 - visible
    } else {
        offset
    }
}

impl crate::ChatSurface {
    /// **Say the host can open Settings on *Agent providers*** — then the
    /// rail ends in a **+** that asks it to.
    pub fn offer_agent_settings(&mut self, offered: bool) {
        self.agent_settings_offered = offered;
    }

    /// Ask the host for Settings on Coding → Agent providers — what the
    /// rail's **+** does, closing the picker first.
    pub fn ask_for_agent_settings(&mut self) {
        self.close_picker();
        self.agent_settings_request = true;
    }

    /// The rail's **+** was pressed since the last ask: the host opens
    /// Settings on Coding → Agent providers.
    pub fn take_agent_settings_request(&mut self) -> bool {
        std::mem::take(&mut self.agent_settings_request)
    }

    /// The **+** at the rail's end, on the last page while the host offers it.
    #[cfg(test)]
    pub(crate) fn picker_manage_rect(&self, _geometry: &Geometry) -> Option<Rect> {
        self.picker_frame()?
            .2
            .node(elements::MANAGE_KEY)
            .map(|node| node.rect)
    }

    /// **The favorite models** (D46), as the host's roaming setting holds
    /// them. Answers whether anything moved; a star pressed here and not
    /// yet stored is settled by this.
    pub fn set_favorite_models(&mut self, favorites: Vec<FavoriteModel>) -> bool {
        let moved = self.favorite_models != favorites;
        self.favorite_models = favorites;
        if moved {
            self.settle_picker_highlight();
        }
        moved
    }

    /// A ☆ was pressed since the last ask: the whole list, for the host to
    /// store (roaming) and hand to every other chat.
    pub fn take_favorite_models_request(&mut self) -> Option<Vec<FavoriteModel>> {
        self.favorite_models_request.take()
    }

    /// The favorites the picker lists (D46): those of every agent the
    /// service offers and this machine has turned on, in the rail's agent
    /// order and, within an agent, the order they were starred. A turned-off
    /// agent's are kept in the setting and only hidden here.
    pub(crate) fn picker_favorites(&self) -> Vec<(AiProviderId, String)> {
        let mut shown = Vec::new();
        for agent in self.ordered_agents() {
            let Some(key) = agent_key(&agent.id) else {
                continue;
            };
            if !self.agent_shown(&agent.id) {
                continue;
            }
            shown.extend(
                self.favorite_models
                    .iter()
                    .filter(|favorite| favorite.agent == key)
                    .map(|favorite| (agent.id.clone(), favorite.model.clone())),
            );
        }
        shown
    }

    /// Whether `agent`'s model `model` is starred.
    pub(crate) fn is_favorite(&self, agent: &AiProviderId, model: &str) -> bool {
        agent_key(agent).is_some_and(|key| {
            self.favorite_models
                .iter()
                .any(|favorite| favorite.agent == key && favorite.model == model)
        })
    }

    /// The model a row would star: an agent's listed model, or a favorite.
    /// The agent's *Default* is no model of its own, and a built-in
    /// provider's models are not an agent's, so neither has a ☆.
    pub(crate) fn row_favorite(
        &self,
        entry: &RailEntry,
        row: &Row,
    ) -> Option<(AiProviderId, String)> {
        match (entry, row) {
            (_, Row::Found(entry, row)) => self.row_favorite(entry, row),
            (RailEntry::Agent(agent), Row::Model(id)) | (_, Row::Favorite(agent, id)) => {
                agent_key(agent)
                    .is_some()
                    .then(|| (agent.clone(), id.clone()))
            }
            _ => None,
        }
    }

    /// **Star or unstar** `agent`'s `model` (D46): shown at once, and the
    /// whole list handed to the host to store.
    pub(crate) fn toggle_favorite(&mut self, agent: &AiProviderId, model: &str) -> bool {
        let Some(key) = agent_key(agent) else {
            return false;
        };
        let next = toggled(&self.favorite_models, FavoriteModel::new(key, model));
        self.favorite_models = next.clone();
        self.favorite_models_request = Some(next);
        self.settle_picker_highlight();
        true
    }

    /// Keep the keyboard on a row that is still there after the list
    /// changed under it — an unstarred favorite leaves *Favorites*, and the
    /// last one takes *Favorites* off the rail.
    fn settle_picker_highlight(&mut self) {
        if !self.chat_model_open {
            return;
        }
        // An entry that left the rail is replaced by the one shown in its
        // place, so a later star does not bring it back under the pointer.
        if self
            .picker
            .rail
            .as_ref()
            .is_some_and(|entry| !self.picker_rail().contains(entry))
        {
            self.picker.rail = self.picker_entry();
        }
        let rows = self.picker_rows();
        let choosable = self.picker_choosable(&rows);
        let fine = self
            .picker
            .highlight
            .is_some_and(|index| choosable.get(index).copied().unwrap_or(false));
        if !fine {
            let from = self.picker.highlight.map(|index| index.min(rows.len()));
            self.picker.highlight = step_highlight(&choosable, from, -1)
                .or_else(|| step_highlight(&choosable, None, 1));
        }
        self.picker.offset = self.picker.offset.min(self.picker_max_offset());
        self.scroll_highlight_into_view();
    }

    /// What the **+** is called for a screen reader and the inspector — it
    /// raises no tooltip (D25: the picker's rows show none; the rail's marks
    /// keep theirs because they are named nowhere else, and this one is).
    /// The element names it by its key; this is its words, for the tests.
    #[cfg(test)]
    pub(crate) fn picker_manage_name(&self) -> &'static str {
        t(self.lang, Key::AiPickerManageAgents)
    }

    /// The rail's entries: every offered agent in the person's order (Agent
    /// providers; Claude first until it is dragged), led —
    /// on a chat that has begun on Koru AI, OpenAI or Ollama — by that
    /// provider (S1). A chat with no message yet is offered only agents
    /// (the user's call, 2026-10-31). An agent turned off on *Agent
    /// providers* is left out, unless it is the chat's own: that chat keeps
    /// its agent, and keeps working.
    ///
    /// **A begun chat's rail is its own entry alone**, and is not drawn
    /// ([`Self::picker_rail_hidden`]): the chat keeps its provider, so the
    /// list, the search and the keyboard know only that one.
    pub(crate) fn picker_rail(&self) -> Vec<RailEntry> {
        let current = self
            .transcript
            .as_ref()
            .map(|transcript| &transcript.chat.current_model.provider);
        let mut rail = Vec::new();
        if !self.picker_favorites().is_empty() {
            rail.push(RailEntry::Favorites);
        }
        if let Some(transcript) = &self.transcript
            && !transcript.messages.is_empty()
            && !is_agent_provider(&transcript.chat.current_model.provider)
        {
            rail.push(RailEntry::Provider(
                transcript.chat.current_model.provider.clone(),
            ));
        }
        rail.extend(
            self.ordered_agents()
                .into_iter()
                .filter(|agent| self.agent_shown(&agent.id) || Some(&agent.id) == current)
                .map(|agent| RailEntry::Agent(agent.id.clone())),
        );
        if self.picker_rail_hidden() {
            rail.retain(|entry| entry.provider() == current);
        }
        rail
    }

    /// **Whether the rail is hidden: the chat has begun** (the user: *once
    /// the chat is running, hide the model rail since we don't support
    /// switching models*). From its first message a chat belongs to its
    /// provider (AGENTUX D1-2) — the same test by which
    /// [`Self::choose_agent_model`] refuses another agent and
    /// [`Self::entry_choosable`] dims every other agent's rows — so the
    /// rail would offer only what cannot be chosen. The picker then shows the
    /// chat's own provider's list with no rail, *Favorites* or **+**, and
    /// its search looks only there. A new chat with no message, or no chat
    /// yet, keeps the rail.
    pub(crate) fn picker_rail_hidden(&self) -> bool {
        self.transcript
            .as_ref()
            .is_some_and(|transcript| !transcript.messages.is_empty())
    }

    /// The marks the rail draws, presses and steps through: its entries,
    /// or none while it is hidden.
    pub(crate) fn picker_marks(&self) -> Vec<RailEntry> {
        if self.picker_rail_hidden() {
            Vec::new()
        } else {
            self.picker_rail()
        }
    }

    fn rail_page_start(&self, count: usize) -> usize {
        self.picker
            .rail_offset
            .min(count.saturating_sub(1) / RAIL_PAGE * RAIL_PAGE)
    }

    /// Only these marks enter the panel's element tree and hit targets.
    pub(crate) fn picker_visible_rail(&self) -> Vec<RailEntry> {
        let rail = self.picker_marks();
        let start = self.rail_page_start(rail.len());
        rail.into_iter().skip(start).take(RAIL_PAGE).collect()
    }

    pub(crate) fn picker_rail_pages(&self) -> (bool, bool) {
        let count = self.picker_marks().len();
        let start = self.rail_page_start(count);
        (start > 0, start + RAIL_PAGE < count)
    }

    fn page_rail(&mut self, down: bool) {
        let count = self.picker_marks().len();
        let start = self.rail_page_start(count);
        self.picker.rail_offset = if down {
            (start + RAIL_PAGE).min(count.saturating_sub(1) / RAIL_PAGE * RAIL_PAGE)
        } else {
            start.saturating_sub(RAIL_PAGE)
        };
        self.picker.focus = PickerFocus::Rail;
    }

    /// **The rail's entry on open** (§9.1): *Favorites*, while there are
    /// favorites and the rail shows (the user: *by default, open
    /// favorites*); else the chat's own provider; else the default agent
    /// from settings; else the first agent that is ready; else the first.
    /// Every open starts here: the entry looked at last is not kept.
    pub(crate) fn picker_initial_entry(&self) -> Option<RailEntry> {
        let rail = self.picker_rail();
        if !self.picker_rail_hidden() && rail.contains(&RailEntry::Favorites) {
            return Some(RailEntry::Favorites);
        }
        let current = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.current_model.provider.clone());
        // The default agent where one is set (D40), else the last chosen.
        let selected = self.default_agent_provider().or_else(|| {
            self.settings
                .as_ref()
                .and_then(|settings| settings.selected_provider.clone())
        });
        let ready = |entry: &&RailEntry| {
            entry.provider().is_some_and(|provider| {
                self.agent_status.get(provider).copied() == Some(AgentStatus::Ready)
            })
        };
        // With no favorites — the rail then has no *Favorites* — it opens
        // on whose models the chat is on.
        rail.iter()
            .find(|entry| entry.provider() == current.as_ref())
            .or_else(|| {
                rail.iter().find(|entry| {
                    matches!(entry, RailEntry::Agent(_)) && entry.provider() == selected.as_ref()
                })
            })
            .or_else(|| rail.iter().find(ready))
            .or_else(|| rail.first())
            .cloned()
    }

    /// The entry whose models the list shows.
    pub(crate) fn picker_entry(&self) -> Option<RailEntry> {
        let rail = self.picker_rail();
        self.picker
            .rail
            .clone()
            .filter(|entry| rail.contains(entry))
            .or_else(|| self.picker_initial_entry())
    }

    fn picker_query(&self) -> String {
        self.picker.query.raw_text().trim().to_owned()
    }

    /// Whether a row of `entry` can be chosen now: an agent while the chat
    /// has no message and the agent is ready — and in a begun chat its own
    /// agent, where that agent's host takes a model per message
    /// ([`Self::model_locked`]); the chat's own built-in provider's models
    /// always, as today. *Favorites* answers row by row, by each favorite's
    /// agent.
    fn entry_choosable(&self, entry: &RailEntry) -> bool {
        match entry {
            RailEntry::Favorites | RailEntry::Provider(_) => true,
            RailEntry::Agent(agent) => {
                self.agent_status.get(agent).copied() == Some(AgentStatus::Ready)
                    && self.transcript.as_ref().is_none_or(|transcript| {
                        transcript.messages.is_empty()
                            || (&transcript.chat.current_model.provider == agent
                                && !self.model_locked())
                    })
            }
        }
    }

    /// An agent's model as its row reads it — its *Default*, its name, or
    /// its id formatted — with the longer window named when the id is that
    /// window's (koru-ai D10): what a note about the model says.
    pub(crate) fn agent_model_name(&self, agent: &AiProviderId, id: &str) -> String {
        let models = self.listed_agent_models(agent);
        if id == AGENT_DEFAULT_MODEL {
            return default_model_label(models, self.lang);
        }
        match find_listed(models, id) {
            Some((listed, true)) => match &listed.context {
                Some(window) => format!(
                    "{} · {}",
                    model_label(listed),
                    context::pill_label(self.lang, window.tokens)
                ),
                None => model_label(listed),
            },
            Some((listed, false)) => model_label(listed),
            None => fallback_label(id, ""),
        }
    }

    /// Whether `agent` has a model the query finds, for the rail's dimming
    /// while typing (S3): what its group in the search would hold.
    fn agent_matches_query(&self, agent: &AiProviderId, query: &str) -> bool {
        query.is_empty()
            || self
                .agent_model_rows(agent, query)
                .iter()
                .any(|row| !matches!(row, Row::Note(_)))
    }

    /// An entry's name, as its heading in a search and its rail mark's
    /// tooltip read.
    fn entry_name(&self, entry: &RailEntry) -> String {
        match entry {
            RailEntry::Favorites => t(self.lang, Key::AiPickerFavorites).to_owned(),
            RailEntry::Agent(agent) => {
                let agents = self
                    .settings
                    .as_ref()
                    .map(|settings| settings.agents.as_slice())
                    .unwrap_or_default();
                crate::agents::agent_label(agents, agent)
            }
            RailEntry::Provider(provider) => provider_name(provider),
        }
    }

    /// **The list** (§9.2): with a query, what it finds across every entry
    /// ([`Self::search_rows`], D67); without one, the selected entry's. In
    /// a begun chat, with no rail ([`Self::picker_rail_hidden`]), a query
    /// narrows the chat's own provider's list and finds nothing elsewhere.
    pub(crate) fn picker_rows(&self) -> Vec<Row> {
        let query = self.picker_query();
        if !query.is_empty() && !self.picker_rail_hidden() {
            return self.search_rows(&query);
        }
        let Some(entry) = self.picker_entry() else {
            return Vec::new();
        };
        self.entry_rows(&entry, &query)
    }

    /// **A search across every entry** (D67): each agent on the rail — the
    /// ones turned on, and the chat's own — and a begun chat's built-in
    /// provider, in the rail's order, with what `query` finds in its list
    /// under a heading of its own; an entry where it finds nothing is left
    /// out. *Favorites* is not searched apart: its models are the agents'.
    /// The rows are the entry's own, so whether one can be chosen is what
    /// that entry says, as on its own list.
    fn search_rows(&self, query: &str) -> Vec<Row> {
        let mut rows = Vec::new();
        for entry in self.picker_rail() {
            if entry == RailEntry::Favorites {
                continue;
            }
            // Its words — loading, checking, nothing found — and its own
            // group headings stay on its own list.
            let found: Vec<Row> = self
                .entry_rows(&entry, query)
                .into_iter()
                .filter(|row| !matches!(row, Row::Note(_) | Row::Heading(_)))
                .map(|row| Row::Found(entry.clone(), Box::new(row)))
                .collect();
            if found.is_empty() {
                continue;
            }
            rows.push(Row::EntryHeading(entry));
            rows.extend(found);
        }
        if rows.is_empty() {
            rows.push(Row::Note(t(self.lang, Key::AiModelsNoMatch).to_owned()));
        }
        rows
    }

    /// **The list for `entry`** (§9.2): the agent's *Default*, then its
    /// models — by group with a heading when it has more than one — the
    /// query applied; or the one word its state calls for.
    fn entry_rows(&self, entry: &RailEntry, query: &str) -> Vec<Row> {
        let note = |key: Key| Row::Note(t(self.lang, key).to_owned());
        match entry {
            // Every agent's starred models; a query searches every agent's
            // list instead (D67).
            RailEntry::Favorites => {
                let rows: Vec<Row> = self
                    .picker_favorites()
                    .into_iter()
                    .map(|(agent, id)| Row::Favorite(agent, id))
                    .collect();
                if rows.is_empty() {
                    vec![note(Key::AiModelsNoMatch)]
                } else {
                    rows
                }
            }
            RailEntry::Provider(provider) => {
                let name = provider_name(provider);
                let models = if provider.as_str() == AiProviderId::KORU {
                    vec![AiProviderId::KORU_HOSTED_MODEL.to_owned()]
                } else {
                    self.chat_model_options()
                };
                let rows: Vec<Row> = models
                    .into_iter()
                    .filter(|model| {
                        matches(query, &[model, &self.provider_model_label(model), &name])
                    })
                    .map(Row::Model)
                    .collect();
                if rows.is_empty() && !query.is_empty() {
                    vec![note(Key::AiModelsNoMatch)]
                } else {
                    rows
                }
            }
            RailEntry::Agent(agent) => {
                let status = self
                    .agent_status
                    .get(agent)
                    .copied()
                    .unwrap_or(AgentStatus::Checking);
                let known = matches!(self.agent_models.get(agent), Some(AgentModels::Listed(_)));
                match status {
                    AgentStatus::Ready if self.picker.refreshing.contains(agent) => {
                        vec![note(Key::TerminalChecking)]
                    }
                    AgentStatus::Ready => match self.agent_models.get(agent) {
                        None | Some(AgentModels::Loading) => vec![note(Key::AiAgentModelsLoading)],
                        Some(AgentModels::Failed) => {
                            let mut rows = vec![note(Key::AiAgentModelsFailed)];
                            if matches(
                                query,
                                &[
                                    &default_model_label(&[], self.lang),
                                    &self.entry_name(entry),
                                ],
                            ) {
                                rows.push(Row::Default);
                            }
                            rows
                        }
                        Some(AgentModels::Listed(_)) => self.agent_model_rows(agent, query),
                    },
                    // A list kept from the last run, dimmed, under *Checking…*.
                    AgentStatus::Checking if known => {
                        let mut rows = vec![note(Key::TerminalChecking)];
                        rows.extend(
                            self.agent_model_rows(agent, query)
                                .into_iter()
                                .filter(|row| !matches!(row, Row::Note(_))),
                        );
                        rows
                    }
                    other => {
                        let mut rows =
                            vec![Row::Note(other.word(self.lang).unwrap_or_else(|| {
                                t(self.lang, Key::TerminalChecking).to_owned()
                            }))];
                        // Checking will settle on its own; everything else
                        // here is put right in *Agent providers*.
                        if other != AgentStatus::Checking && self.agent_settings_offered {
                            rows.push(Row::ManageAgents);
                        }
                        rows
                    }
                }
            }
        }
    }

    /// An agent's listed models as rows, the query applied: a model is
    /// found by its name, id, group and description — the description is
    /// matched though no row shows it (D43) — and by its agent's name, so
    /// a search across every agent can be narrowed to one (as *Favorites*
    /// was searched, D46).
    fn agent_model_rows(&self, agent: &AiProviderId, query: &str) -> Vec<Row> {
        let models = self.listed_agent_models(agent);
        let name = self.entry_name(&RailEntry::Agent(agent.clone()));
        let mut rows = Vec::new();
        let default = models.iter().find(|model| model.id == AGENT_DEFAULT_MODEL);
        if matches(
            query,
            &[
                &default_model_label(models, self.lang),
                default
                    .and_then(|model| model.description.as_deref())
                    .unwrap_or_default(),
                &name,
            ],
        ) {
            rows.push(Row::Default);
        }
        // Headings for the groups the current models fall into; an older
        // model's group is the flyout's.
        let groups: Vec<String> = model_groups(models)
            .into_iter()
            .filter(|group| listed_models(models).any(|m| &m.group == group && !m.legacy))
            .collect();
        for group in &groups {
            let heading = group_label(models, group);
            let found: Vec<Row> = listed_models(models)
                .filter(|model| &model.group == group && !model.legacy)
                .filter(|model| {
                    matches(
                        query,
                        &[
                            &model_label(model),
                            &model.id,
                            &heading,
                            model.description.as_deref().unwrap_or_default(),
                            &name,
                        ],
                    )
                })
                .map(|model| Row::Model(model.id.clone()))
                .collect();
            if found.is_empty() {
                continue;
            }
            if groups.len() > 1 {
                rows.push(Row::Heading(heading));
            }
            rows.extend(found);
        }
        // The models the agent marks as older (D11): under one row at the
        // foot, while it is expanded (M4), or — while searching — inline
        // after the others, tagged.
        let legacy: Vec<_> = listed_models(models).filter(|m| m.legacy).collect();
        if query.is_empty() {
            if !legacy.is_empty() {
                rows.push(Row::LegacyToggle);
                if self.picker.legacy_open {
                    rows.extend(legacy.iter().map(|model| Row::Model(model.id.clone())));
                }
            }
        } else {
            rows.extend(
                legacy
                    .into_iter()
                    .filter(|model| {
                        let heading = group_label(models, &model.group);
                        matches(
                            query,
                            &[
                                &model_label(model),
                                &model.id,
                                &heading,
                                model.description.as_deref().unwrap_or_default(),
                                &name,
                            ],
                        )
                    })
                    .map(|model| Row::Model(model.id.clone())),
            );
        }
        if rows.is_empty() {
            rows.push(Row::Note(t(self.lang, Key::AiModelsNoMatch).to_owned()));
        }
        rows
    }

    /// Whether row `row` of `entry` is a model its agent marks as older.
    fn row_is_legacy(&self, entry: &RailEntry, row: &Row) -> bool {
        match (entry, row) {
            (_, Row::Found(entry, row)) => self.row_is_legacy(entry, row),
            (RailEntry::Agent(agent), Row::Model(id)) => self
                .listed_agent_models(agent)
                .iter()
                .any(|model| &model.id == id && model.legacy),
            _ => false,
        }
    }

    /// **Expand or collapse *Legacy models (N)*** (M4, the user's design):
    /// the agent's older models follow the row in the same list, or go.
    /// The keyboard stays on the row; the list keeps in range.
    fn toggle_legacy(&mut self, index: usize, open: bool) -> bool {
        if self.picker.legacy_open == open {
            return true;
        }
        self.picker.legacy_open = open;
        self.picker.highlight = Some(index);
        self.settle_after_choice();
        true
    }

    /// Whether the chat's own model is one `entry` lists as older: then its
    /// *Legacy models* row opens expanded, so the ✓ is in the list.
    fn current_is_legacy(&self, entry: &RailEntry) -> bool {
        let (Some(transcript), RailEntry::Agent(agent)) = (&self.transcript, entry) else {
            return false;
        };
        let model = &transcript.chat.current_model;
        &model.provider == agent
            && find_listed(self.listed_agent_models(agent), &model.requested_model)
                .is_some_and(|(listed, _)| listed.legacy)
    }

    /// How many older models `entry` lists: *Legacy models (N)*.
    fn legacy_count(&self, entry: &RailEntry) -> usize {
        match entry {
            RailEntry::Agent(agent) => listed_models(self.listed_agent_models(agent))
                .filter(|model| model.legacy)
                .count(),
            RailEntry::Favorites | RailEntry::Provider(_) => 0,
        }
    }

    /// A built-in provider's model as its row reads.
    fn provider_model_label(&self, model: &str) -> String {
        if model == AiProviderId::KORU_HOSTED_MODEL {
            AiProviderId::KORU_HOSTED_MODEL_LABEL.to_owned()
        } else {
            model.to_owned()
        }
    }

    /// Which rows can be chosen now. A favorite can be when its own agent's
    /// rows could be, and a search's row when its entry's could (D67).
    fn picker_choosable(&self, rows: &[Row]) -> Vec<bool> {
        let entry_choosable = self
            .picker_entry()
            .is_some_and(|entry| self.entry_choosable(&entry));
        rows.iter()
            .map(|row| match row {
                // Showing the older models chooses nothing: it works in a
                // begun chat too, even where its models are only shown.
                Row::LegacyToggle => true,
                // Opening Settings chooses nothing either.
                Row::ManageAgents => true,
                Row::Favorite(agent, _) => self.entry_choosable(&RailEntry::Agent(agent.clone())),
                // A search's row can be chosen as on its own entry's list.
                Row::Found(entry, found) => self.entry_choosable(entry) && found.choosable(),
                // A begun agent chat moves only to a model its agent names:
                // *Default* is the agent's own choice when a session starts.
                Row::Default if self.agent_chat_begun() => false,
                _ => entry_choosable && row.choosable(),
            })
            .collect()
    }

    /// A row's label. A row raises no tooltip (D25, the user: *don't show
    /// tooltips*): the row already reads the model's name, and the tooltip
    /// that repeated it — with the description and id beside it — covered
    /// the next rows while choosing.
    fn picker_row_label(&self, entry: &RailEntry, row: &Row) -> String {
        match (entry, row) {
            (_, Row::Found(entry, row)) => self.picker_row_label(entry, row),
            (_, Row::EntryHeading(entry)) => self.entry_name(entry),
            (_, Row::Heading(text) | Row::Note(text)) => text.clone(),
            (RailEntry::Provider(_), Row::Model(id)) => self.provider_model_label(id),
            (RailEntry::Provider(_) | RailEntry::Favorites, Row::Default | Row::Model(_)) => {
                String::new()
            }
            (_, Row::ManageAgents) => t(self.lang, Key::AiPickerManageAgents).to_owned(),
            (_, Row::LegacyToggle) => koru_i18n::fill(
                t(self.lang, Key::AiModelsLegacy),
                &[&koru_i18n::number::decimal(
                    self.lang,
                    self.legacy_count(entry) as u64,
                )],
            ),
            (RailEntry::Agent(agent), Row::Default) => {
                default_model_label(self.listed_agent_models(agent), self.lang)
            }
            (RailEntry::Agent(agent), Row::Model(id)) | (_, Row::Favorite(agent, id)) => {
                let models = self.listed_agent_models(agent);
                match models.iter().find(|model| &model.id == id) {
                    Some(model) => model_label(model),
                    None => fallback_label(id, ""),
                }
            }
        }
    }

    /// **The open picker's frame, with its list** (M1): where a press
    /// lands, which row it is, the list's scrollbar — what is drawn, laid
    /// out without the pointer.
    fn picker_frame(&self) -> Option<(Geometry, elements::Spec, koru_draw::el::Frame)> {
        let geometry = self.picker_geometry()?;
        let spec = self.picker_spec();
        let frame = elements::layout(
            &elements::tree(&spec, elements::Painted::default()),
            geometry.panel,
            FrameCx::root(),
        );
        Some((geometry, spec, frame))
    }

    /// Row `index` where it is drawn, while it is in view.
    #[cfg(test)]
    pub(crate) fn picker_row_rect(&self, _geometry: &Geometry, index: usize) -> Option<Rect> {
        let (geometry, _, frame) = self.picker_frame()?;
        let rect = frame.named(elements::MODEL_ROW).nth(index)?.rect;
        let list = geometry.list;
        (rect.y >= list.y - 0.01 && rect.y + rect.h <= list.y + list.h + 0.01).then_some(rect)
    }

    /// The effort strip's segment for `choice`, where it is drawn.
    #[cfg(test)]
    pub(crate) fn picker_segment_rect(&self, choice: ReasoningChoice) -> Option<Rect> {
        let (_, _, frame) = self.picker_frame()?;
        Some(frame.node(elements::segment_key(choice))?.rect)
    }

    /// How many rows the open list shows.
    fn picker_visible_rows(&self) -> usize {
        self.picker_geometry()
            .map_or(LIST_ROWS, |geometry| geometry.visible_rows())
    }

    /// The furthest the list scrolls: its last row at the list's foot.
    fn picker_max_offset(&self) -> usize {
        self.picker_rows()
            .len()
            .saturating_sub(self.picker_visible_rows())
    }

    /// Whether a row is the chat's current model.
    fn picker_row_current(&self, entry: &RailEntry, row: &Row) -> bool {
        let Some(model) = self
            .transcript
            .as_ref()
            .map(|transcript| &transcript.chat.current_model)
        else {
            return false;
        };
        if let Row::Found(entry, row) = row {
            return self.picker_row_current(entry, row);
        }
        // Whose row it is: the entry's, or a favorite's own agent.
        let (provider, id) = match (row, entry.provider()) {
            (Row::Favorite(agent, id), _) => (agent, Some(id)),
            (Row::Default, Some(provider)) => (provider, None),
            (Row::Model(id), Some(provider)) => (provider, Some(id)),
            _ => return false,
        };
        if &model.provider != provider {
            return false;
        }
        match id {
            None => model.requested_model == AGENT_DEFAULT_MODEL,
            // A chat on a model's longer context is on that model.
            Some(id) => {
                &model.requested_model == id
                    || find_listed(self.listed_agent_models(provider), &model.requested_model)
                        .is_some_and(|(listed, _)| &listed.id == id)
            }
        }
    }

    /// The picker follows the model chip and flips to the side with room.
    pub(crate) fn picker_geometry(&self) -> Option<Geometry> {
        if !self.chat_model_open {
            return None;
        }
        let hide_rail = self.picker_rail_hidden();
        let rail = self.picker_visible_rail();
        let (previous, next) = self.picker_rail_pages();
        let marks = rail.len()
            + usize::from(previous)
            + usize::from(next)
            + usize::from(self.agent_settings_offered && !hide_rail && !next);
        let rail_h = rail_height(marks, rail_dividers(&rail).len());
        let (w, h) = size(self.composer_hit_rect.w, rail_h);
        let chip = self.chat_model_button();
        let panel = self.toolbar_popover(chip, w, h, PopoverAlign::End);
        // The footer as it is drawn: its pills are as wide as their words.
        Some(geometry_with(panel, self.picker_footer(), hide_rail))
    }

    /// The list's scrollbar, as the frame draws it (M1: the shared bar).
    #[cfg(test)]
    pub(crate) fn picker_scrollbar(
        &self,
        _geometry: &Geometry,
    ) -> Option<koru_draw::scrollbar::ScrollbarGeom> {
        self.picker_frame()?.2.scrollbar(elements::LIST_KEY)
    }

    /// The first row a drag of the list's thumb to `y` shows, the thumb
    /// held `grab` below its top: the frame's whole rows.
    fn picker_drag_to(&self, frame: &koru_draw::el::Frame, y: f32, grab: f32) -> Option<usize> {
        let scroll = frame.drag_offset(elements::LIST_KEY, y, grab)?;
        Some(((scroll / ROW_H).round().max(0.0) as usize).min(self.picker_max_offset()))
    }

    /// **Open the picker** on the chat's own entry, the query empty, the
    /// search field focused and the current model in view.
    pub(crate) fn reset_picker(&mut self) {
        self.picker.rail = self.picker_initial_entry();
        self.picker.rail_offset = self
            .picker
            .rail
            .as_ref()
            .and_then(|selected| {
                self.picker_marks()
                    .iter()
                    .position(|entry| entry == selected)
            })
            .map_or(0, |index| index / RAIL_PAGE * RAIL_PAGE);
        self.picker.legacy_open = self
            .picker
            .rail
            .as_ref()
            .is_some_and(|entry| self.current_is_legacy(entry));
        self.picker.query.set_text(&mut self.engine, "");
        self.picker.focus = PickerFocus::Search;
        self.picker.scroll_drag = None;
        self.picker.offset = 0;
        self.highlight_current_row();
    }

    /// Put the keyboard on the current model's row, or the first choosable
    /// one, and scroll it into view.
    fn highlight_current_row(&mut self) {
        let rows = self.picker_rows();
        let choosable = self.picker_choosable(&rows);
        let entry = self.picker_entry();
        let current = entry.as_ref().and_then(|entry| {
            rows.iter()
                .position(|row| self.picker_row_current(entry, row))
        });
        self.picker.highlight = current.or_else(|| step_highlight(&choosable, None, 1));
        self.scroll_highlight_into_view();
    }

    fn scroll_highlight_into_view(&mut self) {
        let Some(index) = self.picker.highlight else {
            self.picker.offset = 0;
            return;
        };
        let visible = self.picker_visible_rows();
        self.picker.offset = scroll_into_view(index, self.picker.offset, visible);
    }

    /// **Show `entry`'s list**: a mark pressed on the rail, or stepped to
    /// from it. A query is cleared, as its results span every entry (D67)
    /// and would hide the list asked for.
    fn select_rail_entry(&mut self, entry: RailEntry) {
        if let Some(index) = self.picker_marks().iter().position(|mark| mark == &entry) {
            self.picker.rail_offset = index / RAIL_PAGE * RAIL_PAGE;
        }
        self.picker.legacy_open = self.current_is_legacy(&entry);
        if !self.picker.query.raw_text().is_empty() {
            self.picker.query.set_text(&mut self.engine, "");
        }
        // The lists the entry's rows are named from: the agent's, or under
        // *Favorites* every favorite's agent's.
        let agents: Vec<AiProviderId> = match &entry {
            RailEntry::Agent(agent) => vec![agent.clone()],
            RailEntry::Favorites => self
                .picker_favorites()
                .into_iter()
                .map(|(agent, _)| agent)
                .collect(),
            RailEntry::Provider(_) => Vec::new(),
        };
        for agent in agents {
            if !matches!(self.agent_models.get(&agent), Some(AgentModels::Listed(_))) {
                self.ask_agent_models(agent);
            }
        }
        self.picker.rail = Some(entry);
        self.picker.offset = 0;
        self.highlight_current_row();
    }

    /// **Choose row `index`**: an agent's model makes the agent and model
    /// the chat's (the level kept when it is offered, D18) — a favorite
    /// too, with its own agent (D46); a built-in provider's model is the
    /// chat's next model. **The picker stays open** (D66): the ✓ moves to
    /// the row and the footer becomes the new model's, to be set next. A
    /// press outside, or Esc, closes it.
    pub(crate) fn choose_picker_row(&mut self, index: usize) -> bool {
        let rows = self.picker_rows();
        let choosable = self.picker_choosable(&rows);
        let (Some(entry), Some(row)) = (self.picker_entry(), rows.get(index)) else {
            return true;
        };
        if !choosable.get(index).copied().unwrap_or(false) {
            return true;
        }
        if *row == Row::LegacyToggle {
            return self.toggle_legacy(index, !self.picker.legacy_open);
        }
        if *row == Row::ManageAgents {
            self.ask_for_agent_settings();
            return true;
        }
        // A search's row is its own entry's (D67).
        let (entry, row) = match row.clone() {
            Row::Found(entry, row) => (entry, *row),
            row => (entry, row),
        };
        self.picker.highlight = Some(index);
        let chosen = match (entry, row) {
            (RailEntry::Agent(agent), Row::Default) => {
                self.choose_agent_model(agent, AGENT_DEFAULT_MODEL.to_owned())
            }
            (RailEntry::Agent(agent), Row::Model(id)) | (_, Row::Favorite(agent, id)) => {
                self.choose_agent_model(agent, id)
            }
            (RailEntry::Provider(_), Row::Model(id)) => self.change_chat_model(id),
            _ => true,
        };
        self.settle_after_choice();
        chosen
    }

    /// **The picker after a model is chosen** (D66): still open, its
    /// footer now the new model's — Fast, Context and the level come and go
    /// with it, and the list gets or gives up their room — so the list is
    /// kept in range and the chosen row in view. Also run when a chat's
    /// model lands from the service, which a chat already made answers.
    pub(crate) fn settle_after_choice(&mut self) {
        if !self.chat_model_open {
            return;
        }
        self.picker.offset = self.picker.offset.min(self.picker_max_offset());
        self.scroll_highlight_into_view();
    }

    /// **↻** (D13): the selected agent's list rebuilt now. *Checking…*
    /// stands in the list until it lands; a second press waits for the
    /// first. Not for an agent that is not installed.
    pub(crate) fn refresh_picker_agent(&mut self) -> bool {
        let Some(RailEntry::Agent(agent)) = self.picker_entry() else {
            return false;
        };
        if !self.refresh_enabled(&agent) {
            return true;
        }
        let sent = self.reader.as_ref().is_some_and(|reader| {
            reader
                .send(crate::reader::Command::RefreshModels(agent.clone()))
                .is_ok()
        });
        if sent {
            self.picker.refreshing.insert(agent.clone());
            self.agent_models_asked.insert(agent);
        }
        true
    }

    fn refresh_enabled(&self, agent: &AiProviderId) -> bool {
        !self.picker.refreshing.contains(agent)
            && self.agent_status.get(agent).copied() != Some(AgentStatus::NotInstalled)
    }

    /// An answer about `agent`'s models landed: its ↻ is over.
    pub(crate) fn agent_models_answered(&mut self, agent: &AiProviderId) {
        self.picker.refreshing.remove(agent);
    }

    /// A press while the picker is open. `None` when the press is not the
    /// picker's — on the chip, which toggles it — and `Some` when the picker
    /// took it, closing it for a press outside.
    pub(crate) fn picker_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        let (geometry, spec, frame) = self.picker_frame()?;
        if !geometry.panel.contains(x, y) {
            if self.chat_model_button().contains(x, y) {
                return None;
            }
            self.close_picker();
            return Some(true);
        }
        if let Some(scrollbar) = frame.scrollbar(elements::LIST_KEY)
            && scrollbar.in_gutter(x, y)
        {
            let grab = scrollbar.grab_at(x, y);
            self.picker.scroll_drag = Some(grab);
            if let Some(offset) = self.picker_drag_to(&frame, y, grab) {
                self.picker.offset = offset;
            }
            return Some(true);
        }
        let hit = frame.hit_at(x, y);
        if hit == Some(elements::RAIL_PREVIOUS_KEY) {
            self.page_rail(false);
            return Some(true);
        }
        if hit == Some(elements::RAIL_NEXT_KEY) {
            self.page_rail(true);
            return Some(true);
        }
        let rail = self.picker_visible_rail();
        if let Some(index) = rail
            .iter()
            .position(|entry| hit == Some(elements::mark_key(entry)))
        {
            self.picker.focus = PickerFocus::Rail;
            self.select_rail_entry(rail[index].clone());
            return Some(true);
        }
        if hit == Some(elements::MANAGE_KEY) {
            self.ask_for_agent_settings();
            return Some(true);
        }
        if geometry.refresh.contains(x, y) {
            return Some(self.refresh_picker_agent());
        }
        if geometry.search.contains(x, y) {
            self.picker.focus = PickerFocus::Search;
            self.picker
                .query
                .click(&mut self.engine, geometry.search, x, y);
            return Some(true);
        }
        // The footer (D76): a level's segment — read only once an agent
        // chat has begun on an older session host, the press saying why
        // (D70) — and the pills.
        if geometry.strip.is_some_and(|strip| strip.contains(x, y)) {
            self.picker.focus = PickerFocus::Effort;
            if self.effort_locked() {
                self.status = Some(t(self.lang, Key::AiEffortLockedAfterStart).to_owned());
                return Some(true);
            }
            let pressed = spec.footer.as_ref().and_then(|footer| {
                footer.segments.iter().find(|segment| {
                    frame
                        .node(segment.key)
                        .is_some_and(|node| node.rect.contains(x, y))
                })
            });
            return Some(match pressed {
                Some(segment) => self.choose_level(segment.choice),
                None => true,
            });
        }
        if geometry.fast.is_some_and(|pill| pill.contains(x, y)) {
            self.picker.focus = PickerFocus::Fast;
            return Some(self.toggle_fast());
        }
        if geometry.context.is_some_and(|pill| pill.contains(x, y)) {
            self.picker.focus = PickerFocus::Context;
            return Some(self.toggle_context());
        }
        if geometry.list.contains(x, y) {
            self.picker.focus = PickerFocus::List;
            let hit = frame.hit_at(x, y);
            // The ☆ stars the row rather than choosing it (D46), and
            // whether or not the row can be chosen now: it is a target of
            // its own, over its row.
            if let Some(index) = spec
                .list
                .iter()
                .position(|row| row.star.as_ref().is_some_and(|star| Some(star.key) == hit))
                && let Some(entry) = self.picker_entry()
                && let Some((agent, model)) = self
                    .picker_rows()
                    .get(index)
                    .and_then(|row| self.row_favorite(&entry, row))
            {
                self.picker.highlight = Some(index);
                return Some(self.toggle_favorite(&agent, &model));
            }
            if let Some(index) = spec
                .list
                .iter()
                .position(|row| row.key.is_some() && row.key == hit)
            {
                return Some(self.choose_picker_row(index));
            }
        }
        Some(true)
    }

    /// **Choose a level on the strip** (D76): the model's own default level
    /// is the chat's *Default* — so a press on it is the way back to the
    /// model's default, with no ↺ — and any other is that level. In a begun
    /// agent chat it applies from the next message (D70), read only where
    /// the session host takes no level per message.
    pub(crate) fn choose_level(&mut self, choice: ReasoningChoice) -> bool {
        if self.effort_locked() {
            self.status = Some(t(self.lang, Key::AiEffortLockedAfterStart).to_owned());
            return true;
        }
        let Some(control) = self.effort_control().cloned() else {
            return false;
        };
        let target = if effort::default_level(&control) == Some(choice) {
            ReasoningChoice::Default
        } else {
            choice
        };
        let current = self
            .transcript
            .as_ref()
            .map_or(ReasoningChoice::Default, |transcript| {
                transcript.chat.current_model.default_reasoning
            });
        if current == target {
            return true;
        }
        let chosen = self.choose_effort(target);
        if chosen && self.attempt.is_some() {
            self.announce_level_next_message(choice);
        }
        chosen
    }

    /// **The note over the strip while a reply runs** (D70): the level
    /// applies to the next message — the one running keeps its own — in
    /// the words the approval-mode chip uses for a mode. With no reply
    /// running there is none: the strip already shows the level.
    pub(crate) fn announce_level_next_message(&mut self, level: ReasoningChoice) {
        let Some(anchor) = self
            .picker_geometry()
            .and_then(|geometry| geometry.strip)
            .or_else(|| Some(self.chat_model_button()))
        else {
            return;
        };
        let label = koru_i18n::fill(
            t(self.lang, Key::AiAgentModeNextMessage),
            &[t(self.lang, effort::word_key(level))],
        );
        self.show_next_message_note(anchor, label);
    }

    /// **The note over the picker when a model is chosen during a reply**:
    /// the model, named as its row reads, applies to the next message, in
    /// the words the level's note uses.
    pub(crate) fn announce_model_next_message(&mut self, name: &str) {
        let anchor = self
            .picker_geometry()
            .map_or_else(|| self.chat_model_button(), |geometry| geometry.list);
        let label = koru_i18n::fill(t(self.lang, Key::AiAgentModeNextMessage), &[name]);
        self.show_next_message_note(anchor, label);
    }

    fn show_next_message_note(&mut self, anchor: Rect, label: String) {
        self.copy_feedback.show_transient(
            TooltipContent::new(LEVEL_NOTE_KEY, anchor, label),
            std::time::Instant::now(),
            LEVEL_NOTE_HOLD,
        );
    }

    /// The pointer moved with the picker open: a scrollbar drag, and the
    /// tooltip under it. `Some` when the picker claimed the move.
    pub(crate) fn picker_mouse_move(&mut self, x: f32, y: f32) -> Option<bool> {
        let geometry = self.picker_geometry()?;
        if let Some(grab) = self.picker.scroll_drag {
            if let Some((_, _, frame)) = self.picker_frame()
                && let Some(next) = self.picker_drag_to(&frame, y, grab)
            {
                let changed = next != self.picker.offset;
                self.picker.offset = next;
                return Some(changed);
            }
            return Some(true);
        }
        let tip = self.picker_tooltip_at(&geometry, x, y);
        let changed = self
            .picker_tooltip
            .set_hover(tip, std::time::Instant::now());
        // Over the panel the picker is all there is; outside it, only a
        // tooltip that went away is news.
        (geometry.panel.contains(x, y) || changed).then_some(true)
    }

    fn picker_tooltip_at(&self, geometry: &Geometry, x: f32, y: f32) -> Option<TooltipContent> {
        if !geometry.panel.contains(x, y) {
            return None;
        }
        let rail = self.picker_marks();
        for (index, rect) in rail_rects(geometry.rail, rail.len(), &rail_dividers(&rail))
            .into_iter()
            .enumerate()
        {
            if rect.contains(x, y) {
                let entry = &rail[index];
                let name = self.entry_name(entry);
                let word = entry
                    .provider()
                    .and_then(|provider| self.agent_status.get(provider))
                    .and_then(|status| status.word(self.lang))
                    .filter(|_| matches!(entry, RailEntry::Agent(_)));
                let label = match word {
                    Some(word) => format!("{name} — {word}"),
                    None => name,
                };
                return Some(TooltipContent::new(
                    TOOLTIP_RAIL + index as u64,
                    rect,
                    label,
                ));
            }
        }
        if geometry.refresh.contains(x, y)
            && matches!(self.picker_entry(), Some(RailEntry::Agent(_)))
        {
            return Some(TooltipContent::new(
                TOOLTIP_REFRESH,
                geometry.refresh,
                t(self.lang, Key::AiModelsRefresh),
            ));
        }
        if let Some(context) = geometry.context
            && context.contains(x, y)
            && self.model_locked()
        {
            return Some(TooltipContent::new(
                TOOLTIP_CONTEXT_LOCKED,
                context,
                t(self.lang, Key::AiModelContextLockedAfterStart),
            ));
        }
        if let Some(strip) = geometry.strip
            && strip.contains(x, y)
            && self.effort_locked()
        {
            return Some(TooltipContent::new(
                TOOLTIP_LOCKED,
                strip,
                t(self.lang, Key::AiEffortLockedAfterStart),
            ));
        }
        // **A model row raises none** (D25): the row already reads its name,
        // and a tooltip over the list hid the rows below it while choosing.
        // A rail mark keeps its tooltip above: an icon with no name on
        // screen, its tooltip is the only place its agent is named.
        None
    }

    /// **The pointer over the open picker** (D44): the I-beam over its
    /// search field, the one place in it that takes typing, and the arrow
    /// everywhere else — the rows, *Legacy models*, the rail, ↻ and the
    /// footer. A scrollbar drag keeps its resize arrows. `None` when the
    /// picker is closed or the pointer is outside it.
    pub(crate) fn picker_cursor(&self, x: f32, y: f32) -> Option<winit::window::CursorIcon> {
        use winit::window::CursorIcon;
        let geometry = self.picker_geometry()?;
        if self.picker.scroll_drag.is_some() {
            return Some(CursorIcon::NsResize);
        }
        if !geometry.panel.contains(x, y) {
            return None;
        }
        Some(if search_field(&geometry).contains(x, y) {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        })
    }

    /// The wheel over the open picker scrolls its list, a row a notch.
    pub(crate) fn picker_scroll(&mut self, dy: f32) -> bool {
        let previous = self.picker.offset;
        let max = self.picker_max_offset();
        if dy > 0.0 {
            self.picker.offset = self.picker.offset.saturating_sub(dy.ceil() as usize);
        } else if dy < 0.0 {
            self.picker.offset = (self.picker.offset + (-dy).ceil() as usize).min(max);
        }
        previous != self.picker.offset
    }

    /// **The keyboard inside the picker** (§9.5; widget navigation, D12):
    /// typing edits the query wherever the focus is; Up and Down move the
    /// highlighted row, or the rail's entry from the rail; Enter chooses,
    /// and the picker stays open (D66);
    /// Left and Right move the effort strip's level a segment, and Space or
    /// Enter toggles a pill (D76); Tab and Shift+Tab move between search,
    /// rail, list, the strip and the pills; Esc clears the query, else
    /// closes. Starring a model is a click on its ☆ only (D47 taken back). `None` when the picker is closed.
    /// With the rail hidden in a begun chat, neither Tab nor an arrow lands
    /// on it: the keyboard goes from the search to the list.
    pub(crate) fn picker_key(
        &mut self,
        key: &WinitKey,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        if !self.chat_model_open {
            return None;
        }
        let hide_rail = self.picker_rail_hidden();
        // The chat began with the keyboard on the rail: it is on the list.
        if hide_rail && self.picker.focus == PickerFocus::Rail {
            self.picker.focus = PickerFocus::List;
        }
        let levels = self.effort_control().is_some();
        let context = self.picker_context().is_some();
        let fast = self.picker_fast().is_some();
        match key {
            WinitKey::Named(NamedKey::Escape) => {
                if !self.picker.query.raw_text().is_empty() {
                    self.picker.query.set_text(&mut self.engine, "");
                    self.highlight_current_row();
                } else {
                    self.close_picker();
                }
                return Some(true);
            }
            WinitKey::Named(NamedKey::Tab) => {
                let mut order = vec![PickerFocus::Search];
                if !hide_rail {
                    order.push(PickerFocus::Rail);
                }
                order.push(PickerFocus::List);
                if levels {
                    order.push(PickerFocus::Effort);
                }
                if fast {
                    order.push(PickerFocus::Fast);
                }
                if context {
                    order.push(PickerFocus::Context);
                }
                let at = order
                    .iter()
                    .position(|focus| *focus == self.picker.focus)
                    .unwrap_or(0);
                let next = if mods.shift_key() {
                    (at + order.len() - 1) % order.len()
                } else {
                    (at + 1) % order.len()
                };
                self.picker.focus = order[next];
                return Some(true);
            }
            _ => {}
        }
        match self.picker.focus {
            PickerFocus::Rail => match key {
                WinitKey::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                    let rail = self.picker_marks();
                    let at = self
                        .picker_entry()
                        .and_then(|entry| rail.iter().position(|candidate| *candidate == entry));
                    let next = match (at, key) {
                        (Some(at), WinitKey::Named(NamedKey::ArrowUp)) => at.saturating_sub(1),
                        (Some(at), _) => (at + 1).min(rail.len().saturating_sub(1)),
                        (None, _) => 0,
                    };
                    if let Some(entry) = rail.get(next).cloned() {
                        self.select_rail_entry(entry);
                    }
                    return Some(true);
                }
                WinitKey::Named(NamedKey::Enter | NamedKey::ArrowRight) => {
                    self.picker.focus = PickerFocus::List;
                    return Some(true);
                }
                _ => {}
            },
            // Left and Right move the level a segment.
            PickerFocus::Effort => {
                if let WinitKey::Named(direction @ (NamedKey::ArrowLeft | NamedKey::ArrowRight)) =
                    key
                {
                    return Some(self.step_level(*direction == NamedKey::ArrowRight));
                }
            }
            // Space or Enter toggles the pill the keyboard is on.
            PickerFocus::Fast => {
                if matches!(key, WinitKey::Named(NamedKey::Enter | NamedKey::Space)) {
                    return Some(self.toggle_fast());
                }
            }
            PickerFocus::Context => {
                if matches!(key, WinitKey::Named(NamedKey::Enter | NamedKey::Space)) {
                    return Some(self.toggle_context());
                }
            }
            PickerFocus::Search | PickerFocus::List => {}
        }
        match key {
            WinitKey::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                let rows = self.picker_rows();
                let choosable = self.picker_choosable(&rows);
                let step = if matches!(key, WinitKey::Named(NamedKey::ArrowUp)) {
                    -1
                } else {
                    1
                };
                if let Some(next) = step_highlight(&choosable, self.picker.highlight, step) {
                    self.picker.highlight = Some(next);
                    self.scroll_highlight_into_view();
                }
                Some(true)
            }
            WinitKey::Named(NamedKey::Enter) => {
                let index = self.picker.highlight?;
                Some(self.choose_picker_row(index))
            }
            // On *Legacy models*, Right expands it and Left collapses it;
            // in the search, either moves the caret as ever.
            WinitKey::Named(direction @ (NamedKey::ArrowRight | NamedKey::ArrowLeft))
                if (self.picker.focus != PickerFocus::Search
                    || self.picker.query.raw_text().is_empty())
                    && self.picker.highlight.is_some_and(|index| {
                        self.picker_rows().get(index) == Some(&Row::LegacyToggle)
                    }) =>
            {
                let index = self.picker.highlight?;
                Some(self.toggle_legacy(index, *direction == NamedKey::ArrowRight))
            }
            WinitKey::Named(NamedKey::ArrowRight)
                if self.picker.focus != PickerFocus::Search
                    || self.picker.query.raw_text().is_empty() =>
            {
                let before = self.picker.query.raw_text().to_owned();
                let typed =
                    self.picker.query.key(&mut self.engine, key, text, mods) == Typed::Handled;
                Some(typed || before != self.picker.query.raw_text())
            }
            _ => {
                // Typing is the query's, wherever the focus was.
                let before = self.picker.query.raw_text().to_owned();
                let typed =
                    self.picker.query.key(&mut self.engine, key, text, mods) == Typed::Handled;
                if self.picker.query.raw_text() != before {
                    self.picker.focus = PickerFocus::Search;
                    self.picker.offset = 0;
                    let rows = self.picker_rows();
                    let choosable = self.picker_choosable(&rows);
                    self.picker.highlight = step_highlight(&choosable, None, 1);
                }
                Some(typed || self.picker.focus == PickerFocus::Search)
            }
        }
    }

    /// Left or Right on the strip: the chat's level one segment over — the
    /// model's default level is its *Default*, as a press makes it.
    fn step_level(&mut self, up: bool) -> bool {
        if self.effort_locked() {
            self.status = Some(t(self.lang, Key::AiEffortLockedAfterStart).to_owned());
            return true;
        }
        let Some(control) = self.effort_control().cloned() else {
            return false;
        };
        let stops = effort::stops(&control);
        let at = self
            .effective_reasoning()
            .and_then(|choice| stops.iter().position(|stop| *stop == choice));
        let next = match (at, up) {
            (Some(at), true) => (at + 1).min(stops.len().saturating_sub(1)),
            (Some(at), false) => at.saturating_sub(1),
            (None, true) => 0,
            (None, false) => stops.len().saturating_sub(1),
        };
        match stops.get(next) {
            Some(choice) => self.choose_level(*choice),
            None => false,
        }
    }
}

/// Where the rail's dividers fall: after *Favorites* and after the chat's
/// own built-in provider, each while an entry follows it.
pub(crate) fn rail_dividers(rail: &[RailEntry]) -> Vec<usize> {
    rail.iter()
        .enumerate()
        .filter(|(index, entry)| {
            matches!(entry, RailEntry::Favorites | RailEntry::Provider(_)) && index + 1 < rail.len()
        })
        .map(|(index, _)| index)
        .collect()
}

/// The search field itself: the search row short of ↻ at its end.
fn search_field(geometry: &Geometry) -> Rect {
    geometry.field
}

/// Koru AI, OpenAI and Ollama by name, for the rail's tooltip.
fn provider_name(provider: &AiProviderId) -> String {
    match provider.as_str() {
        AiProviderId::KORU => "Koru AI".to_owned(),
        AiProviderId::OPENAI => "OpenAI".to_owned(),
        AiProviderId::OLLAMA => "Ollama".to_owned(),
        other => other.to_owned(),
    }
}

/// **A list row's identity** (M1): its entry and what it chooses — a
/// model, the agent's *Default*, *Legacy models* — never its place, so a
/// row keeps its key as the list scrolls, filters and reorders. `None` for
/// a heading or a note, which choose nothing.
pub(crate) fn row_id(entry: &RailEntry, row: &Row) -> Option<String> {
    let entry_id = |entry: &RailEntry| match entry {
        RailEntry::Favorites => "favorites".to_owned(),
        RailEntry::Provider(provider) => format!("provider:{}", provider.as_str()),
        RailEntry::Agent(agent) => format!("agent:{}", agent.as_str()),
    };
    Some(match row {
        Row::Found(entry, row) => return row_id(entry, row),
        Row::Heading(_) | Row::Note(_) | Row::EntryHeading(_) => return None,
        Row::Default => format!("{}\u{1f}default", entry_id(entry)),
        Row::Model(id) => format!("{}\u{1f}model\u{1f}{id}", entry_id(entry)),
        Row::Favorite(agent, id) => {
            format!("{}\u{1f}{}\u{1f}{id}", entry_id(entry), agent.as_str())
        }
        Row::LegacyToggle => format!("{}\u{1f}legacy", entry_id(entry)),
        Row::ManageAgents => format!("{}\u{1f}manage", entry_id(entry)),
    })
}

/// A provider's mark in `inner`: the agent's brand by its kind
/// (`agent-claude` wears Claude's), OpenAI's for OpenAI, a glyph where there
/// is no brand — [`provider_mark`], which a chat tab wears too. The rail
/// draws it, and so does a favorite's row, at its start.
pub(crate) fn draw_provider_mark(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    provider: &AiProviderId,
    agent: bool,
    inner: Rect,
    colour: vello::peniko::Color,
) {
    let glyph = if agent {
        icons::ROBOT
    } else if provider.as_str() == AiProviderId::OLLAMA {
        icons::DESKTOP
    } else {
        icons::SPARKLE
    };
    match provider_mark(provider.as_str()).unwrap_or(brand::ProfileMark::Glyph(glyph)) {
        brand::ProfileMark::Brand(icon) => {
            scene::draw_brand_icon(canvas, root, inner, colour, icon)
        }
        brand::ProfileMark::Glyph(glyph) => {
            let layout = engine.icon_layout(glyph, inner.h);
            scene::draw_layout(
                canvas,
                root,
                &layout,
                inner.x + (inner.w - layout.width()) * 0.5,
                inner.y + (inner.h - layout.height()) * 0.5,
                Some(colour),
            );
        }
    }
}

#[cfg(test)]
mod tests;
