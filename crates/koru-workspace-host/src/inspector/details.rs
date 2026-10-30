// SPDX-License-Identifier: MPL-2.0

//! **Details, grouped.** Who the element is and its ancestors, as a
//! breadcrumb; then its design-system tokens — colour, spacing, size, text,
//! icon — then its state, accessibility and measured layout; and the
//! capture's notes folded at the foot.
//!
//! Reads the element record (ELEMENTS.final §3.9), whose token properties
//! are written `provenance -> value`. A record from a surface not yet on
//! elements carries no tokens: its properties are listed as recorded, and its
//! box shows measured distances.
use super::*;
use vello::peniko::Color;

/// One line of Details, before layout.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Line {
    /// The element's name, kind and source.
    Title(String),
    Heading(&'static str),
    Row(Row),
    Prose(String),
    /// Ancestors, nearest first: record index and name. Each one selects.
    Crumbs(Vec<(usize, String)>),
    /// The capture notes' toggle; when open, the notes follow as prose.
    Fold(bool),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Row {
    pub label: String,
    /// The token it came from, when it came from one.
    pub token: Option<String>,
    pub value: String,
    pub swatch: Option<Color>,
    /// A value written outside the token set: an exception the source scan
    /// would fail.
    pub raw: bool,
}

impl Row {
    pub fn plain(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            token: None,
            value: value.into(),
            swatch: None,
            raw: false,
        }
    }

    /// A recorded property: `provenance -> value` splits into its token and
    /// what it resolved to.
    pub fn recorded(label: &str, value: &str) -> Self {
        let (token, value) = match value.rsplit_once(" -> ") {
            Some((token, value)) => (Some(token.to_string()), value.to_string()),
            None => (None, value.to_string()),
        };
        Self {
            label: label.into(),
            raw: token.as_deref().is_some_and(|t| t.contains("exception")),
            swatch: swatch(&value),
            token,
            value,
        }
    }
}

