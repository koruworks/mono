// SPDX-License-Identifier: MPL-2.0

//! **A status card**: one piece of work that is on its way somewhere else,
//! said as a short card — a lead mark, a title, one status line, an optional
//! quiet note, and the host's actions under them.
//!
//! ```text
//! col  KoruStatusCard                 on the raised plate, S3 inside, S1 between lines
//!   row  Status card head             the host's lead mark, the title
//!   Status card status                one sentence; the attention tone warns
//!   Status card note                  optional, quiet
//!   row  Status card actions          the host's own controls, in its order
//! ```
//!
//! **Host-fed, plain values in.** The card knows no product: it takes words
//! the host has already translated, a tone, the keys the host finds its lines
//! by, an optional lead element (a face, an icon) and the host's action
//! elements, already wired to the host's own focus and pointer state. It
//! holds no state and calls nothing; what a press means is the host's.
//!
//! **Never by color alone.** The attention tone changes the status line's ink,
//! and the words say the same thing; the card's group name is the host's, so a
//! screen reader hears what the card is about before its lines.
//!
//! First consumer: a Bot's request to another Bot, in the asking Bot's tab
//! (M6, `apps/koru/src/bots/children.rs`). Anything else that reports one
//! piece of work running elsewhere — a delegated task, an upload — can use
//! it as it is.

use koru_draw::el::{El, Key, Role, col, row, text};
use koru_draw::theme::{Ink, Radius, Space, Surface, roles};

/// How the status line reads.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum StatusTone {
    /// The ordinary state of the work: secondary ink.
    #[default]
    Plain,
    /// The person is needed: the warning foreground. The words must say so
    /// too.
    Attention,
    /// The work has ended and nothing more will happen: hint ink.
    Settled,
}

impl StatusTone {
    fn ink(self) -> Ink {
        match self {
            Self::Plain => Ink::Secondary,
            Self::Attention => Ink::Notice,
            Self::Settled => Ink::Hint,
        }
    }
}

/// The keys the host finds the card and its lines by: its accessibility
/// tree and its tests read them from the frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct StatusCardKeys {
    pub card: Key,
    pub title: Key,
    pub status: Key,
    pub note: Key,
}

/// What one card says. Every string is the host's, in the reader's language.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusCard {
    /// The card's accessible name: what it is about.
    pub name: String,
    pub title: String,
    pub status: String,
    pub tone: StatusTone,
    /// A quiet line under the status, such as *Paused*.
    pub note: Option<String>,
}

impl StatusCard {
    /// **The card as elements**, with `lead` beside the title and `actions`
    /// in a row under the lines. An empty `actions` draws no row.
    pub fn el(self, keys: StatusCardKeys, lead: Option<El>, actions: Vec<El>) -> El {
        let mut head = row("Status card head").gap(Space::S2).items_center();
        if let Some(lead) = lead {
            head = head.child(lead);
        }
        head = head.child(
            text(self.title)
                .named("Status card title")
                .id(keys.title)
                .role(roles::ROW)
                .emphasis(true)
                .fill()
                .truncate(),
        );
        let mut card = col("KoruStatusCard")
            .id(keys.card)
            .bg(Surface::Raised)
            .radius(Radius::L)
            .pad(Space::S3)
            .gap(Space::S1)
            .semantics(Role::Group)
            .name_text(self.name)
            .child(head)
            .child(
                text(self.status)
                    .named("Status card status")
                    .id(keys.status)
                    .role(roles::META)
                    .ink(self.tone.ink())
                    .emphasis(self.tone == StatusTone::Attention)
                    .wrap(),
            );
        if let Some(note) = self.note {
            card = card.child(
                text(note)
                    .named("Status card note")
                    .id(keys.note)
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .wrap(),
            );
        }
        if !actions.is_empty() {
            card = card.child(
                row("Status card actions")
                    .gap(Space::S2)
                    .items_center()
                    .children(actions),
            );
        }
        card
    }
}

#[cfg(test)]
mod tests;
