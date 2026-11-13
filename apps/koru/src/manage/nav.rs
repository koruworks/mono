// SPDX-License-Identifier: MPL-2.0

//! The Manage tab's nav: this device's Profiles, *New profile*, and what
//! belongs to this computer.
//!
//! **The account's Profiles that are not on this computer are not here**
//! (maintainer, 2026-11-11). They used to sit under *Other Profiles* with a
//! *Connect* beside each, which read as Profiles already on the device. They
//! are rows of *Which profiles sync* on the Koru Cloud page now, each with a
//! switch that brings it here (`manage::cloud`).
//!
//! ```text
//! Profiles                    caption, as Settings captions a card
//! ┃[tile] Personal      •     current Profile: an accent dot (decision 19)
//! │[tile] Work          ⚠     needs attention: a warning mark
//!  2 Profiles hidden          a quiet line (decision 16)
//! │[+]   New profile          the dashed add square
//! This computer               caption
//! │Linked devices        3
//! │Koru Cloud       •   On
//! ```
//!
//! **Settings' nav** (maintainers, 2026-10-07): no panel and no edge — the rows sit on
//! the page's own background, each on the shared `koru_ui::NavList`'s 2 px
//! rule, the current page's in the accent. Where the column goes is the
//! shared `koru_ui::PageShell`; narrower than its compact width the nav is
//! `ChipNav` chips across the top instead — the Profiles, *New profile*,
//! *Linked devices* and *Koru Cloud*.
//!
//! **One function places all of it** — [`layout`], or the chips the last
//! relayout made — and drawing, the hit test, keyboard order and the
//! accessibility tree all read its answer, so a row is pressable exactly where
//! it is drawn. Only the captions and the quiet line are drawn here.

use super::*;
use koru_draw::geometry::Rect;
use koru_draw::tile::Tile;
use koru_ui::card::Card;
use koru_ui::{ChipNav, NavItem, NavList, PageShell};
use vello::peniko::Fill;

/// A caption's band — Settings' card caption: its line and the air under it.
const CAPTION_H: f32 = Card::TITLE_H + Card::TITLE_GAP;
/// The quiet *{n} Profiles hidden* line.
const QUIET_H: f32 = 24.0;
/// Between one group of rows and what follows it.
const SMALL_GAP: f32 = 8.0;
const GROUP_GAP: f32 = 24.0;
/// The air kept under the last row, and around a focused row brought into
/// view.
const END_PAD: f32 = 16.0;

/// The words on the nav, and on its chips.
const PROFILES: &str = "Profiles";
const THIS_COMPUTER: &str = "This computer";
pub(super) const NEW_PROFILE: &str = "New profile";
pub(super) const DEVICES: &str = "Linked devices";
pub(super) const CLOUD: &str = "Koru Cloud";

/// One row of the nav.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// A Profile on this device: its page.
    Profile(ProfileId),
    /// The quiet *{n} Profiles hidden* line.
    Hidden,
    NewProfile,
    Devices,
    Cloud,
}

impl Entry {
    /// The row that leads to `page`.
    pub(crate) fn of(page: Page) -> Self {
        match page {
            Page::Profile(profile) => Self::Profile(profile),
            Page::NewProfile => Self::NewProfile,
            Page::Devices => Self::Devices,
            Page::Cloud => Self::Cloud,
        }
    }
}

/// Where everything in the nav goes, in the tab's own coordinates and
/// already scrolled.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Laid {
    /// The nav is chips across the top: only `rows` means anything.
    pub chips: bool,
    pub profiles_caption: Rect,
    /// The `NavList` area of this device's Profiles.
    pub profiles: Rect,
    pub hidden: Option<Rect>,
    pub new_profile: Rect,
    pub computer_caption: Rect,
    pub computer: Rect,
    /// Every pressable row (or chip), top to bottom — which is also keyboard
    /// order.
    pub rows: Vec<(Entry, Rect)>,
    /// From the column's top to below the last row, unscrolled.
    pub height: f32,
}

