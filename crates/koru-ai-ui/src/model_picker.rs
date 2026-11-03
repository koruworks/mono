// SPDX-License-Identifier: MPL-2.0

//! **The composer's model picker** (agent-models §9, rung 5): the agents run
//! down a rail, one mark each; above the selected agent's models sits a
//! *Search models…* field with ↻; the footer shows the chat's reasoning
//! level as a word, ↺ back to the model's default, and the stepped slider;
//! above it, for a model with a longer window, the Context row
//! ([`context`]). It opens upward, above the composer and end-aligned with the chip, as the
//! workspace and tool-approval panels do — below the chip only when there is
//! no room above — and it is driven fully from the keyboard.
//!
//! A model row reads its description on a second, smaller line when the
//! agent gives one (D43), and a ☆ at its end stars it (D46): the rail then
//! leads with *Favorites*, the starred models of every agent turned on,
//! each beside its agent's mark. The primary modifier and D stars the row
//! the keyboard is on (D47).
//!
//! The geometry, the rows, the search and the footer's words are pure
//! functions, tested beside this file. The surface keeps the state
//! ([`PickerState`]) and draws.

use std::collections::HashSet;

use koru_ai_core::{AGENT_DEFAULT_MODEL, AiProviderId, ReasoningChoice, ReasoningControl};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{brand, icons, scene, theme};
use koru_i18n::{Key, Lang, t};
use koru_ui::icon_button::{IconButton, State as IconButtonState};
use koru_ui::popover::{PopoverAlign, PopoverShell};
use koru_ui::text_input::{InputStyle, TextInput};
use koru_ui::tooltip::TooltipContent;
use koru_ui::{Menu, MenuItem, Typed, scrollbar};
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, RoundedRectRadii};
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

use crate::agents::{
    AgentModels, AgentStatus, agent_key, default_model_label, fallback_label, find_listed,
    group_label, is_agent_provider, listed_models, model_groups, model_label, provider_mark,
};
use crate::effort;

mod context;
mod fast;
pub(crate) use context::CONTEXT_H;
pub(crate) use fast::FAST_H;

/// The rail's width, and the square of each of its marks.
pub(crate) const RAIL_W: f32 = 44.0;
/// **Air between the rail and the picker's content** — the search row, the
/// list and the footer all start this far right of the rail, so its marks
/// are not flush against them (the composer's toolbar keeps the same 8 px
/// between its controls).
pub(crate) const RAIL_CONTENT_GAP: f32 = 8.0;
const RAIL_BUTTON: f32 = 32.0;
const RAIL_GAP: f32 = 6.0;
/// The room a divider takes after the chat's own built-in provider (S1).
const RAIL_DIVIDER: f32 = 9.0;
/// The search row, with ↻ at its end.
pub(crate) const SEARCH_H: f32 = 40.0;
/// A row of one line: a heading, a note, a model with no description.
pub(crate) const ROW_H: f32 = 30.0;
/// **A model row with its description under its name** (D43): the name,
/// then the description a size smaller and dimmed, one line each — the
/// approval-mode list's two-line row.
pub(crate) const TALL_ROW_H: f32 = 46.0;
/// **The picker's one size** (D13): the search row, six one-line rows of
/// the list and the footer — the box the person chose. The list scrolls
/// inside it when there are more, and a short list leaves room under it
/// rather than shrinking the picker, so the picker keeps its size from
/// agent to agent. A model with no footer gives the room to the list.
///
/// **The box, not the count** (D43): a row that reads its description is
/// taller, so fewer of them fit and the list scrolls by whole rows of
/// either height; the picker does not grow to keep six two-line rows.
pub(crate) const LIST_ROWS: usize = 6;
/// The ☆ at a model row's end (D46): its square, and how far its right edge
/// keeps from the list's, clear of the scrollbar's lane.
const STAR: f32 = 22.0;
const STAR_INSET: f32 = 12.0;
/// A favorite's agent mark at its row's start, and the room it takes.
const ROW_MARK: f32 = 16.0;
const ROW_MARK_ROOM: f32 = ROW_MARK + 8.0;
/// The footer: the level's word and ↺ on one line, the slider under it.
pub(crate) const FOOTER_H: f32 = effort::SECTION_H;
const PAD: f32 = 4.0;
/// The picker is as wide as the composer, up to this.
pub(crate) const MAX_W: f32 = 360.0;
/// The tooltips' keys, so moving from one mark or row to the next is a new
/// tooltip: the rail's first mark, ↻, ↺, the read-only footer. A model row
/// has none (D25).
const TOOLTIP_RAIL: u64 = 0x5049_0000;
const TOOLTIP_REFRESH: u64 = 0x5049_1000;
const TOOLTIP_RESET: u64 = 0x5049_1001;
const TOOLTIP_LOCKED: u64 = 0x5049_1002;
const TOOLTIP_CONTEXT_LOCKED: u64 = 0x5049_1003;

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
/// them in this order, the footer's two only when there is a footer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum PickerFocus {
    Search,
    Rail,
    List,
    /// The Fast row's two choices (koru-ai D11).
    Fast,
    /// The Context row's two choices (koru-ai D10).
    Context,
    Reset,
    Slider,
}

/// The picker's own state on the surface. It lives only while the picker
/// is open; opening resets it.
pub(crate) struct PickerState {
    /// The rail's selected entry: whose models the list shows.
    pub(crate) rail: Option<RailEntry>,
    pub(crate) query: TextInput,
    /// The row the keyboard is on, by index into the rows.
    pub(crate) highlight: Option<usize>,
    /// The first row shown.
    pub(crate) offset: usize,
    pub(crate) scroll_drag: Option<f32>,
    pub(crate) focus: PickerFocus,
    /// Agents whose ↻ is under way: the list reads *Checking…* meanwhile.
    pub(crate) refreshing: HashSet<AiProviderId>,
    /// The *Legacy models ›* flyout, while it is open (D11, rung 6).
    pub(crate) legacy: Option<Menu<LegacyItem>>,
}

/// One older model in the *Legacy models ›* flyout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LegacyItem {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) current: bool,
}

impl MenuItem for LegacyItem {
    fn label(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed(&self.label)
    }

    fn checked(&self) -> Option<bool> {
        Some(self.current)
    }
}

impl PickerState {
    pub(crate) fn new(engine: &mut TextEngine) -> Self {
        Self {
            rail: None,
            query: TextInput::new(engine, theme::role::row()),
            highlight: None,
            offset: 0,
            scroll_drag: None,
            focus: PickerFocus::Search,
            refreshing: HashSet::new(),
            legacy: None,
        }
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
    /// *Legacy models ›*, at the foot: the models the agent marks as older,
    /// in a flyout (D11). Only while not searching; a search shows them
    /// inline instead.
    LegacyMore,
}

impl Row {
    fn choosable(&self) -> bool {
        matches!(
            self,
            Self::Default | Self::Model(_) | Self::Favorite(..) | Self::LegacyMore
        )
    }
}

/// The picker's parts, laid out from its panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Geometry {
    pub(crate) panel: Rect,
    pub(crate) rail: Rect,
    pub(crate) search: Rect,
    pub(crate) refresh: Rect,
    pub(crate) list: Rect,
    /// The level's section: its word, ↺ and the slider.
    pub(crate) footer: Option<Rect>,
    /// The Context row, above the level's section (koru-ai D10).
    pub(crate) context: Option<Rect>,
    /// The Fast row, above the Context row (koru-ai D11).
    pub(crate) fast: Option<Rect>,
}

