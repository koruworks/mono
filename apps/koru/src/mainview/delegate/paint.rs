// SPDX-License-Identifier: MPL-2.0

//! Where the modal's controls are, and what they look like.
//!
//! **One geometry, two readers.** [`layout`] returns every control's rectangle
//! in tab order, and both the drawing pass and the hit test walk that same
//! list — the design rule that a surface's draw and hit geometry are one
//! function, so a row can never be clickable somewhere it is not painted.
//!
//! Nothing here decides anything: it reads [`super::modal::Delegation`] and
//! puts pixels on the screen.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use koru_i18n::{Key as I18nKey, fill, t};
use koru_ui::prompt::{KoruPrompt, KoruPromptAction};
use koru_ui::{ActionButton, ActionStyle, Chip};
use vello::Scene;
use vello::kurbo::Affine;

use super::modal::{Delegation, Hit, Phase, readiness};
use crate::bots::view::routine_modal::form as creation;

fn creating(modal: &Delegation) -> bool {
    modal.routine_creation.is_some() && matches!(modal.phase, Phase::Editing { .. })
}

const WIDTH: f32 = 480.0;
const PAD: f32 = 16.0;
const GAP: f32 = 10.0;
const ROW_H: f32 = 26.0;
/// The second, dimmer line a working-folder row grows to show its path.
const DETAIL_H: f32 = 16.0;
const OBJECTIVE_H: f32 = 72.0;
const RADIUS: f32 = 10.0;
/// One drawn thing: a control the ring knows about, or a piece of furniture.
pub(crate) enum Piece {
    Label(I18nKey),
    Sentence(String),
    Dim(String),
    Guidance {
        rect: Rect,
        text: String,
    },
    Objective(Rect),
    ModelSearch(Rect),
    Field {
        rect: Rect,
        hit: Hit,
        placeholder: I18nKey,
        invalid: bool,
    },
    Chip {
        rect: Rect,
        hit: Hit,
        label: String,
        selected: bool,
    },
    /// Agent rows stay inspectable even when launch is unavailable.
    Agent {
        rect: Rect,
        index: usize,
        text: String,
        status: Option<I18nKey>,
        chosen: bool,
    },
    /// A chooser row: its text, whether it is the chosen one, and why it
    /// cannot be chosen at all.
    Choice {
        rect: Rect,
        hit: Hit,
        text: String,
        /// A second, dimmer line under the name -- a working folder's path
        /// (ADR-365). `None` keeps the row one line high.
        detail: Option<String>,
        chosen: bool,
        disabled: Option<I18nKey>,
    },
    /// A row with a trailing *Remove*.
    Removable {
        rect: Rect,
        hit: Hit,
        text: String,
        button: Rect,
    },
    Action {
        rect: Rect,
        hit: Hit,
        label: String,
        style: ActionStyle,
    },
}

impl Piece {
    fn hit(&self) -> Option<(Hit, Rect)> {
        match self {
            Self::ModelSearch(rect) => Some((Hit::ModelSearch, *rect)),
            Self::Objective(rect) => Some((Hit::Objective, *rect)),
            Self::Field { rect, hit, .. } | Self::Chip { rect, hit, .. } => Some((*hit, *rect)),
            Self::Agent { rect, index, .. } => Some((Hit::Agent(*index), *rect)),
            Self::Choice {
                rect,
                hit,
                disabled: None,
                ..
            } => Some((*hit, *rect)),
            Self::Removable { hit, button, .. } => Some((*hit, *button)),
            Self::Action { rect, hit, .. } => Some((*hit, *rect)),
            Self::Choice { .. }
            | Self::Label(_)
            | Self::Sentence(_)
            | Self::Dim(_)
            | Self::Guidance { .. } => None,
        }
    }
}

/// Every agent remains reachable by pointer scrolling as well as keyboard.
pub(crate) fn visible_agents(modal: &Delegation) -> std::ops::Range<usize> {
    0..modal.world.agents.len()
}

/// The panel, centred, sized to what it holds and clamped to the viewport.
///
/// A finished phase is a [`KoruPrompt`] and its panel is the prompt's; the
/// editing form keeps its own arithmetic below. The prompt's WIDTH/PAD are the
/// same constants this file has always used, so neither panel moves.
pub(crate) fn panel(engine: &mut TextEngine, modal: &Delegation, viewport: (f32, f32)) -> Rect {
    if creating(modal) {
        return creation::panel(engine, modal, viewport);
    }
    if !matches!(modal.phase, Phase::Editing { .. }) {
        return finished_prompt(modal, crate::format::lang(), &modal.phase).panel(engine, viewport);
    }
    let h = height(engine, modal, viewport);
    let w = WIDTH.min((viewport.0 - 16.0).max(1.0));
    Rect::new(
        ((viewport.0 - w) * 0.5).max(8.0),
        ((viewport.1 - h) * 0.5).max(8.0),
        w,
        h,
    )
}

fn height(engine: &mut TextEngine, modal: &Delegation, viewport: (f32, f32)) -> f32 {
    let w = WIDTH.min((viewport.0 - 16.0).max(1.0));
    content_height(engine, modal, w).min((viewport.1 - 16.0).max(1.0))
}

fn content_height(engine: &mut TextEngine, modal: &Delegation, width: f32) -> f32 {
    let pieces = pieces(engine, modal, Rect::new(0.0, 0.0, width, 0.0));
    let used = pieces
        .iter()
        .filter_map(|piece| match piece {
            Piece::ModelSearch(r) | Piece::Objective(r) => Some(r.y + r.h),
            Piece::Field { rect, .. } | Piece::Chip { rect, .. } => Some(rect.y + rect.h),
            Piece::Agent { rect, .. } => Some(rect.y + rect.h),
            Piece::Choice { rect, .. } | Piece::Removable { rect, .. } => Some(rect.y + rect.h),
            Piece::Action { rect, .. } => Some(rect.y + rect.h),
            _ => None,
        })
        .fold(0.0_f32, f32::max);
    used + PAD
}

pub(crate) fn scroll_by(
    engine: &mut TextEngine,
    modal: &mut Delegation,
    viewport: (f32, f32),
    delta: f32,
) {
    if creating(modal) {
        return creation::scroll_by(engine, modal, viewport, delta);
    }
    let panel = panel(engine, modal, viewport);
    let max = (content_height(engine, modal, panel.w) - panel.h).max(0.0);
    modal.scroll = (modal.scroll + delta).clamp(0.0, max);
}

pub(crate) fn scrollbar(
    engine: &mut TextEngine,
    modal: &Delegation,
    viewport: (f32, f32),
) -> Option<koru_ui::scrollbar::ScrollbarGeom> {
    if creating(modal) {
        return creation::scrollbar(engine, modal, viewport);
    }
    let panel = panel(engine, modal, viewport);
    koru_ui::scrollbar::ScrollbarGeom::for_content(
        panel,
        content_height(engine, modal, panel.w),
        modal.scroll,
    )
}

