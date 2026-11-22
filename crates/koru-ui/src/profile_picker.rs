// SPDX-License-Identifier: MPL-2.0

//! The profile rows shared by Koru's pane and the global Search footer.
//!
//! Identity and selection belong to the caller. This widget owns the visual
//! contract: heading, compact rows, initials chips, hover, active weight and
//! an optional trailing row menu.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Fill, ImageBrush};

pub const HEADING_H: f32 = 20.0;
pub const ROW_H: f32 = theme::ROW_H;
pub const PAD: f32 = theme::PANEL_PAD;
/// The profile switcher's outer cap in every host.
pub const MAX_H: f32 = 300.0;
const AVATAR_SIZE: f32 = 20.0;
pub const TRAILING_W: f32 = 20.0;

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ProfileRowParts {
    pub chip: Rect,
    pub name: Rect,
    pub trailing: Rect,
}

/// The same horizontal lanes used by workspace rows: 8 px inset, a 20 px
/// identity tile, an 8 px gap, and a reserved 20 px trailing action.
pub fn row_parts(rect: Rect) -> ProfileRowParts {
    let chip = Rect::new(
        rect.x + theme::NAV_PAD,
        rect.y + (rect.h - AVATAR_SIZE) * 0.5,
        AVATAR_SIZE,
        AVATAR_SIZE,
    );
    let trailing = Rect::new(
        (rect.x + rect.w - TRAILING_W - theme::NAV_PAD).max(rect.x),
        rect.y,
        TRAILING_W,
        rect.h,
    );
    let name_x = chip.x + chip.w + 8.0;
    ProfileRowParts {
        chip,
        name: Rect::new(name_x, rect.y, (trailing.x - name_x - 4.0).max(1.0), rect.h),
        trailing,
    }
}

pub struct ProfileRow<'a> {
    pub name: &'a str,
    pub detail: &'a str,
    pub palette_key: &'a str,
    pub selected: bool,
    pub removable: bool,
    /// Draw a trailing ellipsis while this row is hot.
    pub menu: bool,
    pub avatar: Option<&'a ImageBrush>,
    pub icon: Option<&'a str>,
    pub color: Option<&'a str>,
}

pub fn content_height(rows: usize) -> f32 {
    PAD * 2.0 + HEADING_H + rows as f32 * ROW_H
}

pub fn row_rect(panel: Rect, index: usize, scroll: f32) -> Rect {
    Rect::new(
        panel.x,
        panel.y + PAD + HEADING_H + index as f32 * ROW_H - scroll,
        panel.w,
        ROW_H,
    )
}

pub fn row_at(panel: Rect, x: f32, y: f32, rows: usize, scroll: f32) -> Option<usize> {
    if !panel.contains(x, y) {
        return None;
    }
    let offset = y - panel.y - PAD - HEADING_H + scroll;
    if offset < 0.0 {
        return None;
    }
    let index = (offset / ROW_H).floor() as usize;
    (index < rows).then_some(index)
}

pub fn draw_heading(canvas: &mut Scene, root: Affine, engine: &mut TextEngine, panel: Rect) {
    let p = theme::p();
    let layout = engine.simple_layout("PROFILES", theme::role::hint());
    scene::draw_layout_first_line(
        canvas,
        root,
        &layout,
        panel.x + theme::NAV_PAD * 2.0,
        panel.y + PAD + 2.0,
        Some(p.row_hint),
    );
}

