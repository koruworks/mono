// SPDX-License-Identifier: MPL-2.0

//! Sync and profile pages hosted by the same Settings tab and navigation.
use super::*;
use crate::manage::{ManageView, Page};
use koru_settings_ui::{HostPage, PageId};

const CLOUD: u128 = 1;
const DEVICES: u128 = 2;
const PROFILES: u128 = 3;
const WORKSPACES: u128 = 4;

impl SettingsView {
    pub(crate) fn with_workspace_controller(controller: crate::manage::SharedWorkspaces) -> Self {
        let mut view = Self::new();
        view.manage = Box::new(ManageView::with_workspace_controller(controller));
        view.sync_navigation();
        view
    }

    pub(crate) fn show_manage(&mut self, page: Page) {
        self.surface.clear_search();
        self.manage.show(page);
        self.manage_shown = self.manage.current_page();
        self.sync_navigation();
        self.surface.show_page(PageId::Host(page_id(
            self.manage.current_page().unwrap_or(page),
        )));
        self.layout_manage();
    }

    pub(crate) fn seed_cloud_cache(&mut self, cache: &crate::manage::profiles::CloudCache) {
        if self.manage.delegate(|c| c.seed_cloud_cache(cache)) {
            self.sync_navigation();
        }
    }

    pub(crate) fn manage_visible(&self) -> bool {
        matches!(self.surface.visible_page(), Some(PageId::Host(_)))
    }

    pub(super) fn sync_navigation(&mut self) {
        if self.manage_visible()
            && self.manage_shown.is_some()
            && self.manage.current_page() != self.manage_shown
        {
            if let Some(page) = self.manage.current_page() {
                self.manage_shown = Some(page);
                // Update the navigation catalog before revealing a newly made Profile.
                self.surface.set_host_pages(self.navigation_pages());
                self.surface.show_page(PageId::Host(page_id(page)));
            }
        }
        self.surface.set_host_pages(self.navigation_pages());
    }

    fn navigation_pages(&self) -> Vec<HostPage> {
        let mut pages = Vec::new();
        if koru_features::cloud_sync() {
            let mut row = HostPage::new(CLOUD, "Cloud", "Account");
            row.glyph = Some(koru_draw::icons::CLOUD);
            row.trailing = self.manage.cloud_word().map(|(word, _)| word.to_owned());
            pages.push(row);
        }
        let mut row = HostPage::new(DEVICES, "Devices", "Account");
        row.glyph = Some(koru_draw::icons::DESKTOP);
        row.trailing = Some(self.manage.linked_count_label());
        pages.push(row);
        let mut row = HostPage::new(PROFILES, "Profiles", "Account");
        row.attention = self
            .manage
            .profile_rows()
            .iter()
            .any(|p| self.manage.profile_needs_attention(p.id));
        pages.push(row);
        pages.push(HostPage::new(WORKSPACES, "Workspaces", "Account"));
        pages
    }

    pub(super) fn layout_manage(&mut self) {
        self.surface.relayout();
        if !self.manage_visible() {
            if !matches!(self.surface.page(), PageId::Host(_)) {
                self.manage_shown = None;
            }
            self.manage.set_in_front(false);
            return;
        }
        self.manage.embed(
            self.surface.viewport(),
            self.surface.page_area(),
            self.surface.column_rect(),
        );
        self.manage
            .embed_navigation(self.surface.sidebar_rect(), self.surface.host_page_rects());
        if let PageId::Host(id) = self.surface.page() {
            let page = match id {
                CLOUD => Some(Page::Cloud),
                DEVICES => Some(Page::Devices),
                PROFILES => Some(match self.manage.current_page() {
                    Some(page @ (Page::Profile(_) | Page::NewProfile)) => page,
                    _ => self.manage.profiles_page(),
                }),
                WORKSPACES => Some(match self.manage.current_page() {
                    Some(page @ Page::Workspaces(_)) => page,
                    _ => self.manage.workspaces_page(),
                }),
                _ => None,
            };
            if let Some(page) = page
                && self.manage_shown != Some(page)
            {
                self.manage.show(page);
                self.manage_shown = self.manage.current_page();
            }
        }
    }

    pub(super) fn manage_point(&self, x: f32, y: f32) -> bool {
        self.manage_visible()
            && (self.manage.modal_open() || !self.surface.sidebar_rect().contains(x, y))
    }
}

fn page_id(page: Page) -> u128 {
    match page {
        Page::Cloud => CLOUD,
        Page::Devices => DEVICES,
        Page::NewProfile | Page::Profile(_) => PROFILES,
        Page::Workspaces(_) => WORKSPACES,
    }
}
