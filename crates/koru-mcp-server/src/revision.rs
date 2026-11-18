// SPDX-License-Identifier: MPL-2.0

//! The revision a tool answers, and takes back as `base_revision`.
//!
//! It is the document's version vector in Loro's own encoding, as
//! hexadecimal, **with its entries in peer order**. Loro keeps the vector in
//! a hash map and encodes it in the map's order, so two reads of one
//! revision — one from the session that made an edit, one from a fresh join —
//! could give two strings. Anything that compares revisions as strings then
//! sees a change that is not one: the document view's wait answered
//! *changed* for an unchanged document, and a test that compared an edit's
//! revision with the next read's failed whenever the random peer ids hashed
//! into another order (CI on `main`, 2026-11-18).
//!
//! The bytes are still a version vector Loro decodes, so a revision issued
//! before this order was fixed is still accepted as a base.

use std::collections::BTreeMap;

use super::{ToolError, WorkspaceDoc};

/// `doc`'s revision: equal for equal versions, whatever order the map holds
/// its peers in.
pub(crate) fn revision(doc: &WorkspaceDoc) -> String {
    encode(&doc.version())
}

fn encode(version: &loro::VersionVector) -> String {
    // A map in postcard is its length and then its pairs, the encoding Loro
    // gives the hash map; a `BTreeMap` writes the pairs in key order.
    let ordered: BTreeMap<_, _> = version
        .iter()
        .map(|(peer, counter)| (*peer, *counter))
        .collect();
    hex(&postcard::to_allocvec(&ordered).expect("a version vector always encodes"))
}

/// Refuse a `base_revision` this document's history does not contain.
pub(crate) fn validate_base(doc: &WorkspaceDoc, encoded: &str) -> Result<(), ToolError> {
    let bytes = unhex(encoded)?;
    let base = loro::VersionVector::decode(&bytes)
        .map_err(|e| ToolError::invalid(format!("invalid base_revision: {e}")))?;
    if !doc.version().includes_vv(&base) {
        return Err(ToolError {
            code: "revision_conflict".into(),
            message: "base_revision is not in this document's history".into(),
            details: None,
        });
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>, ToolError> {
    if !s.len().is_multiple_of(2) {
        return Err(ToolError::invalid("revision is not hexadecimal"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| ToolError::invalid("revision is not hexadecimal"))
        })
        .collect()
}

#[cfg(test)]
mod tests;
