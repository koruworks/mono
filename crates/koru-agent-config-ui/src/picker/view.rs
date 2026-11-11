// SPDX-License-Identifier: MPL-2.0

//! **The model picker** (agent-models §9, rung 5): the agents run down a
//! rail, one mark each; above the selected agent's models sits a *Search
//! models…* field with ↻; the footer (D76, the user's design *A1a*) is the
//! effort strip — one segment per level the model offers, the chosen level
//! raised, and a press on the model's default level the way back to it —
//! over a row of pill toggles, *⚡ Fast* and *1M context*, each only when
//! the model offers it, with *Effort: High (default)* at the row's end. It
//! opens upward, above its anchor and end-aligned with it — below only when
//! there is no room above — and it is driven fully from the keyboard.
//!
//! **Choosing a model keeps the picker open** (D66): the ✓ moves and the
//! footer becomes the new model's. A press outside the picker, or Esc,
//! closes it. **Search spans every agent** (D67). A model row's ☆ stars it
//! (D46); the rail then leads with *Favorites*. **Once a conversation has
//! begun there is no rail**: it keeps its provider, so the picker shows
//! that provider's list alone, and a search looks only there.
//!
//! **Host-fed.** The host builds [`Inputs`] for each call and lends the
//! picker its state ([`PickerState`]), its open flag, its tooltip and its
//! text engine; [`Picker`] answers questions, [`ModelPicker`] takes the
//! person's input. What the person chose comes back as [`Outcome`]s, which
//! the host drains from the state after the call ([`PickerState::take_outcomes`]),
//! carries out, and then — while [`PickerState::pending`] — lets the picker
//! finish against the inputs as they now are ([`ModelPicker::resume`]): the
//! list settles against the chosen model's footer only once the host has
//! made it the current one.

use std::collections::HashMap;

use koru_draw::el::{Frame, FrameCx, Interaction, Key as ElKey, Name};
use koru_draw::text::TextEngine;
use koru_draw::theme;
use koru_i18n::{Key, t};
use koru_ui::Typed;
use koru_ui::popover::{PopoverAlign, PopoverShell};
use koru_ui::tooltip::{Tooltip, TooltipContent};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};
use winit::window::CursorIcon;

use super::elements::{
    self, Footer, LIGHTNING, ListRow, MarkName, MarkSpec, Painted, Pill, Segment, Spec, StarSpec,
};
use super::inputs::{
    AgentId, DEFAULT_MODEL, Inputs, Listing, Outcome, Readiness, default_model_label, find_listed,
    group_label, listed_models, model_groups, model_label,
};
use super::paint::SearchPaint;
use super::{
    FavoriteModel, Geometry, LIST_ROWS, PickerFocus, PickerState, RAIL_PAGE, ROW_H, RailEntry, Row,
    TOOLTIP_CONTEXT_LOCKED, TOOLTIP_LOCKED, TOOLTIP_RAIL, TOOLTIP_REFRESH, context, geometry_with,
    matches, rail_dividers, rail_height, rail_rects, readout, row_id, scroll_into_view, size,
    step_highlight, toggled, words_id,
};
use crate::effort::{self, Effort};

/// What the picker still owes once the host has carried out a call's
/// outcomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pending<P> {
    /// Keep the list in range and the chosen row in view: a model was
    /// chosen, or *Legacy models* opened or closed.
    Settle,
    /// Keep the keyboard on a row that is still there: the favorites
    /// changed.
    SettleHighlight,
    /// Show `entry`'s list, now that its agents' models were asked for.
    Select(RailEntry<P>),
}

/// **The picker, read**: every question about it, over the host's inputs
/// and the picker's state, changing nothing.
pub struct Picker<'v, 'a, P> {
    pub inputs: &'v Inputs<'a, P>,
    pub state: &'v PickerState<P>,
    pub open: bool,
}

impl<'v, 'a, P> Clone for Picker<'v, 'a, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P> Copy for Picker<'_, '_, P> {}

impl<'v, 'a, P: AgentId> Picker<'v, 'a, P> {
    pub fn new(inputs: &'v Inputs<'a, P>, state: &'v PickerState<P>, open: bool) -> Self {
        Self {
            inputs,
            state,
            open,
        }
    }

    fn lang(&self) -> koru_i18n::Lang {
        self.inputs.lang
    }

