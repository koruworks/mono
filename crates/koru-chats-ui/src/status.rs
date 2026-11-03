// SPDX-License-Identifier: MPL-2.0

//! **A chat's status, decided in one place** (koru-ai D83): working, needs
//! you, unread — or nothing to say.
//!
//! Three things feed it — whether a reply is running, whether a question is
//! waiting on the person (an approval or a permission), and whether
//! something arrived that the person has not seen ([`crate::seen`]) — and
//! [`ChatStatus::of`] is the only place they are weighed. The tab, the
//! Chats row and the chat itself all read the answer, so the three cannot
//! disagree about the same chat.
//!
//! **Needs you outranks working**: a reply that stopped to ask is running,
//! and the question is what the person can do something about. **Working
//! outranks unread**: the ring says more is coming.

use std::time::Instant;

use koru_draw::theme;
use koru_i18n::Lang;
use koru_i18n::strings::{Key, t};
use koru_shell_chrome::status_mark::{self, StatusMark};

/// How often a host redraws while a status ring turns or a light sweeps:
/// re-exported so a chat surface paces its own light at the same frame
/// without naming the chrome crate.
pub use koru_shell_chrome::status_mark::FRAME;

/// What one chat's mark says.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug, Hash)]
pub enum ChatStatus {
    #[default]
    Idle,
    /// A reply is running.
    Working,
    /// A reply stopped to ask: an approval or a permission waits.
    NeedsYou,
    /// A reply finished, or a question arrived, while the person was
    /// looking elsewhere; it stays until the chat is opened.
    Unread,
}

impl ChatStatus {
    /// **The one derivation.** See the module docs for the order.
    pub fn of(working: bool, needs_you: bool, unread: bool) -> Self {
        if needs_you {
            Self::NeedsYou
        } else if working {
            Self::Working
        } else if unread {
            Self::Unread
        } else {
            Self::Idle
        }
    }

    /// The mark a tab or a row wears, `None` for an idle chat. `now` turns
    /// the ring; `None` (reduced motion) holds it still.
    pub fn mark(self, now: Option<Instant>) -> Option<StatusMark> {
        let p = theme::p();
        match self {
            Self::Idle => None,
            Self::Working => Some(StatusMark::Spinner {
                phase: now.map(status_mark::spin_phase),
                color: p.text_dim,
            }),
            Self::NeedsYou => Some(StatusMark::Dot(attention())),
            Self::Unread => Some(StatusMark::Dot(p.accent)),
        }
    }

    /// Whether this status's mark turns — the only reason it asks for a
    /// frame. Never under reduced motion.
    pub fn animating(self, reduced_motion: bool) -> bool {
        self == Self::Working && !reduced_motion
    }

    /// The word a screen reader hears after the row's name. `None` for an
    /// idle chat, which says nothing more.
    pub fn spoken(self, lang: Lang) -> Option<&'static str> {
        let key = match self {
            Self::Idle => return None,
            Self::Working => Key::ChatsStatusWorking,
            Self::NeedsYou => Key::ChatsStatusNeedsYou,
            Self::Unread => Key::ChatsStatusUnread,
        };
        Some(t(lang, key))
    }
}

/// The amber of *needs you*: the accent set's own orange, never a new
/// palette field.
fn attention() -> vello::peniko::Color {
    theme::accent_tone("orange")
        .map(|tone| tone.swatch)
        .unwrap_or(theme::p().accent)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs you over working over unread over nothing.
    #[test]
    fn the_order_is_needs_you_working_unread() {
        assert_eq!(ChatStatus::of(false, false, false), ChatStatus::Idle);
        assert_eq!(ChatStatus::of(false, false, true), ChatStatus::Unread);
        assert_eq!(ChatStatus::of(true, false, true), ChatStatus::Working);
        assert_eq!(ChatStatus::of(true, true, true), ChatStatus::NeedsYou);
        assert_eq!(ChatStatus::of(false, true, false), ChatStatus::NeedsYou);
    }

    /// Only a running chat's ring turns, and never under reduced motion,
    /// which holds the ring still rather than hiding it.
    #[test]
    fn only_a_working_chat_animates_and_reduced_motion_holds_it_still() {
        assert!(ChatStatus::Working.animating(false));
        assert!(!ChatStatus::Working.animating(true));
        assert!(!ChatStatus::Unread.animating(false));
        assert!(!ChatStatus::NeedsYou.animating(false));
        let still = ChatStatus::Working.mark(None).expect("a ring");
        assert!(!still.animating());
        assert!(ChatStatus::Idle.mark(Some(Instant::now())).is_none());
    }
}