impl Line {
    /// The plain-text form: what the accessibility tree reads and what the
    /// tests compare.
    pub fn text(&self) -> String {
        match self {
            Self::Title(text) | Self::Prose(text) => text.clone(),
            Self::Heading(text) => (*text).to_string(),
            Self::Row(row) => match &row.token {
                Some(token) => format!("{}: {token} -> {}", row.label, row.value),
                None => format!("{}: {}", row.label, row.value),
            },
            Self::Crumbs(crumbs) => format!(
                "Ancestors: {}",
                crumbs
                    .iter()
                    .map(|(_, name)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(" › ")
            ),
            Self::Fold(_) => "About this capture".into(),
        }
    }
}

/// `#RRGGBB / alpha a` → the colour, for a swatch.
fn swatch(value: &str) -> Option<Color> {
    let hex = value.strip_prefix('#')?.get(..6)?;
    let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    let alpha = value
        .split("alpha ")
        .nth(1)
        .and_then(|a| a.trim().parse::<f32>().ok())
        .unwrap_or(1.0);
    Some(Color::from_rgba8(channel(0)?, channel(2)?, channel(4)?, 255).multiply_alpha(alpha))
}

/// Identity, written beside the title rather than under a heading.
const IDENTITY: [&str; 3] = ["key", "within", "occurrence"];

/// The token groups first, in the order a designer asks about them.
const GROUPS: [(&str, &[&str]); 7] = [
    ("Colour", &["fill colour", "ink", "line"]),
    (
        "Spacing",
        &[
            "pad",
            "pad_x",
            "pad_y",
            "pad_start",
            "pad_end",
            "pad_top",
            "pad_bottom",
            "gap",
            "radius",
        ],
    ),
    (
        "Size",
        &["fill", "w", "h", "min_w", "max_w", "min_h", "max_h"],
    ),
    (
        "Text",
        &["text role", "weight", "text mode", "centred", "lines"],
    ),
    ("Icon", &["icon size"]),
    (
        "State",
        &[
            "state rule",
            "state",
            "profile",
            "theme",
            "appearance",
            "contrast",
        ],
    ),
    (
        "Accessibility",
        &["semantics", "name", "selected", "disabled", "input"],
    ),
];

/// Recorded by older surfaces and already shown as typography or the icon.
const SHOWN_ELSEWHERE: [&str; 7] = [
    "element",
    "font size",
    "rendered family",
    "font weight",
    "font style",
    "synthetic italic",
    "icon name",
];

impl Inspector {
    /// The selected record's Details, grouped.
    pub(super) fn details(&self, index: usize, coverage: CopyKey) -> Vec<Line> {
        let lang = crate::accessibility::ui_lang();
        let snapshot = &self.snapshot;
        let record = &snapshot.records[index];
        let mut out = Vec::new();
        let kind = property(record, "element").unwrap_or(record.kind);
        out.push(Line::Title(format!(
            "{} · {kind} · {}",
            display_name(record),
            record.source
        )));
        for name in IDENTITY {
            if let Some(value) = property(record, name) {
                out.push(Line::Row(Row::plain(name, value)));
            }
        }

        // Where it sits, before what it is made of.
        let mut crumbs = Vec::new();
        let mut at = record.parent;
        while let Some(i) = at {
            let ancestor = &snapshot.records[i];
            crumbs.push((i, display_name(ancestor).to_string()));
            at = ancestor.parent;
        }
        if !crumbs.is_empty() {
            out.push(Line::Heading("Ancestors"));
            out.push(Line::Crumbs(crumbs));
        }

        let mut placed: BTreeSet<&str> = IDENTITY.into_iter().collect();
        placed.extend(SHOWN_ELSEWHERE);
        for (heading, names) in GROUPS {
            let mut rows: Vec<Row> = names
                .iter()
                .filter_map(|name| property(record, name).map(|value| Row::recorded(name, value)))
                .collect();
            placed.extend(names.iter().copied());
            match heading {
                "Text" => rows.extend(
                    typography_details(snapshot, index)
                        .iter()
                        .filter_map(|line| line.split_once(": ").map(|(l, v)| Row::plain(l, v))),
                ),
                "Icon" => {
                    if let Some(name) = icon_name(snapshot, index) {
                        rows.insert(0, Row::plain("Phosphor icon", name));
                    }
                }
                _ => {}
            }
            if heading == "Accessibility" {
                out.push(Line::Heading(heading));
                out.extend(rows.into_iter().map(Line::Row));
                if self.semantic_details.is_empty() {
                    out.push(Line::Prose(
                        "Not exposed: attach an accessibility client to publish host semantics. \
                         No role/name is inferred from appearance."
                            .into(),
                    ));
                }
                out.extend(self.semantic_details.iter().cloned().map(Line::Prose));
            } else if !rows.is_empty() {
                out.push(Line::Heading(heading));
                out.extend(rows.into_iter().map(Line::Row));
            }
        }
        let rest: Vec<Row> = record
            .properties
            .iter()
            .filter(|p| !placed.contains(p.name))
            .map(|p| Row::recorded(p.name, &p.value))
            .collect();
        if !rest.is_empty() {
            out.push(Line::Heading("Recorded"));
            out.extend(rest.into_iter().map(Line::Row));
        }

        out.push(Line::Heading("Layout (measured)"));
        let r = record.rect();
        out.push(Line::Row(Row::plain(
            "size",
            format!("{:.1} × {:.1} px", r.width(), r.height()),
        )));
        match record.parent.and_then(|p| snapshot.records.get(p)) {
            Some(parent) => {
                out.push(Line::Row(Row::plain("Parent", display_name(parent))));
                if let Some(edges) = edges(snapshot, index) {
                    for (side, edge) in ["left", "top", "right", "bottom"].into_iter().zip(edges) {
                        out.push(Line::Row(Row {
                            label: side.into(),
                            token: edge.token.then(|| edge.short.clone()),
                            value: format!("{:.1}", edge.px),
                            swatch: None,
                            raw: false,
                        }));
                    }
                }
            }
            None => out.push(Line::Row(Row::plain(
                "Parent",
                "unavailable (no recorded parent); spacing rays unavailable",
            ))),
        }
        let (horizontal, vertical) = sibling_gaps(snapshot, index);
        let gap = |v: Option<f64>| v.map_or_else(|| "unavailable".into(), |n| format!("{n:.1}"));
        out.push(Line::Row(Row::plain(
            "sibling gap x/y",
            format!("{} / {}", gap(horizontal), gap(vertical)),
        )));
        out.push(Line::Row(Row::plain(
            "bounds",
            rectangle_numbers(record.bounds),
        )));
        out.push(Line::Row(Row::plain(
            "clip",
            rectangle_numbers(record.clip),
        )));

        out.push(Line::Fold(self.notes_open));
        if self.notes_open {
            out.extend(
                [
                    t(lang, coverage).to_string(),
                    format!("source: {}", record.source),
                    format!(
                        "frame: {} / draw + capture: {} us (not isolated overhead)",
                        snapshot.revision, self.capture_micros
                    ),
                    format!("coverage: {}", snapshot.coverage),
                    "Metadata incomplete: motion, responsive rule, margin policy and durable \
                     domain identity are not exposed."
                        .into(),
                    "Platform: Windows main host; mobile, Apple native, onboarding and \
                     auxiliary windows not verified. docs/features/koruui/INSPECTOR.md"
                        .into(),
                    "Layout values are measured; a box edge names a token only when the \
                     parent's padding, gap and siblings add up to it exactly."
                        .into(),
                    "Spacing rays follow the center axes; labels measure edge gaps in logical \
                     px."
                    .into(),
                    "Accessibility shows prepared host semantics, not native acceptance.".into(),
                ]
                .map(Line::Prose),
            );
        }
        out
    }
}

/// The nearest sibling on each axis, by measurement.
fn sibling_gaps(snapshot: &Snapshot, index: usize) -> (Option<f64>, Option<f64>) {
    let record = &snapshot.records[index];
    let r = record.rect();
    let (mut horizontal, mut vertical): (Option<f64>, Option<f64>) = (None, None);
    for (at, other) in snapshot.records.iter().enumerate() {
        if at == index || other.parent != record.parent {
            continue;
        }
        let o = other.rect();
        if o.y0 < r.y1 && o.y1 > r.y0 {
            let gap = (o.x0 - r.x1).max(r.x0 - o.x1);
            if gap >= 0.0 {
                horizontal = Some(horizontal.map_or(gap, |v| v.min(gap)));
            }
        }
        if o.x0 < r.x1 && o.x1 > r.x0 {
            let gap = (o.y0 - r.y1).max(r.y0 - o.y1);
            if gap >= 0.0 {
                vertical = Some(vertical.map_or(gap, |v| v.min(gap)));
            }
        }
    }
    (horizontal, vertical)
}

/// One side of the box: what made the distance to the parent's edge.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Edge {
    /// The distance, measured.
    pub px: f64,
    /// For the box: tokens and sibling names, `+`-joined.
    pub short: String,
    /// For the hover: each part with its value.
    pub full: String,
    /// Whether the parts add up to the measurement, so `short` names tokens.
    pub token: bool,
}

