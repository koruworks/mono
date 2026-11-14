// SPDX-License-Identifier: MPL-2.0

//! The fence around a leaf that every plan of the milestone links.
//!
//! The orchestrator worker, `runs.db`, the Run document, the events, the
//! surfaces and the phone all speak this vocabulary, so anything this crate
//! links is linked by all of them. It may name the leaf crates whose ids it
//! carries (§9) and nothing that runs a process, opens a database, carries a
//! wire or draws.

use std::{collections::BTreeSet, path::Path, process::Command};

#[test]
fn normal_dependency_direction_is_a_leaf_allowlist() {
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
        BTreeSet::from([
            "blake3",
            "koru-device",
            "koru-events",
            "koru-session-protocol",
            "koru-store-core",
            "schemars",
            "serde",
            "serde_json",
            "uuid",
        ]),
        "koru-delegate-core is a leaf: the id crates it speaks Koru's vocabulary \
         through, pure canonical JSON/digest utilities, an optional schema derive, and uuid"
    );
}

#[test]
fn normal_transitive_graph_has_no_runtime_store_wire_or_surface_package() {
    let output = Command::new(env!("CARGO"))
        .args(["tree", "--manifest-path"])
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
    for forbidden in [
        // Surfaces.
        "winit",
        "vello",
        "parley",
        "koru-ui",
        "koru-window",
        "koru-editor",
        // The node model: the orchestrator has none and must not acquire
        // one here (the `koru-link` precedent).
        "koru-core",
        // Runtimes and transports.
        "tokio",
        "axum",
        "reqwest",
        "portable-pty",
        "alacritty_terminal",
        "koru-ipc",
        // Databases: the vocabulary of `runs.db` is not `runs.db`.
        "rusqlite",
        "koru-store-sqlite",
        "koru-ai-sqlite",
        // The crates above this one in the delegation stack.
        "koru-session-runtime",
        "koru-session-wire",
        "koru-session-client",
        "koru-agent-wire",
        "koru-api-gateway-core",
        "koru-store-client",
        "koru-store-wire",
        "koru-ai-core",
    ] {
        assert!(
            !packages.contains(forbidden),
            "forbidden dependency {forbidden}: {packages:?}"
        );
    }
}
