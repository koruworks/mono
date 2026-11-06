// SPDX-License-Identifier: MPL-2.0

//! What the Trash tab puts on screen.
//!
//! Separate from the geometry for the reason the manager splits them: none of it
//! decides anything, and every rectangle it draws into comes from a function in
//! `view.rs`, so a control and its hit test cannot drift.

use super::view::{
    CONTROL_GAP, DOTS_W, Hit, LANE_W, PAD, RESTORE_W, SUBTITLE_TEXT, TITLE_GAP, TITLE_TEXT,
    TrashView,
};
use crate::format::{deleted_ago, nested_suffix};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use koru_ui::table::RowState;
use vello::Scene;
use vello::kurbo::Affine;

/// The page heading. **Medium, not semibold** — the size already says it is the
/// title, and weight on top of that is a second answer to a question the type
/// scale has answered.
pub(super) const TITLE: theme::TextStyle =
    theme::TextStyle::new(theme::text_metrics::LG, theme::FontWeight::Medium);

impl TrashView {
    pub(super) fn paint(&mut self, scene: &mut Scene, root: Affine) {
        let p = theme::p();
        // `bg`, not `canvas`: this is a **sheet** you read a list on, the same
        // surface the workspace manager uses. `canvas` is the grey *around* a
        // page in Page mode, and using it here made the Trash look like a
        // document view with nothing in it.
        scene::fill(
            scene,
            root,
            p.bg,
            &scene::rect(0.0, 0.0, self.viewport.0, self.viewport.1),
        );
        self.paint_header(scene, root);
        let (t, cols) = (self.table(), self.columns());
        t.paint_columns(scene, root, &mut self.engine, &cols);
        self.paint_rows(scene, root);
        if self.entries.is_empty() {
            self.paint_empty(scene, root);
        }
    }

    fn paint_header(&mut self, scene: &mut Scene, root: Affine) {
        let p = theme::p();
        // `title_top`, not `PAD`: a layout reserves half its leading above the
        // glyphs, so drawing at `PAD` puts more air on top than down the side.
        let top = self.title_top();
        let title = self.engine.simple_layout(TITLE_TEXT, TITLE);
        scene::draw_layout_first_line(scene, root, &title, PAD, top, Some(p.text));
        let sub = self
            .engine
            .simple_layout(SUBTITLE_TEXT, theme::role::meta());
        scene::draw_layout_first_line(
            scene,
            root,
            &sub,
            PAD,
            top + title.height() + TITLE_GAP,
            Some(p.text_dim),
        );
        // The icon button every `…` in the app is, as the Tasks page's.
        let dots = self.header_dots();
        koru_ui::icon_button::draw_with(
            scene,
            root,
            &mut self.engine,
            dots,
            koru_draw::icons::DOTS_THREE,
            koru_ui::icon_button::State {
                hovered: self.hover == Some(Hit::HeaderDots),
                active: false,
                disabled: false,
            },
            koru_ui::icon_button::Metrics::INLINE,
        );
    }

    /// The one thing the table cannot say: that there is nothing to say.
    fn paint_empty(&mut self, scene: &mut Scene, root: Affine) {
        let p = theme::p();
        let l = self
            .engine
            .simple_layout("The Trash is empty.", theme::role::row());
        scene::draw_layout_first_line(
            scene,
            root,
            &l,
            PAD,
            self.table().top() + theme::role::row().metrics.size * 2.0,
            Some(p.text_dim),
        );
    }

