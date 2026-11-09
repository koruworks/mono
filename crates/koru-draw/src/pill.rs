// SPDX-License-Identifier: MPL-2.0

//! Turning an attribute into something drawable.
//!
//! `koru-core` stores *what a thing is* — a date, a reference to a contact, a
//! URL. This decides *how it looks*: which palette, which icon, what caption.
//! Keeping the mapping here rather than in the model is what lets a calendar
//! render the same attribute as a position on a grid instead of a pill.

use std::collections::HashMap;

use parley::Layout;
use peniko::Color;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line};

use crate::scene::{draw_layout, fill, rrect, stroke};
use crate::theme::{self, Brush};

/// Which palette and icon a pill draws with.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum PillKind {
    Document,
    /// A workspace file referenced from a body (attachment / `@file:`).
    File,
    Contact,
    Date,
    Link,
    /// A **delegated run**, left behind by the gesture that started it
    /// (delegation roadmap D8). Its own hue, because it is the one pill whose
    /// caption changes on its own while a person watches: what it says is the
    /// run's state, and a run is not a document, a file or a date.
    Run,
    /// A recurring delegation that is currently active.
    Schedule,
    /// The same durable schedule while future firings are paused.
    SchedulePaused,
    /// A passage of an AI reply quoted into the chat's composer (koru-ai
    /// D88): the quote glyph and the passage's start in quotation marks,
    /// in the document pill's neutral ink. Only the composer writes one.
    Quote,
    /// A skill of the chat's agent picked under `$` in the composer (koru-ai
    /// D121): the wrench glyph and the skill's name, in the document pill's
    /// neutral ink. Only the composer writes one.
    Skill,
}

impl PillKind {
    pub fn palette(self) -> &'static crate::theme::PillPalette {
        let p = crate::theme::p();
        match self {
            Self::Document | Self::File | Self::Quote | Self::Skill => &p.document,
            Self::Contact => &p.contact,
            Self::Date => &p.date,
            Self::Link => &p.link,
            Self::Run | Self::Schedule | Self::SchedulePaused => &p.run,
        }
    }
}

/// Typography inherited from the block that contains a pill.
///
/// Size, line height and font stack are part of the cache key: a title, body
/// paragraph and chat composer may name the same document with different text.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PillTextStyle {
    pub size: f32,
    pub line_height: f32,
    pub stack: &'static str,
}

impl PillTextStyle {
    pub fn new(size: f32, line_height: f32) -> Self {
        Self {
            size,
            line_height,
            stack: theme::body_stack(),
        }
    }

    pub const fn with_stack(mut self, stack: &'static str) -> Self {
        self.stack = stack;
        self
    }

    pub fn body() -> Self {
        let body = theme::role::body().metrics;
        Self::new(body.size, body.line_height)
    }

    fn key(self) -> (u32, u32, &'static str) {
        (self.size.to_bits(), self.line_height.to_bits(), self.stack)
    }
}

/// Everything needed to paint one pill, shaped once and retained in the
/// per-view cache.
pub struct PillVisual {
    pub width: f32,
    pub height: f32,
    /// Inset on each horizontal edge. Document and link pills deliberately
    /// have none: surrounding space belongs to the document text.
    pub horizontal_padding: f32,
    /// The portion above the shared text baseline. Parley treats an inline
    /// box's reported height as ascent, while painting uses the full height.
    pub inline_ascent: f32,
    pub label_baseline: f32,
    pub label_descent: f32,
    /// Cap-height icon column. Zero only when no icon is needed.
    pub icon_slot: f32,
    pub label: Layout<Brush>,
    /// Scheme glyph for links or a leading emoji for documents. Other kinds
    /// draw their icon as vectors.
    pub icon: Option<Layout<Brush>>,
}

