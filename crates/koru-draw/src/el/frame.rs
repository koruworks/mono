// SPDX-License-Identifier: MPL-2.0

//! The one record a layout produces, and everything that reads it: paint,
//! pointer targets, the accessibility tree and the inspector (§3.6). None of
//! them repeats placement arithmetic.
//!
//! Paint uses the text layouts measured during layout and never re-breaks
//! them; colours resolve through each node's own theme and contrast, so two
//! subtrees laid out under two themes paint as two themes.

use std::collections::HashMap;
use std::rc::Rc;

use parley::Layout;
use vello::Scene;
use vello::kurbo::Affine;

use super::{Action, Axis, El, ElState, FrameCx, Key, Kind, Look, Name, Role, TextEl, TextMode};
use crate::geometry::Rect;
use crate::text::TextEngine;
use crate::theme::{Brush, Contrast, TextStyle};
use crate::{inspect, scene};

/// One element's place in the frame, in pre-order.
pub struct Node {
    pub name: &'static str,
    pub parent: Option<usize>,
    pub key: Option<Key>,
    /// The allocated border box, logical units.
    pub rect: Rect,
    /// The viewport clipping it, when a scroll ancestor has one.
    pub clip: Option<Rect>,
    /// Its children's reserved sizes exceed it, or it exceeds its parent's
    /// cross extent: kept, not shrunk, and reported here.
    pub overflow: bool,
    /// A scroll viewport's content extent and its clamped offset.
    pub scroll: Option<(f32, f32)>,
    /// The theme, contrast and interaction this node resolved under.
    pub cx: FrameCx,
    pub state: ElState,
    /// What its state recipe painted, and the rule that won.
    pub look: Look,
    pub rule: &'static str,
    /// Where a pointer reaches it, before clipping; `None` for a node that
    /// is not a target.
    pub hit: Option<Rect>,
    /// The key that region presses: its own, or the target a neutral box
    /// presses for (`El::hit_as`).
    pub hit_key: Option<Key>,
    pub role: Option<Role>,
    pub label: Option<Name>,
    pub selected: Option<bool>,
    pub disabled: bool,
    pub focusable: bool,
    pub actions: Vec<Action>,
    pub decorative: bool,
    pub(crate) text: Option<Rc<Layout<Brush>>>,
}

impl Node {
    /// The measured text's line count, for a text node.
    pub fn lines(&self) -> Option<usize> {
        self.text.as_ref().map(|layout| layout.lines().count())
    }

    /// Its pointer region, limited by the viewport clipping it.
    pub fn hit_region(&self) -> Option<Rect> {
        let region = self.hit?;
        Some(match self.clip {
            Some(clip) => intersect(region, clip),
            None => region,
        })
    }
}

/// A constraint or declaration the frame could not honour, said by element
/// (and axis, for layout) rather than silently repaired.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub element: &'static str,
    pub axis: Option<Axis>,
    pub message: &'static str,
}

/// One pointer target: a key and where it can be pressed.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct HitRegion {
    pub key: Key,
    pub region: Rect,
}

/// One accessibility node: what the host composes into its window's tree.
/// Structural boxes are transparent, so a node's children are its nearest
/// semantic descendants in reading order.
#[derive(Clone, Debug, PartialEq)]
pub struct SemNode {
    pub key: Option<Key>,
    pub role: Role,
    pub name: Option<Name>,
    pub bounds: Rect,
    pub selected: Option<bool>,
    pub disabled: bool,
    pub focusable: bool,
    pub actions: Vec<Action>,
    pub children: Vec<SemNode>,
}

