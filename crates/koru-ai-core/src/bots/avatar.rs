// SPDX-License-Identifier: MPL-2.0

//! Which face a Bot wears.
//!
//! A Bot's face is one of a fixed table of [`BOT_AVATAR_COUNT`] faces, drawn
//! by `koru_draw::face` (whose `FACE_COUNT` must equal this count; the
//! window crate asserts that at compile time). The person may pick one
//! ([`super::BotConfig::avatar`], stored in `bots.avatar`, ai.db 24). A Bot
//! with no stored choice still has a face: it is derived from its id, so
//! every Bot made before the column existed has one with no backfill, and it
//! is the same on every device that knows the id.
//!
//! This is the one owner of that rule. A view asks [`Bot::avatar`] and never
//! hashes an id itself.
use super::{Bot, BotError, BotId};

/// How many faces there are. A stored choice is `0..BOT_AVATAR_COUNT`;
/// migration 024's CHECK holds the same bound.
///
/// **The count and the order of the table are frozen.** A derived face is
/// the id's hash modulo this count, so changing it would change the face of
/// every Bot that never picked one.
pub const BOT_AVATAR_COUNT: u8 = 24;

/// A stored or requested choice is inside the table.
pub fn validate_avatar(avatar: Option<u8>) -> Result<(), BotError> {
    match avatar {
        Some(face) if face >= BOT_AVATAR_COUNT => Err(BotError::Invalid),
        _ => Ok(()),
    }
}

/// **The face of a Bot that never picked one**: FNV-1a (64-bit) over the 16
/// bytes of its id, modulo [`BOT_AVATAR_COUNT`].
///
/// Written out here rather than taken from `std::hash`, whose hashers are
/// free to change between Rust releases: this value is shown to people and
/// must be the same in every build and on every platform, for ever.
pub fn derived_avatar(bot: BotId) -> u8 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hash = bot.as_uuid().as_bytes().iter().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    });
    // The remainder of a count that fits a byte fits a byte.
    (hash % u64::from(BOT_AVATAR_COUNT)) as u8
}

/// The face to draw for `bot`: its stored choice when that is inside the
/// table, else the one derived from its id. A choice from a newer build's
/// longer table falls back to the derived face rather than to a wrong one.
pub fn bot_avatar(bot: BotId, stored: Option<u8>) -> u8 {
    stored
        .filter(|face| *face < BOT_AVATAR_COUNT)
        .unwrap_or_else(|| derived_avatar(bot))
}

impl Bot {
    /// The face to draw for this Bot ([`bot_avatar`]).
    pub fn avatar(&self) -> u8 {
        bot_avatar(self.id, self.config.avatar)
    }
}

#[cfg(test)]
mod tests;