    fn current_provider(&self) -> Option<&'v P> {
        self.inputs.current.as_ref().map(|current| current.provider)
    }

    fn ready(&self, agent: &P) -> bool {
        self.inputs.readiness(agent) == Some(Readiness::Ready)
    }

    /// The favorites the picker lists (D46): those of every agent offered
    /// and turned on, in the rail's agent order and, within an agent, the
    /// order they were starred. A turned-off agent's are kept in the host's
    /// list and only hidden here.
    pub fn favorites(&self) -> Vec<(P, String)> {
        let mut shown = Vec::new();
        for agent in &self.inputs.agents {
            let Some(key) = agent.key else {
                continue;
            };
            if agent.hidden {
                continue;
            }
            shown.extend(
                self.inputs
                    .favorites
                    .iter()
                    .filter(|favorite| favorite.agent == key)
                    .map(|favorite| (agent.id.clone(), favorite.model.clone())),
            );
        }
        shown
    }

    /// Whether `agent`'s model `model` is starred.
    pub fn is_favorite(&self, agent: &P, model: &str) -> bool {
        self.inputs.key(agent).is_some_and(|key| {
            self.inputs
                .favorites
                .iter()
                .any(|favorite| favorite.agent == key && favorite.model == model)
        })
    }

    /// The model a row would star: an agent's listed model, or a favorite.
    /// The agent's *Default* is no model of its own, and a built-in
    /// provider's models are not an agent's, so neither has a ☆.
    pub fn row_favorite(&self, entry: &RailEntry<P>, row: &Row<P>) -> Option<(P, String)> {
        match (entry, row) {
            (_, Row::Found(entry, row)) => self.row_favorite(entry, row),
            (RailEntry::Agent(agent), Row::Model(id)) | (_, Row::Favorite(agent, id)) => self
                .inputs
                .key(agent)
                .is_some()
                .then(|| (agent.clone(), id.clone())),
            _ => None,
        }
    }

    /// The rail's entries: *Favorites* while there are any, a begun
    /// conversation's built-in provider, then every offered agent in the
    /// person's order — a turned-off one only when it is the current one.
    /// A begun conversation's rail is its own entry alone, and is not
    /// drawn; an owned one's is its owner's agent alone.
    pub fn rail(&self) -> Vec<RailEntry<P>> {
        let current = self.current_provider();
        let mut rail = Vec::new();
        if !self.favorites().is_empty() {
            rail.push(RailEntry::Favorites);
        }
        if let Some(builtin) = &self.inputs.builtin {
            rail.push(RailEntry::Provider(builtin.id.clone()));
        }
        rail.extend(
            self.inputs
                .agents
                .iter()
                .filter(|agent| !agent.hidden || Some(&agent.id) == current)
                .map(|agent| RailEntry::Agent(agent.id.clone())),
        );
        if self.rail_hidden() {
            rail.retain(|entry| entry.provider() == current);
        }
        if let Some(allowed) = self.inputs.allowed {
            rail.retain(|entry| entry.provider() == Some(allowed));
        }
        rail
    }

    /// **Whether the rail is hidden**: the conversation has begun, or only
    /// one agent may be offered.
    pub fn rail_hidden(&self) -> bool {
        self.inputs.allowed.is_some() || self.inputs.applies.begun()
    }

    /// The marks the rail draws, presses and steps through: its entries,
    /// or none while it is hidden.
    pub fn marks(&self) -> Vec<RailEntry<P>> {
        if self.rail_hidden() {
            Vec::new()
        } else {
            self.rail()
        }
    }

    fn rail_page_start(&self, count: usize) -> usize {
        self.state
            .rail_offset
            .min(count.saturating_sub(1) / RAIL_PAGE * RAIL_PAGE)
    }

    /// Only these marks enter the panel's element tree and hit targets.
    pub fn visible_rail(&self) -> Vec<RailEntry<P>> {
        let rail = self.marks();
        let start = self.rail_page_start(rail.len());
        rail.into_iter().skip(start).take(RAIL_PAGE).collect()
    }

    /// Whether the rail pages back, and on.
    pub fn rail_pages(&self) -> (bool, bool) {
        let count = self.marks().len();
        let start = self.rail_page_start(count);
        (start > 0, start + RAIL_PAGE < count)
    }

    /// **The rail's entry on open** (§9.1): *Favorites*, while there are
    /// favorites and the rail shows; else the current provider; else the
    /// host's preferred agent; else the first agent that is ready; else the
    /// first.
    pub fn initial_entry(&self) -> Option<RailEntry<P>> {
        let rail = self.rail();
        if !self.rail_hidden() && rail.contains(&RailEntry::Favorites) {
            return Some(RailEntry::Favorites);
        }
        let current = self.current_provider();
        let selected = self.inputs.preferred.as_ref();
        let ready = |entry: &&RailEntry<P>| entry.provider().is_some_and(|p| self.ready(p));
        rail.iter()
            .find(|entry| entry.provider() == current)
            .or_else(|| {
                rail.iter().find(|entry| {
                    matches!(entry, RailEntry::Agent(_)) && entry.provider() == selected
                })
            })
            .or_else(|| rail.iter().find(ready))
            .or_else(|| rail.first())
            .cloned()
    }

    /// The entry whose models the list shows.
    pub fn entry(&self) -> Option<RailEntry<P>> {
        let rail = self.rail();
        self.state
            .rail
            .clone()
            .filter(|entry| rail.contains(entry))
            .or_else(|| self.initial_entry())
    }

    pub(crate) fn query(&self) -> String {
        self.state.query.raw_text().trim().to_owned()
    }

    /// Whether a row of `entry` can be chosen now: an agent while nothing
    /// is begun and the agent is ready — and in a begun conversation its
    /// own agent, where its host takes a model per message; the built-in
    /// provider's models always. *Favorites* answers row by row.
    fn entry_choosable(&self, entry: &RailEntry<P>) -> bool {
        match entry {
            RailEntry::Favorites | RailEntry::Provider(_) => true,
            RailEntry::Agent(agent) => {
                self.ready(agent)
                    && self.inputs.current.as_ref().is_none_or(|current| {
                        !self.inputs.applies.begun()
                            || (current.provider == agent && !self.inputs.applies.model_locked())
                    })
            }
        }
    }

    /// Whether `agent` has a model the query finds, for the rail's dimming
    /// while typing (S3).
    pub fn agent_matches_query(&self, agent: &P, query: &str) -> bool {
        query.is_empty()
            || self
                .agent_model_rows(agent, query)
                .iter()
                .any(|row| !matches!(row, Row::Note(_)))
    }

    /// An entry's name, as its heading in a search and its rail mark's
    /// tooltip read.
    pub fn entry_name(&self, entry: &RailEntry<P>) -> String {
        match entry {
            RailEntry::Favorites => t(self.lang(), Key::AiPickerFavorites).to_owned(),
            RailEntry::Agent(agent) => self
                .inputs
                .agent(agent)
                .map_or_else(|| agent.to_string(), |agent| agent.label.to_owned()),
            RailEntry::Provider(provider) => self.provider_name(provider),
        }
    }

    fn provider_name(&self, provider: &P) -> String {
        match &self.inputs.builtin {
            Some(builtin) if &builtin.id == provider => builtin.name.clone(),
            _ => provider.to_string(),
        }
    }

    /// **The list** (§9.2): with a query, what it finds across every entry
    /// (D67); without one, the selected entry's. With the rail hidden, a
    /// query narrows the conversation's own provider's list.
    pub fn rows(&self) -> Vec<Row<P>> {
        let query = self.query();
        if !query.is_empty() && !self.rail_hidden() {
            return self.search_rows(&query);
        }
        let Some(entry) = self.entry() else {
            return Vec::new();
        };
        self.entry_rows(&entry, &query)
    }

    /// **A search across every entry** (D67), in the rail's order, each
    /// under a heading of its own; an entry where it finds nothing is left
    /// out. *Favorites* is not searched apart: its models are the agents'.
    fn search_rows(&self, query: &str) -> Vec<Row<P>> {
        let mut rows = Vec::new();
        for entry in self.rail() {
            if entry == RailEntry::Favorites {
                continue;
            }
            let found: Vec<Row<P>> = self
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
            rows.push(Row::Note(t(self.lang(), Key::AiModelsNoMatch).to_owned()));
        }
        rows
    }

    /// **The list for `entry`** (§9.2): the agent's *Default*, then its
    /// models — by group with a heading when it has more than one — the
    /// query applied; or the one word its state calls for.
    fn entry_rows(&self, entry: &RailEntry<P>, query: &str) -> Vec<Row<P>> {
        let lang = self.lang();
        let note = |key: Key| Row::Note(t(lang, key).to_owned());
        match entry {
            RailEntry::Favorites => {
                let rows: Vec<Row<P>> = self
                    .favorites()
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
                let name = self.provider_name(provider);
                let models = match &self.inputs.builtin {
                    Some(builtin) if &builtin.id == provider => builtin.models.as_slice(),
                    _ => &[],
                };
                let rows: Vec<Row<P>> = models
                    .iter()
                    .filter(|model| matches(query, &[&model.id, &model.label, &name]))
                    .map(|model| Row::Model(model.id.clone()))
                    .collect();
                if rows.is_empty() && !query.is_empty() {
                    vec![note(Key::AiModelsNoMatch)]
                } else {
                    rows
                }
            }
            RailEntry::Agent(agent) => {
                let status = self.inputs.readiness(agent).unwrap_or(Readiness::Checking);
                let listing = self.inputs.agent(agent).map(|agent| &agent.listing);
                let known = matches!(listing, Some(Listing::Listed(_)));
                match status {
                    Readiness::Ready if self.state.refreshing.contains(agent) => {
                        vec![note(Key::TerminalChecking)]
                    }
                    Readiness::Ready => match listing {
                        None | Some(Listing::Loading) => vec![note(Key::AiAgentModelsLoading)],
                        Some(Listing::Failed) => {
                            let mut rows = vec![note(Key::AiAgentModelsFailed)];
                            if matches(
                                query,
                                &[&default_model_label(&[], lang), &self.entry_name(entry)],
                            ) {
                                rows.push(Row::Default);
                            }
                            rows
                        }
                        Some(Listing::Listed(_)) => self.agent_model_rows(agent, query),
                    },
                    // A list kept from the last run, dimmed, under *Checking…*.
                    Readiness::Checking if known => {
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
                            vec![Row::Note(other.word(lang).unwrap_or_else(|| {
                                t(lang, Key::TerminalChecking).to_owned()
                            }))];
                        // Checking will settle on its own; everything else
                        // here is put right in the host's agent settings.
                        if other != Readiness::Checking && self.inputs.manage {
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
    /// matched though no row shows it (D43) — and by its agent's name.
    fn agent_model_rows(&self, agent: &P, query: &str) -> Vec<Row<P>> {
        let lang = self.lang();
        let models = self.inputs.models(agent);
        let name = self.entry_name(&RailEntry::Agent(agent.clone()));
        let mut rows = Vec::new();
        let default = models.iter().find(|model| model.id == DEFAULT_MODEL);
        if matches(
            query,
            &[
                &default_model_label(models, lang),
                default
                    .and_then(|model| model.description)
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
            .filter(|group| listed_models(models).any(|m| m.group == group && !m.legacy))
            .collect();
        for group in &groups {
            let heading = group_label(models, group);
            let found: Vec<Row<P>> = listed_models(models)
                .filter(|model| model.group == group && !model.legacy)
                .filter(|model| {
                    matches(
                        query,
                        &[
                            &model_label(model),
                            model.id,
                            &heading,
                            model.description.unwrap_or_default(),
                            &name,
                        ],
                    )
                })
                .map(|model| Row::Model(model.id.to_owned()))
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
                if self.state.legacy_open {
                    rows.extend(legacy.iter().map(|model| Row::Model(model.id.to_owned())));
                }
            }
        } else {
            rows.extend(
                legacy
                    .into_iter()
                    .filter(|model| {
                        let heading = group_label(models, model.group);
                        matches(
                            query,
                            &[
                                &model_label(model),
                                model.id,
                                &heading,
                                model.description.unwrap_or_default(),
                                &name,
                            ],
                        )
                    })
                    .map(|model| Row::Model(model.id.to_owned())),
            );
        }
        if rows.is_empty() {
            rows.push(Row::Note(t(lang, Key::AiModelsNoMatch).to_owned()));
        }
        rows
    }

    /// Whether row `row` of `entry` is a model its agent marks as older.
    pub fn row_is_legacy(&self, entry: &RailEntry<P>, row: &Row<P>) -> bool {
        match (entry, row) {
            (_, Row::Found(entry, row)) => self.row_is_legacy(entry, row),
            (RailEntry::Agent(agent), Row::Model(id)) => self
                .inputs
                .models(agent)
                .iter()
                .any(|model| model.id == id && model.legacy),
            _ => false,
        }
    }

    /// Whether the current model is one `entry` lists as older: then its
    /// *Legacy models* row opens expanded, so the ✓ is in the list.
    pub(crate) fn current_is_legacy(&self, entry: &RailEntry<P>) -> bool {
        let (Some(current), RailEntry::Agent(agent)) = (&self.inputs.current, entry) else {
            return false;
        };
        current.provider == agent
            && find_listed(self.inputs.models(agent), current.model)
                .is_some_and(|(listed, _)| listed.legacy)
    }

    /// How many older models `entry` lists: *Legacy models (N)*.
    fn legacy_count(&self, entry: &RailEntry<P>) -> usize {
        match entry {
            RailEntry::Agent(agent) => listed_models(self.inputs.models(agent))
                .filter(|model| model.legacy)
                .count(),
            RailEntry::Favorites | RailEntry::Provider(_) => 0,
        }
    }

    /// A built-in provider's model as its row reads.
    fn builtin_model_label(&self, model: &str) -> String {
        self.inputs
            .builtin
            .as_ref()
            .and_then(|builtin| builtin.models.iter().find(|m| m.id == model))
            .map_or_else(|| model.to_owned(), |m| m.label.clone())
    }

    /// Which rows can be chosen now. A favorite can be when its own agent's
    /// rows could be, and a search's row when its entry's could (D67).
    pub fn choosable(&self, rows: &[Row<P>]) -> Vec<bool> {
        let entry_choosable = self
            .entry()
            .is_some_and(|entry| self.entry_choosable(&entry));
        rows.iter()
            .map(|row| match row {
                // Showing the older models chooses nothing: it works in a
                // begun conversation too, even where its models only show.
                Row::LegacyToggle => true,
                // Opening the agent settings chooses nothing either.
                Row::ManageAgents => true,
                Row::Favorite(agent, _) => self.entry_choosable(&RailEntry::Agent(agent.clone())),
                Row::Found(entry, found) => self.entry_choosable(entry) && found.choosable(),
                // A begun agent conversation moves only to a model its
                // agent names: *Default* is the agent's own choice when a
                // session starts.
                Row::Default if self.inputs.applies.agent_begun() => false,
                _ => entry_choosable && row.choosable(),
            })
            .collect()
    }

    /// A row's label. A row raises no tooltip (D25): the row already reads
    /// the model's name.
    pub fn row_label(&self, entry: &RailEntry<P>, row: &Row<P>) -> String {
        let lang = self.lang();
        match (entry, row) {
            (_, Row::Found(entry, row)) => self.row_label(entry, row),
            (_, Row::EntryHeading(entry)) => self.entry_name(entry),
            (_, Row::Heading(text) | Row::Note(text)) => text.clone(),
            (RailEntry::Provider(_), Row::Model(id)) => self.builtin_model_label(id),
            (RailEntry::Provider(_) | RailEntry::Favorites, Row::Default | Row::Model(_)) => {
                String::new()
            }
            (_, Row::ManageAgents) => t(lang, Key::AiPickerManageAgents).to_owned(),
            (_, Row::LegacyToggle) => koru_i18n::fill(
                t(lang, Key::AiModelsLegacy),
                &[&koru_i18n::number::decimal(
                    lang,
                    self.legacy_count(entry) as u64,
                )],
            ),
            (RailEntry::Agent(agent), Row::Default) => {
                default_model_label(self.inputs.models(agent), lang)
            }
            (RailEntry::Agent(agent), Row::Model(id)) | (_, Row::Favorite(agent, id)) => {
                let models = self.inputs.models(agent);
                match models.iter().find(|model| model.id == id) {
                    Some(model) => model_label(model),
                    None => super::inputs::fallback_label(id, ""),
                }
            }
        }
    }

    /// **The open picker's frame, with its list** (M1): where a press
    /// lands, which row it is, the list's scrollbar — what is drawn, laid
    /// out without the pointer.
    pub fn frame(&self) -> Option<(Geometry, Spec, Frame)> {
        let geometry = self.geometry()?;
        let spec = self.spec();
        let frame = elements::layout(
            &elements::tree(&spec, Painted::default()),
            geometry.panel,
            FrameCx::root(),
        );
        Some((geometry, spec, frame))
    }

    /// How many rows the open list shows.
    fn visible_rows(&self) -> usize {
        self.geometry()
            .map_or(LIST_ROWS, |geometry| geometry.visible_rows())
    }

    /// The furthest the list scrolls: its last row at the list's foot.
    pub(crate) fn max_offset(&self) -> usize {
        self.rows().len().saturating_sub(self.visible_rows())
    }

    /// Whether a row is the current model.
    pub fn row_current(&self, entry: &RailEntry<P>, row: &Row<P>) -> bool {
        let Some(current) = &self.inputs.current else {
            return false;
        };
        if let Row::Found(entry, row) = row {
            return self.row_current(entry, row);
        }
        // Whose row it is: the entry's, or a favorite's own agent.
        let (provider, id) = match (row, entry.provider()) {
            (Row::Favorite(agent, id), _) => (agent, Some(id)),
            (Row::Default, Some(provider)) => (provider, None),
            (Row::Model(id), Some(provider)) => (provider, Some(id)),
            _ => return false,
        };
        if current.provider != provider {
            return false;
        }
        match id {
            None => current.model == DEFAULT_MODEL,
            // A conversation on a model's longer context is on that model.
            Some(id) => {
                current.model == id
                    || find_listed(self.inputs.models(provider), current.model)
                        .is_some_and(|(listed, _)| listed.id == id)
            }
        }
    }

    /// The picker follows its anchor and flips to the side with room.
    pub fn geometry(&self) -> Option<Geometry> {
        if !self.open {
            return None;
        }
        let hide_rail = self.rail_hidden();
        let rail = self.visible_rail();
        let (previous, next) = self.rail_pages();
        let marks = rail.len()
            + usize::from(previous)
            + usize::from(next)
            + usize::from(self.inputs.manage && !hide_rail && !next);
        let rail_h = rail_height(marks, rail_dividers(&rail).len());
        let place = self.inputs.place;
        let (w, h) = size(place.width, rail_h);
        let panel =
            PopoverShell::anchored(place.anchor, place.viewport, w, h, PopoverAlign::End).rect;
        // The footer as it is drawn: its pills are as wide as their words.
        Some(geometry_with(panel, self.footer(), hide_rail))
    }

    /// The first row a drag of the list's thumb to `y` shows, the thumb
    /// held `grab` below its top: the frame's whole rows.
    fn drag_to(&self, frame: &Frame, y: f32, grab: f32) -> Option<usize> {
        let scroll = frame.drag_offset(elements::LIST_KEY, y, grab)?;
        Some(((scroll / ROW_H).round().max(0.0) as usize).min(self.max_offset()))
    }

    /// Whether ↻ can be pressed for `agent`: not while its list is being
    /// rebuilt, and not for an agent that is not installed.
    pub fn refresh_enabled(&self, agent: &P) -> bool {
        !self.state.refreshing.contains(agent)
            && self.inputs.readiness(agent) != Some(Readiness::NotInstalled)
    }

    /// The tooltip at `x`, `y`: a rail mark's name (and its agent's
    /// readiness), ↻, and the read-only strip and window.
    pub fn tooltip_at(&self, geometry: &Geometry, x: f32, y: f32) -> Option<TooltipContent> {
        if !geometry.panel.contains(x, y) {
            return None;
        }
        let lang = self.lang();
        let rail = self.marks();
        for (index, rect) in rail_rects(geometry.rail, rail.len(), &rail_dividers(&rail))
            .into_iter()
            .enumerate()
        {
            if rect.contains(x, y) {
                let entry = &rail[index];
                let name = self.entry_name(entry);
                let word = entry
                    .provider()
                    .and_then(|provider| self.inputs.readiness(provider))
                    .and_then(|status| status.word(lang))
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
        if geometry.refresh.contains(x, y) && matches!(self.entry(), Some(RailEntry::Agent(_))) {
            return Some(TooltipContent::new(
                TOOLTIP_REFRESH,
                geometry.refresh,
                t(lang, Key::AiModelsRefresh),
            ));
        }
        if let Some(context) = geometry.context
            && context.contains(x, y)
            && self.inputs.applies.model_locked()
        {
            return Some(TooltipContent::new(
                TOOLTIP_CONTEXT_LOCKED,
                context,
                t(lang, Key::AiModelContextLockedAfterStart),
            ));
        }
        if let Some(strip) = geometry.strip
            && strip.contains(x, y)
            && self.inputs.applies.effort_locked()
        {
            return Some(TooltipContent::new(
                TOOLTIP_LOCKED,
                strip,
                t(lang, Key::AiEffortLockedAfterStart),
            ));
        }
        // **A model row raises none** (D25); a rail mark keeps its tooltip
        // above: its tooltip is the only place its agent is named.
        None
    }

    /// **The pointer over the open picker** (D44): the I-beam over its
    /// search field, the arrow everywhere else; a scrollbar drag keeps its
    /// resize arrows. `None` when the picker is closed or the pointer is
    /// outside it.
    pub fn cursor(&self, x: f32, y: f32) -> Option<CursorIcon> {
        let geometry = self.geometry()?;
        if self.state.scroll_drag.is_some() {
            return Some(CursorIcon::NsResize);
        }
        if !geometry.panel.contains(x, y) {
            return None;
        }
        Some(if geometry.field.contains(x, y) {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        })
    }

    /// What the chosen level reads as under the model's levels.
    fn effective(&self) -> Option<Effort> {
        let current = self.inputs.current.as_ref()?;
        let levels = self.inputs.footer.levels.as_ref()?;
        Some(effort::resolve(levels, current.effort))
    }

    /// What the open picker's tree declares: its marks — named, the
    /// selected one marked — the **+**, ↻, the list's rows and the rows
    /// under the list. With the rail hidden: no marks and no **+**.
    pub fn spec(&self) -> Spec {
        let hide_rail = self.rail_hidden();
        let rail = self.visible_rail();
        let dividers = rail_dividers(&rail);
        let (rail_previous, rail_next) = self.rail_pages();
        let selected = self.entry();
        let query = self.query();
        // A query's results span every entry (D67): no mark reads as the
        // list's while one is typed.
        let searching = !query.is_empty();
        Spec {
            hide_rail,
            marks: rail
                .iter()
                .enumerate()
                .map(|(index, entry)| MarkSpec {
                    key: elements::mark_key(entry),
                    name: match entry {
                        RailEntry::Favorites => MarkName::Key(Key::AiPickerFavorites),
                        RailEntry::Agent(_) | RailEntry::Provider(_) => {
                            MarkName::Content(self.entry_name(entry))
                        }
                    },
                    provider: self.entry_mark(entry),
                    dim: match entry {
                        RailEntry::Agent(agent) => {
                            !self.ready(agent) || !self.agent_matches_query(agent, &query)
                        }
                        RailEntry::Favorites | RailEntry::Provider(_) => false,
                    },
                    divider_after: dividers.contains(&index),
                    selected: !searching && selected.as_ref() == Some(entry),
                })
                .collect(),
            manage: self.inputs.manage && !hide_rail && !rail_next,
            rail_previous,
            rail_next,
            refresh: match &selected {
                Some(RailEntry::Agent(agent)) => Some(!self.refresh_enabled(agent)),
                _ => None,
            },
            footer: self.footer(),
            list: self.list_rows(),
            offset: self.state.offset,
            list_name: match &selected {
                _ if searching && !hide_rail => Some(Name::Key(Key::AiPickerModels)),
                Some(RailEntry::Favorites) => Some(Name::Key(Key::AiPickerFavorites)),
                Some(entry) => Some(Name::Text(self.entry_name(entry))),
                None => None,
            },
        }
    }

    /// **Where a screen reader's focus is** while the picker is open: the
    /// search field while the keyboard is in it; the highlighted row while
    /// it is in the list (the list itself while no row is); the selected
    /// rail mark, the strip's level or a pill. A host that publishes the
    /// picker publishes this as its focus. `None` while closed.
    ///
    /// The search field keeps the focus while Up and Down move the lit
    /// row under it, as a combo box's field does; saying that row needs an
    /// active descendant, which the element tree does not carry.
    pub fn accessibility_focus(&self) -> Option<ElKey> {
        if !self.open {
            return None;
        }
        match self.state.focus {
            PickerFocus::Search => Some(elements::SEARCH_KEY),
            PickerFocus::List => {
                let entry = self.entry();
                let rows = self.rows();
                Some(
                    self.state
                        .highlight
                        .and_then(|index| rows.get(index))
                        .and_then(|row| row_id(entry.as_ref()?, row))
                        .map_or(elements::LIST_KEY, |id| elements::row_key(&id)),
                )
            }
            _ => self.focused_key(&self.spec()),
        }
    }

    /// What is typed in the search field: the value a host publishes for
    /// it.
    pub fn search_text(&self) -> String {
        self.state.query.raw_text()
    }

    /// The mark an entry wears: an agent's or the built-in provider's, as
    /// the host chose it; none for *Favorites*.
    fn entry_mark(&self, entry: &RailEntry<P>) -> Option<koru_draw::brand::ProfileMark> {
        match entry {
            RailEntry::Favorites => None,
            RailEntry::Agent(agent) => self.agent_mark(agent),
            RailEntry::Provider(provider) => self
                .inputs
                .builtin
                .as_ref()
                .filter(|builtin| &builtin.id == provider)
                .map(|builtin| builtin.mark),
        }
    }

    fn agent_mark(&self, agent: &P) -> Option<koru_draw::brand::ProfileMark> {
        self.inputs.agent(agent).map(|agent| agent.mark)
    }

    /// **The footer** (D76): a segment for each level the current model
    /// offers, the chosen level raised; *⚡ Fast* and *1M context* when the
    /// model offers them; the readout. `None` for a model that offers none
    /// of them: no footer, no divider.
    pub fn footer(&self) -> Option<Footer> {
        let lang = self.lang();
        let offer = &self.inputs.footer;
        let levels = offer.levels.as_ref();
        if levels.is_none() && offer.fast.is_none() && offer.context.is_none() {
            return None;
        }
        let locked = self.inputs.applies.effort_locked();
        let shown = self.effective();
        let choice = self
            .inputs
            .current
            .as_ref()
            .map_or(Effort::Default, |current| current.effort);
        let segments = levels
            .map(|levels| {
                effort::stops(levels)
                    .into_iter()
                    .map(|stop| Segment {
                        key: elements::segment_key(stop),
                        choice: stop,
                        label: effort::short_word(lang, stop),
                        name: effort::word_key(stop),
                        selected: shown == Some(stop),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Footer {
            levels: levels.is_some(),
            segments,
            locked,
            fast: offer.fast.map(|on| Pill {
                label: t(lang, Key::AiModelFast).to_owned(),
                icon: Some(LIGHTNING),
                on,
                disabled: false,
            }),
            context: offer.context.map(|window| Pill {
                label: context::pill_label(lang, window.tokens),
                icon: None,
                on: window.long,
                // The window is a model, the start's in a begun
                // conversation.
                disabled: self.inputs.applies.model_locked(),
            }),
            readout: levels
                .map(|levels| readout(Some(levels), choice, lang))
                .unwrap_or_default(),
        })
    }

    /// Every row of the list as it is drawn and pressed (M1).
    fn list_rows(&self) -> Vec<ListRow> {
        let Some(entry) = self.entry() else {
            return Vec::new();
        };
        let rows = self.rows();
        let choosable = self.choosable(&rows);
        let keyboard = matches!(self.state.focus, PickerFocus::Search | PickerFocus::List);
        let searching = !self.query().is_empty();
        // A heading's or a note's key is its words: two alike in one list
        // (a search's notes under two agents differ by entry already) take
        // their order among the alike, so no key is used twice.
        let mut words_seen: HashMap<String, usize> = HashMap::new();
        rows.iter()
            .enumerate()
            .map(|(index, row)| {
                let id = row_id(&entry, row);
                let label = self.row_label(&entry, row);
                // What the row is, under a search's wrapping.
                let shown = match row {
                    Row::Found(_, found) => &**found,
                    row => row,
                };
                let words_key = words_id(&entry, row).map(|words| {
                    let seen = words_seen.entry(words.clone()).or_insert(0);
                    *seen += 1;
                    match *seen {
                        1 => elements::words_key(&words),
                        n => elements::words_key(&format!("{words}\u{1f}{n}")),
                    }
                });
                let spoken = match shown {
                    Row::Favorite(agent, _) => koru_i18n::fill(
                        t(self.lang(), Key::AiPickerRowWith),
                        &[
                            label.as_str(),
                            &self.entry_name(&RailEntry::Agent(agent.clone())),
                        ],
                    ),
                    _ if self.row_is_legacy(&entry, row) => koru_i18n::fill(
                        t(self.lang(), Key::AiPickerRowWith),
                        &[label.as_str(), t(self.lang(), Key::AiModelsLegacyTag)],
                    ),
                    _ => label.clone(),
                };
                ListRow {
                    key: id.as_deref().map(elements::row_key),
                    spoken,
                    words_key,
                    heading: matches!(shown, Row::Heading(_) | Row::EntryHeading(_)),
                    label,
                    words: matches!(row, Row::Heading(_) | Row::Note(_) | Row::EntryHeading(_)),
                    mark: match row {
                        Row::Favorite(agent, _) => self.agent_mark(agent),
                        Row::EntryHeading(heading) => self.entry_mark(heading),
                        _ => None,
                    },
                    current: self.row_current(&entry, row),
                    can: choosable[index],
                    lit: keyboard && self.state.highlight == Some(index),
                    star: self.row_favorite(&entry, row).and_then(|(agent, model)| {
                        Some(StarSpec {
                            key: elements::star_key(id.as_deref()?),
                            starred: self.is_favorite(&agent, &model),
                            shown: false,
                        })
                    }),
                    legacy: (*row == Row::LegacyToggle).then_some(self.state.legacy_open),
                    // Under an expanded *Legacy models* row an older model
                    // needs no tag; found by a search, it has one.
                    tag: (searching && self.row_is_legacy(&entry, row))
                        .then(|| t(self.lang(), Key::AiModelsLegacyTag).to_owned()),
                }
            })
            .collect()
    }

    /// The control the keyboard is on, by key, for its ring: the selected
    /// rail mark, the strip's raised segment (its first while none is), or
    /// a pill.
    fn focused_key(&self, spec: &Spec) -> Option<ElKey> {
        match self.state.focus {
            PickerFocus::Rail => spec
                .marks
                .iter()
                .find(|mark| mark.selected)
                .map(|mark| mark.key),
            PickerFocus::Effort => spec.footer.as_ref().and_then(|footer| {
                footer
                    .segments
                    .iter()
                    .find(|segment| segment.selected)
                    .or_else(|| footer.segments.first())
                    .map(|segment| segment.key)
            }),
            PickerFocus::Fast => Some(elements::FAST_KEY),
            PickerFocus::Context => Some(elements::CONTEXT_KEY),
            PickerFocus::Search | PickerFocus::List => None,
        }
    }

    /// What the picker's adapters draw this frame beyond the spec: the
    /// search field, with the keyboard on it or not.
    fn painted(&self) -> Painted {
        Painted {
            search: Some(SearchPaint {
                input: self.state.query.clone(),
                placeholder: t(self.lang(), Key::AiModelSearch).to_owned(),
                focused: self.state.focus == PickerFocus::Search,
            }),
        }
    }
}

/// **The picker, driven**: the person's input, with the host's inputs and
/// the picker's lent state. What is chosen queues in the state as
/// [`Outcome`]s.
pub struct ModelPicker<'v, 'a, P> {
    inputs: &'v Inputs<'a, P>,
    state: &'v mut PickerState<P>,
    open: &'v mut bool,
    tooltip: &'v mut Tooltip,
    engine: &'v mut TextEngine,
    pointer: (f32, f32),
}

impl<'v, 'a, P: AgentId> ModelPicker<'v, 'a, P> {
    pub fn new(
        inputs: &'v Inputs<'a, P>,
        state: &'v mut PickerState<P>,
        open: &'v mut bool,
        tooltip: &'v mut Tooltip,
        engine: &'v mut TextEngine,
        pointer: (f32, f32),
    ) -> Self {
        Self {
            inputs,
            state,
            open,
            tooltip,
            engine,
            pointer,
        }
    }

    /// The picker, read.
    pub fn read(&self) -> Picker<'_, 'a, P> {
        Picker {
            inputs: self.inputs,
            state: &*self.state,
            open: *self.open,
        }
    }

    fn emit(&mut self, outcome: Outcome<P>) {
        self.state.outcomes.push(outcome);
    }

    /// **Close the picker**: its drag let go and its tooltip gone.
    pub fn close(&mut self) {
        *self.open = false;
        self.state.scroll_drag = None;
        self.tooltip.clear();
    }

    /// Ask the host for its agent settings, closing the picker first.
    pub fn ask_for_setup(&mut self) {
        self.close();
        self.emit(Outcome::Setup);
    }

    /// **Finish what a call left owed**, against the inputs as the host now
    /// has them. Run it while [`PickerState::pending`] after carrying out a
    /// call's outcomes.
    pub fn resume(&mut self) {
        match self.state.pending.take() {
            Some(Pending::Settle) => self.settle_after_choice(),
            Some(Pending::SettleHighlight) => self.settle_highlight(),
            Some(Pending::Select(entry)) => self.finish_select(entry),
            None => {}
        }
    }

    /// Keep the keyboard on a row that is still there after the list
    /// changed under it — an unstarred favorite leaves *Favorites*, and the
    /// last one takes *Favorites* off the rail.
    pub fn settle_highlight(&mut self) {
        if !*self.open {
            return;
        }
        // An entry that left the rail is replaced by the one shown in its
        // place, so a later star does not bring it back under the pointer.
        let read = self.read();
        let replaced = self
            .state
            .rail
            .as_ref()
            .is_some_and(|entry| !read.rail().contains(entry))
            .then(|| read.entry());
        if let Some(entry) = replaced {
            self.state.rail = entry;
        }
        let read = self.read();
        let rows = read.rows();
        let choosable = read.choosable(&rows);
        let fine = self
            .state
            .highlight
            .is_some_and(|index| choosable.get(index).copied().unwrap_or(false));
        if !fine {
            let from = self.state.highlight.map(|index| index.min(rows.len()));
            self.state.highlight = step_highlight(&choosable, from, -1)
                .or_else(|| step_highlight(&choosable, None, 1));
        }
        self.state.offset = self.state.offset.min(self.read().max_offset());
        self.scroll_highlight_into_view();
    }

    /// **Star or unstar** `agent`'s `model` (D46): the whole list handed to
    /// the host to store and show; the keyboard settles once it has.
    pub fn toggle_favorite(&mut self, agent: &P, model: &str) -> bool {
        let Some(key) = self.inputs.key(agent) else {
            return false;
        };
        let next = toggled(self.inputs.favorites, FavoriteModel::new(key, model));
        self.emit(Outcome::Favorites(next));
        self.state.pending = Some(Pending::SettleHighlight);
        true
    }

    fn page_rail(&mut self, down: bool) {
        let count = self.read().marks().len();
        let start = self.read().rail_page_start(count);
        self.state.rail_offset = if down {
            (start + RAIL_PAGE).min(count.saturating_sub(1) / RAIL_PAGE * RAIL_PAGE)
        } else {
            start.saturating_sub(RAIL_PAGE)
        };
        self.state.focus = PickerFocus::Rail;
    }

    /// **Expand or collapse *Legacy models (N)*** (M4): the agent's older
    /// models follow the row in the same list, or go. The keyboard stays on
    /// the row; the list keeps in range.
    fn toggle_legacy(&mut self, index: usize, open: bool) -> bool {
        if self.state.legacy_open == open {
            return true;
        }
        self.state.legacy_open = open;
        self.state.highlight = Some(index);
        self.settle_after_choice();
        true
    }

    /// **Open the picker** on its entry, the query empty, the search field
    /// focused and the current model in view.
    pub fn reset(&mut self) {
        let read = self.read();
        let rail = read.initial_entry();
        let rail_offset = rail
            .as_ref()
            .and_then(|selected| read.marks().iter().position(|entry| entry == selected))
            .map_or(0, |index| index / RAIL_PAGE * RAIL_PAGE);
        self.state.rail = rail;
        self.state.rail_offset = rail_offset;
        let read = self.read();
        self.state.legacy_open = self
            .state
            .rail
            .as_ref()
            .is_some_and(|entry| read.current_is_legacy(entry));
        self.state.query.set_text(self.engine, "");
        self.state.focus = PickerFocus::Search;
        self.state.scroll_drag = None;
        self.state.offset = 0;
        self.highlight_current_row();
    }

    /// Put the keyboard on the current model's row, or the first choosable
    /// one, and scroll it into view.
    fn highlight_current_row(&mut self) {
        let read = self.read();
        let rows = read.rows();
        let choosable = read.choosable(&rows);
        let entry = read.entry();
        let current = entry
            .as_ref()
            .and_then(|entry| rows.iter().position(|row| read.row_current(entry, row)));
        self.state.highlight = current.or_else(|| step_highlight(&choosable, None, 1));
        self.scroll_highlight_into_view();
    }

    fn scroll_highlight_into_view(&mut self) {
        let Some(index) = self.state.highlight else {
            self.state.offset = 0;
            return;
        };
        let visible = self.read().visible_rows();
        self.state.offset = scroll_into_view(index, self.state.offset, visible);
    }

    /// **Show `entry`'s list**: a mark pressed on the rail, or stepped to
    /// from it. A query is cleared, as its results span every entry (D67)
    /// and would hide the list asked for. Its agents' models are asked for
    /// where they are not listed; the list shows once the host has.
    pub fn select_rail_entry(&mut self, entry: RailEntry<P>) {
        let read = self.read();
        if let Some(index) = read.marks().iter().position(|mark| mark == &entry) {
            self.state.rail_offset = index / RAIL_PAGE * RAIL_PAGE;
        }
        self.state.legacy_open = self.read().current_is_legacy(&entry);
        if !self.state.query.raw_text().is_empty() {
            self.state.query.set_text(self.engine, "");
        }
        // The lists the entry's rows are named from: the agent's, or under
        // *Favorites* every favorite's agent's.
        let agents: Vec<P> = match &entry {
            RailEntry::Agent(agent) => vec![agent.clone()],
            RailEntry::Favorites => self
                .read()
                .favorites()
                .into_iter()
                .map(|(agent, _)| agent)
                .collect(),
            RailEntry::Provider(_) => Vec::new(),
        };
        for agent in agents {
            let listed = matches!(
                self.inputs.agent(&agent).map(|known| &known.listing),
                Some(Listing::Listed(_))
            );
            if !listed {
                self.emit(Outcome::Catalog(agent));
            }
        }
        self.state.pending = Some(Pending::Select(entry));
    }

    fn finish_select(&mut self, entry: RailEntry<P>) {
        self.state.rail = Some(entry);
        self.state.offset = 0;
        self.highlight_current_row();
    }

    /// **Choose row `index`**: an agent's model, a favorite, or a built-in
    /// provider's model, for the host to make the current one. **The picker
    /// stays open** (D66): the footer becomes the new model's once the host
    /// has made it so, and the list settles then.
    pub fn choose_row(&mut self, index: usize) -> bool {
        let read = self.read();
        let rows = read.rows();
        let choosable = read.choosable(&rows);
        let (Some(entry), Some(row)) = (read.entry(), rows.get(index)) else {
            return true;
        };
        if !choosable.get(index).copied().unwrap_or(false) {
            return true;
        }
        if *row == Row::LegacyToggle {
            let open = !self.state.legacy_open;
            return self.toggle_legacy(index, open);
        }
        if *row == Row::ManageAgents {
            self.ask_for_setup();
            return true;
        }
        // A search's row is its own entry's (D67).
        let (entry, row) = match row.clone() {
            Row::Found(entry, row) => (entry, *row),
            row => (entry, row),
        };
        self.state.highlight = Some(index);
        match (entry, row) {
            (RailEntry::Agent(agent), Row::Default) => self.emit(Outcome::Model {
                provider: agent,
                model: DEFAULT_MODEL.to_owned(),
            }),
            (RailEntry::Agent(agent), Row::Model(id)) | (_, Row::Favorite(agent, id)) => {
                self.emit(Outcome::Model {
                    provider: agent,
                    model: id,
                })
            }
            (RailEntry::Provider(_), Row::Model(id)) => self.emit(Outcome::BuiltinModel(id)),
            _ => {}
        }
        self.state.pending = Some(Pending::Settle);
        true
    }

    /// **The picker after a model is chosen** (D66): still open, its footer
    /// now the new model's, so the list is kept in range and the chosen row
    /// in view. Also run when a conversation's model lands from the service.
    pub fn settle_after_choice(&mut self) {
        if !*self.open {
            return;
        }
        self.state.offset = self.state.offset.min(self.read().max_offset());
        self.scroll_highlight_into_view();
    }

    /// **↻** (D13): the selected agent's list rebuilt now. *Checking…*
    /// stands in the list until the host says it landed; a second press
    /// waits for the first. Not for an agent that is not installed.
    pub fn refresh(&mut self) -> bool {
        let Some(RailEntry::Agent(agent)) = self.read().entry() else {
            return false;
        };
        if !self.read().refresh_enabled(&agent) {
            return true;
        }
        self.emit(Outcome::Refresh(agent));
        true
    }

    /// A press while the picker is open. `None` when the press is not the
    /// picker's — on its anchor, which toggles it — and `Some` when the
    /// picker took it, closing it for a press outside.
    pub fn mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        let (geometry, spec, frame) = self.read().frame()?;
        if !geometry.panel.contains(x, y) {
            if self.inputs.place.anchor.contains(x, y) {
                return None;
            }
            self.close();
            return Some(true);
        }
        if let Some(scrollbar) = frame.scrollbar(elements::LIST_KEY)
            && scrollbar.in_gutter(x, y)
        {
            let grab = scrollbar.grab_at(x, y);
            self.state.scroll_drag = Some(grab);
            if let Some(offset) = self.read().drag_to(&frame, y, grab) {
                self.state.offset = offset;
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
        let rail = self.read().visible_rail();
        if let Some(index) = rail
            .iter()
            .position(|entry| hit == Some(elements::mark_key(entry)))
        {
            self.state.focus = PickerFocus::Rail;
            self.select_rail_entry(rail[index].clone());
            return Some(true);
        }
        if hit == Some(elements::MANAGE_KEY) {
            self.ask_for_setup();
            return Some(true);
        }
        if geometry.refresh.contains(x, y) {
            return Some(self.refresh());
        }
        if geometry.search.contains(x, y) {
            self.state.focus = PickerFocus::Search;
            self.state.query.click(self.engine, geometry.search, x, y);
            return Some(true);
        }
        // The footer (D76): a level's segment — read only once an agent
        // conversation has begun on an older session host, the press saying
        // why (D70) — and the pills.
        if geometry.strip.is_some_and(|strip| strip.contains(x, y)) {
            self.state.focus = PickerFocus::Effort;
            if self.inputs.applies.effort_locked() {
                self.emit(Outcome::EffortLocked);
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
                Some(segment) => {
                    self.emit(Outcome::Level(segment.choice));
                    true
                }
                None => true,
            });
        }
        if geometry.fast.is_some_and(|pill| pill.contains(x, y)) {
            self.state.focus = PickerFocus::Fast;
            return Some(self.toggle_fast());
        }
        if geometry.context.is_some_and(|pill| pill.contains(x, y)) {
            self.state.focus = PickerFocus::Context;
            return Some(self.toggle_context());
        }
        if geometry.list.contains(x, y) {
            self.state.focus = PickerFocus::List;
            let hit = frame.hit_at(x, y);
            // The ☆ stars the row rather than choosing it (D46), and
            // whether or not the row can be chosen now: it is a target of
            // its own, over its row.
            let read = self.read();
            if let Some(index) = spec
                .list
                .iter()
                .position(|row| row.star.as_ref().is_some_and(|star| Some(star.key) == hit))
                && let Some(entry) = read.entry()
                && let Some((agent, model)) = read
                    .rows()
                    .get(index)
                    .and_then(|row| read.row_favorite(&entry, row))
            {
                self.state.highlight = Some(index);
                return Some(self.toggle_favorite(&agent, &model));
            }
            if let Some(index) = spec
                .list
                .iter()
                .position(|row| row.key.is_some() && row.key == hit)
            {
                return Some(self.choose_row(index));
            }
        }
        Some(true)
    }

    /// Fast mode the other way, while the model offers it.
    pub fn toggle_fast(&mut self) -> bool {
        match self.inputs.footer.fast {
            Some(on) => {
                self.emit(Outcome::Fast(!on));
                true
            }
            None => false,
        }
    }

    /// The other context window, while the model offers one.
    pub fn toggle_context(&mut self) -> bool {
        match self.inputs.footer.context {
            Some(window) => {
                self.emit(Outcome::Context(!window.long));
                true
            }
            None => false,
        }
    }

    /// The pointer moved with the picker open: a scrollbar drag, and the
    /// tooltip under it. `Some` when the picker claimed the move.
    pub fn mouse_move(&mut self, x: f32, y: f32, now: std::time::Instant) -> Option<bool> {
        let geometry = self.read().geometry()?;
        if let Some(grab) = self.state.scroll_drag {
            if let Some((_, _, frame)) = self.read().frame()
                && let Some(next) = self.read().drag_to(&frame, y, grab)
            {
                let changed = next != self.state.offset;
                self.state.offset = next;
                return Some(changed);
            }
            return Some(true);
        }
        let tip = self.read().tooltip_at(&geometry, x, y);
        let changed = self.tooltip.set_hover(tip, now);
        // Over the panel the picker is all there is; outside it, only a
        // tooltip that went away is news.
        (geometry.panel.contains(x, y) || changed).then_some(true)
    }

    /// The wheel over the open picker scrolls its list, a row a notch.
    pub fn scroll(&mut self, dy: f32) -> bool {
        let previous = self.state.offset;
        let max = self.read().max_offset();
        if dy > 0.0 {
            self.state.offset = self.state.offset.saturating_sub(dy.ceil() as usize);
        } else if dy < 0.0 {
            self.state.offset = (self.state.offset + (-dy).ceil() as usize).min(max);
        }
        previous != self.state.offset
    }

    /// **The keyboard inside the picker** (§9.5; D12): typing edits the
    /// query wherever the focus is; Up and Down move the highlighted row,
    /// or the rail's entry from the rail; Enter chooses, and the picker
    /// stays open (D66); Left and Right move the effort strip's level a
    /// segment, and Space or Enter toggles a pill (D76); Tab and Shift+Tab
    /// move between search, rail, list, the strip and the pills; Esc clears
    /// the query, else closes. `None` when the picker is closed. With the
    /// rail hidden, neither Tab nor an arrow lands on it.
    pub fn key(
        &mut self,
        key: &WinitKey,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        if !*self.open {
            return None;
        }
        let hide_rail = self.read().rail_hidden();
        // The conversation began with the keyboard on the rail: it is on
        // the list.
        if hide_rail && self.state.focus == PickerFocus::Rail {
            self.state.focus = PickerFocus::List;
        }
        let levels = self.inputs.footer.levels.is_some();
        let context = self.inputs.footer.context.is_some();
        let fast = self.inputs.footer.fast.is_some();
        match key {
            WinitKey::Named(NamedKey::Escape) => {
                if !self.state.query.raw_text().is_empty() {
                    self.state.query.set_text(self.engine, "");
                    self.highlight_current_row();
                } else {
                    self.close();
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
                    .position(|focus| *focus == self.state.focus)
                    .unwrap_or(0);
                let next = if mods.shift_key() {
                    (at + order.len() - 1) % order.len()
                } else {
                    (at + 1) % order.len()
                };
                self.state.focus = order[next];
                return Some(true);
            }
            _ => {}
        }
        match self.state.focus {
            PickerFocus::Rail => match key {
                WinitKey::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                    let read = self.read();
                    let rail = read.marks();
                    let at = read
                        .entry()
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
                    self.state.focus = PickerFocus::List;
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
                let read = self.read();
                let rows = read.rows();
                let choosable = read.choosable(&rows);
                let step = if matches!(key, WinitKey::Named(NamedKey::ArrowUp)) {
                    -1
                } else {
                    1
                };
                if let Some(next) = step_highlight(&choosable, self.state.highlight, step) {
                    self.state.highlight = Some(next);
                    self.scroll_highlight_into_view();
                }
                Some(true)
            }
            WinitKey::Named(NamedKey::Enter) => {
                let index = self.state.highlight?;
                Some(self.choose_row(index))
            }
            // On *Legacy models*, Right expands it and Left collapses it;
            // in the search, either moves the caret as ever.
            WinitKey::Named(direction @ (NamedKey::ArrowRight | NamedKey::ArrowLeft))
                if (self.state.focus != PickerFocus::Search
                    || self.state.query.raw_text().is_empty())
                    && self.state.highlight.is_some_and(|index| {
                        self.read().rows().get(index) == Some(&Row::LegacyToggle)
                    }) =>
            {
                let index = self.state.highlight?;
                Some(self.toggle_legacy(index, *direction == NamedKey::ArrowRight))
            }
            WinitKey::Named(NamedKey::ArrowRight)
                if self.state.focus != PickerFocus::Search
                    || self.state.query.raw_text().is_empty() =>
            {
                let before = self.state.query.raw_text().to_owned();
                let typed = self.state.query.key(self.engine, key, text, mods) == Typed::Handled;
                Some(typed || before != self.state.query.raw_text())
            }
            _ => {
                // Typing is the query's, wherever the focus was.
                let before = self.state.query.raw_text().to_owned();
                let typed = self.state.query.key(self.engine, key, text, mods) == Typed::Handled;
                if self.state.query.raw_text() != before {
                    self.state.focus = PickerFocus::Search;
                    self.state.offset = 0;
                    let read = self.read();
                    let rows = read.rows();
                    let choosable = read.choosable(&rows);
                    self.state.highlight = step_highlight(&choosable, None, 1);
                }
                Some(typed || self.state.focus == PickerFocus::Search)
            }
        }
    }

    /// Left or Right on the strip: the level one segment over.
    fn step_level(&mut self, up: bool) -> bool {
        if self.inputs.applies.effort_locked() {
            self.emit(Outcome::EffortLocked);
            return true;
        }
        let Some(levels) = self.inputs.footer.levels.as_ref() else {
            return false;
        };
        let stops = effort::stops(levels);
        let at = self
            .read()
            .effective()
            .and_then(|choice| stops.iter().position(|stop| *stop == choice));
        let next = match (at, up) {
            (Some(at), true) => (at + 1).min(stops.len().saturating_sub(1)),
            (Some(at), false) => at.saturating_sub(1),
            (None, true) => 0,
            (None, false) => stops.len().saturating_sub(1),
        };
        match stops.get(next) {
            Some(choice) => {
                self.emit(Outcome::Level(*choice));
                true
            }
            None => false,
        }
    }

    /// Draw the open picker: the panel, and inside it the rail on its
    /// ground, the search with ↻, the list, and the rows under it — every
    /// record inside the *Popover surface*.
    pub fn draw(&mut self, canvas: &mut Scene, root: Affine) {
        let read = self.read();
        let Some(geometry) = read.geometry() else {
            return;
        };
        let panel = geometry.panel;
        let mut spec = read.spec();
        // The host's pointer and keyboard, by key: the frame's own target
        // under the pointer, the list while its thumb is dragged, and the
        // control the keyboard is on.
        let targets = elements::layout(
            &elements::tree(&spec, Painted::default()),
            panel,
            FrameCx::root(),
        );
        let hot = targets.hit_at(self.pointer.0, self.pointer.1);
        spec.show_stars(hot);
        let interaction = Interaction {
            hot,
            pressed: self.state.scroll_drag.map(|_| elements::LIST_KEY),
            focused: read.focused_key(&spec),
        };
        let tree = elements::tree(&spec, read.painted());
        let frame = elements::layout(&tree, panel, FrameCx::root().with_interaction(interaction));
        let radius = theme::overlay::popover().radius;
        let engine = &mut *self.engine;
        PopoverShell {
            rect: panel,
            radius,
        }
        .draw_containing(canvas, root, |canvas, root| {
            frame.paint(&tree, canvas, root, engine);
        });
    }
}
