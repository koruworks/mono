// SPDX-License-Identifier: MPL-2.0

//! **Drawing the open picker** (D64): its element tree ([`super::elements`])
//! laid out with the host's pointer and keyboard on it and painted inside
//! the *Popover surface* record, so everything drawn in the panel nests
//! under the surface in the inspector. Each leaf runs the drawing its
//! control always had, in the allocation the frame gives it.

use koru_ai_core::{AiProviderId, ReasoningChoice, ReasoningControl};
use koru_draw::el::{ElState, FrameCx, Interaction};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, inspect, scene, theme};
use koru_i18n::{Key, Lang, t};
use koru_ui::icon_button::{IconButton, State as IconButtonState};
use koru_ui::popover::PopoverShell;
use koru_ui::scrollbar;
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, RoundedRectRadii};

use super::context::{ContextPaint, paint_context_row};
use super::elements::{self, MarkName, MarkSpec, Paint, Part, Spec};
use super::fast::{FastPaint, paint_fast_row};
use super::{
    Geometry, PickerFocus, ROW_H, ROW_MARK, ROW_MARK_ROOM, RailEntry, Row, draw_mark,
    draw_provider_mark, footer_words, provider_name, rail_dividers, rail_fill, reset_in, star_rect,
};
use crate::agents::AgentStatus;
use crate::effort;