fn scrolled_panel(engine: &mut TextEngine, modal: &Delegation, panel: Rect) -> Rect {
    let max = (content_height(engine, modal, panel.w) - panel.h).max(0.0);
    Rect::new(
        panel.x,
        panel.y - modal.scroll.clamp(0.0, max),
        panel.w,
        panel.h,
    )
}

pub(crate) fn reveal_focus(engine: &mut TextEngine, modal: &mut Delegation, viewport: (f32, f32)) {
    if creating(modal) {
        return creation::reveal_focus(engine, modal, viewport);
    }
    let panel = panel(engine, modal, viewport);
    let contents = pieces(engine, modal, panel);
    let Some(focused) = modal.focused() else {
        return;
    };
    let Some((_, target)) = contents
        .iter()
        .filter_map(Piece::hit)
        .find(|(hit, _)| *hit == focused)
    else {
        return;
    };
    let top = target.y;
    let mut bottom = target.y + target.h;
    if matches!(focused, Hit::Agent(_) | Hit::AgentRefresh(_)) {
        // Recovery guidance, when present, is immediately after the agent
        // list (apart from a last row's Retry action). Do not grab some later
        // guidance such as the recurrence preview and scroll the focused
        // agent itself out of view.
        let guidance = contents
            .iter()
            .rposition(|piece| matches!(piece, Piece::Agent { .. }))
            .and_then(|last_agent| {
                contents[last_agent + 1..].iter().find(|piece| {
                    !matches!(
                        piece,
                        Piece::Action {
                            hit: Hit::AgentRefresh(_),
                            ..
                        }
                    )
                })
            })
            .and_then(|piece| match piece {
                Piece::Guidance { rect, .. } => Some(*rect),
                _ => None,
            });
        if let Some(rect) = guidance {
            bottom = bottom.max(rect.y + rect.h);
        }
    }
    if top - modal.scroll < panel.y + 4.0 {
        modal.scroll = (top - panel.y - 4.0).max(0.0);
    } else if bottom - modal.scroll > panel.y + panel.h - 4.0 {
        modal.scroll = bottom - panel.y - panel.h + 4.0;
    }
    scroll_by(engine, modal, viewport, 0.0);
}

/// Every control's rectangle, in the order [`Delegation::controls`] lists them.
pub(crate) fn layout(
    engine: &mut TextEngine,
    modal: &Delegation,
    viewport: (f32, f32),
) -> Vec<(Hit, Rect)> {
    if creating(modal) {
        return creation::layout(engine, modal, viewport);
    }
    let panel = panel(engine, modal, viewport);
    let content_panel = scrolled_panel(engine, modal, panel);
    pieces(engine, modal, content_panel)
        .iter()
        .filter_map(Piece::hit)
        .filter(|(_, rect)| rect.y < panel.y + panel.h && rect.y + rect.h > panel.y)
        .collect()
}

/// The whole surface, laid out against `panel`'s origin.
///
/// Called with a zero-height panel to measure and with the real one to draw,
/// which is why the vertical cursor never reads the panel's height.
fn pieces(engine: &mut TextEngine, modal: &Delegation, panel: Rect) -> Vec<Piece> {
    pieces_for_language(engine, modal, panel, crate::format::lang())
}

