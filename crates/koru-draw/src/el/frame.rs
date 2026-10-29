// SPDX-License-Identifier: MPL-2.0

//! The one record a layout produces, and the paint that reads it.
//!
//! Paint uses the text layouts measured during layout and never re-breaks
//! them; colours resolve through the frame's own theme and contrast, so two
//! subtrees laid out under two themes paint as two themes.

use std::collections::HashMap;

use parley::Layout;
use vello::Scene;
use vello::kurbo::Affine;

use super::{Axis, El, FrameCx, Kind, TextEl, TextMode};
use crate::geometry::Rect;
use crate::text::TextEngine;
use crate::theme::{Brush, Contrast, TextStyle};
use crate::{inspect, scene};

/// One element's place in the frame, in pre-order.
pub struct Node {
    pub name: &'static str,
    pub parent: Option<usize>,
    /// The allocated border box, logical units.
    pub rect: Rect,
    /// The viewport clipping it, when a scroll ancestor has one.
    pub clip: Option<Rect>,
    /// Its children's reserved sizes exceed it, or it exceeds its parent's
    /// cross extent: kept, not shrunk, and reported here.
    pub overflow: bool,
    /// A scroll viewport's content extent and its clamped offset.
    pub scroll: Option<(f32, f32)>,
    pub(crate) text: Option<Layout<Brush>>,
}

impl Node {
    /// The measured text's line count, for a text node.
    pub fn lines(&self) -> Option<usize> {
        self.text.as_ref().map(|layout| layout.lines().count())
    }
}

/// A layout constraint the solver could not honour, said by element and axis
/// rather than silently repaired.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub element: &'static str,
    pub axis: Axis,
    pub message: &'static str,
}

/// What `El::layout` produced: every element's allocation, and the
/// diagnostics, under one frame context.
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

    /// Paint `root`, the tree this frame was laid out from.
    pub fn paint(&self, root: &El, scene: &mut Scene, transform: Affine, engine: &mut TextEngine) {
        let mut next = 0;
        self.paint_el(root, &mut next, scene, transform, engine);
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
        *next += 1;
        let Some(node) = self.nodes.get(index) else {
            return;
        };
        let r = node.rect;
        let palette = self.cx.palette();
        let _record = inspect::scope(
            el.name,
            "element",
            transform,
            scene::rect(r.x, r.y, r.w, r.h),
            |record| {
                record.property("theme", self.cx.theme.key, "FrameCx::theme");
                record.property(
                    "appearance",
                    self.cx.theme.appearance.key(),
                    "Theme::appearance",
                );
                record.property("contrast", self.cx.contrast.key(), "FrameCx::contrast");
            },
        );
        if let Some(surface) = el.bg {
            let radius = el.radius.px(self.cx.profile, r.w, r.h);
            scene::fill(
                scene,
                transform,
                surface.of(palette),
                &scene::rrect(r.x, r.y, r.w, r.h, radius),
            );
        }
        let [start, top, end, bottom] = el.pad.map(|space| space.px(self.cx.profile));
        let inner = Rect::new(
            r.x + start,
            r.y + top,
            (r.w - start - end).max(0.0),
            (r.h - top - bottom).max(0.0),
        );
        match &el.kind {
            Kind::Text(t) => {
                if let Some(layout) = &node.text {
                    let colour = t.ink.of(palette);
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
                            let line = line_height(t);
                            scene::draw_layout_first_line(
                                scene,
                                transform,
                                layout,
                                inner.x,
                                inner.y + (inner.h - line) * 0.5,
                                Some(colour),
                            );
                        }
                    }
                }
            }
            Kind::Icon(i) => {
                let layout = engine.icon_layout(i.glyph, i.size.px(self.cx.profile));
                scene::draw_layout(
                    scene,
                    transform,
                    &layout,
                    inner.x + (inner.w - layout.width()) * 0.5,
                    inner.y + (inner.h - layout.height()) * 0.5,
                    Some(i.ink.of(palette)),
                );
            }
            Kind::Leaf(adapter) => adapter.paint(scene, transform, engine, r, &self.cx),
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
#[derive(Default)]
pub struct TextCache {
    map: HashMap<TextKey, Layout<Brush>>,
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

    pub(crate) fn get(&mut self, key: &TextKey) -> Option<Layout<Brush>> {
        let found = self.map.get(key).cloned();
        if found.is_some() {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
        found
    }

    pub(crate) fn put(&mut self, key: TextKey, layout: Layout<Brush>) {
        self.map.insert(key, layout);
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
