// SPDX-License-Identifier: MPL-2.0

//! **The agent's work on a reply, as one quiet view above its answer.**
//!
//! While the reply runs: *Working for 6s* in grey over a hairline, then what
//! the agent said between calls in the text color and each group of calls as
//! one grey line, the running one saying what it is doing. Once the reply is
//! done, all of that folds into *Worked for 1m 34s ⌄* over the hairline,
//! with the answer below; the header opens it again, by click or by Enter or
//! Space once focused. A group line opens to one line per call, and a call
//! line to the call's own detail. No boxes: the only card in a turn is a
//! pending permission request, which `lib.rs` draws and never folds.
//!
//! What is folded and opened is this surface's, in memory, per message.
//! The summaries are `crate::tool_groups`'.

use std::collections::HashSet;

use koru_ai_core::{MessageId, ToolActivity, ToolInvocationId};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::scene;
use koru_draw::text::TextEngine;
use koru_draw::theme::{self, Brush, role};
use koru_i18n::Lang;
use parley::Layout;
use vello::Scene;
use vello::kurbo::Affine;

use crate::tool_groups::{self, StepGroup, WorkItem};

/// The header's height; its hairline sits at the bottom.
pub(crate) const HEADER_H: f32 = 30.0;
/// The space between lines, and below the last one.
const LINE_GAP: f32 = 6.0;
/// How far a call line, and a call's detail, sit in from their group.
const NEST: f32 = 16.0;
const CARET: f32 = 12.0;

/// What in a work view a pointer or the keyboard can open.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkHit {
    Header(MessageId),
    Group(MessageId, usize),
    Call(ToolInvocationId),
}

pub(crate) struct WorkLine {
    /// From the top of the view.
    pub y: f32,
    pub indent: f32,
    pub layout: Layout<Brush>,
    /// The agent's own words are in the text color; everything else is grey.
    pub said: bool,
    pub hit: Option<WorkHit>,
    /// Whether the line's caret points down (opened) or right.
    pub caret: Option<bool>,
}

pub(crate) struct WorkLayout {
    pub message: MessageId,
    pub top: f32,
    /// Still running: the header ticks and the view cannot fold.
    pub live: bool,
    /// When the reply started, for the ticking header.
    pub started_ms: i64,
    /// The finished header, laid out once.
    pub header: Layout<Brush>,
    pub open: bool,
    pub lines: Vec<WorkLine>,
    pub height: f32,
}

/// What one reply's view is built from.
pub(crate) struct WorkInput<'a> {
    pub message: MessageId,
    pub live: bool,
    pub started_ms: i64,
    pub worked_ms: i64,
    pub items: Vec<WorkItem<'a>>,
    pub groups: Vec<StepGroup<'a>>,
}

/// What this surface has opened, in memory.
pub(crate) struct Opened<'a> {
    pub replies: &'a HashSet<MessageId>,
    pub groups: &'a HashSet<(MessageId, usize)>,
    pub calls: &'a HashSet<ToolInvocationId>,
}

pub(crate) fn build(
    engine: &mut TextEngine,
    lang: Lang,
    width: f32,
    input: WorkInput<'_>,
    opened: &Opened<'_>,
    detail: impl Fn(&ToolActivity) -> String,
) -> WorkLayout {
    let open = input.live || opened.replies.contains(&input.message);
    let header = engine.simple_layout(
        &tool_groups::header(false, input.worked_ms, lang),
        role::ui(),
    );
    let mut lines = Vec::new();
    let mut y = HEADER_H + LINE_GAP;
    if open {
        let mut push = |layout: Layout<Brush>,
                        indent: f32,
                        said: bool,
                        hit: Option<WorkHit>,
                        caret: Option<bool>| {
            let height = layout.height();
            lines.push(WorkLine {
                y,
                indent,
                layout,
                said,
                hit,
                caret,
            });
            y += height + LINE_GAP;
        };
        for item in &input.items {
            match item {
                WorkItem::Text(text) => {
                    let plain = tool_groups::plain_text(text);
                    push(
                        engine.simple_layout_wrapped(&plain, role::ui(), width.max(1.0)),
                        0.0,
                        true,
                        None,
                        None,
                    );
                }
                WorkItem::Group(index) => {
                    let Some(group) = input.groups.get(*index) else {
                        continue;
                    };
                    let group_open = opened.groups.contains(&(input.message, *index));
                    let line = engine.elided(
                        &tool_groups::group_line(group, lang),
                        role::ui(),
                        (width - CARET - 4.0).max(1.0),
                    );
                    push(
                        engine.simple_layout(&line, role::ui()),
                        0.0,
                        false,
                        Some(WorkHit::Group(input.message, *index)),
                        Some(group_open),
                    );
                    if !group_open {
                        continue;
                    }
                    for row in &group.rows {
                        let call_open = opened.calls.contains(&row.id);
                        let line = engine.elided(
                            &tool_groups::call_line(row, lang),
                            role::ui(),
                            (width - NEST - CARET - 4.0).max(1.0),
                        );
                        push(
                            engine.simple_layout(&line, role::ui()),
                            NEST,
                            false,
                            Some(WorkHit::Call(row.id)),
                            Some(call_open),
                        );
                        if call_open {
                            push(
                                engine.simple_layout_wrapped(
                                    &detail(row),
                                    role::meta(),
                                    (width - NEST * 2.0).max(1.0),
                                ),
                                NEST * 2.0,
                                false,
                                None,
                                None,
                            );
                        }
                    }
                }
            }
        }
    }
    let height = if lines.is_empty() { HEADER_H + 4.0 } else { y };
    WorkLayout {
        message: input.message,
        top: 0.0,
        live: input.live,
        started_ms: input.started_ms,
        header,
        open,
        lines,
        height,
    }
}

