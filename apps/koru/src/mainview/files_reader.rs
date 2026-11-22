// SPDX-License-Identifier: MPL-2.0

//! Read recognition state off the UI thread, once per Files-menu opening.
//! Always show OCR settings and readiness; enabled readers also offer a re-scan.

use super::{ContextMenu, MainView, MenuAction, MenuSubject, TabView};
use crate::settings_view::reader_link::{Command, ReaderLink};
use koru_store_wire::reader::ReaderState;

#[derive(Default)]
pub(super) struct FilesReader {
    link: Option<ReaderLink>,
    state: Option<ReaderState>,
    menu: Option<(u64, (f32, f32))>,
    pub pending: bool,
}

impl FilesReader {
    pub fn menu_items(&self, files: bool) -> Vec<MenuAction> {
        let mut items = vec![MenuAction::ImportAs];
        // `recognizing` is activity, not enablement. An idle installed reader
        // is enabled. Unavailable installations lead to their explanation.
        if files {
            let enabled = self.state.as_ref().map(|state| {
                state.installed.is_some() && !state.turned_off && state.unavailable.is_none()
            });
            items.push(MenuAction::OcrSettings(enabled));
            if enabled == Some(true) {
                items.push(MenuAction::RescanAllFiles);
            }
        }
        items
    }

    pub fn opened(&mut self, instance: u64, at: (f32, f32)) {
        self.menu = Some((instance, at));
        if self.pending {
            return;
        }
        if !cfg!(test) {
            if self.link.is_none() {
                self.link = ReaderLink::start(koru_workspace_host::cli::root_from_args());
            }
            self.pending = self
                .link
                .as_ref()
                .is_some_and(|link| link.send(Command::Refresh));
        }
    }
}

impl MainView {
    pub(super) fn maintain_files_reader(&mut self) -> bool {
        let Some(answer) = self.files_reader.link.as_ref().and_then(ReaderLink::take) else {
            return false;
        };
        self.files_reader.pending = false;
        let Some(state) = answer.state else {
            return false;
        };
        self.set_files_reader_state(state)
    }

    fn set_files_reader_state(&mut self, state: ReaderState) -> bool {
        self.files_reader.state = Some(state);
        let Some((instance, at)) = self.files_reader.menu else {
            return false;
        };
        let Some(old) = self.menu.as_ref().filter(|menu| menu.instance == instance) else {
            return false;
        };
        let items = self.files_reader.menu_items(true);
        if old.items() == items.as_slice() {
            return false;
        }
        let hover = old.hover;
        // Rows have immutable accessibility identities. Replacing this opening
        // gives new rows new identities, so stale actions cannot target them.
        let mut menu = ContextMenu::new(&mut self.engine, MenuSubject::NewTab, at, items);
        menu.hover = hover.filter(|index| *index < menu.items().len());
        self.files_reader.menu = Some((menu.instance, at));
        self.menu = Some(menu);
        true
    }

    pub(super) fn open_reader_settings(&mut self) -> bool {
        self.open_settings_tab();
        if let TabView::Settings(view) = &mut self.tabs_mut().active_mut().view {
            view.scroll_to_reader();
        }
        true
    }
}

#[cfg(test)]
mod tests;
