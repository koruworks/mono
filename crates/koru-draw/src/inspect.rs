// SPDX-License-Identifier: MPL-2.0

//! Optional, bounded evidence from the draw that actually reached the window.
//!
//! A frame owns the collector. Scopes describe implementation ownership, not
//! an inferred widget tree. With no frame armed every seam is a TLS boolean
//! check: closures, formatting and geometry work are skipped. No scene, text,
//! document content or callbacks survive the frame.

use std::cell::{Cell, RefCell};
use std::panic::Location;

use serde::Serialize;
use vello::kurbo::{Affine, Rect};

pub const MAX_RECORDS: usize = 4096;

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static CAPTURE: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

#[inline]
pub fn enabled() -> bool {
    ENABLED.get()
}

/// Paint without recording, for one design element that is drawn as many
/// primitives. A panel's shadow is sixteen expanding rings because vello has
/// no blur; recorded one by one they would flood the tree with sixteen fills
/// nobody can act on, so the painter records the *shadow* and suspends the
/// rings. Nested suspensions restore in order — the guard remembers the flag
/// it found.
pub fn suspend() -> Suspension {
    Suspension(ENABLED.with(|e| e.replace(false)))
}

pub struct Suspension(bool);

impl Drop for Suspension {
    fn drop(&mut self) {
        ENABLED.with(|e| e.set(self.0));
    }
}

/// Add evidence computed by a shared implementation after resolving its state.
/// Like scopes, the closure is never evaluated while inspection is off.
pub fn current(properties: impl FnOnce(&mut Record)) {
    if !enabled() {
        return;
    }
    CAPTURE.with_borrow_mut(|slot| {
        if let Some(c) = slot
            && !c.snapshot.truncated
            && let Some(index) = c.stack.last().copied()
        {
            properties(&mut c.snapshot.records[index]);
        }
    });
}

/// Contrast of two known opaque sRGB colors. Unknown composition is not a ratio.
pub fn contrast(foreground: peniko::Color, background: peniko::Color) -> Option<f32> {
    if foreground.components[3] != 1.0 || background.components[3] != 1.0 {
        return None;
    }
    fn luminance(color: peniko::Color) -> f32 {
        let linear = |c: f32| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let [r, g, b, _] = color.components;
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }
    let a = luminance(foreground);
    let b = luminance(background);
    Some((a.max(b) + 0.05) / (a.min(b) + 0.05))
}

#[derive(Clone, Debug, Serialize)]
pub struct Property {
    pub name: &'static str,
    pub value: String,
    pub origin: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct Record {
    pub parent: Option<usize>,
    pub name: &'static str,
    pub kind: &'static str,
    pub source: String,
    /// Window logical units, with display zoom removed exactly once.
    pub bounds: [f64; 4],
    pub clip: [f64; 4],
    /// Paint order within the frame. Once a hoisted subtree leads the record
    /// vec, position no longer says what covers what — this does.
    pub z: u32,
    /// Root of a subtree that leads the hierarchy although it painted last —
    /// a modal owns the window beneath it.
    pub hoisted: bool,
    pub properties: Vec<Property>,
}

impl Record {
    pub fn rect(&self) -> Rect {
        rect(self.bounds)
    }
    pub fn visible(&self) -> Rect {
        self.rect().intersect(rect(self.clip))
    }
    pub fn property(&mut self, name: &'static str, value: impl ToString, origin: &'static str) {
        self.properties.push(Property {
            name,
            value: value.to_string(),
            origin,
        });
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Snapshot {
    pub revision: u64,
    pub records: Vec<Record>,
    pub truncated: bool,
    /// Direct Vello commands and embedded/native surfaces require host adapters.
    pub coverage: &'static str,
}

impl Snapshot {
    pub fn pick(&self, x: f64, y: f64) -> Option<usize> {
        if self.truncated {
            return None;
        }
        // Paint order, not vec order: a hoisted subtree leads the vec yet
        // painted last, and the topmost record at a point is the one a person
        // actually sees there.
        self.records
            .iter()
            .enumerate()
            .filter(|(_, r)| r.visible().contains((x, y)))
            .max_by_key(|(_, r)| r.z)
            .map(|(index, _)| index)
    }

    pub fn owner(&self, mut index: usize) -> Option<usize> {
        loop {
            let record = self.records.get(index)?;
            if record.kind == "unregistered" {
                return None;
            }
            if record.kind == "component" {
                return Some(index);
            }
            index = record.parent?;
        }
    }

    pub fn export(&self, selected: Option<usize>) -> Result<String, serde_json::Error> {
        #[derive(Serialize)]
        struct Export<'a> {
            schema: u32,
            selected: Option<usize>,
            snapshot: &'a Snapshot,
        }
        serde_json::to_string_pretty(&Export {
            schema: 1,
            selected,
            snapshot: self,
        })
    }
}

struct Capture {
    snapshot: Snapshot,
    stack: Vec<usize>,
    clips: Vec<Rect>,
    inverse: Affine,
}

/// Balanced even when a host returns early or unwinds.
pub struct Frame;

impl Frame {
    pub fn begin(revision: u64, root: Affine, viewport: Rect) -> Self {
        assert!(!enabled(), "nested inspector frame");
        CAPTURE.with_borrow_mut(|slot| {
            *slot = Some(Capture {
                snapshot: Snapshot {
                    revision,
                    coverage: "partial: shared draw seams; direct/native drawing not classified",
                    ..Default::default()
                },
                stack: Vec::new(),
                clips: vec![viewport],
                inverse: root.inverse(),
            })
        });
        ENABLED.set(true);
        Self
    }

