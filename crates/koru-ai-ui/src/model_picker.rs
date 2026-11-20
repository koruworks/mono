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
//! frame; drawn, the same tree nests inside the *Popover
//! surface* record. The rows, the search and the footer's words are pure
//! functions, tested beside this file. The surface keeps the state
//! ([`PickerState`]).
//!
//! **The picker is the agent-configuration control's** (Bots P1.8,
//! `koru_agent_config_ui::picker`): its rail, list, search, keyboard,
//! pointer, geometry, drawing and semantics run there, over plain data. This
//! module is the chat's side of it: it builds the control's inputs from the
//! chat's catalog, settings and transcript for each call, lends it the
//! picker's state, and carries out what the person chose — a model through
//! `choose_agent_model`, a level through `choose_level`, Fast, the window,
//! the favorites, the catalog asks — with the chat's own rules, before the
//! control settles its list against the result. Every method the chat and
//! its suite called keeps its name and signature, one line over the
//! control.

use std::collections::{HashMap, HashSet};

use koru_agent_config_ui::picker::{
    self as cfg, Agent, Applies, Builtin, BuiltinModel, Context, Current, FooterOffer, Inputs,
    Listing, ModelPicker, ModelRecord, Outcome, Picker, Place,
};
use koru_ai_core::{AGENT_DEFAULT_MODEL, AiProviderId, ReasoningChoice};
use koru_ai_wire::{AgentModelView, ChatTranscript, ProviderSettingsResponse};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{brand, icons};
use koru_i18n::{Key, t};
use koru_ui::tooltip::TooltipContent;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key as WinitKey, ModifiersState};

use crate::agents::{
    AgentModels, AgentStatus, agent_key, default_model_label, fallback_label, find_listed,
    is_agent_provider, model_label, provider_mark,
};
use crate::effort;

mod context;
mod elements;
mod fast;

/// **The element layout the composer's panels share** (D64, D105): one
/// engine and text cache per UI thread, and one way to key a row by its
/// model's id — the approval-mode list and the workspace-access panel are
/// laid out on them as the picker is.
pub(crate) use elements::{layout as layout_elements, stable_id};

// The picker's vocabulary, named for the chat: the favorites the app names,
// the state the surface keeps, and the search field's adapter, which the
// workspace-access panel's filter field is too (D106).
pub use cfg::FavoriteModel;
pub(crate) use cfg::{Geometry, SearchLeaf, SearchPaint, SharedInput};

/// One mark on the rail, the chat's provider id its agent's.
pub(crate) type RailEntry = cfg::RailEntry<AiProviderId>;
/// One row of the list, the chat's provider id its agent's.
#[cfg(test)]
pub(crate) type Row = cfg::Row<AiProviderId>;
/// The picker's own state on the surface. It lives only while the picker
/// is open; opening resets it.
pub(crate) type PickerState = cfg::PickerState<AiProviderId>;

// Names only the suite reads, through `use super::*`; they go when the
// suite moves into the control's crate and drives its public API.
#[cfg(test)]
pub(crate) use cfg::{
    FOOTER_H, FOOTER_TOGGLES_H, LIST_ROWS, MAX_W, PAD, PILL_H, PickerFocus, RAIL_BUTTON,
    RAIL_CONTENT_GAP, RAIL_DIVIDER, RAIL_GAP, RAIL_PAGE, RAIL_W, ROW_H, SEARCH_H, SEGMENT_H,
    STRIP_H, geometry, matches, rail_dividers, rail_height, rail_rects, scroll_into_view, size,
    star_rect, step_highlight, toggled,
};
#[cfg(test)]
use koru_ai_core::ReasoningControl;
#[cfg(test)]
use koru_draw::el::FrameCx;
#[cfg(test)]
use koru_draw::theme;
#[cfg(test)]
use koru_i18n::Lang;
#[cfg(test)]
use winit::keyboard::NamedKey;

/// The note over the strip when a level is chosen during a reply (D70).
const LEVEL_NOTE_KEY: u64 = 0x5049_1004;
const LEVEL_NOTE_HOLD: std::time::Duration = std::time::Duration::from_millis(2200);
const MODEL_NOTE_HOLD: std::time::Duration = std::time::Duration::from_secs(5);

