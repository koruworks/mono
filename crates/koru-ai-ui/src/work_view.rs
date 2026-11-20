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
use koru_draw::theme::tokens::{Profile, Radius, Space};
use koru_draw::theme::{self, Brush, role};
use koru_i18n::{Key as TextKey, Lang, t};
use koru_ui::icon_button::{IconButton, State as IconButtonState};
use parley::Layout;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

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
const COPY_SIZE: f32 = Space::S7.px(Profile::Desktop);
const COPY_GAP: f32 = Space::S1.px(Profile::Desktop);

/// Presentation only. The host owns the shared animation clock.
#[derive(Clone)]
pub(crate) struct WorkingBot {
    pub face: u8,
    pub name: String,
    pub pose: koru_draw::face::FacePose,
}

impl WorkingBot {
    pub fn draw(&self, canvas: &mut Scene, root: Affine, engine: &mut TextEngine, at: Rect) {
        koru_draw::face::Face::new(self.face, &self.name)
            .posed(self.pose)
            .draw(canvas, root, engine, at);
    }
}

/// What in a work view a pointer or the keyboard can open.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum WorkHit {
    Header(MessageId),
    Group(MessageId, usize),
    Call(ToolInvocationId),
    /// Copy the complete stored output, even while its displayed lines fold.
    CopyOutput(ToolInvocationId),
    /// A call's folded block: show all of it, or fold it again.
    Toggle(ToolInvocationId),
    /// One of a call's links, by its place in the call's detail.
    Link(ToolInvocationId, usize),
    /// One of a call's changed files, by its place in the call's detail
    /// (D62).
    Path(ToolInvocationId, usize),
}

#[derive(Clone)]
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
    /// The opened call's trailing copy action, beside its title.
    pub copy_output: Option<ToolInvocationId>,
    /// A call's detail line: its own ink (`None` inside: the layout's
    /// colors), and whether it sits on the code background.
    pub ink: Option<Option<Ink>>,
    pub panel: bool,
    /// Where a link line goes: a web address, or a changed file's path.
    pub url: Option<String>,
    /// A picture a call read, drawn in the line's place (D38).
    pub picture: Option<DetailPicture>,
}

#[derive(Clone)]
pub(crate) struct WorkLayout {
    pub message: MessageId,
    pub top: f32,
    /// Still running: the header ticks and the view cannot fold.
    pub live: bool,
    /// Bot conversations keep prose visible and disclose work below it.
    pub bot: bool,
    /// When the reply started, for the ticking header.
    pub started_ms: i64,
    /// The finished header, laid out once.
    pub header: Rc<Layout<Brush>>,
    pub header_text: String,
    pub open: bool,
    pub lines: Vec<WorkLine>,
    pub height: f32,
}