pub(crate) type PillKey = (PillKind, String, Option<String>, u32, u32, &'static str);

/// Visuals keyed by every input that affects shaping or icon choice.
#[derive(Default)]
pub struct PillCache(pub(crate) HashMap<PillKey, PillVisual>);

impl PillCache {
    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn get(&self, kind: PillKind, label: &str, url: Option<&str>) -> Option<&PillVisual> {
        self.get_with_style(kind, label, url, PillTextStyle::body())
    }

    pub fn get_with_style(
        &self,
        kind: PillKind,
        label: &str,
        url: Option<&str>,
        style: PillTextStyle,
    ) -> Option<&PillVisual> {
        let (size, line_height, stack) = style.key();
        self.0.get(&(
            kind,
            label.to_owned(),
            url.map(str::to_owned),
            size,
            line_height,
            stack,
        ))
    }

    pub(crate) fn key(
        kind: PillKind,
        label: &str,
        url: Option<&str>,
        style: PillTextStyle,
    ) -> PillKey {
        let (size, line_height, stack) = style.key();
        (
            kind,
            label.to_owned(),
            url.map(str::to_owned),
            size,
            line_height,
            stack,
        )
    }
}

/// Everything needed to draw one pill, resolved once per rebuild.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PillInfo {
    pub kind: PillKind,
    pub label: String,
    /// A reference whose target could not be found. Still drawn — as a
    /// placeholder — because a target may live in a profile that is closed or
    /// unreadable, which is an ordinary state rather than an error.
    pub dangling: bool,
    /// This routed document currently lives in its workspace's trash. It stays
    /// a document pill for activation and palette purposes; only its icon and
    /// destination change.
    pub trashed: bool,
    /// A link's target, cached so the tooltip and Ctrl+click never read the
    /// document while drawing a frame.
    pub url: Option<String>,
    /// Opaque document storage id for a routed document pill.
    /// Drawing never interprets them; the host uses them to refresh captions
    /// from manifest entries without making the draw crate depend on the model.
    pub entity: Option<String>,
}

/// Paint a measured pill at its inline-box origin.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    scene: &mut Scene,
    root: Affine,
    kind: PillKind,
    trashed: bool,
    dangling: bool,
    visual: &PillVisual,
    x: f32,
    y: f32,
    dimmed: bool,
    hovered: bool,
    selected: bool,
) {
    let palette = kind.palette();
    let alpha = if dimmed { 0.5 } else { 1.0 };
    let slot = visual.icon_slot;
    let gap = if slot > 0.0 {
        theme::PILL_ICON_GAP
    } else {
        0.0
    };
    let label_x = x + visual.horizontal_padding + slot + gap;
    let label_y = y + theme::PILL_PAD_Y;
    let baseline_y = label_y + visual.label_baseline;
    let icon_x = x + visual.horizontal_padding;
    let icon_color = palette.fg.multiply_alpha(alpha);

    // File state icons win over the cached glyph: trash when binned, a broken
    // mark when the target is gone. Live files keep the measured FILE glyph.
    if kind == PillKind::File && (trashed || dangling) {
        if trashed {
            draw_trash_icon(scene, root, icon_x, baseline_y, slot, icon_color);
        } else {
            draw_broken_file_icon(scene, root, icon_x, baseline_y, slot, icon_color);
        }
    } else if let Some(icon) = &visual.icon {
        let gx = icon_x + (slot - icon.width()) * 0.5;
        let gy = if kind == PillKind::Document {
            let emoji_baseline = icon
                .lines()
                .next()
                .map(|line| line.metrics().baseline)
                .unwrap_or(icon.height());
            baseline_y - emoji_baseline
        } else {
            baseline_y - icon.height()
        };
        // Links and file glyphs inherit the pill foreground; document emoji keep theirs.
        let brush = (kind != PillKind::Document).then_some(icon_color);
        draw_layout(scene, root, icon, gx, gy, brush);
    } else if kind == PillKind::Document && trashed {
        draw_trash_icon(scene, root, icon_x, baseline_y, slot, icon_color);
    } else if kind != PillKind::Link {
        draw_icon(scene, root, kind, icon_x, baseline_y, slot, icon_color);
    }

    draw_layout(
        scene,
        root,
        &visual.label,
        label_x,
        label_y,
        Some(palette.fg.multiply_alpha(alpha)),
    );

    // A missing file reads as dead at a glance: strikethrough the caption.
    if kind == PillKind::File && dangling {
        let strike_y = (baseline_y - visual.label_baseline * 0.35) as f64;
        stroke(
            scene,
            root,
            palette.fg.multiply_alpha(alpha),
            1.0,
            &Line::new(
                (label_x as f64, strike_y),
                ((label_x + visual.label.width()) as f64, strike_y),
            ),
        );
    }

    if hovered || selected {
        let underline_y = (baseline_y + 1.5) as f64;
        stroke(
            scene,
            root,
            palette.fg.multiply_alpha(alpha),
            1.0,
            &Line::new(
                (icon_x as f64, underline_y),
                ((label_x + visual.label.width()) as f64, underline_y),
            ),
        );
    }
}

fn draw_trash_icon(
    scene: &mut Scene,
    root: Affine,
    x: f32,
    baseline_y: f32,
    width: f32,
    color: Color,
) {
    let body_w = width * 0.70;
    let body_h = width * 0.72;
    let left = x + (width - body_w) * 0.5;
    let top = baseline_y - body_h;
    stroke(
        scene,
        root,
        color,
        1.0,
        &rrect(left, top, body_w, body_h, width * 0.10),
    );
    let lid_y = top - width * 0.18;
    stroke(
        scene,
        root,
        color,
        1.0,
        &Line::new(
            ((left - width * 0.08) as f64, lid_y as f64),
            ((left + body_w + width * 0.08) as f64, lid_y as f64),
        ),
    );
    stroke(
        scene,
        root,
        color,
        1.0,
        &Line::new(
            ((x + width * 0.38) as f64, (lid_y - width * 0.12) as f64),
            ((x + width * 0.62) as f64, (lid_y - width * 0.12) as f64),
        ),
    );
}

