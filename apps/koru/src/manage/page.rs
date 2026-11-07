// SPDX-License-Identifier: MPL-2.0

//! The Manage tab's pages: which there are, where the page area is, and the
//! head every page starts with.
//!
//! **Laid out by Settings' geometry** — the shared `koru_ui::PageShell`
//! (maintainers, 2026-10-07: *display Manage in the same style as Settings*). The nav
//! and the page's column are one centred group; narrower than
//! `PageShell::COMPACT_BELOW` the nav becomes chips above the page. A page is
//! Settings' page: a title in the title role, one line of lead under it, then
//! captioned cards.
//!
//! A Profile's page is [`super::profile_page`]: its header, what needs the
//! person, *Sign-in and sync* and *Delete profile…*. Linked devices is
//! [`super::devices`]; Koru Cloud is [`super::cloud`]. The New profile page
//! is the embedded create form ([`super::new_profile`]) under its own head.

use super::*;
use koru_draw::geometry::Rect;
use koru_ui::PageShell;
use koru_ui::button;
use koru_ui::text_paragraph::KoruTextParagraph;
use vello::peniko::Fill;

/// Settings' shell, with a nav wide enough for a Profile's tile, its name
/// and the marks after it — Settings' 148 px elides *Linked devices 11*.
pub(super) const SHELL: PageShell = PageShell {
    nav_w: 184.0,
    ..PageShell::SETTINGS
};
/// The Profile's tile before its name, as tall as the title's line, and the
/// gap between them.
pub(super) const TITLE_TILE: f32 = 32.0;
pub(super) const TITLE_TILE_GAP: f32 = 12.0;

/// Where Manage is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    /// One Profile on this device.
    Profile(ProfileId),
    /// The create form.
    NewProfile,
    /// This computer's linked devices.
    Devices,
    /// Koru Cloud and the account.
    Cloud,
}

/// A page's head: its title and the lead under it, unscrolled.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Head {
    /// Where the title wraps: its left, its top, its width and height.
    pub title: Rect,
    pub title_text: String,
    pub lead: Rect,
    pub lead_text: String,
}

impl Default for Head {
    fn default() -> Self {
        let none = Rect::new(0.0, 0.0, 0.0, 0.0);
        Self {
            title: none,
            title_text: String::new(),
            lead: none,
            lead_text: String::new(),
        }
    }
}

impl Head {
    /// Where the next thing on the page goes: the lead, then Settings' air.
    pub(crate) fn bottom(&self) -> f32 {
        self.lead.y + self.lead.h + PageShell::LEAD_GAP
    }
}

impl ManageView {
    /// Whether the compact chips stand in for the nav's column.
    pub(super) fn compact(&self) -> bool {
        SHELL.compact(self.viewport.0)
    }

    /// The nav's region: its column, or the compact band of chips.
    pub(super) fn sidebar(&self) -> Rect {
        SHELL.sidebar(self.viewport, self.band_h)
    }

    /// Whether a point in the tab is the nav's.
    pub(super) fn in_nav(&self, x: f32, y: f32) -> bool {
        self.sidebar().contains(x, y)
    }

    /// The page's region: right of the nav, or under the compact band.
    pub(super) fn page_area(&self) -> Rect {
        SHELL.page_area(self.viewport, self.band_h)
    }

    /// The page's column, unscrolled: the shared shell's.
    pub(super) fn column(&self) -> Rect {
        SHELL.column(self.viewport, self.band_h)
    }

    /// Measure a page's head at the column's top: `title` from `indent` in,
    /// leaving `reserved` at its right end for a button, and `lead` under it
    /// across the column.
    pub(super) fn lay_head(
        &mut self,
        column: Rect,
        indent: f32,
        reserved: f32,
        title: &str,
        lead: &str,
    ) -> Head {
        let title_w = (column.w - indent - reserved).max(1.0);
        let title_h = koru_ui::KoruTextTitle::new(title).measure(&mut self.engine, title_w);
        let title_rect = Rect::new(column.x + indent, column.y, title_w, title_h);
        let lead_y = column.y + title_h.max(button::H) + PageShell::TITLE_GAP;
        let lead_h = if lead.is_empty() {
            0.0
        } else {
            KoruTextParagraph::new(lead, theme::role::ui(), "", theme::p().text_dim)
                .measure(&mut self.engine, column.w)
        };
        Head {
            title: title_rect,
            title_text: title.to_owned(),
            lead: Rect::new(column.x, lead_y, column.w, lead_h),
            lead_text: lead.to_owned(),
        }
    }