impl Geometry {
    /// ↺, at the footer line's end.
    pub(crate) fn reset(&self) -> Option<Rect> {
        let footer = self.footer?;
        Some(Rect::new(
            footer.x + footer.w - 28.0,
            footer.y + 2.0,
            24.0,
            24.0,
        ))
    }

    /// The slider's section, which [`effort::slider`] lays its stops on.
    pub(crate) fn slider_section(&self) -> Option<Rect> {
        self.footer
    }
}

/// The picker's width and height: as wide as the composer up to
/// [`MAX_W`]; as tall as the search row, [`LIST_ROWS`] rows, the Context
/// row and the level's footer, whatever the list holds and whether or not
/// the chat's model has either row — never shorter than the rail needs.
pub(crate) fn size(composer_w: f32, rail_h: f32) -> (f32, f32) {
    let width = composer_w.min(MAX_W).max(RAIL_W + 160.0);
    let height = PAD * 2.0 + SEARCH_H + LIST_ROWS as f32 * ROW_H + CONTEXT_H + FOOTER_H;
    (width, height.max(rail_h))
}

/// The picker's parts inside `panel`: the level's footer at the foot when
/// `levels`, the Context row above it when `context`, the Fast row above
/// that when `fast`, and the list in what is left — so a model with both
/// rows gives a list row of the fixed height to the second.
pub(crate) fn geometry(panel: Rect, levels: bool, context: bool, fast: bool) -> Geometry {
    let inner_x = panel.x + RAIL_W + RAIL_CONTENT_GAP;
    let inner_w = (panel.w - RAIL_W - RAIL_CONTENT_GAP - PAD).max(1.0);
    let mut bottom = panel.y + panel.h - PAD;
    let footer_rect = levels.then(|| {
        bottom -= FOOTER_H;
        Rect::new(inner_x, bottom, inner_w, FOOTER_H)
    });
    let context_rect = context.then(|| {
        bottom -= CONTEXT_H;
        Rect::new(inner_x, bottom, inner_w, CONTEXT_H)
    });
    let fast_rect = fast.then(|| {
        bottom -= FAST_H;
        Rect::new(inner_x, bottom, inner_w, FAST_H)
    });
    let search = Rect::new(inner_x, panel.y + PAD + 4.0, inner_w, SEARCH_H - 8.0);
    let refresh = Rect::new(
        search.x + search.w - 28.0,
        search.y + 2.0,
        26.0,
        search.h - 4.0,
    );
    let list_top = panel.y + PAD + SEARCH_H;
    let rows = ((bottom - list_top) / ROW_H).floor().max(0.0);
    Geometry {
        panel,
        rail: Rect::new(panel.x, panel.y, RAIL_W, panel.h),
        search,
        refresh,
        list: Rect::new(inner_x, list_top, inner_w, rows * ROW_H),
        footer: footer_rect,
        context: context_rect,
        fast: fast_rect,
    }
}

/// The rail's marks, top to bottom; a divider follows each mark whose index
/// is in `dividers` — *Favorites*, and the chat's own built-in provider
/// (S1), each set off from the agents after it.
pub(crate) fn rail_rects(rail: Rect, count: usize, dividers: &[usize]) -> Vec<Rect> {
    let x = rail.x + (rail.w - RAIL_BUTTON) * 0.5;
    let mut y = rail.y + PAD + 4.0;
    (0..count)
        .map(|index| {
            let rect = Rect::new(x, y, RAIL_BUTTON, RAIL_BUTTON);
            y += RAIL_BUTTON + RAIL_GAP;
            if dividers.contains(&index) {
                y += RAIL_DIVIDER;
            }
            rect
        })
        .collect()
}

/// The height the rail's marks need, with `dividers` dividers among them.
pub(crate) fn rail_height(count: usize, dividers: usize) -> f32 {
    PAD * 2.0 + 8.0 + count as f32 * (RAIL_BUTTON + RAIL_GAP) + dividers as f32 * RAIL_DIVIDER
}

/// The ☆ of a model row drawn at `row` (D46): a square at its end, in the
/// middle of the row whatever its height.
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