fn pieces_for_language(
    engine: &mut TextEngine,
    modal: &Delegation,
    panel: Rect,
    lang: koru_i18n::Lang,
) -> Vec<Piece> {
    let inner = panel.w - PAD * 2.0;
    let x = panel.x + PAD;
    let mut y = panel.y + PAD;
    let mut out = Vec::new();

    let title = t(
        lang,
        if modal.routine.is_some() {
            I18nKey::BotsRoutineEditTiming
        } else if modal.edit.is_some() {
            I18nKey::DelegateScheduleEditTitle
        } else {
            I18nKey::DelegateTitle
        },
    );
    out.push(Piece::Sentence(title.to_owned()));
    y += theme::role::ui().metrics.line_height + GAP;

    // **A Bot Routine's timing and name**: the name field, the recurrence
    // picker, and Save. The same pieces a Repeat draws, and none of a run's.
    if let (Some(routine), Phase::Editing { problem }) = (&modal.routine, &modal.phase) {
        let mut said = Vec::new();
        if let Some(problem) = problem {
            said.push(t(lang, *problem).to_owned());
        }
        if routine.on {
            said.push(t(lang, I18nKey::BotsRoutineOnEditNotice).to_owned());
        }
        said.push(routine.zone.clone());
        for sentence in said {
            y += sentence_height(engine, &sentence, inner);
            out.push(Piece::Dim(sentence));
        }
        y += GAP;
        schedule_pieces(engine, modal, lang, x, inner, &mut y, &mut out);
        let cancel = t(lang, I18nKey::DelegateCancel);
        let submit = t(lang, I18nKey::BotsRoutineSave);
        let cancel_w = ActionButton::new(cancel, ActionStyle::Secondary, None).width(engine);
        let submit_w = ActionButton::new(submit, ActionStyle::Primary, None).width(engine);
        let right = x + inner;
        if modal.ready() {
            out.push(Piece::Action {
                rect: Rect::new(right - submit_w, y, submit_w, ActionButton::H),
                hit: Hit::Submit,
                label: submit.to_owned(),
                style: ActionStyle::Primary,
            });
        }
        let cancel_x = if modal.ready() {
            right - submit_w - ActionButton::GAP - cancel_w
        } else {
            right - cancel_w
        };
        out.push(Piece::Action {
            rect: Rect::new(cancel_x, y, cancel_w, ActionButton::H),
            hit: Hit::Cancel,
            label: cancel.to_owned(),
            style: ActionStyle::Secondary,
        });
        return out;
    }

    match &modal.phase {
        Phase::Editing { problem } => {
            if modal.edit.is_none() {
                out.push(Piece::Label(I18nKey::DelegateModeLabel));
                y += ROW_H;
                let mode_labels = [
                    t(lang, I18nKey::DelegateRunOnce),
                    t(lang, I18nKey::DelegateRepeat),
                ];
                let mode_rects = koru_ui::chip::wrap(engine, x, y, x + inner, mode_labels);
                for (rect, hit, selected, label) in [
                    (
                        mode_rects[0],
                        Hit::RunOnce,
                        modal.kind == super::modal::repeat::Kind::RunOnce,
                        mode_labels[0],
                    ),
                    (
                        mode_rects[1],
                        Hit::Repeat,
                        modal.kind == super::modal::repeat::Kind::Repeat,
                        mode_labels[1],
                    ),
                ] {
                    out.push(Piece::Chip {
                        rect,
                        hit,
                        label: label.to_owned(),
                        selected,
                    });
                }
                y = koru_ui::chip::bottom(&mode_rects, y) + GAP;
            }

            out.push(Piece::Label(I18nKey::DelegateObjectiveLabel));
            y += ROW_H;
            let objective = Rect::new(x, y, inner, OBJECTIVE_H);
            out.push(Piece::Objective(objective));
            y += OBJECTIVE_H + GAP;

            out.push(Piece::Label(I18nKey::DelegateAgentLabel));
            y += ROW_H;
            if modal.world.agents.is_empty() {
                let sentence = t(lang, I18nKey::DelegateNoAgents).to_owned();
                y += sentence_height(engine, &sentence, inner);
                out.push(Piece::Dim(sentence));
            } else {
                let retry = t(lang, I18nKey::TerminalRetry);
                let retry_w = ActionButton::new(retry, ActionStyle::Ghost, None).width(engine);
                for index in visible_agents(modal) {
                    let agent = &modal.world.agents[index];
                    let retryable = readiness::retryable(&agent.readiness);
                    out.push(Piece::Agent {
                        rect: Rect::new(
                            x,
                            y,
                            if retryable {
                                (inner - retry_w - GAP).max(1.0)
                            } else {
                                inner
                            },
                            ROW_H,
                        ),
                        index,
                        text: agent.name.clone(),
                        status: readiness::status(&agent.readiness),
                        chosen: modal.agent == Some(index) && agent.choosable(),
                    });
                    if retryable {
                        out.push(Piece::Action {
                            rect: Rect::new(
                                x + inner - retry_w,
                                y + (ROW_H - ActionButton::H) * 0.5,
                                retry_w,
                                ActionButton::H,
                            ),
                            hit: Hit::AgentRefresh(index),
                            label: retry.to_owned(),
                            style: ActionStyle::Ghost,
                        });
                    }
                    y += ROW_H;
                }
                if let Some(agent) = modal
                    .inspected_agent()
                    .and_then(|index| modal.world.agents.get(index))
                    && let Some(key) = readiness::guidance(&agent.readiness)
                {
                    let sentence = t(lang, key).to_owned();
                    // Full guidance wraps below the list, rather than being clipped
                    // into the status column or requiring a pointer tooltip.
                    let h = sentence_height(engine, &sentence, inner);
                    out.push(Piece::Guidance {
                        rect: Rect::new(x, y, inner, h),
                        text: sentence,
                    });
                    y += h;
                }
            }
            y += GAP;

            if modal.models.offered {
                out.push(Piece::Label(I18nKey::DelegateModelLabel));
                y += ROW_H;
                if let Some(selected) = &modal.models.selected {
                    let text = selected.as_str().to_owned();
                    let h = sentence_height(engine, &text, inner);
                    out.push(Piece::Guidance {
                        rect: Rect::new(x, y, inner, h),
                        text,
                    });
                    y += h;
                }
                out.push(Piece::Choice {
                    rect: Rect::new(x, y, inner, ROW_H),
                    hit: Hit::ModelDefault,
                    text: fill(
                        t(lang, I18nKey::DelegateModelDefault),
                        &[modal
                            .agent
                            .and_then(|index| modal.world.agents.get(index))
                            .map_or("", |agent| agent.name.as_str())],
                    ),
                    detail: None,
                    chosen: modal.models.selected.is_none(),
                    disabled: None,
                });
                y += ROW_H;
                out.push(Piece::ModelSearch(Rect::new(x, y, inner, ROW_H + 8.0)));
                y += ROW_H + 8.0;
                let mut previous_group = None;
                for index in modal.models.visible() {
                    let model = &modal
                        .models
                        .catalog
                        .as_ref()
                        .expect("visible catalog")
                        .models()[index];
                    if previous_group != Some(&model.group) {
                        let text = model.group.as_str().to_owned();
                        let h = sentence_height(engine, &text, inner);
                        out.push(Piece::Guidance {
                            rect: Rect::new(x, y, inner, h),
                            text,
                        });
                        y += h;
                        previous_group = Some(&model.group);
                    }
                    out.push(Piece::Choice {
                        rect: Rect::new(x, y, inner, ROW_H),
                        hit: Hit::Model(index),
                        text: model.id.as_str().to_owned(),
                        detail: None,
                        chosen: modal
                            .models
                            .selected
                            .as_ref()
                            .is_some_and(|selected| selected.as_str() == model.id.as_str()),
                        disabled: None,
                    });
                    y += ROW_H;
                }
                let controls = modal.models.controls();
                for (hit, key) in [
                    (Hit::ModelPrevious, I18nKey::DelegateModelPrevious),
                    (Hit::ModelNext, I18nKey::DelegateModelNext),
                    (Hit::ModelRefresh, I18nKey::DelegateModelRefresh),
                ] {
                    if controls.contains(&hit) {
                        out.push(Piece::Action {
                            rect: Rect::new(x, y, inner, ROW_H),
                            hit,
                            label: t(lang, key).into(),
                            style: ActionStyle::Ghost,
                        });
                        y += ROW_H;
                    }
                }
                let status = if modal.models.request.is_some() {
                    Some(I18nKey::DelegateModelLoading)
                } else if !modal.models.selection_valid() {
                    Some(I18nKey::DelegateModelUnavailable)
                } else if modal.models.problem {
                    Some(I18nKey::DelegateModelFailed)
                } else if modal
                    .models
                    .catalog
                    .as_ref()
                    .is_some_and(|catalog| catalog.models().is_empty())
                {
                    Some(I18nKey::DelegateModelConnectionsEmpty)
                } else if modal.models.filtered.is_empty() {
                    Some(I18nKey::DelegateModelEmpty)
                } else {
                    None
                };
                if let Some(status) = status {
                    let text = t(lang, status).to_owned();
                    let h = sentence_height(engine, &text, inner);
                    out.push(Piece::Guidance {
                        rect: Rect::new(x, y, inner, h),
                        text,
                    });
                    y += h;
                }
                y += GAP;
            }
            out.push(Piece::Label(I18nKey::DelegateScopeLabel));
            y += ROW_H;
            let change = t(lang, I18nKey::DelegateScopeChange);
            let change_w = ActionButton::new(change, ActionStyle::Ghost, None).width(engine);
            for (index, row) in modal.world.scopes.iter().enumerate() {
                // A path makes the row two lines: the name, and where that is.
                let h = match row.path.is_some() {
                    true => ROW_H + DETAIL_H,
                    false => ROW_H,
                };
                // **Only Koru's own folder can be moved from here** -- a
                // project's root belongs to the project. The control takes the
                // width it needs out of the row rather than sitting on top of
                // it, so the two rectangles never overlap and the hit test
                // does not have to know which was laid out first.
                let movable = matches!(row.scope, koru_delegate_core::WorkspaceScope::Temporary);
                let w = match movable {
                    true => (inner - change_w - GAP).max(1.0),
                    false => inner,
                };
                if movable {
                    out.push(Piece::Action {
                        rect: Rect::new(
                            x + inner - change_w,
                            y + (ROW_H - ActionButton::H) * 0.5,
                            change_w,
                            ActionButton::H,
                        ),
                        hit: Hit::ScopeBase,
                        label: change.to_owned(),
                        style: ActionStyle::Ghost,
                    });
                }
                out.push(Piece::Choice {
                    rect: Rect::new(x, y, w, h),
                    hit: Hit::Scope(index),
                    text: row.name.clone(),
                    detail: row.path.clone(),
                    chosen: modal.scope == Some(index),
                    disabled: row.disabled,
                });
                y += h;
            }
            if let Some(problem) = modal.world.base_problem {
                let sentence = t(lang, problem).to_owned();
                y += sentence_height(engine, &sentence, inner);
                out.push(Piece::Dim(sentence));
            }
            y += GAP;

            out.push(Piece::Label(I18nKey::DelegateContextLabel));
            y += ROW_H;
            if modal.context.is_empty() {
                out.push(Piece::Dim(
                    t(lang, I18nKey::DelegateContextEmpty).to_owned(),
                ));
                y += ROW_H;
            }
            let remove = t(lang, I18nKey::DelegateRemove);
            let remove_w = ActionButton::new(remove, ActionStyle::Ghost, None).width(engine);
            if modal.world.dropped_context {
                let sentence = t(lang, I18nKey::DelegateContextDropped).to_owned();
                y += sentence_height(engine, &sentence, inner);
                out.push(Piece::Dim(sentence));
            }
            for (index, item) in modal.context.iter().enumerate() {
                let text = if item.block {
                    fill(t(lang, I18nKey::DelegateContextBlock), &[&item.label])
                } else {
                    item.label.clone()
                };
                out.push(Piece::Removable {
                    rect: Rect::new(x, y, inner, ROW_H),
                    hit: Hit::ContextRemove(index),
                    text,
                    button: Rect::new(
                        x + inner - remove_w,
                        y + (ROW_H - ActionButton::H) * 0.5,
                        remove_w,
                        ActionButton::H,
                    ),
                });
                y += ROW_H;
            }
            let add = t(lang, I18nKey::DelegateContextAdd);
            let add_w = ActionButton::new(add, ActionStyle::Ghost, None).width(engine);
            out.push(Piece::Action {
                rect: Rect::new(x, y, add_w, ActionButton::H),
                hit: Hit::ContextAdd,
                label: add.to_owned(),
                style: ActionStyle::Ghost,
            });
            y += ActionButton::H + GAP;

            // D28. The sentence says what the agent will reach, and the
            // control beside it takes that reach away — which is the whole
            // point of showing it before the work starts rather than after.
            out.push(Piece::Label(I18nKey::DelegateGrantLabel));
            y += ROW_H;
            if modal.grant {
                let sentence = fill(
                    t(lang, I18nKey::DelegateGrantWorkspace),
                    &[&modal.world.workspace_name],
                );
                let h = sentence_height(engine, &sentence, inner - remove_w - GAP);
                out.push(Piece::Removable {
                    rect: Rect::new(x, y, inner, h.max(ROW_H)),
                    hit: Hit::GrantRemove,
                    text: sentence,
                    button: Rect::new(
                        x + inner - remove_w,
                        y + (ROW_H - ActionButton::H) * 0.5,
                        remove_w,
                        ActionButton::H,
                    ),
                });
                y += h.max(ROW_H);
            } else {
                let sentence = t(lang, I18nKey::DelegateGrantRemoved).to_owned();
                y += sentence_height(engine, &sentence, inner);
                out.push(Piece::Dim(sentence));
            }
            y += GAP;

            if modal.kind == super::modal::repeat::Kind::Repeat {
                schedule_pieces(engine, modal, lang, x, inner, &mut y, &mut out);
            }

            if let Some(problem) = problem {
                let sentence = t(lang, *problem).to_owned();
                y += sentence_height(engine, &sentence, inner);
                out.push(Piece::Dim(sentence));
                y += 2.0;
            }

            let cancel = t(lang, I18nKey::DelegateCancel);
            let submit = t(
                lang,
                if modal.edit.is_some() {
                    I18nKey::DelegateScheduleSave
                } else if modal.kind == super::modal::repeat::Kind::Repeat {
                    I18nKey::DelegateScheduleSubmit
                } else {
                    I18nKey::DelegateSubmit
                },
            );
            let cancel_w = ActionButton::new(cancel, ActionStyle::Secondary, None).width(engine);
            let submit_w = ActionButton::new(submit, ActionStyle::Primary, None).width(engine);
            let right = x + inner;
            if modal.ready() {
                out.push(Piece::Action {
                    rect: Rect::new(right - submit_w, y, submit_w, ActionButton::H),
                    hit: Hit::Submit,
                    label: submit.to_owned(),
                    style: ActionStyle::Primary,
                });
            }
            let cancel_x = if modal.ready() {
                right - submit_w - ActionButton::GAP - cancel_w
            } else {
                right - cancel_w
            };
            out.push(Piece::Action {
                rect: Rect::new(cancel_x, y, cancel_w, ActionButton::H),
                hit: Hit::Cancel,
                label: cancel.to_owned(),
                style: ActionStyle::Secondary,
            });
            y += ActionButton::H;
        }
        phase => {
            let prompt = finished_prompt(modal, lang, phase);
            for sentence in &prompt.body {
                y += sentence_height(engine, sentence, inner);
                out.push(Piece::Dim(sentence.clone()));
            }
            y += GAP;
            // The hit rectangles are the prompt's own layout — the surface a
            // person clicks is the surface that was drawn, here as in the form.
            for (action, rect) in prompt
                .actions
                .iter()
                .zip(prompt.action_rects(engine, panel))
            {
                out.push(Piece::Action {
                    rect,
                    hit: action.action,
                    label: action.label.clone(),
                    style: action.style,
                });
            }
            y += ActionButton::H;
        }
    }
    let _ = y;
    out
}

