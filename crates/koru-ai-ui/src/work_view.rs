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
use std::rc::Rc;

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
use crate::tool_output::Ink;
use crate::tool_output::layout::{DetailHit, DetailLine, DetailPicture, color};

/// The header's height; its hairline sits at the bottom.
pub(crate) const HEADER_H: f32 = 30.0;
/// The space between lines, and below the last one.
const LINE_GAP: f32 = 6.0;
/// The space between the lines of a call's detail, and below a block on
/// the code background, which reaches `PANEL_PAD` past its text.
const DETAIL_GAP: f32 = 4.0;
const PANEL_GAP: f32 = 10.0;
const PANEL_PAD: f32 = 4.0;
/// How far a call line, and a call's detail, sit in from their group.
const NEST: f32 = 16.0;
const CARET: f32 = 12.0;

/// What in a work view a pointer or the keyboard can open.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkHit {
    Header(MessageId),
    Group(MessageId, usize),
    Call(ToolInvocationId),
    /// A call's folded block: show all of it, or fold it again.
    Toggle(ToolInvocationId),
    /// One of a call's links, by its place in the call's detail.
    Link(ToolInvocationId, usize),
    /// One of a call's changed files, by its place in the call's detail
    /// (D62).
    Path(ToolInvocationId, usize),
}

pub(crate) struct WorkLine {
    /// From the top of the view.
    pub y: f32,
    pub indent: f32,
    pub layout: Rc<Layout<Brush>>,
    /// The agent's own words are in the text color; everything else is grey.
    pub said: bool,
    pub hit: Option<WorkHit>,
    /// Whether the line's caret points down (opened) or right.
    pub caret: Option<bool>,
    /// A call's detail line: its own ink (`None` inside: the layout's
    /// colors), and whether it sits on the code background.
    pub ink: Option<Option<Ink>>,
    pub panel: bool,
    /// Where a link line goes: a web address, or a changed file's path.
    pub url: Option<String>,
    /// A picture a call read, drawn in the line's place (D38).
    pub picture: Option<DetailPicture>,
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

impl WorkLine {
    fn plain(
        layout: Layout<Brush>,
        indent: f32,
        hit: Option<WorkHit>,
        caret: Option<bool>,
    ) -> Self {
        Self {
            y: 0.0,
            indent,
            layout: Rc::new(layout),
            said: false,
            hit,
            caret,
            ink: None,
            panel: false,
            url: None,
            picture: None,
        }
    }

    fn said(self) -> Self {
        Self { said: true, ..self }
    }

