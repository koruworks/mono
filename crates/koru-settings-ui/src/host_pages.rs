// SPDX-License-Identifier: MPL-2.0

//! Host-owned pages share Settings' navigation, search and page geometry.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPage {
    pub id: u128,
    pub label: String,
    pub group: String,
    pub glyph: Option<char>,
    pub tile: Option<(Option<String>, Option<String>)>,
    pub current: bool,
    pub attention: bool,
    pub trailing: Option<String>,
}

impl HostPage {
    pub fn new(id: u128, label: impl Into<String>, group: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            group: group.into(),
            glyph: None,
            tile: None,
            current: false,
            attention: false,
            trailing: None,
        }
    }

    pub(super) fn item(&self) -> NavItem<'_> {
        let mut item = NavItem::new(&self.label).with_attention(self.attention);
        if let Some(glyph) = self.glyph {
            item = item.with_glyph(glyph);
        }
        if let Some((icon, color)) = &self.tile {
            item = item.with_tile(koru_draw::tile::Tile {
                icon: icon.as_deref(),
                color: color.as_deref(),
                name: &self.label,
            });
        }
        if self.current {
            item = item.with_dot(theme::p().accent);
        }
        if let Some(trailing) = &self.trailing {
            item = item.with_trailing(trailing);
        }
        item
    }
}

impl SettingsSurface {
    /// The selected page, only while it matches the active quick filter.
    pub fn visible_page(&self) -> Option<PageId> {
        self.listed().contains(&self.page).then_some(self.page)
    }
    pub fn pointer_position(&self) -> (f32, f32) {
        self.pointer
    }
    pub fn host_page_rects(&self) -> Vec<(u128, Rect)> {
        let listed = self.listed();
        if self.compact() {
            return listed
                .iter()
                .zip(&self.nav_chips)
                .filter_map(|(page, r)| match page {
                    PageId::Host(id) => Some((*id, *r)),
                    _ => None,
                })
                .collect();
        }
        self.nav_runs(&listed)
            .iter()
            .flat_map(|run| {
                run.pages
                    .iter()
                    .enumerate()
                    .filter_map(|(i, index)| match listed[*index] {
                        PageId::Host(id) => Some((id, NavList::row_rect(run.area, i))),
                        _ => None,
                    })
            })
            .collect()
    }
    pub fn set_host_pages(&mut self, pages: Vec<HostPage>) {
        if self.host_pages == pages {
            return;
        }
        self.host_pages = pages;
        if !self.pages().contains(&self.page) {
            self.show_page(PageId::Appearance);
        }
        self.relayout();
    }

    pub(super) fn pages(&self) -> Vec<PageId> {
        self.host_pages
            .iter()
            .map(|p| PageId::Host(p.id))
            .chain(self.sections.pages())
            .collect()
    }

    pub(super) fn page_label(&self, page: PageId) -> &str {
        match page {
            PageId::Host(id) => self
                .host_pages
                .iter()
                .find(|p| p.id == id)
                .map_or("", |p| p.label.as_str()),
            _ => page.label(self.lang),
        }
    }
}