/// Every sentence the surface says now, in order: a test reading the
/// dialog's words without a window.
#[cfg(test)]
pub(crate) fn sentences(
    engine: &mut TextEngine,
    modal: &Delegation,
    viewport: (f32, f32),
) -> Vec<String> {
    let panel = panel(engine, modal, viewport);
    pieces_for_language(engine, modal, panel, koru_i18n::Lang::En)
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Sentence(text) | Piece::Dim(text) => Some(text),
            Piece::Action { label, .. } => Some(label),
            _ => None,
        })
        .collect()
}

pub(crate) fn schedule_pieces(
    engine: &mut TextEngine,
    modal: &Delegation,
    lang: koru_i18n::Lang,
    x: f32,
    inner: f32,
    y: &mut f32,
    out: &mut Vec<Piece>,
) {
    use koru_recurrence::{RuleShape, Weekday};
    out.push(Piece::Label(I18nKey::DelegateScheduleName));
    *y += ROW_H;
    let name_problem = modal.schedule_name_problem();
    out.push(Piece::Field {
        rect: Rect::new(x, *y, inner, ROW_H + 8.0),
        hit: Hit::ScheduleName,
        placeholder: I18nKey::DelegateScheduleNamePlaceholder,
        invalid: name_problem.is_some(),
    });
    *y += ROW_H + 8.0;
    if let Some(problem) = name_problem {
        validation_guidance(engine, lang, problem, x, inner, y, out);
    }
    *y += GAP;

    out.push(Piece::Label(I18nKey::DelegateScheduleLabel));
    *y += ROW_H;
    let frequencies = [
        (
            Hit::ScheduleHourly,
            I18nKey::DelegateScheduleHourly,
            RuleShape::Hourly,
        ),
        (
            Hit::ScheduleDaily,
            I18nKey::DelegateScheduleDaily,
            RuleShape::Daily,
        ),
        (
            Hit::ScheduleWeekly,
            I18nKey::DelegateScheduleWeekly,
            RuleShape::Weekly,
        ),
        (
            Hit::ScheduleMonthly,
            I18nKey::DelegateScheduleMonthly,
            RuleShape::Monthly,
        ),
    ];
    let labels = frequencies.map(|(_, key, _)| t(lang, key));
    let rects = koru_ui::chip::wrap(engine, x, *y, x + inner, labels);
    for ((hit, key, shape), rect) in frequencies.into_iter().zip(rects.iter().copied()) {
        out.push(Piece::Chip {
            rect,
            hit,
            label: t(lang, key).to_owned(),
            selected: modal.repeat.picker_active && modal.repeat.frequency == shape,
        });
    }
    *y = koru_ui::chip::bottom(&rects, *y) + GAP;

    if modal.repeat.frequency == RuleShape::Weekly {
        let days = [
            (Weekday::Monday, I18nKey::ScheduleMonday),
            (Weekday::Tuesday, I18nKey::ScheduleTuesday),
            (Weekday::Wednesday, I18nKey::ScheduleWednesday),
            (Weekday::Thursday, I18nKey::ScheduleThursday),
            (Weekday::Friday, I18nKey::ScheduleFriday),
            (Weekday::Saturday, I18nKey::ScheduleSaturday),
            (Weekday::Sunday, I18nKey::ScheduleSunday),
        ];
        let labels = days.map(|(_, key)| t(lang, key));
        let rects = koru_ui::chip::wrap(engine, x, *y, x + inner, labels);
        for ((day, key), rect) in days.into_iter().zip(rects.iter().copied()) {
            out.push(Piece::Chip {
                rect,
                hit: Hit::ScheduleWeekday(day),
                label: t(lang, key).to_owned(),
                selected: modal.repeat.weekdays.contains(day),
            });
        }
        *y = koru_ui::chip::bottom(&rects, *y) + GAP;
    }

    if modal.repeat.frequency != RuleShape::Hourly {
        out.push(Piece::Label(I18nKey::DelegateScheduleTime));
        *y += ROW_H;
        out.push(Piece::Field {
            rect: Rect::new(x, *y, inner, ROW_H + 8.0),
            hit: Hit::ScheduleTime,
            placeholder: I18nKey::DelegateScheduleTime,
            invalid: !modal.repeat.time_valid(),
        });
        *y += ROW_H + 8.0;
        if !modal.repeat.time_valid() {
            validation_guidance(
                engine,
                lang,
                I18nKey::DelegateScheduleTimeInvalid,
                x,
                inner,
                y,
                out,
            );
        }
        *y += GAP;
    }
    if modal.repeat.frequency == RuleShape::Monthly {
        out.push(Piece::Label(I18nKey::DelegateScheduleDay));
        *y += ROW_H;
        let invalid = !modal.repeat.day_valid();
        out.push(Piece::Field {
            rect: Rect::new(x, *y, inner, ROW_H + 8.0),
            hit: Hit::ScheduleDay,
            placeholder: I18nKey::DelegateScheduleDay,
            invalid,
        });
        *y += ROW_H + 8.0;
        if invalid {
            validation_guidance(
                engine,
                lang,
                I18nKey::DelegateScheduleDayInvalid,
                x,
                inner,
                y,
                out,
            );
        }
        *y += GAP;
    }

    out.push(Piece::Label(I18nKey::DelegateScheduleAdvanced));
    *y += ROW_H;
    out.push(Piece::Field {
        rect: Rect::new(x, *y, inner, ROW_H + 8.0),
        hit: Hit::ScheduleAdvanced,
        placeholder: I18nKey::DelegateScheduleAdvancedPlaceholder,
        invalid: !modal.repeat.advanced_valid,
    });
    *y += ROW_H + 8.0;
    if !modal.repeat.advanced_valid {
        let text = t(lang, I18nKey::DelegateScheduleAdvancedInvalid).to_owned();
        let h = sentence_height(engine, &text, inner);
        out.push(Piece::Guidance {
            rect: Rect::new(x, *y, inner, h),
            text,
        });
        *y += h;
    }
    *y += GAP;

    out.push(Piece::Label(I18nKey::DelegateScheduleTimeBasis));
    *y += ROW_H;
    let basis = [
        (
            Hit::ScheduleFloating,
            I18nKey::DelegateScheduleFloating,
            super::modal::repeat::TimeBasis::Floating,
        ),
        (
            Hit::ScheduleZoned,
            I18nKey::DelegateScheduleZoned,
            super::modal::repeat::TimeBasis::Zoned,
        ),
    ];
    let labels = basis.map(|(_, key, _)| t(lang, key));
    let rects = koru_ui::chip::wrap(engine, x, *y, x + inner, labels);
    for ((hit, key, value), rect) in basis.into_iter().zip(rects.iter().copied()) {
        out.push(Piece::Chip {
            rect,
            hit,
            label: t(lang, key).to_owned(),
            selected: modal.repeat.basis == value,
        });
    }
    *y = koru_ui::chip::bottom(&rects, *y) + GAP;
    if modal.repeat.basis == super::modal::repeat::TimeBasis::Zoned {
        out.push(Piece::Label(I18nKey::DelegateScheduleZone));
        *y += ROW_H;
        out.push(Piece::Field {
            rect: Rect::new(x, *y, inner, ROW_H + 8.0),
            hit: Hit::ScheduleZone,
            placeholder: I18nKey::DelegateScheduleZonePlaceholder,
            invalid: !modal.repeat.zone_valid(),
        });
        *y += ROW_H + 8.0;
        if !modal.repeat.zone_valid() {
            validation_guidance(
                engine,
                lang,
                I18nKey::DelegateScheduleZoneInvalid,
                x,
                inner,
                y,
                out,
            );
        }
        *y += GAP;
    }
    let preview = modal.repeat.preview();
    let h = sentence_height(engine, &preview, inner);
    out.push(Piece::Guidance {
        rect: Rect::new(x, *y, inner, h),
        text: preview,
    });
    *y += h;
}

