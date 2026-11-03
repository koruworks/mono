// SPDX-License-Identifier: MPL-2.0

//! **Drawing the open picker** (D64): its element tree ([`super::elements`])
//! laid out with the host's pointer and keyboard on it and painted inside
//! the *Popover surface* record, so everything drawn in the panel nests
//! under the surface in the inspector.
//!
//! **Every leaf is a named adapter** (QUESTIONS M7, approved by the user
//! 2026-11-03): a provider's mark, the search field, ↻, the Fast and
//! Context rows and the level's footer are each a type whose `impl Leaf`
//! runs the drawing its control always had, in the allocation the frame
//! gives it and in the frame's palette, never the global one. This file and
//! the tree are on `ui_scan`'s migrated list.

use koru_ai_core::{AiProviderId, ReasoningChoice, ReasoningControl};
use koru_draw::el::{ElState, FrameCx, Interaction, Leaf};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Line, roles};
use koru_draw::{icons, scene, theme};
use koru_i18n::{Key, Lang, t};
use koru_ui::icon_button::{IconButton, State as IconButtonState};
use koru_ui::popover::PopoverShell;
use vello::Scene;
use vello::kurbo::Affine;

use super::context::{CONTEXT_H, ContextPaint, paint_context_row};
use super::elements::{self, ListRow, MarkName, MarkSpec, Painted, Spec, StarSpec};
use super::fast::{FastPaint, paint_fast_row};
use super::{
    FAST_H, FOOTER_H, PickerFocus, RailEntry, Row, SharedInput, draw_provider_mark, footer_words,
    provider_name, rail_dividers, reset_in,
};
use crate::agents::AgentStatus;
use crate::effort;

impl crate::ChatSurface {
    /// What the open picker's tree declares: its marks — named, the
    /// selected one marked — the **+**, ↻, the list's rows and the rows
    /// under the list.
    pub(super) fn picker_spec(&self) -> Spec {
        let rail = self.picker_rail();
        let dividers = rail_dividers(&rail);
        let selected = self.picker_entry();
        let query = self.picker_query();
        // A query's results span every entry (D67): no mark reads as the
        // list's while one is typed.
        let searching = !query.is_empty();
        let agents = self
            .settings
            .as_ref()
            .map(|settings| settings.agents.as_slice())
            .unwrap_or_default();
        Spec {
            marks: rail
                .iter()
                .enumerate()
                .map(|(index, entry)| MarkSpec {
                    key: elements::mark_key(entry),
                    name: match entry {
                        RailEntry::Favorites => MarkName::Key(Key::AiPickerFavorites),
                        RailEntry::Agent(agent) => {
                            MarkName::Content(crate::agents::agent_label(agents, agent))
                        }
                        RailEntry::Provider(provider) => MarkName::Content(provider_name(provider)),
                    },
                    provider: match entry {
                        RailEntry::Favorites => None,
                        RailEntry::Provider(provider) => Some((provider.clone(), false)),
                        RailEntry::Agent(agent) => Some((agent.clone(), true)),
                    },
                    dim: match entry {
                        RailEntry::Agent(agent) => {
                            self.agent_status.get(agent).copied() != Some(AgentStatus::Ready)
                                || !self.agent_matches_query(agent, &query)
                        }
                        RailEntry::Favorites | RailEntry::Provider(_) => false,
                    },
                    divider_after: dividers.contains(&index),
                    selected: !searching && selected.as_ref() == Some(entry),
                })
                .collect(),
            manage: self.agent_settings_offered,
            refresh: match &selected {
                Some(RailEntry::Agent(agent)) => Some(!self.refresh_enabled(agent)),
                _ => None,
            },
            fast: self.picker_fast().is_some(),
            context: self.picker_context().is_some(),
            levels: self.effort_control().is_some(),
            list: self.picker_list_rows(),
            offset: self.picker.offset,
        }
    }

