// SPDX-License-Identifier: MPL-2.0

use super::*;

/// Prove complete immutable state without interpreting document semantics.
/// This does not bound allocation: bounded consumers must cap encoded inputs
/// before calling, and run import away from the Store owner's lock.
pub fn validate_history_evidence(
    snapshot: &[u8],
    version_vector: &[u8],
    frontiers: &[u8],
    expected_checksum: &str,
) -> Result<LoroDoc> {
    let invalid = || Error::new(Code::CorruptState, "invalid immutable history evidence");
    if expected_checksum.len() != 64
        || !expected_checksum
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || checksum(snapshot) != expected_checksum
    {
        return Err(invalid());
    }
    let version = loro::VersionVector::decode(version_vector).map_err(|_| invalid())?;
    let frontier = loro::Frontiers::decode(frontiers).map_err(|_| invalid())?;
    let doc = LoroDoc::new();
    let imported = doc.import(snapshot).map_err(|_| invalid())?;
    if imported.pending.is_some() || doc.oplog_vv() != version || doc.oplog_frontiers() != frontier
    {
        return Err(invalid());
    }
    Ok(doc)
}

#[cfg(test)]
mod tests;
