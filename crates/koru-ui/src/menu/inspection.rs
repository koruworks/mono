// SPDX-License-Identifier: MPL-2.0

//! Evidence from Menu's existing layout and state, never a second menu model.

use super::*;
use koru_draw::inspect::{self, Record, Scope};

/// Optional identities supplied by the host that owns this opening. All IDs
/// share that host's namespace; none are minted by the inspector. A callback
/// borrows the existing row mapping and runs only while capture is enabled.
#[derive(Default)]
pub struct KoruMenuInspection<'a> {
    pub occurrence: Option<u64>,
    pub row_occurrence: Option<&'a dyn Fn(usize) -> Option<u64>>,
    pub owner_row: Option<u64>,
    pub trigger: Option<u64>,
}

fn identity(record: &mut Record, name: &'static str, value: Option<u64>) {
    match value {
        Some(value) => record.property(
            name,
            value,
            "host association; host-local occurrence namespace",
        ),
        None => record.property(name, "unavailable", "host association not supplied"),
    }
}

fn rect_property(record: &mut Record, name: &'static str, rect: Rect, origin: &'static str) {
    record.property(
        name,
        format!("{}, {}, {}, {}", rect.x, rect.y, rect.w, rect.h),
        origin,
    );
}

pub(super) fn menu(
    root: Affine,
    bounds: Rect,
    anchor: Anchor,
    association: &KoruMenuInspection<'_>,
) -> Option<Scope> {
    if !inspect::enabled() {
        return None;
    }
    Some(inspect::scope(
        "KoruMenu",
        "component",
        root,
        scene::rect(bounds.x, bounds.y, bounds.w, bounds.h),
        |r| {
            identity(r, "occurrence", association.occurrence);
            identity(r, "logical owner row", association.owner_row);
            identity(r, "trigger occurrence", association.trigger);
            r.property(
                "component family",
                "C-OVERLAY",
                "docs/features/koruui/COMPONENTS.md",
            );
            r.property(
                "layout",
                "vertical rows; controls form a horizontal band",
                "Menu::item_rect / control_rect",
            );
            r.property(
                "anchor",
                format!("{anchor:?}"),
                "Menu::align; clamped by Menu::panel",
            );
            r.property(
                "recipe width range",
                format!("{MIN_W}–{MAX_W}"),
                "menu::MIN_W / MAX_W; measurement defaults, at_width may override",
            );
            r.property("actual width", bounds.w, "Menu::panel");
            r.property(
                "recipe row height",
                ITEM_H,
                "theme::ROW_H; MenuItem::height may override",
            );
            r.property(
                "recipe separator height",
                DIVIDER_H,
                "menu::DIVIDER_H; MenuItem::height may override",
            );
            r.property("recipe vertical padding", PAD, "theme::PANEL_PAD");
            r.property("recipe horizontal padding", ROW_PAD_X, "menu::ROW_PAD_X");
            r.property("radius", RADIUS, "menu::RADIUS");
            r.property(
                "viewport inset",
                EDGE_MARGIN,
                "menu::EDGE_MARGIN; not PopoverShell inset",
            );
            r.property(
                "native semantics",
                "host-owned; this record is not a native accessibility node",
                "Menu draw evidence",
            );
            r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
        },
    ))
}

pub(super) fn row<T: MenuItem>(
    root: Affine,
    bounds: Rect,
    item: &T,
    index: usize,
    enabled: bool,
    highlighted: bool,
    association: &KoruMenuInspection<'_>,
) -> Option<Scope> {
    if !inspect::enabled() {
        return None;
    }
    let (name, kind, category) = if item.divider() {
        ("Menu separator", "part", "separator")
    } else if !item.controls().is_empty() {
        ("Menu controls", "part", "control group")
    } else if !item.selectable() {
        ("Menu header", "part", "header")
    } else {
        (
            "KoruMenuItem",
            "component",
            if item.has_submenu() {
                "submenu"
            } else if item.checked().is_some() {
                "toggle"
            } else {
                "action"
            },
        )
    };
    Some(inspect::scope(
        name,
        kind,
        root,
        scene::rect(bounds.x, bounds.y, bounds.w, bounds.h),
        |r| {
            identity(
                r,
                "occurrence",
                association.row_occurrence.and_then(|id| id(index)),
            );
            identity(r, "menu occurrence", association.occurrence);
            if kind == "component" {
                r.property(
                    "component family",
                    "C-OVERLAY",
                    "docs/features/koruui/COMPONENTS.md; existing Menu family",
                );
            }
            r.property("row index", index, "immutable Menu item list");
            r.property(
                "category",
                category,
                "MenuItem divider / controls / selectable / checked / has_submenu",
            );
            r.property(
                "enabled",
                enabled,
                "host enabled slice; controls have independent enabled flags",
            );
            r.property("selectable", item.selectable(), "MenuItem::selectable");
            if category == "control group" && item.selectable() {
                r.property("contract mismatch", "controls row is also selectable", "MenuItem::controls requires nonselectable rows; existing input/paint preserved");
            }
            r.property(
                "highlighted",
                highlighted,
                "Menu::hover; shared pointer/keyboard index, not proof of native focus",
            );
            r.property(
                "hover paint visible",
                highlighted && enabled && !item.divider(),
                "existing Menu::draw condition",
            );
            r.property(
                "checked",
                match item.checked() {
                    Some(true) => "true",
                    Some(false) => "false",
                    None => "not a toggle",
                },
                "MenuItem::checked",
            );
            r.property("destructive", item.destructive(), "MenuItem::destructive");
            r.property("has submenu", item.has_submenu(), "MenuItem::has_submenu");
            rect_property(
                r,
                "row bounds (x y w h)",
                bounds,
                "Menu::item_rect; local logical units",
            );
            if item.selectable() {
                rect_property(
                    r,
                    "hit bounds (x y w h)",
                    bounds,
                    "Menu::item_rect / item_at; disabled refusal is separate",
                );
            }
            rect_property(
                r,
                "hover paint bounds (x y w h)",
                Rect::new(bounds.x + 4.0, bounds.y, bounds.w - 8.0, bounds.h),
                "Menu::draw inset shape; local logical units, not hit target",
            );
            r.property("hover paint radius", 5, "Menu::draw current recipe");
        },
    ))
}

