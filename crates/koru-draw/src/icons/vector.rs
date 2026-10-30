// SPDX-License-Identifier: MPL-2.0

//! Compiled icon geometry. Layout carries identity; the shared painter resolves
//! it without shaping an icon font or reparsing SVG on each frame.

use std::sync::OnceLock;

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Join, Stroke};
use vello::peniko::{Color, Fill};

use super::data::ICON_DEFS;
use crate::geometry::Rect;

pub use super::data::{ACTIVE_PACK, DEFAULT_STROKE_WIDTH};

pub struct IconDef {
    pub codepoint: char,
    pub name: &'static str,
    pub source: &'static str,
    pub stroke_d: &'static str,
    pub fill_d: &'static str,
    pub active_fill_d: Option<&'static str>,
}

pub const ICON_BOX_TAG: u64 = 0x4B4F_5255_0000_0000;
pub const ICON_BOX_MASK: u64 = 0xFFFF_FFFF_FFFE_0000;

/// Layout-only identity. Icon constants remain stable BMP private-use values.
pub const fn pack_box_id(icon: char, filled: bool) -> u64 {
    if (icon as u32) < 0xE000 || (icon as u32) > 0xF8FF {
        return 0;
    }
    ICON_BOX_TAG | ((filled as u64) << 16) | (icon as u32 as u64)
}

pub const fn unpack_box_id(id: u64) -> Option<(char, bool)> {
    if id & ICON_BOX_MASK != ICON_BOX_TAG {
        return None;
    }
    let cp = (id & 0xFFFF) as u32;
    if cp < 0xE000 || cp > 0xF8FF {
        return None;
    }
    match char::from_u32(cp) {
        Some(icon) => Some((icon, id & (1 << 16) != 0)),
        None => None,
    }
}

fn index(icon: char) -> Option<usize> {
    ICON_DEFS
        .binary_search_by_key(&icon, |def| def.codepoint)
        .ok()
}

pub fn has_vector_icon(icon: char) -> bool {
    index(icon).is_some()
}

pub fn vector_def(icon: char) -> Option<&'static IconDef> {
    index(icon).map(|index| &ICON_DEFS[index])
}

struct ParsedIcon {
    stroke: Option<BezPath>,
    fill: Option<BezPath>,
    active_fill: Option<BezPath>,
}

static PARSED: [OnceLock<ParsedIcon>; ICON_DEFS.len()] =
    [const { OnceLock::new() }; ICON_DEFS.len()];

fn parsed(index: usize) -> &'static ParsedIcon {
    PARSED[index].get_or_init(|| {
        let def = &ICON_DEFS[index];
        let parse = |d: &str| {
            (!d.is_empty()).then(|| {
                BezPath::from_svg(d).expect("generated icon paths are validated by catalog tests")
            })
        };
        ParsedIcon {
            stroke: parse(def.stroke_d),
            fill: parse(def.fill_d),
            active_fill: def.active_fill_d.and_then(parse),
        }
    })
}

pub fn draw_vector_icon(
    scene: &mut Scene,
    root: Affine,
    icon: char,
    filled: bool,
    slot: Rect,
    color: Color,
) {
    let Some(index) = index(icon) else { return };
    let side = f64::from(slot.w.min(slot.h));
    if !side.is_finite() || side <= 0.0 {
        return;
    }
    let geometry = parsed(index);
    let transform =
        root * Affine::translate((
            f64::from(slot.x) + (f64::from(slot.w) - side) * 0.5,
            f64::from(slot.y) + (f64::from(slot.h) - side) * 0.5,
        )) * Affine::scale(side / 24.0);
    if let Some(active) = geometry.active_fill.as_ref().filter(|_| filled) {
        scene.fill(Fill::NonZero, transform, color, None, active);
    } else {
        if let Some(path) = &geometry.stroke {
            let width = DEFAULT_STROKE_WIDTH + if filled { 0.50 } else { 0.0 };
            scene.stroke(
                &Stroke::new(width)
                    .with_caps(Cap::Round)
                    .with_join(Join::Round),
                transform,
                color,
                None,
                path,
            );
        }
        if let Some(path) = &geometry.fill {
            scene.fill(Fill::NonZero, transform, color, None, path);
        }
    }
    if crate::inspect::enabled() {
        let def = &ICON_DEFS[index];
        crate::inspect::leaf(
            "icon",
            root,
            crate::scene::rect(slot.x, slot.y, slot.w, slot.h),
            |record| {
                record.property("icon pack", ACTIVE_PACK, "compiled icon pack");
                record.property(
                    "icon source",
                    def.source,
                    "upstream or original Koru artwork",
                );
                record.property("icon name", def.name, "icons::ALL canonical name");
                record.property(
                    "icon codepoint",
                    format!("U+{:04X}", icon as u32),
                    "stable icon identity",
                );
                record.property(
                    "icon variant",
                    if filled { "filled" } else { "regular" },
                    "requested state",
                );
                record.property("icon size", side, "layout slot");
                record.property("foreground", crate::inspect::color(color), "resolved paint");
            },
        );
    }
}

#[cfg(test)]
mod tests;
