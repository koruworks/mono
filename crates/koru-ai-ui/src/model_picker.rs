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
use vello::kurbo::Affine;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

use crate::agents::{
    AgentModels, AgentStatus, default_model_label, fallback_label, find_listed, group_label,
    is_agent_provider, listed_models, model_groups, model_label, provider_mark,
};
use crate::effort;

mod context;
mod fast;
pub(crate) use context::CONTEXT_H;
pub(crate) use fast::FAST_H;

/// The rail's width, and the square of each of its marks.
pub(crate) const RAIL_W: f32 = 44.0;
const RAIL_BUTTON: f32 = 32.0;
const RAIL_GAP: f32 = 6.0;
/// The room a divider takes after the chat's own built-in provider (S1).
const RAIL_DIVIDER: f32 = 9.0;
/// The search row, with ↻ at its end.
pub(crate) const SEARCH_H: f32 = 40.0;
pub(crate) const ROW_H: f32 = 30.0;
/// **The picker's one size** (D13): the search row, six rows of the list
/// and the footer — the size the person chose. The list scrolls inside it
/// when there are more, and a short list leaves room under it rather than
/// shrinking the picker, so the picker keeps its size from agent to agent.
/// A model with no footer gives the room to the list.
pub(crate) const LIST_ROWS: usize = 6;
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

/// One mark on the rail: a local agent, or — first, and only on a chat that
/// has begun on one — Koru AI, OpenAI or Ollama (S1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RailEntry {
    Provider(AiProviderId),
    Agent(AiProviderId),
}

