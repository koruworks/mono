// SPDX-License-Identifier: MPL-2.0

//! **The fence around a component another project can take** (QUESTIONS
//! O42): the control links the design system, the strings and the two
//! platform libraries, and nothing that reads a catalog, carries the AI
//! service's types, talks to a service or holds the node model. The form is
//! `koru-delegate-core`'s: the manifest's own `[dependencies]` against an
//! allowlist, and the normal transitive graph against what must never come
//! along.

use std::{collections::BTreeSet, path::Path, process::Command};

#[test]
fn normal_dependencies_are_the_design_system_strings_and_platform_only() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let section = manifest
        .split("[dependencies]")
        .nth(1)
        .unwrap()
        .split("[dev-dependencies]")
        .next()
        .unwrap();
    let actual: BTreeSet<_> = section
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
        .filter(|name| !name.is_empty() && !name.starts_with('#'))
        .map(|name| name.split('.').next().unwrap())
        .collect();
    assert_eq!(
        actual,
        BTreeSet::from(["koru-draw", "koru-i18n", "koru-ui", "vello", "winit"]),
        "koru-agent-config-ui is host-fed: the design system, the strings and \
         the platform libraries, nothing else"
    );
}

#[test]
fn normal_transitive_graph_has_no_service_client_ai_store_or_editor_package() {
    let output = Command::new(env!("CARGO"))
        .args(["tree", "--locked", "--manifest-path"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .args(["-e", "normal", "--prefix", "none"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8(output.stdout).unwrap();
    let packages: BTreeSet<_> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert!(
        packages.contains("koru-draw"),
        "the tree was read: {packages:?}"
    );
    for forbidden in [
        // A service and a way to reach one.
        "koru-api-client",
        "reqwest",
        "tokio",
        // The AI stack: its types, its wire and the chat that hosts this.
        "koru-ai-core",
        "koru-ai-wire",
        "koru-ai-ui",
        "koru-chats-ui",
        // The node model, the editor and the search surface.
        "koru-core",
        "koru-editor",
        "koru-search-ui",
        // The Store, its CRDT and the delegation and session vocabularies.
        "koru-store-core",
        "loro",
        "koru-session-protocol",
        "koru-delegate-core",
        // A window of its own.
        "koru-window",
    ] {
        assert!(
            !packages.contains(forbidden),
            "forbidden dependency {forbidden}: {packages:?}"
        );
    }
}
