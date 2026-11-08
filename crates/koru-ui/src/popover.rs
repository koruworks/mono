// SPDX-License-Identifier: MPL-2.0

//! A generic anchored floating surface.

use koru_draw::geometry::Rect;
use koru_draw::{scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

mod inspection;
pub use inspection::{KoruPopoverInspection, KoruPopoverPlacement, KoruPopoverState};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PopoverAlign {
    Start,
    End,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PopoverShell {
    pub rect: Rect,
    pub radius: f32,
}

impl PopoverShell {
    pub fn anchored(
        anchor: Rect,
        viewport: Rect,
        width: f32,
        height: f32,
        align: PopoverAlign,
    ) -> Self {
        let recipe = theme::overlay::popover();
        let edge = recipe.viewport_inset;
        let mut x = match align {
            PopoverAlign::Start => anchor.x,
            PopoverAlign::End => anchor.x + anchor.w - width,
        };
        x = x.clamp(
            viewport.x + edge,
            (viewport.x + viewport.w - width - edge).max(edge),
        );
        let below = anchor.y + anchor.h + recipe.anchor_gap;
        let above = anchor.y - recipe.anchor_gap - height;
        let mut y = if below + height <= viewport.y + viewport.h - edge {
            below
        } else {
            above
        };
        y = y.clamp(
            viewport.y + edge,
            (viewport.y + viewport.h - height - edge).max(edge),
        );
        Self {
            rect: Rect::new(x, y, width, height),
            radius: recipe.radius,
        }
    }

    #[track_caller]
    pub fn draw(&self, canvas: &mut Scene, root: Affine) {
        // This scope covers shell paint only. Content ownership is a separate
        // migration; custom shell literals do not imply anchored placement.
        let _inspection = self.surface_scope(
            root,
            "Shell paint only; placement origin and content ownership not exposed",
            "PopoverShell::draw",
        );
        self.paint(canvas, root);
    }

    /// Paint this shell, then `content` while its *Popover surface* record
    /// is still open: everything drawn inside the panel nests under the
    /// surface in the inspector rather than lying beside it. The host keeps
    /// input, motion and clipping policy; nothing retains the callback.
    #[track_caller]
    pub fn draw_containing<R>(
        &self,
        canvas: &mut Scene,
        root: Affine,
        content: impl FnOnce(&mut Scene, Affine) -> R,
    ) -> R {
        let _inspection = self.surface_scope(
            root,
            "Shell paint and its content: what is drawn inside the panel nests under this record",
            "PopoverShell::draw_containing",
        );
        self.paint(canvas, root);
        content(canvas, root)
    }

    /// The *Popover surface* record, open until the guard drops.
    #[track_caller]
    fn surface_scope(
        &self,
        root: Affine,
        coverage: &'static str,
        origin: &'static str,
    ) -> koru_draw::inspect::Scope {
        koru_draw::inspect::scope(
            "Popover surface",
            "part",
            root,
            scene::rect(self.rect.x, self.rect.y, self.rect.w, self.rect.h),
            |record| {
                let recipe = theme::overlay::popover();
                record.property(
                    "radius",
                    self.radius,
                    "PopoverShell::radius; caller-owned field",
                );
                record.property("recipe viewport inset", recipe.viewport_inset, "theme::overlay::popover().viewport_inset; anchored default, not measured padding");
                record.property(
                    "recipe anchor gap",
                    recipe.anchor_gap,
                    "theme::overlay::popover().anchor_gap; anchored default, not measured spacing",
                );
                record.property("recipe radius", recipe.radius, "theme::overlay::popover().radius; anchored default, actual radius shown separately");
                record.property("coverage", coverage, origin);
            },
        )
    }

    /// Paint this shell and its content under one shared ownership boundary.
    /// The host keeps input, motion and clipping policy. The callback always
    /// runs, including when inspection is off, and nothing retains it.
    pub fn draw_with_content<R>(
        &self,
        canvas: &mut Scene,
        root: Affine,
        association: KoruPopoverInspection,
        content: impl FnOnce(&mut Scene, Affine) -> R,
    ) -> R {
        let _inspection = inspection::scope(self, root, association);
        self.paint(canvas, root);
        content(canvas, root)
    }

    fn paint(&self, canvas: &mut Scene, root: Affine) {
        scene::floating_panel(
            canvas,
            root,
            self.rect.x,
            self.rect.y,
            self.rect.w,
            self.rect.h,
            &scene::Panel::popover(self.radius).fill(theme::p().panel_bg),
        );
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.rect.contains(x, y)
    }
}

#[cfg(test)]
mod tests;
