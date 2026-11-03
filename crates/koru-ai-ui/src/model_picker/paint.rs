// SPDX-License-Identifier: MPL-2.0

//! **Drawing the open picker** (D64): its element tree ([`super::elements`])
//! laid out with the host's pointer and keyboard on it and painted inside
//! the *Popover surface* record, so everything drawn in the panel nests
//! under the surface in the inspector. Each leaf runs the drawing its
//! control always had, in the allocation the frame gives it.

use koru_ai_core::{AiProviderId, ReasoningChoice, ReasoningControl};
use koru_draw::el::{ElState, FrameCx, Interaction, Leaf};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::Ink;
use koru_draw::{icons, scene, theme};
use koru_i18n::{Key, Lang, t};
use koru_ui::icon_button::{IconButton, State as IconButtonState};
use koru_ui::popover::PopoverShell;
use vello::Scene;
use vello::kurbo::Affine;

use super::context::{ContextPaint, paint_context_row};
use super::elements::{self, ListRow, MarkName, MarkSpec, Paint, Part, Spec, StarSpec};
use super::fast::{FastPaint, paint_fast_row};
use super::{
    PickerFocus, RailEntry, Row, draw_provider_mark, footer_words, provider_name, rail_dividers,
    reset_in,
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
            &elements::tree(&spec, &mut |_| None),
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
        let tree = elements::tree(&spec, &mut |part| self.picker_painter(part));
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

    /// The drawing `part` delegates to, holding what it shows; `None` where
    /// the part draws nothing now.
    fn picker_painter(&self, part: Part) -> Option<Paint> {
        let pointer = self.pointer;
        let lang = self.lang;
        match part {
            Part::Search => {
                let input = self.picker.query.clone();
                let placeholder = t(lang, Key::AiModelSearch).to_owned();
                let focused = self.picker.focus == PickerFocus::Search;
                painter(move |canvas, root, engine, at, _| {
                    input.draw(canvas, root, engine, at, &placeholder, focused)
                })
            }
            Part::Refresh => {
                let Some(RailEntry::Agent(agent)) = self.picker_entry() else {
                    return None;
                };
                let disabled = !self.refresh_enabled(&agent);
                painter(move |canvas, root, engine, at, state| {
                    IconButton::new(icons::ARROWS_CLOCKWISE, at)
                        .with_state(IconButtonState {
                            hovered: state.hovered,
                            active: false,
                            disabled,
                        })
                        .draw(canvas, root, engine)
                })
            }
            Part::Fast => {
                let fast = FastPaint {
                    lang,
                    on: self.picker_fast()?.on,
                    pointer,
                    focused: self.picker.focus == PickerFocus::Fast,
                };
                painter(move |canvas, root, engine, at, _| {
                    paint_fast_row(canvas, root, engine, at, &fast)
                })
            }
            Part::Context => {
                let choice = self.picker_context()?;
                let context = ContextPaint {
                    lang,
                    tokens: choice.tokens,
                    chosen_long: choice.chosen_long,
                    locked: self.effort_locked(),
                    pointer,
                    focused: self.picker.focus == PickerFocus::Context,
                };
                painter(move |canvas, root, engine, at, _| {
                    paint_context_row(canvas, root, engine, at, &context)
                })
            }
            Part::Effort => {
                let effort = EffortPaint {
                    lang,
                    control: self.effort_control().cloned()?,
                    shown: self.effort_shown()?,
                    choice: self
                        .transcript
                        .as_ref()
                        .map_or(ReasoningChoice::Default, |transcript| {
                            transcript.chat.current_model.default_reasoning
                        }),
                    locked: self.effort_locked(),
                    pointer,
                    reset_focused: self.picker.focus == PickerFocus::Reset,
                    slider_focused: self.picker.focus == PickerFocus::Slider,
                };
                painter(move |canvas, root, engine, at, _| {
                    paint_effort(canvas, root, engine, at, &effort)
                })
            }
        }
    }
}

/// A leaf's painter, its closure typed by the bound — `&mut` arguments for
/// any lifetime, as [`Paint`] takes them.
fn painter(
    paint: impl Fn(&mut Scene, Affine, &mut TextEngine, Rect, ElState) + 'static,
) -> Option<Paint> {
    Some(Box::new(paint))
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

/// The level's footer as it is drawn.
struct EffortPaint {
    lang: Lang,
    control: ReasoningControl,
    /// The level the slider shows.
    shown: ReasoningChoice,
    /// The chat's own choice, which may be *Default*.
    choice: ReasoningChoice,
    locked: bool,
    pointer: (f32, f32),
    reset_focused: bool,
    slider_focused: bool,
}

/// The footer in `footer`: a rule above it, the level's word and ↺ on one
/// line, the slider under it.
fn paint_effort(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    footer: Rect,
    effort: &EffortPaint,
) {
    let p = theme::p();
    scene::fill(
        canvas,
        root,
        p.panel_border,
        &scene::rrect(footer.x, footer.y, footer.w, 1.0, 0.5),
    );
    let (word, suffix) = footer_words(&effort.control, effort.choice, effort.lang);
    let line = engine.simple_layout(&word, theme::role::row());
    let label_y = footer.y + 6.0;
    scene::draw_layout_first_line(
        canvas,
        root,
        &line,
        footer.x + 8.0,
        label_y,
        Some(if effort.locked { p.text_dim } else { p.text }),
    );
    if let Some(suffix) = suffix {
        let dim = engine.simple_layout(&suffix, theme::role::meta());
        scene::draw_layout_first_line(
            canvas,
            root,
            &dim,
            footer.x + 8.0 + line.width() + 6.0,
            label_y + (line.height() - dim.height()) * 0.5,
            Some(p.text_dim),
        );
    }
    let reset = reset_in(footer);
    IconButton::new(icons::RESET, reset)
        .with_state(IconButtonState {
            hovered: reset.contains(effort.pointer.0, effort.pointer.1),
            active: effort.reset_focused,
            disabled: effort.locked || effort.choice == ReasoningChoice::Default,
        })
        .draw(canvas, root, engine);
    effort::draw_track(
        canvas,
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
