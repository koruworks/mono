// SPDX-License-Identifier: MPL-2.0

//! The bounded stack solver: ELEMENTS.final §5.1 and §5.2, and nothing past
//! them. One line per stack, Hug / Fixed / weighted Fill on the main axis,
//! Stretch or Start/Centre/End across it, no implicit shrinking, width first
//! so text measures at the width it will be painted at.

use parley::Layout;

use std::collections::HashSet;

use super::frame::{Diagnostic, Frame, Node, TextCache, intersect};
use super::{Align, Axis, El, ElState, FrameCx, Kind, Length, Look, Role, TextEl, TextMode};
use crate::geometry::Rect;
use crate::text::TextEngine;
use crate::theme::{BORDER_W, Brush, Space, TextStyle};

pub(super) fn layout(
    root: &El,
    engine: &mut TextEngine,
    cache: &mut TextCache,
    area: Rect,
    cx: FrameCx,
) -> Frame {
    let mut solver = Solver {
        engine,
        cache,
        cx,
        frame: Frame::new(cx),
    };
    solver.place(root, area, None, None);
    solver.validate();
    solver.frame
}

struct Solver<'a> {
    engine: &'a mut TextEngine,
    cache: &'a mut TextCache,
    cx: FrameCx,
    frame: Frame,
}

/// A child's claim on its stack's main axis.
#[derive(Copy, Clone)]
struct Claim {
    /// What it reserves: a Hug or fixed size, or a Fill's minimum.
    size: f32,
    /// `(weight, maximum)` for a Fill.
    fill: Option<(f32, f32)>,
}

fn style_of(t: &TextEl) -> TextStyle {
    let mut style = t.role.style();
    if let Some(weight) = t.weight {
        style.weight = weight;
    }
    style
}

