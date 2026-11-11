// SPDX-License-Identifier: MPL-2.0

//! **The Bot's agent and model** (Bots P1.8 S7): the settings' *Agent and
//! model* card hosts the agent-configuration control
//! (`koru_ai_ui::agent_catalog::config`) — the model picker under the
//! model's chip, the approval-mode list under the mode's — fed from the
//! tab's catalog ([`crate::bots::catalog`]) and the Bot's draft, and folds
//! what the person chose into that draft ([`crate::bots::settings`]).
//! Nothing is sent until *Save*, which is an `Update` of the configuration
//! at the revision the tab read.
//!
//! **What the choosing applies to is a Bot's next task**
//! (`Applies::NextTask`): nothing is locked for having begun, and the card
//! says the defaults apply to new conversations (QUESTIONS O23). **The
//! agent is fixed after creation** ([`AGENT_FIXED_AFTER_CREATION`], O42):
//! the control is told only the Bot's agent may be offered, so it shows it
//! and hides the rail.
//!
//! **The create form hosts the same control** (S8) over the new Bot's
//! draft, with no agent fixed: every registered agent runtime is on the
//! rail and a turned-off one is not, favorites are offered (QUESTIONS O42),
//! and choosing another agent starts on its own model at its defaults.
//!
//! **Hosted as the page picker is** (`picker.rs`): while a popover is up it
//! takes the keys, the presses, the pointer and the wheel first, it paints
//! last, and the control keeps it inside the tab.

use koru_ai_core::AiProviderId;
use koru_ai_ui::agent_catalog::config::modes::{self, ModeList, ModeRow};
use koru_ai_ui::agent_catalog::config::picker::{
    Applies, Current, FavoriteModel, FooterOffer, Inputs, ModelPicker, Outcome, Picker,
    PickerState, Place,
};
use koru_ai_ui::agent_catalog::{self, CatalogListing};
use koru_draw::el::Key;
use koru_draw::geometry::Rect;
use koru_ui::tooltip::Tooltip;
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

use super::BotView;
use super::region::{AGENT_CARD, MODE_CHIP, MODEL_CHIP};
use crate::bots::catalog::{AgentSettings, Catalog};
use crate::bots::settings::ModelDraft;

/// **Whether a Bot's agent stays the one it was created with** (QUESTIONS
/// O42, recommended and built): the card shows the agent and offers no
/// other. A runtime change would touch the folder, the modes and every
/// conversation's handoff, so it is not offered in this version. One
/// constant, so offering it is a one-line change.
pub(crate) const AGENT_FIXED_AFTER_CREATION: bool = true;

/// The widest the picker opens, as in the chat (`picker::MAX_W`).
const PICKER_MAX_W: f32 = koru_ai_ui::agent_catalog::config::picker::MAX_W;

/// **The card's popovers**, while either is up: the model picker's lent
/// state, its open flag and tooltip, and the mode list's open flag and
/// keyboard row. Boxed in the tab, which is moved by value.
pub(crate) struct AgentPopover {
    pub(super) state: PickerState<AiProviderId>,
    pub(super) picker_open: bool,
    pub(super) tooltip: Tooltip,
    pub(super) modes_open: bool,
    pub(super) mode_focus: Option<usize>,
    pub(super) pointer: (f32, f32),
}

/// What the control is handed for one call: the inputs, built from the
/// tab's fields, which outlive it.
fn inputs<'a>(
    lang: koru_i18n::Lang,
    catalog: &'a Catalog,
    settings: &'a AgentSettings,
    provider: &'a AiProviderId,
    draft: &'a ModelDraft,
    allowed: Option<&'a AiProviderId>,
    place: Place,
) -> Inputs<'a, AiProviderId> {
    let footer = catalog
        .listing(provider)
        .map_or_else(FooterOffer::default, |listing| {
            listing.footer(&draft.model.requested_model, draft.fast)
        });
    Inputs {
        lang,
        agents: catalog.control_agents(settings),
        builtin: None,
        current: Some(Current {
            provider: &draft.model.provider,
            model: &draft.model.requested_model,
            effort: agent_catalog::effort(draft.model.default_reasoning),
        }),
        applies: Applies::NextTask,
        allowed,
        preferred: None,
        favorites: &settings.favorites,
        // *Manage agent providers* is not routed from a Bot tab yet: the
        // row that would open it is not offered, and the agent's readiness
        // still says what is wrong.
        manage: false,
        footer,
        place,
    }
}