/// A card row's panel: its texts as properties, so the inspector reads what
/// the card says without a node per line.
pub(super) fn card(
    root: Affine,
    bounds: Rect,
    card: &super::MenuCard,
    index: usize,
    highlighted: bool,
) -> Option<Scope> {
    if !inspect::enabled() {
        return None;
    }
    Some(inspect::scope(
        "Menu card",
        "part",
        root,
        scene::rect(bounds.x, bounds.y, bounds.w, bounds.h),
        |r| {
            r.property("index", index, "row index");
            r.property("badge", card.badge.as_str(), "the chip on the first line");
            r.property(
                "date",
                card.date.as_str(),
                "beside the chip; empty when unknown",
            );
            r.property("title", card.title.as_str(), "wrapped");
            r.property("body", card.body.as_str(), "wrapped, dimmed");
            r.property(
                "button",
                card.button
                    .as_ref()
                    .map_or(String::from("none"), |(l, on)| {
                        format!("{l} ({})", if *on { "enabled" } else { "dimmed" })
                    }),
                "at the card's foot",
            );
            r.property("highlighted", highlighted, "the pointer's row");
        },
    ))
}

pub(super) fn control(
    root: Affine,
    bounds: Rect,
    control: &Control,
    index: usize,
    highlighted: bool,
) -> Option<Scope> {
    if !inspect::enabled() {
        return None;
    }
    let name = match control {
        Control::Button { .. } => "Menu control button",
        Control::Readout(_) => "Menu readout",
    };
    Some(inspect::scope(
        name,
        "part",
        root,
        scene::rect(bounds.x, bounds.y, bounds.w, bounds.h),
        |r| {
            r.property("control index", index, "MenuItem::controls");
            r.property(
                "pressable",
                control.pressable(),
                "Control::pressable; readouts never activate",
            );
            if let Control::Button { enabled, .. } = control {
                r.property("enabled", enabled, "Control::Button::enabled");
                r.property(
                    "highlighted",
                    highlighted,
                    "Menu::control_hover; not native focus",
                );
                r.property(
                    "hover paint visible",
                    highlighted && *enabled,
                    "Menu::draw_controls",
                );
                rect_property(
                    r,
                    "hover paint bounds (x y w h)",
                    Rect::new(bounds.x, bounds.y + 2.0, bounds.w, bounds.h - 4.0),
                    "Menu::draw_controls; local logical units, not hit target",
                );
            }
            rect_property(
                r,
                "hit bounds (x y w h)",
                bounds,
                "Menu::control_rect / control_at; readout found but not pressable",
            );
        },
    ))
}

pub(super) fn disabled_hint(
    root: Affine,
    bounds: Rect,
    index: usize,
    association: &KoruMenuInspection<'_>,
) -> Option<Scope> {
    if !inspect::enabled() {
        return None;
    }
    Some(inspect::scope(
        "Menu disabled explanation",
        "part",
        root,
        scene::rect(bounds.x, bounds.y, bounds.w, bounds.h),
        |r| {
            identity(
                r,
                "logical owner row",
                association.row_occurrence.and_then(|id| id(index)),
            );
            identity(r, "menu occurrence", association.occurrence);
            r.property(
                "owner row index",
                index,
                "immutable Menu item list; not an occurrence without host association",
            );
            r.property(
                "interaction",
                "noninteractive",
                "Menu::draw_disabled_hint; no tooltip hover delay",
            );
            r.property("radius", 6, "Menu::draw_disabled_hint current recipe");
            r.property("padding", 7, "Menu::draw_disabled_hint current recipe");
        },
    ))
}

pub(super) fn swatch(root: Affine, bounds: Rect, token: Option<&str>) -> Option<Scope> {
    if !inspect::enabled() {
        return None;
    }
    Some(inspect::scope(
        "Menu swatch",
        "part",
        root,
        scene::rect(bounds.x, bounds.y, bounds.w, bounds.h),
        |r| {
            // Only a token accepted by the existing palette is public style data.
            // An unknown host string is never copied into diagnostics.
            r.property(
                "color token",
                token
                    .filter(|token| theme::accent_tone(token).is_some())
                    .unwrap_or("neutral fallback"),
                "MenuItem::swatch resolved by theme::accent_tone",
            );
        },
    ))
}

#[cfg(test)]
mod tests;

pub(super) fn brand(
    root: Affine,
    bounds: Rect,
    brand: &koru_draw::brand::BrandIcon,
) -> Option<Scope> {
    if !inspect::enabled() {
        return None;
    }
    Some(inspect::scope(
        "Menu brand icon",
        "part",
        root,
        scene::rect(bounds.x, bounds.y, bounds.w, bounds.h),
        |r| {
            r.property(
                "brand",
                brand.name,
                "MenuItem::brand_icon; shared static artwork, not Phosphor",
            );
        },
    ))
}
