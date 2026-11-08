// SPDX-License-Identifier: MPL-2.0

//! The in-app search overlay owns its frame, clipping and inspection subtree.

use koru_draw::{geometry::Rect, inspect, scene, theme};
use vello::{Scene, kurbo::Affine, peniko::Fill};

use crate::{SURFACE_RADIUS, SearchSurface};

/// Shared search modal geometry and paint; the host routes input and dismissal.
#[derive(Clone, Copy, Debug)]
pub struct SearchModal {
    viewport: (f32, f32),
    panel: Rect,
}

impl SearchModal {
    pub fn new(viewport: (f32, f32)) -> Self {
        let w = 640.0_f32.min((viewport.0 - 32.0).max(1.0));
        let h = 400.0_f32.min((viewport.1 - 32.0).max(1.0));
        let y = 96.0_f32.min((viewport.1 - h - 16.0).max(16.0));
        Self {
            viewport,
            panel: Rect::new((viewport.0 - w) * 0.5, y, w, h),
        }
    }

    pub const fn bounds(&self) -> Rect {
        self.panel
    }

    pub fn draw(
        &self,
        search: &mut SearchSurface,
        canvas: &mut Scene,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        let panel = self.panel;
        if search.viewport != (panel.w, panel.h) {
            search.set_viewport(panel.w, panel.h);
        }
        let _modal = inspect::scope(
            "SearchModal",
            "component",
            root,
            scene::rect(panel.x, panel.y, panel.w, panel.h),
            |record| {
                record.property(
                    "component ID",
                    "C-SEARCH-MODAL",
                    "koru-search-ui::SearchModal",
                );
                record.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
                record.property("radius", SURFACE_RADIUS, "shared search surface recipe");
                record.property("state", "open", "host renders while search is open");
                record.property(
                    "input and dismissal",
                    "host-owned",
                    "MainView search routing",
                );
            },
        );
        {
            let viewport = scene::rect(0.0, 0.0, self.viewport.0, self.viewport.1);
            let _scrim = inspect::scope("Search scrim", "part", root, viewport, |_| {});
            scene::fill(canvas, root, theme::p().scrim, &viewport);
        }
        scene::floating_panel(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(SURFACE_RADIUS),
        );
        let local_root = root * Affine::translate((f64::from(panel.x), f64::from(panel.y)));
        canvas.push_clip_layer(
            Fill::NonZero,
            local_root,
            &scene::rrect(0.0, 0.0, panel.w, panel.h, SURFACE_RADIUS),
        );
        search.draw(
            canvas,
            local_root,
            caret_on,
            (pointer.0 - panel.x, pointer.1 - panel.y),
        );
        canvas.pop_layer();
        scene::stroke(
            canvas,
            root,
            theme::p().panel_border,
            theme::BORDER_W,
            &scene::rrect(panel.x, panel.y, panel.w, panel.h, SURFACE_RADIUS),
        );
    }
}

#[cfg(test)]
mod tests;