impl BotView {
    /// The Bot's agent, while the tab knows it — or, in create mode (S8),
    /// the agent the new Bot's draft is on.
    pub(super) fn agent_provider(&self) -> Option<AiProviderId> {
        match self.create.as_deref() {
            Some(create) => create.form.model_shown().map(|draft| draft.model.provider),
            None => self.facts.as_ref().map(|facts| facts.provider.clone()),
        }
    }

    /// **The agent-and-model draft the control chooses for**: the settings'
    /// over what the owner holds, or the create form's (S8).
    pub(super) fn agent_draft(&self) -> Option<ModelDraft> {
        match self.create.as_deref() {
            Some(create) => create.form.model_shown(),
            None => self.settings.model_shown(),
        }
    }

    /// The draft after a choice, into whichever form it is.
    fn edit_agent_draft(&mut self, draft: ModelDraft) {
        match self.create.as_deref_mut() {
            Some(create) => create.form.edit_model(draft),
            None => self.settings.edit_model(draft),
        }
    }

    /// **Whether only the Bot's own agent may be offered**: once it exists
    /// ([`AGENT_FIXED_AFTER_CREATION`]). In the create form any registered
    /// runtime is, because that is where the agent is chosen (S8,
    /// QUESTIONS O33).
    fn agent_allowed(&self) -> bool {
        self.create.is_none() && AGENT_FIXED_AFTER_CREATION
    }

    /// The listing of the Bot's agent, while the catalog holds one.
    pub(super) fn agent_listing(&self) -> Option<&CatalogListing> {
        self.catalog.listing(&self.agent_provider()?)
    }

    /// Whether either popover is up.
    pub(super) fn agent_popover_open(&self) -> bool {
        self.agent
            .as_deref()
            .is_some_and(|popover| popover.picker_open || popover.modes_open)
    }

    /// Where the model picker opens: under the model's chip, inside the
    /// tab, as wide as the card up to the chat's width.
    fn picker_place(&self) -> Place {
        let (anchor, card) = self.with_frame(|_, frame| {
            (
                frame.node(MODEL_CHIP).map(|node| node.rect),
                frame.node(AGENT_CARD).map(|node| node.rect),
            )
        });
        let zero = Rect::new(0.0, 0.0, 0.0, 0.0);
        Place {
            anchor: anchor.unwrap_or(zero),
            viewport: Rect::new(0.0, 0.0, self.size.0, self.size.1),
            width: card.map_or(PICKER_MAX_W, |card| card.w.min(PICKER_MAX_W)),
        }
    }

    /// **The picker, read**, over the tab's catalog and draft.
    pub(super) fn agent_picker_read<R>(
        &self,
        f: impl FnOnce(Picker<'_, '_, AiProviderId>) -> R,
    ) -> Option<R> {
        let popover = self.agent.as_deref()?;
        let provider = self.agent_provider()?;
        let draft = self.agent_draft()?;
        let place = self.picker_place();
        let inputs = inputs(
            self.lang,
            &self.catalog,
            &self.agent_settings,
            &provider,
            &draft,
            self.agent_allowed().then_some(&provider),
            place,
        );
        Some(f(Picker::new(&inputs, &popover.state, popover.picker_open)))
    }