/// **The footer's readout** (§9.3, D76) for the chat's reasoning control
/// and choice, in the control's words.
#[cfg(test)]
pub(crate) fn readout(
    control: &ReasoningControl,
    choice: ReasoningChoice,
    lang: Lang,
) -> Vec<String> {
    cfg::readout(
        effort::levels(control).as_ref(),
        effort::effort(choice),
        lang,
    )
}

/// What the chat works out before it lends the picker its fields: where the
/// picker opens, what the footer offers, whom the rail opens on, a begun
/// chat's built-in provider, and what the choosing applies to.
struct Precomputed {
    report: Option<cfg::report::Report>,
    place: Place,
    footer: FooterOffer,
    preferred: Option<AiProviderId>,
    builtin: Option<Builtin<AiProviderId>>,
    applies: Applies,
}

/// An agent's model as the control takes it, borrowed.
fn record(model: &AgentModelView) -> ModelRecord<'_> {
    ModelRecord {
        id: &model.id,
        group: &model.group,
        name: model.name.as_deref(),
        description: model.description.as_deref(),
        group_name: model.group_name.as_deref(),
        legacy: model.legacy,
        context: model.context.as_ref().map(|window| window.id.as_str()),
    }
}

/// **The control's inputs from the chat's fields**, each borrowed: the
/// agents in the person's order, each with its readiness, whether it is
/// turned off and its listing; the chat's model; an owned conversation's
/// one agent; the favorites; the **+** while the host offers it.
#[allow(clippy::too_many_arguments)]
fn chat_inputs<'a>(
    lang: koru_i18n::Lang,
    settings: &'a Option<ProviderSettingsResponse>,
    transcript: &'a Option<ChatTranscript>,
    agent_models: &'a HashMap<AiProviderId, AgentModels>,
    agent_status: &'a HashMap<AiProviderId, AgentStatus>,
    hidden: &'a HashSet<String>,
    order: &'a [String],
    favorites: &'a [FavoriteModel],
    owned: &'a Option<crate::OwnedConversation>,
    manage: bool,
    pre: Precomputed,
) -> Inputs<'a, AiProviderId> {
    let agents = crate::agents::ordered_views(settings.as_ref(), order)
        .into_iter()
        .map(|view| Agent {
            id: view.id.clone(),
            key: agent_key(&view.id),
            label: &view.label,
            mark: provider_profile_mark(&view.id, true),
            readiness: agent_status
                .get(&view.id)
                .copied()
                .map(crate::agents::readiness),
            hidden: agent_key(&view.id).is_some_and(|key| hidden.contains(key)),
            listing: match agent_models.get(&view.id) {
                Some(AgentModels::Listed(models)) => {
                    Listing::Listed(models.iter().map(record).collect())
                }
                Some(AgentModels::Failed) => Listing::Failed,
                None | Some(AgentModels::Loading) => Listing::Loading,
            },
        })
        .collect();
    Inputs {
        report: pre.report,
        lang,
        agents,
        builtin: pre.builtin,
        current: transcript.as_ref().map(|transcript| {
            let model = &transcript.chat.current_model;
            Current {
                provider: &model.provider,
                model: &model.requested_model,
                effort: effort::effort(model.default_reasoning),
            }
        }),
        applies: pre.applies,
        allowed: owned.as_ref().map(|owned| &owned.provider),
        preferred: pre.preferred,
        favorites,
        manage,
        footer: pre.footer,
        place: pre.place,
    }
}

/// The control's inputs from `$surface`'s fields: a macro, so the borrows
/// are of fields, and the picker's own state can be lent beside them.
macro_rules! inputs_of {
    ($surface:expr, $pre:expr) => {
        chat_inputs(
            $surface.lang,
            &$surface.settings,
            &$surface.transcript,
            &$surface.agent_models,
            &$surface.agent_status,
            &$surface.hidden_agents,
            &$surface.agent_order,
            &$surface.favorite_models,
            &$surface.owned,
            $surface.agent_settings_offered,
            $pre,
        )
    };
}