impl Solver<'_> {
    fn px(&self, length: Length) -> f32 {
        length.px(self.cx.profile).max(0.0)
    }

    fn space(&self, space: Space) -> f32 {
        space.px(self.cx.profile).max(0.0)
    }

    /// Start, top, end, bottom.
    fn pad(&self, el: &El) -> [f32; 4] {
        el.pad.map(|space| self.space(space))
    }

    fn clamp(&self, value: f32, min: Option<Length>, max: Option<Length>) -> f32 {
        let lo = min.map_or(0.0, |m| self.px(m));
        let hi = max.map_or(f32::INFINITY, |m| self.px(m)).max(lo);
        value.clamp(lo, hi)
    }

    fn clamp_w(&self, el: &El, value: f32) -> f32 {
        self.clamp(value, el.min_w, el.max_w)
    }

    fn clamp_h(&self, el: &El, value: f32) -> f32 {
        self.clamp(value, el.min_h, el.max_h)
    }

    fn diagnose(&mut self, el: &El, axis: Axis, message: &'static str) {
        self.report(el.name, Some(axis), message);
    }

    fn report(&mut self, element: &'static str, axis: Option<Axis>, message: &'static str) {
        let diagnostic = Diagnostic {
            element,
            axis,
            message,
        };
        if !self.frame.diagnostics.contains(&diagnostic) {
            self.frame.diagnostics.push(diagnostic);
        }
    }

    /// `t` shaped at `width` (`None`: its natural, unwrapped line).
    fn shape(&mut self, t: &TextEl, width: Option<f32>) -> Layout<Brush> {
        let style = style_of(t);
        let width = match t.mode {
            TextMode::Single => None,
            TextMode::Truncate | TextMode::Wrap => width,
        };
        let key = TextCache::key(t, &style, width, &self.cx);
        if let Some(layout) = self.cache.get(&key) {
            return layout;
        }
        let layout = match (t.mode, width) {
            (TextMode::Truncate, Some(w)) => {
                let fitted = self.engine.elided(&t.content, style, w);
                self.engine.simple_layout(&fitted, style)
            }
            (TextMode::Wrap, Some(w)) => self.engine.simple_layout_wrapped(&t.content, style, w),
            _ => self.engine.simple_layout(&t.content, style),
        };
        self.cache.put(key, layout.clone());
        layout
    }

    /// Whether `child` fills along `axis` inside a stack laid along it.
    fn fills(child: &El) -> bool {
        child.fill.is_some()
    }

    /// The natural, unconstrained width.
    fn natural_w(&mut self, el: &El) -> f32 {
        if let Some(w) = el.w {
            return self.clamp_w(el, self.px(w));
        }
        let [start, _, end, _] = self.pad(el);
        let inner = match &el.kind {
            Kind::Text(t) => self.shape(t, None).width(),
            Kind::Icon(i) => i.size.px(self.cx.profile),
            Kind::Leaf(adapter) => adapter.measure(None).0,
            Kind::Divider(_) => BORDER_W as f32,
            Kind::Stack(Axis::Horizontal) => {
                if el.children.iter().any(Self::fills) {
                    self.diagnose(
                        el,
                        Axis::Horizontal,
                        "a Fill child needs a finite width, and this row hugs its own",
                    );
                }
                let gaps = self.gaps(el);
                let mut sum = gaps;
                for child in &el.children {
                    sum += self.claim(child, Axis::Horizontal, 0.0).size;
                }
                sum
            }
            Kind::Stack(Axis::Vertical) | Kind::Scroll { .. } => {
                let mut widest: f32 = 0.0;
                for child in &el.children {
                    widest = widest.max(self.natural_w(child));
                }
                widest
            }
        };
        self.clamp_w(el, inner + start + end)
    }

    /// The height `el` needs at width `w`.
    fn height_at(&mut self, el: &El, w: f32) -> f32 {
        if let Some(h) = el.h {
            return self.clamp_h(el, self.px(h));
        }
        let [start, top, end, bottom] = self.pad(el);
        let inner_w = (w - start - end).max(0.0);
        let inner = match &el.kind {
            Kind::Text(t) => match t.mode {
                TextMode::Wrap => self.shape(t, Some(inner_w)).height(),
                TextMode::Single | TextMode::Truncate => style_of(t).metrics.line_height,
            },
            Kind::Icon(i) => i.size.px(self.cx.profile),
            Kind::Leaf(adapter) => adapter.measure(Some(inner_w)).1,
            Kind::Divider(_) => BORDER_W as f32,
            Kind::Stack(Axis::Vertical) | Kind::Scroll { .. } => {
                if matches!(el.kind, Kind::Stack(_)) && el.children.iter().any(Self::fills) {
                    self.diagnose(
                        el,
                        Axis::Vertical,
                        "a Fill child needs a finite height, and this column hugs its own",
                    );
                }
                let mut sum = self.gaps(el);
                for child in &el.children {
                    let cw = self.cross_size(el, child, Axis::Vertical, inner_w, None);
                    sum += self.claim(child, Axis::Vertical, cw).size;
                }
                sum
            }
            Kind::Stack(Axis::Horizontal) => {
                let (widths, _) = self.main_sizes(el, Axis::Horizontal, inner_w, 0.0);
                let mut tallest: f32 = 0.0;
                for (child, cw) in el.children.iter().zip(widths) {
                    let h = match child.h {
                        Some(h) => self.clamp_h(child, self.px(h)),
                        None => self.height_at(child, cw),
                    };
                    tallest = tallest.max(h);
                }
                tallest
            }
        };
        self.clamp_h(el, inner + top + bottom)
    }

    fn gaps(&self, el: &El) -> f32 {
        let n = el.children.len();
        if n < 2 {
            0.0
        } else {
            self.space(el.gap) * (n - 1) as f32
        }
    }

    /// A child's claim along `axis`. `cross` is its width when the axis is
    /// vertical, which its natural height depends on.
    fn claim(&mut self, child: &El, axis: Axis, cross: f32) -> Claim {
        let (fixed, min, max) = match axis {
            Axis::Horizontal => (child.w, child.min_w, child.max_w),
            Axis::Vertical => (child.h, child.min_h, child.max_h),
        };
        if matches!(child.kind, Kind::Divider(_)) && fixed.is_none() {
            return Claim {
                size: BORDER_W as f32,
                fill: None,
            };
        }
        if let Some(weight) = child.fill
            && fixed.is_none()
        {
            let lo = min.map_or(0.0, |m| self.px(m));
            let hi = max.map_or(f32::INFINITY, |m| self.px(m)).max(lo);
            return Claim {
                size: lo,
                fill: Some((weight.max(f32::MIN_POSITIVE), hi)),
            };
        }
        let size = match axis {
            Axis::Horizontal => self.natural_w(child),
            Axis::Vertical => self.height_at(child, cross),
        };
        Claim { size, fill: None }
    }

    /// Every child's size along `axis` in `avail`, and the spare space left
    /// at the end (negative when the reserved sizes overflow it).
    fn main_sizes(&mut self, el: &El, axis: Axis, avail: f32, cross: f32) -> (Vec<f32>, f32) {
        let mut claims = Vec::with_capacity(el.children.len());
        for child in &el.children {
            let cw = match axis {
                Axis::Vertical => self.cross_size(el, child, axis, cross, None),
                Axis::Horizontal => cross,
            };
            claims.push(self.claim(child, axis, cw));
        }
        let gaps = self.gaps(el);
        let sizes = distribute(&claims, avail - gaps);
        let spare = avail - gaps - sizes.iter().sum::<f32>();
        (sizes, spare)
    }

    /// A child's size across a stack laid along `axis`, in `avail`.
    /// `main` is its main-axis size when the cross size depends on it (a
    /// row's children's heights depend on their widths).
    fn cross_size(
        &mut self,
        parent: &El,
        child: &El,
        axis: Axis,
        avail: f32,
        main: Option<f32>,
    ) -> f32 {
        let align = child.align_self.unwrap_or(parent.align);
        match axis {
            // Across a column is a width.
            Axis::Vertical => {
                if let Some(w) = child.w {
                    return self.clamp_w(child, self.px(w));
                }
                if matches!(child.kind, Kind::Divider(_))
                    || (align == Align::Stretch && !child.hug_w)
                {
                    return self.clamp_w(child, avail);
                }
                let natural = self.natural_w(child);
                self.clamp_w(child, natural)
            }
            // Across a row is a height.
            Axis::Horizontal => {
                if let Some(h) = child.h {
                    return self.clamp_h(child, self.px(h));
                }
                if matches!(child.kind, Kind::Divider(_))
                    || (align == Align::Stretch && !child.hug_h)
                {
                    return self.clamp_h(child, avail);
                }
                let w = main.unwrap_or_else(|| self.natural_w(child));
                self.height_at(child, w)
            }
        }
    }

    /// `el`'s state this frame: its own declarations, and the host's
    /// pointer and keyboard state on its key or on any key it is lit by.
    fn state_of(&self, el: &El, cx: &FrameCx) -> ElState {
        let mine = |key: Option<super::Key>| {
            key.is_some_and(|key| el.key == Some(key) || el.activity.contains(&key))
        };
        let disabled = el.disabled;
        ElState {
            hovered: !disabled && mine(cx.interaction.hot),
            pressed: !disabled && mine(cx.interaction.pressed),
            focused: mine(cx.interaction.focused),
            selected: el.selected.unwrap_or(false),
            disabled,
        }
    }

    fn place(&mut self, el: &El, rect: Rect, parent: Option<usize>, clip: Option<Rect>) -> usize {
        // A subtree may carry its own theme; everything under it resolves,
        // shapes and paints under that one.
        let outer = self.cx;
        if let Some(theme) = el.theme {
            self.cx.theme = theme;
        }
        let cx = self.cx;
        let state = self.state_of(el, &cx);
        let (look, rule) = match el.states {
            Some(recipe) => {
                let (mut look, rule) = recipe.resolve(state);
                look.bg = look.bg.or(el.bg);
                (look, rule)
            }
            None => (
                Look {
                    bg: el.bg,
                    ink: None,
                },
                "base",
            ),
        };
        let hit_key = if el.decorative {
            None
        } else if el.hit_as.is_some() {
            el.hit_as
        } else if el.hit {
            el.key
        } else {
            None
        };
        let hit = ((el.hit || el.hit_as.is_some()) && !el.decorative).then(|| match &el.kind {
            Kind::Leaf(adapter) => adapter.hit_region(rect),
            _ => rect,
        });
        let index = self.frame.nodes.len();
        self.frame.nodes.push(Node {
            name: el.name,
            parent,
            key: el.key,
            rect,
            clip,
            overflow: false,
            scroll: None,
            cx,
            state,
            look,
            rule,
            hit,
            hit_key,
            role: if el.decorative { None } else { el.role },
            label: el.label.clone(),
            selected: el.selected,
            disabled: el.disabled,
            focusable: el.focusable && !el.decorative,
            actions: if el.decorative {
                Vec::new()
            } else {
                el.actions.clone()
            },
            decorative: el.decorative,
            text: None,
        });
        let [start, top, end, bottom] = self.pad(el);
        let inner = Rect::new(
            rect.x + start,
            rect.y + top,
            (rect.w - start - end).max(0.0),
            (rect.h - top - bottom).max(0.0),
        );
        match &el.kind {
            Kind::Text(t) => {
                let layout = self.shape(t, Some(inner.w));
                self.frame.nodes[index].text = Some(layout);
            }
            Kind::Stack(axis) => self.place_stack(el, index, inner, *axis, clip),
            Kind::Scroll { offset } => self.place_scroll(el, index, rect, inner, *offset, clip),
            Kind::Icon(_) | Kind::Leaf(_) | Kind::Divider(_) => {}
        }
        self.cx = outer;
        index
    }

    /// Final-tree validation (§4, layer 2): what composition got wrong, said
    /// by element, never silently repaired.
    fn validate(&mut self) {
        let mut seen = HashSet::new();
        let mut found = Vec::new();
        for node in &self.frame.nodes {
            if let Some(key) = node.key
                && !seen.insert(key)
            {
                found.push((node.name, "a key is used twice in one frame"));
            }
            let target = (node.hit.is_some() && node.hit_key == node.key)
                || node.focusable
                || !node.actions.is_empty();
            if (node.role.is_some() || target) && node.key.is_none() {
                found.push((node.name, "a node, target or action needs a stable key"));
            }
            if node.decorative && (node.label.is_some() || node.role.is_some()) {
                found.push((node.name, "a decorative element has no name or role"));
            }
            if let Some(role) = node.role
                && role != Role::Group
                && node.label.is_none()
            {
                found.push((node.name, "a semantic node needs a name"));
            }
        }
        // A box that presses for another target must name one that exists.
        for node in &self.frame.nodes {
            if let Some(target) = node.hit_key
                && node.key != Some(target)
                && !self.frame.nodes.iter().any(|n| n.key == Some(target))
            {
                found.push((
                    node.name,
                    "a box presses for a target that is not in the frame",
                ));
            }
        }
        for (element, message) in found {
            self.report(element, None, message);
        }
    }

    fn place_stack(&mut self, el: &El, index: usize, inner: Rect, axis: Axis, clip: Option<Rect>) {
        let (main_avail, cross_avail) = match axis {
            Axis::Horizontal => (inner.w, inner.h),
            Axis::Vertical => (inner.h, inner.w),
        };
        let (mains, spare) = self.main_sizes(el, axis, main_avail, cross_avail);
        if spare < -0.001 {
            self.frame.nodes[index].overflow = true;
        }
        let gap = self.space(el.gap);
        let mut cursor = match axis {
            Axis::Horizontal => inner.x,
            Axis::Vertical => inner.y,
        };
        for (child, main) in el.children.iter().zip(mains) {
            let cross = self.cross_size(el, child, axis, cross_avail, Some(main));
            let align = match child.align_self.unwrap_or(el.align) {
                // An explicit size that overrode Stretch sits at the start.
                Align::Stretch => Align::Start,
                other => other,
            };
            let room = cross_avail - cross;
            let offset = if room < 0.0 {
                0.0
            } else {
                match align {
                    Align::Start | Align::Stretch => 0.0,
                    Align::Centre => room * 0.5,
                    Align::End => room,
                }
            };
            let rect = match axis {
                Axis::Horizontal => Rect::new(cursor, inner.y + offset, main, cross),
                Axis::Vertical => Rect::new(inner.x + offset, cursor, cross, main),
            };
            let child_index = self.place(child, rect, Some(index), clip);
            if room < -0.001 {
                self.frame.nodes[child_index].overflow = true;
            }
            cursor += main + gap;
        }
    }

    /// A finite viewport over a vertical Hug column of unbounded height
    /// (§5.2). The host's offset is clamped to the content.
    fn place_scroll(
        &mut self,
        el: &El,
        index: usize,
        viewport: Rect,
        inner: Rect,
        offset: f32,
        clip: Option<Rect>,
    ) {
        let mut heights = Vec::with_capacity(el.children.len());
        for child in &el.children {
            if child.fill.is_some() && child.h.is_none() {
                self.diagnose(
                    child,
                    Axis::Vertical,
                    "a Fill child needs a finite height, and scrolled content has none",
                );
            }
            let w = self.cross_size(el, child, Axis::Vertical, inner.w, None);
            let claim = self.claim(child, Axis::Vertical, w);
            heights.push((w, claim.size));
        }
        let gap = self.space(el.gap);
        let content = heights.iter().map(|(_, h)| h).sum::<f32>() + self.gaps(el);
        let offset = offset.clamp(0.0, (content - inner.h).max(0.0));
        self.frame.nodes[index].scroll = Some((content, offset));
        let clip = Some(match clip {
            Some(outer) => intersect(outer, viewport),
            None => viewport,
        });
        let mut y = inner.y - offset;
        for (child, (w, h)) in el.children.iter().zip(heights) {
            let align = child.align_self.unwrap_or(el.align);
            let room = inner.w - w;
            let x = inner.x
                + match align {
                    Align::Centre if room > 0.0 => room * 0.5,
                    Align::End if room > 0.0 => room,
                    _ => 0.0,
                };
            self.place(child, Rect::new(x, y, w, h), Some(index), clip);
            y += h + gap;
        }
    }
}

