// SPDX-License-Identifier: MPL-2.0

//! Page documents on Tasks pages — the pure rules every reader and writer of
//! one shares.
//!
//! A page document is the document a person writes above the tasks of a list
//! or an agenda bucket. Its identity is derived rather than minted
//! ([`page_document_id`](crate::ids::page_document_id)) and its first
//! operations are written identically on every device
//! ([`WorkspaceDoc::page_genesis`](crate::schema::WorkspaceDoc::page_genesis)).
//! This module holds the two small rules the rest of the feature keeps asking
//! — which generation of a page is current, and whether one replica's version
//! of a document is covered by another's — so that the system document, the
//! desktop and a headless reader cannot come to answer them differently.
//!
//! The persisted keys these rules read are described in
//! `docs/architecture/DATAMODEL.md` and decided in
//! `docs/decisions/ADR-000-pagedocumentidentity.md`.

use loro::VersionVector;

/// The current generation of a page: the **numeric** maximum of its
/// generation keys, or `None` for a page that has never had a document.
///
/// Generations are a grow-only set of decimal keys — `0`, `1`, `2` … — added
/// when a page's document is replaced after a deletion and never removed. The
/// maximum is numeric, never lexical: `"10"` is current over `"9"`, which a
/// string comparison would get wrong the first time a page is replaced ten
/// times. A key that is not a decimal number is not a generation and is
/// skipped, so a newer build's stray key cannot move the page.
pub fn current_generation<'a>(keys: impl IntoIterator<Item = &'a str>) -> Option<u64> {
    keys.into_iter()
        .filter_map(|key| key.parse::<u64>().ok())
        .max()
}

/// The join of two version vectors: the component-wise maximum.
///
/// **Monotone by construction** — the join contains both inputs, is the same
/// whichever order the inputs arrive in, and joining a vector with something
/// it already contains changes nothing. That is what lets a deletion event's
/// coverage, and a replica's record of writing it found outside that
/// coverage, only ever grow, whatever order the decisions behind them are
/// seen in.
pub fn join(a: &VersionVector, b: &VersionVector) -> VersionVector {
    let mut joined = a.clone();
    joined.merge(b);
    joined
}

/// Whether `cover` contains every operation in `version`.
///
/// Answers `true` for an equal vector and `false` for a concurrent one — a
/// vector that is ahead on one peer and behind on another is not covered,
/// and neither covers the other. This is the question the whole deletion
/// protocol rests on: a deletion whose coverage contains everything a replica
/// wrote is simply a deletion, and writing outside it is what makes a
/// conflict.
pub fn covers(cover: &VersionVector, version: &VersionVector) -> bool {
    cover.includes_vv(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_generation_is_the_numeric_maximum() {
        assert_eq!(current_generation(["0", "1", "10", "9"]), Some(10));
        assert_eq!(current_generation(["9", "10"]), Some(10));
        assert_eq!(current_generation(["0"]), Some(0));
    }

    #[test]
    fn a_page_with_no_generation_has_none() {
        assert_eq!(current_generation([]), None);
    }

    #[test]
    fn a_key_that_is_not_a_generation_is_skipped() {
        assert_eq!(current_generation(["2", "later", "", "-1"]), Some(2));
        assert_eq!(current_generation(["later"]), None);
    }

    fn vv(entries: &[(u64, i32)]) -> VersionVector {
        let mut out = VersionVector::new();
        for &(peer, counter) in entries {
            out.insert(peer, counter);
        }
        out
    }

    #[test]
    fn the_join_is_the_component_wise_maximum() {
        let a = vv(&[(1, 5), (2, 1)]);
        let b = vv(&[(2, 4), (3, 2)]);
        let joined = join(&a, &b);
        assert_eq!(joined, vv(&[(1, 5), (2, 4), (3, 2)]));
        assert_eq!(join(&b, &a), joined, "order does not matter");
        assert_eq!(
            join(&joined, &a),
            joined,
            "joining what is contained is a no-op"
        );
        assert!(covers(&joined, &a) && covers(&joined, &b));
    }

    #[test]
    fn coverage_is_containment_not_comparison() {
        let behind = vv(&[(1, 2)]);
        let ahead = vv(&[(1, 5)]);
        let sideways = vv(&[(2, 1)]);
        assert!(covers(&ahead, &behind));
        assert!(!covers(&behind, &ahead));
        assert!(covers(&ahead, &ahead), "equal is covered");
        assert!(
            !covers(&ahead, &sideways) && !covers(&sideways, &ahead),
            "concurrent"
        );
        assert!(
            covers(&ahead, &VersionVector::new()),
            "nothing is always covered"
        );
    }
}