    fn paint_rows(&mut self, scene: &mut Scene, root: Affine) {
        let p = theme::p();
        let t = self.table();
        let bands = self.bands();
        let cols = self.columns();
        let now = self.now_ms;

        for i in t.visible(&bands) {
            let Some(entry) = self.entries.get(i).cloned() else {
                continue;
            };
            let r = t.row_rect(&bands, i);
            // The whole row counts as hovered when any of its controls is, or
            // the fill would drop away the moment the pointer reached Restore.
            let on_row = matches!(
                self.hover,
                Some(Hit::Row(j) | Hit::Restore(j) | Hit::Dots(j)) if j == i
            );
            let state = if self.selection.contains(&entry.node) {
                RowState::Selected
            } else if on_row {
                RowState::Hovered
            } else {
                RowState::Idle
            };
            t.paint_row_fill(scene, root, r, state);

            // ---- name: icon, then the title, then a dim count of what is
            // inside it. The suffix's width comes off the cell *before* the
            // name is elided, so the two cannot overlap.
            let cell = t.cell(&cols, LANE_W, 0, r);
            let icon = Rect::new(cell.x, r.y, koru_shell_chrome::PAGE_ICON_SLOT, r.h);
            // A page that starts with an emoji wears it; everything else wears
            // the document glyph. The same rule, and the same drawing call, the
            // explore column and the tab strip use.
            if entry.is_file {
                draw_icon(
                    scene,
                    root,
                    &mut self.engine,
                    koru_ui::file_icon::FileIcon::new(entry.mime.as_deref(), Some(&entry.name))
                        .glyph(),
                    icon,
                    p.text_dim,
                );
            } else {
                match &entry.icon {
                    Some(emoji) => koru_shell_chrome::draw_page_emoji(
                        scene,
                        root,
                        &mut self.engine,
                        emoji,
                        icon.x,
                        r,
                        None,
                    ),
                    None => draw_icon(
                        scene,
                        root,
                        &mut self.engine,
                        icons::DOCUMENT,
                        icon,
                        p.text_dim,
                    ),
                }
            }
            let suffix = nested_suffix(entry.nested).map(|s| format!(" \u{00B7} {s}"));
            let suffix_w = suffix
                .as_ref()
                .map(|s| self.engine.simple_layout(s, theme::role::meta()).width())
                .unwrap_or(0.0);
            let name_cell = Rect::new(
                cell.x + koru_shell_chrome::PAGE_ICON_SLOT + 4.0,
                cell.y,
                (cell.w - koru_shell_chrome::PAGE_ICON_SLOT - 4.0 - suffix_w).max(0.0),
                cell.h,
            );
            let drawn = t.paint_cell(
                scene,
                root,
                &mut self.engine,
                name_cell,
                &entry.name,
                theme::role::row(),
                p.text,
            );
            if let Some(s) = suffix {
                let after = Rect::new(name_cell.x + drawn, cell.y, suffix_w, cell.h);
                t.paint_cell(
                    scene,
                    root,
                    &mut self.engine,
                    after,
                    &s,
                    theme::role::meta(),
                    p.text_dim,
                );
            }

            for (c, text) in [
                (1, entry.origin.clone()),
                (2, deleted_ago(entry.trashed_at_ms, now)),
                (3, entry.deleted_by.clone()),
            ] {
                t.paint_cell(
                    scene,
                    root,
                    &mut self.engine,
                    t.cell(&cols, LANE_W, c, r),
                    &text,
                    theme::role::meta(),
                    p.text_dim,
                );
            }

            // ---- the controls, drawn only on the hovered row. Their **lane is
            // reserved on every row**, which is what stops the cells above from
            // reflowing as the pointer travels.
            if on_row {
                let c = t.trailing_rects(r, LANE_W, &[RESTORE_W, DOTS_W], CONTROL_GAP);
                t.paint_trailing_button(
                    scene,
                    root,
                    &mut self.engine,
                    c[0],
                    "Restore",
                    self.hover == Some(Hit::Restore(i)),
                    false,
                );
                t.paint_trailing_dots(
                    scene,
                    root,
                    &mut self.engine,
                    c[1],
                    self.hover == Some(Hit::Dots(i)),
                );
            }
        }
    }
}

fn draw_icon(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    glyph: char,
    rect: Rect,
    colour: vello::peniko::Color,
) {
    let layout = engine.icon_layout(glyph, theme::CAPTION_ICON);
    scene::draw_layout(
        scene,
        root,
        &layout,
        rect.x + (rect.w - layout.width()) * 0.5,
        rect.y + (rect.h - layout.height()) * 0.5,
        Some(colour),
    );
}
