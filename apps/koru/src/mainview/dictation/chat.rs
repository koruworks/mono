// SPDX-License-Identifier: MPL-2.0

//! **Dictation into a chat's composer** (koru-ai D103): the side pane's, or a
//! chat tab's.
//!
//! The words travel the path every surface's do — the shell's one
//! controller, `MainView::dictate`, and the composer's own
//! `DictationTarget` — so this is only what is different about a chat:
//!
//! - **Where it goes.** The pane is a field (`FieldId::Ai`), which routing
//!   already knew. A chat tab takes no character the container routes — its
//!   surface owns its keys — so it has no `EditableTarget` of its own; a
//!   dictation reaches its composer when the active tab is a chat and nothing
//!   else would take the words, and `LiveDictation::chat_tab` records it.
//! - **What is shown.** Not the floating listening chip: the composer's
//!   footer becomes the recording row — waveform, timer, ✕, ■ — so the
//!   status is handed to the chat surface instead, and no meter deadline is
//!   kept here.
//! - **What a press and a key mean.** A chat's dictation is a recording
//!   (`View::dictation_records`): ✕ throws it away, ■ and any press on the
//!   composer or any key stop it and keep its words, and a press elsewhere
//!   stops it and still lands.
//! - **How it starts.** The composer's microphone asks
//!   (`ChatSurface::take_wants_dictation`), and the shell begins it exactly as
//!   it begins `/dictate`.

use std::time::Instant;

use koru_ai_ui::{ChatSurface, DictationPress, DictationShown};
use koru_dictation::{DictationAction, Stage, State, Status};
use koru_ui::dictation::ComposeInto;
use koru_workspace_host::routing::{EditableTarget, FieldId};

use crate::mainview::MainView;
use crate::mainview::tab::TabView;
use crate::mainview::tab_rules::KoruTabs;

/// Which chat composer a dictation is going into.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum ChatDictation {
    /// The Koru AI side pane's.
    Pane,
    /// The chat tab's at this index of the focused group.
    Tab(usize),
}

/// **What the composer draws about a session**: its level, whether the
/// microphone is open, and whether the last words are being finished. Never
/// the words.
pub(crate) fn shown(status: Status) -> DictationShown {
    DictationShown {
        level: status.level.step(),
        listening: status.stage == Stage::Live && status.worker == State::Listening,
        finishing: status.stage == Stage::Finalising || status.worker == State::Finalising,
    }
}

impl MainView {
    /// The chat composer the running dictation is going into, if it is one.
    pub(crate) fn chat_dictation(&self) -> Option<ChatDictation> {
        let live = self.dictation.as_ref()?;
        if live.chat_tab {
            return Some(ChatDictation::Tab(live.tab));
        }
        (live.target == EditableTarget::Field(FieldId::Ai)).then_some(ChatDictation::Pane)
    }

    fn chat_surface(&self, which: ChatDictation) -> Option<&ChatSurface> {
        match which {
            ChatDictation::Pane => Some(&self.ai_chat),
            ChatDictation::Tab(index) => match self.tabs().get(index).map(|tab| &tab.view) {
                Some(TabView::AiChat(view)) => Some(view.dictation_surface()),
                _ => None,
            },
        }
    }

    fn chat_surface_mut(&mut self, which: ChatDictation) -> Option<&mut ChatSurface> {
        match which {
            ChatDictation::Pane => Some(&mut self.ai_chat),
            ChatDictation::Tab(index) => {
                match self.tabs_mut().get_mut(index).map(|tab| &mut tab.view) {
                    Some(TabView::AiChat(view)) => Some(view.dictation_surface_mut()),
                    _ => None,
                }
            }
        }
    }

    /// **Whether the active chat tab takes a dictation** that nothing else
    /// would: no blocker over the window, the pane not holding the keyboard,
    /// and the active tab a chat — the same answer a typed character gets.
    pub(crate) fn chat_tab_takes_dictation(&self) -> bool {
        !self.blockers().is_blocking()
            && !(self.ai_visible() && self.ai_focused)
            && matches!(self.tabs().active_tab().view, TabView::AiChat(_))
    }

    /// Perform `action` on the composer of the chat tab at `tab`.
    pub(crate) fn dictate_chat_tab(&mut self, tab: usize, action: &DictationAction) -> bool {
        let Some(surface) = self.chat_surface_mut(ChatDictation::Tab(tab)) else {
            return false;
        };
        let mut compose = ComposeInto::Held(surface.dictation_composer());
        match action {
            DictationAction::Begin { .. } => compose.begin(),
            DictationAction::Update { text, .. } => {
                compose.update(text.as_str());
                true
            }
            DictationAction::Commit { text, .. } => {
                compose.commit(text.as_str());
                true
            }
            DictationAction::Cancel { .. } => {
                compose.cancel();
                true
            }
        }
    }

    /// **Hand the chat what it shows**, or take it away. `Some(changed)` when
    /// a chat answered: a session going into one, or any chat that was showing
    /// a recording when the session ended. `None` leaves the status to the
    /// listening chip.
    pub(crate) fn show_chat_dictation(&mut self, status: Option<Status>) -> Option<bool> {
        let now = Instant::now();
        match status {
            Some(status) => {
                let which = self.chat_dictation()?;
                let surface = self.chat_surface_mut(which)?;
                Some(surface.show_dictation(Some(shown(status)), now))
            }
            None => {
                // The session has ended, and `LiveDictation` with it: every
                // chat lets its row go. Free — one `Option` each.
                let mut changed = self.ai_chat.show_dictation(None, now);
                for tab in self.tabs_mut().iter_mut() {
                    if let TabView::AiChat(view) = &mut tab.view {
                        changed |= view.dictation_surface_mut().show_dictation(None, now);
                    }
                }
                changed.then_some(true)
            }
        }
    }

    /// **What a press at `(x, y)` means to the chat that records**, in window
    /// coordinates. `None` when no chat records, or the press is not on its
    /// composer.
    pub(crate) fn chat_dictation_press(&self, x: f32, y: f32) -> Option<DictationPress> {
        match self.chat_dictation()? {
            ChatDictation::Pane => {
                if !self.ai_visible() {
                    return None;
                }
                // The pane's surface is laid out in window coordinates.
                self.ai_chat.dictation_press_at(x, y)
            }
            ChatDictation::Tab(index) => {
                // Only the tab showing can be pressed.
                if index != self.tabs().active() {
                    return None;
                }
                let (lx, ly) = self.to_leaf(x, y);
                self.chat_surface(ChatDictation::Tab(index))?
                    .dictation_press_at(lx, ly)
            }
        }
    }

    /// **A chat's microphone was pressed**: the pane's, which also takes the
    /// keyboard so the dictation is routed there, or the active chat tab's.
    pub(crate) fn take_chat_wants_dictation(&mut self) -> bool {
        if self.ai_chat.take_wants_dictation() {
            self.ai_focused = true;
            return true;
        }
        match self.tabs_mut().active_view() {
            TabView::AiChat(view) => view.dictation_surface_mut().take_wants_dictation(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