fn validation_guidance(
    engine: &mut TextEngine,
    lang: koru_i18n::Lang,
    key: I18nKey,
    x: f32,
    inner: f32,
    y: &mut f32,
    out: &mut Vec<Piece>,
) {
    let text = t(lang, key).to_owned();
    let h = sentence_height(engine, &text, inner);
    out.push(Piece::Guidance {
        rect: Rect::new(x, *y, inner, h),
        text,
    });
    *y += h;
}

/// What a finished or in-flight surface says. One sentence per fact, so D34's
/// busy-folder warning sits beside the run's name rather than replacing it.
///
/// **The name, not the id.** *Delegated. The run is
/// 27e24511-6d05-4d53-bc99-f5c40b5468c9.* told a person nothing they could
/// carry: the id was simply what was to hand when the sentence was written,
/// before runs had names. `RunName::of` is a pure function of the `RunId`
/// (ADR-360), so this needs no store read and no round trip, and it says the
/// same two words the navigator row, the tab and the pill say.
pub(crate) fn finished_sentences(lang: koru_i18n::Lang, phase: &Phase) -> Vec<String> {
    match phase {
        Phase::Sending => vec![t(lang, I18nKey::DelegateSending).to_owned()],
        Phase::Scheduling => vec![t(lang, I18nKey::DelegateScheduleSending).to_owned()],
        Phase::Done { run, busy, .. } => {
            let mut lines = vec![fill(
                t(lang, I18nKey::DelegateSent),
                &[&super::run_label(*run)],
            )];
            if let Some(busy) = busy {
                lines.push(fill(
                    t(lang, I18nKey::DelegateBusyFolder),
                    &[&super::run_label(*busy)],
                ));
            }
            lines
        }
        Phase::Scheduled { .. } => {
            vec![t(lang, I18nKey::DelegateScheduleCreated).to_owned()]
        }
        Phase::Refused { code } => {
            vec![fill(t(lang, I18nKey::DelegateRefused), &[code])]
        }
        Phase::Editing { .. } => Vec::new(),
    }
}