/// Lay the nav's column out from `area`'s top-left for these Profiles and
/// whether any are hidden, scrolled by `scroll`.
pub(crate) fn layout(area: Rect, profiles: &[ProfileId], hidden: bool, scroll: f32) -> Laid {
    let (x, w) = (area.x, area.w);
    let mut y = area.y - scroll;
    let mut rows = Vec::new();
    let mut list = |y: &mut f32, entries: &mut dyn Iterator<Item = Entry>| {
        let start = *y;
        let mut count = 0;
        for (index, entry) in entries.enumerate() {
            rows.push((entry, NavList::row_rect(Rect::new(x, start, w, 0.0), index)));
            count += 1;
        }
        let area = Rect::new(x, start, w, NavList::height(count));
        *y += area.h;
        area
    };

    let profiles_caption = Rect::new(x, y, w, Card::TITLE_H);
    y += CAPTION_H;
    let profiles = list(&mut y, &mut profiles.iter().copied().map(Entry::Profile));
    let hidden = hidden.then(|| {
        y += SMALL_GAP * 0.5;
        let line = Rect::new(x, y, w, QUIET_H);
        y += QUIET_H;
        line
    });
    y += SMALL_GAP;
    let new_profile = list(&mut y, &mut std::iter::once(Entry::NewProfile));
    y += GROUP_GAP;
    let computer_caption = Rect::new(x, y, w, Card::TITLE_H);
    y += CAPTION_H;
    // *Koru Cloud* is a row only in a build that shows it (koru-features);
    // *Linked devices* is peer-to-peer and always is.
    let computer = list(
        &mut y,
        &mut [Entry::Devices, Entry::Cloud]
            .into_iter()
            .filter(|entry| *entry != Entry::Cloud || koru_features::cloud_sync()),
    );
    if let Some(line) = hidden {
        // In order: the hidden line sits between the Profiles and *New
        // profile*, so keyboard order is visual order.
        let at = rows
            .iter()
            .position(|(entry, _)| *entry == Entry::NewProfile)
            .unwrap_or(rows.len());
        rows.insert(at, (Entry::Hidden, line));
    }
    Laid {
        chips: false,
        profiles_caption,
        profiles,
        hidden,
        new_profile,
        computer_caption,
        computer,
        rows,
        height: y + scroll - area.y + END_PAD,
    }
}

impl ManageView {
    /// The nav's column, from where its first line goes down.
    fn nav_area(&self) -> Rect {
        page::SHELL.nav_area(self.viewport, self.band_h, PageShell::TOP)
    }

    fn nav_layout_scrolled(&self, scroll: f32) -> Laid {
        if self.compact() {
            let none = Rect::new(0.0, 0.0, 0.0, 0.0);
            return Laid {
                chips: true,
                profiles_caption: none,
                profiles: none,
                hidden: None,
                new_profile: none,
                computer_caption: none,
                computer: none,
                rows: self.nav_chips.clone(),
                height: 0.0,
            };
        }
        // *{n} Profiles hidden* is the account's — Koru Cloud's — so a build
        // that hides it has no such line (koru-features).
        layout(
            self.nav_area(),
            &profile_ids(&self.c),
            koru_features::cloud_sync() && self.c.hidden_profile_count() > 0,
            scroll,
        )
    }

    pub(super) fn nav_layout(&self) -> Laid {
        self.nav_layout_scrolled(self.nav_scroll)
    }

    pub(super) fn nav_layout_unscrolled(&self) -> Laid {
        self.nav_layout_scrolled(0.0)
    }

    /// What the compact chips are, in order: the Profiles, then *New
    /// profile*, *Linked devices* and *Koru Cloud*.
    fn chip_entries(&self) -> Vec<(Entry, String)> {
        self.c
            .profiles()
            .iter()
            .map(|row| (Entry::Profile(row.id), row.name.clone()))
            .chain([
                (Entry::NewProfile, NEW_PROFILE.to_owned()),
                (Entry::Devices, DEVICES.to_owned()),
                (Entry::Cloud, CLOUD.to_owned()),
            ])
            .filter(|(entry, _)| *entry != Entry::Cloud || koru_features::cloud_sync())
            .collect()
    }

    /// Lay the compact chips out, and so the band's height — before the
    /// page, which starts under the band. Wide, there are none.
    pub(super) fn relayout_nav(&mut self) {
        if !self.compact() {
            self.nav_chips.clear();
            self.band_h = 0.0;
            return;
        }
        let entries = self.chip_entries();
        let items: Vec<NavItem<'_>> = entries
            .iter()
            .map(|(_, label)| NavItem::new(label))
            .collect();
        let area = page::SHELL.chips_area(self.viewport.0, PageShell::COMPACT_TOP);
        let chips = ChipNav::layout(&mut self.engine, area, &items);
        self.band_h = PageShell::band_h(area, ChipNav::height(&chips));
        self.nav_chips = entries
            .into_iter()
            .map(|(entry, _)| entry)
            .zip(chips)
            .collect();
    }

    /// The row under the pointer, if it does something.
    pub(super) fn nav_hit(&self, x: f32, y: f32) -> Option<Entry> {
        if !self.in_nav(x, y) {
            return None;
        }
        self.nav_layout()
            .rows
            .into_iter()
            .find(|(_, rect)| rect.contains(x, y))
            .map(|(entry, _)| entry)
    }

