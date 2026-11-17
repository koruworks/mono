// SPDX-License-Identifier: MPL-2.0

//! Which face a Bot wears.
//!
//! A Bot's face is one of a fixed table of [`BOT_AVATAR_COUNT`] faces, drawn
//! by `koru_draw::face` (whose `FACE_COUNT` must equal this count; the
//! window crate asserts that at compile time). The person may pick one
//! ([`super::BotConfig::avatar`], stored in `bots.avatar`, ai.db 24). A Bot
//! with no stored choice still has a face: [`DEFAULT_AVATAR`], the first of
//! the table, so every Bot made before the column existed has one with no
//! backfill, and it is the same on every device.
//!
//! This is the one owner of that rule. A view asks [`Bot::avatar`] and never
//! decides a face itself.
use super::{Bot, BotError};

/// How many faces there are. A stored choice is `0..BOT_AVATAR_COUNT`;
/// migration 024's CHECK holds the same bound.
///
/// **The order of the table is frozen.** A stored choice is an index into
/// it, and [`DEFAULT_AVATAR`] names its first row, so reordering it would
/// change faces people already know.
pub const BOT_AVATAR_COUNT: u8 = 24;

/// **The face of a Bot that never picked one**, and the one the *New bot*
/// dialog opens on: the first of the table, the gray face with two dot eyes
/// (`koru_draw::face::ROWS[0]`).
///
/// It was once derived from the Bot's id, so that Bots with no choice did
/// not all look alike; the owner chose one plain default instead. Nothing
/// stored changed: a `NULL` `bots.avatar` is still "none picked".
pub const DEFAULT_AVATAR: u8 = 0;

/// A stored or requested choice is inside the table.
pub fn validate_avatar(avatar: Option<u8>) -> Result<(), BotError> {
    match avatar {
        Some(face) if face >= BOT_AVATAR_COUNT => Err(BotError::Invalid),
        _ => Ok(()),
    }
}

/// The face to draw for a Bot whose stored choice is `stored`: that choice
/// when it is inside the table, else [`DEFAULT_AVATAR`]. A choice from a
/// newer build's longer table falls back to the default rather than to a
/// wrong face.
pub fn bot_avatar(stored: Option<u8>) -> u8 {
    stored
        .filter(|face| *face < BOT_AVATAR_COUNT)
        .unwrap_or(DEFAULT_AVATAR)
}

impl Bot {
    /// The face to draw for this Bot ([`bot_avatar`]).
    pub fn avatar(&self) -> u8 {
        bot_avatar(self.config.avatar)
    }
}

#[cfg(test)]
mod tests;