/// **The footer's words** (§9.3): the chat's level in Koru's words, and the
/// dimmed *(default)* after it when the chat is on *Default* and the model
/// states the level that means. A model that states none reads *Default*.
pub(crate) fn footer_words(
    control: &ReasoningControl,
    choice: ReasoningChoice,
    lang: Lang,
) -> (String, Option<String>) {
    let shown = effort::resolve(control, choice).unwrap_or(ReasoningChoice::Default);
    let suffix = (choice == ReasoningChoice::Default && shown != ReasoningChoice::Default)
        .then(|| t(lang, Key::AiEffortIsDefault).to_owned());
    (effort::word(lang, shown), suffix)
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

// **Rows of two heights** (D43). The list still scrolls by whole rows —
// `offset` is the first row shown — and these place them: a row is shown
// only when all of it fits, and the first one is shown whatever its height.

/// Half a pixel of slack, so rows that fit exactly are not lost to rounding.
const FIT: f32 = 0.5;

/// How many of `heights`, from `offset`, fit whole in `room` — at least one
/// while there is one.
pub(crate) fn rows_that_fit(heights: &[f32], offset: usize, room: f32) -> usize {
    let mut used = 0.0;
    let mut count = 0;
    for height in heights.iter().skip(offset) {
        if count > 0 && used + height > room + FIT {
            break;
        }
        used += height;
        count += 1;
    }
    count
}

/// The first row shown so that row `index` is whole in view, starting from
/// `offset`: unchanged when it already is.
pub(crate) fn scroll_into_view(heights: &[f32], index: usize, offset: usize, room: f32) -> usize {
    if index < offset || index >= heights.len() {
        return index.min(offset);
    }
    let mut first = offset;
    while first < index && heights[first..=index].iter().sum::<f32>() > room + FIT {
        first += 1;
    }
    first
}

/// The furthest the list scrolls: the first row of its last page, so the
/// last row sits at the list's foot rather than above empty room.
pub(crate) fn max_offset(heights: &[f32], room: f32) -> usize {
    let mut first = heights.len();
    let mut used = 0.0;
    while first > 0 && used + heights[first - 1] <= room + FIT {
        first -= 1;
        used += heights[first];
    }
    first.min(heights.len().saturating_sub(1))
}

/// The top of row `index`, from the list's first row.
pub(crate) fn row_top(heights: &[f32], index: usize) -> f32 {
    heights.iter().take(index).sum()
}

/// The row whose top is nearest `scroll` pixels down the list, no further
/// than `max` — what a scrollbar drag lands on.
pub(crate) fn offset_at(heights: &[f32], scroll: f32, max: usize) -> usize {
    let (mut best, mut best_top) = (0, 0.0_f32);
    let mut top = 0.0_f32;
    for (index, height) in heights.iter().enumerate().take(max + 1) {
        if (top - scroll).abs() < (best_top - scroll).abs() {
            (best, best_top) = (index, top);
        }
        top += height;
    }
    best
}

/// The row at `dy` pixels below the top of the list scrolled to `offset`.
pub(crate) fn row_at(heights: &[f32], offset: usize, dy: f32) -> Option<usize> {
    if dy < 0.0 {
        return None;
    }
    let mut top = 0.0;
    for (index, height) in heights.iter().enumerate().skip(offset) {
        if dy < top + height {
            return Some(index);
        }
        top += height;
    }
    None
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

    /// The **+** at the rail's end, while the host offers it: one more
    /// square after the last mark.
    pub(crate) fn picker_manage_rect(&self, geometry: &Geometry) -> Option<Rect> {
        if !self.agent_settings_offered {
            return None;
        }
        let rail = self.picker_rail();
        rail_rects(geometry.rail, rail.len() + 1, &rail_dividers(&rail))
            .last()
            .copied()
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
        let room = self
            .picker_geometry()
            .map_or(LIST_ROWS as f32 * ROW_H, |geometry| geometry.list.h);
        let heights = self.picker_row_heights(&rows);
        self.picker.offset = self.picker.offset.min(max_offset(&heights, room));
        self.scroll_highlight_into_view();
    }

    /// What the **+** is called for a screen reader and the inspector — it
    /// raises no tooltip (D25: the picker's rows show none; the rail's marks
    /// keep theirs because they are named nowhere else, and this one is).
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
        rail
    }

    /// **The rail's entry on open** (§9.1): the chat's own provider; else
    /// the default agent from settings; else the first agent that is ready;
    /// else the first.
    pub(crate) fn picker_initial_entry(&self) -> Option<RailEntry> {
        let rail = self.picker_rail();
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
        // *Favorites* is never where the picker opens: it opens on whose
        // models the chat is on.
        let providers = || {
            rail.iter()
                .filter(|entry| !matches!(entry, RailEntry::Favorites))
        };
        providers()
            .find(|entry| entry.provider() == current.as_ref())
            .or_else(|| {
                providers().find(|entry| {
                    matches!(entry, RailEntry::Agent(_)) && entry.provider() == selected.as_ref()
                })
            })
            .or_else(|| providers().find(ready))
            .or_else(|| providers().next())
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
    /// has no message and the agent is ready; the chat's own built-in
    /// provider's models always, as today. *Favorites* answers row by row,
    /// by each favorite's agent.
    fn entry_choosable(&self, entry: &RailEntry) -> bool {
        match entry {
            RailEntry::Favorites | RailEntry::Provider(_) => true,
            RailEntry::Agent(agent) => {
                self.agent_status.get(agent).copied() == Some(AgentStatus::Ready)
                    && self
                        .transcript
                        .as_ref()
                        .is_none_or(|transcript| transcript.messages.is_empty())
            }
        }
    }

    /// Whether `agent` has a model the query finds, for the rail's dimming
    /// while typing (S3).
    fn agent_matches_query(&self, agent: &AiProviderId, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let models = self.listed_agent_models(agent);
        let default = default_model_label(models, self.lang);
        matches(query, &[&default])
            || listed_models(models).any(|model| {
                let group = group_label(models, &model.group);
                matches(
                    query,
                    &[
                        &model_label(model),
                        &model.id,
                        &group,
                        model.description.as_deref().unwrap_or_default(),
                    ],
                )
            })
    }

    /// Whether the query finds a favorite: in its model's name, id, group
    /// and description, or its agent's name.
    fn favorite_matches(&self, agent: &AiProviderId, id: &str, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let agents = self
            .settings
            .as_ref()
            .map(|settings| settings.agents.as_slice())
            .unwrap_or_default();
        let name = crate::agents::agent_label(agents, agent);
        let models = self.listed_agent_models(agent);
        match models.iter().find(|model| model.id == id) {
            Some(model) => matches(
                query,
                &[
                    &model_label(model),
                    &model.id,
                    &group_label(models, &model.group),
                    model.description.as_deref().unwrap_or_default(),
                    &name,
                ],
            ),
            None => matches(query, &[id, &name]),
        }
    }

    /// **The list for the selected entry** (§9.2): the agent's *Default*,
    /// then its models — by group with a heading when it has more than one
    /// — the query applied; or the one word its state calls for.
    pub(crate) fn picker_rows(&self) -> Vec<Row> {
        let Some(entry) = self.picker_entry() else {
            return Vec::new();
        };
        let query = self.picker_query();
        let note = |key: Key| Row::Note(t(self.lang, key).to_owned());
        match &entry {
            // Every agent's starred models, each found by its own words
            // and by its agent's name.
            RailEntry::Favorites => {
                let rows: Vec<Row> = self
                    .picker_favorites()
                    .into_iter()
                    .filter(|(agent, id)| self.favorite_matches(agent, id, &query))
                    .map(|(agent, id)| Row::Favorite(agent, id))
                    .collect();
                if rows.is_empty() {
                    vec![note(Key::AiModelsNoMatch)]
                } else {
                    rows
                }
            }
            RailEntry::Provider(provider) => {
                let models = if provider.as_str() == AiProviderId::KORU {
                    vec![AiProviderId::KORU_HOSTED_MODEL.to_owned()]
                } else {
                    self.chat_model_options()
                };
                let rows: Vec<Row> = models
                    .into_iter()
                    .filter(|model| matches(&query, &[model, &self.provider_model_label(model)]))
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
                            if matches(&query, &[&default_model_label(&[], self.lang)]) {
                                rows.push(Row::Default);
                            }
                            rows
                        }
                        Some(AgentModels::Listed(_)) => self.agent_model_rows(agent, &query),
                    },
                    // A list kept from the last run, dimmed, under *Checking…*.
                    AgentStatus::Checking if known => {
                        let mut rows = vec![note(Key::TerminalChecking)];
                        rows.extend(
                            self.agent_model_rows(agent, &query)
                                .into_iter()
                                .filter(|row| !matches!(row, Row::Note(_))),
                        );
                        rows
                    }
                    other => {
                        vec![Row::Note(other.word(self.lang).unwrap_or_else(|| {
                            t(self.lang, Key::TerminalChecking).to_owned()
                        }))]
                    }
                }
            }
        }
    }

    /// An agent's listed models as rows, the query applied.
    fn agent_model_rows(&self, agent: &AiProviderId, query: &str) -> Vec<Row> {
        let models = self.listed_agent_models(agent);
        let mut rows = Vec::new();
        let default = models.iter().find(|model| model.id == AGENT_DEFAULT_MODEL);
        if matches(
            query,
            &[
                &default_model_label(models, self.lang),
                default
                    .and_then(|model| model.description.as_deref())
                    .unwrap_or_default(),
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
        // The models the agent marks as older (D11): behind one row at the
        // foot, or — while searching — inline after the others, tagged.
        let legacy: Vec<_> = listed_models(models).filter(|m| m.legacy).collect();
        if query.is_empty() {
            if !legacy.is_empty() {
                rows.push(Row::LegacyMore);
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
            (RailEntry::Agent(agent), Row::Model(id)) => self
                .listed_agent_models(agent)
                .iter()
                .any(|model| &model.id == id && model.legacy),
            _ => false,
        }
    }

    /// **Open *Legacy models ›*** beside its row (rung 6): a flyout of the
    /// agent's older models, to the row's right when it fits and its left
    /// otherwise (`Menu::as_submenu`).
    pub(crate) fn open_legacy_menu(&mut self, index: usize) -> bool {
        let (Some(geometry), Some(RailEntry::Agent(agent))) =
            (self.picker_geometry(), self.picker_entry())
        else {
            return false;
        };
        let Some(parent) = self.picker_row_rect(&geometry, index) else {
            return false;
        };
        let current = self
            .transcript
            .as_ref()
            .filter(|transcript| transcript.chat.current_model.provider == agent)
            .map(|transcript| transcript.chat.current_model.requested_model.clone());
        let models = self.listed_agent_models(&agent);
        let current = current
            .as_deref()
            .and_then(|current| find_listed(models, current))
            .map(|(listed, _)| listed.id.clone());
        let items: Vec<LegacyItem> = listed_models(models)
            .filter(|model| model.legacy)
            .map(|model| LegacyItem {
                id: model.id.clone(),
                label: model_label(model),
                current: current.as_deref() == Some(model.id.as_str()),
            })
            .collect();
        if items.is_empty() {
            return false;
        }
        let viewport = self.menu_viewport();
        let mut menu = Menu::as_submenu(&mut self.engine, parent, items, viewport);
        menu.hover = Some(0);
        self.picker.legacy = Some(menu);
        true
    }

    /// Choose the flyout's item `item`: the older model is the chat's.
    fn choose_legacy_item(&mut self, item: usize) -> bool {
        let Some(RailEntry::Agent(agent)) = self.picker_entry() else {
            return true;
        };
        let Some(id) = self
            .picker
            .legacy
            .as_ref()
            .and_then(|menu| menu.items().get(item))
            .map(|item| item.id.clone())
        else {
            return true;
        };
        let can = self.entry_choosable(&RailEntry::Agent(agent.clone()));
        self.close_picker();
        if can {
            self.choose_agent_model(agent, id)
        } else {
            true
        }
    }

    /// A press while the flyout is open: an item is chosen; anywhere else
    /// closes the flyout and is the picker's as usual. `None` when it is not
    /// open or the press is not on it.
    fn legacy_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        let viewport = self.menu_viewport();
        let menu = self.picker.legacy.as_ref()?;
        if menu.panel(viewport).contains(x, y) {
            return Some(match menu.item_at(viewport, x, y) {
                Some(item) => self.choose_legacy_item(item),
                None => true,
            });
        }
        self.picker.legacy = None;
        None
    }

    /// Draw the open flyout, over the picker.
    pub(crate) fn draw_legacy_menu(&mut self, canvas: &mut Scene, root: Affine) {
        let viewport = self.menu_viewport();
        if let Some(menu) = &self.picker.legacy {
            let enabled = vec![true; menu.items().len()];
            menu.draw(canvas, root, &mut self.engine, viewport, &enabled);
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
    /// rows could be.
    fn picker_choosable(&self, rows: &[Row]) -> Vec<bool> {
        let entry_choosable = self
            .picker_entry()
            .is_some_and(|entry| self.entry_choosable(&entry));
        rows.iter()
            .map(|row| match row {
                Row::Favorite(agent, _) => self.entry_choosable(&RailEntry::Agent(agent.clone())),
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
            (_, Row::Heading(text) | Row::Note(text)) => text.clone(),
            (RailEntry::Provider(_), Row::Model(id)) => self.provider_model_label(id),
            (RailEntry::Provider(_) | RailEntry::Favorites, Row::Default | Row::Model(_)) => {
                String::new()
            }
            (_, Row::LegacyMore) => t(self.lang, Key::AiModelsLegacy).to_owned(),
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

    /// **A row's second line** (D43): the description its agent gives the
    /// model — the agent's *Default* reads what it stands for. `None` for
    /// every other row, and for a model with no description, which stays
    /// one line.
    pub(crate) fn picker_row_description(&self, entry: &RailEntry, row: &Row) -> Option<String> {
        let (agent, id) = match (entry, row) {
            (RailEntry::Agent(agent), Row::Default) => (agent, AGENT_DEFAULT_MODEL),
            (RailEntry::Agent(agent), Row::Model(id)) | (_, Row::Favorite(agent, id)) => {
                (agent, id.as_str())
            }
            _ => return None,
        };
        self.listed_agent_models(agent)
            .iter()
            .find(|model| model.id == id)
            .and_then(|model| model.description.as_deref())
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(str::to_owned)
    }

    /// Each row's height: two lines for a row with a description, one
    /// otherwise.
    pub(crate) fn picker_row_heights(&self, rows: &[Row]) -> Vec<f32> {
        let entry = self.picker_entry();
        rows.iter()
            .map(|row| {
                let tall = entry
                    .as_ref()
                    .is_some_and(|entry| self.picker_row_description(entry, row).is_some());
                if tall { TALL_ROW_H } else { ROW_H }
            })
            .collect()
    }

    /// Row `index` where it is drawn, while it is in view.
    pub(crate) fn picker_row_rect(&self, geometry: &Geometry, index: usize) -> Option<Rect> {
        let heights = self.picker_row_heights(&self.picker_rows());
        let offset = self.picker.offset;
        let shown = rows_that_fit(&heights, offset, geometry.list.h);
        if index < offset || index >= offset + shown {
            return None;
        }
        let top = row_top(&heights, index) - row_top(&heights, offset);
        Some(Rect::new(
            geometry.list.x,
            geometry.list.y + top,
            geometry.list.w,
            heights[index],
        ))
    }

    /// The row drawn at `y`, if any.
    fn picker_row_at(&self, geometry: &Geometry, y: f32) -> Option<usize> {
        let heights = self.picker_row_heights(&self.picker_rows());
        let offset = self.picker.offset;
        let index = row_at(&heights, offset, y - geometry.list.y)?;
        (index < offset + rows_that_fit(&heights, offset, geometry.list.h)).then_some(index)
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

    /// The picker's parts, while it is open: **above the composer**, end
    /// aligned with the chip, where the workspace and tool-approval panels
    /// open; anchored below the chip only when there is no room above.
    pub(crate) fn picker_geometry(&self) -> Option<Geometry> {
        if !self.chat_model_open {
            return None;
        }
        let rail = self.picker_rail();
        let marks = rail.len() + usize::from(self.agent_settings_offered);
        let rail_h = rail_height(marks, rail_dividers(&rail).len());
        let levels = self.effort_control().is_some();
        let context = self.picker_context().is_some();
        let fast = self.picker_fast().is_some();
        let (w, h) = size(self.composer_hit_rect.w, rail_h);
        let chip = self.chat_model_button();
        let panel = if self.fits_above_composer(h) {
            self.above_composer(chip.x + chip.w - w, w, h)
        } else {
            PopoverShell::anchored(chip, self.viewport, w, h, PopoverAlign::End).rect
        };
        Some(geometry(panel, levels, context, fast))
    }

    /// The list's scrollbar. Its content is measured to the top of the last
    /// page plus the list, so the thumb meets the track's foot exactly when
    /// the list can scroll no further.
    pub(crate) fn picker_scrollbar(&self, geometry: &Geometry) -> Option<scrollbar::ScrollbarGeom> {
        let heights = self.picker_row_heights(&self.picker_rows());
        let last_page = max_offset(&heights, geometry.list.h);
        scrollbar::ScrollbarGeom::for_content(
            geometry.list,
            row_top(&heights, last_page) + geometry.list.h,
            row_top(&heights, self.picker.offset),
        )
    }

    /// Where a scrollbar drag to `scroll` pixels puts the list's first row.
    fn picker_offset_at(&self, geometry: &Geometry, scroll: f32) -> usize {
        let heights = self.picker_row_heights(&self.picker_rows());
        offset_at(&heights, scroll, max_offset(&heights, geometry.list.h))
    }

    /// **Open the picker** on the chat's own entry, the query empty, the
    /// search field focused and the current model in view.
    pub(crate) fn reset_picker(&mut self) {
        self.picker.legacy = None;
        self.picker.rail = self.picker_initial_entry();
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
        let room = self
            .picker_geometry()
            .map_or(LIST_ROWS as f32 * ROW_H, |geometry| geometry.list.h);
        let heights = self.picker_row_heights(&self.picker_rows());
        self.picker.offset = scroll_into_view(&heights, index, self.picker.offset, room);
    }

    fn select_rail_entry(&mut self, entry: RailEntry) {
        self.picker.legacy = None;
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
    /// chat's next model. The picker closes.
    pub(crate) fn choose_picker_row(&mut self, index: usize) -> bool {
        let rows = self.picker_rows();
        let choosable = self.picker_choosable(&rows);
        let (Some(entry), Some(row)) = (self.picker_entry(), rows.get(index)) else {
            return true;
        };
        if !choosable.get(index).copied().unwrap_or(false) {
            return true;
        }
        if *row == Row::LegacyMore {
            return self.open_legacy_menu(index);
        }
        let row = row.clone();
        self.close_picker();
        match (entry, row) {
            (RailEntry::Agent(agent), Row::Default) => {
                self.choose_agent_model(agent, AGENT_DEFAULT_MODEL.to_owned())
            }
            (RailEntry::Agent(agent), Row::Model(id)) | (_, Row::Favorite(agent, id)) => {
                self.choose_agent_model(agent, id)
            }
            (RailEntry::Provider(_), Row::Model(id)) => self.change_chat_model(id),
            _ => true,
        }
    }

    /// **Star the row the keyboard is on** (D47), or take its star away;
    /// `false` when that row has no ☆.
    fn toggle_highlighted_favorite(&mut self) -> bool {
        let (Some(entry), Some(index)) = (self.picker_entry(), self.picker.highlight) else {
            return false;
        };
        let rows = self.picker_rows();
        let Some((agent, model)) = rows
            .get(index)
            .and_then(|row| self.row_favorite(&entry, row))
        else {
            return false;
        };
        self.toggle_favorite(&agent, &model)
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
        let geometry = self.picker_geometry()?;
        if let Some(handled) = self.legacy_mouse_down(x, y) {
            return Some(handled);
        }
        if !geometry.panel.contains(x, y) {
            if self.chat_model_button().contains(x, y) {
                return None;
            }
            self.close_picker();
            return Some(true);
        }
        if let Some(scrollbar) = self.picker_scrollbar(&geometry)
            && scrollbar.in_gutter(x, y)
        {
            let grab = if scrollbar.on_thumb(x, y) {
                y - scrollbar.thumb_top
            } else {
                scrollbar.thumb_h * 0.5
            };
            self.picker.scroll_drag = Some(grab);
            let scroll = scrollbar.scroll_for_pointer(y, grab);
            self.picker.offset = self.picker_offset_at(&geometry, scroll);
            return Some(true);
        }
        let rail = self.picker_rail();
        if let Some(index) = rail_rects(geometry.rail, rail.len(), &rail_dividers(&rail))
            .iter()
            .position(|rect| rect.contains(x, y))
        {
            self.picker.focus = PickerFocus::Rail;
            self.select_rail_entry(rail[index].clone());
            return Some(true);
        }
        if self
            .picker_manage_rect(&geometry)
            .is_some_and(|rect| rect.contains(x, y))
        {
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
        if let Some(reset) = geometry.reset()
            && reset.contains(x, y)
        {
            self.picker.focus = PickerFocus::Reset;
            return Some(self.reset_level());
        }
        if let Some(handled) = self.context_mouse_down(&geometry, x, y) {
            return Some(handled);
        }
        if let Some(handled) = self.fast_mouse_down(&geometry, x, y) {
            return Some(handled);
        }
        if let (Some(section), Some(control)) =
            (geometry.slider_section(), self.effort_control().cloned())
            && let Some(choice) = effort::stop_at(&effort::slider(section, &control), x, y)
        {
            self.picker.focus = PickerFocus::Slider;
            if self.effort_locked() {
                self.status = Some(t(self.lang, Key::AiEffortLockedAfterStart).to_owned());
                return Some(true);
            }
            self.effort_drag = Some(choice);
            return Some(true);
        }
        if geometry.list.contains(x, y) {
            self.picker.focus = PickerFocus::List;
            if let Some(index) = self.picker_row_at(&geometry, y) {
                // The ☆ stars the row rather than choosing it (D46), and
                // whether or not the row can be chosen now.
                if let (Some(entry), Some(rect)) =
                    (self.picker_entry(), self.picker_row_rect(&geometry, index))
                    && star_rect(rect).contains(x, y)
                    && let Some((agent, model)) = self
                        .picker_rows()
                        .get(index)
                        .and_then(|row| self.row_favorite(&entry, row))
                {
                    self.picker.highlight = Some(index);
                    return Some(self.toggle_favorite(&agent, &model));
                }
                return Some(self.choose_picker_row(index));
            }
        }
        Some(true)
    }

    /// ↺: back to the model's own default (*Default*, which sends nothing).
    fn reset_level(&mut self) -> bool {
        if self.effort_locked() {
            self.status = Some(t(self.lang, Key::AiEffortLockedAfterStart).to_owned());
            return true;
        }
        let at_default = self.transcript.as_ref().is_none_or(|transcript| {
            transcript.chat.current_model.default_reasoning == ReasoningChoice::Default
        });
        if at_default {
            return true;
        }
        self.choose_effort(ReasoningChoice::Default)
    }

    /// The pointer moved with the picker open: a scrollbar drag, and the
    /// tooltip under it. `Some` when the picker claimed the move.
    pub(crate) fn picker_mouse_move(&mut self, x: f32, y: f32) -> Option<bool> {
        let geometry = self.picker_geometry()?;
        let viewport = self.menu_viewport();
        if let Some(menu) = &mut self.picker.legacy
            && menu.panel(viewport).contains(x, y)
        {
            let next = menu.item_at(viewport, x, y);
            let changed = menu.hover != next;
            menu.hover = next.or(menu.hover);
            return Some(changed);
        }
        if let Some(grab) = self.picker.scroll_drag {
            if let Some(scrollbar) = self.picker_scrollbar(&geometry) {
                let next = self.picker_offset_at(&geometry, scrollbar.scroll_for_pointer(y, grab));
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
        let rail = self.picker_rail();
        let agents = self
            .settings
            .as_ref()
            .map(|settings| settings.agents.as_slice())
            .unwrap_or_default();
        for (index, rect) in rail_rects(geometry.rail, rail.len(), &rail_dividers(&rail))
            .into_iter()
            .enumerate()
        {
            if rect.contains(x, y) {
                let entry = &rail[index];
                let name = match entry {
                    RailEntry::Favorites => t(self.lang, Key::AiPickerFavorites).to_owned(),
                    RailEntry::Agent(agent) => crate::agents::agent_label(agents, agent),
                    RailEntry::Provider(provider) => provider_name(provider),
                };
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
        if let Some(reset) = geometry.reset()
            && reset.contains(x, y)
        {
            return Some(TooltipContent::new(
                TOOLTIP_RESET,
                reset,
                t(self.lang, Key::AiEffortReset),
            ));
        }
        if let Some(context) = geometry.context
            && context.contains(x, y)
            && self.effort_locked()
        {
            return Some(TooltipContent::new(
                TOOLTIP_CONTEXT_LOCKED,
                context,
                t(self.lang, Key::AiModelContextLockedAfterStart),
            ));
        }
        if let Some(footer) = geometry.footer
            && footer.contains(x, y)
            && self.effort_locked()
        {
            return Some(TooltipContent::new(
                TOOLTIP_LOCKED,
                footer,
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
    /// everywhere else — the rows, the rail, ↻, the footer and the
    /// *Legacy models ›* flyout. A scrollbar drag keeps its resize arrows.
    /// `None` when the picker is closed or the pointer is outside it.
    pub(crate) fn picker_cursor(&self, x: f32, y: f32) -> Option<winit::window::CursorIcon> {
        use winit::window::CursorIcon;
        let geometry = self.picker_geometry()?;
        if self.picker.scroll_drag.is_some() {
            return Some(CursorIcon::NsResize);
        }
        if self
            .picker
            .legacy
            .as_ref()
            .is_some_and(|menu| menu.panel(self.menu_viewport()).contains(x, y))
        {
            return Some(CursorIcon::Default);
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
        let room = self
            .picker_geometry()
            .map_or(LIST_ROWS as f32 * ROW_H, |geometry| geometry.list.h);
        let max = max_offset(&self.picker_row_heights(&self.picker_rows()), room);
        if dy > 0.0 {
            self.picker.offset = self.picker.offset.saturating_sub(dy.ceil() as usize);
        } else if dy < 0.0 {
            self.picker.offset = (self.picker.offset + (-dy).ceil() as usize).min(max);
        }
        previous != self.picker.offset
    }

    /// **The keyboard inside the picker** (§9.5; widget navigation, D12):
    /// typing edits the query wherever the focus is; Up and Down move the
    /// highlighted row, or the rail's entry from the rail; Enter chooses;
    /// Left and Right move the slider a stop; Tab and Shift+Tab move between
    /// search, rail, list, ↺ and the slider; the primary modifier and D
    /// stars the highlighted model (D47); Esc clears the query, else
    /// closes. `None` when the picker is closed.
    pub(crate) fn picker_key(
        &mut self,
        key: &WinitKey,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        if !self.chat_model_open {
            return None;
        }
        let footer = self.effort_control().is_some();
        let context = self.picker_context().is_some();
        let fast = self.picker_fast().is_some();
        // The flyout first: Up and Down walk it, Enter chooses, Left and
        // Esc close it.
        if let Some(menu) = &mut self.picker.legacy {
            let count = menu.items().len();
            match key {
                WinitKey::Named(NamedKey::ArrowUp) => {
                    menu.hover = Some(menu.hover.unwrap_or(0).saturating_sub(1));
                    return Some(true);
                }
                WinitKey::Named(NamedKey::ArrowDown) => {
                    menu.hover = Some((menu.hover.map_or(0, |at| at + 1)).min(count - 1));
                    return Some(true);
                }
                WinitKey::Named(NamedKey::Enter) => {
                    let item = menu.hover.unwrap_or(0);
                    return Some(self.choose_legacy_item(item));
                }
                WinitKey::Named(NamedKey::ArrowLeft | NamedKey::Escape) => {
                    self.picker.legacy = None;
                    return Some(true);
                }
                _ => {}
            }
        }
        // **The primary modifier and D stars the row the keyboard is on**
        // (D47; `koru_keys`' `FavoriteModel`), from the search field or the
        // list — before the query, which would otherwise take the key.
        if matches!(self.picker.focus, PickerFocus::Search | PickerFocus::List)
            && koru_keys::registry::get(koru_keys::ShortcutId::FavoriteModel)
                .chord
                .matches(key, text, mods)
        {
            self.toggle_highlighted_favorite();
            return Some(true);
        }
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
                let mut order = vec![PickerFocus::Search, PickerFocus::Rail, PickerFocus::List];
                if fast {
                    order.push(PickerFocus::Fast);
                }
                if context {
                    order.push(PickerFocus::Context);
                }
                if footer {
                    order.extend([PickerFocus::Reset, PickerFocus::Slider]);
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
                    let rail = self.picker_rail();
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
            PickerFocus::Reset => {
                if matches!(key, WinitKey::Named(NamedKey::Enter | NamedKey::Space)) {
                    return Some(self.reset_level());
                }
            }
            PickerFocus::Slider => {
                if let WinitKey::Named(direction @ (NamedKey::ArrowLeft | NamedKey::ArrowRight)) =
                    key
                {
                    return Some(self.step_level(*direction == NamedKey::ArrowRight));
                }
            }
            // Left is Off, Right is On.
            PickerFocus::Fast => {
                if let WinitKey::Named(direction @ (NamedKey::ArrowLeft | NamedKey::ArrowRight)) =
                    key
                {
                    return Some(self.choose_fast(*direction == NamedKey::ArrowRight));
                }
            }
            // Left is Auto, Right the longer window.
            PickerFocus::Context => {
                if let WinitKey::Named(direction @ (NamedKey::ArrowLeft | NamedKey::ArrowRight)) =
                    key
                {
                    return Some(self.choose_context(*direction == NamedKey::ArrowRight));
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
            WinitKey::Named(NamedKey::ArrowRight)
                if self.picker.focus != PickerFocus::Search
                    || self.picker.query.raw_text().is_empty() =>
            {
                let index = self.picker.highlight?;
                if self.picker_rows().get(index) == Some(&Row::LegacyMore) {
                    return Some(self.open_legacy_menu(index));
                }
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

    /// Left or Right on the slider: the chat's level one stop over.
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
            Some(choice) => self.choose_effort(*choice),
            None => false,
        }
    }

    /// Draw the open picker: the panel, the rail, the search with ↻, the
    /// list, and the footer.
    pub(crate) fn draw_model_picker(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(geometry) = self.picker_geometry() else {
            return;
        };
        let p = theme::p();
        let radius = theme::overlay::popover().radius;
        PopoverShell {
            rect: geometry.panel,
            radius,
        }
        .draw(canvas, root);

        // The rail, on the window rail's ground (D45): inside the shell's
        // hairline, rounded where the panel is.
        let rail_rect = geometry.rail;
        let inset = 1.0;
        let corner = f64::from((radius - inset).max(0.0));
        scene::fill(
            canvas,
            root,
            rail_fill(),
            &RoundedRect::new(
                f64::from(rail_rect.x + inset),
                f64::from(rail_rect.y + inset),
                f64::from(rail_rect.x + rail_rect.w),
                f64::from(rail_rect.y + rail_rect.h - inset),
                RoundedRectRadii::new(corner, 0.0, 0.0, corner),
            ),
        );
        let rail = self.picker_rail();
        let dividers = rail_dividers(&rail);
        let selected = self.picker_entry();
        let query = self.picker_query();
        let rects = rail_rects(geometry.rail, rail.len(), &dividers);
        for (entry, rect) in rail.iter().zip(&rects) {
            let chosen = selected.as_ref() == Some(entry);
            let hovered = rect.contains(self.pointer.0, self.pointer.1);
            let keyboard = chosen && self.picker.focus == PickerFocus::Rail;
            if chosen || hovered {
                scene::fill(
                    canvas,
                    root,
                    if chosen { p.row_selected } else { p.hover_tint },
                    &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
                );
            }
            if keyboard {
                scene::stroke(
                    canvas,
                    root,
                    p.accent,
                    1.0,
                    &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
                );
            }
            let dim = match entry {
                RailEntry::Agent(agent) => {
                    self.agent_status.get(agent).copied() != Some(AgentStatus::Ready)
                        || !self.agent_matches_query(agent, &query)
                }
                RailEntry::Favorites | RailEntry::Provider(_) => false,
            };
            let colour = if dim { p.text_dim } else { p.text };
            draw_mark(canvas, root, &mut self.engine, entry, *rect, colour);
        }
        if let Some(rect) = self.picker_manage_rect(&geometry) {
            let hovered = rect.contains(self.pointer.0, self.pointer.1);
            let _manage = koru_draw::inspect::scope(
                self.picker_manage_name(),
                "button",
                root,
                scene::rect(rect.x, rect.y, rect.w, rect.h),
                |r| r.property("accessible name", self.picker_manage_name(), "caller label"),
            );
            if hovered {
                scene::fill(
                    canvas,
                    root,
                    p.hover_tint,
                    &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
                );
            }
            let layout = self.engine.icon_layout(icons::PLUS, 16.0);
            scene::draw_layout(
                canvas,
                root,
                &layout,
                rect.x + (rect.w - layout.width()) * 0.5,
                rect.y + (rect.h - layout.height()) * 0.5,
                Some(if hovered { p.text } else { p.text_dim }),
            );
        }
        for after in dividers.iter().filter_map(|index| rects.get(*index)) {
            let y = after.y + after.h + RAIL_GAP + RAIL_DIVIDER * 0.5 - 0.5;
            scene::fill(
                canvas,
                root,
                p.panel_border,
                &scene::rrect(after.x + 4.0, y, after.w - 8.0, 1.0, 0.5),
            );
        }

        // The search field and ↻.
        let search = search_field(&geometry);
        let placeholder = t(self.lang, Key::AiModelSearch).to_owned();
        self.picker.query.draw(
            canvas,
            root,
            &mut self.engine,
            search,
            InputStyle::search(),
            Some(&placeholder),
            self.picker.focus == PickerFocus::Search,
        );
        if let Some(RailEntry::Agent(agent)) = &selected {
            let disabled = !self.refresh_enabled(agent);
            IconButton::new(icons::ARROWS_CLOCKWISE, geometry.refresh)
                .with_state(IconButtonState {
                    hovered: geometry.refresh.contains(self.pointer.0, self.pointer.1),
                    active: false,
                    disabled,
                })
                .draw(canvas, root, &mut self.engine);
        }

        // The list.
        let rows = self.picker_rows();
        let choosable = self.picker_choosable(&rows);
        let heights = self.picker_row_heights(&rows);
        let visible = rows_that_fit(&heights, self.picker.offset, geometry.list.h);
        if let Some(entry) = &selected {
            let mut top = geometry.list.y;
            for (index, row) in rows
                .iter()
                .enumerate()
                .skip(self.picker.offset)
                .take(visible)
            {
                let rect = Rect::new(geometry.list.x, top, geometry.list.w, heights[index]);
                top += rect.h;
                let description = self.picker_row_description(entry, row);
                let current = self.picker_row_current(entry, row);
                let can = choosable[index];
                let highlighted = self.picker.highlight == Some(index)
                    && matches!(self.picker.focus, PickerFocus::Search | PickerFocus::List);
                let hovered = can && rect.contains(self.pointer.0, self.pointer.1);
                if current || highlighted || hovered {
                    scene::fill(
                        canvas,
                        root,
                        if current {
                            p.row_selected
                        } else {
                            p.hover_tint
                        },
                        &scene::rrect(rect.x, rect.y, rect.w, rect.h, 4.0),
                    );
                }
                let label = self.picker_row_label(entry, row);
                let (style, colour, indent) = match row {
                    Row::Heading(_) => (theme::role::meta(), p.text_dim, 8.0),
                    Row::Note(_) => (theme::role::meta(), p.text_dim, 8.0),
                    _ => (
                        theme::role::row(),
                        if can || current { p.text } else { p.text_dim },
                        // A favorite's agent mark comes first (D46).
                        if matches!(row, Row::Favorite(..)) {
                            8.0 + ROW_MARK_ROOM
                        } else {
                            8.0
                        },
                    ),
                };
                // What sits at the row's end, from the end: the ☆ of a
                // model that can be starred (D46), the current one's check,
                // the flyout's caret, an older model's tag while searching.
                // The ☆'s room is kept whether or not it shows, so a name
                // does not shorten under the pointer.
                let star = self
                    .row_favorite(entry, row)
                    .map(|(agent, model)| (star_rect(rect), self.is_favorite(&agent, &model)));
                let legacy_tag = self
                    .row_is_legacy(entry, row)
                    .then(|| t(self.lang, Key::AiModelsLegacyTag).to_owned());
                let mut end = star.map_or(rect.x + rect.w - 8.0, |(star, _)| star.x - 2.0);
                if let Some((star, starred)) = star {
                    let under = rect.contains(self.pointer.0, self.pointer.1);
                    if starred || under {
                        self.draw_star(canvas, root, star, starred);
                    }
                }
                if current {
                    let check = self.engine.icon_layout(icons::CHECK, 13.0);
                    end -= check.width();
                    scene::draw_layout(
                        canvas,
                        root,
                        &check,
                        end,
                        rect.y + (rect.h - check.height()) * 0.5,
                        Some(p.text_dim),
                    );
                    end -= 9.0;
                }
                if *row == Row::LegacyMore {
                    let caret = self.engine.icon_layout(icons::CARET_RIGHT, 12.0);
                    end -= caret.width();
                    scene::draw_layout(
                        canvas,
                        root,
                        &caret,
                        end,
                        rect.y + (rect.h - caret.height()) * 0.5,
                        Some(p.text_dim),
                    );
                    end -= 8.0;
                }
                if let Some(tag) = &legacy_tag {
                    let tag = self.engine.simple_layout(tag, theme::role::meta());
                    let tag_x = end - tag.width();
                    scene::draw_layout_first_line(
                        canvas,
                        root,
                        &tag,
                        tag_x,
                        rect.y + (rect.h - tag.height()) * 0.5,
                        Some(p.text_dim),
                    );
                    end = tag_x - 6.0;
                }
                let room = (end - rect.x - indent).max(1.0);
                let shown = self.engine.elided(&label, style, room);
                let line = self.engine.simple_layout(&shown, style);
                // One line sits in the row's middle; a name with its
                // description under it starts near the top, as an
                // approval mode's does. What sits at the row's end stays
                // in the middle of the whole row either way.
                let name_y = if description.is_some() {
                    rect.y + 5.0
                } else {
                    rect.y + (rect.h - line.height()) * 0.5
                };
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &line,
                    rect.x + indent,
                    name_y,
                    Some(colour),
                );
                if let Some(description) = &description {
                    let style = theme::role::meta();
                    let shown = self.engine.elided(description, style, room);
                    let second = self.engine.simple_layout(&shown, style);
                    scene::draw_layout_first_line(
                        canvas,
                        root,
                        &second,
                        rect.x + indent,
                        name_y + line.height() + 2.0,
                        Some(p.text_dim),
                    );
                }
                // A favorite's agent, by its mark, level with the name.
                if let Row::Favorite(agent, _) = row {
                    let mark = Rect::new(
                        rect.x + 8.0,
                        name_y + (line.height() - ROW_MARK) * 0.5,
                        ROW_MARK,
                        ROW_MARK,
                    );
                    let tone = if can { p.text } else { p.text_dim };
                    draw_provider_mark(canvas, root, &mut self.engine, agent, true, mark, tone);
                }
            }
        }
        if let Some(scrollbar) = self.picker_scrollbar(&geometry) {
            let active = self.picker.scroll_drag.is_some()
                || scrollbar.in_gutter(self.pointer.0, self.pointer.1);
            scrollbar::draw(canvas, root, scrollbar, active);
        }

        // The Fast row, then the Context row, above the level's.
        self.draw_fast_row(canvas, root, &geometry);
        self.draw_context_row(canvas, root, &geometry);

        // The footer: the level's word, ↺, and the slider.
        let (Some(footer), Some(control), Some(shown)) = (
            geometry.footer,
            self.effort_control().cloned(),
            self.effort_shown(),
        ) else {
            return;
        };
        let choice = self
            .transcript
            .as_ref()
            .map_or(ReasoningChoice::Default, |transcript| {
                transcript.chat.current_model.default_reasoning
            });
        let locked = self.effort_locked();
        scene::fill(
            canvas,
            root,
            p.panel_border,
            &scene::rrect(footer.x, footer.y, footer.w, 1.0, 0.5),
        );
        let (word, suffix) = footer_words(&control, choice, self.lang);
        let line = self.engine.simple_layout(&word, theme::role::row());
        let label_y = footer.y + 6.0;
        scene::draw_layout_first_line(
            canvas,
            root,
            &line,
            footer.x + 8.0,
            label_y,
            Some(if locked { p.text_dim } else { p.text }),
        );
        if let Some(suffix) = suffix {
            let dim = self.engine.simple_layout(&suffix, theme::role::meta());
            scene::draw_layout_first_line(
                canvas,
                root,
                &dim,
                footer.x + 8.0 + line.width() + 6.0,
                label_y + (line.height() - dim.height()) * 0.5,
                Some(p.text_dim),
            );
        }
        if let Some(reset) = geometry.reset() {
            IconButton::new(icons::RESET, reset)
                .with_state(IconButtonState {
                    hovered: reset.contains(self.pointer.0, self.pointer.1),
                    active: self.picker.focus == PickerFocus::Reset,
                    disabled: locked || choice == ReasoningChoice::Default,
                })
                .draw(canvas, root, &mut self.engine);
        }
        effort::draw_track(
            canvas,
            root,
            &mut self.engine,
            self.lang,
            footer,
            &control,
            shown,
            if locked { (-1.0, -1.0) } else { self.pointer },
            self.picker.focus == PickerFocus::Slider,
        );
    }

    /// A row's ☆ (D46): outlined, brighter under the pointer, while it is
    /// not a favorite; filled in the accent once it is. Named for a screen
    /// reader and the inspector by what a press does — it raises no
    /// tooltip, as the rows raise none (D25).
    fn draw_star(&mut self, canvas: &mut Scene, root: Affine, star: Rect, starred: bool) {
        let p = theme::p();
        let name = t(
            self.lang,
            if starred {
                Key::AiModelFavoriteRemove
            } else {
                Key::AiModelFavoriteAdd
            },
        );
        let _star = koru_draw::inspect::scope(
            name,
            "button",
            root,
            scene::rect(star.x, star.y, star.w, star.h),
            |r| r.property("accessible name", name, "caller label"),
        );
        let hovered = star.contains(self.pointer.0, self.pointer.1);
        let (layout, colour) = if starred {
            (self.engine.icon_layout_filled(icons::STAR, 14.0), p.accent)
        } else {
            (
                self.engine.icon_layout(icons::STAR, 14.0),
                if hovered { p.text } else { p.text_dim },
            )
        };
        scene::draw_layout(
            canvas,
            root,
            &layout,
            star.x + (star.w - layout.width()) * 0.5,
            star.y + (star.h - layout.height()) * 0.5,
            Some(colour),
        );
    }
}

/// **The rail's ground** (D45): the colour of the window's own left rail —
/// Home, documents, folders, tasks, chats — which is the window's ground
/// ([`theme::base_fill`]): the person's base colour for the theme on
/// screen, or the theme's own. The agents' marks sit on the same ground as
/// the app's, light or dark.
pub(crate) fn rail_fill() -> vello::peniko::Color {
    theme::base_fill()
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
    Rect::new(
        geometry.search.x,
        geometry.search.y,
        (geometry.search.w - geometry.refresh.w - 4.0).max(1.0),
        geometry.search.h,
    )
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

/// An entry's mark on the rail: *Favorites*' filled star (D46), else its
/// provider's mark ([`draw_provider_mark`]).
fn draw_mark(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    entry: &RailEntry,
    rect: Rect,
    colour: vello::peniko::Color,
) {
    let inner = Rect::new(rect.x + 7.0, rect.y + 7.0, rect.w - 14.0, rect.h - 14.0);
    match entry {
        RailEntry::Favorites => {
            let layout = engine.icon_layout_filled(icons::STAR, 18.0);
            scene::draw_layout(
                canvas,
                root,
                &layout,
                rect.x + (rect.w - layout.width()) * 0.5,
                rect.y + (rect.h - layout.height()) * 0.5,
                Some(colour),
            );
        }
        RailEntry::Provider(provider) => {
            draw_provider_mark(canvas, root, engine, provider, false, inner, colour)
        }
        RailEntry::Agent(agent) => {
            draw_provider_mark(canvas, root, engine, agent, true, inner, colour)
        }
    }
}

/// A provider's mark in `inner`: the agent's brand by its kind
/// (`agent-claude` wears Claude's), OpenAI's for OpenAI, a glyph where there
/// is no brand — [`provider_mark`], which a chat tab wears too. The rail
/// draws it, and so does a favorite's row, at its start.
fn draw_provider_mark(
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
            let layout = engine.icon_layout(glyph, inner.h + 2.0);
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
