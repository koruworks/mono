// SPDX-License-Identifier: MPL-2.0
//! Element layout for the Bot form. Mutable inputs are painted through the
//! frame's host callback, inside their measured inspector ancestry.
use crate::mainview::delegate::{
    modal::{Delegation, Hit, Phase},
    paint::{Piece, schedule_pieces},
};
use koru_ai_core::bots::BotId;
use koru_draw::{
    el::{self, El, Frame, FrameCx, Key, Length, Role, TextCache},
    geometry::Rect,
    scene,
    text::TextEngine,
    theme::{self, Space, metrics, roles},
};
use koru_i18n::{Key as Words, fill, t};
use koru_ui::{ActionButton, ActionStyle};
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use vello::{Scene, kurbo::Affine};

const ROOT: Key = Key::surface("bot-routine-create", "panel");
const BODY: Key = Key::surface("bot-routine-create", "body");
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(64);
pub(crate) struct Host {
    pub instance: u64,
    pub bot: BotId,
    pub tab: u64,
    pub name: String,
    pub uncertain: bool,
    cache: RefCell<TextCache>,
}
impl Host {
    pub fn new(bot: BotId, tab: u64, name: String, uncertain: bool) -> Self {
        Self {
            instance: NEXT_INSTANCE.fetch_add(64, Ordering::Relaxed),
            bot,
            tab,
            name,
            uncertain,
            cache: RefCell::default(),
        }
    }
}
enum Paint {
    Text(String),
    Field(Hit, Words, bool),
    Objective,
    Button(Hit, String, ActionStyle),
    Chip(Hit, String, bool),
}
impl Paint {
    fn hit(&self) -> Option<Hit> {
        match self {
            Self::Field(hit, ..) | Self::Button(hit, ..) | Self::Chip(hit, ..) => Some(*hit),
            Self::Objective => Some(Hit::Objective),
            _ => None,
        }
    }
}
struct Form {
    tree: El,
    frame: Frame,
    paint: Vec<(Key, Paint)>,
}
pub(crate) fn control_key(hit: Hit) -> Key {
    let id = match hit {
        Hit::ScheduleName => 1,
        Hit::Objective => 2,
        Hit::ScheduleHourly => 3,
        Hit::ScheduleDaily => 4,
        Hit::ScheduleWeekly => 5,
        Hit::ScheduleMonthly => 6,
        Hit::ScheduleWeekday(day) => 20 + day as u128,
        Hit::ScheduleTime => 7,
        Hit::ScheduleDay => 8,
        Hit::ScheduleAdvanced => 9,
        Hit::ScheduleFloating => 10,
        Hit::ScheduleZoned => 11,
        Hit::ScheduleZone => 12,
        Hit::Cancel => 13,
        Hit::Submit => 14,
        Hit::Close => 15,
        Hit::Retry => 16,
        _ => unreachable!("only routine form controls"),
    };
    Key::model(id, "routine-form-control")
}
pub(crate) struct Control {
    pub hit: Hit,
    pub label: String,
    pub bounds: Option<Rect>,
    pub value: Option<String>,
    pub enabled: bool,
    pub selected: Option<bool>,
}
pub(crate) fn published(
    engine: &mut TextEngine,
    modal: &Delegation,
    viewport: (f32, f32),
) -> Vec<Control> {
    let form = form(engine, modal, viewport);
    form.paint
        .iter()
        .filter_map(|(key, part)| {
            let hit = part.hit()?;
            let node = form.frame.node(*key)?;
            let label = match node.label.as_ref()? {
                el::Name::Text(value) => value.clone(),
                el::Name::Key(words) => t(crate::format::lang(), *words).into(),
            };
            let value = match hit {
                Hit::ScheduleName => Some(modal.schedule_name.value()),
                Hit::Objective => Some(modal.objective.value()),
                Hit::ScheduleTime => Some(modal.repeat.time.value()),
                Hit::ScheduleDay => Some(modal.repeat.day.value()),
                Hit::ScheduleAdvanced => Some(modal.repeat.advanced.value()),
                Hit::ScheduleZone => Some(modal.repeat.zone.value()),
                _ => None,
            };
            Some(Control {
                hit,
                label,
                bounds: node.hit_region().filter(|r| r.w > 0.0 && r.h > 0.0),
                value,
                enabled: !node.disabled,
                selected: match part {
                    Paint::Chip(_, _, selected) => Some(*selected),
                    _ => None,
                },
            })
        })
        .collect()
}
fn content(engine: &mut TextEngine, modal: &Delegation, width: f32) -> (El, Vec<(Key, Paint)>) {
    let host = modal.routine_creation.as_ref().unwrap();
    let lang = crate::format::lang();
    let mut parts = vec![
        Paint::Text(fill(t(lang, Words::BotsRoutineCreateTitle), &[&host.name])),
        Paint::Text(t(lang, Words::BotsRoutineCreateHelp).into()),
    ];
    if let Phase::Editing {
        problem: Some(words),
    } = modal.phase
    {
        parts.push(Paint::Text(t(lang, words).into()));
    }
    // Reuse the recurrence editor's labels, validation, and available choices.
    // Its legacy coordinates are discarded: this form's element frame owns geometry.
    let mut recurrence = Vec::new();
    schedule_pieces(engine, modal, lang, 0.0, width, &mut 0.0, &mut recurrence);
    for piece in recurrence {
        match piece {
            Piece::Label(words) => parts.push(Paint::Text(t(lang, words).into())),
            Piece::Guidance { text, .. } | Piece::Dim(text) => parts.push(Paint::Text(text)),
            Piece::Field {
                hit,
                placeholder,
                invalid,
                ..
            } => {
                parts.push(Paint::Field(hit, placeholder, invalid));
                if hit == Hit::ScheduleName {
                    parts.push(Paint::Text(fill(
                        t(lang, Words::BotsRoutineCreateTask),
                        &[&host.name],
                    )));
                    parts.push(Paint::Objective);
                }
            }
            Piece::Chip {
                hit,
                label,
                selected,
                ..
            } => parts.push(Paint::Chip(hit, label, selected)),
            _ => {}
        }
    }
    parts.push(Paint::Button(
        Hit::Cancel,
        t(lang, Words::DelegateCancel).into(),
        ActionStyle::Secondary,
    ));
    parts.push(Paint::Button(
        Hit::Submit,
        t(
            lang,
            if host.uncertain {
                Words::TerminalRetry
            } else {
                Words::BotsRoutineCreate
            },
        )
        .into(),
        ActionStyle::Primary,
    ));
    let controls = modal.controls();
    let mut tree = el::col("Routine form contents")
        .pad(Space::S4)
        .gap(Space::S2);
    let mut paint = Vec::new();
    let mut choices = Vec::new();
    let mut used = 0.0;
    let gap = Space::S2.px(theme::Profile::default());
    for (index, part) in parts.into_iter().enumerate() {
        // Non-model furniture is local to this layout; interactive identity is
        // the existing modal's Hit, used by both keyboard and pointer readers.
        let hit = part.hit();
        let key = hit
            .map(control_key)
            .unwrap_or_else(|| Key::model(index as u128, "routine-form-label"));
        let enabled = hit.is_some_and(|hit| controls.contains(&hit));
        let mut element = match &part {
            Paint::Text(value) => el::text(value.clone()).role(roles::UI).wrap(),
            Paint::Objective => el::col("Routine task input").h(Length::Runtime(
                metrics::BUTTON_MIN.value * 3.0,
                "three form rows",
            )),
            Paint::Field(..) => el::col("Routine field").h(metrics::BUTTON_MIN),
            Paint::Chip(_, label, _) => {
                let w = ActionButton::new(label, ActionStyle::Secondary, None)
                    .width(engine)
                    .min(width);
                el::col("Routine recurrence choice")
                    .h(metrics::BUTTON_MIN)
                    .w(Length::Runtime(w, "measured choice label"))
            }
            Paint::Button(..) => el::col("Routine form action").h(metrics::BUTTON_MIN),
        }
        .id(key);
        if let Some(hit) = hit {
            let label = match &part {
                Paint::Field(_, words, _) => t(lang, *words).to_owned(),
                Paint::Objective => fill(t(lang, Words::BotsRoutineCreateTask), &[&host.name]),
                Paint::Button(_, label, _) | Paint::Chip(_, label, _) => label.clone(),
                _ => unreachable!(),
            };
            element = element
                .hit()
                .focusable()
                .disabled(!enabled)
                .name_text(label)
                .semantics(
                    if matches!(
                        hit,
                        Hit::Objective
                            | Hit::ScheduleName
                            | Hit::ScheduleTime
                            | Hit::ScheduleDay
                            | Hit::ScheduleZone
                            | Hit::ScheduleAdvanced
                    ) {
                        Role::TextInput
                    } else {
                        Role::Button
                    },
                );
        }
        if let Paint::Chip(_, label, _) = &part {
            let w = ActionButton::new(label, ActionStyle::Secondary, None)
                .width(engine)
                .min(width);
            if !choices.is_empty() && used + gap + w > width {
                tree = tree.child(
                    el::row("Recurrence choices")
                        .gap(Space::S2)
                        .children(std::mem::take(&mut choices)),
                );
                used = 0.0;
            }
            used += if choices.is_empty() { w } else { gap + w };
            choices.push(element);
        } else {
            if !choices.is_empty() {
                tree = tree.child(
                    el::row("Recurrence choices")
                        .gap(Space::S2)
                        .children(std::mem::take(&mut choices)),
                );
                used = 0.0;
            }
            tree = tree.child(element);
        }
        paint.push((key, part));
    }
    (tree, paint)
}
fn form(engine: &mut TextEngine, modal: &Delegation, viewport: (f32, f32)) -> Form {
    let host = modal.routine_creation.as_ref().unwrap();
    let pad = Space::S4.px(theme::Profile::default());
    let width = (viewport.0 - pad * 2.0).max(1.0).min(480.0);
    let (contents, paint) = content(
        engine,
        modal,
        (width - pad * 2.0 - theme::SCROLLBAR_GUTTER_W).max(1.0),
    );
    let mut cache = host.cache.borrow_mut();
    let measured = contents.layout(
        engine,
        &mut cache,
        Rect::new(0.0, 0.0, width, 100_000.0),
        FrameCx::root(),
    );
    let natural = measured
        .nodes
        .iter()
        .skip(1)
        .map(|node| node.rect.y + node.rect.h)
        .fold(0.0_f32, f32::max)
        + pad;
    let height = natural.min((viewport.1 - pad * 2.0).max(1.0));
    let rect = Rect::new(
        (viewport.0 - width) * 0.5,
        (viewport.1 - height) * 0.5,
        width,
        height,
    );
    let tree = el::col("New Bot routine").id(ROOT).child(
        el::scroll_y("Routine form scroll")
            .id(BODY)
            .fill()
            .offset(modal.scroll)
            .scrollbar()
            .child(contents),
    );
    let frame = tree.layout(engine, &mut cache, rect, FrameCx::root());
    Form { tree, frame, paint }
}
pub(crate) fn panel(engine: &mut TextEngine, modal: &Delegation, viewport: (f32, f32)) -> Rect {
    form(engine, modal, viewport).frame.node(ROOT).unwrap().rect
}
pub(crate) fn layout(
    engine: &mut TextEngine,
    modal: &Delegation,
    viewport: (f32, f32),
) -> Vec<(Hit, Rect)> {
    let form = form(engine, modal, viewport);
    let controls = modal.controls();
    form.paint
        .iter()
        .filter_map(|(key, part)| {
            let hit = part.hit()?;
            let node = form.frame.node(*key)?;
            controls.contains(&hit).then_some((hit, node.rect))
        })
        .collect()
}
pub(crate) fn scrollbar(
    engine: &mut TextEngine,
    modal: &Delegation,
    viewport: (f32, f32),
) -> Option<koru_ui::scrollbar::ScrollbarGeom> {
    form(engine, modal, viewport).frame.node(BODY)?.scrollbar
}
pub(crate) fn scroll_by(
    engine: &mut TextEngine,
    modal: &mut Delegation,
    viewport: (f32, f32),
    delta: f32,
) {
    let form = form(engine, modal, viewport);
    let node = form.frame.node(BODY).unwrap();
    let max = node
        .scroll
        .zip(node.viewport)
        .map_or(0.0, |((extent, _), view)| (extent - view.h).max(0.0));
    modal.scroll = (modal.scroll + delta).clamp(0.0, max);
}
pub(crate) fn reveal_focus(engine: &mut TextEngine, modal: &mut Delegation, viewport: (f32, f32)) {
    let rect = panel(engine, modal, viewport);
    if let Some((_, target)) = layout(engine, modal, viewport)
        .into_iter()
        .find(|(hit, _)| Some(*hit) == modal.focused())
    {
        let delta = if target.y < rect.y {
            target.y - rect.y
        } else {
            (target.y + target.h - rect.y - rect.h).max(0.0)
        };
        scroll_by(engine, modal, viewport, delta);
    }
}
pub(crate) fn draw(
    modal: &mut Delegation,
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    viewport: (f32, f32),
) {
    let form = form(engine, modal, viewport);
    let focus = modal.focused();
    let controls = modal.controls();
    form.frame
        .paint_host(&form.tree, canvas, root, |node, canvas, root| {
            let r = node.rect;
            if node.key == Some(ROOT) {
                scene::floating_panel(
                    canvas,
                    root,
                    r.x,
                    r.y,
                    r.w,
                    r.h,
                    &scene::Panel::popover(10.0),
                );
            }
            if node.key == Some(BODY)
                && let Some(geom) = node.scrollbar
            {
                koru_ui::scrollbar::draw(canvas, root, geom, modal.scrollbar_drag.is_some());
            }
            let Some((_, part)) = form.paint.iter().find(|(key, _)| Some(*key) == node.key) else {
                return;
            };
            match part {
                Paint::Text(value) => {
                    let layout =
                        engine.simple_layout_wrapped(value, theme::role::ui(), r.w.max(1.0));
                    scene::draw_layout(canvas, root, &layout, r.x, r.y, Some(theme::p().text));
                }
                Paint::Field(hit, placeholder, invalid) => {
                    let field = match hit {
                        Hit::ScheduleName => &mut modal.schedule_name,
                        Hit::ScheduleTime => &mut modal.repeat.time,
                        Hit::ScheduleDay => &mut modal.repeat.day,
                        Hit::ScheduleAdvanced => &mut modal.repeat.advanced,
                        Hit::ScheduleZone => &mut modal.repeat.zone,
                        _ => unreachable!(),
                    };
                    let focused = focus == Some(*hit);
                    let style = koru_ui::text_input::InputStyle::form(focused);
                    koru_ui::text_input::draw_text_input(
                        canvas,
                        root,
                        field,
                        engine,
                        r,
                        if *invalid { style.invalid() } else { style },
                        Some(t(crate::format::lang(), *placeholder)),
                        focused,
                        0.0,
                    );
                }
                Paint::Objective => modal.objective.draw(
                    canvas,
                    root,
                    engine,
                    r,
                    None,
                    focus == Some(Hit::Objective),
                ),
                Paint::Button(hit, label, style) => {
                    let button = ActionButton::new(label, *style, None);
                    if controls.contains(hit) {
                        button.draw_focused(
                            canvas,
                            root,
                            engine,
                            r,
                            modal.hover == Some(*hit),
                            focus == Some(*hit),
                        );
                    } else {
                        button.draw_disabled_led(canvas, root, engine, r, None);
                    }
                }
                Paint::Chip(hit, label, selected) => {
                    ActionButton::new(
                        label,
                        if *selected {
                            ActionStyle::Primary
                        } else {
                            ActionStyle::Secondary
                        },
                        None,
                    )
                    .draw_focused(
                        canvas,
                        root,
                        engine,
                        r,
                        modal.hover == Some(*hit),
                        focus == Some(*hit),
                    );
                }
            }
        });
}