pub fn draw_row(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    rect: Rect,
    row: ProfileRow<'_>,
    hot: bool,
) {
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return;
    }
    let p = theme::p();
    if hot {
        scene::fill(
            canvas,
            root,
            p.hover_tint,
            &scene::rrect(rect.x + 4.0, rect.y, (rect.w - 8.0).max(0.0), rect.h, 5.0),
        );
    }

    let parts = row_parts(rect);
    let chip = parts.chip;
    if row.avatar.is_some() && row.icon.is_none() {
        draw_avatar(
            canvas,
            root,
            engine,
            chip,
            row.avatar,
            row.name,
            row.palette_key,
            theme::role::meta(),
        );
    } else {
        koru_draw::tile::Tile {
            icon: row.icon,
            color: row.color,
            name: row.name,
        }
        .draw(canvas, root, engine, chip);
    }

    let name = label(row.name);
    let name_style = if row.selected {
        theme::role::row().with_weight(theme::FontWeight::SemiBold)
    } else {
        theme::role::row()
    };
    let name = engine.elided(&name, name_style, parts.name.w);
    let name = engine.simple_layout(&name, name_style);
    let colour = if row.selected { p.text } else { p.text_dim };
    scene::draw_layout_first_line(
        canvas,
        root,
        &name,
        parts.name.x,
        scene::centre_y_in(&name, rect),
        Some(colour),
    );
    let icon_rect = parts.trailing;
    if hot && row.menu {
        scene::dots_three(canvas, root, icon_rect, p.text_dim);
    } else if hot && row.removable {
        draw_icon(
            canvas,
            root,
            engine,
            icons::WINDOW_CLOSE,
            icon_rect,
            p.text_dim,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw_avatar(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    at: Rect,
    avatar: Option<&ImageBrush>,
    name: &str,
    _palette_key: &str,
    style: theme::TextStyle,
) {
    draw_identity(
        canvas,
        root,
        engine,
        at,
        ProfileIdentity {
            avatar,
            name,
            icon: None,
            color: None,
        },
        style,
        theme::p(),
    );
}

/// Circular identity shared by the profile rail and profile menu. Workspace
/// tiles remain square. An explicit icon takes precedence over a photo.
pub struct ProfileIdentity<'a> {
    pub avatar: Option<&'a ImageBrush>,
    pub name: &'a str,
    pub icon: Option<&'a str>,
    pub color: Option<&'a str>,
}

#[allow(clippy::too_many_arguments)]
pub fn draw_identity(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    at: Rect,
    identity: ProfileIdentity<'_>,
    style: theme::TextStyle,
    p: &theme::Palette,
) {
    if at.w <= 0.0 || at.h <= 0.0 {
        return;
    }
    let (cx, cy) = at.centre();
    let chip = scene::rrect(at.x, at.y, at.w, at.h, at.w.min(at.h) * 0.5);
    if let Some(brush) = identity.avatar.filter(|_| identity.icon.is_none()) {
        canvas.push_clip_layer(Fill::NonZero, root, &chip);
        let img = &brush.image;
        let scale = (at.w / img.width.max(1) as f32).max(at.h / img.height.max(1) as f32);
        canvas.draw_image(
            brush,
            root * Affine::translate((
                (cx - img.width as f32 * scale * 0.5) as f64,
                (cy - img.height as f32 * scale * 0.5) as f64,
            )) * Affine::scale(scale as f64),
        );
        canvas.pop_layer();
        return;
    }
    // Match the profile editor's tile: unset means the panel surface, not
    // an automatically assigned identity color. Unknown saved colors use
    // the neutral chip surface, just as workspace/profile tiles do.
    let background = match identity.color {
        None => p.panel_bg,
        Some(name) => p
            .workspace_colors
            .iter()
            .find(|(key, _)| *key == name)
            .map_or(p.chip_bg, |(_, color)| *color),
    };
    scene::fill(canvas, root, background, &chip);
    if identity.color.is_none() {
        scene::stroke(canvas, root, p.panel_border, theme::BORDER_W, &chip);
    }
    let letters = initials(identity.name);
    let layout = engine.simple_layout(identity.icon.unwrap_or(&letters), style);
    let (x, y) = scene::centre_in(&layout, at);
    scene::draw_layout(
        canvas,
        root,
        &layout,
        x,
        y,
        identity.icon.is_none().then_some(p.text),
    );
}

fn draw_icon(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    icon: char,
    at: Rect,
    colour: vello::peniko::Color,
) {
    let layout = engine.icon_layout(icon, 14.0);
    scene::draw_layout_first_line(
        canvas,
        root,
        &layout,
        at.x + (at.w - layout.width()) * 0.5,
        scene::centre_y_in(&layout, at),
        Some(colour),
    );
}

fn label(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "Untitled".into()
    } else {
        trimmed.into()
    }
}

pub fn initials(name: &str) -> String {
    let letters = name
        .split_whitespace()
        .filter_map(|word| word.chars().find(|character| character.is_alphanumeric()))
        .take(2)
        .collect::<String>()
        .to_uppercase();
    if letters.is_empty() {
        "?".into()
    } else {
        letters
    }
}

pub fn chip_slot(key: &str) -> usize {
    let key = key.trim().to_lowercase();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash % 4) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_is_not_a_profile_row() {
        let panel = Rect::new(8.0, 8.0, 258.0, content_height(2));
        assert_eq!(row_at(panel, 20.0, panel.y + PAD + 2.0, 2, 0.0), None);
        let first = row_rect(panel, 0, 0.0);
        assert_eq!(row_at(panel, first.x + 2.0, first.y + 2.0, 2, 0.0), Some(0));
    }

    #[test]
    fn initials_and_palette_are_stable() {
        assert_eq!(initials("Kim Alvarez"), "KA");
        assert_eq!(initials(""), "?");
        assert_eq!(chip_slot(" Personal "), chip_slot("personal"));
        assert!(chip_slot("personal") < 4);
    }
}
