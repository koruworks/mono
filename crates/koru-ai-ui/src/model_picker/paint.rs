// SPDX-License-Identifier: MPL-2.0

//! **Drawing the open picker** (D64): its element tree ([`super::elements`])
//! laid out with the host's pointer and keyboard on it and painted inside
//! the *Popover surface* record, so everything drawn in the panel nests
//! under the surface in the inspector.
//!
//! **Every leaf is a named adapter** (QUESTIONS M7, approved by the user
//! 2026-11-03): a provider's mark, the search field, ↻, the effort strip's
//! segments, the pill toggles and the readout (D76) are each a type whose
//! `impl Leaf` draws its control in the allocation the frame gives it and
//! in the frame's palette, never the global one. This file and the tree are
//! on `ui_scan`'s migrated list.

use koru_ai_core::{AiProviderId, ReasoningChoice};
use koru_draw::el::{ElState, FrameCx, Interaction, Leaf};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{FontWeight, IconSize, Ink, Line, Surface, roles};
use koru_draw::{icons, scene, theme};
use koru_i18n::{Key, t};
use koru_ui::icon_button::{IconButton, State as IconButtonState};
use koru_ui::popover::PopoverShell;
use vello::Scene;
use vello::kurbo::Affine;

use super::elements::{
    self, Footer, LIGHTNING, ListRow, MarkName, MarkSpec, PILL_HEIGHT, PILL_ICON_GAP,
    PILL_ON_FILL_ALPHA, PILL_PAD, Painted, Pill, SEGMENT_RADIUS, Segment, Spec, StarSpec,
};
use super::{
    PickerFocus, RailEntry, Row, SharedInput, draw_provider_mark, provider_name, rail_dividers,
    readout,
};
use crate::agents::AgentStatus;
use crate::effort;

impl crate::ChatSurface {
    /// What the open picker's tree declares: its marks — named, the
    /// selected one marked — the **+**, ↻, the list's rows and the rows
    /// under the list. A begun chat's has no rail: no marks and no **+**.
    pub(super) fn picker_spec(&self) -> Spec {
        let hide_rail = self.picker_rail_hidden();
        let rail = self.picker_visible_rail();
        let dividers = rail_dividers(&rail);
        let (rail_previous, rail_next) = self.picker_rail_pages();
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
            hide_rail,
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
            manage: self.agent_settings_offered && !hide_rail && !rail_next,
            rail_previous,
            rail_next,
            refresh: match &selected {
                Some(RailEntry::Agent(agent)) => Some(!self.refresh_enabled(agent)),
                _ => None,
            },
            footer: self.picker_footer(),
            list: self.picker_list_rows(),
            offset: self.picker.offset,
        }
    }

    /// **The footer** (D76): a segment for each level the chat's model
    /// offers, the chat's level raised; *⚡ Fast* and *1M context* when the
    /// model offers them; the readout. `None` for a model that offers none
    /// of them: no footer, no divider.
    pub(crate) fn picker_footer(&self) -> Option<Footer> {
        let lang = self.lang;
        let control = self.effort_control().cloned();
        let fast = self.picker_fast();
        let context = self.picker_context();
        if control.is_none() && fast.is_none() && context.is_none() {
            return None;
        }
        let locked = self.effort_locked();
        let shown = self.effective_reasoning();
        let choice = self
            .transcript
            .as_ref()
            .map_or(ReasoningChoice::Default, |transcript| {
                transcript.chat.current_model.default_reasoning
            });
        let segments = control
            .as_ref()
            .map(|control| {
                effort::stops(control)
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
            levels: control.is_some(),
            segments,
            locked,
            fast: fast.map(|fast| Pill {
                label: t(lang, Key::AiModelFast).to_owned(),
                icon: Some(LIGHTNING),
                on: fast.on,
                disabled: false,
            }),
            context: context.map(|context| Pill {
                label: super::context::pill_label(lang, context.tokens),
                icon: None,
                on: context.chosen_long,
                // The window is a model, the start's in a begun chat.
                disabled: self.model_locked(),
            }),
            readout: control
                .map(|control| readout(&control, choice, lang))
                .unwrap_or_default(),
        })
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

    /// What the picker's adapters draw this frame beyond the spec: the
    /// search field, with the keyboard on it or not.
    fn picker_painted(&self) -> Painted {
        Painted {
            search: Some(SearchPaint {
                input: self.picker.query.clone(),
                placeholder: t(self.lang, Key::AiModelSearch).to_owned(),
                focused: self.picker.focus == PickerFocus::Search,
            }),
        }
    }

    /// The control the keyboard is on, by key, for its ring: the selected
    /// rail mark, the strip's raised segment (its first while none is), or a
    /// pill.
    fn picker_focused_key(&self, spec: &Spec) -> Option<koru_draw::el::Key> {
        match self.picker.focus {
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
            pressed: self.picker.scroll_drag.map(|_| elements::LIST_KEY),
            focused: self.picker_focused_key(&spec),
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

/// **A level's segment in the effort strip** (D76, M7): its short word,
/// centred; the chat's level raised — the popover's own surface on a soft
/// shadow, its word set SemiBold in the primary ink — and the others in
/// the secondary ink, lit under the pointer. Read only, every word is
/// secondary. The keyboard's segment wears the accent ring.
pub(crate) struct SegmentLeaf {
    pub(crate) label: String,
    pub(crate) selected: bool,
    pub(crate) locked: bool,
}

impl Leaf for SegmentLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, super::SEGMENT_H)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        state: ElState,
    ) {
        let palette = cx.palette();
        let radius = SEGMENT_RADIUS.value;
        let plate = scene::rrect(at.x, at.y, at.w, at.h, radius);
        if self.selected {
            // The design's `0 1px 2px` lift: the shadow a pixel down, and
            // a fainter one half a pixel wider round it.
            scene::fill(
                scene,
                root,
                palette.panel_shadow.multiply_alpha(0.5),
                &scene::rrect(at.x - 0.5, at.y + 0.5, at.w + 1.0, at.h + 1.5, radius + 0.5),
            );
            scene::fill(
                scene,
                root,
                palette.panel_shadow,
                &scene::rrect(at.x, at.y + 1.0, at.w, at.h, radius),
            );
            scene::fill(scene, root, cx.surface(Surface::Raised), &plate);
        } else if state.hovered {
            scene::fill(scene, root, cx.surface(Surface::Hover), &plate);
        }
        let style = if self.selected {
            roles::HINT.style().with_weight(FontWeight::SemiBold)
        } else {
            roles::HINT.style()
        };
        let words = engine.simple_layout(&self.label, style);
        let ink = if self.selected && !self.locked {
            Ink::Primary
        } else {
            Ink::Secondary
        };
        scene::draw_layout_first_line(
            scene,
            root,
            &words,
            at.x + ((at.w - words.width()) * 0.5).max(0.0),
            scene::centre_y_in(&words, at),
            Some(ink.of(palette)),
        );
        if state.focused {
            scene::stroke(scene, root, palette.accent, 1.0, &plate);
        }
    }
}

