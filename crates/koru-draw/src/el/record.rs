// SPDX-License-Identifier: MPL-2.0

//! **An element's inspector record** (ELEMENTS.final §3.9): what F12 shows
//! for every box, text, icon, scroll viewport and leaf.
//!
//! Read from the frame the paint is using, never recomputed: the authored
//! token beside the value it resolved to, the state rule that won, the
//! geometry, and what the element is to a screen reader. Model content is
//! named as such, never copied: the inspector exports, and a workspace's
//! title is the person's, not the frame's.

use super::frame::Node;
use super::{El, Frame, Key, Kind, Length, Name, Object, Role, Spacing, TextMode};
use crate::inspect::{self, Record};
use crate::theme::{Radius, Space};

pub(super) fn describe(frame: &Frame, index: usize, el: &El, record: &mut Record) {
    let node = &frame.nodes[index];
    let cx = node.cx;
    let profile = cx.profile;
    let palette = cx.palette();

    record.property("element", kind(el), "El constructor");

    // Identity: its own key, or the keyed element it belongs to.
    match node.key {
        Some(key) => record.property("key", key_text(key), "El::id"),
        None => {
            if let Some(owner) = keyed_ancestor(frame, index) {
                record.property("within", key_text(owner), "ancestry");
            }
        }
    }
    if let Some((n, of)) = occurrence(frame, index) {
        record.property("occurrence", format!("{n} of {of} named alike"), "Frame");
    }

    record.property("profile", profile.key(), "FrameCx::profile");
    record.property("theme", cx.theme.key, "FrameCx::theme");
    record.property("appearance", cx.theme.appearance.key(), "Theme::appearance");
    record.property("contrast", cx.contrast.key(), "FrameCx::contrast");

    // Sizes, authored token first.
    if let Some(weight) = el.fill {
        record.property("fill", format!("Fill({weight})"), "El::fill");
    }
    for (name, origin, length) in [
        ("w", "El::w", el.w),
        ("h", "El::h", el.h),
        ("min_w", "El::min_w", el.min_w),
        ("max_w", "El::max_w", el.max_w),
        ("min_h", "El::min_h", el.min_h),
        ("max_h", "El::max_h", el.max_h),
    ] {
        if let Some(length) = length {
            record.property(name, length_text(length, profile), origin);
        }
    }
    let [start, top, end, bottom] = el.pad;
    if start == end && top == bottom && start == top {
        if start.space != Space::S0 {
            record.property("pad", spacing_text(start, profile), "El::pad");
        }
    } else {
        for (name, origin, spacing) in [
            ("pad_x", "El::pad_x", (start == end).then_some(start)),
            ("pad_y", "El::pad_y", (top == bottom).then_some(top)),
        ] {
            if let Some(spacing) = spacing
                && spacing.space != Space::S0
            {
                record.property(name, spacing_text(spacing, profile), origin);
            }
        }
        if start != end {
            record.property("pad_start", spacing_text(start, profile), "El::pad_start");
            record.property("pad_end", spacing_text(end, profile), "El::pad_end");
        }
        if top != bottom {
            record.property("pad_top", spacing_text(top, profile), "El::pad_top");
            record.property(
                "pad_bottom",
                spacing_text(bottom, profile),
                "El::pad_bottom",
            );
        }
    }
    if el.gap.space != Space::S0 {
        record.property("gap", spacing_text(el.gap, profile), "El::gap");
    }
    if el.radius != Radius::None {
        let r = node.rect;
        let value = el.radius.px(profile, r.w, r.h);
        let authored = match el.radius {
            Radius::Metric(metric) => format!("metrics::{}", metric.name),
            Radius::Px(v) => format!("Radius::Px({v}) exception"),
            radius => format!("Radius::{}", radius.name()),
        };
        record.property("radius", format!("{authored} -> {value}"), "El::radius");
    }

    // Colour and state.
    if let Some(recipe) = el.states {
        record.property(
            "state rule",
            format!("{}: {}", recipe.name, node.rule),
            "El::states",
        );
    }
    if el.states.is_some() || node.key.is_some() {
        record.property("state", state_text(node), "Interaction");
    }
    if let Some(surface) = node.look.bg {
        let origin = if el.states.is_some() && el.bg != Some(surface) {
            "state rule"
        } else {
            "El::bg"
        };
        record.property(
            "fill colour",
            format!(
                "{} -> {}",
                surface.name(),
                inspect::color(surface.of(palette))
            ),
            origin,
        );
    }
    match &el.kind {
        Kind::Text(t) => {
            record.property("text role", t.role.name, "El::role");
            let ink = node.look.ink.unwrap_or(t.ink);
            record.property(
                "ink",
                format!("{} -> {}", ink.name(), inspect::color(ink.of(palette))),
                if node.look.ink.is_some() {
                    "state rule"
                } else {
                    "El::ink"
                },
            );
            if let Some(weight) = t.weight {
                record.property("weight", format!("{weight:?}"), "El::emphasis");
            }
            let mode = match t.mode {
                TextMode::Single => "single line",
                TextMode::Truncate => "single line, elided",
                TextMode::Clip => "single line, cut at a word",
                TextMode::Wrap => "wrapped",
            };
            record.property("text mode", mode, "El::truncate / El::wrap");
            if t.ink_centred {
                record.property("centred", "on its ink", "El::ink_centred");
            }
            if let Some(lines) = node.lines() {
                record.property("lines", lines, "measured layout");
            }
        }
        Kind::Icon(i) => {
            record.property(
                "icon size",
                format!("IconSize::{} -> {}", i.size.name(), i.size.px(profile)),
                "icon",
            );
            let ink = node.look.ink.unwrap_or(i.ink);
            record.property(
                "ink",
                format!("{} -> {}", ink.name(), inspect::color(ink.of(palette))),
                "El::ink",
            );
        }
        Kind::Divider(line) => {
            record.property(
                "line",
                format!("{} -> {}", line.name(), inspect::color(line.of(palette))),
                "divider",
            );
        }
        Kind::Scroll { .. } | Kind::Leaf(_) | Kind::Stack(_) => {}
    }

    // Geometry beyond the record's own bounds and clip.
    if let Some((content, offset)) = node.scroll {
        record.property(
            "scroll",
            format!("content {content}, offset {offset}"),
            "runtime: host scroll offset",
        );
    }
    if node.overflow {
        record.property("overflow", "kept, not shrunk", "solver");
    }

    // What it is to a screen reader, and to the pointer.
    record.property("semantics", semantics_text(el, node), "El::semantics");
    match &node.label {
        Some(Name::Key(key)) => record.property("name", format!("Key::{key:?}"), "El::name"),
        Some(Name::Text(_)) => record.property("name", "model content", "El::name_text"),
        None => {}
    }
    if let Some(selected) = node.selected {
        record.property("selected", selected, "El::selected");
    }
    if node.disabled {
        record.property("disabled", true, "El::disabled");
    }
    record.property("input", input_text(el, node), "El::hit / El::action");
}