fn finished_sentences_for(modal: &Delegation, lang: koru_i18n::Lang, phase: &Phase) -> Vec<String> {
    if modal.routine_creation.is_some() {
        return vec![t(lang, I18nKey::BotsRoutineSaving).to_owned()];
    }
    // A Routine's timing has one phase besides the form: its Save is out.
    if modal.routine.is_some() {
        return vec![t(lang, I18nKey::BotsRoutineSaving).to_owned()];
    }
    if modal.edit.is_some() {
        return match phase {
            Phase::Scheduling => vec![t(lang, I18nKey::DelegateScheduleSaving).to_owned()],
            Phase::Scheduled { .. } => vec![t(lang, I18nKey::DelegateScheduleUpdated).to_owned()],
            Phase::Refused { code }
                if matches!(
                    code.as_str(),
                    "schedule_revision_conflict" | "schedule_name_conflict"
                ) =>
            {
                vec![t(lang, I18nKey::DelegateScheduleEditStale).to_owned()]
            }
            Phase::Refused { .. } => {
                vec![t(lang, I18nKey::DelegateScheduleEditRefused).to_owned()]
            }
            _ => finished_sentences(lang, phase),
        };
    }
    finished_sentences(lang, phase)
}

/// A finished phase as a [`KoruPrompt`]: the same title the piece walk draws,
/// the same sentences, the same action row — so the surface drawn and the
/// surface hit-tested are one layout.
fn finished_prompt(modal: &Delegation, lang: koru_i18n::Lang, phase: &Phase) -> KoruPrompt<Hit> {
    let title = t(
        lang,
        if modal.routine.is_some() {
            I18nKey::BotsRoutineEditTiming
        } else if modal.edit.is_some() {
            I18nKey::DelegateScheduleEditTitle
        } else {
            I18nKey::DelegateTitle
        },
    );
    KoruPrompt::new(
        modal.routine_creation.as_ref().map_or_else(
            || title.to_owned(),
            |host| fill(t(lang, I18nKey::BotsRoutineCreateTitle), &[&host.name]),
        ),
        finished_sentences_for(modal, lang, phase),
        finished_actions(modal, lang, phase),
    )
}

/// The action row of a finished surface, in corner-out order: the first
/// action owns the right-hand corner and the rest walk left — the order
/// [`Delegation::controls`] already lists the ring in.
fn finished_actions(
    modal: &Delegation,
    lang: koru_i18n::Lang,
    phase: &Phase,
) -> Vec<KoruPromptAction<Hit>> {
    if modal.routine_creation.is_some() {
        return vec![KoruPromptAction {
            action: Hit::Cancel,
            label: t(lang, I18nKey::DelegateClose).into(),
            style: ActionStyle::Secondary,
        }];
    }
    let retryable = matches!(phase, Phase::Refused { .. })
        && modal.retry_request.as_ref().is_some_and(|request| {
            request.correlation().mode == super::modal::repeat::Kind::Repeat
        });
    let (label, hit) = if retryable {
        (t(lang, I18nKey::DelegateRetry), Hit::Retry)
    } else {
        action_for(lang, phase)
    };
    let style = if retryable {
        ActionStyle::Primary
    } else {
        ActionStyle::Secondary
    };
    let mut actions = vec![KoruPromptAction {
        action: hit,
        label: label.to_owned(),
        style,
    }];
    // **A way to go and look, beside the way to shut the window.** Only where
    // there is a run to look at. Both are secondary: the work is already
    // handed over, so neither is *the* thing to do next, and a filled button
    // here would make opening the tab read as expected rather than offered.
    // *OK* keeps the right-hand corner it has always had.
    if matches!(phase, Phase::Done { .. }) {
        actions.push(KoruPromptAction {
            action: Hit::ViewRun,
            label: t(lang, I18nKey::DelegateViewRun).to_owned(),
            style: ActionStyle::Secondary,
        });
    } else if retryable {
        actions.push(KoruPromptAction {
            action: Hit::Close,
            label: t(lang, I18nKey::DelegateClose).to_owned(),
            style: ActionStyle::Secondary,
        });
    }
    actions
}

