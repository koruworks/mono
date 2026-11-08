// SPDX-License-Identifier: MPL-2.0

//! **What the person has seen of each chat, on this device** (koru-ai D83,
//! D84): the last message they saw, when, and whether something has
//! arrived since.
//!
//! **Device-local by the person's choice** — not on the chat, not synced.
//! The host keeps one book and stores it where it keeps the window's own
//! session ([`SeenBook::to_storage`], one `key=value` line); this crate only
//! says what the line holds and how it reads back.
//!
//! ## Unread
//!
//! A chat is unread when either is true:
//!
//! - **a host saw it arrive** — a reply finished, or a question came, in a
//!   chat open somewhere but not looked at ([`SeenBook::arrived`]);
//! - **its activity is newer than the look** — the Chats list's
//!   `updated_at`, which moves when a message is sent and when its reply
//!   ends, is later than the moment the chat was last looked at. That
//!   catches a reply that ended with no window open on it.
//!
//! A chat with no mark at all reads as **seen**: a book that starts empty
//! must not light every chat a person already has.
//!
//! Looking at a chat writes its mark twice — when it is shown and when it
//! stops being shown — so whatever the person did while looking (sent a
//! message, changed the model) is behind the mark, not news.

use std::collections::HashMap;

use koru_ai_core::{ChatId, MessageId};

/// The most marks the line keeps — the most recently looked-at first. A
/// chat dropped off the end reads as seen, the safe way to be wrong.
pub const MAX_MARKS: usize = 400;

/// One chat's mark.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct SeenMark {
    /// When the person last looked at it, on the device's clock (epoch ms).
    pub at_ms: i64,
    /// The newest finished message they saw then. A NEW divider goes above
    /// the first message after it.
    pub message: Option<MessageId>,
    /// Something arrived while they were looking elsewhere.
    pub unread: bool,
}

/// Every chat's mark.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct SeenBook {
    marks: HashMap<ChatId, SeenMark>,
}

impl SeenBook {
    /// Read the stored line back. **Anything unreadable is dropped, never
    /// fatal**, as every other line of the session file: one bad entry costs
    /// that chat its mark, and that chat reads as seen.
    pub fn parse(line: &str) -> Self {
        let mut marks = HashMap::new();
        for entry in line.split(',') {
            let mut parts = entry.trim().split(':');
            let (Some(chat), Some(at), Some(message), Some(flag), None) = (
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
            ) else {
                continue;
            };
            let (Some(chat), Ok(at_ms)) = (ChatId::parse(chat), at.parse::<i64>()) else {
                continue;
            };
            let message = match message {
                "-" => None,
                id => match MessageId::parse(id) {
                    Some(id) => Some(id),
                    None => continue,
                },
            };
            let unread = match flag {
                "u" => true,
                "s" => false,
                _ => continue,
            };
            marks.insert(
                chat,
                SeenMark {
                    at_ms,
                    message,
                    unread,
                },
            );
        }
        Self { marks }
    }