    /// **The picker driven, and what the person chose folded into the
    /// draft**: the call, then each outcome in order, then — over the draft
    /// as it now is — what the picker still owes. `None` when the tab has
    /// no Bot or no draft to choose for.
    pub(super) fn agent_picker_run<R>(
        &mut self,
        f: impl FnOnce(&mut ModelPicker<'_, '_, AiProviderId>) -> R,
    ) -> Option<R> {
        let provider = self.agent_provider()?;
        let place = self.picker_place();
        let allowed = self.agent_allowed();
        let result = {
            let draft = self.agent_draft()?;
            let popover = self.agent.as_deref_mut()?;
            let inputs = inputs(
                self.lang,
                &self.catalog,
                &self.agent_settings,
                &provider,
                &draft,
                allowed.then_some(&provider),
                place,
            );
            crate::input_engine::with(|engine| {
                let mut view = ModelPicker::new(
                    &inputs,
                    &mut popover.state,
                    &mut popover.picker_open,
                    &mut popover.tooltip,
                    engine,
                    popover.pointer,
                );
                f(&mut view)
            })
        };
        loop {
            let outcomes = self
                .agent
                .as_deref_mut()
                .map(|popover| popover.state.take_outcomes())
                .unwrap_or_default();
            for outcome in outcomes {
                let Some(current) = self.agent_provider() else {
                    break;
                };
                self.apply_agent_outcome(&current, outcome);
            }
            let pending = self
                .agent
                .as_deref()
                .is_some_and(|popover| popover.state.pending());
            if !pending {
                break;
            }
            // A choice may have moved the draft to another agent (S8).
            let provider = self.agent_provider()?;
            let draft = self.agent_draft()?;
            let popover = self.agent.as_deref_mut()?;
            let inputs = inputs(
                self.lang,
                &self.catalog,
                &self.agent_settings,
                &provider,
                &draft,
                allowed.then_some(&provider),
                place,
            );
            crate::input_engine::with(|engine| {
                ModelPicker::new(
                    &inputs,
                    &mut popover.state,
                    &mut popover.picker_open,
                    &mut popover.tooltip,
                    engine,
                    popover.pointer,
                )
                .resume();
            });
        }
        // The control closed itself (Esc, a press outside): the keyboard
        // goes back to the chip.
        if self
            .agent
            .as_deref()
            .is_some_and(|popover| !popover.picker_open && !popover.modes_open)
        {
            self.agent = None;
            self.focus = Some(MODEL_CHIP);
        }
        Some(result)
    }

    /// **One choice folded into the draft**, by the draft's own rules: the
    /// Bot's agent only; a model, its level, Fast or its longer window; the
    /// listing asked for; ↻; the starred models kept for the window to
    /// store. Nothing is sent.
    fn apply_agent_outcome(&mut self, provider: &AiProviderId, outcome: Outcome<AiProviderId>) {
        let Some(mut draft) = self.agent_draft() else {
            return;
        };
        match outcome {
            Outcome::Model {
                provider: chosen,
                model,
            } => {
                if self.agent_allowed() && chosen != *provider {
                    return;
                }
                // Another agent, in the create form (S8): its own model, at
                // its own default level, mode and Fast; nothing of the
                // previous agent's carries over.
                if chosen != *provider {
                    self.catalog.want(&chosen);
                    draft.model.default_reasoning = agent_catalog::reasoning(
                        koru_ai_ui::agent_catalog::config::effort::Effort::Default,
                    );
                    draft.mode = None;
                    draft.fast = None;
                }
                // A model with no Fast takes no Fast setting.
                let offers_fast = self
                    .catalog
                    .listing(&chosen)
                    .and_then(|listing| listing.find(&model))
                    .is_some_and(|(listed, _)| listed.fast.is_some());
                draft.model.provider = chosen;
                draft.model.requested_model = model;
                if !offers_fast {
                    draft.fast = None;
                }
            }
            Outcome::Level(effort) => {
                draft.model.default_reasoning = agent_catalog::reasoning(effort);
            }
            Outcome::Fast(on) => draft.fast = Some(on),
            Outcome::Context(long) => {
                let Some((listed, _)) = self
                    .catalog
                    .listing(provider)
                    .and_then(|listing| listing.find(&draft.model.requested_model))
                else {
                    return;
                };
                draft.model.requested_model = match (&listed.context, long) {
                    (Some((long_id, _)), true) => long_id.clone(),
                    _ => listed.id.clone(),
                };
            }
            Outcome::Catalog(agent) => {
                self.catalog.want(&agent);
                return;
            }
            Outcome::Refresh(agent) => {
                self.catalog.refresh(&agent);
                if let Some(popover) = self.agent.as_deref_mut() {
                    popover.state.refresh_started(agent);
                }
                return;
            }
            Outcome::Favorites(next) => {
                self.agent_settings.favorites = next.clone();
                self.favorites_request = Some(next);
                return;
            }
            // Not offered to a Bot: no built-in provider, nothing locked,
            // no agent settings from here.
            Outcome::BuiltinModel(_) | Outcome::EffortLocked | Outcome::Setup => return,
        }
        self.edit_agent_draft(draft);
    }

