// SPDX-License-Identifier: MPL-2.0

//! The fence around the authority's dependencies.
//!
//! This crate is linked into the API gateway process. A UI crate reaching in
//! here would put parley and vello behind an HTTP handler; an async runtime
//! would put a second one beside the gateway's own. Neither would fail to
//! compile — they would just quietly make the authority expensive to host, and
//! the mistake would be found by somebody wondering why the gateway binary
//! grew. So it is a test.
//!
//! The fence around who may *open* a settings file is here too, for the same
//! reason: three parsers of `preferences.conf` is what ADR-266 exists to
//! remove, and "there is only one now" is a sentence that decays into a
//! comment unless something fails when a fourth appears.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

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
        BTreeSet::from(["directories", "schemars", "serde", "serde_json"]),
        "the settings authority is a leaf: it holds the registry and the file \
         formats, and nothing that draws, renders, or runs an event loop"
    );
}

#[test]
fn normal_transitive_graph_has_no_runtime_or_surface_package() {
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
        // The surfaces. The authority owns no UI, and this is where that
        // sentence in the charter stops being a promise and starts being a
        // build failure.
        "winit",
        "vello",
        "parley",
        "koru-draw",
        "koru-window",
        "koru-ui",
        "koru-workspace-host",
        // The runtimes. The gateway host already has one; a second one inside
        // the authority would be a second reactor for no reason.
        "tokio",
        "axum",
        "reqwest",
        // The other authorities. This crate *faces* the AI worker and the
        // events journal through ports; it must never link them, or the port
        // was pointless.
        "koru-events",
        "koru-ai-sqlite",
        "koru-update",
        "rusqlite",
    ] {
        assert!(
            !packages.contains(forbidden),
            "forbidden dependency {forbidden}: {packages:?}"
        );
    }
}

/// **`preferences.conf` has exactly one reader and one writer.**
///
/// It had three parsers — this crate's predecessors in `koru-workspace-host`,
/// `koru-window` and `koru-dictation` — plus an undocumented fourth in the
/// Agent's tray that read the file line by line and tolerated neither a
/// byte-order mark nor a space before the `=`. They disagreed about what a
/// value meant, which is the defect ADR-266 exists to remove.
///
/// **A file nobody can locate is a file nobody can parse**, so what this pins
/// is the *locator*. Outside this crate there is exactly one —
/// `koru_window::user_preferences_path` — and this lists every file allowed to
/// name it, with what each one is for. Its own resolver, `SettingsPaths`, never
/// leaves this crate: it is private to the authority by construction, since a
/// caller would have to link `koru-settings` and then still ask the Gateway.
///
/// **Adding a name here is a decision**, and having to make it deliberately is
/// the whole point of the test. Prose about the move is deliberately not
/// counted: the mistake being guarded against is opening the file, not
/// mentioning it.
#[test]
fn preferences_conf_has_one_locator_outside_the_authority() {
    let root = workspace_root();
    let named: BTreeSet<String> = rust_sources(&root)
        .into_iter()
        .filter(|file| {
            std::fs::read_to_string(file).is_ok_and(|text| text.contains("user_preferences_path"))
        })
        .map(|file| relative(&root, &file))
        .collect();
    let allowed: BTreeSet<String> = [
        // The resolver itself, and its re-export.
        "crates/koru-window/src/window_geometry.rs",
        "crates/koru-window/src/lib.rs",
        // The one-time relocation of `emoji_recents` and `last_export_dir`,
        // which reads and never writes and is deletable once no install
        // predates the move. `koru_workspace_host::preferences::tests` pins
        // that it is the only read in that crate.
        "crates/koru-workspace-host/src/settings.rs",
        "crates/koru-workspace-host/src/preferences/tests.rs",
        // The roaming *directory*, for a data-locations menu item. It never
        // opens the file.
        "apps/koru-agent/src/paths.rs",
        // This test, and the note in `paths.rs` recording that the resolver it
        // replaces could take no root — survey note 3.
        "crates/koru-settings/src/dependency.rs",
        "crates/koru-settings/src/paths.rs",
        // UI Lab's own isolation test, which lists `user_preferences_path` as
        // a forbidden token rather than calling it.
        "apps/koru/src/ui_lab/tests.rs",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(
        named.difference(&allowed).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "a new file resolves preferences.conf; if it then opens it, that is a \
         second parser and ADR-266 says there is one"
    );
}

/// Every `.rs` file in the workspace's own crates and apps.
fn rust_sources(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for area in ["crates", "apps"] {
        walk(&root.join(area), &mut out);
    }
    out
}

fn walk(directory: &Path, into: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // `target` is build output, and reading it would make this test
            // depend on what happens to have been compiled.
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            walk(&path, into);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            into.push(path);
        }
    }
}

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/koru-settings sits two levels under the workspace root")
        .to_path_buf()
}

fn relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}