    /// The line: `chat:at_ms:message-or-dash:u|s`, comma-separated, the most
    /// recently looked-at first, at most [`MAX_MARKS`]. Empty when the book
    /// is — the host then writes no line at all.
    pub fn to_storage(&self) -> String {
        let mut marks: Vec<(&ChatId, &SeenMark)> = self.marks.iter().collect();
        marks.sort_by(|(a_id, a), (b_id, b)| b.at_ms.cmp(&a.at_ms).then(a_id.cmp(b_id)));
        marks
            .into_iter()
            .take(MAX_MARKS)
            .map(|(chat, mark)| {
                format!(
                    "{}:{}:{}:{}",
                    chat.to_storage(),
                    mark.at_ms,
                    mark.message
                        .map_or_else(|| "-".to_owned(), MessageId::to_storage),
                    if mark.unread { "u" } else { "s" }
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn mark(&self, chat: ChatId) -> Option<&SeenMark> {
        self.marks.get(&chat)
    }

    /// **The person is looking at `chat`** (or just stopped): everything up
    /// to now is seen. `message` is the newest finished message on screen;
    /// `None` keeps the one the mark had. `true` when the mark moved.
    pub fn seen(&mut self, chat: ChatId, at_ms: i64, message: Option<MessageId>) -> bool {
        let was = self.marks.get(&chat).copied();
        let mark = SeenMark {
            at_ms: was.map_or(at_ms, |was| was.at_ms.max(at_ms)),
            message: message.or(was.and_then(|was| was.message)),
            unread: false,
        };
        self.marks.insert(chat, mark);
        was != Some(mark)
    }

    /// **Something arrived in `chat` while the person looked elsewhere** — a
    /// reply finished, or a question came. A chat with no mark yet takes
    /// one at `before`, the newest message the person could have seen, so
    /// its divider lands above what arrived. `true` when the mark moved.
    pub fn arrived(&mut self, chat: ChatId, at_ms: i64, before: Option<MessageId>) -> bool {
        let was = self.marks.get(&chat).copied();
        let mark = match was {
            Some(was) => SeenMark {
                unread: true,
                ..was
            },
            None => SeenMark {
                at_ms,
                message: before,
                unread: true,
            },
        };
        self.marks.insert(chat, mark);
        was != Some(mark)
    }

    /// **The person, still looking, now sees `message` too** — a reply that
    /// ended while they watched. Only the anchor moves: the look's time is
    /// written when it ends. Nothing for a chat with no mark. `true` when
    /// the mark moved.
    pub fn saw_message(&mut self, chat: ChatId, message: MessageId) -> bool {
        match self.marks.get_mut(&chat) {
            Some(mark) if mark.message != Some(message) => {
                mark.message = Some(message);
                true
            }
            _ => false,
        }
    }

    /// Whether `chat` holds something the person has not seen. `updated_at`
    /// is the chat's last activity as the Chats list has it, when the host
    /// has a list.
    pub fn is_unread(&self, chat: ChatId, updated_at_ms: Option<i64>) -> bool {
        self.marks.get(&chat).is_some_and(|mark| {
            mark.unread || updated_at_ms.is_some_and(|updated| updated > mark.at_ms)
        })
    }

    /// The last message the person saw in `chat`, for its NEW divider.
    pub fn last_seen(&self, chat: ChatId) -> Option<MessageId> {
        self.marks.get(&chat).and_then(|mark| mark.message)
    }

    /// **Fold another book in** — a session file read back, perhaps another
    /// window's or another profile's. Per chat the later look wins, so a
    /// merge never forgets a look and never wipes a book; merging an empty
    /// one changes nothing.
    pub fn merge(&mut self, other: SeenBook) {
        for (chat, mark) in other.marks {
            match self.marks.get(&chat) {
                Some(have) if have.at_ms >= mark.at_ms => {}
                _ => {
                    self.marks.insert(chat, mark);
                }
            }
        }
    }

    /// A chat that was deleted keeps no mark.
    pub fn forget(&mut self, chat: ChatId) -> bool {
        self.marks.remove(&chat).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line reads back as it went out, unread flags and dashes kept.
    #[test]
    fn the_line_round_trips() {
        let (a, b) = (ChatId::new(), ChatId::new());
        let message = MessageId::new();
        let mut book = SeenBook::default();
        book.seen(a, 10, Some(message));
        book.arrived(b, 20, None);
        let line = book.to_storage();
        assert_eq!(SeenBook::parse(&line), book);
        assert!(line.starts_with(&b.to_storage()), "most recent first");
    }

    /// One bad entry drops itself and leaves its neighbours, as every other
    /// line of the session file does; an empty line is an empty book.
    #[test]
    fn an_unreadable_entry_is_dropped_not_fatal() {
        let chat = ChatId::new();
        let line = format!(
            "nonsense,{}:5:-:s,{}:x:-:u,{}:5:-:maybe",
            chat.to_storage(),
            ChatId::new().to_storage(),
            ChatId::new().to_storage()
        );
        let book = SeenBook::parse(&line);
        assert_eq!(book.marks.len(), 1);
        assert!(book.mark(chat).is_some());
        assert!(SeenBook::parse("").is_empty());
    }

    /// No mark reads as seen; an arrival makes a chat unread until it is
    /// looked at; activity newer than the look is unread too.
    #[test]
    fn unread_is_an_arrival_or_activity_after_the_look() {
        let chat = ChatId::new();
        let mut book = SeenBook::default();
        assert!(!book.is_unread(chat, Some(1_000)), "an unmarked chat");
        book.seen(chat, 100, None);
        assert!(!book.is_unread(chat, Some(100)));
        assert!(book.is_unread(chat, Some(101)), "activity after the look");
        book.seen(chat, 200, None);
        assert!(!book.is_unread(chat, Some(150)));
        book.arrived(chat, 300, None);
        assert!(book.is_unread(chat, None), "an arrival");
        book.seen(chat, 400, None);
        assert!(!book.is_unread(chat, None), "until it is looked at");
    }

    /// The divider's anchor: the message seen last, kept when a later look
    /// names none, and the first one a fresh arrival is told.
    #[test]
    fn the_last_seen_message_is_kept_until_a_newer_one() {
        let chat = ChatId::new();
        let (first, second) = (MessageId::new(), MessageId::new());
        let mut book = SeenBook::default();
        book.arrived(chat, 5, Some(first));
        assert_eq!(book.last_seen(chat), Some(first));
        book.seen(chat, 6, None);
        assert_eq!(book.last_seen(chat), Some(first));
        book.seen(chat, 7, Some(second));
        assert_eq!(book.last_seen(chat), Some(second));
        assert!(!book.seen(chat, 7, Some(second)), "nothing moved");
    }

    /// A merge keeps the later look of each chat and never empties a book.
    #[test]
    fn a_merge_keeps_the_later_look() {
        let (a, b) = (ChatId::new(), ChatId::new());
        let mut book = SeenBook::default();
        book.seen(a, 10, None);
        book.seen(b, 30, None);
        let mut other = SeenBook::default();
        other.arrived(a, 20, None);
        other.seen(b, 20, None);
        book.merge(other);
        assert_eq!(book.mark(a).map(|mark| mark.at_ms), Some(20));
        assert!(book.mark(a).is_some_and(|mark| mark.unread));
        assert_eq!(book.mark(b).map(|mark| mark.at_ms), Some(30));
        let before = book.clone();
        book.merge(SeenBook::parse(""));
        assert_eq!(book, before);
    }

    /// The line is bounded: the most recently looked-at are kept.
    #[test]
    fn the_line_keeps_the_most_recent_marks() {
        let mut book = SeenBook::default();
        let mut newest = ChatId::new();
        for at in 0..(MAX_MARKS as i64 + 10) {
            newest = ChatId::new();
            book.seen(newest, at, None);
        }
        let back = SeenBook::parse(&book.to_storage());
        assert_eq!(back.marks.len(), MAX_MARKS);
        assert!(back.mark(newest).is_some());
    }
}