impl crate::ChatSurface {
    /// What the chat works out for the picker before lending it anything.
    fn picker_precompute(&self) -> Precomputed {
        let begun = self
            .transcript
            .as_ref()
            .is_some_and(|transcript| !transcript.messages.is_empty());
        let builtin = self
            .transcript
            .as_ref()
            .filter(|transcript| {
                !transcript.messages.is_empty()
                    && !is_agent_provider(&transcript.chat.current_model.provider)
            })
            .map(|transcript| {
                let provider = &transcript.chat.current_model.provider;
                let ids = if provider.as_str() == AiProviderId::KORU {
                    vec![AiProviderId::KORU_HOSTED_MODEL.to_owned()]
                } else {
                    self.chat_model_options()
                };
                Builtin {
                    id: provider.clone(),
                    name: provider_name(provider),
                    mark: provider_profile_mark(provider, false),
                    models: ids
                        .into_iter()
                        .map(|id| BuiltinModel {
                            label: builtin_model_label(&id),
                            id,
                        })
                        .collect(),
                }
            });
        Precomputed {
            report: (self.chat_model_open && (begun || self.pending_user.is_some()))
                .then(|| self.context_window_snapshot())
                .flatten()
                .map(|snapshot| snapshot.report(self.lang)),
            place: Place {
                anchor: self.chat_model_button(),
                viewport: self.viewport,
                width: self.composer_hit_rect.w,
            },
            footer: FooterOffer {
                levels: self.effort_control().and_then(effort::levels),
                fast: self.picker_fast().map(|fast| fast.on),
                context: self.picker_context().map(|window| Context {
                    tokens: window.tokens,
                    long: window.chosen_long,
                }),
            },
            // The default agent where one is set (D40), else the last
            // chosen.
            preferred: self.default_agent_provider().or_else(|| {
                self.settings
                    .as_ref()
                    .and_then(|settings| settings.selected_provider.clone())
            }),
            builtin,
            applies: Applies::Conversation {
                begun,
                agent_begun: self.agent_chat_begun(),
                model_locked: self.model_locked(),
                effort_locked: self.effort_locked(),
            },
        }
    }

    /// The picker, read, over the chat's fields as they are.
    fn picker_read<R>(&self, f: impl FnOnce(Picker<'_, '_, AiProviderId>) -> R) -> R {
        let pre = self.picker_precompute();
        let inputs = inputs_of!(self, pre);
        f(Picker::new(&inputs, &self.picker, self.chat_model_open))
    }

    /// The picker, driven, over the chat's fields as they are; what it
    /// queues is left for [`Self::picker_run`] to carry out.
    fn picker_drive<R>(
        &mut self,
        f: impl FnOnce(&mut ModelPicker<'_, '_, AiProviderId>) -> R,
    ) -> R {
        let pre = self.picker_precompute();
        let inputs = inputs_of!(self, pre);
        let mut view = ModelPicker::new(
            &inputs,
            &mut self.picker,
            &mut self.chat_model_open,
            &mut self.picker_tooltip,
            &mut self.engine,
            self.pointer,
        );
        f(&mut view)
    }

    /// **The picker driven, and what the person chose carried out**: the
    /// call, then each outcome in order with the chat's own rules, then —
    /// over the chat as it now is — what the picker still owes, until it
    /// owes nothing. The second half is the host's answer to the last
    /// choice the call made, which is the call's answer.
    fn picker_run<R>(
        &mut self,
        f: impl FnOnce(&mut ModelPicker<'_, '_, AiProviderId>) -> R,
    ) -> (R, Option<bool>) {
        let result = self.picker_drive(f);
        let mut answer = None;
        loop {
            for outcome in self.picker.take_outcomes() {
                if let Some(answered) = self.apply_picker_outcome(outcome) {
                    answer = Some(answered);
                }
            }
            if !self.picker.pending() {
                break;
            }
            self.picker_drive(|view| view.resume());
        }
        (result, answer)
    }

