// SPDX-License-Identifier: MPL-2.0

//! Every rectangle on the page, measured before anything is drawn.
//!
//! **One geometry.** [`measure`] produces a [`Layout`]; the draw pass paints
//! from it and `WelcomeView::target_at` hit-tests against its `hits`. Neither
//! computes a position of its own, so a control cannot be pressable somewhere
//! other than where it is painted. Coordinates are content space: the scroll
//! offset is applied by the caller.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;
use koru_ui::card::CardRow;
use koru_ui::swatch::Swatch;
use koru_ui::text_paragraph::KoruTextParagraph;

use super::rows::{RowId, Section};
use super::{Target, WelcomeView};

/// Widest the column ever draws.
pub(super) const COLUMN_W: f32 = 640.0;
pub(super) const TOP: f32 = 64.0;
/// Between the Customize card and each section.
pub(super) const SECTION_GAP: f32 = 32.0;
/// A section's label line and the air under it.
pub(super) const LABEL_H: f32 = 16.0;
const LABEL_GAP: f32 = 10.0;
/// The tick circle.
pub(super) const TICK: f32 = 19.0;
const TICK_GAP: f32 = 12.0;
const SENTENCE_GAP: f32 = 2.0;
const CHIPS_GAP: f32 = 10.0;
const HINTS_GAP: f32 = 8.0;
const CODE_GAP: f32 = 8.0;
pub(super) const HINT_LINE_H: f32 = 24.0;
pub(super) const HINT_CHORD_W: f32 = 112.0;
/// The key / command chip at a row's right end.
pub(super) const KEY_H: f32 = koru_ui::KeyChip::H;
const CARET_W: f32 = 16.0;
/// The pairing square, at most: a pointer to *Link another device*, never
/// the code itself, so it is drawn the row's height or a little less and the
/// card stays one row tall.
pub(super) const QR: f32 = 44.0;
/// The least air above and below the pairing square, inside its row.
const QR_INSET: f32 = 8.0;
const BOTTOM_PAD: f32 = 64.0;

pub(super) fn title_style() -> theme::TextStyle {
    theme::role::ui()
}

pub(super) fn sentence_style() -> theme::TextStyle {
    theme::role::meta()
}

pub(super) struct Layout {
    pub column: Rect,
    pub customize: CustomizeLayout,
    pub sections: Vec<SectionLayout>,
    /// Every pressable rectangle in visual order — which is also the Tab
    /// order. A target may appear more than once (a link that wrapped).
    pub hits: Vec<(Target, Rect)>,
    /// The bottom of the page, padding included: the scroll range.
    pub bottom: f32,
}

pub(super) struct CustomizeLayout {
    pub card: Rect,
    pub swatches: Vec<Rect>,
    /// The text half of the row, swatches excluded.
    pub row: Rect,
    pub text_w: f32,
    pub button: Rect,
}

pub(super) struct SectionLayout {
    pub section: Section,
    pub label_y: f32,
    pub progress: Option<(String, Rect)>,
    pub card: Rect,
    pub dividers: Vec<f32>,
    pub rows: Vec<RowLayout>,
    /// The pairing square: what is drawn and what is pressed.
    pub qr: Option<Rect>,
}

/// The MCP row's agent chips: one line, scrolled sideways inside `window`
/// when it is wider than the row.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct AgentStrip {
    /// What is shown of the line; a chip is clipped to it.
    pub window: Rect,
    /// How far the line is scrolled, clamped to `0..=overflow`.
    pub scroll: f32,
    /// How much wider the line is than `window`.
    pub overflow: f32,
}

impl AgentStrip {
    /// What of `chip` shows through the window — and is pressed. Empty for a
    /// chip scrolled out of it.
    pub fn visible(&self, chip: Rect) -> Rect {
        let left = chip.x.max(self.window.x);
        let right = (chip.x + chip.w).min(self.window.x + self.window.w);
        Rect::new(left, chip.y, (right - left).max(0.0), chip.h)
    }
}