/// Split `avail` among `claims`: each keeps what it reserves; the positive
/// remainder goes to the Fills by weight, a Fill that reaches its maximum is
/// frozen there and its unused share goes round again. A deficit takes
/// nothing from anyone.
fn distribute(claims: &[Claim], avail: f32) -> Vec<f32> {
    let mut sizes: Vec<f32> = claims.iter().map(|c| c.size).collect();
    let reserved: f32 = sizes.iter().sum();
    let mut remaining = avail - reserved;
    if remaining <= 0.0 {
        return sizes;
    }
    let mut frozen: Vec<bool> = claims.iter().map(|c| c.fill.is_none()).collect();
    loop {
        let weight: f32 = claims
            .iter()
            .zip(&frozen)
            .filter(|(_, f)| !**f)
            .map(|(c, _)| c.fill.map_or(0.0, |(w, _)| w))
            .sum();
        if weight <= 0.0 || remaining <= 0.0 {
            break;
        }
        // Freeze every Fill whose share would carry it past its maximum,
        // then share again what is left.
        let mut capped = false;
        for (i, claim) in claims.iter().enumerate() {
            if frozen[i] {
                continue;
            }
            let (w, max) = claim.fill.expect("an unfrozen claim fills");
            if sizes[i] + remaining * w / weight > max {
                remaining -= max - sizes[i];
                sizes[i] = max;
                frozen[i] = true;
                capped = true;
            }
        }
        if capped {
            continue;
        }
        for (i, claim) in claims.iter().enumerate() {
            if !frozen[i] {
                let (w, _) = claim.fill.expect("an unfrozen claim fills");
                sizes[i] += remaining * w / weight;
            }
        }
        break;
    }
    sizes
}