    /// How tall the line is drawn: its picture's height, else its text's.
    fn height(&self) -> f32 {
        self.picture
            .as_ref()
            .map_or_else(|| self.layout.height(), |picture| picture.height)
    }
}

/// Lines stacked down the view, each `gap` above the next.
struct Stack {
    lines: Vec<WorkLine>,
    y: f32,
}

impl Stack {
    fn push(&mut self, line: WorkLine, gap: f32) {
        let height = line.height();
        self.lines.push(WorkLine { y: self.y, ..line });
        self.y += height + gap;
    }
}

/// A call's detail, laid out `width` wide: `crate::tool_output`'s lines,
/// which the surface keeps until the call changes.
pub(crate) type Detail<'a> =
    dyn FnMut(&mut TextEngine, &ToolActivity, f32) -> Rc<Vec<DetailLine>> + 'a;

pub(crate) fn build(
    engine: &mut TextEngine,
    lang: Lang,
    width: f32,
    input: WorkInput<'_>,
    opened: &Opened<'_>,
    detail: &mut Detail<'_>,
) -> WorkLayout {
    let open = input.live || opened.replies.contains(&input.message);
    let header = engine.simple_layout(
        &tool_groups::header(false, input.worked_ms, lang),
        role::ui(),
    );
    let mut stack = Stack {
        lines: Vec::new(),
        y: HEADER_H + LINE_GAP,
    };
    if open {
        for item in &input.items {
            match item {
                WorkItem::Text(text) => {
                    let plain = tool_groups::plain_text(text);
                    stack.push(
                        WorkLine::plain(
                            engine.simple_layout_wrapped(&plain, role::ui(), width.max(1.0)),
                            0.0,
                            None,
                            None,
                        )
                        .said(),
                        LINE_GAP,
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
                    stack.push(
                        WorkLine::plain(
                            engine.simple_layout(&line, role::ui()),
                            0.0,
                            Some(WorkHit::Group(input.message, *index)),
                            Some(group_open),
                        ),
                        LINE_GAP,
                    );
                    if !group_open {
                        continue;
                    }
                    for row in &group.rows {
                        let call_open = opened.calls.contains(&row.id);
                        let line = engine.elided(
                            &tool_groups::step_call_line(group, row, lang),
                            role::ui(),
                            (width - NEST - CARET - 4.0).max(1.0),
                        );
                        stack.push(
                            WorkLine::plain(
                                engine.simple_layout(&line, role::ui()),
                                NEST,
                                Some(WorkHit::Call(row.id)),
                                Some(call_open),
                            ),
                            LINE_GAP,
                        );
                        if call_open {
                            let detail = detail(engine, row, (width - NEST * 2.0).max(1.0));
                            for (index, line) in detail.iter().enumerate() {
                                let hit = line.hit.map(|hit| match hit {
                                    DetailHit::Toggle => WorkHit::Toggle(row.id),
                                    DetailHit::Link(index) => WorkHit::Link(row.id, index),
                                    DetailHit::Path(index) => WorkHit::Path(row.id, index),
                                });
                                let gap = match detail.get(index + 1) {
                                    None => LINE_GAP + line.panel as u8 as f32 * PANEL_PAD,
                                    Some(next) if line.panel || next.panel => PANEL_GAP,
                                    Some(_) => DETAIL_GAP,
                                };
                                stack.push(
                                    WorkLine {
                                        y: 0.0,
                                        indent: NEST * 2.0,
                                        layout: Rc::clone(&line.layout),
                                        said: false,
                                        hit,
                                        caret: None,
                                        ink: Some(line.ink),
                                        panel: line.panel,
                                        url: line.url.clone(),
                                        picture: line.picture.clone(),
                                    },
                                    gap,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    let Stack { lines, y } = stack;
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

/// Draws the picture the image cache keeps under a key, fitted in a rect
/// (`crate::ChatSurface::images`).
pub(crate) type PictureDrawer<'a> = dyn FnMut(&mut Scene, Affine, &str, Rect) + 'a;

/// How wide the light that sweeps *Working for …* is, either side of its
/// centre (koru-ai D83).
const SWEEP_HALF_W: f32 = 28.0;

/// Draw `work` with its top-left at `(x, y)`, `w` wide; a call's picture
/// through `picture`. `sweep` is how far the light has crossed a running
/// reply's header, `0..1`; `None` draws it still (reduced motion).
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
    sweep: Option<f32>,
    picture: &mut PictureDrawer<'_>,
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
    // **The light across *Working for 25s*** while the reply runs: from
    // before the words' start to past their end, then again.
    if work.live
        && let Some(phase) = sweep
    {
        let travel = header.width() + SWEEP_HALF_W * 4.0;
        scene::draw_layout_first_line_sweep(
            canvas,
            root,
            header,
            x,
            header_y,
            p.text,
            x - SWEEP_HALF_W * 2.0 + travel * phase,
            SWEEP_HALF_W,
        );
    }
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
        if let Some(shown) = &line.picture {
            picture(
                canvas,
                root,
                &shown.key,
                Rect::new(x + line.indent, y + line.y, shown.width, shown.height),
            );
            continue;
        }
        if line.panel {
            scene::fill(
                canvas,
                root,
                p.code_bg,
                &scene::rrect(
                    x + line.indent - PANEL_PAD * 2.0,
                    y + line.y - PANEL_PAD,
                    (w - line.indent + PANEL_PAD * 2.0).max(1.0),
                    line.layout.height() + PANEL_PAD * 2.0,
                    5.0,
                ),
            );
        }
        let hovered = line.hit.is_some() && hover == line.hit;
        let ink = match line.ink {
            // A call's detail: its own ink, or the layout's colors.
            Some(Some(Ink::Link)) if hovered => Some(p.text),
            Some(ink) => ink.map(color),
            None if line.said || hovered => Some(p.text),
            None => Some(p.text_dim),
        };
        scene::draw_layout(canvas, root, &line.layout, x + line.indent, y + line.y, ink);
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
            line.height().max(1.0),
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