    pub fn finish(self) -> Snapshot {
        ENABLED.set(false);
        CAPTURE.with_borrow_mut(|slot| {
            let mut snapshot = slot.take().expect("inspector frame").snapshot;
            hoist(&mut snapshot.records);
            snapshot
        })
    }
}

/// Move hoisted subtrees to the front of the record vec, keeping append order
/// inside each half and remapping every `parent` through the old→new index
/// map. A hoisted root whose own parent stayed behind ends up with a parent
/// index *after* itself, which the hierarchy view reads as a top-level row —
/// the modal leads the tree even though it painted last. `z` is untouched:
/// paint order and tree order are different facts.
fn hoist(records: &mut Vec<Record>) {
    if !records.iter().any(|record| record.hoisted) {
        return;
    }
    let count = records.len();
    // Parents are always appended before their children, so every chain walk
    // terminates; a record belongs to a hoisted subtree when its chain meets
    // a hoisted root.
    let mut in_subtree = vec![false; count];
    for index in 0..count {
        let mut at = Some(index);
        while let Some(current) = at {
            if records[current].hoisted {
                in_subtree[index] = true;
                break;
            }
            at = records[current].parent;
        }
    }
    let mut order = Vec::with_capacity(count);
    for member in [true, false] {
        order.extend((0..count).filter(|index| in_subtree[*index] == member));
    }
    let mut moved = vec![0_usize; count];
    for (new, old) in order.iter().enumerate() {
        moved[*old] = new;
    }
    let mut taken: Vec<Option<Record>> = std::mem::take(records).into_iter().map(Some).collect();
    records.extend(order.iter().map(|old| {
        let mut record = taken[*old].take().expect("each record moves once");
        record.parent = record.parent.map(|parent| moved[parent]);
        record
    }));
}

impl Drop for Frame {
    fn drop(&mut self) {
        ENABLED.set(false);
        CAPTURE.with_borrow_mut(|slot| *slot = None);
    }
}

pub struct Scope(bool);
impl Drop for Scope {
    fn drop(&mut self) {
        if self.0 {
            CAPTURE.with_borrow_mut(|slot| {
                if let Some(c) = slot {
                    c.stack.pop();
                }
            });
        }
    }
}

/// The caller is the shared implementation, never a host-provided Koru name.
#[track_caller]
pub fn scope(
    name: &'static str,
    kind: &'static str,
    root: Affine,
    bounds: Rect,
    properties: impl FnOnce(&mut Record),
) -> Scope {
    scope_at(name, kind, root, bounds, Location::caller(), properties)
}

/// A scope whose source is where the thing was *written*, not where it was
/// painted: an element built in a host module and painted by the shared
/// frame names the host's line (ELEMENTS.final §3.9, construction site).
pub fn scope_at(
    name: &'static str,
    kind: &'static str,
    root: Affine,
    bounds: Rect,
    site: &'static Location<'static>,
    properties: impl FnOnce(&mut Record),
) -> Scope {
    if !enabled() {
        return Scope(false);
    }
    let index = append(name, kind, root, bounds, site, properties);
    if let Some(index) = index {
        CAPTURE.with_borrow_mut(|slot| slot.as_mut().unwrap().stack.push(index));
    }
    Scope(index.is_some())
}

#[track_caller]
pub fn leaf(name: &'static str, root: Affine, bounds: Rect, properties: impl FnOnce(&mut Record)) {
    if enabled() {
        append(
            name,
            "primitive",
            root,
            bounds,
            Location::caller(),
            properties,
        );
    }
}

/// A scope whose subtree leads the finished hierarchy although it painted
/// last. For a modal root: the scrim covers the window, so the tree should
/// open with what a person is actually looking at. Picking is unaffected —
/// it follows paint order (`z`), never vec order.
#[track_caller]
pub fn scope_hoisted(
    name: &'static str,
    kind: &'static str,
    root: Affine,
    bounds: Rect,
    properties: impl FnOnce(&mut Record),
) -> Scope {
    if !enabled() {
        return Scope(false);
    }
    let index = append(name, kind, root, bounds, Location::caller(), properties);
    if let Some(index) = index {
        CAPTURE.with_borrow_mut(|slot| {
            let c = slot.as_mut().unwrap();
            c.snapshot.records[index].hoisted = true;
            c.stack.push(index);
        });
    }
    Scope(index.is_some())
}

fn append(
    name: &'static str,
    kind: &'static str,
    root: Affine,
    bounds: Rect,
    source: &'static Location<'static>,
    properties: impl FnOnce(&mut Record),
) -> Option<usize> {
    CAPTURE.with_borrow_mut(|slot| {
        let c = slot.as_mut()?;
        if c.snapshot.records.len() >= MAX_RECORDS {
            c.snapshot.truncated = true;
            return None;
        }
        let bounds = (c.inverse * root).transform_rect_bbox(bounds);
        let clip = *c.clips.last().unwrap();
        if kind == "primitive" && bounds.intersect(clip).area() <= 0.0 {
            return None;
        }
        // file! paths in workspace builds are relative. Avoid exporting absolute
        // developer paths if an external caller was built with different flags.
        let file = source.file().replace('\\', "/");
        let file = file
            .find("crates/")
            .or_else(|| file.find("apps/"))
            .map_or("external", |at| &file[at..]);
        let mut record = Record {
            parent: c.stack.last().copied(),
            name,
            kind,
            source: format!("{file}:{}", source.line()),
            bounds: array(bounds),
            clip: array(clip),
            z: c.snapshot.records.len() as u32,
            hoisted: false,
            properties: Vec::new(),
        };
        properties(&mut record);
        let index = c.snapshot.records.len();
        c.snapshot.records.push(record);
        Some(index)
    })
}

pub struct Clip(bool);
impl Drop for Clip {
    fn drop(&mut self) {
        if self.0 {
            CAPTURE.with_borrow_mut(|slot| {
                if let Some(c) = slot {
                    c.clips.pop();
                }
            });
        }
    }
}

pub fn clip(root: Affine, bounds: Rect) -> Clip {
    if !enabled() {
        return Clip(false);
    }
    CAPTURE.with_borrow_mut(|slot| {
        let c = slot.as_mut().unwrap();
        let bounds = (c.inverse * root).transform_rect_bbox(bounds);
        c.clips.push(bounds.intersect(*c.clips.last().unwrap()));
    });
    Clip(true)
}

pub fn array(r: Rect) -> [f64; 4] {
    [r.x0, r.y0, r.x1, r.y1]
}
pub fn rect(r: [f64; 4]) -> Rect {
    Rect::new(r[0], r[1], r[2], r[3])
}

pub fn color(color: peniko::Color) -> String {
    let [r, g, b, a] = color.components;
    format!(
        "#{:02X}{:02X}{:02X} / alpha {a:.3}",
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8
    )
}

/// Values come from the shaped run already being painted, never a second layout.
#[track_caller]
pub fn text_run(
    root: Affine,
    gr: &parley::layout::GlyphRun<'_, crate::theme::Brush>,
    ox: f32,
    oy: f32,
    color_value: peniko::Color,
) {
    use skrifa::MetadataProvider;
    let run = gr.run();
    let metrics = run.metrics();
    let x = ox + gr.offset();
    let baseline = oy + gr.baseline();
    let bounds = Rect::new(
        x as f64,
        (baseline - metrics.ascent) as f64,
        (x + gr.advance()) as f64,
        (baseline + metrics.descent) as f64,
    );
    leaf("text run", root, bounds, |record| {
        record.property("font size", run.font_size(), "rendered run");
        record.property(
            "font weight",
            run.font_attrs().weight.value(),
            "Parley run font attributes",
        );
        record.property(
            "font style",
            format!("{:?}", run.font_attrs().style),
            "Parley run font attributes; synthetic italic reported separately",
        );
        record.property("baseline", baseline, "local layout coordinates");
        record.property(
            "ascent / descent",
            format!("{} / {}", metrics.ascent, metrics.descent),
            "rendered font metrics; not padding",
        );
        record.property(
            "foreground",
            color(color_value),
            "resolved paint; token origin not exposed",
        );
        let data = run.font();
        if let Ok(font) = skrifa::FontRef::from_index(data.data.as_ref(), data.index) {
            let family = font
                .localized_strings(skrifa::string::StringId::FAMILY_NAME)
                .english_or_first()
                .map(|name| name.to_string())
                .unwrap_or_else(|| "Unknown".into());
            record.property(
                "rendered family",
                family,
                "actual font face; fallback included",
            );
            record.property(
                "face attributes",
                format!("{:?}", font.attributes()),
                "font face; variation axes may override",
            );
        }
        record.property(
            "variation coordinates",
            format!("{:?}", run.normalized_coords()),
            "rendered run, normalized font axes",
        );
        record.property(
            "synthetic italic",
            format!("{:?}", run.synthesis().skew()),
            "rendered run",
        );
        record.property(
            "tracking",
            "Not exposed",
            "unknown; do not infer from advances",
        );
    });
}

#[cfg(test)]
mod tests;