    /// **The starred models after a ☆ in the card's picker**, for the
    /// window to store (they roam) and hand to every other surface. Taken
    /// once.
    pub(crate) fn take_favorite_models_request(&mut self) -> Option<Vec<FavoriteModel>> {
        self.favorites_request.take()
    }

    /// The ↻s the catalog answered, for the picker's *Checking…*.
    pub(super) fn settle_agent_refreshes(&mut self) {
        let answered = self.catalog.take_refreshed();
        if let Some(popover) = self.agent.as_deref_mut() {
            for agent in &answered {
                popover.state.refresh_answered(agent);
            }
        }
    }

    fn new_popover() -> Box<AgentPopover> {
        Box::new(AgentPopover {
            state: crate::input_engine::with(|engine| PickerState::new(engine)),
            picker_open: false,
            tooltip: Tooltip::default(),
            modes_open: false,
            mode_focus: None,
            pointer: (0.0, 0.0),
        })
    }

    /// **The model's chip**: the picker opens under it, on the Bot's agent.
    pub(super) fn open_agent_picker(&mut self) -> bool {
        if self.agent_draft().is_none() {
            return false;
        }
        if let Some(provider) = self.agent_provider() {
            self.catalog.want(&provider);
        }
        let mut popover = Self::new_popover();
        popover.picker_open = true;
        self.agent = Some(popover);
        self.agent_picker_run(|view| view.reset());
        true
    }

    /// The mode list's rows: the Bot's agent's modes in Koru's words.
    fn mode_rows(&self) -> Vec<ModeRow> {
        let Some(provider) = self.agent_provider() else {
            return Vec::new();
        };
        self.catalog
            .listing(&provider)
            .map(|listing| modes::rows(self.lang, provider.as_str(), &listing.offers()))
            .unwrap_or_default()
    }

    /// The mode the draft names, else the one the agent says it starts in.
    pub(super) fn mode_shown(&self) -> Option<String> {
        if let Some(mode) = self.agent_draft().and_then(|draft| draft.mode) {
            return Some(mode);
        }
        // A new Bot starts in its runtime's asking mode, never the person's
        // own default permission (S8, QUESTIONS O42).
        if self.create.is_some() {
            return self.asking_mode();
        }
        self.agent_listing()?.default_mode.clone()
    }

    /// **The mode's chip**: the list opens under it, the keyboard on the
    /// current mode.
    pub(super) fn open_agent_modes(&mut self) -> bool {
        let rows = self.mode_rows();
        if rows.is_empty() {
            return false;
        }
        let current = self.mode_shown();
        let mut popover = Self::new_popover();
        popover.modes_open = true;
        popover.mode_focus = Some(
            rows.iter()
                .position(|row| Some(row.id.as_str()) == current.as_deref())
                .unwrap_or(0),
        );
        self.agent = Some(popover);
        true
    }

