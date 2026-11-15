// SPDX-License-Identifier: MPL-2.0

use std::fmt;

// A separate domain from `koru.bot.routine.bridge.v1` / `Koru-Routine `: the
// same parent secret derives an unrelated key and a different scheme, so a
// Routine credential never matches here and this one never matches there.
const CONTEXT: &str = "koru.bot.decision.bridge.v1";
const PREFIX: &[u8] = b"Koru-Decision ";
const HEADER_BYTES: usize = PREFIX.len() + 64;

/// Per-parent-run capability for the private decision enrollment bridge.
/// Deliberately not serializable: derived from trusted bootstrap only.
#[derive(Clone)]
pub struct DecisionBridgeAuthority([u8; 32]);

impl DecisionBridgeAuthority {
    pub(crate) fn from_parent_bytes(parent: &[u8]) -> Self {
        Self(blake3::derive_key(CONTEXT, parent))
    }

    fn header_bytes(&self) -> [u8; HEADER_BYTES] {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut header = [0; HEADER_BYTES];
        header[..PREFIX.len()].copy_from_slice(PREFIX);
        for (index, byte) in self.0.iter().enumerate() {
            header[PREFIX.len() + index * 2] = HEX[(byte >> 4) as usize];
            header[PREFIX.len() + index * 2 + 1] = HEX[(byte & 15) as usize];
        }
        header
    }

    /// Format only for an outbound Authorization header. Never log, persist
    /// or include the returned value in a request's Debug output.
    pub fn authorization_header(&self) -> String {
        // The fixed prefix and lowercase-hex alphabet are always ASCII.
        String::from_utf8(self.header_bytes().to_vec()).expect("ASCII bridge header")
    }

    /// Exact fixed-length comparison before any body parsing; equal-length
    /// input never exits on the first different byte.
    pub fn matches_authorization(&self, candidate: &[u8]) -> bool {
        if candidate.len() != HEADER_BYTES {
            return false;
        }
        self.header_bytes()
            .iter()
            .zip(candidate)
            .fold(0u8, |different, (a, b)| different | (a ^ b))
            == 0
    }
}

impl fmt::Debug for DecisionBridgeAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DecisionBridgeAuthority([REDACTED])")
    }
}
