// SPDX-License-Identifier: MPL-2.0

//! Compiled icon geometry. Layout carries identity; the shared painter resolves
//! it without shaping an icon font or reparsing SVG on each frame.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::theme::preview::{self, Preview, PreviewSlot};

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Join, Stroke};
use vello::peniko::{Color, Fill};

use super::data::ICON_DEFS;
use crate::geometry::Rect;

pub use super::data::{ACTIVE_PACK, DEFAULT_STROKE_WIDTH};

/// All artwork is bundled; selecting a pack only changes paint, never layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum IconPack {
    Phosphor,
    Tabler,
    Lucide,
    Remix,
}

impl IconPack {
    pub const ALL: [Self; 4] = [Self::Phosphor, Self::Tabler, Self::Lucide, Self::Remix];

    pub fn parse(token: &str) -> Self {
        match token {
            "tabler" => Self::Tabler,
            "lucide" => Self::Lucide,
            "remix" => Self::Remix,
            _ => Self::Phosphor,
        }
    }

    pub const fn token(self) -> &'static str {
        match self {
            Self::Phosphor => "phosphor",
            Self::Tabler => "tabler",
            Self::Lucide => "lucide",
            Self::Remix => "remix",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Phosphor => "Phosphor",
            Self::Tabler => "Tabler",
            Self::Lucide => "Lucide",
            Self::Remix => "Remix",
        }
    }

    fn defs(self) -> &'static [IconDef] {
        match self {
            Self::Phosphor => &super::data::phosphor::ICON_DEFS,
            Self::Tabler => &super::data::tabler::ICON_DEFS,
            Self::Lucide => &super::data::lucide::ICON_DEFS,
            Self::Remix => &super::data::remix::ICON_DEFS,
        }
    }
}

/// `SELECTED` while nobody has chosen a pack: draw this build's default.
const FOLLOW_DEFAULT: u8 = u8::MAX;

static SELECTED: AtomicU8 = AtomicU8::new(FOLLOW_DEFAULT);

/// The pack in effect, independent of a menu's temporary preview: the
/// person's choice, or this build's default when they have not made one.
pub fn active_pack() -> IconPack {
    chosen_pack().unwrap_or(super::data::DEFAULT_PACK)
}

/// The person's choice, or `None` while the app's default applies.
///
/// **What is saved, not what is drawn.** "Follow the default" persists as no
/// key, as a font role does, so a later build's default reaches everybody who
/// never chose; reading the drawn pack back would freeze today's default into
/// every file on its first save.
pub fn chosen_pack() -> Option<IconPack> {
    IconPack::ALL
        .get(SELECTED.load(Ordering::Relaxed) as usize)
        .copied()
}

/// Choose `pack`. Clears any preview.
pub fn set_icon_pack(pack: IconPack) {
    set_icon_pack_setting(Some(pack));
}

/// Set the saved state: a choice, or `None` to follow the build's default.
pub fn set_icon_pack_setting(pack: Option<IconPack>) {
    SELECTED.store(
        pack.map_or(FOLLOW_DEFAULT, |pack| pack as u8),
        Ordering::Relaxed,
    );
    preview::clear(PreviewSlot::IconPack);
}

/// Artwork to paint now; persistence must continue to read `active_pack`.
pub fn rendered_pack() -> IconPack {
    preview::previewed(PreviewSlot::IconPack)
        .and_then(|i| IconPack::ALL.get(usize::from(i)).copied())
        .unwrap_or_else(active_pack)
}

/// A menu owns its preview. Dismissal, navigation and destruction restore the
/// saved choice. An older menu cannot clear or overwrite a newer preview —
/// the shared [`Preview`] guard's rule.
pub struct IconPreview(Preview);

impl IconPreview {
    pub fn new(pack: IconPack) -> Self {
        Self(Preview::new(PreviewSlot::IconPack, pack as u16))
    }

    pub fn set(&mut self, pack: IconPack) {
        self.0.set(pack as u16);
    }
}

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
    vector_def_for(rendered_pack(), icon)
}

pub fn vector_def_for(pack: IconPack, icon: char) -> Option<&'static IconDef> {
    index(icon).map(|index| &pack.defs()[index])
}

struct ParsedIcon {
    stroke: Option<BezPath>,
    fill: Option<BezPath>,
    active_fill: Option<BezPath>,
}

static PARSED: [[OnceLock<ParsedIcon>; ICON_DEFS.len()]; IconPack::ALL.len()] =
    [const { [const { OnceLock::new() }; ICON_DEFS.len()] }; IconPack::ALL.len()];

fn parsed(pack: IconPack, index: usize) -> &'static ParsedIcon {
    PARSED[pack as usize][index].get_or_init(|| {
        let def = &pack.defs()[index];
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
    draw_vector_icon_for(rendered_pack(), scene, root, icon, filled, slot, color);
}

fn draw_vector_icon_for(
    pack: IconPack,
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
    let geometry = parsed(pack, index);
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
        let def = &pack.defs()[index];
        crate::inspect::leaf(
            "icon",
            root,
            crate::scene::rect(slot.x, slot.y, slot.w, slot.h),
            |record| {
                record.property("icon pack", pack.label(), "selected icon pack");
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