/// The one button a delegation that is no longer being edited carries, and
/// **the word depends on what already happened**.
///
/// While it is sending, cancelling is real: the request stops and nothing was
/// handed over. Once the run exists, *Cancel* would be a lie — the work is
/// with the agent and pressing a button in this window takes none of it back
/// (§7.1: the run was durable before the gateway answered) — so the button
/// accepts what it is being told. A refusal started nothing at all, so there
/// is nothing to accept and the button only shuts the window.
pub(crate) fn action_for(lang: koru_i18n::Lang, phase: &Phase) -> (&'static str, Hit) {
    match phase {
        Phase::Sending | Phase::Scheduling => (t(lang, I18nKey::DelegateCancel), Hit::Cancel),
        Phase::Done { .. } | Phase::Scheduled { .. } => (t(lang, I18nKey::DelegateOk), Hit::Close),
        Phase::Refused { .. } => (t(lang, I18nKey::DelegateClose), Hit::Close),
        _ => (t(lang, I18nKey::DelegateClose), Hit::Close),
    }
}

fn sentence_height(engine: &mut TextEngine, text: &str, width: f32) -> f32 {
    engine
        .simple_layout_wrapped(text, theme::role::hint(), width.max(1.0))
        .height()
        + 4.0
}

pub(crate) fn hit_at(
    engine: &mut TextEngine,
    modal: &Delegation,
    viewport: (f32, f32),
    x: f32,
    y: f32,
) -> Option<Hit> {
    if !panel(engine, modal, viewport).contains(x, y) {
        return None;
    }
    layout(engine, modal, viewport)
        .into_iter()
        .find(|(_, rect)| rect.contains(x, y))
        .map(|(hit, _)| hit)
}

pub(crate) fn objective_rect(
    engine: &mut TextEngine,
    modal: &Delegation,
    viewport: (f32, f32),
) -> Option<Rect> {
    layout(engine, modal, viewport)
        .into_iter()
        .find(|(hit, _)| *hit == Hit::Objective)
        .map(|(_, rect)| rect)
}

pub(crate) fn field_rect(
    engine: &mut TextEngine,
    modal: &Delegation,
    viewport: (f32, f32),
    hit: Hit,
) -> Option<Rect> {
    layout(engine, modal, viewport)
        .into_iter()
        .find(|(candidate, _)| *candidate == hit)
        .map(|(_, rect)| rect)
}

pub(crate) fn draw(
    modal: &mut Delegation,
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    viewport: (f32, f32),
) {
    // The scrim and the inspector's KoruModal root come from the shared
    // component; what floats on the scrim stays this file's business.
    koru_ui::modal::draw(canvas, root, viewport, |canvas| {
        if creating(modal) {
            creation::draw(modal, canvas, root, engine, viewport);
        } else if matches!(modal.phase, Phase::Editing { .. }) {
            draw_editing(modal, canvas, root, engine, viewport);
        } else {
            let prompt = finished_prompt(modal, crate::format::lang(), &modal.phase);
            let panel = prompt.panel(engine, viewport);
            let focused = modal.focused();
            let hover = modal.hover;
            prompt.draw(
                canvas,
                root,
                engine,
                panel,
                hover.as_ref(),
                focused.as_ref(),
            );
        }
    });
}

