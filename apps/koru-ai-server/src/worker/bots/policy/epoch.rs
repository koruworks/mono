// SPDX-License-Identifier: MPL-2.0
//! Sole native-policy epoch encoding; identity/revision only.
use super::*;

pub(in crate::worker) fn key(
    owner: BotOwner,
    bot: BotId,
    revision: u64,
) -> ApiResult<NativePolicyRequestId> {
    if revision == 0 || revision > i64::MAX as u64 {
        return Err(bot_failure(BotError::Invalid));
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"koru.bot.native-policy.v1\0");
    hash.update(owner.user_scope.as_uuid().as_bytes());
    hash.update(owner.device.as_uuid().as_bytes());
    hash.update(bot.as_uuid().as_bytes());
    hash.update(&revision.to_be_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.finalize().as_bytes()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
        .try_into()
        .map_err(|_| bot_failure(BotError::Invalid))
}

#[cfg(test)]
mod tests;