    /// Paint a page's head as Settings paints its page title: the title role,
    /// then the lead in the dim body color.
    pub(super) fn paint_head(&mut self, canvas: &mut Scene, root: Affine, head: &Head) {
        let p = theme::p();
        let t = head.title;
        let bottom = head.lead.y + head.lead.h;
        let _head = inspect::scope(
            "Manage page title",
            "part",
            root,
            scene::rect(t.x, t.y, t.w, bottom - t.y),
            |r| {
                r.property("title", head.title_text.as_str(), "page content");
                r.property("title role", "theme::role::title()", "Settings' page title");
                r.property("geometry", "koru_ui::PageShell", "shared shell");
            },
        );
        koru_ui::KoruTextTitle::new(&head.title_text).draw(canvas, root, &mut self.engine, t);
        if !head.lead_text.is_empty() {
            KoruTextParagraph::new(
                &head.lead_text,
                theme::role::ui(),
                "theme::role::ui()",
                p.text_dim,
            )
            .draw(canvas, root, &mut self.engine, head.lead);
        }
    }

    /// A caption over a card, in the words and style Settings gives one:
    /// `theme::role::meta()` in the dim color, at the card's left edge. `at`
    /// is the caption's line; its text is centred on it.
    pub(super) fn paint_caption(&mut self, canvas: &mut Scene, root: Affine, at: Rect, text: &str) {
        let p = theme::p();
        let style = theme::role::meta();
        let line = style.metrics.line_height;
        let _caption = inspect::scope(
            "Manage caption",
            "part",
            root,
            scene::rect(at.x, at.y, at.w, at.h),
            |r| r.property("title", text, "page content"),
        );
        koru_ui::text_paragraph::draw_line(
            canvas,
            root,
            &mut self.engine,
            text,
            at.x,
            at.y + (at.h - line) * 0.5,
            style,
            "theme::role::meta()",
            p.text_dim,
        );
    }

    /// The page heading's text — what the accessibility tree calls the page.
    pub(super) fn page_title(&self) -> String {
        match self.page {
            Some(Page::Profile(profile)) => self
                .c
                .profiles()
                .iter()
                .find(|row| row.id == profile)
                .map_or_else(|| "Profile".to_owned(), |row| row.name.clone()),
            Some(Page::NewProfile) => new_profile::TITLE.to_owned(),
            Some(Page::Devices) => self.devices_title(),
            Some(Page::Cloud) => {
                koru_i18n::t(crate::format::lang(), koru_i18n::Key::CloudTitle).to_owned()
            }
            None => "Manage".to_owned(),
        }
    }

    /// Where the heading is drawn, scrolled with the page — for the
    /// accessibility tree. One title line tall: the tree wants a place to
    /// point at, not a second measurement of the wrap.
    pub(super) fn heading_rect(&self) -> Rect {
        let column = self.column();
        let line = theme::role::title().metrics.line_height;
        let y = column.y - self.page_scroll;
        match self.page {
            Some(Page::Profile(_)) => {
                let name = self.laid.header.name;
                Rect::new(name.x, y, self.laid.header.text_w.max(1.0), line)
            }
            Some(Page::NewProfile) => Rect::new(column.x, y, column.w, line),
            None => self.page_area(),
            Some(Page::Devices | Page::Cloud) => Rect::new(column.x, y, column.w, line),
        }
    }

    /// How tall the page's content is, for its scroll: from the tab's top to
    /// below the last thing, with Settings' air under it.
    pub(super) fn page_content_height(&self) -> f32 {
        match self.page {
            Some(Page::Profile(_) | Page::Devices | Page::Cloud) => {
                self.column().y + self.laid.height + PageShell::BOTTOM_PAD
            }
            Some(Page::NewProfile) => {
                let form = self.form_rect();
                form.y + form.h + PageShell::BOTTOM_PAD
            }
            _ => 0.0,
        }
    }

    pub(super) fn paint_page(&mut self, canvas: &mut Scene, root: Affine) {
        let area = self.page_area();
        let bounds = scene::rect(area.x, area.y, area.w, area.h);
        canvas.push_clip_layer(Fill::NonZero, root, &bounds);
        let _clip = inspect::clip(root, bounds);
        let scrolled = root * Affine::translate((0.0, -(self.page_scroll as f64)));
        match self.page {
            Some(Page::Profile(_)) => self.paint_profile_page(canvas, scrolled),
            Some(Page::NewProfile) => self.paint_new_profile(canvas, scrolled),
            Some(Page::Devices) => self.paint_devices_page(canvas, scrolled),
            Some(Page::Cloud) => self.paint_cloud_page(canvas, scrolled),
            None => {}
        }
        // The account switcher replaces the page while it is open, as it
        // replaced Manage Profiles' pane.
        if self.switcher.open {
            let rows = self.c.account().held_accounts.len() + 2;
            let _switcher = inspect::scope("Manage account switcher", "part", root, bounds, |r| {
                r.property("rows", rows, "AccountSwitcher")
            });
            let at = root * Affine::translate((f64::from(area.x), f64::from(area.y)));
            self.switcher.draw(
                canvas,
                at,
                &mut self.engine,
                self.c.account(),
                crate::format::lang(),
                (area.w, area.h),
            );
        }
        canvas.pop_layer();
    }
}