/// What one reply's view is built from.
pub(crate) struct WorkInput<'a> {
    pub message: MessageId,
    pub live: bool,
    pub bot: bool,
    pub progress: crate::bot_presentation::Progress,
    pub started_ms: i64,
    pub worked_ms: i64,
    pub items: Vec<WorkItem<'a>>,
    pub groups: Vec<StepGroup<'a>>,
    /// Existing UI display caches only; resolving a status never fetches data.
    pub names: crate::bot_presentation::ActivityNames<'a>,
    /// Includes actual discovery calls hidden from the expanded work history.
    pub activity: Option<&'a ToolActivity>,
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
            copy_output: None,
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
    pub(crate) fn height(&self) -> f32 {
        self.picture
            .as_ref()
            .map_or_else(|| self.layout.height(), |picture| picture.height)
            .max(if self.copy_output.is_some() {
                COPY_SIZE
            } else {
                0.0
            })
    }

    fn rect(&self, x: f32, y: f32, w: f32) -> Rect {
        Rect::new(
            x + self.indent,
            y + self.y,
            (w - self.indent).max(0.0),
            self.height(),
        )
    }

    fn copy_button(&self, x: f32, y: f32, w: f32) -> Option<IconButton<WorkHit>> {
        let call = self.copy_output?;
        let row = self.rect(x, y, w);
        let width = COPY_SIZE.min(row.w);
        (width > 0.0).then(|| {
            IconButton::new(
                icons::COPY,
                Rect::new(row.x + row.w - width, row.y, width, row.h),
            )
            .on_click(WorkHit::CopyOutput(call))
        })
    }

    pub(crate) fn action_rect(&self, x: f32, y: f32, w: f32) -> Rect {
        let mut rect = self.rect(x, y, w);
        if let Some(button) = self.copy_button(x, y, w) {
            rect.w = (button.rect().x - rect.x - COPY_GAP).max(0.0);
        }
        rect
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
    let open = !input.bot && (input.live || opened.replies.contains(&input.message));
    let header_text = if input.bot {
        crate::bot_presentation::status_text(lang, &input)
    } else {
        tool_groups::header(false, input.worked_ms, lang)
    };
    let header = Rc::new(if input.bot {
        crate::bot_presentation::status_label(engine, width, input.live, &header_text)
    } else {
        engine.simple_layout(&header_text, role::ui())
    });
    let header_height = if input.bot {
        if input.live {
            crate::bot_presentation::STATUS_H
        } else {
            0.0
        }
    } else {
        HEADER_H
    };
    let mut stack = Stack {
        lines: Vec::new(),
        y: header_height + LINE_GAP,
    };
    if open {
        for item in &input.items {
            match item {
                WorkItem::Text(text) => {
                    if input.bot {
                        continue;
                    }
                    // What the agent said is the chat's text, in its font;
                    // the grey summaries around it are the chrome's.
                    let plain = tool_groups::plain_text(text);
                    stack.push(
                        WorkLine::plain(
                            crate::chat_text_layout(engine, &plain, width.max(1.0)),
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
                        let copy_output = (call_open
                            && crate::tool_output::copy_text(row).is_some())
                        .then_some(row.id);
                        let copy_space = if copy_output.is_some() {
                            COPY_SIZE + COPY_GAP
                        } else {
                            0.0
                        };
                        let line = engine.elided(
                            &tool_groups::step_call_line(group, row, lang),
                            role::ui(),
                            (width - NEST - CARET - 4.0 - copy_space).max(1.0),
                        );
                        stack.push(
                            WorkLine {
                                copy_output,
                                ..WorkLine::plain(
                                    engine.simple_layout(&line, role::ui()),
                                    NEST,
                                    Some(WorkHit::Call(row.id)),
                                    Some(call_open),
                                )
                            },
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
                                        copy_output: None,
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
    let height = if lines.is_empty() {
        if input.bot {
            header_height
        } else {
            HEADER_H + 4.0
        }
    } else {
        y
    };
    WorkLayout {
        message: input.message,
        top: 0.0,
        live: input.live,
        bot: input.bot,
        started_ms: input.started_ms,
        header,
        header_text,
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

/// One paint-only animation shared by ordinary work headers and Bot activity.
pub(crate) fn draw_sweep(
    canvas: &mut Scene,
    root: Affine,
    header: &Layout<Brush>,
    x: f32,
    y: f32,
    phase: Option<f32>,
) {
    if let Some(phase) = phase {
        let travel = header.width() + SWEEP_HALF_W * 4.0;
        scene::draw_layout_first_line_sweep(
            canvas,
            root,
            header,
            x,
            y,
            theme::p().text,
            x - SWEEP_HALF_W * 2.0 + travel * phase,
            SWEEP_HALF_W,
        );
    }
}

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
    focused: Option<WorkHit>,
    now_ms: i64,
    sweep: Option<f32>,
    bot: Option<&WorkingBot>,
    picture: &mut PictureDrawer<'_>,
) {
    let p = theme::p();
    let (x, y, w) = (origin.x, origin.y, origin.w);
    if work.bot {
        crate::bot_presentation::draw_status(
            canvas,
            root,
            engine,
            work,
            Rect::new(x, y, w, crate::bot_presentation::STATUS_H),
            bot,
            hover,
            focused,
            sweep,
        );
    } else {
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
        let header_x = if let Some(bot) = bot.filter(|_| work.live) {
            bot.draw(canvas, root, engine, Rect::new(x, y + 2.0, 20.0, 20.0));
            x + 26.0
        } else {
            x
        };
        scene::draw_layout_first_line(
            canvas,
            root,
            header,
            header_x,
            header_y,
            Some(if hover == Some(header_hit) {
                p.text
            } else {
                p.text_dim
            }),
        );
        // **The light across *Working for 25s*** while the reply runs: from
        // before the words' start to past their end, then again.
        if work.live {
            draw_sweep(canvas, root, header, header_x, header_y, sweep);
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
        if focused == Some(header_hit) {
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
    }
    draw_lines(
        canvas, root, engine, lang, work, origin, hover, focused, picture,
    );
}

/// The retained details painter, shared by inline work and the Bot dialog.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_lines(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    lang: Lang,
    work: &WorkLayout,
    origin: Rect,
    hover: Option<WorkHit>,
    focused: Option<WorkHit>,
    picture: &mut PictureDrawer<'_>,
) {
    let p = theme::p();
    let (x, y, w) = (origin.x, origin.y, origin.w);
    for line in &work.lines {
        let row = line.rect(x, y, w);
        let _row = line.caret.map(|_| {
            koru_draw::inspect::scope(
                "Tool call row",
                "row",
                root,
                scene::rect(row.x, row.y, row.w, row.h),
                |_| {},
            )
        });
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
        let text_y = y + line.y + (line.height() - line.layout.height()) * 0.5;
        scene::draw_layout(canvas, root, &line.layout, x + line.indent, text_y, ink);
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
                y + line.y + (line.height() - caret.height()) * 0.5,
                Some(p.text_dim),
            );
        }
        if line.hit.is_some() && focused == line.hit {
            draw_focus(canvas, root, line.action_rect(x, y, w));
        }
        if let Some(button) = line.copy_button(x, y, w) {
            let rect = button.rect();
            let action = line.copy_output.map(WorkHit::CopyOutput);
            let _copy = koru_draw::inspect::scope(
                "Copy tool output",
                "button",
                root,
                scene::rect(rect.x, rect.y, rect.w, rect.h),
                |record| {
                    record.property(
                        "accessible name",
                        t(lang, TextKey::MenuCopy),
                        "koru_i18n::Key::MenuCopy",
                    );
                    record.property(
                        "target",
                        t(lang, TextKey::AiToolOutput),
                        "koru_i18n::Key::AiToolOutput",
                    );
                    record.property("slot", "S7", "theme::tokens::Space::S7");
                },
            );
            button
                .with_state(IconButtonState {
                    hovered: hover == action,
                    active: false,
                    disabled: false,
                })
                .draw(canvas, root, engine);
            if focused == action {
                draw_focus(canvas, root, rect);
            }
        }
    }
}

/// What in `work` is under `(px, py)`, with its top-left at `(x, y)`.
pub(crate) fn hit(work: &WorkLayout, x: f32, y: f32, w: f32, px: f32, py: f32) -> Option<WorkHit> {
    if work.bot {
        return None;
    }
    if !work.bot && !work.live && Rect::new(x, y, w, HEADER_H).contains(px, py) {
        return Some(WorkHit::Header(work.message));
    }
    work.lines.iter().find_map(|line| {
        if let Some(action) = line
            .copy_button(x, y, w)
            .and_then(|button| button.action_at(px, py))
        {
            return Some(action);
        }
        let hit = line.hit?;
        line.action_rect(x, y, w).contains(px, py).then_some(hit)
    })
}

fn draw_focus(canvas: &mut Scene, root: Affine, rect: Rect) {
    scene::stroke(
        canvas,
        root,
        theme::p().focus_ring,
        theme::BORDER_W,
        &scene::rrect(
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            Radius::M.px(Profile::Desktop, rect.w, rect.h),
        ),
    );
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

impl crate::ChatSurface {
    pub(crate) fn work_focus(&self) -> Option<WorkHit> {
        match self.focus {
            crate::Focus::Work(message) => Some(WorkHit::Header(message)),
            crate::Focus::WorkControl(hit) => Some(hit),
            _ => None,
        }
    }

    pub(crate) fn focus_work(&mut self, hit: WorkHit) {
        self.focus = match hit {
            WorkHit::Header(message) => crate::Focus::Work(message),
            _ => crate::Focus::WorkControl(hit),
        };
        self.composer.set_focused(false);
    }

    fn work_controls(&mut self) -> Vec<(WorkHit, f32, f32)> {
        let area = self.conversation_rect();
        if self.messages_dirty || (self.message_width - area.w).abs() > 0.5 {
            self.rebuild_messages(area.w);
        }
        if self.bot_chat_flow() {
            return self
                .message_layouts
                .iter()
                .filter_map(|message| {
                    let id = message.id?;
                    self.bot_work_replies.contains(&id).then(|| {
                        let rect = self.bot_work_anchor_for(message);
                        (
                            WorkHit::Header(id),
                            rect.y - area.y + self.conversation_scroll,
                            rect.h,
                        )
                    })
                })
                .collect();
        }
        self.work_layouts
            .iter()
            .flat_map(|work| {
                let header = (!work.live || work.bot).then_some((
                    WorkHit::Header(work.message),
                    work.top,
                    if work.bot {
                        crate::bot_presentation::STATUS_H
                    } else {
                        HEADER_H
                    },
                ));
                header
                    .into_iter()
                    .chain(work.lines.iter().flat_map(move |line| {
                        [line.hit, line.copy_output.map(WorkHit::CopyOutput)]
                            .into_iter()
                            .flatten()
                            .map(move |hit| (hit, work.top + line.y, line.height()))
                    }))
            })
            .collect()
    }

    fn reveal_work_control(&mut self, (hit, top, height): (WorkHit, f32, f32)) {
        self.focus_work(hit);
        self.follow_tail = false;
        let area = self.conversation_rect();
        if top < self.conversation_scroll {
            self.conversation_scroll = top;
        } else if top + height > self.conversation_scroll + area.h {
            self.conversation_scroll = (top + height - area.h).max(0.0);
        }
    }

    pub(crate) fn focus_last_work(&mut self) -> bool {
        let Some(control) = self.work_controls().last().copied() else {
            return false;
        };
        self.reveal_work_control(control);
        true
    }

    pub(crate) fn copy_tool_output(&mut self, call: ToolInvocationId) -> bool {
        let Some(text) = self
            .transcript
            .as_ref()
            .and_then(|transcript| transcript.tool_activities.iter().find(|row| row.id == call))
            .and_then(crate::tool_output::copy_text)
            .map(str::to_owned)
        else {
            return false;
        };
        // The dependency is not compiled with cfg(test) for this crate's
        // suite, so keep the real clipboard out of the surface tests here.
        #[cfg(test)]
        crate::tests::COPIED.with(|copied| copied.borrow_mut().push(text.clone()));
        #[cfg(not(test))]
        koru_ui::clipboard::put_plain(&text);
        self.show_copy_feedback(t(self.lang, TextKey::AiLinkCopied));
        true
    }

    /// The work header and its currently visible controls share one Tab
    /// order. Enter/Space activates the focused control; Escape returns to
    /// the composer. Opening a group makes its calls reachable immediately.
    pub(crate) fn work_key(&mut self, key: &Key, mods: ModifiersState) -> Option<bool> {
        let current = self.work_focus()?;
        if matches!(key, Key::Named(NamedKey::Tab))
            && (mods.is_empty() || mods == ModifiersState::SHIFT)
        {
            let controls = self.work_controls();
            let next = controls
                .iter()
                .position(|(hit, _, _)| *hit == current)
                .and_then(|index| {
                    if mods.shift_key() {
                        index.checked_sub(1)
                    } else {
                        Some(index + 1)
                    }
                })
                .and_then(|index| controls.get(index))
                .copied();
            if let Some(control) = next {
                self.reveal_work_control(control);
            } else {
                self.focus = crate::Focus::Composer;
                self.composer.set_focused(true);
            }
            return Some(true);
        }
        if !mods.is_empty() {
            return None;
        }
        match key {
            Key::Named(NamedKey::Enter | NamedKey::Space) => Some(self.toggle_work(current)),
            Key::Character(value) if value.as_str() == " " => Some(self.toggle_work(current)),
            Key::Named(NamedKey::Escape) => {
                self.focus = crate::Focus::Composer;
                self.composer.set_focused(true);
                Some(true)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
