// SPDX-License-Identifier: MPL-2.0

//! **Reopen the tab that was just closed** (`Ctrl+Shift+T`), as a browser does.
//!
//! A tab closed by hand (its ✕, `Ctrl+W`, *Close* in its menu) leaves a small
//! record here, newest last. Only what can come back without guessing is kept:
//!
//! - a **document**, reopened through the same navigation a search hit uses;
//! - a **terminal**, reopened the way a restored one is. Closing a terminal tab
//!   detaches it and the process keeps running, so reopening finds it again, or
//!   offers to resume it when it has since gone.
//!
//! Anything else (the welcome page, a task list, a run) is a place one reaches
//! in a click, and is not worth a slot. The list lives as long as the window
//! and is not saved.

use koru_session_protocol::{SessionId, SessionProfileKey};
use koru_workspace_host::SessionRecovery;

use super::MainView;
use super::tab::TabView;

/// More than anyone steps back through, few enough to stay a handful of bytes.
const KEPT: usize = 20;

pub(super) enum ClosedTab {
    Document(koru_core::DocumentId),
    Session {
        session: SessionId,
        profile: SessionProfileKey,
        recovery: Option<SessionRecovery>,
    },
}

impl MainView {
    /// Note tab `i` before it closes.
    pub(super) fn remember_closed_tab(&mut self, i: usize) {
        let Some(tab) = self.tabs().get(i) else {
            return;
        };
        if tab.is_pinned() {
            return;
        }
        let closed = match &tab.view {
            TabView::Session(view) => match (view.session, view.profile_key.clone()) {
                (Some(session), Some(profile)) => ClosedTab::Session {
                    session,
                    profile,
                    recovery: self.session_recovery(view),
                },
                _ => return,
            },
            TabView::Leaf(_) | TabView::Canvas(_) => match tab.document {
                Some(document) => ClosedTab::Document(document),
                None => return,
            },
            _ => return,
        };
        self.closed_tabs.push(closed);
        if self.closed_tabs.len() > KEPT {
            self.closed_tabs.remove(0);
        }
    }

    /// *Reopen closed tab*: the newest one that can still come back. One that
    /// is already showing is skipped, so the chord never opens a second copy.
    pub(super) fn reopen_closed_tab(&mut self) -> bool {
        while let Some(closed) = self.closed_tabs.pop() {
            match closed {
                ClosedTab::Document(document) => {
                    if self.focus_showing_document(document) {
                        return true;
                    }
                    self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
                        target: koru_workspace_host::NavigationTarget::Document {
                            document,
                            node: None,
                        },
                        match_text: None,
                        disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
                    });
                    return true;
                }
                ClosedTab::Session {
                    session,
                    profile,
                    recovery,
                } => {
                    if self.session_tab_for(session).is_some() {
                        continue;
                    }
                    let tab = self.restored_session_tab(session, profile, recovery);
                    self.record_jump();
                    self.tabs_mut().push(tab);
                    self.slides_mut().clear();
                    self.mark_tab_widths_stale();
                    self.size_visible_groups();
                    self.ensure_tab_visible(self.tabs().active());
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests;