/// The editing form: the floating panel, the piece walk and the scrollbar it
/// has always had, clipped to the panel's rounded corner.
fn draw_editing(
    modal: &mut Delegation,
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    viewport: (f32, f32),
) {
    let p = theme::p();
    let panel = panel(engine, modal, viewport);
    // The form is the surface floating on the scrim: one named component
    // around the panel and every piece in it, so the tree reads KoruModal →
    // KoruDelegateForm → the fields and buttons, never a flat run of fills.
    let _form = koru_draw::inspect::scope(
        "KoruDelegateForm",
        "component",
        root,
        scene::rect(panel.x, panel.y, panel.w, panel.h),
        |r| {
            r.property(
                "implementation",
                "mainview::delegate::paint",
                "app surface, not a shared component",
            );
            r.property(
                "width / padding",
                format!("{WIDTH} / {PAD}"),
                "delegate::paint constants",
            );
        },
    );
    scene::floating_panel(
        canvas,
        root,
        panel.x,
        panel.y,
        panel.w,
        panel.h,
        &scene::Panel::popover(RADIUS),
    );

    let focused = modal.focused();
    let hover = modal.hover;
    let content_panel = scrolled_panel(engine, modal, panel);
    let pieces = pieces(engine, modal, content_panel);
    canvas.push_clip_layer(
        vello::peniko::Fill::NonZero,
        root,
        &scene::rrect(panel.x, panel.y, panel.w, panel.h, RADIUS),
    );
    let inner = panel.w - PAD * 2.0;
    let x = panel.x + PAD;
    // The furniture is laid out by the same pass, but only the controls carry
    // rectangles; a label or a sentence follows the piece before it, so the
    // cursor is walked again here rather than stored twice.
    let mut y = content_panel.y + PAD;
    let lang = crate::format::lang();
    for piece in &pieces {
        match piece {
            Piece::Sentence(text) => {
                let layout = engine.simple_layout(text, theme::role::ui());
                scene::draw_layout_first_line(canvas, root, &layout, x, y, Some(p.text));
                y += theme::role::ui().metrics.line_height + GAP;
            }
            Piece::Label(key) => {
                let layout = engine.simple_layout(t(lang, *key), theme::role::section_heading());
                scene::draw_layout_first_line(canvas, root, &layout, x, y, Some(p.text_dim));
                y += ROW_H;
            }
            Piece::Dim(text) => {
                let layout = engine.simple_layout_wrapped(text, theme::role::hint(), inner);
                scene::draw_layout(canvas, root, &layout, x, y, Some(p.text_dim));
                y += layout.height() + 4.0;
            }
            Piece::Guidance { rect, text } => {
                let layout = engine.simple_layout_wrapped(text, theme::role::hint(), rect.w);
                scene::draw_layout(canvas, root, &layout, rect.x, rect.y, Some(p.text_dim));
                y = rect.y + rect.h;
            }
            Piece::Chip {
                rect,
                hit,
                label,
                selected,
            } => {
                Chip::new(label, *selected).draw(
                    canvas,
                    root,
                    engine,
                    *rect,
                    hover == Some(*hit),
                    focused == Some(*hit),
                );
                y = rect.y + rect.h;
            }
            Piece::Field {
                rect,
                hit,
                placeholder,
                invalid,
            } => {
                let field = match hit {
                    Hit::ScheduleName => &mut modal.schedule_name,
                    Hit::ScheduleTime => &mut modal.repeat.time,
                    Hit::ScheduleDay => &mut modal.repeat.day,
                    Hit::ScheduleAdvanced => &mut modal.repeat.advanced,
                    Hit::ScheduleZone => &mut modal.repeat.zone,
                    _ => unreachable!("only schedule text fields use Piece::Field"),
                };
                draw_text_field(
                    canvas,
                    root,
                    engine,
                    field,
                    *rect,
                    t(lang, *placeholder),
                    focused == Some(*hit),
                    *invalid,
                );
                y = rect.y + rect.h;
            }
            Piece::ModelSearch(rect) => {
                modal.models.search.draw(
                    canvas,
                    root,
                    engine,
                    *rect,
                    Some(t(lang, I18nKey::DelegateModelSearch)),
                    focused == Some(Hit::ModelSearch),
                );
                y = rect.y + rect.h;
            }
            Piece::Objective(rect) => {
                modal.objective.draw(
                    canvas,
                    root,
                    engine,
                    *rect,
                    Some(t(lang, I18nKey::DelegateObjectivePlaceholder)),
                    focused == Some(Hit::Objective),
                );
                y = rect.y + rect.h + GAP;
            }
            Piece::Agent {
                rect,
                index,
                text,
                status,
                chosen,
            } => {
                draw_choice(
                    canvas,
                    root,
                    engine,
                    *rect,
                    text,
                    None,
                    *chosen,
                    *status,
                    focused == Some(Hit::Agent(*index)),
                    hover == Some(Hit::Agent(*index)),
                );
                y = rect.y + rect.h;
            }
            Piece::Choice {
                rect,
                hit,
                text,
                detail,
                chosen,
                disabled,
            } => {
                draw_choice(
                    canvas,
                    root,
                    engine,
                    *rect,
                    text,
                    detail.as_deref(),
                    *chosen,
                    *disabled,
                    focused == Some(*hit),
                    hover == Some(*hit),
                );
                y = rect.y + rect.h;
            }
            Piece::Removable {
                rect,
                hit,
                text,
                button,
            } => {
                let layout = engine.simple_layout_wrapped(
                    text,
                    theme::role::row(),
                    (button.x - rect.x - GAP).max(1.0),
                );
                scene::draw_layout(canvas, root, &layout, rect.x, rect.y + 4.0, Some(p.text));
                ActionButton::new(t(lang, I18nKey::DelegateRemove), ActionStyle::Ghost, None)
                    .draw_focused(
                        canvas,
                        root,
                        engine,
                        *button,
                        hover == Some(*hit),
                        focused == Some(*hit),
                    );
                y = rect.y + rect.h;
            }
            Piece::Action {
                rect,
                hit,
                label,
                style,
            } => {
                ActionButton::new(label, *style, None).draw_focused(
                    canvas,
                    root,
                    engine,
                    *rect,
                    hover == Some(*hit),
                    focused == Some(*hit),
                );
                y = rect.y + rect.h;
            }
        }
    }
    canvas.pop_layer();
    if let Some(geom) = scrollbar(engine, modal, viewport) {
        koru_ui::scrollbar::draw(canvas, root, geom, modal.scrollbar_drag.is_some());
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_text_field(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    field: &mut koru_ui::TextField,
    rect: Rect,
    placeholder: &str,
    focused: bool,
    invalid: bool,
) {
    let p = theme::p();
    let shape = scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0);
    scene::fill(canvas, root, p.panel_bg, &shape);
    scene::stroke(
        canvas,
        root,
        if invalid {
            p.close_hover
        } else if focused {
            p.focus_ring
        } else {
            p.panel_border
        },
        if focused {
            theme::BORDER_W * 2.0
        } else {
            theme::BORDER_W
        },
        &shape,
    );
    let ghost = field
        .shows_placeholder()
        .then(|| engine.simple_layout(placeholder, theme::role::ui()));
    koru_ui::text_field::draw_field(
        canvas,
        root,
        field,
        engine,
        ghost.as_ref(),
        (rect.x, rect.y, rect.w, rect.h),
        focused,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_choice(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    rect: Rect,
    text: &str,
    detail: Option<&str>,
    chosen: bool,
    disabled: Option<I18nKey>,
    focused: bool,
    hovered: bool,
) {
    let p = theme::p();
    if chosen {
        scene::fill(
            canvas,
            root,
            p.row_selected,
            &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
        );
    } else if hovered && disabled.is_none() {
        scene::fill(
            canvas,
            root,
            p.hover_tint,
            &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
        );
    }
    if focused {
        scene::stroke(
            canvas,
            root,
            p.focus_ring,
            theme::BORDER_W,
            &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
        );
    }
    let color = if disabled.is_some() {
        p.text_dim
    } else {
        p.text
    };
    let note_width = disabled
        .map(|reason| {
            engine
                .simple_layout(t(crate::format::lang(), reason), theme::role::hint())
                .width()
                + GAP
        })
        .unwrap_or(0.0);
    let layout = engine.simple_layout_wrapped(
        text,
        theme::role::row(),
        (rect.w - 12.0 - note_width).max(1.0),
    );
    scene::draw_layout_first_line(
        canvas,
        root,
        &layout,
        rect.x + 6.0,
        rect.y + 4.0,
        Some(color),
    );
    if let Some(reason) = disabled {
        let lang = crate::format::lang();
        let note = engine.simple_layout(t(lang, reason), theme::role::hint());
        scene::draw_layout_first_line(
            canvas,
            root,
            &note,
            rect.x + rect.w - note.width() - 6.0,
            rect.y + 4.0,
            Some(p.text_dim),
        );
    }
    // **Where that folder is**, under the name. Wrapped to the row and drawn
    // one line deep, so a path longer than the panel is cut rather than
    // spilling over the row beneath it.
    if let Some(detail) = detail {
        let layout =
            engine.simple_layout_wrapped(detail, theme::role::hint(), (rect.w - 12.0).max(1.0));
        scene::draw_layout_first_line(
            canvas,
            root,
            &layout,
            rect.x + 6.0,
            rect.y + ROW_H - 2.0,
            Some(p.text_dim),
        );
    }
}

/// The panel's rectangle at its widest, without measuring — what the dictation
/// indicator hangs from. It over-estimates the height rather than laying the
/// surface out again from an `&self`, which is the honest trade: the indicator
/// wants somewhere to point, not a pixel-exact box.
pub(crate) fn anchor(viewport: (f32, f32)) -> Rect {
    let w = WIDTH.min((viewport.0 - 16.0).max(1.0));
    let h = (viewport.1 * 0.7).min((viewport.1 - 16.0).max(1.0));
    Rect::new(
        ((viewport.0 - w) * 0.5).max(8.0),
        ((viewport.1 - h) * 0.5).max(8.0),
        w,
        h,
    )
}

#[cfg(test)]
mod tests;
