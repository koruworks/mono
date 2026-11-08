// SPDX-License-Identifier: MPL-2.0

//! The New profile page: a Settings page head, and the embedded create form
//! under it.
//!
//! **Today's form, unchanged** (maintainers' decision 17). It is the onboarding form
//! the first-run window shows; there it takes the whole window, here it
//! takes the page area under the page's title and lead. The form lays out at
//! its own origin, so the one thing this page adds is the offset: input is
//! shifted into the form's space and drawing is translated out of it, each in
//! exactly one place ([`ManageView::form_rect`]).
//!
//! The form lives in the controller, not on this page, so leaving the page
//! does not lose what was typed or a sign-in in flight; coming back finds it
//! as it was. When the form finishes — a Profile made, chosen or connected —
//! the controller closes it and the tab moves to that Profile's page
//! (`ManageView::settle_page`).

use super::profile_page::{Control, Look, Placed};
use super::*;
use koru_draw::geometry::Rect;
use koru_ui::button;

pub(super) const TITLE: &str = "New profile";
const LEAD: &str = "Start a profile on this computer, or bring one in from your Koru account.";
/// The lead without the account half, in a build that hides Koru Cloud
/// (koru-features): the embedded form then starts on the local Profile step.
const LEAD_LOCAL: &str = "Start a profile on this computer.";
/// Between the title and the X at its right end.
const CLOSE_GAP: f32 = 12.0;

impl ManageView {
    /// The page's head, at the column's top — laid with the rest of the
    /// page, so the form below it knows where it starts.
    pub(super) fn lay_new_profile_page(&mut self) -> profile_page::Laid {
        let column = self.column();
        let lead = if koru_features::cloud_sync() {
            LEAD
        } else {
            LEAD_LOCAL
        };
        // The X at the title line's right end (the review's note), the place
        // Linked devices keeps its own head button.
        let head = self.lay_head(column, 0.0, button::H + CLOSE_GAP, TITLE, lead);
        let line = theme::role::title().metrics.line_height;
        let close = Placed {
            control: Control::CloseCreation,
            rect: Rect::new(
                column.x + column.w - button::H,
                column.y + ((line - button::H) * 0.5).max(0.0),
                button::H,
                button::H,
            ),
            label: "Close".into(),
            look: Look::Close,
        };
        profile_page::Laid {
            head: Some(head),
            controls: vec![close],
            ..Default::default()
        }
    }

    /// The page's X (the review's note): the form is put away, and the tab
    /// lands on the first Profile in the nav. What was typed goes with the
    /// form, as closing a creator anywhere does.
    pub(super) fn close_creation(&mut self) -> bool {
        self.c.cancel_profile_creation();
        match self.c.profiles().first().map(|row| row.id) {
            Some(first) => self.show(Page::Profile(first)),
            None => self.sync(),
        }
        true
    }

    /// Where the form goes: under the head, in the head's column, so the
    /// form's left edge is the title's.
    pub(super) fn form_rect(&self) -> Rect {
        let area = self.page_area();
        let column = self.column();
        let top = self.laid.head.as_ref().map_or(column.y, |head| {
            head.lead.y + head.lead.h + koru_ui::PageShell::TITLE_GAP
        });
        Rect::new(column.x, top, column.w, (area.y + area.h - top).max(0.0))
    }

    /// A point in the tab, in the form's space.
    pub(super) fn form_point(&self, x: f32, y: f32) -> (f32, f32) {
        let form = self.form_rect();
        (x - form.x, y - form.y)
    }

    /// Give the form its rect's size. Asked whenever the tab's size changes
    /// and when the controller has just opened the form.
    pub(super) fn size_creation_form(&mut self) {
        let form = self.form_rect();
        if let Some(form_view) = self.c.creation_form_mut() {
            form_view.set_viewport(form.w, form.h);
        }
    }

    pub(super) fn paint_new_profile(&mut self, canvas: &mut Scene, root: Affine) {
        let area = self.page_area();
        let _page = inspect::scope(
            "Manage new profile",
            "part",
            root,
            scene::rect(area.x, area.y, area.w, area.h),
            |r| {
                r.property(
                    "form",
                    "koru_workspace_host::onboarding::OnboardingView",
                    "embedded, unchanged",
                );
            },
        );
        if let Some(head) = self.laid.head.clone() {
            self.paint_head(canvas, root, &head);
        }
        if let Some(close) = self.laid.placed(Control::CloseCreation).cloned() {
            self.paint_control(canvas, root, &close);
        }
        let form = self.form_rect();
        let at = root * Affine::translate((form.x as f64, form.y as f64));
        if let Some(form_view) = self.c.creation_form_mut() {
            form_view.draw(canvas, at);
        }
    }
}