    /// **One choice carried out** the way the chat always has; `Some` with
    /// the chat's answer for a choice that answers the call that made it.
    fn apply_picker_outcome(&mut self, outcome: Outcome<AiProviderId>) -> Option<bool> {
        let answers = outcome.answers();
        let answer = match outcome {
            Outcome::Model { provider, model } => self.choose_agent_model(provider, model),
            Outcome::BuiltinModel(model) => self.change_chat_model(model),
            Outcome::Level(choice) => self.choose_level(effort::reasoning(choice)),
            Outcome::EffortLocked => {
                self.status = Some(t(self.lang, Key::AiEffortLockedAfterStart).to_owned());
                true
            }
            Outcome::Fast(on) => self.choose_fast(on),
            Outcome::Context(long) => self.choose_context(long),
            // The picker closed itself first.
            Outcome::Setup => {
                self.agent_settings_request = true;
                true
            }
            Outcome::Favorites(next) => {
                self.favorite_models = next.clone();
                self.favorite_models_request = Some(next);
                true
            }
            Outcome::Catalog(agent) => {
                self.ask_agent_models(agent);
                true
            }
            Outcome::Refresh(agent) => {
                let sent = self.reader.as_ref().is_some_and(|reader| {
                    reader
                        .send(crate::reader::Command::RefreshModels(agent.clone()))
                        .is_ok()
                });
                if sent {
                    self.picker.refresh_started(agent.clone());
                    self.agent_models_asked.insert(agent);
                }
                true
            }
        };
        answers.then_some(answer)
    }

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
            self.picker_drive(|view| view.settle_highlight());
        }
        moved
    }

    /// A ☆ was pressed since the last ask: the whole list, for the host to
    /// store (roaming) and hand to every other chat.
    pub fn take_favorite_models_request(&mut self) -> Option<Vec<FavoriteModel>> {
        self.favorite_models_request.take()
    }

    /// The favorites the picker lists (D46).
    #[cfg(test)]
    pub(crate) fn picker_favorites(&self) -> Vec<(AiProviderId, String)> {
        self.picker_read(|picker| picker.favorites())
    }

    /// Whether `agent`'s model `model` is starred.
    #[cfg(test)]
    pub(crate) fn is_favorite(&self, agent: &AiProviderId, model: &str) -> bool {
        self.picker_read(|picker| picker.is_favorite(agent, model))
    }

    /// The model a row would star.
    #[cfg(test)]
    pub(crate) fn row_favorite(
        &self,
        entry: &RailEntry,
        row: &Row,
    ) -> Option<(AiProviderId, String)> {
        self.picker_read(|picker| picker.row_favorite(entry, row))
    }

    /// **Star or unstar** `agent`'s `model` (D46).
    #[cfg(test)]
    pub(crate) fn toggle_favorite(&mut self, agent: &AiProviderId, model: &str) -> bool {
        self.picker_run(|view| view.toggle_favorite(agent, model)).0
    }

    /// What the **+** is called for a screen reader and the inspector.
    #[cfg(test)]
    pub(crate) fn picker_manage_name(&self) -> &'static str {
        t(self.lang, Key::AiPickerManageAgents)
    }

    /// The rail's entries ([`cfg::Picker::rail`]).
    pub(crate) fn picker_rail(&self) -> Vec<RailEntry> {
        self.picker_read(|picker| picker.rail())
    }

    /// Whether the rail is hidden: the chat has begun, or it is owned.
    #[cfg(test)]
    pub(crate) fn picker_rail_hidden(&self) -> bool {
        self.picker_read(|picker| picker.rail_hidden())
    }

    /// The marks the rail draws, presses and steps through.
    #[cfg(test)]
    pub(crate) fn picker_marks(&self) -> Vec<RailEntry> {
        self.picker_read(|picker| picker.marks())
    }

    /// Only these marks enter the panel's element tree and hit targets.
    #[cfg(test)]
    pub(crate) fn picker_visible_rail(&self) -> Vec<RailEntry> {
        self.picker_read(|picker| picker.visible_rail())
    }

    /// **The rail's entry on open** (§9.1).
    #[cfg(test)]
    pub(crate) fn picker_initial_entry(&self) -> Option<RailEntry> {
        self.picker_read(|picker| picker.initial_entry())
    }

    /// The entry whose models the list shows.
    #[cfg(test)]
    pub(crate) fn picker_entry(&self) -> Option<RailEntry> {
        self.picker_read(|picker| picker.entry())
    }

    /// Whether `agent` has a model the query finds.
    #[cfg(test)]
    fn agent_matches_query(&self, agent: &AiProviderId, query: &str) -> bool {
        self.picker_read(|picker| picker.agent_matches_query(agent, query))
    }

    /// **The list** (§9.2).
    #[cfg(test)]
    pub(crate) fn picker_rows(&self) -> Vec<Row> {
        self.picker_read(|picker| picker.rows())
    }

    /// Whether row `row` of `entry` is a model its agent marks as older.
    #[cfg(test)]
    fn row_is_legacy(&self, entry: &RailEntry, row: &Row) -> bool {
        self.picker_read(|picker| picker.row_is_legacy(entry, row))
    }

    /// Which rows can be chosen now.
    #[cfg(test)]
    fn picker_choosable(&self, rows: &[Row]) -> Vec<bool> {
        self.picker_read(|picker| picker.choosable(rows))
    }

    /// A row's label.
    #[cfg(test)]
    fn picker_row_label(&self, entry: &RailEntry, row: &Row) -> String {
        self.picker_read(|picker| picker.row_label(entry, row))
    }

    /// **The open picker's frame, with its list** (M1).
    #[cfg(test)]
    fn picker_frame(&self) -> Option<(Geometry, elements::Spec, koru_draw::el::Frame)> {
        self.picker_read(|picker| picker.frame())
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
    pub(crate) fn picker_segment_rect(&self, choice: impl effort::AsEffort) -> Option<Rect> {
        let (_, _, frame) = self.picker_frame()?;
        Some(frame.node(elements::segment_key(choice.as_effort()))?.rect)
    }

    /// Whether a row is the chat's current model.
    #[cfg(test)]
    fn picker_row_current(&self, entry: &RailEntry, row: &Row) -> bool {
        self.picker_read(|picker| picker.row_current(entry, row))
    }

    /// The picker follows the model chip and flips to the side with room.
    pub(crate) fn picker_geometry(&self) -> Option<Geometry> {
        self.picker_read(|picker| picker.geometry())
    }

    /// The list's scrollbar, as the frame draws it (M1: the shared bar).
    #[cfg(test)]
    pub(crate) fn picker_scrollbar(
        &self,
        _geometry: &Geometry,
    ) -> Option<koru_draw::scrollbar::ScrollbarGeom> {
        self.picker_frame()?.2.scrollbar(elements::LIST_KEY)
    }

    /// **Open the picker** on the chat's own entry, the query empty, the
    /// search field focused and the current model in view.
    pub(crate) fn reset_picker(&mut self) {
        self.picker_run(|view| view.reset());
    }

    /// **Show `entry`'s list**: a mark pressed on the rail, or stepped to.
    #[cfg(test)]
    fn select_rail_entry(&mut self, entry: RailEntry) {
        self.picker_run(|view| view.select_rail_entry(entry));
    }

    /// **Choose row `index`**: the chat takes the model, and the picker
    /// stays open (D66), its list settled against the new model's footer.
    #[cfg(test)]
    pub(crate) fn choose_picker_row(&mut self, index: usize) -> bool {
        let (chosen, answer) = self.picker_run(|view| view.choose_row(index));
        answer.unwrap_or(chosen)
    }

    /// **The picker after a model is chosen** (D66): the list kept in range
    /// and the chosen row in view — run too when a chat's model lands from
    /// the service.
    pub(crate) fn settle_after_choice(&mut self) {
        self.picker_drive(|view| view.settle_after_choice());
    }

    /// **↻** (D13): the selected agent's list rebuilt now.
    #[cfg(test)]
    pub(crate) fn refresh_picker_agent(&mut self) -> bool {
        self.picker_run(|view| view.refresh()).0
    }

    #[cfg(test)]
    fn refresh_enabled(&self, agent: &AiProviderId) -> bool {
        self.picker_read(|picker| picker.refresh_enabled(agent))
    }

    /// An answer about `agent`'s models landed: its ↻ is over.
    pub(crate) fn agent_models_answered(&mut self, agent: &AiProviderId) {
        self.picker.refresh_answered(agent);
    }

    /// A press while the picker is open. `None` when the press is not the
    /// picker's — on the chip, which toggles it — and `Some` when the picker
    /// took it, closing it for a press outside.
    pub(crate) fn picker_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        let (handled, answer) = self.picker_run(|view| view.mouse_down(x, y));
        handled.map(|handled| answer.unwrap_or(handled))
    }

    /// The pointer moved with the picker open: a scrollbar drag, and the
    /// tooltip under it. `Some` when the picker claimed the move.
    pub(crate) fn picker_mouse_move(&mut self, x: f32, y: f32) -> Option<bool> {
        self.picker_run(|view| view.mouse_move(x, y, std::time::Instant::now()))
            .0
    }

    #[cfg(test)]
    fn picker_tooltip_at(&self, geometry: &Geometry, x: f32, y: f32) -> Option<TooltipContent> {
        self.picker_read(|picker| picker.tooltip_at(geometry, x, y))
    }

    /// **The pointer over the open picker** (D44).
    pub(crate) fn picker_cursor(&self, x: f32, y: f32) -> Option<winit::window::CursorIcon> {
        self.picker_read(|picker| picker.cursor(x, y))
    }

    /// The wheel over the open picker scrolls its list, a row a notch.
    pub(crate) fn picker_scroll(&mut self, dy: f32) -> bool {
        self.picker_drive(|view| view.scroll(dy))
    }

    /// **The keyboard inside the picker** (§9.5): `None` when it is closed.
    pub(crate) fn picker_key(
        &mut self,
        key: &WinitKey,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        let (handled, answer) = self.picker_run(|view| view.key(key, text, mods));
        handled.map(|handled| answer.unwrap_or(handled))
    }

    /// Where a screen reader's focus is in the open picker, and what its
    /// search field holds: what a host that publishes the picker publishes.
    /// The chat publishes no accessibility tree of its own, so only its
    /// pins read them.
    #[cfg(test)]
    pub(crate) fn picker_accessibility(&self) -> (Option<koru_draw::el::Key>, String) {
        self.picker_read(|picker| (picker.accessibility_focus(), picker.search_text()))
    }

    /// What the open picker's tree declares.
    #[cfg(test)]
    pub(super) fn picker_spec(&self) -> elements::Spec {
        self.picker_read(|picker| picker.spec())
    }

    /// **The footer** (D76), or `None` for a model that offers none of it.
    #[cfg(test)]
    pub(crate) fn picker_footer(&self) -> Option<elements::Footer> {
        self.picker_read(|picker| picker.footer())
    }

    /// Draw the open picker inside the *Popover surface* record.
    pub(crate) fn draw_model_picker(&mut self, canvas: &mut Scene, root: Affine) {
        self.picker_drive(|view| view.draw(canvas, root));
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
        let label = koru_i18n::fill(t(self.lang, Key::AiModelDuringReply), &[name]);
        self.copy_feedback.show_transient(
            TooltipContent::new(LEVEL_NOTE_KEY, anchor, label),
            std::time::Instant::now(),
            MODEL_NOTE_HOLD,
        );
    }

    fn show_next_message_note(&mut self, anchor: Rect, label: String) {
        self.copy_feedback.show_transient(
            TooltipContent::new(LEVEL_NOTE_KEY, anchor, label),
            std::time::Instant::now(),
            LEVEL_NOTE_HOLD,
        );
    }
}