    /// The rows the keyboard visits, top to bottom.
    pub(super) fn focus_order(&self) -> Vec<Entry> {
        self.nav_layout_unscrolled()
            .rows
            .into_iter()
            .map(|(entry, _)| entry)
            .collect()
    }

    /// Scroll the nav's column so the focused row is in view. The chips do
    /// not scroll.
    pub(super) fn reveal_focus(&mut self) {
        let Some(focus) = self.focus else {
            return;
        };
        if self.compact() {
            return;
        }
        let Some((_, rect)) = self
            .nav_layout_unscrolled()
            .rows
            .into_iter()
            .find(|(entry, _)| *entry == focus)
        else {
            return;
        };
        let vh = self.viewport.1;
        if rect.y < self.nav_scroll + END_PAD {
            self.nav_scroll = (rect.y - END_PAD).max(0.0);
        } else if rect.y + rect.h > self.nav_scroll + vh - END_PAD {
            self.nav_scroll = rect.y + rect.h - vh + END_PAD;
        }
        self.clamp_scroll();
    }

    /// How far the nav's column can scroll.
    pub(super) fn nav_max_scroll(&self) -> f32 {
        let laid = self.nav_layout_unscrolled();
        if laid.chips {
            return 0.0;
        }
        (self.nav_area().y + laid.height - self.viewport.1).max(0.0)
    }

    /// The row that shows the current page.
    fn current_entry(&self) -> Option<Entry> {
        self.page.map(Entry::of)
    }

    pub(super) fn paint_nav(&mut self, canvas: &mut Scene, root: Affine) {
        let s = self.sidebar();
        let panel = scene::rect(s.x, s.y, s.w, s.h);
        let _nav = inspect::scope("Manage nav", "part", root, panel, |r| {
            r.property("width", page::SHELL.nav_w, "page::SHELL");
            r.property("geometry", "koru_ui::PageShell", "shared shell");
            r.property("chips", self.compact(), "PageShell::COMPACT_BELOW");
        });
        // No fill and no edge: the nav sits on the page's own background, and
        // its rules are what set it apart — as Settings' does.
        if self.compact() {
            self.paint_chips(canvas, root);
            return;
        }
        canvas.push_clip_layer(Fill::NonZero, root, &panel);
        let _clip = inspect::clip(root, panel);
        let laid = self.nav_layout();
        self.paint_nav_caption(canvas, root, laid.profiles_caption, PROFILES);
        self.paint_profiles(canvas, root, &laid);
        if let Some(line) = laid.hidden {
            self.paint_hidden(canvas, root, line);
        }
        let new_items = [NavItem::new(NEW_PROFILE).with_add()];
        self.paint_list(
            canvas,
            root,
            laid.new_profile,
            &new_items,
            &[Entry::NewProfile],
        );
        self.paint_nav_caption(canvas, root, laid.computer_caption, THIS_COMPUTER);
        let devices = self.linked_count().to_string();
        let cloud = self.cloud_word();
        // A computer and a cloud left of the labels (the review's note).
        let mut cloud_item = NavItem::new(CLOUD).with_glyph(koru_draw::icons::CLOUD);
        if let Some((word, on)) = cloud {
            cloud_item = cloud_item.with_trailing(word);
            if on {
                cloud_item = cloud_item.with_dot(dot_on());
            }
        }
        let computer = [
            NavItem::new(DEVICES)
                .with_glyph(koru_draw::icons::DESKTOP)
                .with_trailing(&devices),
            cloud_item,
        ];
        self.paint_list(
            canvas,
            root,
            laid.computer,
            &computer,
            &[Entry::Devices, Entry::Cloud],
        );
        canvas.pop_layer();
    }

    /// The compact nav: every destination as a chip, the current page's
    /// filled.
    fn paint_chips(&mut self, canvas: &mut Scene, root: Affine) {
        let entries = self.chip_entries();
        let items: Vec<NavItem<'_>> = entries
            .iter()
            .map(|(_, label)| NavItem::new(label))
            .collect();
        let chips: Vec<Rect> = self.nav_chips.iter().map(|(_, rect)| *rect).collect();
        let order: Vec<Entry> = self.nav_chips.iter().map(|(entry, _)| *entry).collect();
        let find = |wanted: Option<Entry>| {
            wanted.and_then(|wanted| order.iter().position(|entry| *entry == wanted))
        };
        let (current, hovered, focused) = (
            find(self.current_entry()),
            find(self.hover),
            find(self.focus),
        );
        ChipNav::draw(
            canvas,
            root,
            &mut self.engine,
            &chips,
            &items,
            current,
            hovered,
            focused,
        );
    }