    /// Every row of the list as it is drawn and pressed (M1).
    fn picker_list_rows(&self) -> Vec<ListRow> {
        let Some(entry) = self.picker_entry() else {
            return Vec::new();
        };
        let rows = self.picker_rows();
        let choosable = self.picker_choosable(&rows);
        let keyboard = matches!(self.picker.focus, PickerFocus::Search | PickerFocus::List);
        let searching = !self.picker_query().is_empty();
        rows.iter()
            .enumerate()
            .map(|(index, row)| {
                let id = super::row_id(&entry, row);
                ListRow {
                    key: id.as_deref().map(elements::row_key),
                    label: self.picker_row_label(&entry, row),
                    words: matches!(row, Row::Heading(_) | Row::Note(_) | Row::EntryHeading(_)),
                    mark: match row {
                        Row::Favorite(agent, _) => Some((agent.clone(), true)),
                        Row::EntryHeading(heading) => heading.provider().map(|provider| {
                            (provider.clone(), matches!(heading, RailEntry::Agent(_)))
                        }),
                        _ => None,
                    },
                    current: self.picker_row_current(&entry, row),
                    can: choosable[index],
                    lit: keyboard && self.picker.highlight == Some(index),
                    star: self.row_favorite(&entry, row).and_then(|(agent, model)| {
                        Some(StarSpec {
                            key: elements::star_key(id.as_deref()?),
                            starred: self.is_favorite(&agent, &model),
                            shown: false,
                        })
                    }),
                    legacy: (*row == Row::LegacyToggle).then_some(self.picker.legacy_open),
                    // Under an expanded *Legacy models* row an older model
                    // needs no tag; found by a search, it has one.
                    tag: (searching && self.row_is_legacy(&entry, row))
                        .then(|| t(self.lang, Key::AiModelsLegacyTag).to_owned()),
                }
            })
            .collect()
    }

    /// What the picker's adapters draw this frame: the search field, the
    /// Fast and Context rows and the level's footer, each with the pointer
    /// and the keyboard on it.
    fn picker_painted(&self) -> Painted {
        let pointer = self.pointer;
        let lang = self.lang;
        let focus = self.picker.focus;
        Painted {
            search: Some(SearchPaint {
                input: self.picker.query.clone(),
                placeholder: t(lang, Key::AiModelSearch).to_owned(),
                focused: focus == PickerFocus::Search,
            }),
            fast: self.picker_fast().map(|fast| FastPaint {
                lang,
                on: fast.on,
                pointer,
                focused: focus == PickerFocus::Fast,
            }),
            context: self.picker_context().map(|choice| ContextPaint {
                lang,
                tokens: choice.tokens,
                chosen_long: choice.chosen_long,
                locked: self.effort_locked(),
                pointer,
                focused: focus == PickerFocus::Context,
            }),
            effort: self.effort_control().cloned().zip(self.effort_shown()).map(
                |(control, shown)| EffortPaint {
                    lang,
                    control,
                    shown,
                    choice: self
                        .transcript
                        .as_ref()
                        .map_or(ReasoningChoice::Default, |transcript| {
                            transcript.chat.current_model.default_reasoning
                        }),
                    locked: self.effort_locked(),
                    pointer,
                    reset_focused: focus == PickerFocus::Reset,
                    slider_focused: focus == PickerFocus::Slider,
                },
            ),
        }
    }

    /// Draw the open picker: the panel, and inside it the rail on its ground,
    /// the search with ↻, the list, and the rows under it.
    pub(crate) fn draw_model_picker(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(geometry) = self.picker_geometry() else {
            return;
        };
        let panel = geometry.panel;
        let mut spec = self.picker_spec();
        // The host's pointer and keyboard, by key: the frame's own target
        // under the pointer, the list while its thumb is dragged, and the
        // selected mark while the rail has the keyboard.
        let targets = elements::layout(
            &elements::tree(&spec, Painted::default()),
            panel,
            FrameCx::root(),
        );
        let hot = targets.hit_at(self.pointer.0, self.pointer.1);
        spec.show_stars(hot);
        let interaction = Interaction {
            hot,
            pressed: self.picker.scroll_drag.map(|_| elements::LIST_KEY),
            focused: spec
                .marks
                .iter()
                .find(|mark| mark.selected)
                .filter(|_| self.picker.focus == PickerFocus::Rail)
                .map(|mark| mark.key),
        };
        let tree = elements::tree(&spec, self.picker_painted());
        let frame = elements::layout(&tree, panel, FrameCx::root().with_interaction(interaction));
        let radius = theme::overlay::popover().radius;
        let engine = &mut self.engine;
        PopoverShell {
            rect: panel,
            radius,
        }
        .draw_containing(canvas, root, |canvas, root| {
            frame.paint(&tree, canvas, root, engine);
        });
    }
}

/// **A provider's mark, as a leaf** (M1): the agent's brand by its kind,
/// OpenAI's for OpenAI, a glyph where there is no brand
/// ([`draw_provider_mark`]) — on the rail inside its mark's square, and at
/// a row's start for a favorite or a search's heading — in an ink role.
pub(crate) struct ProviderMark {
    pub(crate) provider: AiProviderId,
    pub(crate) agent: bool,
    pub(crate) ink: Ink,
    /// How far inside its allocation the mark is drawn.
    pub(crate) inset: f32,
}

impl Leaf for ProviderMark {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, 0.0)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        let inner = Rect::new(
            at.x + self.inset,
            at.y + self.inset,
            (at.w - self.inset * 2.0).max(0.0),
            (at.h - self.inset * 2.0).max(0.0),
        );
        draw_provider_mark(
            scene,
            root,
            engine,
            &self.provider,
            self.agent,
            inner,
            self.ink.of(cx.palette()),
        );
    }
}

