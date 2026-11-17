// SPDX-License-Identifier: MPL-2.0

//! The fence: this crate stays carrier-neutral. Its manifest names no
//! network, bridge, trust or database crate, so the same messages can ride
//! another carrier unchanged (contract 4.10).

const MANIFEST: &str = include_str!("../../Cargo.toml");

fn dependencies() -> Vec<&'static str> {
    let mut inside = false;
    let mut names = Vec::new();
    for line in MANIFEST.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == "[dependencies]" || line == "[dev-dependencies]";
            continue;
        }
        if !inside || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name = line
            .split(['=', '.', ' '])
            .next()
            .expect("a dependency line names its crate");
        names.push(name);
    }
    names
}

#[test]
fn m8_wire_surface_names_no_carrier_trust_record_or_database() {
    assert_eq!(
        dependencies(),
        [
            "koru-ai-core",
            "koru-ai-wire",
            "serde",
            "serde_json",
            "uuid"
        ]
    );
}

#[test]
fn m8_wire_surface_no_source_file_names_a_carrier_crate() {
    let sources = [
        include_str!("../lib.rs"),
        include_str!("../frame.rs"),
        include_str!("../json.rs"),
        include_str!("../message.rs"),
        include_str!("../negotiate.rs"),
        include_str!("../record.rs"),
        include_str!("../command.rs"),
    ];
    for source in sources {
        for forbidden in [
            "koru_network",
            "koru_device::",
            "koru_sync_control",
            "koru_pairing",
            "rusqlite",
            "tokio",
            "iroh::",
        ] {
            assert!(!source.contains(forbidden), "names {forbidden}");
        }
    }
}