    /// The Koru Cloud row's trailing word and whether its dot is lit: *On*
    /// with a green dot while the account is subscribed, *Off* once it is
    /// known not to be, and nothing while that is still unknown — unknown is
    /// not *Off*.
    fn cloud_word(&self) -> Option<(&'static str, bool)> {
        // A build with no Koru Cloud: known, and off — the page says why.
        if self.c.account().stage == AccountStage::NotConfigured {
            return Some(("Off", false));
        }
        match self.c.cloud_access() {
            Some(true) => Some(("On", true)),
            Some(false) => Some(("Off", false)),
            None => None,
        }
    }

    /// A group's caption — Settings' card caption: `theme::role::meta()` in
    /// the dim color, level with the rows' rule.
    fn paint_nav_caption(&mut self, canvas: &mut Scene, root: Affine, at: Rect, text: &str) {
        let p = theme::p();
        let style = theme::role::meta();
        let _caption = inspect::scope(
            "Manage nav caption",
            "part",
            root,
            scene::rect(at.x, at.y, at.w, at.h),
            |r| r.property("text", text, "caller"),
        );
        koru_ui::text_paragraph::draw_line(
            canvas,
            root,
            &mut self.engine,
            text,
            at.x,
            at.y,
            style,
            "theme::role::meta()",
            p.text_dim,
        );
    }

    fn paint_profiles(&mut self, canvas: &mut Scene, root: Affine, laid: &Laid) {
        let current = self.c.current();
        let rows: Vec<_> = self
            .c
            .profiles()
            .iter()
            .map(|row| {
                (
                    row.id,
                    row.name.clone(),
                    row.icon.clone(),
                    row.color.clone(),
                    self.c.needs_attention(row.id),
                )
            })
            .collect();
        let items: Vec<NavItem<'_>> = rows
            .iter()
            .map(|(id, name, icon, color, attention)| {
                let mut item = NavItem::new(name)
                    .with_tile(Tile {
                        icon: icon.as_deref(),
                        color: color.as_deref(),
                        name,
                    })
                    .with_attention(*attention);
                if current == Some(*id) {
                    item = item.with_dot(dot_current());
                }
                item
            })
            .collect();
        let entries: Vec<Entry> = rows.iter().map(|row| Entry::Profile(row.0)).collect();
        self.paint_list(canvas, root, laid.profiles, &items, &entries);
    }

    fn paint_hidden(&mut self, canvas: &mut Scene, root: Affine, line: Rect) {
        let p = theme::p();
        let lang = crate::format::lang();
        let text = koru_i18n::t(lang, koru_i18n::Key::AccountSwitcherHidden).replace(
            "{}",
            &koru_i18n::number::decimal(lang, self.c.hidden_profile_count() as u64),
        );
        let style = theme::role::meta();
        let layout = self.engine.simple_layout(&text, style);
        let lit = self.hover == Some(Entry::Hidden) || self.focus == Some(Entry::Hidden);
        let _hidden = inspect::scope(
            "Manage hidden profiles",
            "part",
            root,
            scene::rect(line.x, line.y, line.w, line.h),
            |r| r.property("count", self.c.hidden_profile_count(), "controller"),
        );
        if self.focus == Some(Entry::Hidden) {
            scene::stroke(
                canvas,
                root,
                p.focus_ring,
                theme::BORDER_W * 2.0,
                &scene::rrect(line.x, line.y, line.w, line.h, 6.0),
            );
        }
        // Where a row's name starts, so the line reads as one of them.
        koru_ui::text_paragraph::draw_line(
            canvas,
            root,
            &mut self.engine,
            &text,
            line.x + 12.0,
            line.y + (line.h - scene::first_line_height(&layout)) * 0.5,
            style,
            "theme::role::meta()",
            if lit { p.text } else { p.text_dim },
        );
    }

    /// One run of shared rows, with this tab's current, hovered and focused
    /// row found among `entries`.
    fn paint_list(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        area: Rect,
        items: &[NavItem<'_>],
        entries: &[Entry],
    ) {
        if items.is_empty() {
            return;
        }
        let find = |wanted: Option<Entry>| {
            wanted.and_then(|wanted| entries.iter().position(|entry| *entry == wanted))
        };
        let (current, hovered, focused) = (
            find(self.current_entry()),
            find(self.hover),
            find(self.focus),
        );
        NavList::draw(
            canvas,
            root,
            &mut self.engine,
            area,
            items,
            current,
            hovered,
            focused,
        );
    }
}