/// **A pill toggle** (D76, M7): *⚡ Fast*, *1M context* — as wide as its
/// words, fully rounded, a hairline round it. On, it is the accent's: an
/// accent border, a light accent wash and accent words. Lit under the
/// pointer; a pill that is only shown is drawn at half strength. The
/// keyboard's pill wears the accent ring outside its border.
pub(crate) struct PillLeaf {
    pub(crate) label: String,
    pub(crate) icon: Option<char>,
    pub(crate) on: bool,
    /// Its words, inset and icon: measured as the tree was built.
    pub(crate) width: f32,
}

impl Leaf for PillLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.width, PILL_HEIGHT.value)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        state: ElState,
    ) {
        let palette = cx.palette();
        let strength = if state.disabled { 0.5 } else { 1.0 };
        let radius = at.h * 0.5;
        let shape = scene::rrect(at.x, at.y, at.w, at.h, radius);
        let (border, words_ink) = if self.on {
            (palette.accent, palette.accent)
        } else {
            (Line::Border.of(palette), Ink::Primary.of(palette))
        };
        if self.on {
            scene::fill(
                scene,
                root,
                palette.accent.multiply_alpha(PILL_ON_FILL_ALPHA * strength),
                &shape,
            );
        }
        if state.hovered {
            scene::fill(scene, root, cx.surface(Surface::Hover), &shape);
        }
        scene::stroke(
            scene,
            root,
            border.multiply_alpha(strength),
            1.0,
            &scene::rrect(at.x + 0.5, at.y + 0.5, at.w - 1.0, at.h - 1.0, radius - 0.5),
        );
        let mut x = at.x + PILL_PAD.token.px(cx.profile);
        if let Some(glyph) = self.icon {
            let size = IconSize::XS.px(cx.profile);
            let layout = engine.icon_layout(glyph, size);
            scene::draw_layout(
                scene,
                root,
                &layout,
                x + (size - layout.width()) * 0.5,
                at.y + (at.h - layout.height()) * 0.5,
                Some(words_ink.multiply_alpha(strength)),
            );
            x += size + PILL_ICON_GAP.token.px(cx.profile);
        }
        let words = engine.simple_layout(&self.label, roles::META.style());
        scene::draw_layout_first_line(
            scene,
            root,
            &words,
            x,
            scene::centre_y_in(&words, at),
            Some(words_ink.multiply_alpha(strength)),
        );
        if state.focused {
            scene::stroke(
                scene,
                root,
                palette.accent,
                1.0,
                &scene::rrect(at.x - 2.0, at.y - 2.0, at.w + 4.0, at.h + 4.0, radius + 2.0),
            );
        }
    }
}

/// **The readout** (D76, M7): *Effort: High (default)* in the secondary
/// ink at the end of the pills' row, the design's `margin-left: auto`. Its
/// longest form that fits the room the pills leave, the shortest whatever.
pub(crate) struct ReadoutLeaf {
    pub(crate) forms: Vec<String>,
}

impl Leaf for ReadoutLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, PILL_HEIGHT.value)
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
        let style = roles::HINT.style();
        let count = self.forms.len();
        for (index, form) in self.forms.iter().enumerate() {
            let words = engine.simple_layout(form, style);
            if words.width() <= at.w || index + 1 == count {
                scene::draw_layout_first_line(
                    scene,
                    root,
                    &words,
                    (at.x + at.w - words.width()).max(at.x),
                    scene::centre_y_in(&words, at),
                    Some(Ink::Secondary.of(cx.palette())),
                );
                return;
            }
        }
    }
}