/// What an authored spacing resolved to: its short token and full chain.
struct Spacing {
    short: String,
    full: String,
    px: f64,
}

fn spacing(property: &inspect::Property) -> Option<Spacing> {
    let (token, value) = property.value.rsplit_once(" -> ")?;
    let short = match token.strip_prefix("alias::") {
        Some(alias) => alias.split(" = ").next().unwrap_or(alias).to_string(),
        None => token.rsplit("::").next().unwrap_or(token).to_string(),
    };
    Some(Spacing {
        short,
        full: format!("{} {token} → {value}", property.name),
        px: value.trim().parse().ok()?,
    })
}

/// The parent's padding on one side: the most specific setter that names it.
fn side_pad(parent: &inspect::Record, names: [&str; 3]) -> Option<Spacing> {
    names.iter().find_map(|name| {
        parent
            .properties
            .iter()
            .find(|p| p.name == *name)
            .and_then(spacing)
    })
}

/// Left, top, right, bottom: each side's distance to the parent, and the
/// padding, gaps and siblings that make it up when they add up exactly.
/// `None` without a parent.
pub(super) fn edges(snapshot: &Snapshot, index: usize) -> Option<[Edge; 4]> {
    let record = snapshot.records.get(index)?;
    let parent = snapshot.records.get(record.parent?)?;
    let (r, p) = (record.rect(), parent.rect());
    let element = property(parent, "element");
    let axis = match element {
        Some("row") => Some(true),
        Some("column") => Some(false),
        _ => None,
    };
    let gap = parent
        .properties
        .iter()
        .find(|p| p.name == "gap")
        .and_then(spacing);
    let siblings: Vec<&inspect::Record> = snapshot
        .records
        .iter()
        .enumerate()
        .filter(|(at, other)| *at != index && other.parent == record.parent)
        .map(|(_, other)| other)
        .collect();
    let sides: [(f64, [&str; 3], bool, bool); 4] = [
        (r.x0 - p.x0, ["pad_start", "pad_x", "pad"], true, true),
        (r.y0 - p.y0, ["pad_top", "pad_y", "pad"], false, true),
        (p.x1 - r.x1, ["pad_end", "pad_x", "pad"], true, false),
        (p.y1 - r.y1, ["pad_bottom", "pad_y", "pad"], false, false),
    ];
    Some(sides.map(|(px, names, horizontal, before)| {
        let measured = || Edge {
            px,
            short: format!("{px:.1} measured"),
            full: format!("{px:.1} px, measured"),
            token: false,
        };
        if element.is_none() {
            return Edge {
                full: format!("{px:.1} px, measured: this surface does not record its tokens"),
                ..measured()
            };
        }
        let pad = side_pad(parent, names);
        // Between this element and that edge, along the parent's axis.
        let between: Vec<&inspect::Record> = if axis == Some(horizontal) {
            siblings
                .iter()
                .copied()
                .filter(|o| {
                    let o = o.rect();
                    match (horizontal, before) {
                        (true, true) => o.x1 <= r.x0 + 0.5,
                        (true, false) => o.x0 >= r.x1 - 0.5,
                        (false, true) => o.y1 <= r.y0 + 0.5,
                        (false, false) => o.y0 >= r.y1 - 0.5,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut sum = pad.as_ref().map_or(0.0, |p| p.px);
        let mut short = Vec::new();
        let mut full = Vec::new();
        if !between.is_empty() {
            let gaps = between.len() as f64;
            sum += gap.as_ref().map_or(0.0, |g| g.px) * gaps;
            if let Some(g) = &gap {
                short.push(if between.len() > 1 {
                    format!("{} ×{}", g.short, between.len())
                } else {
                    g.short.clone()
                });
                full.push(if between.len() > 1 {
                    format!("{} (×{})", g.full, between.len())
                } else {
                    g.full.clone()
                });
            }
            for sibling in &between {
                let o = sibling.rect();
                let extent = if horizontal { o.width() } else { o.height() };
                sum += extent;
                short.push(display_name(sibling).to_string());
                full.push(format!("{} {extent:.1} px", display_name(sibling)));
            }
        }
        if let Some(p) = &pad
            && p.px > 0.0
        {
            short.push(p.short.clone());
            full.push(p.full.clone());
        }
        if (sum - px).abs() > 0.5 {
            return measured();
        }
        if short.is_empty() {
            return Edge {
                px,
                short: "0".into(),
                full: "0 px: no padding".into(),
                token: true,
            };
        }
        Edge {
            px,
            short: short.join(" + "),
            full: format!("{}\n= {px:.1} px", full.join("\n+ ")),
            token: true,
        }
    }))
}

/// The element's size as its rule — `fill × ≥ ROW_MIN` — when it has one.
pub(super) fn size_rule(record: &inspect::Record) -> Option<String> {
    property(record, "element")?;
    let short = |name: &str| {
        record
            .properties
            .iter()
            .find(|p| p.name == name)
            .and_then(spacing)
            .map(|s| s.short)
    };
    let w = if property(record, "fill").is_some() {
        "fill".to_string()
    } else if let Some(w) = short("w") {
        w
    } else {
        "hug".into()
    };
    let h = if let Some(h) = short("h") {
        h
    } else if let Some(min) = short("min_h") {
        format!("≥ {min}")
    } else {
        "hug".into()
    };
    Some(format!("{w} × {h}"))
}

/// What a click in Details lands on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Hit {
    Crumb(usize),
    Fold,
}

const LABEL_W: f32 = 112.0;
const SWATCH: f32 = 10.0;
const LINE_GAP: f32 = 4.0;

/// Laid-out Details: a scene to replay, its height, and where its links are.
#[derive(Default)]
pub(super) struct Laid {
    pub scene: Scene,
    pub height: f32,
    pub hits: Vec<(Rect, Hit)>,
}

/// Lay `lines` out `width` wide.
pub(super) fn lay_out(engine: &mut TextEngine, lines: &[Line], width: f32) -> Laid {
    let p = theme::p();
    let style = diagnostic_style();
    let strong = style.with_weight(theme::FontWeight::SemiBold);
    let stack = diagnostic_stack();
    let mut laid = Laid::default();
    let mut y = 0.0_f32;
    let put = |laid: &mut Laid,
               engine: &mut TextEngine,
               text: &str,
               style: theme::TextStyle,
               x: f32,
               y: f32,
               w: f32,
               color: Color|
     -> f32 {
        let layout = engine.simple_layout_wrapped_in(text, style, stack, w.max(1.0));
        scene::draw_layout(
            &mut laid.scene,
            Affine::IDENTITY,
            &layout,
            x,
            y,
            Some(color),
        );
        layout.height()
    };
    for line in lines {
        match line {
            Line::Title(text) => {
                y += put(&mut laid, engine, text, strong, 0.0, y, width, p.text) + LINE_GAP * 2.0;
            }
            Line::Heading(text) => {
                y += LINE_GAP * 2.0;
                scene::fill(
                    &mut laid.scene,
                    Affine::IDENTITY,
                    p.panel_border,
                    &scene::rect(0.0, y, width, 1.0),
                );
                y += LINE_GAP;
                y += put(&mut laid, engine, text, strong, 0.0, y, width, p.text_dim) + LINE_GAP;
            }
            Line::Prose(text) => {
                y += put(&mut laid, engine, text, style, 0.0, y, width, p.text_dim) + LINE_GAP;
            }
            Line::Row(row) => {
                let label = put(
                    &mut laid,
                    engine,
                    &row.label,
                    style,
                    0.0,
                    y,
                    LABEL_W - 8.0,
                    p.text_dim,
                );
                let rest = (width - LABEL_W).max(1.0);
                let (token_w, value_x) = if row.token.is_some() {
                    let w = (rest * 0.6).floor();
                    (w, LABEL_W + w + 8.0)
                } else {
                    (0.0, LABEL_W)
                };
                let mut height = label;
                if let Some(token) = &row.token {
                    let token = if row.raw {
                        format!("{token} · raw")
                    } else {
                        token.clone()
                    };
                    let color = if row.raw { p.spell } else { p.accent };
                    height = height.max(put(
                        &mut laid, engine, &token, style, LABEL_W, y, token_w, color,
                    ));
                }
                let mut x = value_x;
                if let Some(color) = row.swatch {
                    let square = scene::rect(x, y + 3.0, SWATCH, SWATCH);
                    scene::fill(&mut laid.scene, Affine::IDENTITY, color, &square);
                    scene::stroke(
                        &mut laid.scene,
                        Affine::IDENTITY,
                        p.panel_border,
                        1.0,
                        &square,
                    );
                    x += SWATCH + 6.0;
                }
                height = height.max(put(
                    &mut laid,
                    engine,
                    &row.value,
                    style,
                    x,
                    y,
                    (width - x).max(1.0),
                    p.text,
                ));
                y += height + LINE_GAP;
            }
            Line::Crumbs(crumbs) => {
                let mut x = 0.0_f32;
                let mut row_h = 0.0_f32;
                for (n, (index, name)) in crumbs.iter().enumerate() {
                    let text = if n + 1 < crumbs.len() {
                        format!("{name} ›")
                    } else {
                        name.clone()
                    };
                    let layout = engine.simple_layout_in(&text, style, stack);
                    let (w, h) = (layout.width(), layout.height());
                    if x > 0.0 && x + w > width {
                        x = 0.0;
                        y += row_h + LINE_GAP;
                    }
                    scene::draw_layout(
                        &mut laid.scene,
                        Affine::IDENTITY,
                        &layout,
                        x,
                        y,
                        Some(p.accent),
                    );
                    laid.hits.push((Rect::new(x, y, w, h), Hit::Crumb(*index)));
                    x += w + 8.0;
                    row_h = h;
                }
                y += row_h + LINE_GAP;
            }
            Line::Fold(open) => {
                y += LINE_GAP * 2.0;
                let text = format!("{} About this capture", if *open { "▾" } else { "▸" });
                let h = put(&mut laid, engine, &text, strong, 0.0, y, width, p.text_dim);
                laid.hits.push((Rect::new(0.0, y, width, h), Hit::Fold));
                y += h + LINE_GAP;
            }
        }
    }
    laid.height = y;
    laid
}