    /// The open mode list, laid out by the control under the mode's chip.
    fn with_mode_list<R>(&self, f: impl FnOnce(&ModeList<'_>) -> R) -> Option<R> {
        let popover = self.agent.as_deref().filter(|popover| popover.modes_open)?;
        let anchor = self.with_frame(|_, frame| frame.node(MODE_CHIP).map(|node| node.rect))?;
        let rows = self.mode_rows();
        let current = self.mode_shown();
        let list = ModeList {
            lang: self.lang,
            rows: &rows,
            current: current.as_deref(),
            focus: popover.mode_focus,
            anchor,
            viewport: Rect::new(0.0, 0.0, self.size.0, self.size.1),
            width: anchor.w.max(PICKER_MAX_W),
            pointer: popover.pointer,
            footer: false,
        };
        Some(f(&list))
    }

    /// A mode chosen: into the draft, and the list closes.
    fn choose_agent_mode(&mut self, index: usize) -> bool {
        let Some(row) = self.mode_rows().into_iter().nth(index) else {
            return false;
        };
        if let Some(mut draft) = self.agent_draft() {
            draft.mode = Some(row.id);
            self.edit_agent_draft(draft);
        }
        self.close_agent_popover(MODE_CHIP);
        true
    }

    fn close_agent_popover(&mut self, back: Key) {
        self.agent = None;
        self.focus = Some(back);
    }

    /// **A key while a popover is up**: all of them are its.
    pub(super) fn agent_key(
        &mut self,
        key: &WinitKey,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        let modes = self
            .agent
            .as_deref()
            .is_some_and(|popover| popover.modes_open);
        if !modes {
            return self
                .agent_picker_run(|view| view.key(key, text, mods))
                .flatten()
                .unwrap_or(true);
        }
        let count = self.mode_rows().len();
        let Some(popover) = self.agent.as_deref_mut() else {
            return false;
        };
        let at = popover.mode_focus.unwrap_or(0);
        match key {
            WinitKey::Named(NamedKey::Escape) => self.close_agent_popover(MODE_CHIP),
            WinitKey::Named(NamedKey::ArrowDown) => {
                popover.mode_focus = Some((at + 1).min(count.saturating_sub(1)));
            }
            WinitKey::Named(NamedKey::ArrowUp) => popover.mode_focus = Some(at.saturating_sub(1)),
            WinitKey::Named(NamedKey::Enter | NamedKey::Space) => {
                return self.choose_agent_mode(at);
            }
            _ => {}
        }
        true
    }

    /// **A press while a popover is up**: inside it, the popover's; outside
    /// it, it closes.
    pub(super) fn agent_press(&mut self, x: f32, y: f32) -> bool {
        if let Some(popover) = self.agent.as_deref_mut() {
            popover.pointer = (x, y);
        }
        if self
            .agent
            .as_deref()
            .is_some_and(|popover| popover.picker_open)
        {
            return self
                .agent_picker_run(|view| view.mouse_down(x, y))
                .flatten()
                .unwrap_or(true);
        }
        let hit = self.with_mode_list(|list| (list.panel().contains(x, y), list.row_at(x, y)));
        match hit {
            Some((true, Some(index))) => self.choose_agent_mode(index),
            Some((true, None)) => true,
            _ => {
                self.close_agent_popover(MODE_CHIP);
                true
            }
        }
    }

    pub(super) fn agent_move(&mut self, x: f32, y: f32) -> bool {
        let Some(popover) = self.agent.as_deref_mut() else {
            return false;
        };
        popover.pointer = (x, y);
        if popover.picker_open {
            return self
                .agent_picker_run(|view| view.mouse_move(x, y, std::time::Instant::now()))
                .flatten()
                .unwrap_or(false);
        }
        true
    }

    pub(super) fn agent_release(&mut self) -> bool {
        self.agent.as_deref_mut().is_some_and(|popover| {
            let held = popover.state.dragging();
            popover.state.release_drag();
            held
        })
    }

    pub(super) fn agent_scroll(&mut self, delta: MouseScrollDelta) {
        if !self
            .agent
            .as_deref()
            .is_some_and(|popover| popover.picker_open)
        {
            return;
        }
        // In rows, as the chat's picker takes the wheel.
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, lines) => lines,
            MouseScrollDelta::PixelDelta(position) => (position.y as f32 / 30.0).clamp(-4.0, 4.0),
        };
        self.agent_picker_run(|view| view.scroll(dy));
    }

    /// Paint the popover that is up over the region, last.
    pub(super) fn paint_agent_popover(&mut self, scene: &mut Scene, root: Affine) {
        if self
            .agent
            .as_deref()
            .is_some_and(|popover| popover.picker_open)
        {
            self.agent_picker_run(|view| view.draw(scene, root));
            return;
        }
        // The list is laid out first (the tab's frame names its chip), then
        // drawn on the window's text engine: never one inside the other.
        self.with_mode_list(|list| {
            crate::input_engine::with(|engine| list.draw(scene, root, engine));
        });
    }

    /// **The popover's nodes for the accessibility tree**, in the tab's
    /// coordinates: the picker's frame's semantics or the mode list's, and
    /// where its focus is.
    pub(super) fn agent_semantics(
        &self,
    ) -> Option<(Vec<koru_draw::el::SemNode>, Option<Key>, Option<String>)> {
        let popover = self.agent.as_deref()?;
        if popover.picker_open {
            return self.agent_picker_read(|picker| {
                let nodes = picker
                    .frame()
                    .map(|(_, _, frame)| frame.semantics())
                    .unwrap_or_default();
                (
                    nodes,
                    picker.accessibility_focus(),
                    Some(picker.search_text()),
                )
            });
        }
        self.with_mode_list(|list| (list.frame().1.semantics(), list.focused_key(), None))
    }
}

