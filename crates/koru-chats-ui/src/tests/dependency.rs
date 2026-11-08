// SPDX-License-Identifier: MPL-2.0

//! **The dependency list is the point.**
//!
//! One component, shown by the workspace window's navigator. It may name
//! neither host nor chat — `koru-ai-ui` depends on it for the list's
//! vocabulary, so depending back would be a cycle, and a workspace crate would
//! tie a list of chats to a workspace document it has no use for.

use std::collections::BTreeSet;

/// Crates this surface must never link, and why.
const FORBIDDEN: &[&str] = &[
    // The chat surface, which depends on this one.
    "koru-ai-ui",
    // The workspace document and the shell that owns one.
    "koru-core",
    "koru-workspace",
    "koru-editor",
    "koru-docstore",
    "koru-workspace-host",
    "koru-window",
    // The AI service's insides: this surface reads chats through the
    // gateway, and knows nothing about how they are stored.
    "koru-ai-sqlite",
    "koru-ai-provider",
];

fn manifest() -> String {
    include_str!("../../Cargo.toml").to_owned()
}

fn named() -> BTreeSet<String> {
    manifest()
        .lines()
        .filter_map(|line| {
            line.split_once(" = {")
                .map(|(name, _)| name.trim().to_owned())
        })
        .collect()
}

#[test]
fn the_surface_names_neither_host_nor_a_document_model() {
    let named = named();
    for forbidden in FORBIDDEN {
        assert!(
            !named.contains(*forbidden),
            "{forbidden} would make this surface un-hostable"
        );
    }
    // And the two that make it a *surface* are there.
    assert!(named.contains("koru-draw"));
    assert!(named.contains("koru-i18n"));
}

/// The `no-reader` feature exists, because a host's own tests cannot be
/// covered by this crate's `cfg(test)`.
#[test]
fn a_host_can_turn_the_reader_off_for_its_own_tests() {
    assert!(manifest().contains("no-reader = []"));
}