/// Draw `work` with its top-left at `(x, y)`, `w` wide.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    lang: Lang,
    work: &WorkLayout,
    origin: Rect,
    hover: Option<WorkHit>,
    focused: bool,
    now_ms: i64,
) {
    let p = theme::p();
    let (x, y, w) = (origin.x, origin.y, origin.w);
    let live_header;
    let header = if work.live {
        live_header = engine.simple_layout(
            &tool_groups::header(true, now_ms - work.started_ms, lang),
            role::ui(),
        );
        &live_header
    } else {
        &work.header
    };
    let header_hit = WorkHit::Header(work.message);
    let header_y = y + (HEADER_H - 6.0 - scene::first_line_height(header)) / 2.0;
    scene::draw_layout_first_line(
        canvas,
        root,
        header,
        x,
        header_y,
        Some(if hover == Some(header_hit) {
            p.text
        } else {
            p.text_dim
        }),
    );
    if !work.live {
        let caret = engine.icon_layout(
            if work.open {
                icons::CARET_UP
            } else {
                icons::CARET_DOWN
            },
            CARET,
        );
        scene::draw_layout_first_line(
            canvas,
            root,
            &caret,
            x + header.width() + 6.0,
            header_y + 2.0,
            Some(p.text_dim),
        );
    }
    if focused {
        let ring = header_rect(work, x, y);
        scene::stroke(
            canvas,
            root,
            p.focus_ring,
            theme::BORDER_W,
            &scene::rrect(ring.x - 4.0, ring.y, ring.w + 8.0, ring.h, 5.0),
        );
    }
    // The hairline under the header.
    scene::fill(
        canvas,
        root,
        p.panel_border,
        &scene::rect(x, y + HEADER_H - 1.0, w, theme::BORDER_W as f32),
    );
    for line in &work.lines {
        let color = if line.said || (line.hit.is_some() && hover == line.hit) {
            p.text
        } else {
            p.text_dim
        };
        scene::draw_layout(
            canvas,
            root,
            &line.layout,
            x + line.indent,
            y + line.y,
            Some(color),
        );
        if let Some(open) = line.caret {
            let caret = engine.icon_layout(
                if open {
                    icons::CARET_DOWN
                } else {
                    icons::CARET_RIGHT
                },
                CARET - 2.0,
            );
            scene::draw_layout_first_line(
                canvas,
                root,
                &caret,
                x + line.indent + line.layout.width() + 6.0,
                y + line.y + 2.0,
                Some(p.text_dim),
            );
        }
    }
}

/// What in `work` is under `(px, py)`, with its top-left at `(x, y)`.
pub(crate) fn hit(work: &WorkLayout, x: f32, y: f32, w: f32, px: f32, py: f32) -> Option<WorkHit> {
    if !work.live && Rect::new(x, y, w, HEADER_H).contains(px, py) {
        return Some(WorkHit::Header(work.message));
    }
    work.lines.iter().find_map(|line| {
        let hit = line.hit?;
        Rect::new(
            x + line.indent,
            y + line.y,
            (w - line.indent).max(1.0),
            line.layout.height().max(1.0),
        )
        .contains(px, py)
        .then_some(hit)
    })
}

/// The header's own text and caret, for the focus ring.
fn header_rect(work: &WorkLayout, x: f32, y: f32) -> Rect {
    Rect::new(
        x,
        y + 2.0,
        work.header.width() + 6.0 + CARET,
        HEADER_H - 8.0,
    )
}

#[cfg(test)]
mod tests;