/// What `El::layout` produced: every element's allocation, state, target and
/// declarations, and the diagnostics.
pub struct Frame {
    pub cx: FrameCx,
    pub nodes: Vec<Node>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Frame {
    pub(crate) fn new(cx: FrameCx) -> Self {
        Self {
            cx,
            nodes: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Every node with developer name `name`, in tree order.
    pub fn named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.nodes.iter().filter(move |node| node.name == name)
    }

    /// The first node named `name`.
    pub fn rect_of(&self, name: &str) -> Option<Rect> {
        self.named(name).next().map(|node| node.rect)
    }

    /// The node `key` identifies, when this frame shows it.
    pub fn node(&self, key: Key) -> Option<&Node> {
        self.nodes.iter().find(|node| node.key == Some(key))
    }

    /// Whether `key` is still shown: a press or focus held on a key this
    /// frame no longer has is the host's to drop, never to move to whatever
    /// took its place.
    pub fn contains(&self, key: Key) -> bool {
        self.node(key).is_some()
    }

    /// Every pointer target in paint order, clipped to its viewport.
    pub fn hits(&self) -> Vec<HitRegion> {
        self.nodes
            .iter()
            .filter_map(|node| {
                let region = node.hit_region()?;
                let key = node.hit_key?;
                (region.w > 0.0 && region.h > 0.0).then_some(HitRegion { key, region })
            })
            .collect()
    }

    /// The foremost target under a point in the frame's logical
    /// coordinates: the last painted wins where targets overlap. A region
    /// owns its start edges and not its end edges, so two rows that share
    /// an edge never both claim the line between them.
    pub fn hit_at(&self, x: f32, y: f32) -> Option<Key> {
        self.hits()
            .into_iter()
            .rev()
            .find(|hit| {
                let r = hit.region;
                x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
            })
            .map(|hit| hit.key)
    }

    /// The accessibility tree: semantic nodes nested under their nearest
    /// semantic ancestor, in reading order.
    pub fn semantics(&self) -> Vec<SemNode> {
        // Children of each semantic node, by the index of that node; roots
        // under `usize::MAX`.
        let semantic = |i: usize| self.nodes[i].role.is_some() && !self.nodes[i].decorative;
        let owner = |mut i: usize| loop {
            match self.nodes[i].parent {
                Some(parent) if semantic(parent) => return parent,
                Some(parent) => i = parent,
                None => return usize::MAX,
            }
        };
        let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in (0..self.nodes.len()).filter(|i| semantic(*i)) {
            children.entry(owner(i)).or_default().push(i);
        }
        fn build(frame: &Frame, children: &HashMap<usize, Vec<usize>>, i: usize) -> SemNode {
            let node = &frame.nodes[i];
            SemNode {
                key: node.key,
                role: node.role.expect("a semantic node has a role"),
                name: node.label.clone(),
                bounds: node.rect,
                selected: node.selected,
                disabled: node.disabled,
                focusable: node.focusable,
                actions: node.actions.clone(),
                children: children
                    .get(&i)
                    .into_iter()
                    .flatten()
                    .map(|c| build(frame, children, *c))
                    .collect(),
            }
        }
        children
            .get(&usize::MAX)
            .into_iter()
            .flatten()
            .map(|i| build(self, &children, *i))
            .collect()
    }

    /// The offset `key`'s scroll viewport needs to show it whole, when it
    /// is in one and is not showing whole now. The host stores the offset;
    /// it is never asked to substitute another item.
    pub fn reveal(&self, key: Key) -> Option<(usize, f32)> {
        let index = self.nodes.iter().position(|node| node.key == Some(key))?;
        let target = self.nodes[index].rect;
        let mut at = self.nodes[index].parent;
        while let Some(i) = at {
            let node = &self.nodes[i];
            if let Some((_, offset)) = node.scroll {
                let view = node.rect;
                let delta = if target.y < view.y {
                    target.y - view.y
                } else if target.y + target.h > view.y + view.h {
                    target.y + target.h - (view.y + view.h)
                } else {
                    return None;
                };
                return Some((i, offset + delta));
            }
            at = node.parent;
        }
        None
    }

    /// Paint `root`, the tree this frame was laid out from.
    pub fn paint(&self, root: &El, scene: &mut Scene, transform: Affine, engine: &mut TextEngine) {
        let mut next = 0;
        self.paint_el(root, &mut next, scene, transform, engine);
    }

    /// Step over `el`'s subtree without painting it: a row scrolled wholly
    /// out of its viewport costs nothing to draw.
    fn skip_el(el: &El, next: &mut usize) {
        *next += 1;
        for child in &el.children {
            Self::skip_el(child, next);
        }
    }

    fn paint_el(
        &self,
        el: &El,
        next: &mut usize,
        scene: &mut Scene,
        transform: Affine,
        engine: &mut TextEngine,
    ) {
        let index = *next;
        let Some(node) = self.nodes.get(index) else {
            *next += 1;
            return;
        };
        if let Some(clip) = node.clip
            && {
                let seen = intersect(node.rect, clip);
                seen.w <= 0.0 || seen.h <= 0.0
            }
        {
            Self::skip_el(el, next);
            return;
        }
        *next += 1;
        let r = node.rect;
        let cx = node.cx;
        let palette = cx.palette();
        // The element's record (§3.9), sourced where it was written.
        let _record = inspect::scope_at(
            el.name,
            "element",
            transform,
            scene::rect(r.x, r.y, r.w, r.h),
            el.site,
            |record| super::record::describe(self, index, el, record),
        );
        if let Some(surface) = node.look.bg {
            let radius = el.radius.px(cx.profile, r.w, r.h);
            scene::fill(
                scene,
                transform,
                surface.of(palette),
                &scene::rrect(r.x, r.y, r.w, r.h, radius),
            );
        }
        let [start, top, end, bottom] = el.pad.map(|spacing| spacing.px(cx.profile));
        let inner = Rect::new(
            r.x + start,
            r.y + top,
            (r.w - start - end).max(0.0),
            (r.h - top - bottom).max(0.0),
        );
        match &el.kind {
            Kind::Text(t) => {
                if let Some(layout) = &node.text {
                    let colour = node.look.ink.unwrap_or(t.ink).of(palette);
                    match t.mode {
                        TextMode::Wrap => scene::draw_layout(
                            scene,
                            transform,
                            layout,
                            inner.x,
                            inner.y,
                            Some(colour),
                        ),
                        // One line, centred on its line box in the height it
                        // was given.
                        TextMode::Single | TextMode::Truncate => {
                            let y = if t.ink_centred {
                                scene::centre_y_in(layout, inner)
                            } else {
                                inner.y + (inner.h - line_height(t)) * 0.5
                            };
                            scene::draw_layout_first_line(
                                scene,
                                transform,
                                layout,
                                inner.x,
                                y,
                                Some(colour),
                            );
                        }
                    }
                }
            }
            Kind::Icon(i) => {
                let layout = engine.icon_layout(i.glyph, i.size.px(cx.profile));
                scene::draw_layout(
                    scene,
                    transform,
                    &layout,
                    inner.x + (inner.w - layout.width()) * 0.5,
                    inner.y + (inner.h - layout.height()) * 0.5,
                    Some(node.look.ink.unwrap_or(i.ink).of(palette)),
                );
            }
            Kind::Leaf(adapter) => adapter.paint(scene, transform, engine, r, &cx, node.state),
            Kind::Divider(line) => {
                scene::fill(
                    scene,
                    transform,
                    line.of(palette),
                    &scene::rect(r.x, r.y, r.w, r.h),
                );
            }
            Kind::Scroll { .. } => {
                scene.push_clip_layer(
                    peniko::Fill::NonZero,
                    transform,
                    &scene::rect(r.x, r.y, r.w, r.h),
                );
                let _clip = inspect::clip(transform, scene::rect(r.x, r.y, r.w, r.h));
                for child in &el.children {
                    self.paint_el(child, next, scene, transform, engine);
                }
                scene.pop_layer();
                return;
            }
            Kind::Stack(_) => {}
        }
        for child in &el.children {
            self.paint_el(child, next, scene, transform, engine);
        }
    }
}

fn line_height(t: &TextEl) -> f32 {
    t.role.style().metrics.line_height
}

pub(crate) fn intersect(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.w).min(b.x + b.w);
    let y1 = (a.y + a.h).min(b.y + b.h);
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct TextKey {
    content: String,
    role: &'static str,
    size: u32,
    line_height: u32,
    weight: u32,
    mode: u8,
    width: u32,
    /// Shaped layouts carry a brush, so a layout made under one theme or
    /// contrast is never handed to another.
    theme: &'static str,
    contrast: Contrast,
}

/// Shaped text, kept across frames. Keyed on everything that changes the
/// shape — content, role, persistent weight, mode, the width it was broken
/// at — and on the theme and contrast, so a theme switch or a differently
/// themed subtree never reuses a stale layout.
///
/// **Bounded.** A sidebar dragged wider re-breaks every visible name at
/// every width it passes; past [`TextCache::CAP`] entries, what the last
/// few layouts did not use is let go, so a long drag costs no memory.
#[derive(Default)]
pub struct TextCache {
    /// Shared, not copied: a hit hands back the shaped layout itself. Each
    /// entry remembers the layout that last used it.
    map: HashMap<TextKey, (Rc<Layout<Brush>>, u64)>,
    /// Layouts begun with this cache.
    generation: u64,
    hits: u64,
    misses: u64,
}

impl TextCache {
    pub(crate) fn key(t: &TextEl, style: &TextStyle, width: Option<f32>, cx: &FrameCx) -> TextKey {
        TextKey {
            content: t.content.clone(),
            role: t.role.name,
            size: style.metrics.size.to_bits(),
            line_height: style.metrics.line_height.to_bits(),
            weight: style.weight.value().to_bits(),
            mode: t.mode as u8,
            width: width.map_or(u32::MAX, f32::to_bits),
            theme: cx.theme.key,
            contrast: cx.contrast,
        }
    }

    /// Entries kept before the unused ones are let go.
    pub const CAP: usize = 1024;

    /// Layouts an entry may go unused for before it can be let go: an
    /// input layout and a paint layout a frame, for a couple of frames.
    const KEEP: u64 = 4;

    /// A layout is starting.
    pub(crate) fn begin(&mut self) {
        self.generation += 1;
    }

    pub(crate) fn get(&mut self, key: &TextKey) -> Option<Rc<Layout<Brush>>> {
        let generation = self.generation;
        match self.map.get_mut(key) {
            Some((layout, used)) => {
                *used = generation;
                self.hits += 1;
                Some(Rc::clone(layout))
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    pub(crate) fn put(&mut self, key: TextKey, layout: Rc<Layout<Brush>>) {
        if self.map.len() >= Self::CAP {
            let recent = self.generation.saturating_sub(Self::KEEP);
            self.map.retain(|_, (_, used)| *used >= recent);
        }
        self.map.insert(key, (layout, self.generation));
    }

    /// Entries held now.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Layouts handed back without shaping.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Layouts that had to be shaped.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }
}