impl RailEntry {
    pub(crate) fn provider(&self) -> &AiProviderId {
        match self {
            Self::Provider(provider) | Self::Agent(provider) => provider,
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
    /// A word in place of models: listing, checking, failed, no match.
    Note(String),
    /// *Legacy models ›*, at the foot: the models the agent marks as older,
    /// in a flyout (D11). Only while not searching; a search shows them
    /// inline instead.
    LegacyMore,
}

impl Row {
    fn choosable(&self) -> bool {
        matches!(self, Self::Default | Self::Model(_) | Self::LegacyMore)
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
    /// Row `visible` of the list, counted from the first one shown.
    pub(crate) fn row(&self, visible: usize) -> Rect {
        Rect::new(
            self.list.x,
            self.list.y + visible as f32 * ROW_H,
            self.list.w,
            ROW_H,
        )
    }

    /// How many rows the list has room for.
    pub(crate) fn visible_rows(&self) -> usize {
        (self.list.h / ROW_H).floor().max(0.0) as usize
    }

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
    let inner_x = panel.x + RAIL_W;
    let inner_w = (panel.w - RAIL_W - PAD).max(1.0);
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

/// The rail's marks, top to bottom; a divider follows the first when the
/// chat's own built-in provider leads (S1).
pub(crate) fn rail_rects(rail: Rect, count: usize, divider: bool) -> Vec<Rect> {
    let x = rail.x + (rail.w - RAIL_BUTTON) * 0.5;
    let mut y = rail.y + PAD + 4.0;
    (0..count)
        .map(|index| {
            let rect = Rect::new(x, y, RAIL_BUTTON, RAIL_BUTTON);
            y += RAIL_BUTTON + RAIL_GAP;
            if divider && index == 0 {
                y += RAIL_DIVIDER;
            }
            rect
        })
        .collect()
}

/// The height the rail's marks need.
pub(crate) fn rail_height(count: usize, divider: bool) -> f32 {
    PAD * 2.0
        + 8.0
        + count as f32 * (RAIL_BUTTON + RAIL_GAP)
        + if divider { RAIL_DIVIDER } else { 0.0 }
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

/// The first row shown so that `index` is in view, starting from `offset`.
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

    /// The **+** at the rail's end, while the host offers it: one more
    /// square after the last mark.
    pub(crate) fn picker_manage_rect(&self, geometry: &Geometry) -> Option<Rect> {
        if !self.agent_settings_offered {
            return None;
        }
        let rail = self.picker_rail();
        rail_rects(geometry.rail, rail.len() + 1, self.rail_has_divider(&rail))
            .last()
            .copied()
    }

    /// What the **+** is called for a screen reader and the inspector — it
    /// raises no tooltip (D25: the picker's rows show none; the rail's marks
    /// keep theirs because they are named nowhere else, and this one is).
    pub(crate) fn picker_manage_name(&self) -> &'static str {
        t(self.lang, Key::AiPickerManageAgents)
    }

    /// The rail's entries: every offered agent in registration order, led —
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
        if let Some(transcript) = &self.transcript
            && !transcript.messages.is_empty()
            && !is_agent_provider(&transcript.chat.current_model.provider)
        {
            rail.push(RailEntry::Provider(
                transcript.chat.current_model.provider.clone(),
            ));
        }
        if let Some(settings) = &self.settings {
            rail.extend(
                settings
                    .agents
                    .iter()
                    .filter(|agent| self.agent_shown(&agent.id) || Some(&agent.id) == current)
                    .map(|agent| RailEntry::Agent(agent.id.clone())),
            );
        }
        rail
    }

    fn rail_has_divider(&self, rail: &[RailEntry]) -> bool {
        matches!(rail.first(), Some(RailEntry::Provider(_))) && rail.len() > 1
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
            self.agent_status.get(entry.provider()).copied() == Some(AgentStatus::Ready)
        };
        rail.iter()
            .find(|entry| Some(entry.provider()) == current.as_ref())
            .or_else(|| {
                rail.iter().find(|entry| {
                    matches!(entry, RailEntry::Agent(_))
                        && Some(entry.provider()) == selected.as_ref()
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
    /// has no message and the agent is ready; the chat's own built-in
    /// provider's models always, as today.
    fn entry_choosable(&self, entry: &RailEntry) -> bool {
        match entry {
            RailEntry::Provider(_) => true,
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
        let Some(visible) = index.checked_sub(self.picker.offset) else {
            return false;
        };
        let parent = geometry.row(visible);
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

    /// Which rows can be chosen now.
    fn picker_choosable(&self, rows: &[Row]) -> Vec<bool> {
        let entry_choosable = self
            .picker_entry()
            .is_some_and(|entry| self.entry_choosable(&entry));
        rows.iter()
            .map(|row| entry_choosable && row.choosable())
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
            (RailEntry::Provider(_), Row::Default) => String::new(),
            (_, Row::LegacyMore) => t(self.lang, Key::AiModelsLegacy).to_owned(),
            (RailEntry::Agent(agent), Row::Default) => {
                default_model_label(self.listed_agent_models(agent), self.lang)
            }
            (RailEntry::Agent(agent), Row::Model(id)) => {
                let models = self.listed_agent_models(agent);
                match models.iter().find(|model| &model.id == id) {
                    Some(model) => model_label(model),
                    None => fallback_label(id, ""),
                }
            }
        }
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
        if &model.provider != entry.provider() {
            return false;
        }
        match row {
            Row::Default => model.requested_model == AGENT_DEFAULT_MODEL,
            // A chat on a model's longer context is on that model.
            Row::Model(id) => {
                &model.requested_model == id
                    || find_listed(
                        self.listed_agent_models(entry.provider()),
                        &model.requested_model,
                    )
                    .is_some_and(|(listed, _)| &listed.id == id)
            }
            _ => false,
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
        let rail_h = rail_height(marks, self.rail_has_divider(&rail));
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

    pub(crate) fn picker_scrollbar(&self, geometry: &Geometry) -> Option<scrollbar::ScrollbarGeom> {
        scrollbar::ScrollbarGeom::for_content(
            geometry.list,
            self.picker_rows().len() as f32 * ROW_H,
            self.picker.offset as f32 * ROW_H,
        )
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
        let visible = self
            .picker_geometry()
            .map_or(LIST_ROWS, |geometry| geometry.visible_rows());
        self.picker.offset = scroll_into_view(index, self.picker.offset, visible);
    }

    fn select_rail_entry(&mut self, entry: RailEntry) {
        self.picker.legacy = None;
        if let RailEntry::Agent(agent) = &entry
            && !matches!(self.agent_models.get(agent), Some(AgentModels::Listed(_)))
        {
            self.ask_agent_models(agent.clone());
        }
        self.picker.rail = Some(entry);
        self.picker.offset = 0;
        self.highlight_current_row();
    }

    /// **Choose row `index`**: an agent's model makes the agent and model
    /// the chat's (the level kept when it is offered, D18); a built-in
    /// provider's model is the chat's next model. The picker closes.
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
            (RailEntry::Agent(agent), Row::Model(id)) => self.choose_agent_model(agent, id),
            (RailEntry::Provider(_), Row::Model(id)) => self.change_chat_model(id),
            _ => true,
        }
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
            self.picker.offset = (scroll / ROW_H).round() as usize;
            return Some(true);
        }
        let rail = self.picker_rail();
        let divider = self.rail_has_divider(&rail);
        if let Some(index) = rail_rects(geometry.rail, rail.len(), divider)
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
            let visible = ((y - geometry.list.y) / ROW_H).floor().max(0.0) as usize;
            let index = self.picker.offset + visible;
            self.picker.focus = PickerFocus::List;
            if index < self.picker_rows().len() {
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
                let next = (scrollbar.scroll_for_pointer(y, grab) / ROW_H).round() as usize;
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
        let divider = self.rail_has_divider(&rail);
        let agents = self
            .settings
            .as_ref()
            .map(|settings| settings.agents.as_slice())
            .unwrap_or_default();
        for (index, rect) in rail_rects(geometry.rail, rail.len(), divider)
            .into_iter()
            .enumerate()
        {
            if rect.contains(x, y) {
                let entry = &rail[index];
                let name = match entry {
                    RailEntry::Agent(agent) => crate::agents::agent_label(agents, agent),
                    RailEntry::Provider(provider) => provider_name(provider),
                };
                let word = self
                    .agent_status
                    .get(entry.provider())
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

    /// The wheel over the open picker scrolls its list, a row a notch.
    pub(crate) fn picker_scroll(&mut self, dy: f32) -> bool {
        let previous = self.picker.offset;
        let visible = self
            .picker_geometry()
            .map_or(LIST_ROWS, |geometry| geometry.visible_rows());
        let max = self.picker_rows().len().saturating_sub(visible);
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
    /// search, rail, list, ↺ and the slider; Esc clears the query, else
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
        PopoverShell {
            rect: geometry.panel,
            radius: theme::overlay::popover().radius,
        }
        .draw(canvas, root);

        // The rail.
        let rail = self.picker_rail();
        let divider = self.rail_has_divider(&rail);
        let selected = self.picker_entry();
        let query = self.picker_query();
        let rects = rail_rects(geometry.rail, rail.len(), divider);
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
                RailEntry::Provider(_) => false,
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
        if divider && let Some(first) = rects.first() {
            let y = first.y + first.h + RAIL_GAP + RAIL_DIVIDER * 0.5 - 0.5;
            scene::fill(
                canvas,
                root,
                p.panel_border,
                &scene::rrect(first.x + 4.0, y, first.w - 8.0, 1.0, 0.5),
            );
        }

        // The search field and ↻.
        let search = Rect::new(
            geometry.search.x,
            geometry.search.y,
            (geometry.search.w - geometry.refresh.w - 4.0).max(1.0),
            geometry.search.h,
        );
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
        let visible = geometry.visible_rows();
        if let Some(entry) = &selected {
            for (index, row) in rows
                .iter()
                .enumerate()
                .skip(self.picker.offset)
                .take(visible)
            {
                let rect = geometry.row(index - self.picker.offset);
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
                        8.0,
                    ),
                };
                // What sits at the row's end: the flyout's caret, an older
                // model's tag while searching, the current one's check.
                let legacy_tag = self
                    .row_is_legacy(entry, row)
                    .then(|| t(self.lang, Key::AiModelsLegacyTag).to_owned());
                let mut end = rect.x + rect.w - 8.0;
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
                    let tag_x = end - tag.width() - if current { 22.0 } else { 0.0 };
                    scene::draw_layout_first_line(
                        canvas,
                        root,
                        &tag,
                        tag_x,
                        rect.y + (rect.h - tag.height()) * 0.5,
                        Some(p.text_dim),
                    );
                    end = tag_x - 6.0 + if current { 22.0 } else { 0.0 };
                }
                let check_w = if current { 22.0 } else { 0.0 };
                let room = (end - rect.x - indent - check_w).max(1.0);
                let shown = self.engine.elided(&label, style, room);
                let line = self.engine.simple_layout(&shown, style);
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &line,
                    rect.x + indent,
                    rect.y + (rect.h - line.height()) * 0.5,
                    Some(colour),
                );
                if current {
                    let check = self.engine.icon_layout(icons::CHECK, 13.0);
                    scene::draw_layout(
                        canvas,
                        root,
                        &check,
                        rect.x + rect.w - 8.0 - check.width(),
                        rect.y + (rect.h - check.height()) * 0.5,
                        Some(p.text_dim),
                    );
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

/// An entry's mark: the agent's brand by its kind (`agent-claude` wears
/// Claude's), OpenAI's for OpenAI, a glyph where there is no brand —
/// [`provider_mark`], which a chat tab wears too.
fn draw_mark(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    entry: &RailEntry,
    rect: Rect,
    colour: vello::peniko::Color,
) {
    let mark = provider_mark(entry.provider().as_str()).unwrap_or(brand::ProfileMark::Glyph(
        match entry {
            RailEntry::Provider(provider) if provider.as_str() == AiProviderId::OLLAMA => {
                icons::DESKTOP
            }
            RailEntry::Provider(_) => icons::SPARKLE,
            RailEntry::Agent(_) => icons::ROBOT,
        },
    ));
    let inner = Rect::new(rect.x + 7.0, rect.y + 7.0, rect.w - 14.0, rect.h - 14.0);
    match mark {
        brand::ProfileMark::Brand(icon) => {
            scene::draw_brand_icon(canvas, root, inner, colour, icon)
        }
        brand::ProfileMark::Glyph(glyph) => {
            let layout = engine.icon_layout(glyph, 18.0);
            scene::draw_layout(
                canvas,
                root,
                &layout,
                rect.x + (rect.w - layout.width()) * 0.5,
                rect.y + (rect.h - layout.height()) * 0.5,
                Some(colour),
            );
        }
    }
}

#[cfg(test)]
mod tests;