pub(super) struct RowLayout {
    pub id: RowId,
    pub rect: Rect,
    pub tick: Option<Rect>,
    pub title: &'static str,
    pub title_rect: Rect,
    pub sentence: Vec<Run>,
    pub trailing: Trailing,
    pub chips: Vec<(&'static str, String, Rect)>,
    pub hints: Vec<HintLine>,
    /// A command to copy, in a code chip under the sentence: what is drawn,
    /// shortened in the middle when it does not fit.
    pub code: Option<(String, Rect)>,
    /// The MCP row's agents, one chip each where it is drawn (scrolled), and
    /// whether it is the one chosen.
    pub agents: Vec<(super::mcp::Agent, Rect, bool)>,
    /// The line [`agents`](Self::agents) sit on.
    pub agents_strip: Option<AgentStrip>,
}

/// A stretch of one sentence line in one style.
pub(super) struct Run {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    /// `Some` for a link.
    pub link: Option<Target>,
}

impl Run {
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.w, sentence_style().metrics.line_height)
    }
}

pub(super) enum Trailing {
    None,
    /// A shortcut, from the registry.
    Key(String, Rect),
    Button(&'static str, Target, Rect),
    /// `›` closed, `⌄` open.
    Caret(bool, Rect),
    /// A command to type, drawn as a code chip.
    Code(&'static str, Rect),
}

pub(super) struct HintLine {
    pub chord: String,
    pub what: &'static str,
    pub y: f32,
}

/// What one row says, before it is measured.
struct RowCopy {
    title: &'static str,
    sentence: Vec<(String, Option<Target>)>,
}

fn copy(row: RowId, workspace: &str) -> RowCopy {
    use super::*;
    let plain = |s: &str| vec![(s.to_owned(), None)];
    let (title, sentence) = match row {
        RowId::FirstDocument => (
            FIRST_DOCUMENT_TITLE,
            vec![
                (FIRST_DOCUMENT_BEFORE.to_owned(), None),
                (FIRST_DOCUMENT_MIC.to_owned(), Some(Target::MicLink)),
                (FIRST_DOCUMENT_AFTER.to_owned(), None),
            ],
        ),
        RowId::Documents => (DOCUMENTS_TITLE, plain(DOCUMENTS_HINT)),
        RowId::Files => (FILES_TITLE, plain(&files_hint(workspace))),
        RowId::Learn => (LEARN_TITLE, plain(LEARN_HINT)),
        RowId::AppMenu => (
            koru_i18n::t(crate::format::lang(), koru_i18n::Key::WelcomeAppMenu),
            plain(koru_i18n::t(
                crate::format::lang(),
                koru_i18n::Key::SettingsShowInAppMenuHint,
            )),
        ),
        RowId::TaskList => (TASK_LIST_TITLE, Vec::new()),
        RowId::Reminder => (REMINDER_TITLE, plain(REMINDER_HINT)),
        RowId::Feedback => (
            FEEDBACK_TITLE,
            vec![
                (FEEDBACK_BEFORE.to_owned(), None),
                (FEEDBACK_FORM.to_owned(), Some(Target::FeedbackForm)),
                (FEEDBACK_OR.to_owned(), None),
                (FEEDBACK_EMAIL.to_owned(), Some(Target::Email)),
                (FEEDBACK_AFTER.to_owned(), None),
            ],
        ),
        RowId::Phone => (PHONE_TITLE, plain(PHONE_HINT)),
        RowId::Computer => (COMPUTER_TITLE, plain(COMPUTER_HINT)),
        RowId::Watch => (WATCH_TITLE, plain(WATCH_HINT)),
        RowId::Ai => (AI_TITLE, plain(AI_HINT)),
        RowId::Mcp => (MCP_TITLE, plain(MCP_HINT)),
        RowId::TerminalAgent => (AGENT_TITLE, plain(AGENT_HINT)),
        RowId::OwnApps => (APPS_TITLE, plain(APPS_HINT)),
    };
    RowCopy { title, sentence }
}

/// Whether the whole row is the row's action.
pub(super) fn row_is_target(row: RowId) -> bool {
    matches!(
        row,
        RowId::FirstDocument
            | RowId::Learn
            | RowId::AppMenu
            | RowId::Reminder
            | RowId::Phone
            | RowId::Computer
            | RowId::Ai
            | RowId::TerminalAgent
    )
}

/// The width of a key or code chip for `text` — the shared [`koru_ui::KeyChip`].
pub(super) fn key_w(engine: &mut TextEngine, text: &str) -> f32 {
    koru_ui::KeyChip::width(engine, text)
}

/// The command field's inset either side of its text.
pub(super) const FIELD_PAD_X: f32 = 10.0;

/// How wide the command field is around `text`: set like the *Copy* button
/// beside it ([`theme::role::row`]), so the two read as one line.
pub(super) fn field_w(engine: &mut TextEngine, text: &str) -> f32 {
    engine.simple_layout(text, theme::role::row()).width() + FIELD_PAD_X * 2.0
}

/// `text` cut in the middle, with an ellipsis, to fit a command field no
/// wider than `max_w`. A path's end says the most — the bridge's name and its
/// flags — so the end keeps two thirds of what is kept.
pub(super) fn elide_middle(engine: &mut TextEngine, text: &str, max_w: f32) -> String {
    if field_w(engine, text) <= max_w {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |keep: usize| {
        let head = keep / 3;
        let tail = keep - head;
        let mut out: String = chars[..head].iter().collect();
        out.push('\u{2026}');
        out.extend(&chars[chars.len() - tail..]);
        out
    };
    // The most characters that still fit: `lo` always fits, `hi` never does.
    let (mut lo, mut hi) = (0, chars.len());
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if field_w(engine, &cut(mid)) <= max_w {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    cut(lo)
}

/// Lay a sentence of styled segments into lines no wider than `width`,
/// wrapping between words. A piece that follows another with no space
/// between (a comma after a link) never starts a line.
fn flow(
    engine: &mut TextEngine,
    segments: &[(String, Option<Target>)],
    x: f32,
    y: f32,
    width: f32,
) -> (Vec<Run>, f32) {
    let style = sentence_style();
    let line_h = style.metrics.line_height;
    let space =
        engine.simple_layout("a b", style).width() - engine.simple_layout("ab", style).width();
    let right = x + width;
    let mut runs: Vec<Run> = Vec::new();
    let mut cx = x;
    let mut line = 0usize;
    let mut glued = false;
    for (text, link) in segments {
        for piece in text.split_inclusive(' ') {
            let word = piece.trim_end_matches(' ');
            let trailing = piece.len() - word.len();
            let w = if word.is_empty() {
                0.0
            } else {
                engine.simple_layout(word, style).width()
            };
            if !glued && cx > x && cx + w > right {
                line += 1;
                cx = x;
            }
            let ry = y + line as f32 * line_h;
            let extends = runs
                .last()
                .is_some_and(|run| run.link == *link && (run.y - ry).abs() < 0.5);
            if extends {
                let run = runs.last_mut().expect("checked above");
                run.text.push_str(piece);
                run.w = cx + w - run.x;
            } else if !word.is_empty() || trailing > 0 {
                runs.push(Run {
                    text: piece.to_owned(),
                    x: cx,
                    y: ry,
                    w,
                    link: *link,
                });
            }
            cx += w + space * trailing as f32;
            glued = trailing == 0;
        }
    }
    // A run's text may end in spaces; its width never includes them.
    for run in &mut runs {
        let trimmed = run.text.trim_end().len();
        run.text.truncate(trimmed);
    }
    runs.retain(|run| !run.text.is_empty());
    let height = if runs.is_empty() {
        0.0
    } else {
        (line + 1) as f32 * line_h
    };
    (runs, height)
}

/// Measure the page as `view` stands now.
pub(super) fn measure(view: &mut WelcomeView) -> Layout {
    // Everything the measure reads from the view, taken before the engine is
    // borrowed for the rest of the function.
    let (vw, _) = view.viewport;
    let identity = view.identity.clone();
    let learn_open = view.learn_open;
    let mcp_command = view.mcp_command.clone();
    let mcp_agent = view.mcp_agent;
    let agents_scroll = view.agents_scroll;
    let progress: Vec<(Section, (usize, usize))> = Section::ALL
        .into_iter()
        .map(|s| (s, view.progress_of(s)))
        .collect();
    let engine = &mut view.engine;

    let w = COLUMN_W.min((vw - theme::MARGIN_X * 2.0).max(0.0));
    let x = ((vw - w) * 0.5).max(0.0);
    let column = Rect::new(x, TOP, w, 0.0);
    let mut hits: Vec<(Target, Rect)> = Vec::new();

    // No header for now: the page opens on the Customize card.
    let mut y = TOP;

    // ---------------------------------------------------------- customize
    let swatch_w = Swatch::row_width(3);
    let row_x = x + CardRow::PAD_X + swatch_w;
    let row_w = (w - CardRow::PAD_X - swatch_w).max(0.0);
    let button_w = koru_ui::button::button_width(engine, super::CUSTOMIZE_BUTTON);
    let customize_text_w = CardRow::text_w(row_w, button_w);
    let customize_h = CardRow::new(super::CUSTOMIZE_TITLE)
        .hint(super::CUSTOMIZE_HINT)
        .height(engine, customize_text_w, koru_ui::button::H);
    let card = Rect::new(x, y, w, customize_h);
    let swatches = Swatch::row(
        x + CardRow::PAD_X,
        y + (customize_h - Swatch::SIZE) * 0.5,
        3,
    );
    let row = Rect::new(row_x, y, row_w, customize_h);
    let button = CardRow::control_slot(row, button_w, koru_ui::button::H);
    hits.push((Target::Customize, button));
    let customize = CustomizeLayout {
        card,
        swatches,
        row,
        text_w: customize_text_w,
        button,
    };
    y += customize_h + SECTION_GAP;

    // ----------------------------------------------------------- sections
    let mut sections = Vec::new();
    for (section, (n, m)) in progress {
        let label_y = y;
        let progress = section.counts().then(|| {
            let text = super::progress(n, m);
            let pw = engine.simple_layout(&text, sentence_style()).width();
            (text, Rect::new(x + w - pw, label_y, pw, LABEL_H))
        });
        y += LABEL_H + LABEL_GAP;

        let card_top = y;
        let mut rows = Vec::new();
        let mut dividers = Vec::new();
        let mut qr = None;
        let rows_w = if section == Section::Devices {
            (w - QR - CardRow::PAD_X * 2.0).max(120.0)
        } else {
            w
        };
        for (i, &id) in section.rows().iter().enumerate() {
            if i > 0 {
                dividers.push(y);
            }
            let laid = measure_row(
                engine,
                id,
                &identity.workspace,
                Rect::new(x, y, rows_w, 0.0),
                learn_open,
                (&mcp_command, mcp_agent, agents_scroll),
            );
            if row_is_target(id) {
                hits.push((Target::Row(id), laid.rect));
            }
            for run in &laid.sentence {
                if let Some(link) = run.link {
                    hits.push((link, run.rect()));
                }
            }
            for (format, _, rect) in &laid.chips {
                hits.push((Target::Import(format), *rect));
            }
            if let Some(strip) = laid.agents_strip {
                for (agent, rect, _) in &laid.agents {
                    hits.push((Target::McpAgent(*agent), strip.visible(*rect)));
                }
            }
            if let Some((_, rect)) = &laid.code {
                hits.push((Target::CopyMcp, *rect));
            }
            if let Trailing::Button(_, target, rect) = &laid.trailing {
                hits.push((*target, *rect));
            }
            y += laid.rect.h;
            rows.push(laid);
        }
        if section == Section::Devices {
            // Centred on the rows, never taller than they are: the card is
            // as tall as its rows.
            let rows_h = y - card_top;
            let size = (rows_h - QR_INSET * 2.0).clamp(0.0, QR);
            let square = Rect::new(
                x + w - CardRow::PAD_X - size,
                card_top + (rows_h - size) * 0.5,
                size,
                size,
            );
            hits.push((Target::PairQr, square));
            qr = Some(square);
        }
        let card = Rect::new(x, card_top, w, y - card_top);
        sections.push(SectionLayout {
            section,
            label_y,
            progress,
            card,
            dividers,
            rows,
            qr,
        });
        y += SECTION_GAP;
    }

    // The last section ends the page: there is no footer under it.
    let bottom = y - SECTION_GAP + BOTTOM_PAD;

    Layout {
        column: Rect::new(column.x, column.y, column.w, bottom - TOP),
        customize,
        sections,
        hits,
        bottom,
    }
}

/// One row: its rect grows to whatever its text, chips and hints need.
fn measure_row(
    engine: &mut TextEngine,
    id: RowId,
    workspace: &str,
    at: Rect,
    learn_open: bool,
    (mcp_command, mcp_agent, agents_scroll): (&str, super::mcp::Agent, f32),
) -> RowLayout {
    let RowCopy { title, sentence } = copy(id, workspace);
    // The MCP row's *Copy* sits beside its command, under the text, so the
    // row has nothing at its right end.
    let trailing_w = match id {
        RowId::FirstDocument => {
            super::hints::new_document_chord().map(|chord| key_w(engine, &chord))
        }
        RowId::Files => Some(koru_ui::button::button_width(engine, super::FILES_BUTTON)),
        RowId::Learn => Some(CARET_W),
        RowId::TerminalAgent => Some(key_w(engine, super::AGENT_COMMAND)),
        _ => None,
    };
    let text_x = at.x + CardRow::PAD_X + TICK + TICK_GAP;
    let text_w = (at.w
        - CardRow::PAD_X * 2.0
        - TICK
        - TICK_GAP
        - trailing_w.map_or(0.0, |tw| tw + CardRow::GAP))
    .max(40.0);
    let top = at.y + CardRow::PAD_Y;
    let title_h =
        KoruTextParagraph::new(title, title_style(), "", theme::p().text).measure(engine, text_w);
    let title_rect = Rect::new(text_x, top, text_w, title_h);
    let mut bottom = top + title_h;
    let (runs, sentence_h) = flow(engine, &sentence, text_x, bottom + SENTENCE_GAP, text_w);
    if sentence_h > 0.0 {
        bottom += SENTENCE_GAP + sentence_h;
    }
    let mut chips = Vec::new();
    if id == RowId::Documents {
        let formats: Vec<(&'static str, String)> =
            koru_workspace_host::formats::import_menu_formats()
                .into_iter()
                .map(|descriptor| {
                    let label = koru_workspace_host::formats::import_menu_name(
                        descriptor.id,
                        crate::format::lang(),
                    )
                    .unwrap_or(descriptor.name);
                    (descriptor.id, label.to_owned())
                })
                .collect();
        let rects = koru_ui::chip::wrap(
            engine,
            text_x,
            bottom + CHIPS_GAP,
            text_x + text_w,
            formats.iter().map(|(_, label)| label.as_str()),
        );
        bottom = koru_ui::chip::bottom(&rects, bottom);
        chips = formats
            .into_iter()
            .zip(rects)
            .map(|((format, label), rect)| (format, label, rect))
            .collect();
    }
    let mut agents = Vec::new();
    let mut agents_strip = None;
    let mut copy_button = None;
    let mut code = None;
    if id == RowId::Mcp {
        // **Every agent on one line**, never wrapped: a line wider than the
        // row scrolls sideways inside it (the wheel, and Tab to a chip).
        let y = bottom + CHIPS_GAP;
        let line = koru_ui::chip::wrap(
            engine,
            text_x,
            y,
            f32::INFINITY,
            super::mcp::Agent::ALL.iter().map(|agent| agent.label()),
        );
        let line_w = line.last().map_or(0.0, |last| last.x + last.w - text_x);
        let overflow = (line_w - text_w).max(0.0);
        let scroll = agents_scroll.clamp(0.0, overflow);
        agents_strip = Some(AgentStrip {
            window: Rect::new(text_x, y, text_w, koru_ui::chip::Chip::H),
            scroll,
            overflow,
        });
        bottom = koru_ui::chip::bottom(&line, bottom);
        agents = super::mcp::Agent::ALL
            .into_iter()
            .zip(line)
            .map(|(agent, rect)| {
                let shifted = Rect::new(rect.x - scroll, rect.y, rect.w, rect.h);
                (agent, shifted, agent == mcp_agent)
            })
            .collect();

        // The command, as wide as the text less *Copy* at its right, and as
        // tall as *Copy*, in its text size: one line of two controls.
        let copy_w = koru_ui::button::button_width(engine, super::MCP_COPY);
        let line_h = koru_ui::button::H;
        let y = bottom + CODE_GAP;
        let field_w = (text_w - copy_w - CODE_GAP).max(40.0);
        let shown = elide_middle(engine, mcp_command, field_w);
        code = Some((shown, Rect::new(text_x, y, field_w, line_h)));
        copy_button = Some(Rect::new(text_x + field_w + CODE_GAP, y, copy_w, line_h));
        bottom = y + line_h;
    }
    let mut hints = Vec::new();
    if id == RowId::Learn && learn_open {
        let lines = super::hints::hint_lines();
        if !lines.is_empty() {
            bottom += HINTS_GAP;
            for (chord, what) in lines {
                hints.push(HintLine {
                    chord,
                    what,
                    y: bottom,
                });
                bottom += HINT_LINE_H;
            }
        }
    }
    let content_h = (bottom - top).max(TICK);
    let trailing_h = match id {
        RowId::Files => koru_ui::button::H,
        _ => KEY_H,
    };
    let h = (content_h.max(if trailing_w.is_some() {
        trailing_h
    } else {
        0.0
    }) + CardRow::PAD_Y * 2.0)
        .max(CardRow::MIN_H);
    let rect = Rect::new(at.x, at.y, at.w, h);
    let slot = |tw: f32, th: f32| CardRow::control_slot(rect, tw, th);
    let trailing = match (id, trailing_w) {
        (RowId::FirstDocument, Some(tw)) => super::hints::new_document_chord()
            .map_or(Trailing::None, |chord| {
                Trailing::Key(chord, slot(tw, KEY_H))
            }),
        (RowId::Files, Some(tw)) => Trailing::Button(
            super::FILES_BUTTON,
            Target::ChooseFiles,
            slot(tw, koru_ui::button::H),
        ),
        (RowId::Learn, Some(tw)) => Trailing::Caret(learn_open, slot(tw, KEY_H)),
        (RowId::TerminalAgent, Some(tw)) => Trailing::Code(super::AGENT_COMMAND, slot(tw, KEY_H)),
        // Not in the row's right-hand slot but beside the command it copies.
        (RowId::Mcp, _) => copy_button.map_or(Trailing::None, |rect| {
            Trailing::Button(super::MCP_COPY, Target::CopyMcp, rect)
        }),
        _ => Trailing::None,
    };
    // The circle sits on the title's first line.
    let first_line = title_style().metrics.line_height;
    let tick = id.ticks().then(|| {
        Rect::new(
            at.x + CardRow::PAD_X,
            top + (first_line - TICK) * 0.5,
            TICK,
            TICK,
        )
    });
    RowLayout {
        id,
        rect,
        tick,
        title,
        title_rect,
        sentence: runs,
        trailing,
        chips,
        hints,
        code,
        agents,
        agents_strip,
    }
}
