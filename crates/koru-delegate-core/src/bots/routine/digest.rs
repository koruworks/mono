// SPDX-License-Identifier: MPL-2.0

use super::RoutineDigest32;
use serde::Serialize;
use std::io::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoutineDigestError {
    Invalid,
    TooLarge,
}

/// Stream a reviewed, versioned Routine encoding into its domain-separated
/// digest. Owners supply fixed-order structs and sorted set fields, never an
/// unordered map or a digest field. This computes bytes, not live authority.
/// `limit` bounds JSON bytes; domain bytes precede them without a separator.
#[doc(hidden)]
pub fn routine_digest_json<T: Serialize>(
    domain: &'static [u8],
    value: &T,
    limit: usize,
) -> Result<RoutineDigest32, RoutineDigestError> {
    let mut writer = DigestWriter {
        hash: blake3::Hasher::new(),
        remaining: limit,
        exceeded: false,
    };
    writer.hash.update(domain);
    if serde_json::to_writer(&mut writer, value).is_err() {
        return Err(if writer.exceeded {
            RoutineDigestError::TooLarge
        } else {
            RoutineDigestError::Invalid
        });
    }
    Ok(RoutineDigest32::from_bytes(
        *writer.hash.finalize().as_bytes(),
    ))
}

struct DigestWriter {
    hash: blake3::Hasher,
    remaining: usize,
    exceeded: bool,
}
impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(remaining) = self.remaining.checked_sub(bytes.len()) else {
            self.exceeded = true;
            return Err(std::io::Error::other("Routine digest input too large"));
        };
        self.hash.update(bytes);
        self.remaining = remaining;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
