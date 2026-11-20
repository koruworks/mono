// SPDX-License-Identifier: MPL-2.0

//! The fence: host-agnostic means no process, socket, HTTP server or
//! surface in the normal dependency graph. `winit`, `vello`, `parley`, the
//! `koru-*` window crates and every transport are forbidden; the leaf
//! crates the orchestrator speaks through are the allowlist.

use std::{collections::BTreeSet, path::Path, process::Command};

#[test]
fn normal_dependencies_are_the_delegation_leaves_and_serde() {
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
            "koru-delegate-core",
            "koru-delegate-document",
            "koru-delegate-preamble",
            "koru-recurrence",
            "koru-delegate-schedule",
            "koru-delegate-scope",
            "koru-delegate-sqlite",
            "koru-session-protocol",
            "serde",
            "serde_json",
            "tracing",
            "koru-core",
        ]),
        "koru-delegate-runtime drives runs through ports and links nothing that hosts one"
    );
}

#[test]
fn normal_transitive_graph_has_no_window_process_or_transport_package() {
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
        // Surfaces: every window crate.
        "winit",
        "vello",
        "parley",
        "koru-ui",
        "koru-window",
        "koru-editor",
        "koru-workspace",
        "koru-draw",
        "koru-keys",
        "koru-search-ui",
        "koru-voice-ui",
        // Runtimes, servers and transports: the host's, not this crate's.
        "tokio",
        "axum",
        "reqwest",
        "tungstenite",
        "portable-pty",
        "alacritty_terminal",
        "koru-ipc",
        "koru-session-client",
        "koru-session-wire",
        "koru-session-runtime",
        "koru-session-host",
        "koru-api-client",
        "koru-api-gateway-core",
        "koru-store-client",
        "koru-store-server",
        "koru-agent-worker",
        "koru-delegate-api",
    ] {
        assert!(
            !packages.contains(forbidden),
            "forbidden dependency {forbidden}: {packages:?}"
        );
    }
}
