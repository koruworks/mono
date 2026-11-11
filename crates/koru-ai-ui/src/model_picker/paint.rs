// SPDX-License-Identifier: MPL-2.0

//! **Drawing the open picker** (D64): its element tree — the control's,
//! `koru_agent_config_ui::picker::elements` — declared from the chat's
//! state, laid out with the host's pointer and keyboard on it and painted
//! inside the *Popover surface* record, so everything drawn in the panel
//! nests under the surface in the inspector. The leaves it is painted with
//! are the control's too (`koru_agent_config_ui::picker::paint`).

use koru_ai_core::ReasoningChoice;
use koru_draw::el::{FrameCx, Interaction};
use koru_draw::theme;
use koru_i18n::{Key, t};
use koru_ui::popover::PopoverShell;
use vello::Scene;
use vello::kurbo::Affine;

use super::elements::{
    self, Footer, LIGHTNING, ListRow, MarkName, MarkSpec, Painted, Pill, Segment, Spec, StarSpec,
};
use super::{
    PickerFocus, RailEntry, Row, SearchPaint, provider_name, provider_profile_mark, rail_dividers,
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
                        RailEntry::Provider(provider) => {
                            Some(provider_profile_mark(provider, false))
                        }
                        RailEntry::Agent(agent) => Some(provider_profile_mark(agent, true)),
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
                        key: elements::segment_key(effort::effort(stop)),
                        choice: effort::effort(stop),
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
                        Row::Favorite(agent, _) => Some(provider_profile_mark(agent, true)),
                        Row::EntryHeading(heading) => heading.provider().map(|provider| {
                            provider_profile_mark(provider, matches!(heading, RailEntry::Agent(_)))
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