/// A file whose target is gone: the page outline with a diagonal break.
fn draw_broken_file_icon(
    scene: &mut Scene,
    root: Affine,
    x: f32,
    baseline_y: f32,
    width: f32,
    color: Color,
) {
    let top = baseline_y - width;
    let inset = width * 0.08;
    stroke(
        scene,
        root,
        color,
        1.0,
        &rrect(x + inset, top, width - inset * 2.0, width, width * 0.12),
    );
    stroke(
        scene,
        root,
        color,
        1.25,
        &Line::new(
            ((x + width * 0.22) as f64, (top + width * 0.78) as f64),
            ((x + width * 0.78) as f64, (top + width * 0.22) as f64),
        ),
    );
}

/// Draw the vector icon used by document, contact and date pills.
pub fn draw_icon(
    scene: &mut Scene,
    root: Affine,
    kind: PillKind,
    x: f32,
    baseline_y: f32,
    width: f32,
    color: Color,
) {
    let top = baseline_y - width;
    let cy = baseline_y - width * 0.5;
    match kind {
        // `Run` never reaches here in practice — its glyph is measured with
        // the visual, like a file's — but a pill with no font is still a pill,
        // and the page mark is the honest fallback for "a reference".
        PillKind::Document
        | PillKind::File
        | PillKind::Run
        | PillKind::Schedule
        | PillKind::SchedulePaused
        | PillKind::Quote
        | PillKind::Skill => {
            let inset = width * 0.08;
            stroke(
                scene,
                root,
                color,
                1.0,
                &rrect(x + inset, top, width - inset * 2.0, width, width * 0.12),
            );
            for i in 0..2 {
                let line_y = top + width * (0.38 + 0.24 * i as f32);
                stroke(
                    scene,
                    root,
                    color,
                    1.0,
                    &Line::new(
                        ((x + width * 0.28) as f64, line_y as f64),
                        ((x + width * 0.72) as f64, line_y as f64),
                    ),
                );
            }
        }
        PillKind::Contact => {
            let radius = width * 0.22;
            fill(
                scene,
                root,
                color,
                &Circle::new(
                    ((x + width * 0.5) as f64, (cy - width * 0.18) as f64),
                    radius as f64,
                ),
            );
            fill(
                scene,
                root,
                color,
                &rrect(
                    x + width * 0.14,
                    cy + width * 0.06,
                    width * 0.72,
                    width * 0.38,
                    width * 0.2,
                ),
            );
        }
        PillKind::Date => {
            let height = width * 1.05;
            let top = cy - height * 0.5;
            let tab = width * 0.28;
            let body_top = top + tab * 0.45;
            stroke(
                scene,
                root,
                color,
                1.0,
                &rrect(
                    x + width * 0.06,
                    body_top,
                    width * 0.88,
                    height - tab * 0.45,
                    width * 0.12,
                ),
            );
            for i in 0..2 {
                let tab_x = x + width * (0.3 + 0.4 * i as f32);
                stroke(
                    scene,
                    root,
                    color,
                    1.0,
                    &Line::new(
                        (tab_x as f64, top as f64),
                        (tab_x as f64, (body_top + tab * 0.15) as f64),
                    ),
                );
            }
            let rule_y = top + height * 0.42;
            stroke(
                scene,
                root,
                color,
                1.0,
                &Line::new(
                    ((x + width * 0.12) as f64, rule_y as f64),
                    ((x + width * 0.88) as f64, rule_y as f64),
                ),
            );
        }
        PillKind::Link => {
            // Normally a Phosphor scheme glyph is cached with the visual.
            let radius = width * 0.28;
            for (dx, dy) in [(-0.16, 0.16), (0.16, -0.16)] {
                let circle_x = x + width * (0.5 + dx);
                let circle_y = cy + width * dy;
                stroke(
                    scene,
                    root,
                    color,
                    1.0,
                    &Circle::new((circle_x as f64, circle_y as f64), radius as f64),
                );
            }
            stroke(
                scene,
                root,
                color,
                1.0,
                &Line::new(
                    ((x + width * 0.32) as f64, (cy + width * 0.1) as f64),
                    ((x + width * 0.68) as f64, (cy - width * 0.1) as f64),
                ),
            );
        }
    }
}