/// A built-in provider's model as its row reads.
fn builtin_model_label(model: &str) -> String {
    if model == AiProviderId::KORU_HOSTED_MODEL {
        AiProviderId::KORU_HOSTED_MODEL_LABEL.to_owned()
    } else {
        model.to_owned()
    }
}

/// The search field itself: the search row short of ↻ at its end.
#[cfg(test)]
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

/// **A provider's mark**: the agent's brand by its kind (`agent-claude`
/// wears Claude's), OpenAI's for OpenAI, a glyph where there is no brand —
/// [`provider_mark`], which a chat tab wears too. What the picker's rail
/// and a favorite's row wear, handed to the control as data.
pub(crate) fn provider_profile_mark(provider: &AiProviderId, agent: bool) -> brand::ProfileMark {
    let glyph = if agent {
        icons::ROBOT
    } else if provider.as_str() == AiProviderId::OLLAMA {
        icons::DESKTOP
    } else {
        icons::SPARKLE
    };
    provider_mark(provider.as_str()).unwrap_or(brand::ProfileMark::Glyph(glyph))
}

/// A provider's mark in `inner` ([`provider_profile_mark`]), drawn as the
/// picker draws it: the context ring wears it too.
pub(crate) fn draw_provider_mark(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    provider: &AiProviderId,
    agent: bool,
    inner: Rect,
    colour: vello::peniko::Color,
) {
    cfg::mark::draw_mark(
        canvas,
        root,
        engine,
        provider_profile_mark(provider, agent),
        inner,
        colour,
    );
}

#[cfg(test)]
mod pins;
#[cfg(test)]
mod tests;