fn kind(el: &El) -> &'static str {
    match &el.kind {
        Kind::Stack(super::Axis::Horizontal) => "row",
        Kind::Stack(super::Axis::Vertical) => "column",
        Kind::Text(_) => "text",
        Kind::Icon(_) => "icon",
        Kind::Scroll { .. } => "scroll viewport",
        Kind::Leaf(_) => "leaf",
        Kind::Divider(_) => "divider",
    }
}

/// `model 0123…/manage` or `surface explore-popover/heading`.
pub(super) fn key_text(key: Key) -> String {
    match key.object {
        Object::Model(id) => format!("model {}/{}", uuid_text(id), key.part),
        Object::Surface(name) => format!("surface {name}/{}", key.part),
    }
}

fn uuid_text(id: u128) -> String {
    let hex = format!("{id:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn keyed_ancestor(frame: &Frame, index: usize) -> Option<Key> {
    let mut at = frame.nodes[index].parent;
    while let Some(i) = at {
        if let Some(key) = frame.nodes[i].key {
            return Some(key);
        }
        at = frame.nodes[i].parent;
    }
    None
}

/// Its place among the siblings that share its developer name, when names
/// repeat: forty rows are each "Workspace row", and n of 40.
fn occurrence(frame: &Frame, index: usize) -> Option<(usize, usize)> {
    let node = &frame.nodes[index];
    let alike: Vec<usize> = frame
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.parent == node.parent && n.name == node.name)
        .map(|(i, _)| i)
        .collect();
    if alike.len() < 2 {
        return None;
    }
    let n = alike.iter().position(|&i| i == index)? + 1;
    Some((n, alike.len()))
}

fn length_text(length: Length, profile: crate::theme::Profile) -> String {
    format!("{} -> {}", length.provenance(), length.px(profile))
}

fn spacing_text(spacing: Spacing, profile: crate::theme::Profile) -> String {
    let step = match spacing.space {
        Space::Px(v) => format!("Space::Px({v}) exception"),
        space => format!("Space::{}", space.name()),
    };
    let value = spacing.px(profile);
    match spacing.alias {
        Some(alias) => format!("alias::{alias} = {step} -> {value}"),
        None => format!("{step} -> {value}"),
    }
}

fn state_text(node: &Node) -> String {
    let s = node.state;
    let on: Vec<&str> = [
        (s.hovered, "hovered"),
        (s.pressed, "pressed"),
        (s.focused, "focused"),
        (s.selected, "selected"),
        (s.disabled, "disabled"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    if on.is_empty() {
        "rest".into()
    } else {
        on.join(", ")
    }
}

/// Decorative, a role, or none — and for an icon or leaf, which of its three
/// modes it declared.
fn semantics_text(el: &El, node: &Node) -> String {
    let classified = matches!(el.kind, Kind::Icon(_) | Kind::Leaf(_));
    match (node.decorative, node.role) {
        (true, _) => "decorative".into(),
        (false, Some(Role::Image)) if classified && matches!(node.label, Some(Name::Key(_))) => {
            "Image: meaningful, by alt".into()
        }
        (false, Some(role)) if classified => format!("{role:?}: delegated to the leaf's own node"),
        (false, Some(role)) => format!("{role:?}"),
        (false, None) => "none: transparent to semantic parenting".into(),
    }
}

fn input_text(el: &El, node: &Node) -> String {
    let mut parts = Vec::new();
    match (node.hit.is_some(), node.hit_key) {
        (true, Some(target)) if Some(target) == node.key => parts.push("pointer target".into()),
        (true, Some(target)) => parts.push(format!("presses {}", key_text(target))),
        _ => {}
    }
    if node.focusable {
        parts.push("focusable".into());
    }
    if !node.actions.is_empty() {
        let actions: Vec<String> = node.actions.iter().map(|a| format!("{a:?}")).collect();
        parts.push(actions.join("/"));
    }
    if !el.activity.is_empty() {
        let keys: Vec<String> = el.activity.iter().map(|k| key_text(*k)).collect();
        parts.push(format!("lit by {}", keys.join(", ")));
    }
    if parts.is_empty() {
        "neutral".into()
    } else {
        parts.join("; ")
    }
}