/// The search field as it is drawn: its input, shared with the picker
/// (`SharedInput`, QUESTIONS M6), its placeholder and its focus.
pub(crate) struct SearchPaint {
    pub(crate) input: SharedInput,
    pub(crate) placeholder: String,
    pub(crate) focused: bool,
}

/// **The search field** (M7): the picker's `TextInput`, drawn in the
/// allocation the frame gives it; it fills the search box short of ↻.
pub(crate) struct SearchLeaf(pub(crate) Option<SearchPaint>);

impl Leaf for SearchLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, 0.0)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        _state: ElState,
    ) {
        if let Some(search) = &self.0 {
            search
                .input
                .draw(scene, root, engine, at, &search.placeholder, search.focused);
        }
    }
}

/// **↻** (M7): the shared `IconButton`, lit under the pointer; `disabled`
/// while the agent's list is being rebuilt. `None` keeps its room with
/// nothing drawn — a built-in provider has no ↻.
pub(crate) struct RefreshLeaf {
    pub(crate) disabled: Option<bool>,
}

impl Leaf for RefreshLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (elements::REFRESH_W, elements::REFRESH_H)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        state: ElState,
    ) {
        if let Some(disabled) = self.disabled {
            IconButton::new(icons::ARROWS_CLOCKWISE, at)
                .with_state(IconButtonState {
                    hovered: state.hovered,
                    active: false,
                    disabled,
                })
                .draw(scene, root, engine);
        }
    }
}

/// **The Fast row** (koru-ai D11, M7): *Fast · Off | On*.
pub(crate) struct FastLeaf(pub(crate) Option<FastPaint>);

impl Leaf for FastLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, FAST_H)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        if let Some(fast) = &self.0 {
            paint_fast_row(scene, root, engine, at, fast, cx.palette());
        }
    }
}

/// **The Context row** (koru-ai D10, M7): *Context · Auto | 1M*.
pub(crate) struct ContextLeaf(pub(crate) Option<ContextPaint>);

impl Leaf for ContextLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, CONTEXT_H)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        if let Some(context) = &self.0 {
            paint_context_row(scene, root, engine, at, context, cx.palette());
        }
    }
}

/// The level's footer as it is drawn.
pub(crate) struct EffortPaint {
    pub(crate) lang: Lang,
    pub(crate) control: ReasoningControl,
    /// The level the slider shows.
    pub(crate) shown: ReasoningChoice,
    /// The chat's own choice, which may be *Default*.
    pub(crate) choice: ReasoningChoice,
    pub(crate) locked: bool,
    pub(crate) pointer: (f32, f32),
    pub(crate) reset_focused: bool,
    pub(crate) slider_focused: bool,
}

/// **The level's footer** (§9.3, M7): a rule above it, the level's word and
/// ↺ on one line, the stepped slider under it.
pub(crate) struct EffortLeaf(pub(crate) Option<EffortPaint>);

impl Leaf for EffortLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, FOOTER_H)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        footer: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        let Some(effort) = &self.0 else {
            return;
        };
        let palette = cx.palette();
        scene::fill(
            scene,
            root,
            Line::Divider.of(palette),
            &scene::rrect(footer.x, footer.y, footer.w, 1.0, 0.5),
        );
        let (word, suffix) = footer_words(&effort.control, effort.choice, effort.lang);
        let line = engine.simple_layout(&word, roles::ROW.style());
        let label_y = footer.y + 6.0;
        scene::draw_layout_first_line(
            scene,
            root,
            &line,
            footer.x + 8.0,
            label_y,
            Some(if effort.locked {
                Ink::Secondary.of(palette)
            } else {
                Ink::Primary.of(palette)
            }),
        );
        if let Some(suffix) = suffix {
            let dim = engine.simple_layout(&suffix, roles::META.style());
            scene::draw_layout_first_line(
                scene,
                root,
                &dim,
                footer.x + 8.0 + line.width() + 6.0,
                label_y + (line.height() - dim.height()) * 0.5,
                Some(Ink::Secondary.of(palette)),
            );
        }
        let reset = reset_in(footer);
        IconButton::new(icons::RESET, reset)
            .with_state(IconButtonState {
                hovered: reset.contains(effort.pointer.0, effort.pointer.1),
                active: effort.reset_focused,
                disabled: effort.locked || effort.choice == ReasoningChoice::Default,
            })
            .draw(scene, root, engine);
        effort::draw_track(
            scene,
            root,
            engine,
            effort.lang,
            footer,
            &effort.control,
            effort.shown,
            if effort.locked {
                (-1.0, -1.0)
            } else {
                effort.pointer
            },
            effort.slider_focused,
        );
    }
}
