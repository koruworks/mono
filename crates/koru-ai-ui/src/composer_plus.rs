// SPDX-License-Identifier: MPL-2.0

//! The composer's plus menu. Its recent rows come from the containing window;
//! selecting one writes the same document pill as the `@` picker.

use std::borrow::Cow;

use koru_ai_core::DocumentId;
use koru_i18n::{Key as TextKey, t};
use koru_ui::menu::MenuItem;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::{ChatSurface, Focus, Menu, icons};

#[derive(Clone)]
pub(super) enum PlusItem {
    Recent(&'static str),
    KoruContext(&'static str),
    Back(&'static str),
    Document(DocumentId, String),
    Empty(&'static str),
    Hint(&'static str),
}

impl MenuItem for PlusItem {
    fn label(&self) -> Cow<'_, str> {
        match self {
            Self::Recent(label)
            | Self::KoruContext(label)
            | Self::Back(label)
            | Self::Empty(label)
            | Self::Hint(label) => Cow::Borrowed(label),
            Self::Document(_, title) => Cow::Borrowed(title),
        }
    }

    fn icon(&self) -> Option<char> {
        match self {
            Self::Recent(_) => Some(icons::CLOCK),
            Self::KoruContext(_) | Self::Document(_, _) => Some(icons::DOCUMENT),
            Self::Back(_) | Self::Empty(_) | Self::Hint(_) => None,
        }
    }

    fn has_submenu(&self) -> bool {
        matches!(self, Self::Recent(_))
    }

    fn selectable(&self) -> bool {
        !matches!(self, Self::Empty(_) | Self::Hint(_))
    }
}

impl ChatSurface {
    /// The containing window's resolved recents, newest first. A changed
    /// title updates the next open of the menu; a menu already under the
    /// pointer keeps the rows it was opened with.
    pub fn set_recent_documents(&mut self, documents: Vec<(DocumentId, String)>) -> bool {
        if self.recent_documents == documents {
            return false;
        }
        self.recent_documents = documents;
        true
    }

    fn plus_menu_at(&mut self, items: Vec<PlusItem>) {
        let button = self.plus_button();
        self.plus_menu = Some(Menu::new(&mut self.engine, (button.x, button.y), items));
    }

    fn open_plus_menu(&mut self) {
        self.dismiss_popovers();
        self.plus_menu_at(vec![
            PlusItem::Recent(t(self.lang, TextKey::HomeRecent)),
            PlusItem::KoruContext(t(self.lang, TextKey::AiComposerKoruContext)),
            PlusItem::Hint(t(self.lang, TextKey::AiComposerPlusHint)),
        ]);
    }

    fn open_recent_plus_menu(&mut self) {
        let mut items = vec![PlusItem::Back(t(self.lang, TextKey::Back))];
        if self.recent_documents.is_empty() {
            items.push(PlusItem::Empty(t(self.lang, TextKey::AiComposerNoRecent)));
        } else {
            items.extend(
                self.recent_documents
                    .iter()
                    .map(|(id, title)| PlusItem::Document(*id, title.clone())),
            );
        }
        self.plus_menu_at(items);
    }

    fn choose_plus_item(&mut self, index: usize) -> bool {
        let Some(item) = self
            .plus_menu
            .as_ref()
            .and_then(|menu| menu.items().get(index))
            .cloned()
        else {
            return false;
        };
        match item {
            PlusItem::Recent(_) => self.open_recent_plus_menu(),
            PlusItem::Back(_) => self.open_plus_menu(),
            PlusItem::KoruContext(_) => {
                self.plus_menu = None;
                self.focus = Focus::Composer;
                self.composer.set_focused(true);
                self.composer.key(
                    &Key::Character("@".into()),
                    Some("@"),
                    ModifiersState::empty(),
                );
            }
            PlusItem::Document(id, title) => {
                self.plus_menu = None;
                self.focus = Focus::Composer;
                self.composer.insert_document(id, &title);
            }
            PlusItem::Empty(_) | PlusItem::Hint(_) => return false,
        }
        true
    }

    pub(super) fn plus_menu_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        if self.plus_button().contains(x, y) {
            if self.plus_menu.take().is_none() {
                self.open_plus_menu();
            }
            return Some(true);
        }
        let menu = self.plus_menu.as_ref()?;
        if let Some(index) = menu.item_at(self.menu_viewport(), x, y) {
            return Some(self.choose_plus_item(index));
        }
        self.plus_menu = None;
        Some(true)
    }

    pub(super) fn plus_menu_mouse_move(&mut self, x: f32, y: f32) -> Option<bool> {
        let viewport = self.menu_viewport();
        let menu = self.plus_menu.as_mut()?;
        let next = menu.item_at(viewport, x, y);
        let changed = menu.hover != next;
        menu.hover = next;
        Some(changed)
    }

    pub(super) fn plus_menu_key(&mut self, key: &Key) -> Option<bool> {
        let menu = self.plus_menu.as_mut()?;
        match key {
            Key::Named(NamedKey::Escape) => {
                self.plus_menu = None;
            }
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                let enabled: Vec<_> = menu.items().iter().map(MenuItem::selectable).collect();
                menu.step(
                    &enabled,
                    if matches!(key, Key::Named(NamedKey::ArrowUp)) {
                        -1
                    } else {
                        1
                    },
                );
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let selected = menu.hover.unwrap_or(0);
                self.choose_plus_item(selected);
            }
            _ => return Some(false),
        }
        Some(true)
    }
}

#[cfg(test)]
mod tests;