/// The parts of the control's model-keyed nodes — a rail mark, a row, a
/// heading, a mode — which the window maps back to a Bot tab's key.
pub(crate) const MODEL_PARTS: &[&str] = &["agent", "provider", "row", "words", "approval-mode"];

/// **A control node as the Bot tab publishes it**: a surface key under the
/// tab's own surface, by its part; a model key as it is.
pub(crate) fn published_key(key: Key) -> Key {
    match key.object {
        koru_draw::el::Object::Surface(_) => Key::surface(super::SURFACE, key.part),
        koru_draw::el::Object::Model(_) => key,
    }
}

/// **One of the control's semantic nodes as the tab's [`super::Control`]**,
/// its children under it: the search field with what is typed as its
/// value, the focus where the control says it is.
pub(crate) fn control_of(
    node: &koru_draw::el::SemNode,
    focus: Option<Key>,
    search: Option<&str>,
    lang: koru_i18n::Lang,
) -> super::Control {
    use super::ControlRole;
    use koru_draw::el::{Name, Role};
    let role = match node.role {
        Role::Button => ControlRole::Button,
        Role::List => ControlRole::List,
        Role::ListItem | Role::MenuItem => ControlRole::ListItem,
        Role::TextInput => ControlRole::TextInput,
        Role::Group | Role::Menu => ControlRole::Group,
        Role::Heading | Role::Image => ControlRole::Label,
    };
    let key = node.key.unwrap_or(MODEL_CHIP);
    super::Control {
        key: published_key(key),
        rect: node.bounds,
        role,
        label: match &node.name {
            Some(Name::Key(words)) => koru_i18n::t(lang, *words).to_owned(),
            Some(Name::Text(words)) => words.clone(),
            None => String::new(),
        },
        value: (role == ControlRole::TextInput).then(|| search.unwrap_or("").to_owned()),
        selected: node.selected,
        disabled: node.disabled,
        focused: node.key.is_some() && node.key == focus,
        children: node
            .children
            .iter()
            .map(|child| control_of(child, focus, search, lang))
            .collect(),
    }
}

impl BotView {
    /// **What an assistive technology asked of the card's popover**: a
    /// focus is taken where it is; a click acts on the control by its key,
    /// through that control's own choose path — a model row, a rail mark, a
    /// level's segment, Fast and the context window, ↻, a ☆, a mode row —
    /// never by a press at its centre, which for a control outside the
    /// popover's visible part lands outside it and closes it. A control
    /// drawn inert does nothing. `None` when no popover is up or the key is
    /// not one of its.
    pub(super) fn agent_accessibility_action(&mut self, key: Key, click: bool) -> Option<bool> {
        let (nodes, _, _) = self.agent_semantics()?;
        fn find(nodes: &[koru_draw::el::SemNode], key: Key) -> Option<(Key, bool)> {
            nodes.iter().find_map(|node| match node.key {
                Some(own) if published_key(own) == key => Some((own, node.disabled)),
                _ => find(&node.children, key),
            })
        }
        // The control's own key: the tab publishes a surface key under its
        // own surface ([`published_key`]).
        let (own, disabled) = find(&nodes, key)?;
        if !click {
            return Some(true);
        }
        if disabled {
            return Some(false);
        }
        if self
            .agent
            .as_deref()
            .is_some_and(|popover| popover.picker_open)
        {
            return Some(
                self.agent_picker_run(|view| view.act(own))
                    .flatten()
                    .unwrap_or(false),
            );
        }
        // The mode list: a mode chosen by its key.
        let index = self.mode_rows().iter().position(|row| {
            koru_ai_ui::agent_catalog::config::modes::elements::row_key(&row.id) == own
        });
        Some(match index {
            Some(index) => self.choose_agent_mode(index),
            None => false,
        })
    }
}