impl crate::ChatSurface {
    /// What the open picker's tree declares: its marks — named, the
    /// selected one marked — the **+**, ↻ and the rows under the list.
    pub(super) fn picker_spec(&self) -> Spec {
        let rail = self.picker_rail();
        let dividers = rail_dividers(&rail);
        let selected = self.picker_entry();
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
                    divider_after: dividers.contains(&index),
                    selected: selected.as_ref() == Some(entry),
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
        }
    }

    /// Draw the open picker: the panel, and inside it the rail on its ground,
    /// the search with ↻, the list, and the rows under it.
    pub(crate) fn draw_model_picker(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(geometry) = self.picker_geometry() else {
            return;
        };
        let panel = geometry.panel;
        let spec = self.picker_spec();
        // The host's pointer and keyboard, by key: the frame's own target
        // under the pointer, and the selected mark while the rail has the
        // keyboard.
        let targets = elements::layout(
            &elements::tree(&spec, &mut |_| None),
            panel,
            FrameCx::root(),
        );
        let interaction = Interaction {
            hot: targets.hit_at(self.pointer.0, self.pointer.1),
            pressed: None,
            focused: spec
                .marks
                .iter()
                .find(|mark| mark.selected)
                .filter(|_| self.picker.focus == PickerFocus::Rail)
                .map(|mark| mark.key),
        };
        let rail = self.picker_rail();
        let query = self.picker_query();
        let tree = elements::tree(&spec, &mut |part| {
            self.picker_painter(part, &rail, &query, &geometry)
        });
        let frame = elements::layout(&tree, panel, FrameCx::root().with_interaction(interaction));
        let radius = theme::overlay::popover().radius;
        let engine = &mut self.engine;
        PopoverShell {
            rect: panel,
            radius,
        }
        .draw_containing(canvas, root, |canvas, root| {
            paint_rail_ground(canvas, root, geometry.rail, radius);
            frame.paint(&tree, canvas, root, engine);
        });
    }

    /// The drawing `part` delegates to, holding what it shows; `None` where
    /// the part draws nothing now.
    fn picker_painter(
        &self,
        part: Part,
        rail: &[RailEntry],
        query: &str,
        geometry: &Geometry,
    ) -> Option<Paint> {
        let pointer = self.pointer;
        let lang = self.lang;
        match part {
            Part::Mark(index) => {
                let entry = rail.get(index)?.clone();
                let dim = match &entry {
                    RailEntry::Agent(agent) => {
                        self.agent_status.get(agent).copied() != Some(AgentStatus::Ready)
                            || !self.agent_matches_query(agent, query)
                    }
                    RailEntry::Favorites | RailEntry::Provider(_) => false,
                };
                painter(move |canvas, root, engine, at, state| {
                    paint_mark(canvas, root, engine, &entry, at, state, dim)
                })
            }
            Part::Manage => {
                let name = self.picker_manage_name();
                painter(move |canvas, root, engine, at, state| {
                    paint_manage(canvas, root, engine, name, at, state.hovered)
                })
            }
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
            Part::List => {
                let list = self.list_paint(geometry)?;
                painter(move |canvas, root, engine, at, _| {
                    paint_list(canvas, root, engine, at, &list)
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

    /// The rows the list shows now, in view, as they are drawn.
    fn list_paint(&self, geometry: &Geometry) -> Option<ListPaint> {
        let entry = self.picker_entry()?;
        let rows = self.picker_rows();
        let choosable = self.picker_choosable(&rows);
        let shown = rows
            .iter()
            .enumerate()
            .skip(self.picker.offset)
            .take(geometry.visible_rows())
            .map(|(index, row)| ListRow {
                label: self.picker_row_label(&entry, row),
                words: matches!(row, Row::Heading(_) | Row::Note(_)),
                favorite: match row {
                    Row::Favorite(agent, _) => Some(agent.clone()),
                    _ => None,
                },
                current: self.picker_row_current(&entry, row),
                can: choosable[index],
                highlighted: self.picker.highlight == Some(index)
                    && matches!(self.picker.focus, PickerFocus::Search | PickerFocus::List),
                star: self
                    .row_favorite(&entry, row)
                    .map(|(agent, model)| self.is_favorite(&agent, &model)),
                legacy_more: *row == Row::LegacyMore,
                legacy_tag: self
                    .row_is_legacy(&entry, row)
                    .then(|| t(self.lang, Key::AiModelsLegacyTag).to_owned()),
            })
            .collect();
        Some(ListPaint {
            rows: shown,
            total: rows.len(),
            offset: self.picker.offset,
            dragging: self.picker.scroll_drag.is_some(),
            pointer: self.pointer,
            lang: self.lang,
        })
    }
}

/// A leaf's painter, its closure typed by the bound — `&mut` arguments for
/// any lifetime, as [`Paint`] takes them.
fn painter(
    paint: impl Fn(&mut Scene, Affine, &mut TextEngine, Rect, ElState) + 'static,
) -> Option<Paint> {
    Some(Box::new(paint))
}

/// **The rail's ground** (D45), under the marks: the window rail's colour
/// inside the shell's hairline, rounded where the panel is. Painted by the
/// host from the frame's rail, as a box's background can only be a surface
/// token with one radius (QUESTIONS M2).
fn paint_rail_ground(canvas: &mut Scene, root: Affine, rail: Rect, radius: f32) {
    let _ground = inspect::scope(
        "Agent rail ground",
        "part",
        root,
        scene::rect(rail.x, rail.y, rail.w, rail.h),
        |record| {
            record.property(
                "fill",
                "theme::base_fill()",
                "D45: the window rail's ground",
            );
            record.property("geometry", "the frame's Agent rail", "elements::RAIL");
        },
    );
    let inset = 1.0;
    let corner = f64::from((radius - inset).max(0.0));
    scene::fill(
        canvas,
        root,
        rail_fill(),
        &RoundedRect::new(
            f64::from(rail.x + inset),
            f64::from(rail.y + inset),
            f64::from(rail.x + rail.w),
            f64::from(rail.y + rail.h - inset),
            RoundedRectRadii::new(corner, 0.0, 0.0, corner),
        ),
    );
}

/// A rail mark: the selected entry's plate, the hovered one's, the
/// keyboard's ring, and the agent's mark — dimmed while it is not ready or
/// the search finds nothing of it.
fn paint_mark(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    entry: &RailEntry,
    at: Rect,
    state: ElState,
    dim: bool,
) {
    let p = theme::p();
    if state.selected || state.hovered {
        scene::fill(
            canvas,
            root,
            if state.selected {
                p.row_selected
            } else {
                p.hover_tint
            },
            &scene::rrect(at.x, at.y, at.w, at.h, 6.0),
        );
    }
    if state.focused {
        scene::stroke(
            canvas,
            root,
            p.accent,
            1.0,
            &scene::rrect(at.x, at.y, at.w, at.h, 6.0),
        );
    }
    let colour = if dim { p.text_dim } else { p.text };
    draw_mark(canvas, root, engine, entry, at, colour);
}

/// The **+** at the rail's end (D29).
fn paint_manage(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    name: &'static str,
    at: Rect,
    hovered: bool,
) {
    let p = theme::p();
    let _manage = inspect::scope(
        name,
        "button",
        root,
        scene::rect(at.x, at.y, at.w, at.h),
        |r| r.property("accessible name", name, "caller label"),
    );
    if hovered {
        scene::fill(
            canvas,
            root,
            p.hover_tint,
            &scene::rrect(at.x, at.y, at.w, at.h, 6.0),
        );
    }
    let layout = engine.icon_layout(icons::PLUS, 16.0);
    scene::draw_layout(
        canvas,
        root,
        &layout,
        at.x + (at.w - layout.width()) * 0.5,
        at.y + (at.h - layout.height()) * 0.5,
        Some(if hovered { p.text } else { p.text_dim }),
    );
}

/// One row of the list as it is drawn.
struct ListRow {
    label: String,
    /// A heading or a note: words, not a model.
    words: bool,
    /// A favorite's agent, whose mark leads the row (D46).
    favorite: Option<AiProviderId>,
    current: bool,
    can: bool,
    highlighted: bool,
    /// The ☆ of a model that can be starred, and whether it is.
    star: Option<bool>,
    legacy_more: bool,
    legacy_tag: Option<String>,
}

/// The list as it is drawn: the rows in view, from the first one shown,
/// and what its scrollbar needs.
struct ListPaint {
    rows: Vec<ListRow>,
    total: usize,
    offset: usize,
    dragging: bool,
    pointer: (f32, f32),
    lang: Lang,
}

/// The list in `band`, the room the frame gave it: whole rows from its top,
/// and the scrollbar when it scrolls.
fn paint_list(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    band: Rect,
    list: &ListPaint,
) {
    let p = theme::p();
    let area = elements::list_rect(band);
    let pointer = list.pointer;
    for (visible, row) in list.rows.iter().enumerate() {
        let rect = Rect::new(area.x, area.y + visible as f32 * ROW_H, area.w, ROW_H);
        let hovered = row.can && rect.contains(pointer.0, pointer.1);
        if row.current || row.highlighted || hovered {
            scene::fill(
                canvas,
                root,
                if row.current {
                    p.row_selected
                } else {
                    p.hover_tint
                },
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, 4.0),
            );
        }
        let (style, colour, indent) = if row.words {
            (theme::role::meta(), p.text_dim, 8.0)
        } else {
            (
                theme::role::row(),
                if row.can || row.current {
                    p.text
                } else {
                    p.text_dim
                },
                // A favorite's agent mark comes first (D46).
                if row.favorite.is_some() {
                    8.0 + ROW_MARK_ROOM
                } else {
                    8.0
                },
            )
        };
        // What sits at the row's end, from the end: the ☆ of a model that
        // can be starred (D46), the current one's check, the flyout's
        // caret, an older model's tag while searching. The ☆'s room is kept
        // whether or not it shows, so a name does not shorten under the
        // pointer.
        let star = row.star.map(|starred| (star_rect(rect), starred));
        let mut end = star.map_or(rect.x + rect.w - 8.0, |(star, _)| star.x - 2.0);
        if let Some((star, starred)) = star {
            let under = rect.contains(pointer.0, pointer.1);
            if starred || under {
                draw_star(canvas, root, engine, list.lang, pointer, star, starred);
            }
        }
        if row.current {
            let check = engine.icon_layout(icons::CHECK, 13.0);
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
        if row.legacy_more {
            let caret = engine.icon_layout(icons::CARET_RIGHT, 12.0);
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
        if let Some(tag) = &row.legacy_tag {
            let tag = engine.simple_layout(tag, theme::role::meta());
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
        let shown = engine.elided(&row.label, style, room);
        let line = engine.simple_layout(&shown, style);
        let name_y = rect.y + (rect.h - line.height()) * 0.5;
        scene::draw_layout_first_line(canvas, root, &line, rect.x + indent, name_y, Some(colour));
        // A favorite's agent, by its mark, level with the name.
        if let Some(agent) = &row.favorite {
            let mark = Rect::new(
                rect.x + 8.0,
                name_y + (line.height() - ROW_MARK) * 0.5,
                ROW_MARK,
                ROW_MARK,
            );
            let tone = if row.can { p.text } else { p.text_dim };
            draw_provider_mark(canvas, root, engine, agent, true, mark, tone);
        }
    }
    if let Some(bar) = scrollbar::ScrollbarGeom::for_content(
        area,
        list.total as f32 * ROW_H,
        list.offset as f32 * ROW_H,
    ) {
        let active = list.dragging || bar.in_gutter(pointer.0, pointer.1);
        scrollbar::draw(canvas, root, bar, active);
    }
}

/// A row's ☆ (D46): outlined, brighter under the pointer, while it is not a
/// favorite; filled in the accent once it is. Named for a screen reader and
/// the inspector by what a press does — it raises no tooltip, as the rows
/// raise none (D25).
fn draw_star(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    lang: Lang,
    pointer: (f32, f32),
    star: Rect,
    starred: bool,
) {
    let p = theme::p();
    let name = t(
        lang,
        if starred {
            Key::AiModelFavoriteRemove
        } else {
            Key::AiModelFavoriteAdd
        },
    );
    let _star = inspect::scope(
        name,
        "button",
        root,
        scene::rect(star.x, star.y, star.w, star.h),
        |r| r.property("accessible name", name, "caller label"),
    );
    let hovered = star.contains(pointer.0, pointer.1);
    let (layout, colour) = if starred {
        (engine.icon_layout_filled(icons::STAR, 14.0), p.accent)
    } else {
        (
            engine.icon_layout(icons::STAR, 14.0),
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
