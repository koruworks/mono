// SPDX-License-Identifier: MPL-2.0

//! The Profiles page reuses the navigation chip component above its detail.
use super::*;
use koru_ui::{ChipNav, NavItem, PageShell};

impl ManageView {
    pub(crate) fn profiles_page(&self) -> Page {
        self.c
            .selected()
            .or_else(|| self.c.current())
            .or_else(|| self.c.profiles().first().map(|p| p.id))
            .map_or(Page::NewProfile, Page::Profile)
    }

    pub(super) fn layout_profile_tabs(&mut self) {
        self.profile_tabs.clear();
        let Some((_, column)) = self.embedded else {
            return;
        };
        if !matches!(self.page, Some(Page::Profile(_) | Page::NewProfile)) {
            return;
        }
        let mut entries: Vec<_> = self
            .c
            .profiles()
            .iter()
            .map(|p| (nav::Entry::Profile(p.id), p.name.clone()))
            .collect();
        if koru_features::cloud_sync() && self.c.hidden_profile_count() > 0 {
            entries.push((
                nav::Entry::Hidden,
                format!("{} hidden", self.c.hidden_profile_count()),
            ));
        }
        entries.push((nav::Entry::NewProfile, "+".to_owned()));
        let items: Vec<_> = entries.iter().map(|(_, name)| NavItem::new(name)).collect();
        let rects = ChipNav::layout(&mut self.engine, column, &items);
        self.profile_tabs = entries
            .into_iter()
            .zip(rects)
            .map(|((entry, name), rect)| (entry, name, rect))
            .collect();
    }

    pub(super) fn profile_tabs_offset(&self) -> f32 {
        match (self.profile_tabs.first(), self.profile_tabs.last()) {
            (Some((_, _, first)), Some((_, _, last))) => {
                last.y + last.h - first.y + PageShell::LEAD_GAP
            }
            _ => 0.0,
        }
    }

    pub(super) fn paint_profile_tabs(&mut self, canvas: &mut Scene, root: Affine) {
        if self.profile_tabs.is_empty() {
            return;
        }
        let items: Vec<_> = self
            .profile_tabs
            .iter()
            .map(|(_, name, _)| NavItem::new(name))
            .collect();
        let rects: Vec<_> = self.profile_tabs.iter().map(|(_, _, rect)| *rect).collect();
        let index = |entry| {
            self.profile_tabs
                .iter()
                .position(|(candidate, _, _)| Some(*candidate) == entry)
        };
        let (current, hover, focus) = (
            index(self.page.map(nav::Entry::of)),
            index(self.hover),
            index(self.focus),
        );
        ChipNav::draw(
            canvas,
            root,
            &mut self.engine,
            &rects,
            &items,
            current,
            hover,
            focus,
        );
    }
}
